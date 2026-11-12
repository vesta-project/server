//! Bounded local console observation. No list commands, network lookups or polling jobs.
use super::{Automation, Severity};
use crate::{files, operations::RuntimeHandle};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use uuid::Uuid;
use vesta_game_runtime::process::GenerationId;

struct Baseline {
    directories: VecDeque<PathBuf>,
    current: Option<fs::ReadDir>,
    historical: BTreeSet<Uuid>,
    pending: Vec<Uuid>,
    scanned: usize,
    ready: bool,
    safe: bool,
}
impl Baseline {
    fn new(game: &Path) -> Self {
        let mut result = Self {
            directories: VecDeque::new(),
            current: None,
            historical: BTreeSet::new(),
            pending: Vec::new(),
            scanned: 0,
            ready: false,
            safe: true,
        };
        let world = (|| {
            files::plain_entry(&fs::symlink_metadata(game).ok()?).ok()?;
            let properties = match files::read_local_document(
                &game.join("server.properties"),
                crate::minecraft_admission::MAX_SERVER_PROPERTIES_BYTES,
            ) {
                Ok(bytes) => bytes,
                Err(files::FileError::NotFound) => Vec::new(),
                Err(_) => return None,
            };
            let (name, _) = crate::config::properties::world_values(&properties).ok()?;
            files::RelativePath::parse(&name).ok()?;
            if name.contains('/') {
                return None;
            }
            let world = game.join(name);
            match fs::symlink_metadata(&world) {
                Ok(meta) => {
                    files::plain_entry(&meta).ok()?;
                    if !meta.is_dir() {
                        return None;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(world),
                Err(_) => return None,
            }
            Some(world)
        })();
        if let Some(world) = world {
            result
                .directories
                .extend([world.join("playerdata"), world.join("stats")]);
        } else {
            result.ready = true;
            result.safe = false;
        }
        result
    }
    fn step(&mut self) {
        // At most 128 directory entries per host pulse, including unrelated entries.
        for _ in 0..128 {
            if self.ready {
                return;
            }
            if self.current.is_none() {
                let Some(directory) = self.directories.pop_front() else {
                    self.ready = true;
                    return;
                };
                match fs::symlink_metadata(&directory) {
                    Ok(meta) if meta.is_dir() && files::plain_entry(&meta).is_ok() => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    _ => {
                        self.safe = false;
                        self.ready = true;
                        return;
                    }
                }
                match fs::read_dir(directory) {
                    Ok(entries) => self.current = Some(entries),
                    Err(_) => {
                        self.safe = false;
                        self.ready = true;
                        return;
                    }
                }
            }
            let Some(entry) = self.current.as_mut().and_then(Iterator::next) else {
                self.current = None;
                continue;
            };
            self.scanned += 1;
            if self.scanned > 200_000 {
                self.safe = false;
                self.ready = true;
                return;
            }
            let Ok(entry) = entry else {
                self.safe = false;
                self.ready = true;
                return;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some((stem, extension)) = name.rsplit_once('.') else {
                continue;
            };
            if !extension.eq_ignore_ascii_case("dat") && !extension.eq_ignore_ascii_case("json") {
                continue;
            }
            let Ok(uuid) = Uuid::parse_str(stem) else {
                continue;
            };
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                self.safe = false;
                self.ready = true;
                return;
            };
            if !meta.is_file() || files::plain_entry(&meta).is_err() {
                self.safe = false;
                self.ready = true;
                return;
            }
            if self.historical.insert(uuid) {
                self.pending.push(uuid);
            }
            if self.historical.len() > 100_000 {
                self.safe = false;
                self.ready = true;
                return;
            }
        }
    }
}

pub(crate) struct JoinTracker {
    game: PathBuf,
    baseline: Baseline,
    generation: Option<GenerationId>,
    cursor: u64,
    profiles: BTreeMap<String, (Uuid, SystemTime)>,
    online: BTreeSet<String>,
    online_known: bool,
    trusted: bool,
    warned: bool,
}
impl JoinTracker {
    pub(super) fn discontinuity(&mut self) {
        self.trusted = false;
        self.profiles.clear();
        self.online.clear();
        self.online_known = false;
    }
    #[cfg(test)]
    pub(super) fn for_test(game: PathBuf, automation: &Automation) -> Self {
        let mut baseline = Baseline::new(&game);
        while !baseline.ready {
            baseline.step();
            automation.seed_player_history(&baseline.pending).unwrap();
            baseline.pending.clear();
        }
        Self {
            game,
            baseline,
            generation: None,
            cursor: 0,
            profiles: BTreeMap::new(),
            online: BTreeSet::new(),
            online_known: true,
            trusted: true,
            warned: false,
        }
    }
    #[cfg(test)]
    pub(super) fn observe_line(
        &mut self,
        line: &str,
        automation: &Automation,
    ) -> crate::Result<()> {
        self.observe_line_at(line, SystemTime::now(), automation)
    }
    pub(super) fn observe_line_at(
        &mut self,
        line: &str,
        at: SystemTime,
        automation: &Automation,
    ) -> crate::Result<()> {
        if let Some((name, uuid)) = profile_line(line) {
            if self.profiles.len() >= 4096 {
                self.profiles.clear();
            }
            self.profiles.insert(name.to_ascii_lowercase(), (uuid, at));
        } else if let Some((name, kind)) = crate::metrics::activity_line(line) {
            let key = name.to_ascii_lowercase();
            if kind == "join" {
                if let Some((uuid, identified)) = self.profiles.get(&key).copied()
                    && at
                        .duration_since(identified)
                        .is_ok_and(|elapsed| elapsed <= Duration::from_secs(120))
                {
                    let historical = !self.baseline.safe
                        || !self.trusted
                        || self.baseline.historical.contains(&uuid);
                    automation.observe_player_join(uuid, name, historical)?;
                } else {
                    // An unidentifiable visit must not become a false "first visit"
                    // later in the same run when UUID evidence becomes available.
                    self.baseline.safe = false;
                }
                let empty = self.online.is_empty();
                if self.online.len() >= 10_000 {
                    self.online_known = false;
                } else if self.online.insert(key.clone())
                    && empty
                    && self.online_known
                    && self.trusted
                    && let Err(error) =
                        automation.publish(super::AutomationEvent::FirstPlayerJoined)
                {
                    self.online.remove(&key);
                    return Err(error);
                }
            } else if self.online.remove(&key)
                && self.online.is_empty()
                && self.online_known
                && self.trusted
                && let Err(error) = automation.publish(super::AutomationEvent::PlayerCountZero)
            {
                self.online.insert(key);
                return Err(error);
            }
        }
        Ok(())
    }
    pub(crate) fn new(game: PathBuf, runtime: &RuntimeHandle) -> Self {
        let cursor = runtime
            .console_snapshot(0, 1)
            .map(|p| p.latest_cursor)
            .unwrap_or(0);
        let generation = runtime
            .game_snapshot()
            .ok()
            .and_then(|snapshot| snapshot.generation);
        Self {
            baseline: Baseline::new(&game),
            game,
            generation,
            cursor,
            profiles: BTreeMap::new(),
            online: BTreeSet::new(),
            online_known: generation.is_none(),
            trusted: true,
            warned: false,
        }
    }
    pub(crate) fn tick(&mut self, runtime: &RuntimeHandle, automation: &Automation) {
        let Ok(snapshot) = runtime.game_snapshot() else {
            return;
        };
        if snapshot.generation != self.generation {
            self.generation = snapshot.generation;
            self.profiles.clear();
            self.trusted = true;
            self.warned = false;
            self.online.clear();
            self.online_known = true;
            self.baseline = Baseline::new(&self.game);
        }
        if self.baseline.pending.is_empty() {
            self.baseline.step();
        }
        if automation
            .seed_player_history(&self.baseline.pending)
            .is_err()
        {
            if !self.warned {
                self.warned = true;
                let _ = automation.notify(Severity::Warning, "First-visit tracking delayed", "Player history is unavailable or full. Join tracking will retry automatically.", "players");
            }
            return;
        }
        self.baseline.pending.clear();
        if !self.baseline.ready {
            return;
        }
        if self.generation.is_none() {
            if let Ok(page) = runtime.console_snapshot(self.cursor, 1) {
                self.cursor = page.latest_cursor;
            }
            return;
        }
        // A cached, generation-scoped empty roster can re-arm occupancy tracking
        // after attaching to an already running game. This never sends a command.
        if !self.online_known
            && self.trusted
            && self.online.is_empty()
            && runtime
                .last_player_count(self.generation)
                .is_some_and(|(count, _)| count == 0)
        {
            self.online_known = true;
        }
        for _ in 0..4 {
            let Ok(page) = runtime.console_snapshot(self.cursor, 128) else {
                return;
            };
            if page.gap {
                self.discontinuity();
            }
            for line in &page.lines {
                if line.truncated || line.cursor != self.cursor.saturating_add(1) {
                    self.discontinuity();
                }
                if !line.truncated
                    && self
                        .observe_line_at(&line.text, line.observed_at, automation)
                        .is_err()
                {
                    if !self.warned {
                        self.warned = true;
                        let _ = automation.notify(Severity::Warning, "First-visit tracking delayed", "Player history or workflow capacity is unavailable. Join tracking will retry automatically.", "players");
                    }
                    return;
                }
                self.cursor = line.cursor;
            }
            if !self.warned && (!self.trusted || !self.baseline.safe) {
                self.warned = true;
                let _ = automation.notify(Severity::Warning, "First-visit tracking paused", "Player history or console continuity could not be verified. New-player automations are paused for this Minecraft run.", "players");
            }
            if page.next_cursor >= page.latest_cursor {
                break;
            }
        }
    }
}

fn profile_line(line: &str) -> Option<(&str, Uuid)> {
    let message = vesta_game_runtime::readiness::server_info_message(line).or_else(|| {
        // Vanilla emits UUID evidence on its authenticator thread rather than server thread.
        let (_, tail) = line
            .strip_prefix('[')?
            .split_once("] [User Authenticator #")?;
        let (number, message) = tail.split_once("/INFO]: ")?;
        (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(message)
    })?;
    let (name, uuid) = message
        .strip_prefix("UUID of player ")?
        .split_once(" is ")?;
    if !crate::moderation::valid_player(name) {
        return None;
    }
    let uuid = Uuid::parse_str(uuid).ok()?;
    (!uuid.is_nil()).then_some((name, uuid))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uuid_evidence_is_anchored_and_never_comes_from_chat() {
        let id = Uuid::new_v4();
        for line in [
            format!("[12:34:56 INFO]: UUID of player Alex is {id}"),
            format!("[12:34:56] [User Authenticator #1/INFO]: UUID of player Alex is {id}"),
        ] {
            assert_eq!(profile_line(&line), Some(("Alex", id)));
        }
        for line in [
            format!("[12:34:56 INFO]: <Bob> UUID of player Alex is {id}"),
            format!("[12:34:56 WARN]: UUID of player Alex is {id}"),
            format!("[12:34:56] [User Authenticator #x/INFO]: UUID of player Alex is {id}"),
            format!("[12:34:56 INFO]: UUID of player Alex is {id} trailing"),
        ] {
            assert!(profile_line(&line).is_none());
        }
    }
    #[test]
    fn baseline_scans_incrementally_and_refuses_unbounded_directories() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("world/playerdata");
        fs::create_dir_all(&directory).unwrap();
        let id = Uuid::new_v4();
        fs::write(directory.join(format!("{id}.dat")), []).unwrap();
        let windows_id = Uuid::new_v4();
        fs::write(directory.join(format!("{windows_id}.DAT")), []).unwrap();
        for n in 0..256 {
            fs::write(directory.join(format!("unrelated-{n}")), []).unwrap();
        }
        let mut baseline = Baseline::new(root.path());
        baseline.step();
        assert!(!baseline.ready);
        assert!(baseline.scanned <= 128);
        while !baseline.ready {
            baseline.step();
        }
        assert!(baseline.safe);
        assert!(baseline.historical.contains(&id));
        assert!(baseline.historical.contains(&windows_id));
        let mut bounded = Baseline::new(root.path());
        bounded.scanned = 200_000;
        bounded.step();
        assert!(bounded.ready && !bounded.safe);
    }
}
