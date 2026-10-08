//! Local automatic JAR installation backend, not an authenticated management API.
//! Plans are reviewed snapshots; receipts and software support grant no authority.
//! No EULA acceptance, server launch, update/adoption, service or listener is implemented.
mod execution;
// Compiled internal primitives; production loader recipe constructors are deliberately absent.
mod bundle;
mod config;
#[allow(dead_code)]
mod execution_workspace;
mod install;
mod model;
mod paths;
pub mod progress;
mod providers;
mod runtime;
mod transfer;

pub use bundle::{ArtifactRecord, BundleReceipt};
pub use config::TransportConfig;
pub use install::{InstallOutcome, InstallRoots, Receipt};
pub use model::{
    Acknowledgments, Approval, ArtifactReview, DigestAlgorithm, GeneratedFile, InstallError,
    InstallPlan, JavaRequirement, LaunchRecipe, Limits, PublishedDigest, Result, Software,
    SoftwareDescriptor, SourceFamily, Warning,
};
pub use progress::{ProgressReporter, SilentProgressReporter};
pub use providers::Selection;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct ServerInstaller {
    roots: InstallRoots,
    limits: Limits,
    http: transfer::Transport,
    coordinator: Arc<install::Coordinator>,
    #[cfg(test)]
    fixture_java: Option<u32>,
    #[cfg(test)]
    fixture_space: Option<(PathBuf, u64)>,
    #[cfg(test)]
    fail_after_artifact: bool,
    /// Zero-based bundle link index; artifacts first, generated argfile last.
    #[cfg(test)]
    fail_after_bundle_link: Option<usize>,
    #[cfg(test)]
    fixture_space_error: Option<InstallError>,
}
impl ServerInstaller {
    pub fn new(roots: InstallRoots, transport: TransportConfig) -> Result<Self> {
        Self::with_limits(roots, Limits::default(), transport)
    }
    /// Limits may be tightened, not increased beyond the hard production ceilings/reserve.
    pub fn with_limits(
        roots: InstallRoots,
        limits: Limits,
        transport: TransportConfig,
    ) -> Result<Self> {
        Ok(Self {
            roots,
            limits: limits.validate()?,
            http: transfer::Transport::new(transport),
            coordinator: install::Coordinator::shared(),
            #[cfg(test)]
            fixture_java: None,
            #[cfg(test)]
            fixture_space: None,
            #[cfg(test)]
            fail_after_artifact: false,
            #[cfg(test)]
            fail_after_bundle_link: None,
            #[cfg(test)]
            fixture_space_error: None,
        })
    }
    /// Metadata-only network catalogue. No filesystem writes or runtime execution.
    pub async fn versions(
        &self,
        software: Software,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<String>> {
        if reporter.is_dry_run() {
            return Err(InstallError::InvalidInput);
        }
        providers::versions(
            &self.http,
            software,
            &mut transfer::Job::new(self.limits, 0),
            reporter,
        )
        .await
    }
    /// Resolves an exact artifact and probes the caller-selected existing Java; no install writes.
    pub async fn plan(
        &self,
        software: Software,
        selection: &Selection,
        java: &Path,
        reporter: &dyn ProgressReporter,
    ) -> Result<InstallPlan> {
        if reporter.is_dry_run() {
            return Err(InstallError::InvalidInput);
        }
        self.roots.recheck()?;
        let mut job = transfer::Job::new(self.limits, 0);
        let resolved =
            providers::resolve(&self.http, software, selection, &mut job, reporter).await?;
        job.ready(reporter).await?;
        let selected = runtime::RuntimeBinding::snapshot(java, 0)?;
        if install::overlap(&selected.path, &self.roots.base) {
            return Err(InstallError::PathRefused);
        }
        #[cfg(test)]
        let runtime = if let Some(major) = self.fixture_java {
            runtime::RuntimeBinding::snapshot(java, major)?
        } else {
            runtime::probe_until(java, reporter, job.deadline).await?
        };
        #[cfg(not(test))]
        let runtime = runtime::probe_until(java, reporter, job.deadline).await?;
        if (runtime::RuntimeBinding {
            major: 0,
            ..runtime.clone()
        }) != selected
        {
            return Err(InstallError::RuntimeFailed);
        }
        if install::overlap(&runtime.path, &self.roots.base) {
            return Err(InstallError::PathRefused);
        }
        if runtime.major < resolved.java.major {
            return Err(InstallError::RuntimeIncompatible);
        }
        job.ready(reporter).await?;
        Ok(InstallPlan {
            schema: if resolved.bundle.is_some() { 2 } else { 1 },
            install_id: format!("srv_{}", uuid::Uuid::new_v4().simple()),
            software,
            version: resolved.version,
            build: resolved.build,
            artifact: resolved.artifact,
            runtime,
            required_java: resolved.java,
            base: self.roots.base.clone(),
            forbidden: self.roots.forbidden.clone(),
            expires: now()?.checked_add(3600).ok_or(InstallError::InvalidInput)?,
            warnings: resolved.warnings,
            metadata: resolved.metadata,
            transferred: job.bytes,
            limits: self.limits,
            connection_policy: self.http.policy_id.clone(),
            bundle: resolved.bundle,
        })
    }
    /// Approval is a local workflow assertion, never principal/device authentication.
    pub async fn install(
        &self,
        plan: &InstallPlan,
        approval: Approval,
        reporter: &dyn ProgressReporter,
    ) -> Result<InstallOutcome> {
        install::execute(self, plan, approval, reporter).await
    }
    /// Rehashes installed bytes against a strictly parsed, plan-bound receipt. Not ongoing confinement.
    pub async fn verify(&self, plan: &InstallPlan) -> Result<Receipt> {
        if plan.base != self.roots.base || plan.forbidden != self.roots.forbidden {
            return Err(InstallError::ApprovalMismatch);
        }
        self.roots.recheck()?;
        install::verify(plan).await
    }
    fn space(&self) -> Result<(PathBuf, u64)> {
        #[cfg(test)]
        if let Some(error) = self.fixture_space_error {
            return Err(error);
        }
        #[cfg(test)]
        if let Some(space) = &self.fixture_space {
            return Ok(space.clone());
        }
        install::disk_space(&self.roots.base)
    }
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|n| n.as_secs())
        .map_err(|_| InstallError::InvalidInput)
}
#[cfg(test)]
mod tests;
