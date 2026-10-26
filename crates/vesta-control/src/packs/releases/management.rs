//! Atomic authorized pack mutations and durable exact-key outcomes.
use super::*;

/// Canonical desired revision change, never a source/path/hash grant.
pub struct PackMutation {
    key: String,
    input: String,
    workspace: Option<WorkspacePublication>,
}
struct WorkspacePublication {
    required_update: bool,
    software_revision: Option<i64>,
    draft_revision: u64,
    manifest_json: String,
    acknowledge_lower_version: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackOutcome {
    pub hosted_revision: u64,
    pub minimum_revision: u64,
}
impl PackMutation {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn workspace_publish(
        key: String,
        expected: u64,
        input_sha256: &str,
        draft_revision: u64,
        manifest_json: String,
        acknowledge_lower_version: bool,
        software_revision: Option<i64>,
        required_update: bool,
    ) -> Result<Self, ReleaseError> {
        let mut request = Self::new(key, format!("publish:{expected}:{input_sha256}"))?;
        request.workspace = Some(WorkspacePublication {
            required_update,
            software_revision,
            draft_revision,
            manifest_json,
            acknowledge_lower_version,
        });
        Ok(request)
    }
    pub fn publish(key: String, expected: u64) -> Result<Self, ReleaseError> {
        Self::new(key, format!("publish:{expected}"))
    }
    pub fn minimum(key: String, expected: u64, minimum: u64) -> Result<Self, ReleaseError> {
        Self::new(key, format!("minimum:{expected}:{minimum}"))
    }
    fn new(key: String, input: String) -> Result<Self, ReleaseError> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|v| v.is_ascii_graphic()) {
            return Err(ReleaseError::Conflict);
        }
        Ok(Self {
            key,
            input,
            workspace: None,
        })
    }
}
impl PackReleases {
    fn remote_authorize(
        &self,
        connection: &Connection,
        subject: &Subject,
        request: &PackMutation,
    ) -> Result<(), ReleaseError> {
        let capability = if request.input.starts_with("publish:") {
            Capability::PackPublish
        } else {
            Capability::PackForceUpdate
        };
        let policy = identity::policy_in(
            connection,
            self.db.server_id(),
            self.db.clock().millis(),
            Some(subject),
        )?;
        if let Decision::Deny(reason) = policy.check(capability, None) {
            return Err(ControlError::forbidden(reason).into());
        }
        if request.workspace.is_some() {
            for cap in [Capability::PackEdit, Capability::PackBuild] {
                if let Decision::Deny(reason) = policy.check(cap, None) {
                    return Err(ControlError::forbidden(reason).into());
                }
            }
        }
        if request
            .workspace
            .as_ref()
            .is_some_and(|w| w.required_update)
            && let Decision::Deny(reason) = policy.check(Capability::PackForceUpdate, None)
        {
            return Err(ControlError::forbidden(reason).into());
        }
        Ok(())
    }
    fn remote_in(
        &self,
        connection: &Connection,
        subject: &Subject,
        request: &PackMutation,
    ) -> Result<Option<PackOutcome>, ReleaseError> {
        self.remote_authorize(connection, subject, request)?;

        let row: Option<(String, i64, i64)> = connection.query_row(
            "SELECT input_json,revision,minimum_revision FROM management_pack_outcomes WHERE server_id=? AND principal_id=? AND request_key=?",
            params![self.db.server_id(), subject.principal.as_str(), request.key], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))
        ).optional()?;
        match row {
            Some((input, _, _)) if input != request.input => Err(ReleaseError::IdempotencyConflict),
            Some((_, revision, minimum)) => Ok(Some(PackOutcome {
                hosted_revision: revision as u64,
                minimum_revision: minimum as u64,
            })),
            None => Ok(None),
        }
    }
    pub fn remote_outcome(
        &self,
        subject: &Subject,
        request: &PackMutation,
    ) -> Result<Option<PackOutcome>, ReleaseError> {
        Ok(self
            .db
            .read_with(|snapshot| self.remote_in(snapshot.connection(), subject, request))?)
    }
    pub(super) fn record_remote(
        &self,
        connection: &Connection,
        subject: &Subject,
        request: &PackMutation,
        revision: u64,
        minimum: u64,
    ) -> Result<(), ReleaseError> {
        self.remote_authorize(connection, subject, request)?;
        if let Some(workspace) = &request.workspace {
            if crate::software::SoftwareHandle::pack_target_in(connection)?.map(|(r, _)| r)
                != workspace.software_revision
            {
                return Err(ReleaseError::Conflict);
            }
            let current: Option<i64> = connection
                .query_row(
                    "SELECT revision FROM pack_workspace WHERE server_id=?",
                    [self.db.server_id()],
                    |r| r.get(0),
                )
                .optional()?;
            if current.unwrap_or(0) as u64 != workspace.draft_revision {
                return Err(ReleaseError::Conflict);
            }
            connection.execute("INSERT INTO pack_workspace_published(server_id,release_revision,draft_revision,manifest_json) VALUES(?,?,?,?) ON CONFLICT(server_id) DO UPDATE SET release_revision=excluded.release_revision,draft_revision=excluded.draft_revision,manifest_json=excluded.manifest_json",params![self.db.server_id(),to_sql(revision)?,to_sql(workspace.draft_revision)?,workspace.manifest_json])?;
        }
        connection.execute("INSERT INTO management_pack_outcomes(server_id,principal_id,request_key,input_json,revision,minimum_revision) VALUES(?,?,?,?,?,?)",
            params![self.db.server_id(),subject.principal.as_str(),request.key,request.input,to_sql(revision)?,to_sql(minimum)?])?;
        Ok(())
    }
    /// Configured immutable preparation; publication and receipt commit with the existing success audit.
    pub fn publish_remote(
        &self,
        subject: &Subject,
        plan: &ReleasePlan,
        pack: &super::super::PreparedPack,
        request: &PackMutation,
    ) -> Result<PackOutcome, ReleaseError> {
        if request.input != format!("publish:{}", plan.previous)
            && !(request.workspace.is_some()
                && request
                    .input
                    .starts_with(&format!("publish:{}:", plan.previous)))
        {
            return Err(ReleaseError::Conflict);
        }
        if let Some(outcome) = self.remote_outcome(subject, request)? {
            return Ok(outcome);
        }
        let metadata = pack.manifest.metadata();
        if metadata.revision() != plan.revision
            || metadata.minimum_revision() != plan.minimum_revision
        {
            return Err(ReleaseError::RevisionMismatch);
        }
        let acknowledge_lower_version = request
            .workspace
            .as_ref()
            .is_some_and(|w| w.acknowledge_lower_version);
        self.preflight(subject, plan, metadata.version(), acknowledge_lower_version)?;
        let publication = match &pack.content {
            super::super::configured::PreparedContent::Scanned(built) => {
                self.artifacts.publish_vesta_tracked(built)?
            }
            super::super::configured::PreparedContent::Archive(archive) => {
                self.artifacts.publish_archive_tracked(archive)?
            }
            super::super::configured::PreparedContent::Platform { bytes, hash } => {
                self.artifacts.publish_platform_tracked(bytes, hash)?
            }
        };
        match self.record_publication(
            subject,
            plan,
            publication,
            metadata.version(),
            acknowledge_lower_version,
            Some(request),
        ) {
            Ok(release) => Ok(PackOutcome {
                hosted_revision: release.revision(),
                minimum_revision: release.minimum_revision(),
            }),
            Err(error) => match self.remote_outcome(subject, request)? {
                Some(outcome) => Ok(outcome),
                None => Err(error),
            },
        }
    }
    /// Monotonic server compatibility policy. Artifacts/history are never rewritten.
    pub fn minimum_remote(
        &self,
        subject: &Subject,
        expected: u64,
        minimum: u64,
        request: &PackMutation,
    ) -> Result<PackOutcome, ReleaseError> {
        if request.input != format!("minimum:{expected}:{minimum}") {
            return Err(ReleaseError::Conflict);
        }
        let outcome = self.db.write_with(|transaction| {
            let connection = transaction.connection();
            if let Some(outcome) = self.remote_in(connection, subject, request)? { return Ok(outcome); }
            let release = current(connection, self.db.server_id())?.ok_or(ReleaseError::Conflict)?;
            if release.revision() != expected || minimum < release.minimum_revision() || minimum > release.revision() { return Err(ReleaseError::Conflict); }
            connection.execute("INSERT INTO pack_minimum_policy(server_id,minimum_revision) VALUES(?,?) ON CONFLICT(server_id) DO UPDATE SET minimum_revision=excluded.minimum_revision", params![self.db.server_id(),to_sql(minimum)?])?;
            self.record_remote(connection, subject, request, expected, minimum)?;
            self.audit.append(connection, &AuditRecord::new(actor(subject)?, "pack_release.minimum".into(), TARGET.into(), "success".into(), None, [("minimum_revision".into(),Some(minimum.to_string()))])?)?;
            Ok(PackOutcome { hosted_revision: expected, minimum_revision: minimum })
        });
        let outcome = self.audit_refusal(
            subject,
            "pack_release.minimum",
            outcome.map_err(ReleaseError::from),
        )?;
        self.changes.send_if_modified(|revision| {
            if *revision <= outcome.hosted_revision {
                *revision = outcome.hosted_revision;
                true
            } else {
                false
            }
        });
        Ok(outcome)
    }
}
