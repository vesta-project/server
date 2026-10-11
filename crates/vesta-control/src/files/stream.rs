//! Bounded observations of growing regular files; no persistent read grants.
use super::{FileBrowser, FileError, RelativePath, Result, authorize, io_error, native};
use crate::identity::{self, Capability, Subject};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    time::UNIX_EPOCH,
};

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
