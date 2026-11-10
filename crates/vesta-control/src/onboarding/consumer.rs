//! Installation-local consumer binding and stopped-state observation. Native
//! callers own the lease; observations are never remote update authority.
use crate::{
    config::{HostConfiguration, ListenerDescription},
    files,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
};
use vesta_state::{CURRENT_SCHEMA_VERSION, RunnerLock, StateDirectory, artifacts::ArtifactName};
type Result<T> = std::result::Result<T, &'static str>;
pub const SESSION_PENDING: &str = "operator-session-pending";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u8,
    pub installation_id: String,
    pub server_id: String,
    pub job_id: String,
    pub configuration: String,
    pub state: String,
    pub launch_sha256: String,
    pub host_sha256: String,
    pub installed_manifest_sha256: String,
}
#[derive(Clone)]
pub struct Consumer {
    root: PathBuf,
    record: Record,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct Observation {
    pub installation_id: String,
    pub server_id: String,
    pub state_schema: u32,
    pub supervisor_ipc: u8,
    pub configuration_sha256: String,
    pub launch_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Readiness {
    pub schema: u8,
    pub nonce: String,
    pub pid: u32,
    pub manifest_sha256: String,
    pub tls_sha256: String,
    pub observation: Observation,
}
pub struct StoppedConsumer {
    consumer: Consumer,
    _lease: File,
    _runner: RunnerLock,
    _supervisor: File,
}
fn unavailable<T>(_: T) -> &'static str {
    "update_consumer_unavailable"
}
impl Consumer {
    fn readiness_name(nonce: &str, suffix: &str) -> Result<ArtifactName> {
        if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("update_readiness_invalid");
        }
        ArtifactName::new(&format!("update-ready-{nonce}.{suffix}")).map_err(unavailable)
    }
    pub fn read_readiness(&self, nonce: &str) -> Result<Option<Readiness>> {
        let name = Self::readiness_name(nonce, "json")?;
        let state =
            StateDirectory::new(self.root.join("private/state"), None).map_err(unavailable)?;
        let store = state.artifacts().map_err(unavailable)?;
        let Some(file) = store.open(&name).map_err(unavailable)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take(8193)
            .read_to_end(&mut bytes)
            .map_err(unavailable)?;
        if bytes.len() > 8192 {
            return Err("update_readiness_invalid");
        }
        let proof: Readiness =
            serde_json::from_slice(&bytes).map_err(|_| "update_readiness_invalid")?;
        if proof.schema != 1
            || proof.nonce != nonce
            || proof.pid == 0
            || proof.observation.installation_id != self.record.installation_id
            || proof.observation.server_id != self.record.server_id
        {
            return Err("update_readiness_invalid");
        }
        Ok(Some(proof))
    }
    /// Called by the actual agent after its runtime, TLS and local control are
    /// ready, while its installation lease and session witness remain held.
    /// The nonce/manifest come only from the native bootstrap's private child env.
    pub fn publish_readiness(
        &self,
        config: &HostConfiguration,
        db: &vesta_state::StateDatabase,
        tls_sha256: &str,
        nonce: &str,
        manifest: &str,
    ) -> Result<()> {
        let name = Self::readiness_name(nonce, "json")?;
        let staged = Self::readiness_name(nonce, "staged")?;
        for value in [tls_sha256, manifest] {
            if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("update_readiness_invalid");
            }
        }
        if db.server_id() != self.record.server_id
            || !files::same_directory(db.directory().path(), &self.root.join("private/state"))
                .map_err(unavailable)?
        {
            return Err("update_consumer_binding_invalid");
        }
        let state_schema = db.schema_version().map_err(unavailable)?;
        if state_schema != CURRENT_SCHEMA_VERSION {
            return Err("update_state_schema_unproven");
        }
        let configuration =
            files::read_local_document(config.source(), 65536).map_err(unavailable)?;
        let launch =
            files::read_local_document(config.launch_definition(), 65536).map_err(unavailable)?;
        let observation = Observation {
            installation_id: self.record.installation_id.clone(),
            server_id: self.record.server_id.clone(),
            state_schema,
            supervisor_ipc: vesta_supervisor::wire::VERSION,
            configuration_sha256: hex::encode(Sha256::digest(configuration)),
            launch_sha256: hex::encode(Sha256::digest(launch)),
        };
        let proof = Readiness {
            schema: 1,
            nonce: nonce.into(),
            pid: std::process::id(),
            manifest_sha256: manifest.into(),
            tls_sha256: tls_sha256.into(),
            observation,
        };
        let bytes = serde_json::to_vec(&proof).map_err(unavailable)?;
        let store = db.directory().artifacts().map_err(unavailable)?;
        let mut file = store.create_new(&staged).map_err(unavailable)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(unavailable)?;
        drop(file);
        store.link_new(&staged, &name).map_err(unavailable)?;
        store.remove(&staged).map_err(unavailable)?;
        store.sync().map_err(unavailable)
    }
    /// Absent registry means an ordinary legacy server, not implicit adoption.
    pub fn from_configuration(config: &HostConfiguration) -> Result<Option<Self>> {
        let root = config
            .source()
            .parent()
            .ok_or("update_consumer_unavailable")?;
        let path = root.join("private/consumer.json");
        let bytes = match files::read_local_document(&path, 65536) {
            Ok(b) => b,
            Err(files::FileError::NotFound) => return Ok(None),
            Err(_) => return Err("update_consumer_unavailable"),
        };
        let record: Record = serde_json::from_slice(&bytes).map_err(unavailable)?;
        if record.schema != 1
            || record.installation_id.len() != 32
            || !record
                .installation_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || uuid::Uuid::parse_str(&record.server_id).is_err()
            || uuid::Uuid::parse_str(&record.job_id).is_err()
            || record.configuration != "host.toml"
            || record.state != "private/state"
            || config.source().file_name().is_none_or(|n| n != "host.toml")
            || !files::same_directory(config.base(), root).map_err(unavailable)?
            || !files::same_directory(config.state(), &root.join("private/state"))
                .map_err(unavailable)?
        {
            return Err("update_consumer_binding_invalid");
        }
        for digest in [
            &record.host_sha256,
            &record.launch_sha256,
            &record.installed_manifest_sha256,
        ] {
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("update_consumer_binding_invalid");
            }
        }
        let ListenerDescription::Tls(tls) = config.listener() else {
            return Err("update_consumer_tls_unproven");
        };
        if !tls.bind().is_loopback()
            || tls
                .certificate()
                .file_name()
                .is_none_or(|n| n != "certificate.pem")
            || tls.private_key().file_name().is_none_or(|n| n != "key.pem")
            || !files::same_directory(
                tls.certificate()
                    .parent()
                    .ok_or("update_consumer_tls_unproven")?,
                &root.join("private"),
            )
            .map_err(unavailable)?
            || !files::same_directory(
                tls.private_key()
                    .parent()
                    .ok_or("update_consumer_tls_unproven")?,
                &root.join("private"),
            )
            .map_err(unavailable)?
        {
            return Err("update_consumer_tls_unproven");
        }
        Ok(Some(Self {
            root: root.to_owned(),
            record,
        }))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn record(&self) -> &Record {
        &self.record
    }
    pub fn acquire_lease(&self) -> Result<File> {
        let state =
            StateDirectory::new(self.root.join("private/state"), None).map_err(unavailable)?;
        state
            .artifacts()
            .and_then(|d| d.try_lock_exclusive(&ArtifactName::new("installation-consumer.lock")?))
            .map_err(|_| "update_consumer_busy")
    }
    pub fn stopped(self) -> Result<StoppedConsumer> {
        let lease = self.acquire_lease()?;
        match fs::symlink_metadata(self.root.join(SESSION_PENDING)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err("update_consumer_shutdown_unproven"),
        }
        let state =
            StateDirectory::new(self.root.join("private/state"), None).map_err(unavailable)?;
        let runner = state
            .acquire_runner_lock()
            .map_err(|_| "update_consumer_runtime_busy")?;
        let directory = state
            .supervisor()
            .map_err(|_| "update_supervisor_directory_unavailable")?;
        let supervisor = directory
            .try_lock_exclusive(&ArtifactName::new("supervisor.lock").map_err(unavailable)?)
            .map_err(|_| "update_supervisor_busy")?;
        if vesta_supervisor::record::read_intent(&directory)
            .map_err(|_| "update_supervisor_intent_unavailable")?
            .is_some()
            || vesta_supervisor::record::read(&directory)
                .map_err(|_| "update_supervisor_record_unavailable")?
                .is_some_and(|r| r.held())
        {
            return Err("update_game_stop_unproven");
        }
        let held = StoppedConsumer {
            consumer: self,
            _lease: lease,
            _runner: runner,
            _supervisor: supervisor,
        };
        held.observe()?;
        Ok(held)
    }
    /// Non-mutating probe under the native parent's retained maintenance fences.
    /// Calling this alone proves no lease, stopped process or update authority.
    pub fn observe_readonly(&self) -> Result<Observation> {
        let config = HostConfiguration::load_file(&self.root.join("host.toml"), &self.root)
            .map_err(|_| "update_consumer_configuration_invalid")?;
        let current =
            Self::from_configuration(&config)?.ok_or("update_consumer_binding_invalid")?;
        if current.record.server_id != self.record.server_id
            || current.record.installation_id != self.record.installation_id
        {
            return Err("update_consumer_binding_invalid");
        }
        let state = config
            .validate_host_paths()
            .map_err(|_| "update_consumer_paths_invalid")?
            .state_directory()
            .clone();
        state
            .verify_database_files()
            .map_err(|_| "update_consumer_state_files_unavailable")?;
        // Immutable SQLite is valid only when companions are absent and the
        // native parent retains the runtime/consumer fences. Never ignore a WAL.
        for suffix in ["-wal", "-shm", "-journal"] {
            let path = PathBuf::from(format!("{}{suffix}", state.database_file().display()));
            match fs::symlink_metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                _ => return Err("update_state_not_quiescent"),
            }
        }
        let mut uri = url::Url::from_file_path(state.database_file())
            .map_err(|_| "update_consumer_unavailable")?;
        uri.set_query(Some("mode=ro&immutable=1"));
        let db = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NOFOLLOW
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "update_consumer_sqlite_open_failed")?;
        db.execute_batch("PRAGMA query_only=ON;BEGIN;")
            .map_err(|_| "update_consumer_sqlite_snapshot_failed")?;
        let schema: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|_| "update_consumer_schema_query_failed")?;
        if schema != CURRENT_SCHEMA_VERSION {
            return Err("update_state_schema_unproven");
        }
        let server_id: String = db
            .query_row(
                "SELECT server_id FROM server_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "update_consumer_identity_query_failed")?;
        if server_id != self.record.server_id {
            return Err("update_consumer_binding_invalid");
        }
        for (table, query) in [
            (
                "operations",
                "SELECT EXISTS(SELECT 1 FROM operations WHERE state NOT IN ('succeeded','failed','cancelled'))",
            ),
            (
                "resource_locks",
                "SELECT EXISTS(SELECT 1 FROM resource_locks)",
            ),
            (
                "file_intents",
                "SELECT EXISTS(SELECT 1 FROM file_intents WHERE status IN ('pending','recovery-required'))",
            ),
            (
                "file_trash",
                "SELECT EXISTS(SELECT 1 FROM file_trash WHERE status IN ('pending','recovery-required'))",
            ),
            (
                "backup_jobs",
                "SELECT EXISTS(SELECT 1 FROM backup_jobs WHERE state NOT IN ('succeeded','failed'))",
            ),
            (
                "software_jobs",
                "SELECT EXISTS(SELECT 1 FROM software_jobs WHERE state NOT IN ('succeeded','failed'))",
            ),
            (
                "software_state",
                "SELECT EXISTS(SELECT 1 FROM software_state WHERE recovery_required!=0)",
            ),
            (
                "hosted_pack_state",
                "SELECT EXISTS(SELECT 1 FROM hosted_pack_state WHERE pending IS NOT NULL OR recovery!=0)",
            ),
            (
                "hosted_pack_jobs",
                "SELECT EXISTS(SELECT 1 FROM hosted_pack_jobs WHERE COALESCE(json_extract(body,'$.state'),'unknown') NOT IN ('completed','failed'))",
            ),
            (
                "setup_provisioning",
                "SELECT EXISTS(SELECT 1 FROM setup_provisioning WHERE state IN ('planning','applying','ready'))",
            ),
        ] {
            let exists: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
                    [table],
                    |r| r.get(0),
                )
                .map_err(|_| "update_consumer_table_query_failed")?;
            if exists
                && db
                    .query_row(query, [], |r| r.get::<_, bool>(0))
                    .map_err(|_| match table {
                        "operations" => "update_consumer_operations_query_failed",
                        "resource_locks" => "update_consumer_locks_query_failed",
                        "file_intents" => "update_consumer_file_intents_query_failed",
                        "file_trash" => "update_consumer_trash_query_failed",
                        "backup_jobs" => "update_consumer_backup_query_failed",
                        "software_jobs" => "update_consumer_software_jobs_query_failed",
                        "software_state" => "update_consumer_software_state_query_failed",
                        "hosted_pack_state" => "update_consumer_hosted_state_query_failed",
                        "hosted_pack_jobs" => "update_consumer_hosted_jobs_query_failed",
                        _ => "update_consumer_setup_query_failed",
                    })?
            {
                return Err("update_consumer_writes_unresolved");
            }
        }
        let configuration = files::read_local_document(config.source(), 65536)
            .map_err(|_| "update_consumer_configuration_read_failed")?;
        let launch = files::read_local_document(config.launch_definition(), 65536)
            .map_err(|_| "update_consumer_launch_read_failed")?;
        let parsed = vesta_game_runtime::launch_config::LaunchConfiguration::parse_toml(
            &launch,
            config.base(),
        )
        .map_err(|_| "update_consumer_launch_document_invalid")?;
        if !files::same_directory(parsed.working_directory(), config.game())
            .map_err(|_| "update_consumer_launch_directory_unavailable")?
        {
            return Err("update_consumer_launch_unproven");
        }
        parsed.into_launch_spec(None).map_err(|e| e.code())?;
        let software:Option<String>=if db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='software_state')",[],|r|r.get::<_,bool>(0)).map_err(unavailable)? {db.query_row("SELECT CASE WHEN length(CAST(current_launch AS BLOB))<=1048576 THEN current_launch ELSE NULL END FROM software_state WHERE singleton=1",[],|r|r.get(0)).optional().map_err(unavailable)?}else{None};
        if let Some(software) = software {
            let stored: serde_json::Value = serde_json::from_str(&software).map_err(unavailable)?;
            if stored["document"].as_str() != std::str::from_utf8(&launch).ok() {
                return Err("update_consumer_launch_unproven");
            }
        }
        db.execute_batch("ROLLBACK;").map_err(unavailable)?;
        Ok(Observation {
            installation_id: self.record.installation_id.clone(),
            server_id,
            state_schema: schema,
            supervisor_ipc: vesta_supervisor::wire::VERSION,
            configuration_sha256: hex::encode(Sha256::digest(configuration)),
            launch_sha256: hex::encode(Sha256::digest(launch)),
        })
    }
}
impl StoppedConsumer {
    pub fn observe(&self) -> Result<Observation> {
        self.consumer.observe_readonly()
    }
    pub fn consumer(&self) -> &Consumer {
        &self.consumer
    }
}
