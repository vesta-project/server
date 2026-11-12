use super::{Error, Input, Result, model::BuildLimits};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    time::Instant,
};
use zip::ZipArchive;

pub(crate) struct Archive {
    pub id: String,
    pub expected: String,
    pub zip: ZipArchive<File>,
}
#[derive(Clone, Copy)]
pub(crate) struct Member {
    pub pack: usize,
    pub index: usize,
    pub size: u64,
}

pub(crate) fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(Error::TimedOut)
    } else {
        Ok(())
    }
}

pub(crate) fn hashes(
    file: &mut File,
    limit: u64,
    deadline: Instant,
) -> Result<(String, String, u64)> {
    use sha1::Sha1;
    file.seek(SeekFrom::Start(0))?;
    let (mut sha256, mut sha1, mut size) = (Sha256::new(), Sha1::new(), 0u64);
    let mut buffer = [0u8; 32 * 1024];
    loop {
        check(deadline)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .filter(|n| *n <= limit)
            .ok_or(Error::TooLarge)?;
        sha256.update(&buffer[..count]);
        sha1.update(&buffer[..count]);
    }
    Ok((
        hex::encode(sha256.finalize()),
        hex::encode(sha1.finalize()),
        size,
    ))
}

/// Check bounded ordinary ZIP central-directory counts before the ZIP parser
/// allocates its member table. Multi-volume/ZIP64 archives are outside this API.
fn preflight(file: &mut File, size: u64, entries: usize) -> Result<()> {
    if size < 22 {
        return Err(Error::InvalidArchive);
    }
    let tail_size = size.min(22 + 65535) as usize;
    file.seek(SeekFrom::End(-(tail_size as i64)))?;
    let mut tail = vec![0u8; tail_size];
    file.read_exact(&mut tail)?;
    let offset = (0..=tail_size - 22)
        .rev()
        .find(|at| {
            tail[*at..*at + 4] == *b"PK\x05\x06"
                && *at + 22 + u16::from_le_bytes([tail[*at + 20], tail[*at + 21]]) as usize
                    == tail_size
        })
        .ok_or(Error::InvalidArchive)?;
    let u16_at = |at| u16::from_le_bytes([tail[offset + at], tail[offset + at + 1]]);
    let u32_at = |at| u32::from_le_bytes(tail[offset + at..offset + at + 4].try_into().unwrap());
    let count = u16_at(10);
    if u16_at(4) != 0
        || u16_at(6) != 0
        || u16_at(8) != count
        || count == u16::MAX
        || u32_at(12) == u32::MAX
        || u32_at(16) == u32::MAX
    {
        return Err(Error::InvalidArchive);
    }
    if count as usize > entries || u32_at(12) > 8 * 1024 * 1024 {
        return Err(Error::TooLarge);
    }
    if u32_at(16) as u64 + u32_at(12) as u64 > size - tail_size as u64 + offset as u64 {
        return Err(Error::InvalidArchive);
    }
    // ZipArchive indexes by name and can hide duplicate central-directory
    // members. Validate the raw table before handing it to that parser.
    let directory_size = u32_at(12) as usize;
    file.seek(SeekFrom::Start(u32_at(16) as u64))?;
    let mut directory = vec![0u8; directory_size];
    file.read_exact(&mut directory)?;
    let mut cursor = 0usize;
    let mut names = std::collections::BTreeSet::new();
    for _ in 0..count {
        let header = directory
            .get(cursor..cursor.saturating_add(46))
            .ok_or(Error::InvalidArchive)?;
        if &header[..4] != b"PK\x01\x02" {
            return Err(Error::InvalidArchive);
        }
        let field = |at| u16::from_le_bytes([header[at], header[at + 1]]) as usize;
        let name_end = cursor + 46 + field(28);
        let next = name_end + field(30) + field(32);
        if next > directory.len() {
            return Err(Error::InvalidArchive);
        }
        let name = std::str::from_utf8(&directory[cursor + 46..name_end])
            .map_err(|_| Error::InvalidArchive)?;
        let normalized = name.strip_suffix('/').unwrap_or(name);
        if !portable(normalized) || !names.insert(normalized.to_owned()) {
            return Err(Error::InvalidArchive);
        }
        cursor = next;
    }
    if cursor != directory.len() {
        return Err(Error::InvalidArchive);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

pub(crate) fn open(input: Input, limits: BuildLimits, deadline: Instant) -> Result<Archive> {
    let Input {
        id,
        mut file,
        sha256,
    } = input;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::InvalidInput);
    }
    if metadata.len() > limits.compressed_bytes {
        return Err(Error::TooLarge);
    }
    if hashes(&mut file, limits.compressed_bytes, deadline)?.0 != sha256 {
        return Err(Error::ChangedInput);
    }
    preflight(&mut file, metadata.len(), limits.entries)?;
    let zip = ZipArchive::new(file).map_err(|_| Error::InvalidArchive)?;
    if zip.len() > limits.entries {
        return Err(Error::TooLarge);
    }
    Ok(Archive {
        id,
        expected: sha256,
        zip,
    })
}

pub(crate) fn portable(name: &str) -> bool {
    name.len() <= 240 && vesta_protocol::pack::catalogue::RelativePath::parse(name).is_ok()
}

pub(crate) fn copy_member<W: Write>(
    archive: &mut Archive,
    member: Member,
    sink: &mut W,
    limit: u64,
    deadline: Instant,
) -> Result<()> {
    let mut file = archive
        .zip
        .by_index(member.index)
        .map_err(|_| Error::InvalidArchive)?;
    let mut buffer = [0u8; 32 * 1024];
    let mut total = 0u64;
    loop {
        check(deadline)?;
        let count = file.read(&mut buffer).map_err(|_| Error::InvalidArchive)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|n| *n <= limit && *n <= member.size)
            .ok_or(Error::TooLarge)?;
        sink.write_all(&buffer[..count])?;
    }
    if total != member.size {
        return Err(Error::InvalidArchive);
    }
    Ok(())
}

pub(crate) fn json(
    archive: &mut Archive,
    member: Member,
    limit: usize,
    deadline: Instant,
) -> Result<serde_json::Value> {
    if member.size > limit as u64 {
        return Err(Error::TooLarge);
    }
    let mut bytes = Vec::with_capacity(member.size as usize);
    copy_member(archive, member, &mut bytes, limit as u64, deadline)?;
    serde_json::from_slice(&bytes).map_err(|_| Error::InvalidMetadata)
}
