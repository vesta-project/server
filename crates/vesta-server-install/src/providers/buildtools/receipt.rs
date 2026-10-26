//! Reviewed build input and locally generated output are separate evidence.
use super::*;
use crate::{ArtifactRecord, InstallPlan, LaunchRecipe, Receipt, install as publication};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltReceipt {
    pub schema: u32,
    pub install_id: String,
    pub plan_hash: String,
    pub software: Software,
    pub version: String,
    pub build: u32,
    pub sources: SourceRefs,
    pub source_url: String,
    pub source_sha256: String,
    pub selected_java_major: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_sha256: Option<String>,
    pub input: ArtifactRecord,
    pub output: GeneratedFile,
    pub java_requirement: JavaRequirement,
    pub launch: LaunchRecipe,
    pub warnings: Vec<Warning>,
    pub completed_at: u64,
}
impl BuiltReceipt {
    pub(crate) fn into_receipt(self) -> Result<Receipt> {
        if self.schema != 4
            || !matches!(self.software, Software::Bukkit | Software::Spigot)
            || self.output.path != self.software.jar_path()
            || self.output.size == 0
            || !hex(&self.output.sha256, 64)
            || self
                .reference_sha256
                .as_ref()
                .is_some_and(|hash| hash != &self.output.sha256)
            || !hex(&self.source_sha256, 64)
            || self.source_url != format!("{INDEX}{}.json", self.version)
            || !(8..=99).contains(&self.selected_java_major)
            || [
                &self.sources.build_data,
                &self.sources.bukkit,
                &self.sources.craft_bukkit,
                &self.sources.spigot,
            ]
            .iter()
            .any(|id| !hex(id, 40))
            || self.input.path != "build/BuildTools.jar"
            || !receipt_tool_url(&self.input.url)
            || self.input.family != SourceFamily::SpigotBuildTools
            || self.input.published_digest.algorithm != DigestAlgorithm::Sha256
            || !hex(self.input.published_digest.hex(), 64)
            || self.input.observed_sha256 != self.input.published_digest.hex()
            || self.input.actual_size == 0
            || self.input.actual_size > 64 * 1024 * 1024
            || self.input.published_size != self.input.actual_size
        {
            return Err(InstallError::ReceiptInvalid);
        }
        Ok(Receipt {
            schema: 4,
            install_id: self.install_id.clone(),
            plan_hash: self.plan_hash.clone(),
            software: self.software,
            version: self.version.clone(),
            build: Some(self.build),
            published_digest: self.input.published_digest.clone(),
            published_size: Some(self.input.published_size),
            observed_sha256: self.input.observed_sha256.clone(),
            actual_size: self.input.actual_size,
            java_requirement: self.java_requirement.clone(),
            launch: self.launch.clone(),
            eula: "not_accepted".into(),
            warnings: self.warnings.clone(),
            completed_at: self.completed_at,
            bundle: None,
            processed: None,
            built: Some(self),
        })
    }
}
fn receipt_tool_url(raw: &str) -> bool {
    raw.strip_prefix("https://hub.spigotmc.org/jenkins/job/BuildTools/")
        .and_then(|s| s.strip_suffix("/artifact/target/BuildTools.jar"))
        .is_some_and(|id| {
            id.parse::<u32>()
                .is_ok_and(|n| n > 0 && n.to_string() == id)
        })
}
pub(crate) async fn verify(value: &InstallPlan) -> Result<Receipt> {
    if value.schema != 4 || value.processed.is_some() || value.bundle.is_some() {
        return Err(InstallError::ReceiptInvalid);
    }
    let recipe = value
        .source_build
        .as_ref()
        .ok_or(InstallError::ReceiptInvalid)?;
    let path = value.destination().join("control/receipt.json");
    publication::plain_file(&path)?;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    if bytes.len() > 65_536 {
        return Err(InstallError::ReceiptInvalid);
    }
    let receipt: BuiltReceipt =
        serde_json::from_slice(&bytes).map_err(|_| InstallError::ReceiptInvalid)?;
    let pin = value
        .artifact
        .as_ref()
        .ok_or(InstallError::ReceiptInvalid)?;
    let source = value.metadata.first().ok_or(InstallError::ReceiptInvalid)?;
    if receipt.plan_hash != value.hash()
        || receipt.install_id != value.install_id
        || receipt.software != value.software
        || receipt.version != value.version
        || Some(receipt.build) != value.build
        || receipt.sources != recipe.refs
        || receipt.source_url != source.url
        || receipt.source_sha256 != source.observed_sha256
        || receipt.selected_java_major != value.runtime.major
        || receipt.reference_sha256 != recipe.reference_hash
        || receipt.java_requirement != value.required_java
        || receipt.launch != value.launch_recipe()
        || receipt.warnings != value.warnings
        || receipt.completed_at == 0
        || receipt.completed_at >= value.expires
        || receipt.completed_at > crate::now()?
        || receipt.output.size > value.limits.artifact_bytes
        || recipe
            .reference_hash
            .as_ref()
            .is_some_and(|expected| expected != &receipt.output.sha256)
        || receipt.input.actual_size > value.limits.artifact_bytes
        || receipt.input.url != recipe.tool.url
        || receipt.input.published_digest != recipe.tool.digest
        || recipe.tool.size != Some(receipt.input.actual_size)
        || pin.url != recipe.tool.url
        || pin.digest != recipe.tool.digest
        || pin.size != recipe.tool.size
    {
        return Err(InstallError::ReceiptInvalid);
    }
    let receipt = receipt.into_receipt()?;
    let output = &receipt
        .built
        .as_ref()
        .ok_or(InstallError::ReceiptInvalid)?
        .output;
    let (size, hash) = crate::providers::forge::digest(
        &value.destination().join(&output.path),
        value.limits.artifact_bytes,
    )
    .await?;
    if size != output.size || hash != output.sha256 {
        return Err(InstallError::ArtifactChanged);
    }
    Ok(receipt)
}
