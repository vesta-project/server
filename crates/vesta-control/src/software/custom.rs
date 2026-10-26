use super::*;
use crate::files::{FILE_CHUNK_BYTES, FileBrowser, MAX_WRITE_BYTES, RelativePath};
use serde::{Deserialize, Serialize};

pub(super) struct PendingCustom {
    actor: Subject,
    token_hash: String,
    expires: i64,
    revision: u64,
    candidate: StoredLaunch,
    restore: bool,
}
#[derive(Serialize)]
pub struct CustomJarPlan {
    pub plan_id: String,
    pub confirmation_token: String,
    pub expires_at_ms: i64,
    pub expected_revision: u64,
    pub custom_jar: Option<model::CustomJar>,
    pub main_class: Option<String>,
    pub risks: Vec<&'static str>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomJarRequest {
    pub plan_id: String,
    pub confirmation_token: String,
    pub idempotency_key: String,
}
pub(super) fn probe(
    files: &FileBrowser,
    subject: &Subject,
    path: &str,
) -> Result<(Vec<u8>, String)> {
    if !path.ends_with(".jar") {
        return Err(SoftwareError::InvalidRequest);
    }
    let path = RelativePath::parse(path).map_err(|_| SoftwareError::InvalidRequest)?;
    let mut stream = files
        .open_stream(subject, &path)
        .map_err(|_| SoftwareError::InvalidRequest)?;
    if stream.size() > MAX_WRITE_BYTES as u64 * 16 {
        return Err(SoftwareError::InvalidRequest);
    }
    let manifest = files
        .custom_jar_manifest(subject, &path)
        .map_err(|_| SoftwareError::InvalidRequest)?;
    let mut digest = Sha256::new();
    let mut cursor = 0;
    while cursor < stream.size() {
        let bytes = stream
            .read_chunk(cursor, FILE_CHUNK_BYTES)
            .map_err(|_| SoftwareError::InvalidRequest)?;
        cursor += bytes.len() as u64;
        digest.update(bytes);
    }
    if !stream
        .unchanged()
        .map_err(|_| SoftwareError::InvalidRequest)?
    {
        return Err(SoftwareError::RevisionConflict);
    }
    Ok((digest.finalize().to_vec(), manifest))
}
impl SoftwareHandle {
    pub fn prepare_custom_jar(
        &self,
        subject: &Subject,
        path: Option<String>,
        expected_revision: u64,
    ) -> Result<CustomJarPlan> {
        self.authorize(subject)?;
        let state = self
            .inner
            .db
            .read_with(|tx| store::state(tx.connection()))?;
        if state.revision != expected_revision {
            return Err(SoftwareError::RevisionConflict);
        }
        if state.recovery {
            return Err(SoftwareError::RecoveryRequired);
        }
        let restore = path.is_none();
        let (candidate, main_class) = if let Some(path) = path {
            let files = FileBrowser::open(self.inner.db.clone(), &self.inner.game, &[])
                .map_err(|_| SoftwareError::InvalidRequest)?;
            let (hash, main_class) = probe(&files, subject, &path)?;
            let absolute = self.inner.game.join(&path);
            let size = std::fs::metadata(&absolute)
                .map_err(|_| SoftwareError::InvalidRequest)?
                .len();
            let java = self.selected_java_path(subject)?;
            let launch = vesta_server_install::ManagedLaunch {
                java,
                working_directory: self.inner.game.clone(),
                args: vec![
                    "-jar".into(),
                    absolute.to_string_lossy().into(),
                    "nogui".into(),
                ],
                required_files: vec![absolute.clone()],
                artifacts: vec![vesta_server_install::ManagedArtifact {
                    path: absolute,
                    size,
                    sha256: hex::encode(hash),
                }],
            };
            let candidate = StoredLaunch {
                installation: Installation {
                    custom_jar: Some(model::CustomJar { path }),
                    loader_version: None,
                    software: None,
                    version: state.current.installation.version.clone(),
                    build: None,
                    installation_id: "custom_jar".into(),
                },
                document: self.inner.runtime.launch_document(&launch)?,
                artifacts: launch.artifacts,
            };
            (candidate, Some(main_class))
        } else {
            let candidate = self.inner.db.read_with(|tx| -> Result<_> {
                let raw: String = tx
                    .connection()
                    .query_row(
                        "SELECT launch_json FROM software_managed_launch WHERE singleton=1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|_| SoftwareError::PreviousUnavailable)?;
                store::decode(&raw)
            })?;
            let mut candidate: StoredLaunch = candidate;
            let java = self.selected_java_path(subject)?;
            let mut tree: toml::Value =
                toml::from_str(&candidate.document).map_err(|_| SoftwareError::StateUnavailable)?;
            let table = tree
                .get_mut("launch")
                .and_then(toml::Value::as_table_mut)
                .ok_or(SoftwareError::StateUnavailable)?;
            let key = if table.get("kind").and_then(toml::Value::as_str) == Some("native") {
                "executable"
            } else {
                "java-command"
            };
            table.insert(
                key.into(),
                toml::Value::String(java.to_string_lossy().replace('\\', "/")),
            );
            candidate.document =
                toml::to_string(&tree).map_err(|_| SoftwareError::StateUnavailable)?;
            (candidate, None)
        };
        let token = ConfirmationToken::random().map_err(|_| SoftwareError::RuntimeUnavailable)?;
        let expires = self.inner.db.clock().millis() + PLAN_TTL_MS;
        let id = uuid::Uuid::new_v4().to_string();
        let mut plans = self
            .inner
            .custom_plans
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        plans.retain(|_, p| p.expires > self.inner.db.clock().millis());
        if plans.len() >= MAX_PLANS {
            return Err(SoftwareError::Busy);
        }
        let custom_jar = candidate.installation.custom_jar.clone();
        plans.insert(
            id.clone(),
            PendingCustom {
                actor: subject.clone(),
                token_hash: token.hash().to_string(),
                expires,
                revision: expected_revision,
                candidate,
                restore,
            },
        );
        Ok(CustomJarPlan {
            plan_id: id,
            confirmation_token: token.expose_secret().into(),
            expires_at_ms: expires,
            expected_revision,
            custom_jar,
            main_class,
            risks: vec![
                "executes_owner_selected_server_jar",
                "review_backup_before_software_change",
            ],
        })
    }
    pub fn apply_custom_jar(&self, subject: &Subject, request: CustomJarRequest) -> Result<Job> {
        self.authorize(subject)?;
        valid_key(&request.idempotency_key)?;
        let token_hash = hash(request.confirmation_token.as_bytes());
        let request_hash = hash(
            serde_json::json!(["custom_jar", request.plan_id, token_hash])
                .to_string()
                .as_bytes(),
        );
        let mut queue = self
            .inner
            .queue
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?;
        if queue.stop {
            return Err(SoftwareError::RuntimeUnavailable);
        }
        if let Some(job) = self.inner.db.read_with(|tx| {
            store::existing(
                tx.connection(),
                subject,
                &request.idempotency_key,
                &request_hash,
            )
        })? {
            return Ok(job);
        }
        let plan = self
            .inner
            .custom_plans
            .lock()
            .map_err(|_| SoftwareError::StateUnavailable)?
            .remove(&request.plan_id)
            .ok_or(SoftwareError::PlanExpired)?;
        use subtle::ConstantTimeEq;
        if plan.actor != *subject
            || !bool::from(plan.token_hash.as_bytes().ct_eq(token_hash.as_bytes()))
        {
            return Err(SoftwareError::ConfirmationInvalid);
        }
        if plan.expires <= self.inner.db.clock().millis() {
            return Err(SoftwareError::PlanExpired);
        }
        let job = self.inner.db.write_with(|tx| -> Result<_> {
            let c = tx.connection();
            store::authorize(c, &self.inner.db, subject)?;
            let state = store::state(c)?;
            if state.revision != plan.revision {
                return Err(SoftwareError::RevisionConflict);
            }
            if state.recovery {
                return Err(SoftwareError::RecoveryRequired);
            }
            store::admit(
                c,
                &self.inner.db,
                &self.inner.audit,
                subject,
                if plan.restore {
                    "restore_managed"
                } else {
                    "custom_jar"
                },
                &request.idempotency_key,
                &request_hash,
                plan.revision,
            )
        })?;
        queue.work.push_back(Work {
            id: job.id.clone(),
            actor: subject.clone(),
            kind: WorkKind::CustomLaunch(Box::new(plan.candidate), plan.restore),
        });
        self.inner.wake.notify_one();
        Ok(job)
    }
}
