use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{fmt, path::PathBuf, time::Duration};

pub type Result<T> = std::result::Result<T, InstallError>;

/// Fixed, input-free failures. No transport/OS cause or rejected value is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error("server_install.invalid_input")]
    InvalidInput,
    #[error("server_install.unavailable")]
    Unavailable,
    #[error("server_install.invalid_metadata")]
    InvalidMetadata,
    #[error("server_install.source_refused")]
    SourceRefused,
    #[error("server_install.limit_exceeded")]
    LimitExceeded,
    #[error("server_install.transport_failed")]
    TransportFailed,
    #[error("server_install.artifact_changed")]
    ArtifactChanged,
    #[error("server_install.integrity_conflict")]
    IntegrityConflict,
    #[error("server_install.deadline")]
    Deadline,
    #[error("server_install.cancelled")]
    Cancelled,
    #[error("server_install.approval_required")]
    ApprovalRequired,
    #[error("server_install.approval_mismatch")]
    ApprovalMismatch,
    #[error("server_install.expired")]
    Expired,
    #[error("server_install.runtime_unknown")]
    RuntimeUnknown,
    #[error("server_install.runtime_incompatible")]
    RuntimeIncompatible,
    #[error("server_install.runtime_failed")]
    RuntimeFailed,
    #[error("server_install.path_refused")]
    PathRefused,
    #[error("server_install.destination_exists")]
    DestinationExists,
    #[error("server_install.storage_unknown")]
    StorageUnknown,
    #[error("server_install.storage_low")]
    StorageLow,
    #[error("server_install.busy")]
    Busy,
    #[error("server_install.publish_failed")]
    PublishFailed,
    #[error("server_install.receipt_invalid")]
    ReceiptInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Software {
    Vanilla,
    Paper,
    Folia,
    Purpur,
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}
impl Software {
    pub const ALL: [Self; 8] = [
        Self::Vanilla,
        Self::Paper,
        Self::Folia,
        Self::Purpur,
        Self::Fabric,
        Self::Quilt,
        Self::Forge,
        Self::NeoForge,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::Vanilla => "vanilla",
            Self::Paper => "paper",
            Self::Folia => "folia",
            Self::Purpur => "purpur",
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }
    pub fn descriptor(self) -> SoftwareDescriptor {
        let (source, licence) = match self {
            Self::Vanilla => (
                "https://www.minecraft.net/download/server",
                "https://www.minecraft.net/usage-guidelines",
            ),
            Self::Paper => (
                "https://papermc.io/software/paper",
                "https://github.com/PaperMC/Paper/blob/main/LICENSE.md",
            ),
            Self::Folia => (
                "https://papermc.io/software/folia",
                "https://github.com/PaperMC/Folia/blob/master/LICENSE.md",
            ),
            Self::Purpur => (
                "https://purpurmc.org",
                "https://github.com/PurpurMC/Purpur/blob/ver/1.21.11/LICENSE",
            ),
            Self::Fabric => (
                "https://fabricmc.net",
                "https://github.com/FabricMC/fabric-loader/blob/master/LICENSE",
            ),
            Self::Quilt => (
                "https://quiltmc.org",
                "https://github.com/QuiltMC/quilt-loader/blob/develop/LICENSE",
            ),
            Self::Forge => (
                "https://minecraftforge.net",
                "https://github.com/MinecraftForge/MinecraftForge/blob/master/LICENSE.txt",
            ),
            Self::NeoForge => (
                "https://neoforged.net",
                "https://github.com/neoforged/NeoForge/blob/main/LICENSE.txt",
            ),
        };
        SoftwareDescriptor {
            software: self,
            automatic_backend: self.automatic(),
            unavailable_reason: if self.automatic() {
                None
            } else {
                Some("provider_not_implemented")
            },
            source,
            licence,
            eula: "https://www.minecraft.net/eula",
            caution: if self == Self::Folia {
                Some("region_threading_compatibility")
            } else {
                None
            },
        }
    }
    pub(crate) fn automatic(self) -> bool {
        matches!(
            self,
            Self::Vanilla
                | Self::Paper
                | Self::Folia
                | Self::Purpur
                | Self::Fabric
                | Self::Quilt
                | Self::Forge
                | Self::NeoForge
        )
    }
    pub(crate) fn jar_path(self) -> String {
        format!("software/{}.jar", self.id())
    }
}

/// Review links, not a certification of licensing, redistribution or compatibility.
#[derive(Debug, Clone, Serialize)]
pub struct SoftwareDescriptor {
    pub software: Software,
    pub automatic_backend: bool,
    pub unavailable_reason: Option<&'static str>,
    pub source: &'static str,
    pub licence: &'static str,
    pub eula: &'static str,
    pub caution: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestAlgorithm {
    Sha256,
    Sha1,
    Md5,
}
impl DigestAlgorithm {
    pub fn legacy(self) -> bool {
        self != Self::Sha256
    }
    fn chars(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha1 => 40,
            Self::Md5 => 32,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedDigest {
    pub(crate) algorithm: DigestAlgorithm,
    pub(crate) hex: String,
}
impl PublishedDigest {
    pub(crate) fn new(algorithm: DigestAlgorithm, hex: String) -> Result<Self> {
        if hex.len() != algorithm.chars() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InstallError::InvalidMetadata);
        }
        Ok(Self {
            algorithm,
            hex: hex.to_ascii_lowercase(),
        })
    }
    pub fn algorithm(&self) -> DigestAlgorithm {
        self.algorithm
    }
    pub fn hex(&self) -> &str {
        &self.hex
    }
    pub(crate) fn validate(&self) -> Result<()> {
        Self::new(self.algorithm, self.hex.clone()).map(|_| ())
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ArtifactPin {
    pub url: String,
    pub digest: PublishedDigest,
    pub size: Option<u64>,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MetadataRevision {
    pub url: String,
    pub observed_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct JavaRequirement {
    pub major: u32,
    pub evidence: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Warning {
    LegacyChecksum,
    UnsupportedUpstream,
    ExperimentalBuild,
    RegionThreading,
    FirstRunEffects,
    ProxyResolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Limits {
    pub metadata_bytes: u64,
    pub artifact_bytes: u64,
    pub total_bytes: u64,
    pub reserve_bytes: u64,
    pub max_entries: usize,
    pub attempts: u8,
    pub redirects: u8,
    pub connect_ms: u64,
    pub idle_ms: u64,
    pub total_ms: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            metadata_bytes: 16 * 1024 * 1024,
            artifact_bytes: 1024 * 1024 * 1024,
            total_bytes: 4 * 1024 * 1024 * 1024,
            reserve_bytes: 512 * 1024 * 1024,
            max_entries: 10_000,
            attempts: 3,
            redirects: 3,
            connect_ms: 15_000,
            idle_ms: 30_000,
            total_ms: 600_000,
        }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<Self> {
        let d = Self::default();
        if self.metadata_bytes == 0
            || self.metadata_bytes > d.metadata_bytes
            || self.artifact_bytes == 0
            || self.artifact_bytes > d.artifact_bytes
            || self.total_bytes == 0
            || self.total_bytes > d.total_bytes
            || self.reserve_bytes < d.reserve_bytes
            || self.max_entries == 0
            || self.max_entries > d.max_entries
            || self.attempts == 0
            || self.attempts > d.attempts
            || self.redirects > d.redirects
            || self.connect_ms == 0
            || self.connect_ms > d.connect_ms
            || self.idle_ms == 0
            || self.idle_ms > d.idle_ms
            || self.total_ms == 0
            || self.total_ms > d.total_ms
        {
            return Err(InstallError::InvalidInput);
        }
        Ok(self)
    }
    pub(crate) fn duration(self) -> Duration {
        Duration::from_millis(self.total_ms)
    }
}

#[derive(Clone, Serialize)]
pub struct InstallPlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) processed: Option<super::providers::forge::ProcessorPlan>,
    pub(crate) schema: u32,
    pub(crate) install_id: String,
    pub(crate) software: Software,
    pub(crate) version: String,
    pub(crate) build: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) artifact: Option<ArtifactPin>,
    pub(crate) runtime: super::runtime::RuntimeBinding,
    pub(crate) required_java: JavaRequirement,
    pub(crate) base: PathBuf,
    pub(crate) forbidden: Vec<PathBuf>,
    pub(crate) expires: u64,
    pub(crate) warnings: Vec<Warning>,
    pub(crate) metadata: Vec<MetadataRevision>,
    pub(crate) transferred: u64,
    pub(crate) limits: Limits,
    pub(crate) connection_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bundle: Option<DownloadBundle>,
}
impl fmt::Debug for InstallPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallPlan")
            .field("software", &self.software)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}
impl InstallPlan {
    pub fn retained_metadata_bytes(&self) -> usize {
        self.processed
            .as_ref()
            .map(|p| p.embedded.values().map(Vec::len).sum())
            .unwrap_or(0)
    }
    pub fn runtime_compatible(&self) -> bool {
        // Historical loader/tool chains are certified per Java major, not for
        // every newer Java. Other providers retain their minimum-major policy.
        if self.processed.is_some() {
            self.runtime.major == self.required_java.major
        } else {
            self.runtime.major >= self.required_java.major
        }
    }
    pub fn selected_java_major(&self) -> u32 {
        self.runtime.major
    }
    fn primary_pin(&self) -> &ArtifactPin {
        if let Some(p) = &self.processed {
            return &p.artifacts[0].pin;
        }
        self.artifact.as_ref().unwrap_or_else(|| {
            &self
                .bundle
                .as_ref()
                .expect("backend validated bundle")
                .artifacts[0]
                .pin
        })
    }
    /// Ordered review evidence; schema 2 begins with the Vanilla game JAR.
    pub fn artifacts(&self) -> Vec<ArtifactReview<'_>> {
        if let Some(plan) = &self.processed {
            return plan
                .artifacts
                .iter()
                .chain(plan.mappings.iter())
                .map(|a| ArtifactReview {
                    destination: a.destination.clone(),
                    digest: &a.pin.digest,
                    size: a.pin.size,
                    url: &a.pin.url,
                    family: Some(a.family),
                })
                .collect();
        }
        if let Some(bundle) = &self.bundle {
            bundle
                .artifacts
                .iter()
                .map(|a| ArtifactReview {
                    destination: a.destination.clone(),
                    digest: &a.pin.digest,
                    size: a.pin.size,
                    url: &a.pin.url,
                    family: Some(a.family),
                })
                .collect()
        } else {
            let a = self.primary_pin();
            vec![ArtifactReview {
                destination: self.software.jar_path(),
                digest: &a.digest,
                size: a.size,
                url: &a.url,
                family: None,
            }]
        }
    }
    pub fn software(&self) -> Software {
        self.software
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn build(&self) -> Option<u32> {
        self.build
    }
    pub fn loader_version(&self) -> Option<&str> {
        self.processed
            .as_ref()
            .map(|p| p.loader_version.as_str())
            .or_else(|| {
                self.bundle
                    .as_ref()
                    .map(|bundle| bundle.loader_version.as_str())
            })
    }
    pub fn digest(&self) -> &PublishedDigest {
        &self.primary_pin().digest
    }
    pub fn published_size(&self) -> Option<u64> {
        self.primary_pin().size
    }
    pub fn java_requirement(&self) -> &JavaRequirement {
        &self.required_java
    }
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
    pub fn destination(&self) -> PathBuf {
        self.base.join("instances").join(&self.install_id)
    }
    pub fn expires_at(&self) -> u64 {
        self.expires
    }
    /// Fixed struct order, no maps or floats, with all safety fields and expiry included.
    pub fn hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("validated UTF-8 plan is serializable");
        let mut h = Sha256::new();
        if self.schema == 3 {
            h.update(b"vesta.server-install.plan.v3\0");
            h.update(bytes);
            return format!("{:x}", h.finalize());
        }
        // Schema 1 keeps its original domain so existing approvals/receipts stay valid.
        h.update(if self.schema == 1 {
            b"vesta.server-install.plan.v1\0"
        } else {
            b"vesta.server-install.plan.v2\0"
        });
        h.update(bytes);
        format!("{:x}", h.finalize())
    }
    pub fn launch_recipe(&self) -> LaunchRecipe {
        if self.processed.is_some() {
            return LaunchRecipe {
                kind: "java-argfile".into(),
                java: self.runtime.path.clone(),
                working_directory: self.destination().join("game"),
                artifact: None,
                argfile: Some(self.destination().join("software/launch.args")),
                args: Vec::new(),
                stop_mode: "console-stop".into(),
            };
        }
        LaunchRecipe {
            kind: if self.schema == 2 {
                "java-argfile"
            } else {
                "java-jar"
            }
            .into(),
            java: self.runtime.path.clone(),
            working_directory: self.destination().join("game"),
            artifact: if self.schema == 2 {
                None
            } else {
                Some(self.destination().join(self.software.jar_path()))
            },
            argfile: if self.schema == 2 {
                Some(self.destination().join("software/launch.args"))
            } else {
                None
            },
            args: if self.schema == 2 {
                Vec::new()
            } else {
                vec!["nogui".into()]
            },
            stop_mode: "console-stop".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaunchRecipe {
    pub kind: String,
    pub java: PathBuf,
    pub working_directory: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<PathBuf>,
    pub args: Vec<String>,
    pub stop_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argfile: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Acknowledgments {
    pub legacy_checksum: bool,
    pub warnings: bool,
}
/// Single-use local review assertion. Not authentication, OS authorization or an admin grant.
#[derive(Debug)]
pub struct Approval {
    pub(crate) hash: String,
}
impl Approval {
    pub fn review(plan: &InstallPlan, acknowledgments: Acknowledgments) -> Result<Self> {
        if !plan.runtime_compatible() {
            return Err(InstallError::RuntimeIncompatible);
        }
        if super::now()? >= plan.expires {
            return Err(InstallError::Expired);
        }
        if (plan.artifacts().iter().any(|a| a.digest.algorithm.legacy())
            && !acknowledgments.legacy_checksum)
            || (!plan.warnings.is_empty() && !acknowledgments.warnings)
        {
            return Err(InstallError::ApprovalRequired);
        }
        Ok(Self { hash: plan.hash() })
    }
}

pub(crate) fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s != "."
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
pub(crate) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Compiled origins/path families, never supplied by an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFamily {
    ForgeMaven,
    NeoForgeMaven,
    MinecraftLibraries,
    ModrinthNeoForge,
    MojangObjects,
    ModrinthFabric,
    ModrinthQuilt,
    FabricMeta,
    FabricMaven,
    QuiltMeta,
    QuiltMaven,
}
#[derive(Debug, Clone, Serialize)]
pub struct ArtifactReview<'a> {
    pub destination: String,
    pub digest: &'a PublishedDigest,
    pub size: Option<u64>,
    pub url: &'a str,
    pub family: Option<SourceFamily>,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct BundleArtifact {
    pub destination: String,
    #[serde(flatten)]
    pub pin: ArtifactPin,
    pub family: SourceFamily,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DownloadBundle {
    pub loader_version: String,
    pub artifacts: Vec<BundleArtifact>,
    pub classpath: Vec<usize>,
    pub generated: GeneratedFile,
    pub classpath_separator: String,
}
pub(crate) fn loader_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && !s.ends_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
}

impl<'de> Deserialize<'de> for LaunchRecipe {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: String,
            java: PathBuf,
            working_directory: PathBuf,
            artifact: Option<PathBuf>,
            #[serde(deserialize_with = "decode_args")]
            args: Vec<String>,
            stop_mode: String,
            argfile: Option<PathBuf>,
        }
        let w = Wire::deserialize(d)?;
        if !matches!(
            (w.kind.as_str(), w.artifact.is_some(), w.argfile.is_some()),
            ("java-jar", true, false) | ("java-argfile", false, true)
        ) || (w.kind == "java-jar" && w.args != ["nogui"])
            || (w.kind == "java-argfile" && !w.args.is_empty())
        {
            return Err(serde::de::Error::custom("invalid launch recipe"));
        }
        Ok(Self {
            kind: w.kind,
            java: w.java,
            working_directory: w.working_directory,
            artifact: w.artifact,
            args: w.args,
            stop_mode: w.stop_mode,
            argfile: w.argfile,
        })
    }
}

fn bounded_vec<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
    d: D,
) -> std::result::Result<Vec<T>, D::Error> {
    struct V<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for V<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("bounded receipt collection")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(value) = seq.next_element()? {
                if out.len() == N {
                    return Err(serde::de::Error::custom("collection limit"));
                }
                out.push(value);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(V::<T, N>(std::marker::PhantomData))
}
fn decode_args<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<String>, D::Error> {
    bounded_vec::<D, String, 1>(d)
}
pub(super) fn decode_warnings<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<Warning>, D::Error> {
    bounded_vec::<D, Warning, 6>(d)
}
