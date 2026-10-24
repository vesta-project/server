//! Bounded world metadata and explicitly selected fresh-world configuration.
use crate::files::{FileError, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;

pub const MAX_LEVEL_BYTES: usize = crate::files::MAX_READ_BYTES;
const MAX_NBT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldStatus {
    pub name: String,
    pub seed: Option<String>,
    pub configured_seed: String,
    pub exists: bool,
    pub revision: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewWorld {
    pub seed: String,
    pub expected_revision: String,
    pub confirm_world: String,
}

/// Embedded seeds before 26.1; None means a valid level needs its split worldgen file.
pub(crate) fn seed(bytes: &[u8]) -> Result<Option<String>> {
    parse_seed(bytes, false)
}

/// Java 26.1+ stores the seed in data/minecraft/world_gen_settings.dat.
pub(crate) fn world_generation_seed(bytes: &[u8]) -> Result<String> {
    parse_seed(bytes, true)?.ok_or(FileError::Corrupt)
}

fn parse_seed(bytes: &[u8], split: bool) -> Result<Option<String>> {
    if bytes.len() > MAX_LEVEL_BYTES {
        return Err(FileError::TooLarge);
    }
    let mut expanded = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take((MAX_NBT_BYTES + 1) as u64)
        .read_to_end(&mut expanded)
        .map_err(|_| FileError::Corrupt)?;
    if expanded.len() > MAX_NBT_BYTES {
        return Err(FileError::TooLarge);
    }
    let mut nbt = Nbt {
        bytes: &expanded,
        offset: 0,
        work: 200_000,
        modern: None,
        legacy: None,
        split,
        level_data: false,
        embedded_worldgen: false,
    };
    if nbt.byte()? != 10 {
        return Err(FileError::Corrupt);
    }
    nbt.text()?;
    nbt.value(10, &mut Vec::new(), 0)?;
    if nbt.offset != expanded.len() {
        return Err(FileError::Corrupt);
    }
    if !split && !nbt.level_data {
        return Err(FileError::Corrupt);
    }
    if !split && nbt.embedded_worldgen && nbt.modern.is_none() && nbt.legacy.is_none() {
        return Err(FileError::Corrupt);
    }
    Ok(nbt.modern.or(nbt.legacy).map(|seed| seed.to_string()))
}

struct Nbt<'a> {
    bytes: &'a [u8],
    offset: usize,
    work: usize,
    modern: Option<i64>,
    legacy: Option<i64>,
    split: bool,
    level_data: bool,
    embedded_worldgen: bool,
}
impl<'a> Nbt<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.offset.checked_add(count).ok_or(FileError::Corrupt)?;
        let value = self.bytes.get(self.offset..end).ok_or(FileError::Corrupt)?;
        self.offset = end;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn text(&mut self) -> Result<&'a [u8]> {
        let count =
            u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| FileError::Corrupt)?) as usize;
        self.take(count)
    }
    fn count(&mut self) -> Result<usize> {
        let count = i32::from_be_bytes(self.take(4)?.try_into().map_err(|_| FileError::Corrupt)?);
        usize::try_from(count).map_err(|_| FileError::Corrupt)
    }
    fn value(&mut self, tag: u8, path: &mut Vec<Vec<u8>>, depth: usize) -> Result<()> {
        if depth > 48 || self.work == 0 {
            return Err(FileError::TooLarge);
        }
        self.work -= 1;
        let seed_path = if self.split {
            path.len() == 2 && path[0] == b"data" && path[1] == b"seed"
        } else {
            (path.len() == 2 && path[0] == b"Data" && path[1] == b"RandomSeed")
                || (path.len() == 3
                    && path[0] == b"Data"
                    && path[1] == b"WorldGenSettings"
                    && path[2] == b"seed")
        };
        if seed_path && tag != 4 {
            return Err(FileError::Corrupt);
        }
        match tag {
            1 => {
                self.take(1)?;
            }
            2 => {
                self.take(2)?;
            }
            3 | 5 => {
                self.take(4)?;
            }
            4 => {
                let value =
                    i64::from_be_bytes(self.take(8)?.try_into().map_err(|_| FileError::Corrupt)?);
                let names: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                let slot = if (self.split && names == [b"data".as_slice(), b"seed"])
                    || (!self.split && names == [b"Data".as_slice(), b"WorldGenSettings", b"seed"])
                {
                    Some(&mut self.modern)
                } else if !self.split && names == [b"Data".as_slice(), b"RandomSeed"] {
                    Some(&mut self.legacy)
                } else {
                    None
                };
                if let Some(slot) = slot
                    && slot.replace(value).is_some()
                {
                    return Err(FileError::Corrupt);
                }
            }
            6 => {
                self.take(8)?;
            }
            7 | 11 | 12 => {
                let count = self.count()?;
                let width = match tag {
                    11 => 4,
                    12 => 8,
                    _ => 1,
                };
                self.take(count.checked_mul(width).ok_or(FileError::TooLarge)?)?;
            }
            8 => {
                self.text()?;
            }
            9 => {
                let element = self.byte()?;
                let count = self.count()?;
                if count > self.work || (element == 0 && count != 0) {
                    return Err(FileError::TooLarge);
                }
                // List contents do not extend a named compound path.
                path.push(Vec::new());
                for _ in 0..count {
                    self.value(element, path, depth + 1)?;
                }
                path.pop();
            }
            10 => {
                if !self.split
                    && path.len() == 2
                    && path[0] == b"Data"
                    && path[1] == b"WorldGenSettings"
                {
                    self.embedded_worldgen = true;
                }
                if !self.split && path.len() == 1 && path[0] == b"Data" {
                    if self.level_data {
                        return Err(FileError::Corrupt);
                    }
                    self.level_data = true;
                }
                loop {
                    let child = self.byte()?;
                    if child == 0 {
                        break;
                    }
                    path.push(self.text()?.to_vec());
                    self.value(child, path, depth + 1)?;
                    path.pop();
                }
            }
            _ => return Err(FileError::Corrupt),
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    pub(crate) fn level(seed: i64, modern: bool) -> Vec<u8> {
        let mut nbt = vec![10, 0, 0, 10, 0, 4, b'D', b'a', b't', b'a'];
        if modern {
            nbt.extend([10, 0, 16]);
            nbt.extend(b"WorldGenSettings");
        }
        let key = if modern { "seed" } else { "RandomSeed" };
        nbt.push(4);
        nbt.extend((key.len() as u16).to_be_bytes());
        nbt.extend(key.as_bytes());
        nbt.extend(seed.to_be_bytes());
        nbt.extend(if modern { vec![0, 0, 0] } else { vec![0, 0] });
        gzip(&nbt)
    }
    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }
    #[test]
    fn modern_and_legacy_seeds_keep_full_signed_precision() {
        for modern in [true, false] {
            for value in [i64::MIN, i64::MAX, 0, -4172144997902289642] {
                assert_eq!(
                    seed(&level(value, modern)).unwrap(),
                    Some(value.to_string())
                );
            }
        }
    }
    pub(crate) fn split_level() -> Vec<u8> {
        // The world seed is absent from the new level.dat, but Data is still required.
        gzip(&[10, 0, 0, 10, 0, 4, b'D', b'a', b't', b'a', 0, 0])
    }
    pub(crate) fn split_worldgen(value: i64) -> Vec<u8> {
        let mut nbt = vec![10, 0, 0, 10, 0, 4, b'd', b'a', b't', b'a', 4, 0, 4];
        nbt.extend(b"seed");
        nbt.extend(value.to_be_bytes());
        nbt.extend([0, 0]);
        gzip(&nbt)
    }
    #[test]
    fn split_worldgen_seeds_preserve_precision_and_do_not_leak_into_level_parser() {
        assert_eq!(seed(&split_level()).unwrap(), None);
        for value in [i64::MIN, i64::MAX, 0, -4172144997902289642] {
            let split = split_worldgen(value);
            assert_eq!(world_generation_seed(&split).unwrap(), value.to_string());
            assert!(seed(&split).is_err());
            assert!(world_generation_seed(&level(value, true)).is_err());
        }
        assert!(world_generation_seed(&split_level()).is_err());
    }
    #[test]
    fn malformed_and_oversized_world_metadata_refuses() {
        for bytes in [
            b"not gzip".to_vec(),
            gzip(&[10, 0, 0, 4]),
            gzip(&[10, 0, 0, 0, 0]),
        ] {
            assert!(seed(&bytes).is_err());
        }
        assert!(matches!(
            seed(&gzip(&vec![0; MAX_NBT_BYTES + 1])),
            Err(FileError::TooLarge)
        ));
    }
}
