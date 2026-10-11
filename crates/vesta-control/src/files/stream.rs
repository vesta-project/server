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
    resources: Vec<identity::Resource>,
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
                let actual_resource = root.resource(&actual)?;
                authorize(&policy, Capability::FilesRead, &actual_resource)?;
                let resources = vec![path.resource(), checked.resource, actual_resource];
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
                    resources,
                })
            })
            .map_err(FileError::from)
    }
    /// Recheck the scopes admitted for this handle; never reopen a rotated pathname.
    pub fn reauthorize_stream(&self, subject: &Subject, stream: &FileStream) -> Result<()> {
        self.db
            .read_with(|snapshot| {
                let policy = identity::policy_in(
                    snapshot.connection(),
                    self.db.server_id(),
                    self.db.clock().millis(),
                    Some(subject),
                )?;
                for resource in &stream.resources {
                    authorize(&policy, Capability::FilesRead, resource)?;
                }
                Ok(())
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
        Ok(metadata.len() == self.size
            && metadata.modified().map_err(io_error)? == self.modified
            && native::file_identity(&self.file)? == self.identity)
    }
    /// Appends are provisional: the transport verifies the served bytes against an
    /// opening digest before completing, so overwrite/truncate-regrow cannot finish
    /// under the opening strong ETag even when length has increased.
    pub fn still_covers(&self) -> Result<bool> {
        let metadata = self.file.metadata().map_err(io_error)?;
        Ok(metadata.len() >= self.size
            && (metadata.len() > self.size
                || metadata.modified().map_err(io_error)? == self.modified)
            && native::file_identity(&self.file)? == self.identity)
    }
    /// Fingerprint the admitted interval with fixed-size reads. Reject observations
    /// made while the opening version changes; this never allocates a file-sized buffer.
    pub fn snapshot_digest(&mut self, offset: u64, count: u64) -> Result<[u8; 32]> {
        if offset > self.size || count > self.size - offset {
            return Err(FileError::InvalidLimit);
        }
        if !self.unchanged()? {
            return Err(FileError::Conflict(None));
        }
        let mut digest = Sha256::new();
        let mut cursor = offset;
        while cursor < offset + count {
            let bytes = self.read_chunk(
                cursor,
                (offset + count - cursor).min(FILE_CHUNK_BYTES as u64) as usize,
            )?;
            cursor += bytes.len() as u64;
            digest.update(bytes);
        }
        if !self.unchanged()? {
            return Err(FileError::Conflict(None));
        }
        Ok(digest.finalize().into())
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
// Also bound input work: concatenated empty members need not produce any output.
const MAX_COMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
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
        let limit = decompressed_limit(source.size);
        let mut space =
            |incoming| super::mutation::require_space(self.db.directory().path(), incoming);
        // Reserve the worst-case view before creating a stage, not after publishing it.
        let mut out = create_stage(&spool, &stage, limit, &mut space)?;
        let written = decompress(&mut source, &mut out, limit, &mut space);
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
        let file = spool.open(&name)?.ok_or(FileError::Unavailable)?;
        spooled(file, &source, &key)
    }
}

fn decompressed_limit(compressed: u64) -> u64 {
    MAX_DECOMPRESSED_BYTES.min(
        compressed
            .saturating_mul(MAX_DECOMPRESSION_RATIO)
            .max(MIN_DECOMPRESSED_ALLOWANCE),
    )
}

/// Checks beneath the decoder also run when it consumes empty gzip members without
/// returning output. An exhausted input budget may probe EOF, but cannot feed more work.
struct CompressedInput<R> {
    inner: R,
    remaining: u64,
    deadline: std::time::Instant,
    failure: Option<FileError>,
}
impl<R: Read> Read for CompressedInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let result = (|| {
            if std::time::Instant::now() >= self.deadline {
                return Err(FileError::Unavailable);
            }
            let count = if self.remaining == 0 {
                let mut probe = [0];
                if self.inner.read(&mut probe).map_err(io_error)? != 0 {
                    return Err(FileError::TooLarge);
                }
                0
            } else {
                let limit = buffer
                    .len()
                    .min(FILE_CHUNK_BYTES)
                    .min(self.remaining as usize);
                self.inner.read(&mut buffer[..limit]).map_err(io_error)?
            };
            // In particular, a read returning EOF must not bypass the deadline.
            if std::time::Instant::now() >= self.deadline {
                return Err(FileError::Unavailable);
            }
            self.remaining -= count as u64;
            Ok(count)
        })();
        result.map_err(|error| {
            self.failure = Some(error);
            std::io::Error::other("bounded compressed input refused")
        })
    }
}

fn decompress(
    source: &mut FileStream,
    out: &mut File,
    limit: u64,
    space: &mut impl FnMut(u64) -> Result<()>,
) -> Result<()> {
    source.file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    decode_into(
        (&source.file).take(source.size),
        out,
        limit,
        MAX_COMPRESSED_BYTES,
        std::time::Instant::now() + DECOMPRESS_BUDGET,
        space,
    )?;
    out.sync_all().map_err(io_error)
}

fn decode_into(
    input: impl Read,
    out: &mut impl std::io::Write,
    limit: u64,
    input_limit: u64,
    deadline: std::time::Instant,
    space: &mut impl FnMut(u64) -> Result<()>,
) -> Result<()> {
    let reader = std::io::BufReader::with_capacity(
        FILE_CHUNK_BYTES,
        CompressedInput {
            inner: input,
            remaining: input_limit,
            deadline,
            failure: None,
        },
    );
    let mut decoder = flate2::bufread::MultiGzDecoder::new(reader);
    let mut buffer = vec![0; FILE_CHUNK_BYTES];
    let mut total = 0_u64;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(FileError::Unavailable);
        }
        // Truncation, bad headers and CRC/length mismatches all surface here.
        let count = decoder.read(&mut buffer).map_err(|_| {
            decoder
                .get_ref()
                .get_ref()
                .failure
                .clone()
                .unwrap_or(FileError::Corrupt)
        })?;
        if std::time::Instant::now() >= deadline {
            return Err(FileError::Unavailable);
        }
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > limit {
            return Err(FileError::TooLarge);
        }
        // Recheck the real volume before each chunk; concurrent uploads can consume
        // space after the initial worst-case admission check.
        space(count as u64)?;
        out.write_all(&buffer[..count]).map_err(io_error)?;
    }
    Ok(())
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
    let identity = native::file_identity(&file)?;
    Ok(FileStream {
        file,
        size: metadata.len(),
        // Distinct from the archive's own validator, and changes when the archive does.
        etag: format!("\"gz-{key}\""),
        identity,
        modified: metadata.modified().map_err(io_error)?,
        resources: source.resources.clone(),
    })
}

struct CacheEntry {
    name: ArtifactName,
    modified: std::time::SystemTime,
    len: u64,
    stale: bool,
    removable: bool,
}

fn create_stage(
    spool: &ArtifactDirectory,
    stage: &ArtifactName,
    incoming: u64,
    space: &mut impl FnMut(u64) -> Result<()>,
) -> Result<File> {
    // list() is bounded rather than exhaustive; refuse if its scan could miss entries.
    if spool.count(256)?.is_none() {
        return Err(FileError::Unavailable);
    }
    let now = std::time::SystemTime::now();
    let mut cached = Vec::new();
    for name in spool.list(256)? {
        if !name.as_str().starts_with("gz-") {
            continue;
        }
        let Some(file) = spool.open(&name)? else {
            continue;
        };
        let metadata = file.metadata().map_err(io_error)?;
        drop(file);
        let modified = metadata.modified().map_err(io_error)?;
        let stage = name.as_str().ends_with(".tmp");
        let stale = if stage {
            now.duration_since(modified).unwrap_or_default() > SPOOL_STALE_STAGE
        } else {
            expired(&metadata)
        };
        cached.push(CacheEntry {
            name,
            modified,
            len: metadata.len(),
            stale,
            removable: !stage || stale,
        });
    }
    reserve_cache(cached, incoming, |name| spool.remove(name).is_ok(), space)?;
    spool.create_new(stage).map_err(Into::into)
}

/// Admission includes the incoming view's entire allowance. Failed removals remain
/// counted, including expired files held open without delete sharing on Windows.
fn reserve_cache(
    mut cached: Vec<CacheEntry>,
    incoming: u64,
    mut remove: impl FnMut(&ArtifactName) -> bool,
    space: &mut impl FnMut(u64) -> Result<()>,
) -> Result<()> {
    cached.sort_by_key(|entry| (!entry.stale, entry.modified));
    let mut count = cached.len().saturating_add(1);
    let mut bytes = cached
        .iter()
        .fold(incoming, |sum, entry| sum.saturating_add(entry.len));
    for entry in &cached {
        let capacity = count <= SPOOL_MAX_ENTRIES && bytes <= SPOOL_MAX_BYTES;
        let room = if capacity {
            space(incoming)
        } else {
            Err(FileError::Unavailable)
        };
        if capacity && room.is_ok() && !entry.stale {
            return Ok(());
        }
        if capacity
            && let Err(error) = &room
            && *error != FileError::InsufficientSpace
        {
            return Err(error.clone());
        }
        if entry.removable && remove(&entry.name) {
            count -= 1;
            bytes = bytes.saturating_sub(entry.len);
        }
    }
    if count > SPOOL_MAX_ENTRIES || bytes > SPOOL_MAX_BYTES {
        return Err(FileError::Unavailable);
    }
    space(incoming)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        time::{Duration, Instant, SystemTime},
    };

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }
    fn refused<T>(result: Result<T>, code: &str) {
        match result {
            Ok(_) => panic!("must refuse with {code}"),
            Err(error) => assert_eq!(error.code(), code),
        }
    }
    fn entry(index: usize, len: u64, stale: bool) -> CacheEntry {
        CacheEntry {
            name: ArtifactName::new(&format!("gz-{index}.log")).unwrap(),
            modified: SystemTime::UNIX_EPOCH,
            len,
            stale,
            removable: true,
        }
    }

    #[test]
    fn concatenated_empty_members_are_bounded_without_output() {
        let member = gzip(b"");
        let archive = member.repeat(100);
        let mut output = Vec::new();
        refused(
            decode_into(
                archive.as_slice(),
                &mut output,
                MAX_DECOMPRESSED_BYTES,
                (member.len() * 10) as u64,
                Instant::now() + DECOMPRESS_BUDGET,
                &mut |_| Ok(()),
            ),
            "file_too_large",
        );
        assert!(output.is_empty());
        // At the exact budget, genuine EOF is allowed instead of treating the cap
        // itself as evidence of an oversized archive.
        decode_into(
            member.as_slice(),
            &mut output,
            0,
            member.len() as u64,
            Instant::now() + DECOMPRESS_BUDGET,
            &mut |_| Ok(()),
        )
        .unwrap();
    }

    #[test]
    fn compressed_reader_checks_deadline_after_eof_and_before_work() {
        struct SlowEof;
        impl Read for SlowEof {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(Duration::from_millis(20));
                Ok(0)
            }
        }
        let mut input = CompressedInput {
            inner: SlowEof,
            remaining: 0,
            deadline: Instant::now() + Duration::from_millis(5),
            failure: None,
        };
        assert!(input.read(&mut [0]).is_err());
        assert_eq!(input.failure.unwrap().code(), "file_unavailable");
        let archive = gzip(b"").repeat(100);
        refused(
            decode_into(
                archive.as_slice(),
                &mut Vec::new(),
                0,
                MAX_COMPRESSED_BYTES,
                Instant::now(),
                &mut |_| Ok(()),
            ),
            "file_unavailable",
        );
    }

    #[test]
    fn admission_counts_the_newly_kept_view_bytes_before_publication() {
        let mut removed = Vec::new();
        reserve_cache(
            vec![entry(1, 600 * 1024 * 1024, false)],
            MAX_DECOMPRESSED_BYTES,
            |name| {
                removed.push(name.as_str().to_owned());
                true
            },
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(removed, ["gz-1.log"]);
        refused(
            reserve_cache(
                vec![entry(1, 600 * 1024 * 1024, false)],
                MAX_DECOMPRESSED_BYTES,
                |_| false,
                &mut |_| Ok(()),
            ),
            "file_unavailable",
        );
    }

    #[test]
    fn unremovable_expired_views_still_consume_count_and_byte_capacity() {
        refused(
            reserve_cache(
                (0..SPOOL_MAX_ENTRIES).map(|i| entry(i, 1, true)).collect(),
                1,
                |_| false,
                &mut |_| Ok(()),
            ),
            "file_unavailable",
        );
        refused(
            reserve_cache(
                vec![entry(1, SPOOL_MAX_BYTES, true)],
                1,
                |_| false,
                &mut |_| Ok(()),
            ),
            "file_unavailable",
        );
        // Successfully reclaimed expired files release capacity.
        reserve_cache(
            (0..SPOOL_MAX_ENTRIES).map(|i| entry(i, 1, true)).collect(),
            1,
            |_| true,
            &mut |_| Ok(()),
        )
        .unwrap();
    }

    #[test]
    fn insufficient_volume_space_refuses_before_stage_creation() {
        let temp = tempfile::tempdir().unwrap();
        let state = vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let spool = state.spool().unwrap();
        let stage = ArtifactName::new("gz-new.tmp").unwrap();
        let mut checked = Vec::new();
        refused(
            create_stage(&spool, &stage, 123, &mut |incoming| {
                checked.push(incoming);
                Err(FileError::InsufficientSpace)
            }),
            "file_insufficient_space",
        );
        assert_eq!(checked, [123]);
        assert!(spool.open(&stage).unwrap().is_none());
        assert_eq!(spool.count(256).unwrap(), Some(0));
    }

    #[test]
    fn volume_pressure_reclaims_cache_and_is_rechecked_before_each_write() {
        let reclaimed = std::cell::Cell::new(false);
        reserve_cache(
            vec![entry(1, 1, false)],
            10,
            |_| {
                reclaimed.set(true);
                true
            },
            &mut |incoming| {
                assert_eq!(incoming, 10);
                if reclaimed.get() {
                    Ok(())
                } else {
                    Err(FileError::InsufficientSpace)
                }
            },
        )
        .unwrap();
        let archive = gzip(&vec![b'x'; FILE_CHUNK_BYTES * 3]);
        let mut output = Vec::new();
        let mut checks = 0;
        refused(
            decode_into(
                archive.as_slice(),
                &mut output,
                (FILE_CHUNK_BYTES * 3) as u64,
                MAX_COMPRESSED_BYTES,
                Instant::now() + DECOMPRESS_BUDGET,
                &mut |incoming| {
                    assert!(incoming > 0 && incoming <= FILE_CHUNK_BYTES as u64);
                    checks += 1;
                    if checks == 1 {
                        Ok(())
                    } else {
                        Err(FileError::InsufficientSpace)
                    }
                },
            ),
            "file_insufficient_space",
        );
        assert_eq!(checks, 2);
        assert!(!output.is_empty());
        assert!(output.len() <= FILE_CHUNK_BYTES);
    }

    #[cfg(windows)]
    #[test]
    fn windows_open_expired_views_refuse_new_staging_when_reclaim_fails() {
        use std::os::windows::fs::OpenOptionsExt;
        let temp = tempfile::tempdir().unwrap();
        let state = vesta_state::StateDirectory::new(temp.path().join("state"), None).unwrap();
        state.prepare().unwrap();
        let spool = state.spool().unwrap();
        let mut held = Vec::new();
        for i in 0..SPOOL_MAX_ENTRIES {
            let name = ArtifactName::new(&format!("gz-{i}.log")).unwrap();
            let mut file = spool.create_new(&name).unwrap();
            file.write_all(b"x").unwrap();
            file.set_modified(SystemTime::now() - SPOOL_VIEW_TTL - Duration::from_secs(1))
                .unwrap();
            drop(file);
            held.push(
                std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(3)
                    .open(state.path().join("spool").join(name.as_str()))
                    .unwrap(),
            );
        }
        let stage = ArtifactName::new("gz-new.tmp").unwrap();
        refused(
            create_stage(&spool, &stage, 1, &mut |_| Ok(())),
            "file_unavailable",
        );
        assert!(spool.open(&stage).unwrap().is_none());
        assert_eq!(spool.count(256).unwrap(), Some(SPOOL_MAX_ENTRIES));
        drop(held);
        drop(create_stage(&spool, &stage, 1, &mut |_| Ok(())).unwrap());
        assert_eq!(spool.count(256).unwrap(), Some(1));
    }
}
