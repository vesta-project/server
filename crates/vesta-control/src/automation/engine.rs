enum PortDecision {
    None,
    AuthorizationError(ActionError),
    Begun(std::result::Result<ActionHandle, ActionError>),
    Polled(std::result::Result<ActionStatus, ActionError>),
}

impl Automation {
    /// Trusted runtime pulse, UTC epoch milliseconds. Scheduling has a sixty-
    /// second grace; older occurrences are a missed backlog. Each pulse performs
    /// at most one durable transition per active run.
    pub fn tick(&self, now: i64) -> Result<()> {
        if now < 0 {
            return Err(invalid());
        }
        // A read-only pulse must not queue behind lifecycle/authentication writes
        // when there is no work to change. Read fresh state, without a cache or
        // skipped retention: a concurrent admission is picked up next pulse.
        if !self.read(|c| self.tick_needed(c, now))? {
            return Ok(());
        }
        let runs = self.write(|c| {
            self.retain(c, now)?;
            self.schedule(c, now)?;
            c.execute("UPDATE workflow_runs AS q SET state='running' WHERE q.state='queued' AND NOT EXISTS(SELECT 1 FROM workflow_runs a WHERE a.workflow_id=q.workflow_id AND a.state IN ('running','suspended')) AND q.rowid=(SELECT min(x.rowid) FROM workflow_runs x WHERE x.workflow_id=q.workflow_id AND x.state='queued')", [])?;
            // Deleted workflows may still have live snapshots. Bound by total
            // retained runs, not current workflow count, to avoid starving them.
            let mut q = c.prepare(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE state='running' ORDER BY rowid LIMIT ?"))?;
            Ok(q.query_map([MAX_RUNS], run_row)?.collect::<std::result::Result<Vec<_>, _>>()?)
        })?;
        for run in runs {
            self.advance(&run, now)?;
        }
        Ok(())
    }
    pub(super) fn tick_needed(&self, c: &Connection, now: i64) -> Result<bool> {
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflows WHERE enabled=1)
             OR EXISTS(SELECT 1 FROM workflow_runs WHERE state IN ('running','queued'))
             OR EXISTS(SELECT 1 FROM workflow_runs WHERE state IN ('succeeded','failed') AND finished_at<?1)
             OR EXISTS(SELECT 1 FROM workflow_runs WHERE state IN ('succeeded','failed') LIMIT 1 OFFSET 1024)
             OR EXISTS(SELECT 1 FROM notifications WHERE created_at<?1)
             OR EXISTS(SELECT 1 FROM notifications LIMIT 1 OFFSET ?2)",
            params![now.saturating_sub(inbox::HISTORY_MS), inbox::MAX_NOTIFICATIONS],
            |r| r.get(0),
        )?)
    }
    fn schedule(&self, c: &Connection, now: i64) -> Result<()> {
        let mut q = c.prepare(&format!(
            "SELECT {WORKFLOW_COLUMNS} FROM workflows WHERE enabled=1 ORDER BY id"
        ))?;
        let workflows = q
            .query_map([], workflow_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for w in workflows {
            let last: i64 = c.query_row(
                "SELECT last_occurrence FROM workflow_schedule WHERE workflow_id=?",
                [&w.id],
                |r| r.get(0),
            )?;
            let (first, latest) = match &w.definition.trigger {
                Trigger::Interval { seconds } => {
                    let period = i64::from(*seconds).checked_mul(1000).ok_or_else(invalid)?;
                    let Some(first) = last.checked_add(period) else {
                        continue;
                    };
                    if first > now {
                        continue;
                    }
                    (first, last + ((now - last) / period) * period)
                }
                Trigger::Cron {
                    expr,
                    utc_offset_minutes,
                } => {
                    let cron = Cron::parse(expr)?;
                    let Some(first) = cron.next_after(last, *utc_offset_minutes) else {
                        continue;
                    };
                    if first > now {
                        continue;
                    }
                    let Some(latest) = cron.previous_at_or_before(now, *utc_offset_minutes) else {
                        continue;
                    };
                    (first, latest)
                }
                _ => continue,
            };
            let missed = now.saturating_sub(first) >= GRACE_MS;
            let occurrences = if !missed {
                vec![latest]
            } else {
                match w.definition.missed_policy {
                    MissedPolicy::SkipAndNotify => {
                        inbox::insert(
                            c,
                            now,
                            Severity::Warning,
                            "Missed workflow occurrences",
                            "Scheduled work was skipped after downtime; no historical action was replayed.",
                            &w.id,
                        )?;
                        self.audit(c, None, "automation.missed", &w.id, "skipped", None)?;
                        Vec::new()
                    }
                    MissedPolicy::RunLatest => vec![latest],
                    MissedPolicy::CatchUp { max } => {
                        let mut tail = vec![latest];
                        while tail.len() < usize::from(max) {
                            let previous = match &w.definition.trigger {
                                Trigger::Interval { seconds } => tail
                                    .last()
                                    .and_then(|n| n.checked_sub(i64::from(*seconds) * 1000)),
                                Trigger::Cron {
                                    expr,
                                    utc_offset_minutes,
                                } => tail.last().and_then(|n| n.checked_sub(1)).and_then(|n| {
                                    Cron::parse(expr)
                                        .ok()?
                                        .previous_at_or_before(n, *utc_offset_minutes)
                                }),
                                _ => None,
                            };
                            let Some(previous) = previous.filter(|p| *p > last) else {
                                break;
                            };
                            tail.push(previous);
                        }
                        tail.reverse();
                        tail
                    }
                }
            };
            for occurrence in occurrences {
                self.enqueue(
                    c,
                    &w,
                    &serde_json::json!({"kind":"schedule","occurrence":occurrence}),
                    now,
                    matches!(w.definition.missed_policy, MissedPolicy::CatchUp { .. }),
                )?;
            }
            c.execute(
                "UPDATE workflow_schedule SET last_occurrence=? WHERE workflow_id=?",
                params![latest, w.id],
            )?;
        }
        Ok(())
    }
    fn advance(&self, run: &Run, now: i64) -> Result<()> {
        run.definition.validate()?;
        let step = run
            .definition
            .steps
            .get(run.current_step)
            .ok_or_else(invalid)?;
        let previous = self.read(|c| self.current_step(c, run))?;
        if previous.is_none() && run.next_wake_at.is_some_and(|wake| now < wake) {
            return Ok(());
        }
        // Never call a host while holding a database transaction: adapters often
        // submit/poll durable operations in this same StateDatabase. Reconcile
        // results with optimistic compare-and-set after the callback returns.
        let expired = previous.as_ref().is_some_and(|p| {
            now.saturating_sub(p.started_at) >= i64::from(step.timeout_secs) * 1000
        });
        let authorization = if !expired && step.action.mutates() {
            self.port.authorize(&run.execution_principal, &step.action)
        } else {
            Ok(())
        };
        let decision = if let Err(error) = authorization {
            PortDecision::AuthorizationError(error)
        } else if let Some(p) = previous.as_ref().filter(|_| !expired) {
            match p.state.as_str() {
                "submitting" => {
                    let key = format!("automation.{}.{}.{}", run.id, run.current_step, run.attempt);
                    PortDecision::Begun(self.port.begin(
                        &run.execution_principal,
                        &step.action,
                        &key,
                    ))
                }
                "submitted" => PortDecision::Polled(self.port.poll(&ActionHandle(
                    p.operation_reference.clone().ok_or_else(invalid)?,
                ))),
                _ => PortDecision::None,
            }
        } else {
            PortDecision::None
        };
        self.write(|c| {
            let current = self.load_run(c, &run.id)?;
            if current.state != "running"
                || current.current_step != run.current_step
                || current.attempt != run.attempt
                || current.next_wake_at != run.next_wake_at
                || self.current_step(c, run)? != previous
            {
                return Ok(());
            }
            self.advance_decided(c, run, step, previous.as_ref(), now, decision)
        })
    }
    fn current_step(&self, c: &Connection, run: &Run) -> Result<Option<RunStep>> {
        Ok(c.query_row(
            &format!(
                "SELECT {STEP_COLUMNS} FROM run_steps WHERE run_id=? AND index_no=? AND attempt=?"
            ),
            params![run.id, run.current_step as i64, run.attempt],
            step_row,
        )
        .optional()?)
    }
    fn advance_decided(
        &self,
        c: &Connection,
        run: &Run,
        step: &Step,
        previous: Option<&RunStep>,
        now: i64,
        decision: PortDecision,
    ) -> Result<()> {
        // Deadlines also govern persisted delays and authorization outages.
        // A first transient refusal starts a durable pending attempt so outages
        // cannot postpone a step indefinitely or generate notification spam.
        if previous.is_some_and(|p| {
            now.saturating_sub(p.started_at) >= i64::from(step.timeout_secs) * 1000
        }) {
            return self.fail_step(c, run, now, "timed_out", false);
        }
        if let PortDecision::AuthorizationError(error) = decision {
            if error == ActionError::Forbidden {
                return self.suspend(c, run, now);
            }
            if previous.is_none() {
                self.insert_step(c, run, now, "submitting")?;
            }
            return match error {
                ActionError::Unavailable | ActionError::Busy => Ok(()),
                _ => self.fail_step(c, run, now, error.code(), false),
            };
        }
        if let Some(previous) = previous {
            if previous.state == "waiting" {
                if run.next_wake_at.is_some_and(|wake| now < wake) {
                    return Ok(());
                }
                return self.succeed(c, run, now, &serde_json::Value::Null);
            }
            if matches!(previous.state.as_str(), "submitted" | "submitting") {
                if previous.state == "submitting" {
                    let PortDecision::Begun(result) = decision else {
                        return Err(invalid());
                    };
                    return self.submitted(c, run, now, result);
                }
                let PortDecision::Polled(result) = decision else {
                    return Err(invalid());
                };
                match result {
                    Ok(ActionStatus::Running) => return Ok(()),
                    Ok(ActionStatus::Succeeded { output }) => {
                        if !bounded_output(&output) {
                            return self.fail_step(c, run, now, "invalid_output", false);
                        }
                        return self.succeed(c, run, now, &output);
                    }
                    Ok(ActionStatus::Failed { code }) => {
                        return self.fail_step(c, run, now, safe_code(&code), true);
                    }
                    Ok(ActionStatus::Uncertain { .. }) => {
                        return self.fail_step(c, run, now, "uncertain", false);
                    }
                    Err(ActionError::Forbidden) => return self.suspend(c, run, now),
                    Err(ActionError::Unavailable | ActionError::Busy) => return Ok(()),
                    Err(error) => return self.fail_step(c, run, now, error.code(), false),
                }
            }
            return Err(invalid());
        }
        if run.next_wake_at.is_some_and(|wake| now < wake) {
            return Ok(());
        }
        match &step.action {
            Action::Delay { secs } => {
                let wake = now
                    .checked_add(i64::from(*secs) * 1000)
                    .ok_or_else(invalid)?;
                self.insert_step(c, run, now, "waiting")?;
                c.execute(
                    "UPDATE workflow_runs SET next_wake_at=? WHERE id=?",
                    params![wake, run.id],
                )?;
                Ok(())
            }
            Action::Notify {
                severity,
                title,
                body,
            } => {
                self.insert_step(c, run, now, "submitting")?;
                inbox::insert(c, now, *severity, title, body, &run.workflow_id)?;
                self.succeed(c, run, now, &serde_json::Value::Null)
            }
            _ => self.insert_step(c, run, now, "submitting"),
        }
    }
    fn insert_step(&self, c: &Connection, run: &Run, now: i64, state: &str) -> Result<()> {
        c.execute(
            "INSERT INTO run_steps(run_id,index_no,attempt,state,started_at) VALUES (?,?,?,?,?)",
            params![run.id, run.current_step as i64, run.attempt, state, now],
        )?;
        Ok(())
    }
    fn submitted(
        &self,
        c: &Connection,
        run: &Run,
        now: i64,
        result: std::result::Result<ActionHandle, ActionError>,
    ) -> Result<()> {
        match result {
            Ok(handle) => {
                if !super::model::bounded_text(&handle.0, 128, false) {
                    return self.fail_step(c, run, now, "invalid_handle", false);
                }
                c.execute("UPDATE run_steps SET state='submitted',operation_ref=? WHERE run_id=? AND index_no=? AND attempt=?", params![handle.0, run.id, run.current_step as i64, run.attempt])?;
                Ok(())
            }
            Err(ActionError::Forbidden) => self.suspend(c, run, now),
            Err(ActionError::Unavailable | ActionError::Busy) => Ok(()),
            Err(error) => self.fail_step(c, run, now, error.code(), false),
        }
    }
    fn succeed(
        &self,
        c: &Connection,
        run: &Run,
        now: i64,
        output: &serde_json::Value,
    ) -> Result<()> {
        c.execute("UPDATE run_steps SET state='succeeded',output_json=?,finished_at=? WHERE run_id=? AND index_no=? AND attempt=?", params![json(output)?, now, run.id, run.current_step as i64, run.attempt])?;
        let next = match &run.definition.steps[run.current_step].on_success {
            SuccessBranch::Next => Some(run.current_step + 1),
            SuccessBranch::End => None,
            SuccessBranch::Goto { step_id } => Some(target_index(run, step_id)?),
            SuccessBranch::IfOutputEquals {
                key,
                value,
                then_step,
                otherwise,
            } => {
                if output.get(key) == Some(value) {
                    Some(target_index(run, then_step)?)
                } else {
                    otherwise
                        .as_ref()
                        .map(|id| target_index(run, id))
                        .transpose()?
                        .or(Some(run.current_step + 1))
                }
            }
        };
        self.move_to(c, run, now, next)
    }
    fn fail_step(
        &self,
        c: &Connection,
        run: &Run,
        now: i64,
        code: &str,
        retryable: bool,
    ) -> Result<()> {
        let step = &run.definition.steps[run.current_step];
        c.execute("UPDATE run_steps SET state=?,error_code=?,finished_at=? WHERE run_id=? AND index_no=? AND attempt=?", params![if code == "timed_out" {"timed_out"} else {"failed"}, code, now, run.id, run.current_step as i64, run.attempt])?;
        if retryable && run.attempt < step.retries.max_retries {
            let backoff = i64::from(step.retries.backoff_secs) * 1000 * (1_i64 << run.attempt);
            let wake = now.checked_add(backoff).ok_or_else(invalid)?;
            c.execute("UPDATE workflow_runs SET attempt=attempt+1,next_wake_at=?,outcome_code=? WHERE id=?", params![wake, code, run.id])?;
            return Ok(());
        }
        match &step.on_failure {
            FailureBranch::Fail => self.finish(c, run, now, "failed", code),
            FailureBranch::Next => self.move_to(c, run, now, Some(run.current_step + 1)),
            FailureBranch::Goto { step_id } => {
                self.move_to(c, run, now, Some(target_index(run, step_id)?))
            }
        }
    }
    fn move_to(&self, c: &Connection, run: &Run, now: i64, next: Option<usize>) -> Result<()> {
        if let Some(next) = next.filter(|index| *index < run.definition.steps.len()) {
            c.execute("UPDATE workflow_runs SET current_step=?,attempt=0,next_wake_at=?,outcome_code=NULL WHERE id=?", params![next as i64, now, run.id])?;
            Ok(())
        } else {
            self.finish(c, run, now, "succeeded", "completed")
        }
    }
    fn finish(&self, c: &Connection, run: &Run, now: i64, state: &str, code: &str) -> Result<()> {
        c.execute("UPDATE workflow_runs SET state=?,finished_at=?,outcome_code=?,next_wake_at=NULL WHERE id=?", params![state, now, code, run.id])?;
        self.audit(
            c,
            None,
            "automation.finished",
            &run.workflow_id,
            state,
            Some(&run.id),
        )
    }
    fn suspend(&self, c: &Connection, run: &Run, now: i64) -> Result<()> {
        c.execute("UPDATE workflow_runs SET state='suspended',outcome_code='authority_revoked',next_wake_at=NULL WHERE id=?", [&run.id])?;
        inbox::insert(
            c,
            now,
            Severity::Critical,
            "Workflow authority revoked",
            "Schedules cannot preserve old access. This workflow is suspended; an already-started operation is not cancelled.",
            &run.workflow_id,
        )?;
        self.audit(
            c,
            None,
            "automation.suspended",
            &run.workflow_id,
            "authority_revoked",
            Some(&run.id),
        )
    }
}
fn target_index(run: &Run, id: &str) -> Result<usize> {
    run.definition
        .steps
        .iter()
        .position(|s| s.id == id)
        .filter(|index| *index > run.current_step)
        .ok_or_else(invalid)
}
fn safe_code(code: &str) -> &str {
    if code.len() <= 64
        && !code.is_empty()
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        code
    } else {
        "action_failed"
    }
}
