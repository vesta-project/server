use super::*;
use crate::{
    config::properties,
    identity::BuiltinRole,
    operations::RuntimeHandle,
    world::{NewWorld, WorldStatus},
};
use serde_json::{Value, json};

impl FileBrowser {
    /// Observation only; an unset creation seed never masquerades as the saved seed.
    pub fn world(&self, subject: &Subject) -> Result<WorldStatus> {
        self.world_observation(subject).map(|(status, _, _)| status)
    }
    fn world_observation(
        &self,
        subject: &Subject,
    ) -> Result<(WorldStatus, Vec<u8>, Option<String>)> {
        self.check_global_authority(subject, Capability::ConfigView)?;
        let files = self.for_properties(None);
        let (bytes, hash) = match files.read_with_capability(
            subject,
            &RelativePath::parse("server.properties")?,
            crate::minecraft_admission::MAX_SERVER_PROPERTIES_BYTES,
            Some(Capability::ConfigView),
        ) {
            Ok(read) => (read.bytes().to_vec(), Some(read.sha256().to_owned())),
            Err(FileError::NotFound) => (Vec::new(), None),
            Err(error) => return Err(error),
        };
        let (name, configured_seed) = properties::world_values(&bytes)?;
        let directory = RelativePath::parse(&name)?;
        // One ordinary root child only; custom universe/absolute world paths need adoption.
        if name.contains('/') {
            return Err(FileError::InvalidPath);
        }
        let metadata_path = RelativePath::parse(&format!("{name}/level.dat"))?;
        let metadata =
            match self
                .for_remote()
                .read(subject, &metadata_path, crate::world::MAX_LEVEL_BYTES)
            {
                Ok(read) => Some(read),
                Err(FileError::NotFound) => None,
                Err(e) => return Err(e),
            };
        let exists = match self.root.resolve(Some(&directory)) {
            Ok(checked) if checked.metadata.is_dir() => true,
            Ok(_) => return Err(FileError::NotDirectory),
            Err(FileError::NotFound) => false,
            Err(e) => return Err(e),
        };
        let mut revision = Sha256::new();
        revision.update([u8::from(hash.is_some())]);
        revision.update(&bytes);
        revision.update([u8::from(exists)]);
        let level_details = metadata
            .as_ref()
            .map(|m| crate::world::details(m.bytes()))
            .transpose()?
            .unwrap_or_default();
        let details = self.world_details(subject, &name, level_details)?;
        let seed = if let Some(metadata) = metadata {
            revision.update(metadata.bytes());
            match crate::world::seed(metadata.bytes())? {
                Some(seed) => Some(seed),
                None => {
                    let path = RelativePath::parse(&format!(
                        "{name}/data/minecraft/world_gen_settings.dat"
                    ))?;
                    let worldgen = self
                        .for_remote()
                        .read(subject, &path, crate::world::MAX_LEVEL_BYTES)
                        .map_err(|error| match error {
                            FileError::NotFound => FileError::Corrupt,
                            other => other,
                        })?;
                    // A changed split seed must invalidate the same world confirmation
                    // revision as a changed embedded seed. Retain ordinary path authority.
                    revision.update(b"\0world-gen-settings\0");
                    revision.update(worldgen.bytes());
                    Some(crate::world::world_generation_seed(worldgen.bytes())?)
                }
            }
        } else {
            None
        };
        Ok((
            WorldStatus {
                name,
                seed,
                configured_seed,
                exists,
                revision: format!("{:x}", revision.finalize()),
                details,
            },
            bytes,
            hash,
        ))
    }

    /// Select a fresh world, preserving every existing world/dimension in place.
    /// The single configuration replacement uses ordinary recoverable file admission.
    pub fn new_world(
        &self,
        runtime: &RuntimeHandle,
        subject: &Subject,
        key: &str,
        patch: NewWorld,
    ) -> Result<Value> {
        if patch.seed.len() > 256
            || patch.seed.chars().any(char::is_control)
            || patch.expected_revision.len() != 64
            || !patch
                .expected_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err(FileError::InvalidPath);
        }
        self.db
            .read_with(|snapshot| {
                let policy = identity::policy_in(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                if !policy.holds_builtin(BuiltinRole::Owner) {
                    return Err(FileError::from(crate::ControlError::forbidden(
                        identity::DenyReason::OwnerReserved,
                    )));
                }
                Ok(())
            })
            .map_err(FileError::from)?;
        let mut files = self.for_properties(None);
        files.world_write = true;
        let input = json!(["new-world", patch]);
        files.execute_remote(
            subject,
            key,
            input,
            || {
                Ok(vec![
                    (Capability::ConfigEdit.id().into(), String::new()),
                    (Capability::ConfigView.id().into(), String::new()),
                    (
                        Capability::FilesCreate.id().into(),
                        "server.properties".into(),
                    ),
                    (
                        Capability::FilesOverwrite.id().into(),
                        "server.properties".into(),
                    ),
                ])
            },
            |files| {
                let hold = StoppedGuard::acquire(runtime, files.db.directory())?;
                let (current, original, original_hash) = files.world_observation(subject)?;
                if current.name != patch.confirm_world
                    || current.revision != patch.expected_revision
                {
                    return Err(FileError::Conflict(Some(current.revision)));
                }
                let name = format!("world-{}", uuid::Uuid::new_v4().simple());
                let candidate = RelativePath::parse(&name)?;
                match files.root.resolve(Some(&candidate)) {
                    Err(FileError::NotFound) => {}
                    Ok(_) => return Err(FileError::Conflict(None)),
                    Err(e) => return Err(e),
                }
                let path = RelativePath::parse("server.properties")?;
                let edited = properties::select_new_world(&original, &name, &patch.seed)?;
                let mut revision = Sha256::new();
                revision.update([1]);
                revision.update(&edited);
                revision.update([0]);
                let details = self.world_details(subject, &name, Default::default())?;
                let status = WorldStatus {
                    name,
                    seed: None,
                    configured_seed: patch.seed.clone(),
                    exists: false,
                    revision: format!("{:x}", revision.finalize()),
                    details,
                };
                let mut preserved = Vec::new();
                for directory in [
                    current.name.clone(),
                    format!("{}_nether", current.name),
                    format!("{}_the_end", current.name),
                ] {
                    match files.root.resolve(Some(&RelativePath::parse(&directory)?)) {
                        Ok(checked) if checked.metadata.is_dir() => preserved.push(directory),
                        Ok(_) => return Err(FileError::NotDirectory),
                        Err(FileError::NotFound) => {}
                        Err(e) => return Err(e),
                    }
                }
                hold.check()?;
                let outcome =
                    json!({"world":status,"preserved_worlds":preserved,"outcome":"succeeded"});
                let mut guarded = files.clone();
                guarded.stopped_guard = Some(hold);
                guarded.remote_outcome = Some(outcome.clone());
                let precondition =
                    original_hash.map_or(WritePrecondition::Absent, WritePrecondition::Matches);
                guarded.write(subject, &path, &precondition, &edited)?;
                Ok(outcome)
            },
        )
    }
}
