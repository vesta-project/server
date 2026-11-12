use super::{Error, PackFormat, Resolution, Result};
use serde_json::{Map, Value, json};

fn format(value: &Value, maximum: bool) -> Result<PackFormat> {
    let implicit_minor = if maximum { i32::MAX as u64 } else { 0 };
    let (major, minor) = if let Some(n) = value.as_u64() {
        (n, implicit_minor)
    } else if let Some(pair) = value.as_array().filter(|a| (1..=2).contains(&a.len())) {
        (
            pair[0].as_u64().ok_or(Error::InvalidMetadata)?,
            if pair.len() == 2 {
                pair[1].as_u64().ok_or(Error::InvalidMetadata)?
            } else {
                implicit_minor
            },
        )
    } else {
        return Err(Error::InvalidMetadata);
    };
    PackFormat::new(
        major.try_into().map_err(|_| Error::InvalidMetadata)?,
        minor.try_into().map_err(|_| Error::InvalidMetadata)?,
    )
}

pub(crate) fn metadata(value: &Value, target: PackFormat) -> Result<()> {
    let root = value.as_object().ok_or(Error::InvalidMetadata)?;
    let pack = root
        .get("pack")
        .and_then(Value::as_object)
        .ok_or(Error::InvalidMetadata)?;
    if !pack.contains_key("description") {
        return Err(Error::InvalidMetadata);
    }
    let modern = pack.contains_key("min_format") || pack.contains_key("max_format");
    if !modern && pack.get("pack_format").and_then(Value::as_u64).is_none() {
        return Err(Error::InvalidMetadata);
    }
    let (low, high) = if modern {
        (
            format(pack.get("min_format").ok_or(Error::InvalidMetadata)?, false)?,
            format(pack.get("max_format").ok_or(Error::InvalidMetadata)?, true)?,
        )
    } else if let Some(supported) = pack.get("supported_formats") {
        if let Some(range) = supported.as_array().filter(|a| a.len() == 2) {
            (format(&range[0], false)?, format(&range[1], true)?)
        } else if let Some(range) = supported.as_object() {
            (
                format(
                    range.get("min_inclusive").ok_or(Error::InvalidMetadata)?,
                    false,
                )?,
                format(
                    range.get("max_inclusive").ok_or(Error::InvalidMetadata)?,
                    true,
                )?,
            )
        } else {
            (format(supported, false)?, format(supported, true)?)
        }
    } else {
        let value = pack.get("pack_format").ok_or(Error::InvalidMetadata)?;
        (format(value, false)?, format(value, true)?)
    };
    if (modern && low.major >= 65 && pack.contains_key("supported_formats"))
        || (!modern && high.major >= 65)
        || (modern
            && low.major < 65
            && (!pack.contains_key("supported_formats")
                || pack.get("pack_format").and_then(Value::as_u64).is_none()))
    {
        return Err(Error::InvalidMetadata);
    }
    if low > high {
        return Err(Error::InvalidMetadata);
    }
    if target < low || target > high {
        return Err(Error::IncompatibleFormat);
    }
    // Pack-boundary filtering and conditional overlays cannot be treated as
    // ordinary priority overrides. Never silently discard their semantics.
    for (key, entries) in [("filter", "block"), ("overlays", "entries")] {
        if let Some(section) = root.get(key) {
            let empty = section.as_object().is_some_and(|o| {
                o.is_empty()
                    || (o.len() == 1
                        && o.get(entries)
                            .and_then(Value::as_array)
                            .is_some_and(Vec::is_empty))
            });
            if !empty {
                return Err(Error::UnsupportedMetadata);
            }
        }
    }
    Ok(())
}

pub(crate) fn output_metadata(
    inputs: &[Value],
    target: PackFormat,
    description: &str,
) -> Result<Value> {
    let mut root = Map::new();
    let mut languages = Map::new();
    for input in inputs {
        for (key, value) in input.as_object().ok_or(Error::InvalidMetadata)? {
            match key.as_str() {
                "pack" | "filter" | "overlays" => (),
                "language" => {
                    languages.extend(value.as_object().ok_or(Error::InvalidMetadata)?.clone())
                }
                _ => {
                    root.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let pack = if target.major < 65 {
        if target.minor != 0 {
            return Err(Error::InvalidMetadata);
        }
        json!({"description":description,"pack_format":target.major})
    } else {
        // Modern-only packs must omit supported_formats. Emit explicit pairs
        // because a scalar maximum permits every minor version of its major.
        json!({"description":description,"min_format":[target.major,target.minor],"max_format":[target.major,target.minor]})
    };
    root.insert("pack".into(), pack);
    if !languages.is_empty() {
        root.insert("language".into(), Value::Object(languages));
    }
    Ok(Value::Object(root))
}

pub(crate) fn resolution(path: &str) -> Resolution {
    let parts: Vec<_> = path.split('/').collect();
    if parts.first() != Some(&"assets") || !path.ends_with(".json") {
        return Resolution::HigherPriority;
    }
    if parts.len() == 3 && parts[2] == "sounds.json" {
        return Resolution::SoundEvents;
    }
    if parts.len() >= 4 {
        match parts[2] {
            "lang" => Resolution::LanguageKeys,
            "atlases" => Resolution::AtlasSources,
            "font" => Resolution::FontProviders,
            _ => Resolution::HigherPriority,
        }
    } else {
        Resolution::HigherPriority
    }
}

/// Fold low to high. Arrays retain order and duplicates because they can change
/// weighted sounds, font fallback and sequential atlas filter behavior.
pub(crate) fn merge(kind: Resolution, current: Option<Value>, mut next: Value) -> Result<Value> {
    let next_object = next.as_object_mut().ok_or(Error::InvalidMetadata)?;
    match kind {
        Resolution::LanguageKeys => {
            if next_object.values().any(|v| !v.is_string()) {
                return Err(Error::InvalidMetadata);
            }
            let mut result = current
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            result.extend(next_object.clone());
            Ok(Value::Object(result))
        }
        Resolution::AtlasSources | Resolution::FontProviders => {
            let field = if kind == Resolution::AtlasSources {
                "sources"
            } else {
                "providers"
            };
            let incoming = next_object
                .get_mut(field)
                .and_then(Value::as_array_mut)
                .ok_or(Error::InvalidMetadata)?;
            if incoming.iter().any(|entry| !entry.is_object()) {
                return Err(Error::InvalidMetadata);
            }
            if let Some(mut previous) = current {
                let previous = previous
                    .get_mut(field)
                    .and_then(Value::as_array_mut)
                    .ok_or(Error::InvalidMetadata)?;
                if kind == Resolution::AtlasSources {
                    let mut combined = std::mem::take(previous);
                    combined.append(incoming);
                    *incoming = combined;
                } else {
                    incoming.append(previous);
                }
            }
            Ok(next)
        }
        Resolution::SoundEvents => {
            let mut result = current
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            for (name, event) in next_object {
                let event = event.as_object_mut().ok_or(Error::InvalidMetadata)?;
                if event.get("replace").is_some_and(|v| !v.is_boolean())
                    || event.get("subtitle").is_some_and(|v| !v.is_string())
                {
                    return Err(Error::InvalidMetadata);
                }
                if !event.contains_key("sounds") {
                    event.insert("sounds".into(), json!([]));
                }
                let sounds = event
                    .get("sounds")
                    .and_then(Value::as_array)
                    .ok_or(Error::InvalidMetadata)?;
                if sounds.iter().any(|v| match v {
                    Value::String(name) => name.is_empty(),
                    Value::Object(sound) => {
                        sound
                            .get("name")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                            || sound.get("type").is_some_and(|kind| {
                                !matches!(kind.as_str(), Some("sound" | "event"))
                            })
                    }
                    _ => true,
                }) {
                    return Err(Error::InvalidMetadata);
                }
                if event.get("replace").and_then(Value::as_bool) == Some(true)
                    || !result.contains_key(name)
                {
                    result.insert(name.clone(), Value::Object(event.clone()));
                } else {
                    // Minecraft retains the existing event/subtitle on append.
                    // A later explicit replacement resets both its list and subtitle.
                    let existing = result
                        .get_mut(name)
                        .and_then(Value::as_object_mut)
                        .ok_or(Error::InvalidMetadata)?;
                    existing
                        .get_mut("sounds")
                        .and_then(Value::as_array_mut)
                        .ok_or(Error::InvalidMetadata)?
                        .extend(sounds.clone());
                }
            }
            Ok(Value::Object(result))
        }
        Resolution::HigherPriority => Ok(next),
    }
}
