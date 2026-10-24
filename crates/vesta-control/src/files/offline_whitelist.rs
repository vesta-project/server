//! Structured stopped-game whitelist edits using the existing recoverable file journal.
use super::*;
use crate::{moderation, operations::RuntimeHandle};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::Duration,
};

const LIMIT: usize = 1024 * 1024;
const LEASE: Duration = Duration::from_secs(60);

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineWhitelistPatch {
    pub player: String,
    pub add: bool,
    /// None means the file was absent, not an unconditional overwrite.
    pub expected_sha256: Option<String>,
}

pub struct StoppedGuard {
    _block: crate::operations::StartBlock,
    runtime: RuntimeHandle,
    supervisor: Option<(vesta_supervisor::SupervisorClient, String)>,
    renewer: Mutex<Option<crate::backups::Renewer>>,
    properties: Option<(std::path::PathBuf, Vec<u8>)>,
}
impl StoppedGuard {
    pub(crate) fn supervisor_token(&self) -> Option<String> {
        self.supervisor.as_ref().map(|(_, token)| token.clone())
    }
    pub fn acquire(
        runtime: &RuntimeHandle,
        state: &vesta_state::StateDirectory,
    ) -> Result<Arc<Self>> {
        let block = runtime.block_starts();
        let supervised = runtime.is_supervised();
        let (supervisor, renewer) = if supervised {
            let client = vesta_supervisor::SupervisorClient::new(state)
                .map_err(|_| FileError::Unavailable)?;
            let token = vesta_supervisor::save::hold(&client, LEASE)
                .map_err(|_| FileError::Conflict(None))?;
            let renew_token = token.clone();
            let renewer = crate::backups::Renewer::start(state.clone(), move |client| {
                vesta_supervisor::save::renew_hold(client, &renew_token, LEASE)
            });
            (Some((client, token)), Some(renewer))
        } else {
            (None, None)
        };
        let guard = Arc::new(Self {
            _block: block,
            runtime: runtime.clone(),
            supervisor,
            renewer: Mutex::new(renewer),
            properties: None,
        });
        guard.check()?;
        Ok(guard)
    }
    pub fn check(&self) -> Result<()> {
        if let Some((path, expected)) = &self.properties
            && read_local_document(
                path,
                crate::minecraft_admission::MAX_SERVER_PROPERTIES_BYTES,
            )? != *expected
        {
            return Err(FileError::Conflict(None));
        }
        let snapshot = self
            .runtime
            .game_snapshot()
            .map_err(|_| FileError::Unavailable)?;
        if snapshot.generation.is_some()
            || snapshot.phase != vesta_game_runtime::process::ProcessPhase::Stopped
        {
            return Err(FileError::Conflict(None));
        }
        if let Some(health) = self
            .runtime
            .supervision()
            .map_err(|_| FileError::Unavailable)?
            && (health.orphan_running || health.launch_unresolved || health.observation_failed)
        {
            return Err(FileError::Unavailable);
        }
        if self
            .renewer
            .lock()
            .map_err(|_| FileError::Unavailable)?
            .as_ref()
            .is_some_and(|r| !r.intact())
        {
            return Err(FileError::Unavailable);
        }
        if let Some((client, token)) = &self.supervisor {
            vesta_supervisor::save::renew_hold(client, token, LEASE)
                .map_err(|_| FileError::Unavailable)?;
        }
        Ok(())
    }
}
impl Drop for StoppedGuard {
    fn drop(&mut self) {
        if let Ok(renewer) = self.renewer.get_mut()
            && let Some(renewer) = renewer.take()
        {
            renewer.finish();
        }
        if let Some((client, token)) = &self.supervisor {
            let _ = vesta_supervisor::save::release_hold(client, token);
        }
    }
}

impl FileBrowser {
    fn for_whitelist(&self) -> Self {
        let mut files = self.clone();
        files.root = files.root.clone().whitelist();
        files
    }
    pub fn offline_whitelist(&self, subject: &Subject) -> Result<Value> {
        let files = self.for_whitelist();
        files.check_global_authority(subject, Capability::PlayersView)?;
        let (bytes, hash) = document(&files, subject)?;
        let items = parse(&bytes)?;
        Ok(json!({"items":items,"sha256":hash}))
    }
    pub fn patch_offline_whitelist(
        &self,
        runtime: &RuntimeHandle,
        subject: &Subject,
        key: &str,
        patch: OfflineWhitelistPatch,
    ) -> Result<Value> {
        if !moderation::valid_player(&patch.player)
            || patch.expected_sha256.as_ref().is_some_and(|h| {
                h.len() != 64
                    || !h
                        .bytes()
                        .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            })
        {
            return Err(FileError::InvalidPath);
        }
        self.check_global_authority(subject, Capability::PlayersModerate)?;
        let files = self.for_whitelist().remote_preparation();
        let input = json!(["offline-whitelist", patch]);
        if let Some(outcome) = files.remote_replay_before_preparation(subject, key, &input)? {
            return Ok(outcome);
        }
        let authority = vec![
            (Capability::PlayersModerate.id().into(), String::new()),
            (Capability::FilesRead.id().into(), "whitelist.json".into()),
            (
                if patch.expected_sha256.is_some() {
                    Capability::FilesOverwrite
                } else {
                    Capability::FilesCreate
                }
                .id()
                .into(),
                "whitelist.json".into(),
            ),
        ];
        files
            .db
            .read_with(|s| files.remote_authorize(s.connection(), subject, &authority))
            .map_err(FileError::from)?;
        let snapshot = runtime
            .game_snapshot()
            .map_err(|_| FileError::Unavailable)?;
        if snapshot.generation.is_some()
            || snapshot.phase != vesta_game_runtime::process::ProcessPhase::Stopped
        {
            return Err(FileError::Conflict(None));
        }
        let properties_path = files.root.raw_path().join("server.properties");
        let properties = read_local_document(
            &properties_path,
            crate::minecraft_admission::MAX_SERVER_PROPERTIES_BYTES,
        )?;
        let online = crate::minecraft_admission::ServerProperties::parse(&properties)
            .map_err(|_| FileError::Corrupt)?
            .online_mode();
        // Replays avoid lookup; fresh external IO holds neither admission nor a
        // stopped-state exclusion. Admission rechecks both the receipt and mode.
        let identity = if patch.add {
            Some(resolve_bounded(
                &patch.player,
                online,
                files.remote_deadline,
            )?)
        } else {
            None
        };
        files.execute_remote(subject, key, input, || Ok(authority), |files| {
            let (bytes, hash) = document(files,subject)?;
            if hash != patch.expected_sha256 { return Err(FileError::Conflict(hash)); }
            let mut entries = parse(&bytes)?;
            let snapshot=runtime.game_snapshot().map_err(|_|FileError::Unavailable)?;
            if snapshot.generation.is_some() || snapshot.phase != vesta_game_runtime::process::ProcessPhase::Stopped {return Err(FileError::Conflict(None));}
            let mut guard = StoppedGuard::acquire(runtime, self.db.directory())?;
            Arc::get_mut(&mut guard).ok_or(FileError::Unavailable)?.properties=Some((properties_path.clone(),properties.clone()));
            // Mode changes during lookup require a fresh explicit edit.
            if read_local_document(&properties_path, crate::minecraft_admission::MAX_SERVER_PROPERTIES_BYTES)? != properties { return Err(FileError::Conflict(None)); }
            edit(&mut entries,&patch,identity)?;
            let bytes = serde_json::to_vec_pretty(&entries).map_err(|_| FileError::Corrupt)?;
            if bytes.len()>LIMIT { return Err(FileError::TooLarge); }
            let mut files = files.clone();
            files.stopped_guard = Some(guard.clone());
            let precondition = patch.expected_sha256.clone().map_or(WritePrecondition::Absent,WritePrecondition::Matches);
            let change = files.write(subject,&RelativePath::parse("whitelist.json")?,&precondition,&bytes)?;
            // finish_intent checks hold continuity after the write, in the same
            // transaction that records success. Do not fail after that commit.
            Ok(json!({"outcome":"succeeded","sha256":change.sha256,"size":change.size.to_string(),"trash_id":change.trash}))
        })
    }
}
fn document(files: &FileBrowser, subject: &Subject) -> Result<(Vec<u8>, Option<String>)> {
    match files.read(subject, &RelativePath::parse("whitelist.json")?, LIMIT) {
        Ok(read) => Ok((read.bytes().to_vec(), Some(read.sha256().into()))),
        Err(FileError::NotFound) => Ok((b"[]".to_vec(), None)),
        Err(e) => Err(e),
    }
}
fn parse(bytes: &[u8]) -> Result<Vec<Value>> {
    #[derive(Deserialize, Serialize)]
    struct Entry {
        name: String,
        uuid: String,
        #[serde(flatten)]
        extra: std::collections::BTreeMap<String, Value>,
    }
    let parsed: Vec<Entry> = serde_json::from_slice(bytes).map_err(|_| FileError::Corrupt)?;
    let entries: Vec<Value> = parsed
        .into_iter()
        .map(|e| serde_json::to_value(e).map_err(|_| FileError::Corrupt))
        .collect::<Result<_>>()?;
    if entries.len() > 10_000 {
        return Err(FileError::TooLarge);
    }
    let mut uuids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for e in &entries {
        let name = e["name"].as_str().ok_or(FileError::Corrupt)?;
        let uuid = uuid::Uuid::parse_str(e["uuid"].as_str().ok_or(FileError::Corrupt)?)
            .map_err(|_| FileError::Corrupt)?;
        if !moderation::entry_valid(name, &uuid.to_string())
            || !uuids.insert(uuid)
            || !names.insert(name.to_ascii_lowercase())
        {
            return Err(FileError::Corrupt);
        }
    }
    Ok(entries)
}
fn edit(
    entries: &mut Vec<Value>,
    patch: &OfflineWhitelistPatch,
    identity: Option<(String, String)>,
) -> Result<()> {
    if let Some((name, uuid)) = identity {
        let id = uuid::Uuid::parse_str(&uuid).map_err(|_| FileError::Corrupt)?;
        if entries.iter().any(|e| {
            e["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(&name))
                && uuid::Uuid::parse_str(e["uuid"].as_str().unwrap_or("")).ok() != Some(id)
        }) {
            return Err(FileError::Conflict(None));
        }
        if !entries
            .iter()
            .any(|e| uuid::Uuid::parse_str(e["uuid"].as_str().unwrap_or("")).ok() == Some(id))
        {
            if entries.len() >= 10_000 {
                return Err(FileError::TooLarge);
            }
            entries.push(json!({"name":name,"uuid":uuid}));
        }
    } else {
        entries.retain(|e| {
            !e["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(&patch.player))
        });
    }
    Ok(())
}
#[cfg(test)]
fn resolve(player: &str, online: bool) -> Result<(String, String)> {
    resolve_bounded(player, online, None)
}
pub(super) fn resolve_bounded(
    player: &str,
    online: bool,
    deadline: Option<std::time::Instant>,
) -> Result<(String, String)> {
    let timeout = deadline.map_or(Duration::from_secs(8), |deadline| {
        deadline
            .saturating_duration_since(std::time::Instant::now())
            .min(Duration::from_secs(8))
    });
    if timeout.is_zero() {
        return Err(FileError::Unavailable);
    }
    if !online {
        use md5::{Digest, Md5};
        let mut bytes: [u8; 16] = Md5::digest(format!("OfflinePlayer:{player}").as_bytes()).into();
        bytes[6] = (bytes[6] & 0x0f) | 0x30;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        return Ok((player.into(), uuid::Uuid::from_bytes(bytes).to_string()));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .max_response_header_size(16 * 1024)
        .build()
        .into();
    let response = agent
        .get(&format!(
            "https://api.mojang.com/users/profiles/minecraft/{player}"
        ))
        .call()
        .map_err(|_| FileError::Unavailable)?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| FileError::Unavailable)?;
    decode_profile(&bytes, player)
}
fn decode_profile(bytes: &[u8], player: &str) -> Result<(String, String)> {
    if bytes.len() > 4096 {
        return Err(FileError::TooLarge);
    }
    #[derive(Deserialize)]
    struct Profile {
        name: String,
        id: String,
    }
    let value: Profile = serde_json::from_slice(bytes).map_err(|_| FileError::Unavailable)?;
    if !value.name.eq_ignore_ascii_case(player) || !moderation::valid_player(&value.name) {
        return Err(FileError::Unavailable);
    }
    let uuid = uuid::Uuid::parse_str(&value.id).map_err(|_| FileError::Unavailable)?;
    if uuid.is_nil() {
        return Err(FileError::Unavailable);
    }
    Ok((value.name, uuid.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_resolution_refuses_ambiguous_mismatched_and_oversized_replies() {
        let valid = br#"{"name":"Alex","id":"123e4567e89b42d3a456426614174000"}"#;
        assert_eq!(
            decode_profile(valid, "alex").unwrap(),
            ("Alex".into(), "123e4567-e89b-42d3-a456-426614174000".into())
        );
        assert!(decode_profile(valid, "Bob").is_err());
        for invalid in [
            br#"{"name":"Alex","name":"Bob","id":"123e4567e89b42d3a456426614174000"}"#.as_slice(),
            br#"{"name":"Alex","id":"bad"}"#,
            br#"{"name":"Alex","id":"00000000000000000000000000000000"}"#,
        ] {
            assert!(decode_profile(invalid, "Alex").is_err());
        }
        assert!(decode_profile(&vec![b' '; 4097], "Alex").is_err());
    }
    #[test]
    fn stopped_guard_blocks_local_starts_and_releases_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let state =
            vesta_state::StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap();
        let db =
            vesta_state::StateDatabase::open(state.clone(), Arc::new(vesta_state::SystemClock))
                .unwrap();
        let launch = vesta_game_runtime::process::LaunchSpec::new(
            std::env::current_exe().unwrap(),
            vec![],
            game,
            vesta_game_runtime::process::GracefulStop::MinecraftCommand,
        )
        .unwrap();
        let mut runtime = crate::operations::OperationRuntime::open(
            crate::operations::Operations::new(db.clone()).unwrap(),
            launch,
        )
        .unwrap();
        let gate = runtime.start_block();
        let mut guard = StoppedGuard::acquire(&runtime.network_handle(), &state).unwrap();
        assert!(gate.blocked());
        guard.check().unwrap();
        let path = temp.path().join("game/server.properties");
        std::fs::write(&path, b"online-mode=false\n").unwrap();
        Arc::get_mut(&mut guard).unwrap().properties =
            Some((path.clone(), b"online-mode=false\n".to_vec()));
        std::fs::write(path, b"online-mode=true\n").unwrap();
        assert!(guard.check().is_err());
        drop(guard);
        assert!(!gate.blocked());
        runtime.shutdown(Duration::from_secs(2)).unwrap();
        drop(runtime);
        db.close().unwrap();
    }
    #[test]
    fn stopped_guard_holds_supervisor_and_refuses_lost_hold() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let state =
            vesta_state::StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap();
        let db =
            vesta_state::StateDatabase::open(state.clone(), Arc::new(vesta_state::SystemClock))
                .unwrap();
        let launch = vesta_game_runtime::process::LaunchSpec::new(
            std::env::current_exe().unwrap(),
            vec![],
            game,
            vesta_game_runtime::process::GracefulStop::MinecraftCommand,
        )
        .unwrap();
        let config = vesta_supervisor::SupervisorConfig {
            state: state.clone(),
            launch,
            launch_identity: "1".repeat(64),
            restart: None,
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(vesta_supervisor::run(config, async {
                    let _ = stopped.await;
                }))
        });
        // Closing the channel always stops this owned, never-started supervisor.
        struct Cleanup(
            Option<tokio::sync::oneshot::Sender<()>>,
            Option<std::thread::JoinHandle<vesta_supervisor::Result<()>>>,
        );
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.take();
                if let Some(t) = self.1.take() {
                    let _ = t.join();
                }
            }
        }
        let mut cleanup = Cleanup(Some(stop), Some(thread));
        let client = Arc::new(vesta_supervisor::SupervisorClient::new(&state).unwrap());
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while vesta_supervisor::probe(&client).ok().flatten().is_none() {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(20));
        }
        let remote =
            vesta_supervisor::RemoteProcess::attach(client.clone(), "1".repeat(64)).unwrap();
        let mut runtime = crate::operations::OperationRuntime::open_supervised(
            crate::operations::Operations::new(db.clone()).unwrap(),
            remote,
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let handle = runtime.network_handle();
        let guard = StoppedGuard::acquire(&handle, &state).unwrap();
        assert!(
            vesta_supervisor::save::health(&client)
                .unwrap()
                .maintenance_hold
        );
        assert!(matches!(
            client
                .call(
                    vesta_supervisor::wire::Op::Start {
                        launch_identity: "1".repeat(64)
                    },
                    Duration::from_secs(2)
                )
                .unwrap(),
            vesta_supervisor::wire::Reply::Refused { .. }
        ));
        guard.check().unwrap();
        drop(guard);
        assert!(
            !vesta_supervisor::save::health(&client)
                .unwrap()
                .maintenance_hold
        );
        let guard = StoppedGuard::acquire(&handle, &state).unwrap();
        let (client, token) = guard.supervisor.as_ref().unwrap();
        vesta_supervisor::save::release_hold(client, token).unwrap();
        assert!(guard.check().is_err());
        drop(guard);
        runtime.shutdown(Duration::from_secs(3)).unwrap();
        drop(runtime);
        cleanup.0.take();
        cleanup.1.take().unwrap().join().unwrap().unwrap();
        drop(handle);
        db.close().unwrap();
    }
    #[test]
    fn profile_preparation_refuses_an_expired_intent_deadline() {
        let expired = std::time::Instant::now() - Duration::from_secs(1);
        for online in [false, true] {
            assert!(matches!(
                resolve_bounded("Alex", online, Some(expired)),
                Err(FileError::Unavailable)
            ));
        }
    }
    #[test]
    fn offline_identity_matches_java_and_preserves_case() {
        assert_eq!(
            resolve("Notch", false).unwrap().1,
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
        assert_ne!(
            resolve("Alex", false).unwrap().1,
            resolve("alex", false).unwrap().1
        );
    }
    #[test]
    fn full_whitelist_refuses_growth_but_allows_replay_and_removal() {
        let mut entries = (0..10_000)
            .map(|n| {
                let (name, uuid) = resolve(&format!("player{n}"), false).unwrap();
                json!({"name":name,"uuid":uuid})
            })
            .collect::<Vec<_>>();
        let add = OfflineWhitelistPatch {
            player: "overflow".into(),
            add: true,
            expected_sha256: None,
        };
        assert!(matches!(
            edit(
                &mut entries,
                &add,
                Some(resolve("overflow", false).unwrap())
            ),
            Err(FileError::TooLarge)
        ));
        assert_eq!(entries.len(), 10_000);
        edit(&mut entries, &add, Some(resolve("player0", false).unwrap())).unwrap();
        let remove = OfflineWhitelistPatch {
            player: "PLAYER0".into(),
            add: false,
            expected_sha256: None,
        };
        edit(&mut entries, &remove, None).unwrap();
        edit(
            &mut entries,
            &add,
            Some(resolve("overflow", false).unwrap()),
        )
        .unwrap();
        assert!(parse(&serde_json::to_vec(&entries).unwrap()).is_ok());
    }
    #[test]
    fn ambiguous_and_malformed_entries_refuse_without_losing_extensions() {
        for bytes in [
            br#"[{"name":"Alex","name":"Bob","uuid":"123e4567-e89b-42d3-a456-426614174000"}]"#
                .as_slice(),
            br#"[{"name":"Alex","uuid":"bad"}]"#,
        ] {
            assert!(parse(bytes).is_err());
        }
        let mut entries = parse(
            br#"[{"name":"Alex","uuid":"123e4567-e89b-42d3-a456-426614174000","plugin":true}]"#,
        )
        .unwrap();
        let patch = OfflineWhitelistPatch {
            player: "Bob".into(),
            add: true,
            expected_sha256: None,
        };
        edit(&mut entries, &patch, Some(resolve("Bob", false).unwrap())).unwrap();
        assert_eq!(entries[0]["plugin"], true);
        let mut duplicate = entries.clone();
        duplicate.push(entries[0].clone());
        assert!(parse(&serde_json::to_vec(&duplicate).unwrap()).is_err());
        let remove = OfflineWhitelistPatch {
            player: "BOB".into(),
            add: false,
            expected_sha256: None,
        };
        edit(&mut entries, &remove, None).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
