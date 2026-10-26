//! Shared first-party BuildTools discovery for CraftBukkit and Spigot.
//! Catalogue availability is not permission to execute a source build.
use super::*;
use crate::runtime::RuntimeBinding;
use serde::Serialize;
use std::path::{Path, PathBuf};

mod install;
mod layout;
mod receipt;
pub(crate) use install::build;
pub use receipt::BuiltReceipt;
pub(crate) use receipt::output_warnings;
pub(crate) use receipt::verify;

// Audited upstream runner. Updating it requires reviewing its source and pin.
const TOOL_URL: &str =
    "https://hub.spigotmc.org/jenkins/job/BuildTools/201/artifact/target/BuildTools.jar";
const TOOL_HASH: &str = "81ed06f5ce5fb705b558bf9b3cc2c5c53cdd9742406c5338c452faa13a1e1c24";
const TOOL_BUILD: u32 = 201;
const TOOL_BYTES: u64 = 3_606_247;
const BUILD_SECONDS: u64 = 3600;

#[derive(Clone, Serialize)]
pub(crate) struct BuildPlan {
    refs: Refs,
    // Current plans require a published reference. Generated output and verified
    // build inputs still have separate receipt fields and trust roles.
    reference_hash: Option<String>,
    tool: ArtifactPin,
    pub(crate) java_min: u32,
    pub(crate) java_max: u32,
    git: Option<RuntimeBinding>,
    shell: Option<RuntimeBinding>,
    compiler: Option<RuntimeBinding>,
    decompiler_heap_mib: u32,
    reference_policy: &'static str,
    // Explicit source-build budget, separate from the network query deadline.
    seconds: u64,
}
impl BuildPlan {
    pub(crate) fn output_reference(&self) -> Option<String> {
        self.reference_hash.clone()
    }
    pub(crate) fn source_evidence(
        &self,
        plan: &InstallPlan,
    ) -> Result<(SourceRefs, String, String)> {
        let source = plan.metadata.first().ok_or(InstallError::InvalidMetadata)?;
        Ok((
            self.refs.clone(),
            source.url.clone(),
            source.observed_sha256.clone(),
        ))
    }
    pub(crate) fn bind_tools(&mut self, java: &RuntimeBinding, base: &Path) -> Result<()> {
        if base
            .to_str()
            .is_none_or(|s| s.contains(['\'', '#', '~', '(', ')', '!']))
        {
            return Err(InstallError::PathRefused);
        }
        let compiler = java
            .path
            .parent()
            .ok_or(InstallError::RuntimeUnknown)?
            .join(if cfg!(windows) { "javac.exe" } else { "javac" });
        self.compiler = Some(
            RuntimeBinding::snapshot(&compiler, java.major)
                .map_err(|_| InstallError::BuildToolsMissing)?,
        );
        self.git = Some(find_tool(if cfg!(windows) { "git.exe" } else { "git" })?);
        self.shell = Some(
            find_tool(if cfg!(windows) { "sh.exe" } else { "sh" }).or_else(|_| {
                if !cfg!(windows) {
                    return Err(InstallError::RuntimeUnknown);
                }
                // Git for Windows exposes git.exe on PATH, but not its POSIX tools.
                let root = self
                    .git
                    .as_ref()
                    .ok_or(InstallError::RuntimeUnknown)?
                    .path
                    .parent()
                    .and_then(Path::parent)
                    .ok_or(InstallError::RuntimeUnknown)?;
                RuntimeBinding::snapshot(&root.join("usr/bin/sh.exe"), 0)
            })?,
        );
        for tool in [&self.git, &self.shell, &self.compiler]
            .into_iter()
            .flatten()
        {
            if crate::install::overlap(&tool.path, base) {
                return Err(InstallError::PathRefused);
            }
        }
        Ok(())
    }
    pub(crate) fn tool_review(&self) -> ArtifactReview<'_> {
        ArtifactReview {
            destination: "build/BuildTools.jar".into(),
            digest: &self.tool.digest,
            size: self.tool.size,
            url: &self.tool.url,
            family: Some(SourceFamily::SpigotBuildTools),
        }
    }
}
fn find_tool(name: &str) -> Result<RuntimeBinding> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join(name);
        if let Ok(path) = dunce::canonicalize(candidate) {
            if let Ok(binding) = RuntimeBinding::snapshot(&path, 0) {
                return Ok(binding);
            }
        }
    }
    Err(InstallError::BuildToolsMissing)
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRefs {
    #[serde(rename = "BuildData")]
    pub build_data: String,
    #[serde(rename = "Bukkit")]
    pub bukkit: String,
    #[serde(rename = "CraftBukkit")]
    pub craft_bukkit: String,
    #[serde(rename = "Spigot")]
    pub spigot: String,
}
type Refs = SourceRefs;

pub(super) async fn resolve(
    http: &Transport,
    software: Software,
    selection: &Selection,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Resolved> {
    if http.uses_proxy() {
        // BuildTools downloads dependencies itself. Do not silently bypass the
        // operator's configured transport proxy with a different HTTP stack.
        return Err(InstallError::Unavailable);
    }
    if selection.loader_version.is_some() || selection.experimental {
        return Err(InstallError::InvalidInput);
    }
    let minecraft = selection
        .version
        .as_deref()
        .ok_or(InstallError::InvalidInput)?;
    if super::version_order(minecraft).is_none() || !identifier(minecraft) {
        return Err(InstallError::InvalidInput);
    }
    let url = format!("{INDEX}{minecraft}.json");
    let response = http
        .family_metadata(&url, SourceFamily::SpigotVersions, None, job, reporter)
        .await?;
    let value = version(&response.data)?;
    let target = Target::from_software(software)?;
    let reference_hash = Some(
        target
            .expected_hash(&value.hashes)
            .ok_or(InstallError::Unavailable)?
            .to_owned(),
    );
    let build = value
        .name
        .parse::<u32>()
        .map_err(|_| InstallError::InvalidMetadata)?;
    if selection.build.is_some_and(|selected| selected != build) {
        return Err(InstallError::InvalidInput);
    }
    if !(138..=TOOL_BUILD).contains(&value.tools_version) {
        return Err(InstallError::Unavailable);
    }
    let range = value.java_versions.ok_or(InstallError::RuntimeUnknown)?;
    let java_min = range[0] - 44;
    let java_max = range[1] - 44;
    Ok(Resolved {
        source_build: Some(BuildPlan {
            refs: value.refs,
            reference_hash,
            tool: ArtifactPin {
                url: TOOL_URL.into(),
                digest: PublishedDigest::new(DigestAlgorithm::Sha256, TOOL_HASH.into())?,
                size: Some(TOOL_BYTES),
            },
            java_min,
            java_max,
            git: None,
            shell: None,
            compiler: None,
            decompiler_heap_mib: if java_min >= 25 { 1024 } else { 512 },
            reference_policy: "warn_on_local_output_difference",
            seconds: BUILD_SECONDS,
        }),
        processed: None,
        bundle: None,
        artifact: Some(ArtifactPin {
            url: TOOL_URL.into(),
            digest: PublishedDigest::new(DigestAlgorithm::Sha256, TOOL_HASH.into())?,
            size: Some(TOOL_BYTES),
        }),
        version: minecraft.into(),
        build: Some(build),
        java: major(java_min, "spigot_buildtools_classfile_range")?,
        warnings: vec![Warning::SourceBuild, Warning::FirstRunEffects],
        metadata: vec![MetadataRevision {
            url,
            observed_sha256: response.observed_sha256,
        }],
    })
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
    // Offer only the reviewed bootstrap layout and an explicit Java range.
    if !(138..=TOOL_BUILD).contains(&value.tools_version) || value.java_versions.is_none() {
        return Ok(Vec::new());
    }
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
