//! Schema-two download-only publication, with receipt-last commit and compiled launch policy.
use super::{
    install::{self, InstallOutcome, Receipt},
    model::*,
    paths,
    transfer::{self, Job, Role},
};
use crate::progress::{ProgressReporter, SilentProgressReporter};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) const RECEIPT_BYTES: usize = 1024 * 1024;
const MAX_ARTIFACTS: usize = 129;
const ARGFILE_BYTES: usize = 128 * 1024;
pub(super) fn client_main(s: Software) -> Result<&'static str> {
    match s {
        Software::Fabric => Ok("net.fabricmc.loader.impl.launch.knot.KnotClient"),
        Software::Quilt => Ok("org.quiltmc.loader.impl.launch.knot.KnotClient"),
        _ => Err(InstallError::Unavailable),
    }
}
pub(super) fn server_main(s: Software) -> Result<&'static str> {
    match s {
        Software::Fabric => Ok("net.fabricmc.loader.impl.launch.knot.KnotServer"),
        Software::Quilt => Ok("org.quiltmc.loader.impl.launch.knot.KnotServer"),
        _ => Err(InstallError::Unavailable),
    }
}
/// All tokens are ASCII, contain no quoting/escaping characters, and end in LF.
pub(super) fn argfile(s: Software, b: &DownloadBundle) -> Result<Vec<u8>> {
    let main = server_main(s)?;
    if b.classpath_separator != if cfg!(windows) { ";" } else { ":" }
        || b.classpath.is_empty()
        || b.classpath.len() > 128
    {
        return Err(InstallError::InvalidInput);
    }
    let mut cp = Vec::new();
    for &index in &b.classpath {
        let a = b.artifacts.get(index).ok_or(InstallError::InvalidInput)?;
        paths::destination(&a.destination)?;
        if index == 0 || !a.destination.starts_with("software/libraries/") {
            return Err(InstallError::InvalidInput);
        }
        cp.push(format!("../{}", a.destination));
    }
    let property = if s == Software::Fabric {
        "fabric"
    } else {
        "loader"
    };
    let bytes = format!(
        "-D{}.gameJarPath=../software/vanilla.jar\n-cp\n{}\n{}\nnogui\n",
        property,
        cp.join(&b.classpath_separator),
        main
    )
    .into_bytes();
    if bytes.len() > ARGFILE_BYTES {
        return Err(InstallError::LimitExceeded);
    }
    Ok(bytes)
}
/// Revalidate every policy field even though plans can only be constructed by the backend.
pub(super) fn validate(
    s: Software,
    b: &DownloadBundle,
    limits: Limits,
) -> Result<(u64, BTreeSet<String>)> {
    server_main(s)?;
    if !loader_identifier(&b.loader_version)
        || b.artifacts.len() < 2
        || b.artifacts.len() > MAX_ARTIFACTS
        || b.artifacts.len() > limits.max_entries
        || b.classpath != (1..b.artifacts.len()).collect::<Vec<_>>()
        || b.generated.path != "software/launch.args"
    {
        return Err(InstallError::InvalidInput);
    }
    let mut total = 0u64;
    for (i, a) in b.artifacts.iter().enumerate() {
        paths::destination(&a.destination)?;
        a.pin.digest.validate()?;
        transfer::validate_family_url(&a.pin.url, a.family, Role::Artifact, false)?;
        let size = a.pin.size.ok_or(InstallError::InvalidInput)?;
        if size == 0 || size > limits.artifact_bytes {
            return Err(InstallError::LimitExceeded);
        }
        if i == 0 {
            if a.destination != "software/vanilla.jar"
                || a.family != SourceFamily::MojangObjects
                || a.pin.digest.algorithm != DigestAlgorithm::Sha1
            {
                return Err(InstallError::InvalidInput);
            }
            let u = url::Url::parse(&a.pin.url).map_err(|_| InstallError::SourceRefused)?;
            if u.path() != format!("/v1/objects/{}/server.jar", a.pin.digest.hex) {
                return Err(InstallError::SourceRefused);
            }
        } else {
            if a.pin.digest.algorithm != DigestAlgorithm::Sha256
                || !matches!(
                    (s, a.family),
                    (Software::Fabric, SourceFamily::FabricMaven)
                        | (
                            Software::Quilt,
                            SourceFamily::FabricMaven | SourceFamily::QuiltMaven
                        )
                )
            {
                return Err(InstallError::InvalidInput);
            }
            let u = url::Url::parse(&a.pin.url).map_err(|_| InstallError::SourceRefused)?;
            let maven = u
                .path()
                .strip_prefix(if a.family == SourceFamily::QuiltMaven {
                    "/repository/release/"
                } else {
                    "/"
                })
                .ok_or(InstallError::SourceRefused)?;
            if a.destination != format!("software/libraries/{}", maven) || !maven_destination(maven)
            {
                return Err(InstallError::PathRefused);
            }
        }
        total = total.checked_add(size).ok_or(InstallError::LimitExceeded)?;
    }
    let bytes = argfile(s, b)?;
    if b.generated.size != bytes.len() as u64 || b.generated.sha256 != sha256(&bytes) {
        return Err(InstallError::InvalidInput);
    }
    total = total
        .checked_add(b.generated.size)
        .ok_or(InstallError::LimitExceeded)?;
    if total > limits.total_bytes {
        return Err(InstallError::LimitExceeded);
    }
    paths::destination(&b.generated.path)?;
    let directories = paths::declarations(
        b.artifacts
            .iter()
            .map(|a| a.destination.as_str())
            .chain(std::iter::once(b.generated.path.as_str())),
        limits.max_entries,
    )?;
    Ok((total, directories))
}
fn maven_destination(path: &str) -> bool {
    let p: Vec<_> = path.split('/').collect();
    if p.len() < 4 {
        return false;
    }
    let n = p.len();
    let coordinate = format!("{}:{}:{}", p[..n - 3].join("."), p[n - 3], p[n - 2]);
    super::providers::loaders::maven_path(&coordinate).is_ok_and(|s| s == path)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    pub path: String,
    pub url: String,
    pub family: SourceFamily,
    pub published_size: u64,
    pub published_digest: PublishedDigest,
    pub actual_size: u64,
    pub observed_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleReceipt {
    pub schema: u32,
    pub install_id: String,
    pub plan_hash: String,
    pub software: Software,
    pub version: String,
    pub loader_version: String,
    #[serde(deserialize_with = "records")]
    pub artifacts: Vec<ArtifactRecord>,
    pub generated: GeneratedFile,
    pub java_requirement: JavaRequirement,
    pub launch: LaunchRecipe,
    pub eula: String,
    #[serde(deserialize_with = "decode_warnings")]
    pub warnings: Vec<Warning>,
    pub completed_at: u64,
}
fn records<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<ArtifactRecord>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<ArtifactRecord>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded artifact records")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(a) = seq.next_element()? {
                if out.len() == MAX_ARTIFACTS {
                    return Err(serde::de::Error::custom("record limit"));
                }
                out.push(a);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(V)
}
impl BundleReceipt {
    /// Preserve the existing public receipt accessors without labelling generated bytes upstream.
    pub(super) fn into_receipt(self) -> Result<Receipt> {
        if self.schema != 2
            || !matches!(self.software, Software::Fabric | Software::Quilt)
            || self.artifacts.len() < 2
            || self.artifacts.len() > MAX_ARTIFACTS
        {
            return Err(InstallError::ReceiptInvalid);
        }
        let vanilla = self.artifacts.first().ok_or(InstallError::ReceiptInvalid)?;
        Ok(Receipt {
            schema: self.schema,
            install_id: self.install_id.clone(),
            plan_hash: self.plan_hash.clone(),
            software: self.software,
            version: self.version.clone(),
            build: None,
            published_digest: vanilla.published_digest.clone(),
            published_size: Some(vanilla.published_size),
            observed_sha256: vanilla.observed_sha256.clone(),
            actual_size: vanilla.actual_size,
            java_requirement: self.java_requirement.clone(),
            launch: self.launch.clone(),
            eula: self.eula.clone(),
            warnings: self.warnings.clone(),
            completed_at: self.completed_at,
            bundle: Some(self),
        })
    }
}

struct Stage {
    directory: PathBuf,
    files: Vec<PathBuf>,
}
impl Stage {
    fn create(base: &Path) -> Result<Self> {
        let root = base.join(".staging");
        install::child_directory(&root)?;
        let directory = root.join(format!("job_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&directory).map_err(|_| InstallError::PublishFailed)?;
        install::guard(&directory)?;
        Ok(Self {
            directory,
            files: Vec::new(),
        })
    }
    async fn write(
        &mut self,
        name: &str,
        bytes: Option<&[u8]>,
        job: &Job,
        reporter: &dyn ProgressReporter,
    ) -> Result<tokio::fs::File> {
        job.ready(reporter).await?;
        install::guard(&self.directory)?;
        let path = self.directory.join(name);
        let file = transfer::controlled(
            async {
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .await
                    .map_err(|_| InstallError::PublishFailed)
            },
            job.deadline,
            reporter,
        )
        .await?;
        self.files.push(path);
        let mut file = file;
        if let Some(bytes) = bytes {
            transfer::controlled(
                async {
                    file.write_all(bytes)
                        .await
                        .map_err(|_| InstallError::PublishFailed)?;
                    file.sync_all()
                        .await
                        .map_err(|_| InstallError::PublishFailed)
                },
                job.deadline,
                reporter,
            )
            .await?;
        }
        Ok(file)
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if install::guard(&self.directory).is_err() {
            return;
        }
        for file in &self.files {
            if install::plain_file(file).is_ok() {
                let _ = std::fs::remove_file(file);
            }
        }
        let _ = std::fs::remove_dir(&self.directory);
    }
}
fn context(installer: &super::ServerInstaller, plan: &InstallPlan) -> Result<()> {
    if super::now()? >= plan.expires {
        return Err(InstallError::Expired);
    }
    installer.roots.recheck()?;
    plan.runtime.recheck()
}
/// Original pin plus independently observed SHA-256; opened/reopened native identity,
/// exact authorized hardlink count, size and mtime are checked at both ends.
async fn rehash(
    path: &Path,
    pin: &ArtifactPin,
    size: u64,
    observed: &str,
    links: u64,
) -> Result<()> {
    use super::execution_workspace::file_identity_with_links;
    install::plain_file(path)?;
    let opened = std::fs::File::open(path).map_err(|_| InstallError::ArtifactChanged)?;
    let id = file_identity_with_links(&opened, links)?;
    let before = opened
        .metadata()
        .map_err(|_| InstallError::ArtifactChanged)?;
    if before.len() != size || pin.size != Some(size) {
        return Err(InstallError::ArtifactChanged);
    }
    let mut file = tokio::fs::File::from_std(opened);
    let mut strong = Sha256::new();
    let mut legacy = sha1::Sha1::new();
    let mut weak = md5::Md5::new();
    let mut actual = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| InstallError::ArtifactChanged)?;
        if n == 0 {
            break;
        }
        actual = actual
            .checked_add(n as u64)
            .ok_or(InstallError::LimitExceeded)?;
        if actual > size {
            return Err(InstallError::ArtifactChanged);
        }
        strong.update(&buffer[..n]);
        legacy.update(&buffer[..n]);
        weak.update(&buffer[..n]);
    }
    let sha = format!("{:x}", strong.finalize());
    let digest = match pin.digest.algorithm {
        DigestAlgorithm::Sha256 => sha.clone(),
        DigestAlgorithm::Sha1 => format!("{:x}", legacy.finalize()),
        DigestAlgorithm::Md5 => format!("{:x}", weak.finalize()),
    };
    if actual != size || sha != observed || digest != pin.digest.hex {
        return Err(InstallError::ArtifactChanged);
    }
    let opened = file.into_std().await;
    install::plain_file(path)?;
    let current = std::fs::File::open(path).map_err(|_| InstallError::ArtifactChanged)?;
    for handle in [&opened, &current] {
        if file_identity_with_links(handle, links)? != id {
            return Err(InstallError::ArtifactChanged);
        }
        let after = handle
            .metadata()
            .map_err(|_| InstallError::ArtifactChanged)?;
        if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            return Err(InstallError::ArtifactChanged);
        }
    }
    Ok(())
}
fn generated_pin(b: &DownloadBundle) -> Result<ArtifactPin> {
    Ok(ArtifactPin {
        url: String::new(),
        digest: PublishedDigest::new(DigestAlgorithm::Sha256, b.generated.sha256.clone())?,
        size: Some(b.generated.size),
    })
}
fn check_scan(
    root: &Path,
    b: &DownloadBundle,
    dirs: &BTreeSet<String>,
    cap: usize,
    deadline: tokio::time::Instant,
    reporter: &dyn ProgressReporter,
) -> Result<()> {
    let files: HashSet<_> = b
        .artifacts
        .iter()
        .map(|a| a.destination.as_str())
        .chain(std::iter::once(b.generated.path.as_str()))
        .collect();
    let mut pending = vec![root.join("software")];
    let mut seen = 0usize;
    while let Some(dir) = pending.pop() {
        super::execution::check(deadline, reporter)?;
        install::guard(&dir)?;
        if !std::fs::symlink_metadata(&dir)
            .map_err(|_| InstallError::PathRefused)?
            .is_dir()
        {
            return Err(InstallError::PathRefused);
        }
        for entry in std::fs::read_dir(&dir).map_err(|_| InstallError::PathRefused)? {
            super::execution::check(deadline, reporter)?;
            seen = seen.checked_add(1).ok_or(InstallError::LimitExceeded)?;
            if seen > cap {
                return Err(InstallError::LimitExceeded);
            }
            let path = entry.map_err(|_| InstallError::PathRefused)?.path();
            install::guard(&path)?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| InstallError::PathRefused)?
                .to_str()
                .ok_or(InstallError::PathRefused)?
                .replace('\\', "/");
            let m = std::fs::symlink_metadata(&path).map_err(|_| InstallError::PathRefused)?;
            if m.is_dir() && dirs.contains(&relative) {
                pending.push(path);
            } else if m.is_file() && files.contains(relative.as_str()) {
                install::plain_file(&path)?;
            } else {
                return Err(InstallError::PathRefused);
            }
        }
    }
    Ok(())
}
pub(super) async fn execute(
    installer: &super::ServerInstaller,
    plan: &InstallPlan,
    reporter: &dyn ProgressReporter,
) -> Result<InstallOutcome> {
    if plan.artifact.is_some() || plan.build.is_some() {
        return Err(InstallError::InvalidInput);
    }
    let b = plan.bundle.as_ref().ok_or(InstallError::InvalidInput)?;
    let (bytes, dirs) = validate(plan.software, b, plan.limits)?;
    let _slot = installer.coordinator.slot()?;
    let (volume, free) = installer.space()?;
    let reservation = installer.coordinator.reserve(
        volume,
        free,
        bytes
            .checked_add(RECEIPT_BYTES as u64)
            .ok_or(InstallError::LimitExceeded)?,
        plan.limits.reserve_bytes,
    )?;
    let mut job = Job::new(plan.limits, plan.transferred);
    job.ready(reporter).await?;
    let mut stage = Stage::create(&plan.base)?;
    let mut records = Vec::new();
    let mut staged = Vec::new();
    reporter.start_step(
        "Downloading server software",
        Some(b.artifacts.len() as u32),
    );
    for (i, a) in b.artifacts.iter().enumerate() {
        let name = format!("artifact_{:03}.part", i);
        let path = stage.directory.join(&name);
        let mut file = stage.write(&name, None, &job, reporter).await?;
        let data = installer
            .http
            .family_artifact(&a.pin, a.family, &mut job, reporter, &mut file)
            .await?;
        drop(file);
        records.push(ArtifactRecord {
            path: a.destination.clone(),
            url: a.pin.url.clone(),
            family: a.family,
            published_size: a.pin.size.ok_or(InstallError::InvalidInput)?,
            published_digest: a.pin.digest.clone(),
            actual_size: data.size,
            observed_sha256: data.observed_sha256,
        });
        staged.push(path);
    }
    let args = argfile(plan.software, b)?;
    drop(
        stage
            .write("launch.part", Some(&args), &job, reporter)
            .await?,
    );
    let generated = stage.directory.join("launch.part");
    for ((a, record), path) in b.artifacts.iter().zip(&records).zip(&staged) {
        transfer::controlled(
            rehash(path, &a.pin, record.actual_size, &record.observed_sha256, 1),
            job.deadline,
            reporter,
        )
        .await?;
    }
    transfer::controlled(
        rehash(
            &generated,
            &generated_pin(b)?,
            b.generated.size,
            &b.generated.sha256,
            1,
        ),
        job.deadline,
        reporter,
    )
    .await?;
    context(installer, plan)?;
    let receipt = BundleReceipt {
        schema: 2,
        install_id: plan.install_id.clone(),
        plan_hash: plan.hash(),
        software: plan.software,
        version: plan.version.clone(),
        loader_version: b.loader_version.clone(),
        artifacts: records,
        generated: b.generated.clone(),
        java_requirement: plan.required_java.clone(),
        launch: plan.launch_recipe(),
        eula: "not_accepted".into(),
        warnings: plan.warnings.clone(),
        completed_at: super::now()?,
    };
    let data = serde_json::to_vec(&receipt).map_err(|_| InstallError::ReceiptInvalid)?;
    if data.len() > RECEIPT_BYTES {
        return Err(InstallError::LimitExceeded);
    }
    drop(
        stage
            .write("receipt.part", Some(&data), &job, reporter)
            .await?,
    );
    job.ready(reporter).await?;
    context(installer, plan)?;
    let (volume, free) = installer.space()?;
    reservation.recheck(&volume, free, plan.limits.reserve_bytes)?;
    let destination = plan.destination();
    install::child_directory(&plan.base.join("instances"))?;
    install::guard(&destination)?;
    std::fs::create_dir(&destination).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            InstallError::DestinationExists
        } else {
            InstallError::PublishFailed
        }
    })?;
    // From here onward any partial bundle is retained; published links are never removed.
    for dir in ["software", "game", "control"]
        .into_iter()
        .map(str::to_string)
        .chain(dirs.iter().filter(|d| d.as_str() != "software").cloned())
    {
        job.ready(reporter).await?;
        let path = destination.join(dir);
        install::guard(&path)?;
        std::fs::create_dir(&path).map_err(|_| InstallError::PublishFailed)?;
        install::guard(&path)?;
    }
    let artifacts = b.artifacts.iter().zip(&staged);
    #[cfg(test)]
    let artifacts = artifacts.enumerate();
    for artifact in artifacts {
        #[cfg(not(test))]
        let (a, source) = artifact;
        #[cfg(test)]
        let (index, (a, source)) = artifact;
        job.ready(reporter).await?;
        let target = destination.join(&a.destination);
        install::plain_file(source)?;
        install::guard(&target)?;
        std::fs::hard_link(source, &target).map_err(|_| InstallError::PublishFailed)?;
        #[cfg(test)]
        if installer.fail_after_artifact || installer.fail_after_bundle_link == Some(index) {
            return Err(InstallError::PublishFailed);
        }
    }
    job.ready(reporter).await?;
    let target = destination.join(&b.generated.path);
    install::guard(&target)?;
    install::plain_file(&generated)?;
    std::fs::hard_link(&generated, &target).map_err(|_| InstallError::PublishFailed)?;
    #[cfg(test)]
    if installer.fail_after_bundle_link == Some(b.artifacts.len()) {
        return Err(InstallError::PublishFailed);
    }
    // Verify the published tree, original pins and generated file immediately before commit.
    for (a, record) in b.artifacts.iter().zip(&receipt.artifacts) {
        job.ready(reporter).await?;
        transfer::controlled(
            rehash(
                &destination.join(&a.destination),
                &a.pin,
                record.actual_size,
                &record.observed_sha256,
                2,
            ),
            job.deadline,
            reporter,
        )
        .await?;
    }
    transfer::controlled(
        rehash(
            &target,
            &generated_pin(b)?,
            b.generated.size,
            &b.generated.sha256,
            2,
        ),
        job.deadline,
        reporter,
    )
    .await?;
    check_scan(
        &destination,
        b,
        &dirs,
        plan.limits.max_entries,
        job.deadline,
        reporter,
    )?;
    let receipt_path = stage.directory.join("receipt.part");
    transfer::controlled(
        rehash(
            &receipt_path,
            &ArtifactPin {
                url: String::new(),
                digest: PublishedDigest::new(DigestAlgorithm::Sha256, sha256(&data))?,
                size: Some(data.len() as u64),
            },
            data.len() as u64,
            &sha256(&data),
            1,
        ),
        job.deadline,
        reporter,
    )
    .await?;
    job.ready(reporter).await?;
    context(installer, plan)?;
    let (volume, free) = installer.space()?;
    reservation.recheck(&volume, free, plan.limits.reserve_bytes)?;
    let marker = destination.join("control/receipt.json");
    install::guard(&marker)?;
    std::fs::hard_link(&receipt_path, &marker).map_err(|_| InstallError::PublishFailed)?;
    // No cancellable checkpoint after commit.
    reporter.done(
        true,
        Some("Server software installed; EULA not accepted; server not started"),
    );
    Ok(InstallOutcome::Installed(receipt.into_receipt()?))
}
pub(super) async fn verify(plan: &InstallPlan, deadline: tokio::time::Instant) -> Result<Receipt> {
    let b = plan.bundle.as_ref().ok_or(InstallError::ReceiptInvalid)?;
    if plan.artifact.is_some() || plan.build.is_some() {
        return Err(InstallError::ReceiptInvalid);
    }
    let (_, dirs) = validate(plan.software, b, plan.limits).map_err(verification_error)?;
    let root = plan.destination();
    let marker = root.join("control/receipt.json");
    install::plain_file(&marker)?;
    let file = tokio::fs::File::open(&marker)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    let mut data = Vec::new();
    file.take((RECEIPT_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    if data.len() > RECEIPT_BYTES {
        return Err(InstallError::ReceiptInvalid);
    }
    let receipt: BundleReceipt =
        serde_json::from_slice(&data).map_err(|_| InstallError::ReceiptInvalid)?;
    if receipt.schema != 2
        || receipt.install_id != plan.install_id
        || receipt.plan_hash != plan.hash()
        || receipt.software != plan.software
        || receipt.version != plan.version
        || receipt.loader_version != b.loader_version
        || receipt.generated != b.generated
        || receipt.java_requirement != plan.required_java
        || receipt.launch != plan.launch_recipe()
        || receipt.eula != "not_accepted"
        || receipt.warnings != plan.warnings
        || receipt.completed_at == 0
        || receipt.completed_at >= plan.expires
        || receipt.completed_at > super::now()?
        || receipt.artifacts.len() != b.artifacts.len()
    {
        return Err(InstallError::ReceiptInvalid);
    }
    check_scan(
        &root,
        b,
        &dirs,
        plan.limits.max_entries,
        deadline,
        &SilentProgressReporter,
    )
    .map_err(verification_error)?;
    for (a, r) in b.artifacts.iter().zip(&receipt.artifacts) {
        if r.path != a.destination
            || r.url != a.pin.url
            || r.family != a.family
            || Some(r.published_size) != a.pin.size
            || r.published_digest != a.pin.digest
            || r.actual_size != r.published_size
            || r.observed_sha256.len() != 64
            || !r.observed_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(InstallError::ReceiptInvalid);
        }
        rehash(
            &root.join(&a.destination),
            &a.pin,
            r.actual_size,
            &r.observed_sha256,
            1,
        )
        .await
        .map_err(verification_error)?;
    }
    // validate() recomputes the argfile from compiled policy, not the receipt.
    rehash(
        &root.join(&b.generated.path),
        &generated_pin(b)?,
        b.generated.size,
        &b.generated.sha256,
        1,
    )
    .await
    .map_err(verification_error)?;
    check_scan(
        &root,
        b,
        &dirs,
        plan.limits.max_entries,
        deadline,
        &SilentProgressReporter,
    )
    .map_err(verification_error)?;
    receipt.into_receipt()
}

fn verification_error(error: InstallError) -> InstallError {
    if error == InstallError::Deadline {
        error
    } else {
        InstallError::ReceiptInvalid
    }
}
