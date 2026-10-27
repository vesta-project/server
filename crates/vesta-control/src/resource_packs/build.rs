use super::{
    BuildLimits, BuildReport, Error, Input, PackFormat, Resolution, ResolvedPath, Result,
    archive::{self, Archive, Member},
    semantic,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use vesta_state::{
    StateDirectory,
    artifacts::{ArtifactDirectory, ArtifactName},
};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

static BUILDING: AtomicBool = AtomicBool::new(false);
struct Permit;
impl Permit {
    fn acquire() -> Result<Self> {
        BUILDING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        Ok(Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        BUILDING.store(false, Ordering::Release);
    }
}

struct Stage {
    directory: ArtifactDirectory,
    name: ArtifactName,
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.directory.remove(&self.name);
    }
}

/// A finished build, still private and unpublished. Dropping it retires its stage.
pub struct BuiltPack {
    stage: Stage,
    sha256: String,
    sha1: String,
    size: u64,
    report: BuildReport,
}
/// Immutable artifact only. This does not change the active Minecraft pack.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PublishedPack {
    artifact_name: String,
    sha256: String,
    sha1: String,
    size: u64,
    report: BuildReport,
}
impl BuiltPack {
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn sha1(&self) -> &str {
        &self.sha1
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn report(&self) -> &BuildReport {
        &self.report
    }
    /// Trusted caller must authorize publication separately. Never replaces a name.
    pub fn publish(self) -> Result<PublishedPack> {
        let published = ArtifactName::new(&format!("resource-pack-{}.zip", self.sha256))
            .map_err(|_| Error::InvalidInput)?;
        let directory = &self.stage.directory;
        // The stage may have been retained since the build finished. Verify it
        // before giving it a content-addressed name, not just after publication.
        let mut staged = directory
            .open(&self.stage.name)
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?;
        let actual = archive::hashes(
            &mut staged,
            BuildLimits::default().output_bytes,
            Instant::now() + BuildLimits::default().time,
        )?;
        drop(staged);
        if actual != (self.sha256.clone(), self.sha1.clone(), self.size) {
            return Err(Error::ChangedInput);
        }
        match directory.link_new(&self.stage.name, &published) {
            Ok(()) => {
                directory
                    .remove(&self.stage.name)
                    .map_err(|_| Error::Unavailable)?;
            }
            Err(error) if error.code() == "state_artifact_exists" => (),
            Err(_) => return Err(Error::Unavailable),
        }
        let result = PublishedPack {
            artifact_name: published.as_str().into(),
            sha256: self.sha256.clone(),
            sha1: self.sha1.clone(),
            size: self.size,
            report: self.report.clone(),
        };
        result.open_verified(directory)?;
        Ok(result)
    }
}
impl PublishedPack {
    /// Complete path provenance remains embedded in the ZIP. Do not duplicate it
    /// into every historical job and status response.
    pub(crate) fn compact_report(mut self) -> Self {
        self.report.resolved.clear();
        self
    }
    pub fn artifact_name(&self) -> &str {
        &self.artifact_name
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn sha1(&self) -> &str {
        &self.sha1
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn report(&self) -> &BuildReport {
        &self.report
    }
    /// Open and verify once before handing bytes to a serving adapter. No background polling.
    pub fn open_verified(&self, directory: &ArtifactDirectory) -> Result<File> {
        let expected = format!("resource-pack-{}.zip", self.sha256);
        if self.artifact_name != expected || self.size > BuildLimits::default().output_bytes {
            return Err(Error::InvalidInput);
        }
        let name = ArtifactName::new(&expected).map_err(|_| Error::InvalidInput)?;
        let mut file = directory
            .open(&name)
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?;
        let (sha256, sha1, size) = archive::hashes(
            &mut file,
            BuildLimits::default().output_bytes,
            Instant::now() + BuildLimits::default().time,
        )?;
        if sha256 != self.sha256 || sha1 != self.sha1 || size != self.size {
            return Err(Error::ChangedInput);
        }
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }
}

struct BudgetFile {
    file: File,
    max: u64,
    deadline: Instant,
}
impl Write for BudgetFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        archive::check(self.deadline).map_err(io::Error::other)?;
        if self
            .file
            .stream_position()?
            .checked_add(bytes.len() as u64)
            .is_none_or(|n| n > self.max)
        {
            return Err(io::Error::other(Error::TooLarge));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for BudgetFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        archive::check(self.deadline).map_err(io::Error::other)?;
        let destination = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.file.stream_position()? as i128 + n as i128,
            SeekFrom::End(n) => self.file.metadata()?.len() as i128 + n as i128,
        };
        if destination < 0 || destination > self.max as i128 {
            return Err(io::Error::other(Error::TooLarge));
        }
        self.file.seek(SeekFrom::Start(destination as u64))
    }
}
fn zip_error(error: zip::result::ZipError) -> Error {
    match error {
        zip::result::ZipError::Io(error) => error.into(),
        _ => Error::InvalidArchive,
    }
}

/// Build on an owned blocking worker. Lowest priority input first, highest last.
/// No game files are scanned or changed. Time limits are cooperative between IO.
pub fn build(
    state: &StateDirectory,
    inputs: Vec<Input>,
    target: PackFormat,
    description: &str,
    limits: BuildLimits,
) -> Result<BuiltPack> {
    let _permit = Permit::acquire()?;
    let limits = limits.validate()?;
    PackFormat::new(target.major, target.minor)?;
    if inputs.is_empty()
        || inputs.len() > limits.inputs
        || description.is_empty()
        || description.len() > 1024
    {
        return Err(Error::InvalidInput);
    }
    let deadline = Instant::now() + limits.time;
    let mut ids = BTreeSet::new();
    let mut compressed = 0u64;
    for input in &inputs {
        if !ids.insert(input.id.clone()) {
            return Err(Error::InvalidInput);
        }
        compressed = compressed
            .checked_add(input.file.metadata()?.len())
            .filter(|n| *n <= limits.compressed_bytes)
            .ok_or(Error::TooLarge)?;
    }
    let mut archives: Vec<Archive> = Vec::new();
    let mut remaining = limits.entries;
    for input in inputs {
        let archive = archive::open(
            input,
            BuildLimits {
                entries: remaining,
                ..limits
            },
            deadline,
        )?;
        remaining = remaining
            .checked_sub(archive.zip.len())
            .ok_or(Error::TooLarge)?;
        archives.push(archive);
    }
    let mut paths = BTreeMap::<String, Vec<Member>>::new();
    let mut metadata = Vec::new();
    let (mut expanded, mut entries) = (0u64, 0usize);
    for (pack, archive) in archives.iter_mut().enumerate() {
        let mut names = BTreeSet::new();
        let mut root = None;
        for index in 0..archive.zip.len() {
            archive::check(deadline)?;
            let file = archive
                .zip
                .by_index(index)
                .map_err(|_| Error::InvalidArchive)?;
            entries = entries
                .checked_add(1)
                .filter(|n| *n <= limits.entries)
                .ok_or(Error::TooLarge)?;
            let name = std::str::from_utf8(file.name_raw())
                .map_err(|_| Error::InvalidArchive)?
                .to_owned();
            let is_dir = file.is_dir();
            let spelling = name.strip_suffix('/').unwrap_or(&name);
            if !archive::portable(spelling)
                || !names.insert(spelling.to_owned())
                || file.is_symlink()
                || file.encrypted()
                || !matches!(
                    file.compression(),
                    CompressionMethod::Stored | CompressionMethod::Deflated
                )
                || file
                    .unix_mode()
                    .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000))
            {
                return Err(Error::InvalidArchive);
            }
            if file.size() > limits.member_bytes {
                return Err(Error::TooLarge);
            }
            expanded = expanded
                .checked_add(file.size())
                .filter(|n| *n <= limits.expanded_bytes)
                .ok_or(Error::TooLarge)?;
            if is_dir {
                if file.size() != 0 {
                    return Err(Error::InvalidArchive);
                }
                continue;
            }
            let member = Member {
                pack,
                index,
                size: file.size(),
            };
            drop(file);
            if name == "pack.mcmeta" {
                root = Some(member);
                continue;
            }
            let path = if name.starts_with("assets/") || name == "pack.png" {
                name
            } else {
                format!("vesta/attribution/{}/{}", archive.id, name)
            };
            paths.entry(path).or_default().push(member);
        }
        let root = root.ok_or(Error::InvalidMetadata)?;
        let value = archive::json(archive, root, limits.json_bytes.min(64 * 1024), deadline)?;
        semantic::metadata(&value, target)?;
        metadata.push(value);
    }
    if paths.len().saturating_add(2) > limits.entries {
        return Err(Error::TooLarge);
    }
    let mut report = BuildReport {
        inputs: archives.iter().map(|a| a.id.clone()).collect(),
        target,
        resolved: Vec::new(),
    };
    for (path, members) in &paths {
        if members.len() > 1 {
            report.resolved.push(ResolvedPath {
                path: path.clone(),
                layers: members
                    .iter()
                    .map(|m| archives[m.pack].id.clone())
                    .collect(),
                resolution: semantic::resolution(path),
            });
        }
    }
    let directory = state.artifacts().map_err(|_| Error::Unavailable)?;
    if directory
        .count(4095)
        .map_err(|_| Error::Unavailable)?
        .is_none()
    {
        return Err(Error::TooLarge);
    }
    let name = ArtifactName::new(&format!("resource-build-{}", uuid::Uuid::new_v4()))
        .map_err(|_| Error::Unavailable)?;
    let file = directory
        .create_new(&name)
        .map_err(|_| Error::Unavailable)?;
    let stage = Stage { directory, name };
    let mut zip = ZipWriter::new(BudgetFile {
        file,
        max: limits.output_bytes,
        deadline,
    });
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(1))
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644)
        .system(zip::System::Unix)
        .large_file(false);
    write_json(
        &mut zip,
        "pack.mcmeta",
        &semantic::output_metadata(&metadata, target, description)?,
        options,
        limits.json_bytes,
    )?;
    let provenance = json!({"schema":1,"target":target,"inputs":archives.iter().map(|a|json!({"id":a.id,"sha256":a.expected})).collect::<Vec<_>>(),"resolved":report.resolved});
    write_json(
        &mut zip,
        "vesta/stack.json",
        &provenance,
        options,
        limits.json_bytes,
    )?;
    for (path, members) in &paths {
        let kind = semantic::resolution(path);
        if kind == Resolution::HigherPriority {
            zip.start_file(path, options).map_err(zip_error)?;
            let member = *members.last().ok_or(Error::InvalidInput)?;
            archive::copy_member(
                &mut archives[member.pack],
                member,
                &mut zip,
                limits.member_bytes,
                deadline,
            )?;
        } else {
            let mut combined = None;
            for member in members {
                let next = archive::json(
                    &mut archives[member.pack],
                    *member,
                    limits.json_bytes,
                    deadline,
                )?;
                let value = semantic::merge(kind, combined, next)?;
                if serde_json::to_vec(&value)
                    .map_err(|_| Error::InvalidMetadata)?
                    .len()
                    > limits.json_bytes
                {
                    return Err(Error::TooLarge);
                }
                combined = Some(value);
            }
            write_json(
                &mut zip,
                path,
                &combined.ok_or(Error::InvalidMetadata)?,
                options,
                limits.json_bytes,
            )?;
        }
    }
    let mut writer = zip.finish().map_err(zip_error)?;
    writer.file.sync_all()?;
    let (sha256, sha1, size) = archive::hashes(&mut writer.file, limits.output_bytes, deadline)?;
    drop(writer);
    // Recheck the pinned files after copying. Metadata timestamps alone are not
    // proof against a same-length in-place modification during the build.
    for archive in archives {
        let mut input = archive.zip.into_inner();
        if archive::hashes(&mut input, limits.compressed_bytes, deadline)?.0 != archive.expected {
            return Err(Error::ChangedInput);
        }
    }
    Ok(BuiltPack {
        stage,
        sha256,
        sha1,
        size,
        report,
    })
}

fn write_json(
    zip: &mut ZipWriter<BudgetFile>,
    path: &str,
    value: &Value,
    options: SimpleFileOptions,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidMetadata)?;
    if bytes.len() > limit {
        return Err(Error::TooLarge);
    }
    zip.start_file(path, options).map_err(zip_error)?;
    zip.write_all(&bytes)?;
    Ok(())
}
