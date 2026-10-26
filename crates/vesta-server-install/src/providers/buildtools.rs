//! Shared first-party BuildTools discovery for CraftBukkit and Spigot.
//! Catalogue availability is not permission to execute a source build.
use super::*;

const INDEX: &str = "https://hub.spigotmc.org/versions/";

#[derive(Clone, Copy)]
enum Target {
    CraftBukkit,
    Spigot,
}
impl Target {
    fn from_software(software: Software) -> Result<Self> {
        match software {
            Software::Bukkit => Ok(Self::CraftBukkit),
            Software::Spigot => Ok(Self::Spigot),
            _ => Err(InstallError::Unavailable),
        }
    }
    fn expected_hash(self, hashes: &Hashes) -> Option<&str> {
        match self {
            Self::CraftBukkit => hashes.craft_bukkit.as_deref(),
            Self::Spigot => hashes.spigot.as_deref(),
        }
    }
}

#[derive(Deserialize)]
struct Refs {
    #[serde(rename = "BuildData")]
    build_data: String,
    #[serde(rename = "Bukkit")]
    bukkit: String,
    #[serde(rename = "CraftBukkit")]
    craft_bukkit: String,
    #[serde(rename = "Spigot")]
    spigot: String,
}
#[derive(Default, Deserialize)]
struct Hashes {
    #[serde(rename = "CraftBukkit")]
    craft_bukkit: Option<String>,
    #[serde(rename = "Spigot")]
    spigot: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Version {
    name: String,
    refs: Refs,
    tools_version: u32,
    #[serde(default)]
    hashes: Hashes,
    java_versions: Option<Vec<u32>>,
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn version(bytes: &[u8]) -> Result<Version> {
    let value: Version = parse(bytes)?;
    if value
        .name
        .parse::<u32>()
        .ok()
        .is_none_or(|build| build == 0 || build.to_string() != value.name)
        || value.tools_version == 0
        || [
            &value.refs.build_data,
            &value.refs.bukkit,
            &value.refs.craft_bukkit,
            &value.refs.spigot,
        ]
        .iter()
        .any(|id| !hex(id, 40))
        || [
            value.hashes.craft_bukkit.as_deref(),
            value.hashes.spigot.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|hash| !hex(hash, 64))
        || value.java_versions.as_ref().is_some_and(|range| {
            range.len() != 2 || range[0] < 51 || range[1] > 100 || range[0] > range[1]
        })
    {
        return Err(InstallError::InvalidMetadata);
    }
    Ok(value)
}

pub(super) async fn versions(
    http: &Transport,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<String>> {
    let response = http
        .family_metadata(INDEX, SourceFamily::SpigotVersions, None, job, reporter)
        .await?;
    let text = std::str::from_utf8(&response.data).map_err(|_| InstallError::InvalidMetadata)?;
    let mut versions = HashSet::new();
    for link in text.split("href=\"").skip(1) {
        let Some((file, _)) = link.split_once('"') else {
            return Err(InstallError::InvalidMetadata);
        };
        let Some(id) = file.strip_suffix(".json") else {
            continue;
        };
        if !identifier(id) || super::version_order(id).is_none() {
            continue;
        }
        if versions.len() >= job.limits.max_entries {
            return Err(InstallError::LimitExceeded);
        }
        if !versions.insert(id.to_owned()) {
            return Err(InstallError::InvalidMetadata);
        }
    }
    if versions.is_empty() {
        return Err(InstallError::Unavailable);
    }
    let mut versions: Vec<_> = versions.into_iter().collect();
    versions.sort_by_cached_key(|version| {
        std::cmp::Reverse(super::version_order(version).unwrap_or_default())
    });
    Ok(versions)
}

pub(super) async fn releases(
    http: &Transport,
    software: Software,
    minecraft: &str,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<super::releases::SoftwareRelease>> {
    let target = Target::from_software(software)?;
    if !identifier(minecraft) || super::version_order(minecraft).is_none() {
        return Err(InstallError::InvalidInput);
    }
    let response = http
        .family_metadata(
            &format!("{INDEX}{minecraft}.json"),
            SourceFamily::SpigotVersions,
            None,
            job,
            reporter,
        )
        .await?;
    let value = version(&response.data)?;
    // Old metadata can describe source builds without an independently published
    // output digest. Do not present that target as a verifiable build choice.
    if target.expected_hash(&value.hashes).is_none() {
        return Ok(Vec::new());
    }
    Ok(vec![super::releases::SoftwareRelease {
        id: value.name.clone(),
        build: Some(
            value
                .name
                .parse()
                .map_err(|_| InstallError::InvalidMetadata)?,
        ),
        loader_version: None,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn metadata() -> serde_json::Value {
        json!({"name":"4534","refs":{"BuildData":"a".repeat(40),"Bukkit":"b".repeat(40),"CraftBukkit":"c".repeat(40),"Spigot":"d".repeat(40)},"toolsVersion":181,"javaVersions":[65,69],"hashes":{"CraftBukkit":"e".repeat(64),"Spigot":"f".repeat(64)}})
    }
    #[test]
    fn targets_share_strict_provenance_but_keep_their_output_hashes_separate() {
        let value = version(&serde_json::to_vec(&metadata()).unwrap()).unwrap();
        assert_eq!(
            Target::CraftBukkit.expected_hash(&value.hashes),
            Some("e".repeat(64).as_str())
        );
        assert_eq!(
            Target::Spigot.expected_hash(&value.hashes),
            Some("f".repeat(64).as_str())
        );
        for field in ["BuildData", "Bukkit", "CraftBukkit", "Spigot"] {
            let mut invalid = metadata();
            invalid["refs"][field] = json!("master");
            assert!(version(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        let mut old = metadata();
        old.as_object_mut().unwrap().remove("hashes");
        let old = version(&serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(Target::Spigot.expected_hash(&old.hashes).is_none());
    }
}
