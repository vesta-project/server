use super::{Inner, Result, SoftwareError, Work, WorkKind, custom, model::StoredLaunch, store};
use rusqlite::params;
use std::sync::Arc;
use vesta_server_install::ProgressReporter;

struct Reporter<'a>(&'a Inner);
impl ProgressReporter for Reporter<'_> {
    fn start_step(&self, _: &str, _: Option<u32>) {}
    fn update_bytes(&self, _: u64, _: Option<u64>) {}
    fn done(&self, _: bool, _: Option<&str>) {}
    fn is_cancelled(&self) -> bool {
        self.0.cancel.load(std::sync::atomic::Ordering::Acquire)
    }
    fn is_paused(&self) -> bool {
        false
    }
    fn is_dry_run(&self) -> bool {
        false
    }
}

pub(super) fn run(inner: Arc<Inner>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    loop {
        let mut work = {
            let mut q = inner.queue.lock().unwrap_or_else(|p| p.into_inner());
            while q.work.is_empty() && !q.stop {
                q = inner.wake.wait(q).unwrap_or_else(|p| p.into_inner());
            }
            if q.work.is_empty() {
                return;
            }
            q.work.pop_front().expect("queued software work")
        };
        let result = match &runtime {
            Ok(runtime) => runtime.block_on(execute(&inner, &mut work)),
            Err(_) => Err(SoftwareError::RuntimeUnavailable),
        };
        if let Err(error) = result {
            let _ = fail(&inner, &work, error);
        }
        #[cfg(test)]
        if let Some(probe) = inner.completion_probe.lock().unwrap().take() {
            let _ = probe.send(());
        }
    }
}

async fn execute(inner: &Inner, work: &mut Work) -> Result<()> {
    if inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(SoftwareError::RuntimeUnavailable);
    }
    let expected = inner.db.write_with(|tx| -> Result<u64> {
        let c = tx.connection();
        store::authorize(c, &inner.db, &work.actor)?;
        let (state, expected): (String, i64) = c.query_row(
            "SELECT state,expected_revision FROM software_jobs WHERE id=?",
            [&work.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if state != "queued" {
            return Err(SoftwareError::StateUnavailable);
        }
        c.execute(
            "UPDATE software_jobs SET state='staging',updated_at_ms=? WHERE id=?",
            params![inner.db.clock().millis(), work.id],
        )?;
        u64::try_from(expected).map_err(|_| SoftwareError::StateUnavailable)
    })?;
    if let WorkKind::JavaDownload(config, major) = &work.kind {
        crate::java::download(config, *major).await?;
        inner.db.write_with(|tx| -> Result<()> {
            let c = tx.connection();
            store::authorize(c, &inner.db, &work.actor)?;
            c.execute(
                "UPDATE software_jobs SET state='succeeded',updated_at_ms=? WHERE id=?",
                params![inner.db.clock().millis(), work.id],
            )?;
            store::audit(
                c,
                &inner.audit,
                &work.actor,
                "java.download",
                &work.id,
                "succeeded",
            )
        })?;
        return Ok(());
    }
    if let WorkKind::Pack(pack, target) = &work.kind {
        let _hold = inner.runtime.acquire_stopped()?;
        if inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        pack.workspace
            .prepare_install_payloads(&work.actor, &pack.input, &pack.files)
            .map_err(|error| pack_failure(inner, work, "preparation", error))?;
        if inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        pack.workspace
            .install_apply(
                &work.actor,
                &target.minecraft_version,
                Some(&target.software),
                &pack.key,
                &pack.input,
                &pack.files,
            )
            .map_err(|error| pack_failure(inner, work, "application", error))?;
        inner.db.write_with(|tx| -> Result<()> {
            tx.connection().execute(
                "UPDATE software_jobs SET state='succeeded',updated_at_ms=? WHERE id=?",
                params![inner.db.clock().millis(), work.id],
            )?;
            Ok(())
        })?;
        return Ok(());
    }
    if let WorkKind::Apply(_, _, Some(pack)) = &work.kind {
        let target = pack
            .workspace
            .install_switch_review(&work.actor, &pack.input)
            .map_err(|_| SoftwareError::ConfirmationInvalid)?;
        let _hold = inner.runtime.acquire_stopped()?;
        if inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        pack.workspace
            .prepare_install_payloads(&work.actor, &pack.input, &pack.files)
            .map_err(|error| pack_failure(inner, work, "preparation", error))?;
        if inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        if !target.software_switch_required {
            pack.workspace
                .install_apply(
                    &work.actor,
                    &target.minecraft_version,
                    Some(&target.software),
                    &pack.key,
                    &pack.input,
                    &pack.files,
                )
                .map_err(|error| pack_failure(inner, work, "application", error))?;
            inner.db.write_with(|tx| -> Result<()> {
                tx.connection().execute(
                    "UPDATE software_jobs SET state='succeeded',updated_at_ms=? WHERE id=?",
                    params![inner.db.clock().millis(), work.id],
                )?;
                Ok(())
            })?;
            return Ok(());
        }
    }
    let mut source_hold = match &work.kind {
        WorkKind::Apply(plan, _, _) if plan.requires_stopped_build() => {
            Some(inner.runtime.acquire_stopped()?)
        }
        _ => None,
    };
    let candidate = match &mut work.kind {
        WorkKind::Apply(plan, approval, _) => {
            let approval = approval.take().ok_or(SoftwareError::ConfirmationInvalid)?;
            let outcome = inner
                .installer
                .install(plan, approval, &Reporter(inner))
                .await?;
            let warnings = match outcome {
                vesta_server_install::InstallOutcome::Installed(receipt) => receipt
                    .warnings
                    .into_iter()
                    .filter(|warning| !plan.warnings().contains(warning))
                    .collect(),
                vesta_server_install::InstallOutcome::DryRun => {
                    return Err(SoftwareError::RuntimeUnavailable);
                }
            };
            let launch = inner.installer.managed_launch(plan, &inner.game).await?;
            let document = inner.runtime.launch_document(&launch)?;
            if document.is_empty() || document.len() > 65_536 {
                return Err(SoftwareError::RuntimeUnavailable);
            }
            StoredLaunch {
                warnings,
                installation: super::Installation {
                    custom_jar: None,
                    loader_version: plan.loader_version().map(str::to_owned),
                    software: Some(plan.software()),
                    version: Some(plan.version().into()),
                    build: plan.build(),
                    installation_id: plan
                        .destination()
                        .file_name()
                        .and_then(|p| p.to_str())
                        .ok_or(SoftwareError::RuntimeUnavailable)?
                        .into(),
                },
                document,
                artifacts: launch.artifacts,
            }
        }
        WorkKind::JavaDownload(..) | WorkKind::Pack(..) => unreachable!(),
        WorkKind::CustomLaunch(candidate, _) => {
            vesta_server_install::verify_managed_artifacts(&candidate.artifacts).await?;
            if let Some(custom) = &candidate.installation.custom_jar {
                let files = crate::files::FileBrowser::open(inner.db.clone(), &inner.game, &[])
                    .map_err(|_| SoftwareError::InvalidRequest)?;
                custom::probe(&files, &work.actor, &custom.path)?;
            }
            candidate.as_ref().clone()
        }
        WorkKind::JavaSelect(path) => {
            crate::java::probe(path, false).await?;
            let mut candidate = inner
                .db
                .read_with(|tx| -> Result<_> { Ok(store::state(tx.connection())?.current) })?;
            let mut tree: toml::Value =
                toml::from_str(&candidate.document).map_err(|_| SoftwareError::StateUnavailable)?;
            let launch = tree
                .get_mut("launch")
                .and_then(toml::Value::as_table_mut)
                .ok_or(SoftwareError::StateUnavailable)?;
            let key = if launch.get("kind").and_then(toml::Value::as_str) == Some("native") {
                "executable"
            } else {
                "java-command"
            };
            launch.insert(
                key.into(),
                toml::Value::String(
                    path.to_str()
                        .ok_or(SoftwareError::InvalidRequest)?
                        .replace('\\', "/"),
                ),
            );
            candidate.document =
                toml::to_string(&tree).map_err(|_| SoftwareError::StateUnavailable)?;
            candidate
        }
        WorkKind::Rollback => inner.db.read_with(|tx| -> Result<_> {
            store::authorize(tx.connection(), &inner.db, &work.actor)?;
            store::state(tx.connection())?
                .previous
                .ok_or(SoftwareError::PreviousUnavailable)
        })?,
    };
    if matches!(work.kind, WorkKind::Rollback) {
        if candidate.artifacts.is_empty()
            && !inner
                .db
                .read_with(|tx| -> Result<bool> { Ok(store::state(tx.connection())?.recovery) })?
        {
            return Err(SoftwareError::PreviousUnavailable);
        }
        vesta_server_install::verify_managed_artifacts(&candidate.artifacts).await?;
    }
    if inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(SoftwareError::RuntimeUnavailable);
    }
    // All provider/download/hash work finishes before taking the tiny stopped
    // activation exclusion. The guard must also refuse a pending crash restart.
    let mut guard = match source_hold.take() {
        Some(guard) => guard,
        None => inner.runtime.acquire_stopped()?,
    };
    if inner.cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(SoftwareError::RuntimeUnavailable);
    }
    inner.db.write_with(|tx| -> Result<()> {
        let c=tx.connection();
        store::authorize(c,&inner.db,&work.actor)?;
        let s=store::state(c)?;
        if s.revision!=expected {return Err(SoftwareError::RevisionConflict);}
        if !matches!(work.kind,WorkKind::Rollback) && s.recovery {return Err(SoftwareError::RecoveryRequired);}
        // Previous is written BEFORE touching the actual launch. During recovery
        // rollback always targets the known old launch, even after a partial write.
        if !matches!(work.kind,WorkKind::Rollback) {
            if matches!(work.kind,WorkKind::CustomLaunch(_,false)) && s.current.installation.custom_jar.is_none() {
                c.execute("INSERT OR REPLACE INTO software_managed_launch VALUES(1,?)",[store::encode(&s.current)?])?;
            }
            c.execute("UPDATE software_state SET previous_launch=current_launch,recovery_required=1 WHERE singleton=1",[])?;
        } else {
            c.execute("UPDATE software_state SET recovery_required=1 WHERE singleton=1",[])?;
        }
        c.execute("UPDATE software_jobs SET state='activating',candidate_launch=?,updated_at_ms=? WHERE id=?",params![store::encode(&candidate)?,inner.db.clock().millis(),work.id])?;
        if !candidate.warnings.is_empty() {
            let warnings = serde_json::to_string(&candidate.warnings).map_err(|_| SoftwareError::StateUnavailable)?;
            c.execute("INSERT INTO software_job_results(job_id,warnings_json) VALUES(?,?)", params![work.id,warnings])?;
        }
        store::audit(c,&inner.audit,&work.actor,"software.activate",&work.id,"pending")?;
        Ok(())
    })?;
    inner.runtime.recovery_block(true)?;
    // Errors from this point are ambiguous by design. Keep the durable previous
    // launch and start block; never automatically replay or reverse the effect.
    guard.activate(&candidate.document)?;
    if let WorkKind::Apply(_, _, Some(pack)) = &work.kind {
        // Retain recovery and stopped exclusion until pack staging succeeds.
        inner.db.write_with(|tx| -> Result<()> {
            tx.connection().execute(
                "UPDATE software_state SET current_launch=? WHERE singleton=1",
                [store::encode(&candidate)?],
            )?;
            Ok(())
        })?;
        let mc = candidate
            .installation
            .version
            .as_deref()
            .ok_or(SoftwareError::StateUnavailable)?;
        let family = serde_json::to_value(candidate.installation.software)
            .map_err(|_| SoftwareError::StateUnavailable)?;
        pack.workspace
            .install_apply(
                &work.actor,
                mc,
                family.as_str(),
                &pack.key,
                &pack.input,
                &pack.files,
            )
            .map_err(|error| {
                let _ = pack_failure(inner, work, "application", error);
                SoftwareError::RecoveryRequired
            })?;
    }
    inner.db.write_with(|tx| -> Result<()> {
        let c=tx.connection();
        // Authority can expire while the local activation call runs. That cannot
        // undo its effect; refuse completion and require explicit recovery.
        store::authorize(c,&inner.db,&work.actor)?;
        let s=store::state(c)?;
        if s.revision!=expected {return Err(SoftwareError::RevisionConflict);}
        let revision=i64::try_from(expected.checked_add(1).ok_or(SoftwareError::StateUnavailable)?).map_err(|_|SoftwareError::StateUnavailable)?;
        if matches!(work.kind,WorkKind::Rollback) && !s.recovery {
            return Err(SoftwareError::StateUnavailable);
        }
        c.execute("UPDATE software_state SET current_launch=?,revision=?,recovery_required=0 WHERE singleton=1",params![store::encode(&candidate)?,revision])?;
        // Resolve earlier ambiguous jobs only after the rollback is durable.
        if matches!(work.kind,WorkKind::Rollback) {
            c.execute("UPDATE software_jobs SET state='failed',error='software_recovered_by_rollback',updated_at_ms=? WHERE state='recovery_required'",[inner.db.clock().millis()])?;
        }
        c.execute("UPDATE software_jobs SET state='succeeded',error=NULL,updated_at_ms=? WHERE id=?",params![inner.db.clock().millis(),work.id])?;
        store::audit(c,&inner.audit,&work.actor,"software.complete",&work.id,"succeeded")?;
        Ok(())
    })?;
    inner.runtime.recovery_block(false)?;
    Ok(())
}

pub(super) fn fail(inner: &Inner, work: &Work, error: SoftwareError) -> Result<()> {
    let ambiguous = inner.db.write_with(|tx| -> Result<bool> {
        let c = tx.connection();
        let job = store::job(c, &work.id)?;
        let ambiguous = job.state == "activating" || job.state == "succeeded";
        let state = if ambiguous {
            "recovery_required"
        } else {
            "failed"
        };
        c.execute(
            "UPDATE software_jobs SET state=?,error=?,updated_at_ms=? WHERE id=?",
            params![state, error.code(), inner.db.clock().millis(), work.id],
        )?;
        if ambiguous {
            c.execute(
                "UPDATE software_state SET recovery_required=1 WHERE singleton=1",
                [],
            )?;
        }
        store::audit(
            c,
            &inner.audit,
            &work.actor,
            "software.complete",
            &work.id,
            state,
        )?;
        Ok(ambiguous)
    })?;
    if ambiguous {
        inner.runtime.recovery_block(true)?;
    }
    Ok(())
}

pub(super) fn pack_failure(
    inner: &Inner,
    work: &Work,
    phase: &str,
    error: crate::packs::ReleaseError,
) -> SoftwareError {
    // Only bounded public identifiers/codes. Never persist a URL, body, header or key.
    let mut context = serde_json::json!({"code":error.code(),"phase":phase});
    match &error {
        crate::packs::ReleaseError::ProviderFile { details, .. } => {
            for (key, value) in [
                ("provider", Some(details.provider.as_str())),
                ("project_id", Some(details.project_id.as_str())),
                ("version_id", Some(details.version_id.as_str())),
                ("filename", details.filename.as_deref()),
                ("stage", Some(details.stage)),
                ("reason", Some(details.reason)),
            ] {
                if let Some(value) = value {
                    context[key] = value.chars().take(1024).collect::<String>().into();
                }
            }
        }
        crate::packs::ReleaseError::Refusal { rule, item, .. } => {
            context["reason"] = (*rule).into();
            // Archive entry paths are useful diagnostics. URL-like descriptions
            // and control characters are excluded rather than logged verbatim.
            if !item.contains("://") && !item.chars().any(char::is_control) {
                context["path"] = item.chars().take(1024).collect::<String>().into();
            }
        }
        _ => {}
    }
    let pack = match &work.kind {
        WorkKind::Pack(pack, _) | WorkKind::Apply(_, _, Some(pack)) => Some(pack),
        _ => None,
    };
    if let Some(pack) = pack {
        let source: std::result::Result<String, _> = inner.db.read(|c| {
            c.query_row(
                "SELECT json_extract(body,'$.source') FROM pack_install_plans WHERE id=?",
                [&pack.input.plan_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        });
        if let Ok(source) = source
            && let Ok(crate::packs::workspace::InstallSource::Provider {
                provider,
                project_id,
                version_id,
            }) = serde_json::from_str(&source)
        {
            for (name, value) in [
                ("pack_provider", provider),
                ("pack_project_id", project_id),
                ("pack_version_id", version_id),
            ] {
                context[name] = value.chars().take(128).collect::<String>().into();
            }
        }
    }
    let stored = inner.db.write(|c| {
        c.execute(
            "INSERT OR REPLACE INTO software_job_failures VALUES(?,?)",
            params![work.id, context.to_string()],
        )
        .map(|_| ())
        .map_err(Into::into)
    });
    if stored.is_err() {
        return SoftwareError::StateUnavailable;
    }
    error.into()
}
