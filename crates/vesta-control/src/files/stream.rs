//! Bounded observations of growing regular files; no persistent read grants.
use super::{FileBrowser, FileError, RelativePath, Result, authorize, io_error, native};
use crate::identity::{self, Capability, Subject};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    time::UNIX_EPOCH,
};
use vesta_state::artifacts::{ArtifactDirectory, ArtifactName};

pub const MAX_RANGE_BYTES: u64 = 4 * 1024 * 1024;
pub const FILE_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_TEXT_LINES: usize = 1000;

pub struct FileStream {
    file: File,
    size: u64,
    etag: String,
    identity: String,
    modified: std::time::SystemTime,
}
impl FileBrowser {
    /// All three resources (submitted, canonical, handle) pass live policy and
    /// remote document exclusions. The handle allows writers/rotation on Windows.
    pub fn open_stream(&self, subject: &Subject, path: &RelativePath) -> Result<FileStream> {
        self.db
            .read_with(|snapshot| {
                let policy = identity::policy_in(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                let root = self.root.clone().remote(false);
                authorize(&policy, Capability::FilesRead, &path.resource())?;
                let checked = root.resolve(Some(path))?;
                authorize(&policy, Capability::FilesRead, &checked.resource)?;
                let file = native::open_growing(&checked.path)?;
                let actual = native::actual_path(&file, &checked.path)?;
                authorize(&policy, Capability::FilesRead, &root.resource(&actual)?)?;
                let metadata = file.metadata().map_err(io_error)?;
                let identity = native::file_identity(&file)?;
                let modified = metadata
                    .modified()
                    .map_err(io_error)?
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| FileError::Unavailable)?
                    .as_nanos();
                let etag = format!(
                    "\"{:x}\"",
                    Sha256::digest(format!("{}:{modified}:{identity}", metadata.len()).as_bytes())
                );
                Ok(FileStream {
                    file,
                    size: metadata.len(),
                    etag,
                    identity,
                    modified: metadata.modified().map_err(io_error)?,
                })
            })
            .map_err(FileError::from)
    }
}
impl FileStream {
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn etag(&self) -> &str {
        &self.etag
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn unchanged(&self) -> Result<bool> {
        let metadata = self.file.metadata().map_err(io_error)?;
        Ok(metadata.len() == self.size && metadata.modified().map_err(io_error)? == self.modified)
    }
    /// A download serves the size seen when it started. Appended bytes are ignored; a
    /// shorter file means truncation, so the download must stop.
    pub fn still_covers(&self) -> Result<bool> {
        Ok(self.file.metadata().map_err(io_error)?.len() >= self.size)
    }
    pub fn read_chunk(&mut self, offset: u64, limit: usize) -> Result<Vec<u8>> {
        if limit == 0 || limit > FILE_CHUNK_BYTES || offset > self.size {
            return Err(FileError::InvalidLimit);
        }
        self.file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        let mut bytes = vec![0; (self.size - offset).min(limit as u64) as usize];
        self.file.read_exact(&mut bytes).map_err(io_error)?;
        Ok(bytes)
    }
    /// Encoding is a hint about a bounded prefix, not a claim about unseen bytes.
    pub fn encoding_hint(&mut self) -> Result<&'static str> {
        let bytes = self.read_chunk(0, FILE_CHUNK_BYTES)?;
        Ok(encoding_hint(&bytes))
    }
    pub fn lines(&mut self, from: u64, limit: usize, backwards: bool) -> Result<LinePage> {
        if limit == 0 || limit > MAX_TEXT_LINES || from > self.size {
            return Err(FileError::InvalidLimit);
        }
        let mut bytes = Vec::new();
        let (start, end);
        if backwards {
            let mut offset = from;
            while offset != 0 {
                let count = offset.min(FILE_CHUNK_BYTES as u64) as usize;
                if bytes.len() + count > MAX_RANGE_BYTES as usize {
                    return Err(FileError::TooLarge);
                }
                offset -= count as u64;
                let chunk = self.read_chunk(offset, count)?;
                bytes.splice(0..0, chunk);
                let skip = usize::from(bytes.last() == Some(&b'\n'));
                let boundaries: Vec<_> = bytes
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, b)| **b == b'\n')
                    .skip(skip)
                    .take(limit)
                    .map(|(i, _)| i + 1)
                    .collect();
                if boundaries.len() == limit {
                    let cut = *boundaries.last().expect("bounded lines");
                    bytes.drain(..cut);
                    offset += cut as u64;
                    break;
                }
            }
            start = offset;
            end = from;
        } else {
            // A byte cursor must be a line boundary, never silently skip bytes.
            if from != 0 && from != self.size && self.read_chunk(from - 1, 1)? != b"\n" {
                return Err(FileError::InvalidLimit);
            }
            let mut offset = from;
            while offset < self.size {
                let chunk = self.read_chunk(offset, FILE_CHUNK_BYTES)?;
                if bytes.len() + chunk.len() > MAX_RANGE_BYTES as usize {
                    return Err(FileError::TooLarge);
                }
                offset += chunk.len() as u64;
                bytes.extend_from_slice(&chunk);
                if let Some(cut) = bytes
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| **b == b'\n')
                    .nth(limit - 1)
                    .map(|(i, _)| i + 1)
                {
                    bytes.truncate(cut);
                    break;
                }
            }
            start = from;
            end = from + bytes.len() as u64;
        }
        if backwards && from != self.size && from != 0 && self.read_chunk(from - 1, 1)? != b"\n" {
            return Err(FileError::InvalidLimit);
        }
        Ok(LinePage {
            encoding: encoding_hint(&bytes),
            bytes,
            start,
            end,
            size: self.size,
            etag: self.etag.clone(),
        })
    }
}
pub fn encoding_hint(bytes: &[u8]) -> &'static str {
    if !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() {
        "utf-8"
    } else {
        "binary"
    }
}
pub struct LinePage {
    pub bytes: Vec<u8>,
    pub start: u64,
    pub end: u64,
    pub size: u64,
    pub etag: String,
    pub encoding: &'static str,
}

/// Decompressed views are capped in size, in expansion ratio and in time, so a crafted
/// archive cannot exhaust disk, memory or a worker.
pub const MAX_DECOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DECOMPRESSION_RATIO: u64 = 256;
const MIN_DECOMPRESSED_ALLOWANCE: u64 = 1024 * 1024;
const DECOMPRESS_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);
const SPOOL_MAX_ENTRIES: usize = 8;
const SPOOL_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const SPOOL_STALE_STAGE: std::time::Duration = std::time::Duration::from_secs(300);
/// Decompressed views are reused for this long after they are made, then rebuilt on demand.
pub const SPOOL_VIEW_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// One decompression at a time; a busy cache refuses rather than blocking a worker.
static DECOMPRESSING: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl FileBrowser {
    /// The decompressed text of a rotated `*.gz` log, as a seekable read-only view.
    /// Authority and path checks are those of `open_stream` on the archive itself.
    /// Gzip cannot seek, so the archive is decompressed once into a bounded owner-only
    /// cache keyed by the archive's own validator; later reads reuse it.
    pub fn open_decompressed(&self, subject: &Subject, path: &RelativePath) -> Result<FileStream> {
        if !path.as_str().to_ascii_lowercase().ends_with(".gz") {
            return Err(FileError::NotCompressed);
        }
        let mut source = self.open_stream(subject, path)?;
        if source.size < 2 || source.read_chunk(0, 2)? != [0x1f, 0x8b] {
            return Err(FileError::NotCompressed);
        }
        let key = hex::encode(
            &Sha256::digest(format!("{}|{}", source.etag, source.identity).as_bytes())[..16],
        );
        let spool = self.db.directory().spool()?;
        let name = ArtifactName::new(&format!("gz-{key}.log"))?;
        if let Some(file) = fresh(&spool, &name)? {
            return spooled(file, &source, &key);
        }
        let _busy = DECOMPRESSING
            .try_lock()
            .map_err(|_| FileError::Unavailable)?;
        // Another request may have finished the same archive while we waited.
        if let Some(file) = fresh(&spool, &name)? {
            return spooled(file, &source, &key);
        }
        let stage = ArtifactName::new(&format!("gz-{key}-{}.tmp", super::mutation::random_id()?))?;
        let mut out = spool.create_new(&stage)?;
        let written = decompress(&mut source, &mut out);
        drop(out);
        let finished = written.and_then(|()| {
            if !source.unchanged()? {
                return Err(FileError::Unavailable);
            }
            spool.sync()?;
            match spool.link_new(&stage, &name) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == "state_artifact_exists" => Ok(()),
                Err(e) => Err(e.into()),
            }
        });
        let _ = spool.remove(&stage);
        finished?;
        evict(&spool, &name);
        let file = spool.open(&name)?.ok_or(FileError::Unavailable)?;
        spooled(file, &source, &key)
    }
}

fn decompress(source: &mut FileStream, out: &mut File) -> Result<()> {
    use std::io::Write;
    source.file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let limit = MAX_DECOMPRESSED_BYTES.min(
        source
            .size
            .saturating_mul(MAX_DECOMPRESSION_RATIO)
            .max(MIN_DECOMPRESSED_ALLOWANCE),
    );
    let deadline = std::time::Instant::now() + DECOMPRESS_BUDGET;
    let reader =
        std::io::BufReader::with_capacity(FILE_CHUNK_BYTES, (&source.file).take(source.size));
    let mut decoder = flate2::read::MultiGzDecoder::new(reader);
    let mut buffer = vec![0; FILE_CHUNK_BYTES];
    let mut total = 0_u64;
    loop {
        // Truncation, bad headers and CRC/length mismatches all surface here.
        let count = decoder.read(&mut buffer).map_err(|_| FileError::Corrupt)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > limit {
            return Err(FileError::TooLarge);
        }
        if std::time::Instant::now() > deadline {
            return Err(FileError::Unavailable);
        }
        out.write_all(&buffer[..count]).map_err(io_error)?;
    }
    out.sync_all().map_err(io_error)
}

/// A cached view younger than the TTL. An expired one is removed so it is rebuilt; if
/// another reader still holds it open, the removal can fail and the view is reused.
fn fresh(spool: &ArtifactDirectory, name: &ArtifactName) -> Result<Option<File>> {
    let Some(file) = spool.open(name)? else {
        return Ok(None);
    };
    if expired(&file.metadata().map_err(io_error)?) {
        drop(file);
        if spool.remove(name).is_ok() {
            return Ok(None);
        }
        return spool.open(name).map_err(Into::into);
    }
    Ok(Some(file))
}
fn expired(metadata: &std::fs::Metadata) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|made| std::time::SystemTime::now().duration_since(made).ok())
        .is_some_and(|age| age > SPOOL_VIEW_TTL)
}

fn spooled(file: File, source: &FileStream, key: &str) -> Result<FileStream> {
    let metadata = file.metadata().map_err(io_error)?;
    Ok(FileStream {
        file,
        size: metadata.len(),
        // Distinct from the archive's own validator, and changes when the archive does.
        etag: format!("\"gz-{key}\""),
        identity: format!("{}:gz", source.identity),
        modified: metadata.modified().map_err(io_error)?,
    })
}

/// Best effort: keep the cache within its count and byte budgets, oldest first, and
/// clear stages abandoned by an interrupted decompression.
fn evict(spool: &ArtifactDirectory, keep: &ArtifactName) {
    let Ok(names) = spool.list(256) else { return };
    let now = std::time::SystemTime::now();
    let mut cached = Vec::new();
    for name in names {
        let Ok(Some(file)) = spool.open(&name) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        drop(file);
        let modified = metadata.modified().unwrap_or(now);
        if name.as_str().ends_with(".tmp") {
            if now.duration_since(modified).unwrap_or_default() > SPOOL_STALE_STAGE {
                let _ = spool.remove(&name);
            }
        } else if name.as_str().starts_with("gz-") && &name != keep {
            if expired(&metadata) {
                let _ = spool.remove(&name);
            } else {
                cached.push((modified, metadata.len(), name));
            }
        }
    }
    cached.sort_by_key(|(modified, _, _)| *modified);
    let mut count = cached.len() + 1;
    let mut bytes: u64 = cached.iter().map(|(_, len, _)| len).sum();
    for (_, len, name) in cached {
        if count <= SPOOL_MAX_ENTRIES && bytes <= SPOOL_MAX_BYTES {
            break;
        }
        if spool.remove(&name).is_ok() {
            count -= 1;
            bytes -= len;
        }
    }
}
