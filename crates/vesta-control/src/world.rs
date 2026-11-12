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
    #[serde(flatten)]
    pub details: WorldDetails,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewWorld {
    pub seed: String,
    pub expected_revision: String,
    pub confirm_world: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineDatapackPatch {
    pub pack_id: String,
    pub enabled: bool,
    pub expected_revision: String,
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
    parse_level(bytes, split).map(|v| v.0)
}
pub(crate) fn details(bytes: &[u8]) -> Result<LevelDetails> {
    parse_level(bytes, false).map(|v| v.1)
}
fn parse_level(bytes: &[u8], split: bool) -> Result<(Option<String>, LevelDetails)> {
    parse_document(bytes, split).map(|(seed, details, _, _)| (seed, details))
}
type PackRanges = [Option<(usize, usize)>; 2];
type LevelDocument = (Option<String>, LevelDetails, Vec<u8>, PackRanges);
fn parse_document(bytes: &[u8], split: bool) -> Result<LevelDocument> {
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
        details: LevelDetails::default(),
        pack_ranges: [None, None],
        datapack_compound: false,
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
    let seed = nbt.modern.or(nbt.legacy).map(|seed| seed.to_string());
    let details = nbt.details;
    let ranges = nbt.pack_ranges;
    Ok((seed, details, expanded, ranges))
}

/// Change only the two selection lists; retain every other NBT byte verbatim.
pub(crate) fn toggle_datapack(bytes: &[u8], id: &str, enabled: bool) -> Result<Vec<u8>> {
    use std::io::Write;
    if !id.starts_with("file/") || id.len() > 1024 || id.chars().any(char::is_control) {
        return Err(FileError::InvalidPath);
    }
    let (_, details, mut document, ranges) = parse_document(bytes, false)?;
    let mut lists = [
        details.enabled.ok_or(FileError::Corrupt)?,
        details.disabled.ok_or(FileError::Corrupt)?,
    ];
    if lists[0].iter().any(|pack| lists[1].contains(pack)) {
        return Err(FileError::Corrupt);
    }
    if lists[usize::from(!enabled)].iter().any(|pack| pack == id) {
        return Ok(bytes.to_vec());
    }
    for list in &mut lists {
        list.retain(|pack| pack != id);
    }
    lists[usize::from(!enabled)].push(id.to_owned());
    let mut replacements = Vec::new();
    for (index, list) in lists.iter().enumerate() {
        if list.len() > 2048 {
            return Err(FileError::TooLarge);
        }
        let range = ranges[index].ok_or(FileError::Corrupt)?;
        let mut payload = vec![8];
        payload.extend_from_slice(&(list.len() as i32).to_be_bytes());
        for pack in list {
            let encoded = nbt_text_encode(pack);
            payload.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
            payload.extend_from_slice(&encoded);
        }
        replacements.push((range, payload));
    }
    replacements.sort_by_key(|(range, _)| std::cmp::Reverse(range.0));
    for ((start, end), payload) in replacements {
        document.splice(start..end, payload);
    }
    if document.len() > MAX_NBT_BYTES {
        return Err(FileError::TooLarge);
    }
    let mut compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    compressed
        .write_all(&document)
        .map_err(|_| FileError::Unavailable)?;
    let result = compressed.finish().map_err(|_| FileError::Unavailable)?;
    if result.len() > MAX_LEVEL_BYTES {
        return Err(FileError::TooLarge);
    }
    self::details(&result)?;
    Ok(result)
}

// Java NBT strings use modified UTF-8, including UTF-16 surrogate pairs.
fn nbt_text_encode(text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for unit in text.encode_utf16() {
        match unit {
            1..=0x7f => bytes.push(unit as u8),
            0..=0x7ff => {
                bytes.push(0xc0 | (unit >> 6) as u8);
                bytes.push(0x80 | (unit & 63) as u8);
            }
            _ => {
                bytes.push(0xe0 | (unit >> 12) as u8);
                bytes.push(0x80 | ((unit >> 6) & 63) as u8);
                bytes.push(0x80 | (unit & 63) as u8);
            }
        }
    }
    bytes
}
fn nbt_text_decode(bytes: &[u8]) -> Result<String> {
    let mut units = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index];
        index += 1;
        let count = match first {
            0..=0x7f => 0,
            0xc0..=0xdf => 1,
            0xe0..=0xef => 2,
            _ => return Err(FileError::Corrupt),
        };
        let mut unit = u16::from(
            first
                & match count {
                    0 => 0x7f,
                    1 => 0x1f,
                    _ => 0x0f,
                },
        );
        for _ in 0..count {
            let next = *bytes.get(index).ok_or(FileError::Corrupt)?;
            index += 1;
            if next & 0xc0 != 0x80 {
                return Err(FileError::Corrupt);
            }
            unit = (unit << 6) | u16::from(next & 63);
        }
        if (count == 1 && unit < 128 && unit != 0) || (count == 2 && unit < 0x800) {
            return Err(FileError::Corrupt);
        }
        units.push(unit);
    }
    String::from_utf16(&units).map_err(|_| FileError::Corrupt)
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
    details: LevelDetails,
    pack_ranges: PackRanges,
    datapack_compound: bool,
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
        if path.len() == 2 && path[0] == b"Data" && path[1] == b"DataPacks" && tag != 10 {
            return Err(FileError::Corrupt);
        }
        if path.len() == 3
            && path[0] == b"Data"
            && path[1] == b"DataPacks"
            && matches!(path[2].as_slice(), b"Enabled" | b"Disabled")
            && tag != 9
        {
            return Err(FileError::Corrupt);
        }
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
                if names == [b"Data".as_slice(), b"LastPlayed"] {
                    self.details.last_played_at_ms = Some(value);
                }
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
                let payload_start = self.offset;
                let element = self.byte()?;
                let count = self.count()?;
                if count > self.work || (element == 0 && count != 0) {
                    return Err(FileError::TooLarge);
                }
                if path.len() == 3
                    && path[0] == b"Data"
                    && path[1] == b"DataPacks"
                    && matches!(path[2].as_slice(), b"Enabled" | b"Disabled")
                {
                    if (element != 8 && !(element == 0 && count == 0)) || count > 2048 {
                        return Err(FileError::Corrupt);
                    }
                    let mut packs = Vec::new();
                    for _ in 0..count {
                        let value = nbt_text_decode(self.text()?)?;
                        if value.len() > 1024 || packs.contains(&value) {
                            return Err(FileError::Corrupt);
                        }
                        packs.push(value);
                    }
                    let slot = if path[2] == b"Enabled" {
                        &mut self.details.enabled
                    } else {
                        &mut self.details.disabled
                    };
                    if slot.replace(packs).is_some() {
                        return Err(FileError::Corrupt);
                    }
                    self.pack_ranges[usize::from(path[2] == b"Disabled")] =
                        Some((payload_start, self.offset));
                    return Ok(());
                }
                // List contents do not extend a named compound path.
                path.push(Vec::new());
                for _ in 0..count {
                    self.value(element, path, depth + 1)?;
                }
                path.pop();
            }
            10 => {
                if path.len() == 2 && path[0] == b"Data" && path[1] == b"DataPacks" {
                    if self.datapack_compound {
                        return Err(FileError::Corrupt);
                    }
                    self.datapack_compound = true;
                }
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorldDetails {
    pub size_bytes: Option<String>,
    pub size_complete: bool,
    pub size_observed_at_ms: Option<i64>,
    pub last_modified_at_ms: Option<i64>,
    pub last_played_at_ms: Option<i64>,
    pub dimensions: Vec<WorldDimension>,
    pub datapacks: Vec<Datapack>,
    pub datapack_state_source: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorldDimension {
    pub id: String,
    pub present: bool,
    pub folders: Vec<String>,
    pub layout: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Datapack {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub path: Option<String>,
    pub enabled: Option<bool>,
}
#[derive(Default)]
pub(crate) struct LevelDetails {
    pub last_played_at_ms: Option<i64>,
    pub enabled: Option<Vec<String>>,
    pub disabled: Option<Vec<String>>,
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
    pub(crate) fn level_with_packs() -> Vec<u8> {
        let mut nbt = vec![10, 0, 0, 10, 0, 4, b'D', b'a', b't', b'a'];
        for (key, value) in [("RandomSeed", 42i64), ("LastPlayed", 1791115200000i64)] {
            nbt.push(4);
            nbt.extend((key.len() as u16).to_be_bytes());
            nbt.extend(key.as_bytes());
            nbt.extend(value.to_be_bytes());
        }
        nbt.extend([10, 0, 9]);
        nbt.extend(b"DataPacks");
        for (key, values) in [
            ("Enabled", vec!["vanilla", "file/enabled.zip"]),
            ("Disabled", vec!["file/disabled"]),
        ] {
            nbt.push(9);
            nbt.extend((key.len() as u16).to_be_bytes());
            nbt.extend(key.as_bytes());
            nbt.push(8);
            nbt.extend((values.len() as i32).to_be_bytes());
            for value in values {
                nbt.extend((value.len() as u16).to_be_bytes());
                nbt.extend(value.as_bytes());
            }
        }
        nbt.extend([0, 0, 0]);
        gzip(&nbt)
    }
    #[test]
    fn saved_datapack_lists_and_last_played_are_bounded_metadata() {
        let details = details(&level_with_packs()).unwrap();
        assert_eq!(details.last_played_at_ms, Some(1791115200000));
        assert_eq!(
            details.enabled.unwrap(),
            vec!["vanilla", "file/enabled.zip"]
        );
        assert_eq!(details.disabled.unwrap(), vec!["file/disabled"]);
    }
    #[test]
    fn offline_datapack_selection_preserves_world_bytes_and_order() {
        let original = level_with_packs();
        let edited = toggle_datapack(&original, "file/disabled", true).unwrap();
        let selected = details(&edited).unwrap();
        assert_eq!(
            selected.enabled.unwrap(),
            ["vanilla", "file/enabled.zip", "file/disabled"]
        );
        assert!(selected.disabled.unwrap().is_empty());
        assert_eq!(seed(&edited).unwrap(), seed(&original).unwrap());
        assert_eq!(
            toggle_datapack(&edited, "file/disabled", true).unwrap(),
            edited
        );
        let restored = toggle_datapack(&edited, "file/disabled", false).unwrap();
        let expand = |bytes: &[u8]| {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes)
                .read_to_end(&mut out)
                .unwrap();
            out
        };
        assert_eq!(expand(&restored), expand(&original));
    }
    #[test]
    fn offline_datapack_selection_handles_java_unicode_and_rejects_unsafe_inputs() {
        let original = level_with_packs();
        let name = "file/雪😀.zip";
        let changed = toggle_datapack(&original, name, true).unwrap();
        assert!(
            details(&changed)
                .unwrap()
                .enabled
                .unwrap()
                .contains(&name.into())
        );
        for id in [
            "vanilla",
            "paper",
            "minecart_improvements",
            "file/bad\nname",
        ] {
            assert!(toggle_datapack(&original, id, false).is_err());
        }
        assert!(toggle_datapack(b"not gzip", "file/pack.zip", true).is_err());
        assert!(toggle_datapack(&level(42, true), "file/pack.zip", true).is_err());
        assert!(nbt_text_decode(&[0xed, 0xa0, 0x80]).is_err());
        assert!(nbt_text_decode(&[0xc0, 0xaf]).is_err());
    }
    #[test]
    fn offline_datapack_selection_accepts_empty_end_typed_lists_and_refuses_wrong_tags() {
        let edited = toggle_datapack(&level_with_packs(), "file/disabled", true).unwrap();
        let (_, _, mut document, ranges) = parse_document(&edited, false).unwrap();
        let disabled_start = ranges[1].unwrap().0;
        document[disabled_start] = 0;
        let empty_end = gzip(&document);
        assert!(details(&empty_end).unwrap().disabled.unwrap().is_empty());
        assert!(toggle_datapack(&empty_end, "file/enabled.zip", false).is_ok());
        document[disabled_start - 3 - "Disabled".len()] = 8;
        assert!(toggle_datapack(&gzip(&document), "file/enabled.zip", false).is_err());
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
