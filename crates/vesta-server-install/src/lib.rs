//! Local automatic JAR installation backend, not an authenticated management API.
//! Plans are reviewed snapshots; receipts and software support grant no authority.
//! No EULA acceptance, server launch, update/adoption, service or listener is implemented.
mod build_process;
mod execution;
// Compiled internal primitives; production loader recipe constructors are deliberately absent.
mod bundle;
mod catalogue_cache;
mod config;
#[allow(dead_code)]
mod execution_workspace;
mod install;
mod managed;
mod model;
mod paths;
pub mod progress;
mod providers;
mod runtime;
mod transfer;

pub use bundle::{ArtifactRecord, BundleReceipt};
pub use catalogue_cache::{
    CATALOGUE_CACHE_DISK_BYTES, CATALOGUE_CACHE_DOCUMENT_BYTES, CATALOGUE_CACHE_ENTRIES,
    CatalogueRecord, CatalogueStore,
};
pub use config::TransportConfig;
pub use install::{InstallOutcome, InstallRoots, Receipt};
pub use managed::{ManagedArtifact, ManagedLaunch, verify_managed_artifacts};
pub use model::{
    Acknowledgments, Approval, ArtifactReview, DigestAlgorithm, GeneratedFile, InstallError,
    InstallPlan, JavaRequirement, LaunchRecipe, Limits, PublishedDigest, Result, Software,
    SoftwareDescriptor, SourceFamily, Warning,
};
pub use progress::{ProgressReporter, SilentProgressReporter};
pub use providers::buildtools::{BuiltReceipt, SourceRefs};
pub use providers::{Selection, releases::SoftwareRelease};
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

// Catalogue browsing must finish before the launcher's 15-second HTTP budget.
// Installation/planning retain their independent, longer job deadline.
const CATALOGUE_TOTAL_MS: u64 = 12_000;

impl ServerInstaller {
    fn catalogue_job(&self) -> transfer::Job {
        let mut limits = self.limits;
        limits.total_ms = limits.total_ms.min(CATALOGUE_TOTAL_MS);
        let mut job = transfer::Job::new(limits, 0);
        job.catalogue = true;
        job
    }

    /// Optional protected host persistence. Browsing snapshots never authorize
    /// install plans; those keep independent fresh provider reads.
    pub fn with_catalogue_store(mut self, store: Arc<dyn CatalogueStore>) -> Self {
        self.http.catalogues = catalogue_cache::CatalogueCache::new(Some(store));
        self
    }

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
    /// Browsing catalogue. Optional protected cache writes only; no game writes
    /// or runtime execution. Installation plans resolve their own fresh evidence.
    pub async fn versions(
        &self,
        software: Software,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<String>> {
        if reporter.is_dry_run() {
            return Err(InstallError::InvalidInput);
        }
        let mut job = self.catalogue_job();
        job.ready(reporter).await?;
        let key = self
            .http
            .catalogue_key(&format!("versions:{software:?}:{:?}", self.limits));
        let guard = self.http.catalogues.query(&key, &job, reporter).await?;
        if let Some(value) = self.cached_result::<String>(&key, &job, reporter).await? {
            if value.iter().all(|id| model::identifier(id)) {
                return Ok(value);
            }
        }
        let value = providers::versions(&self.http, software, &mut job, reporter)
            .await
            .inspect_err(|error| guard.failed(*error))?;
        self.cache_result(key, &value, job);
        Ok(value)
    }
    /// Available build/loader choices. Metadata only; planning still verifies the selection.
    pub async fn releases(
        &self,
        software: Software,
        version: &str,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<SoftwareRelease>> {
        if reporter.is_dry_run() {
            return Err(InstallError::InvalidInput);
        }
        if !model::identifier(version) {
            return Err(InstallError::InvalidInput);
        }
        let mut job = self.catalogue_job();
        job.ready(reporter).await?;
        let key = self.http.catalogue_key(&format!(
            "releases:{software:?}:{version}:{:?}",
            self.limits
        ));
        let guard = self.http.catalogues.query(&key, &job, reporter).await?;
        if let Some(value) = self
            .cached_result::<SoftwareRelease>(&key, &job, reporter)
            .await?
        {
            if value.iter().all(|release| {
                model::identifier(&release.id)
                    && release.build != Some(0)
                    && release
                        .loader_version
                        .as_deref()
                        .is_none_or(model::identifier)
            }) {
                return Ok(value);
            }
        }
        let value = providers::releases::list(&self.http, software, version, &mut job, reporter)
            .await
            .inspect_err(|error| guard.failed(*error))?;
        self.cache_result(key, &value, job);
        Ok(value)
    }

    async fn cached_result<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
        job: &transfer::Job,
        reporter: &dyn ProgressReporter,
    ) -> Result<Option<Vec<T>>> {
        let value = transfer::controlled(
            async { Ok(self.http.catalogues.get(key).await) },
            job.deadline,
            reporter,
        )
        .await?;
        Ok(value
            .and_then(|record| serde_json::from_slice::<providers::List<T>>(&record.bytes).ok())
            .filter(|list| list.0.len() <= job.limits.max_entries)
            .map(|list| list.0))
    }
    fn cache_result(&self, key: String, value: &impl serde::Serialize, mut job: transfer::Job) {
        if let Ok(bytes) = serde_json::to_vec(value) {
            job.cache_candidate(
                key,
                &bytes,
                job.fresh_until
                    .saturating_duration_since(tokio::time::Instant::now()),
            );
        }
        self.http.catalogues.commit(job.pending_cache);
    }
    /// Resolves an exact artifact and probes the caller-selected existing Java; no install writes.
    pub async fn plan(
        &self,
        software: Software,
        selection: &Selection,
        java: &Path,
        reporter: &dyn ProgressReporter,
    ) -> Result<InstallPlan> {
        let plan = self
            .plan_preview(software, selection, java, reporter)
            .await?;
        if !plan.runtime_compatible() {
            return Err(InstallError::RuntimeIncompatible);
        }
        Ok(plan)
    }
    /// Metadata preview for management: an incompatible result cannot be approved.
    pub async fn plan_preview(
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
        job.ready(reporter).await?;
        let mut source_build = resolved.source_build;
        if let Some(build) = &mut source_build {
            build.bind_tools(&runtime, &self.roots.base)?;
        }
        let lifetime = if source_build.is_some() { 7200 } else { 3600 };
        Ok(InstallPlan {
            schema: if source_build.is_some() {
                4
            } else if resolved.processed.is_some() {
                3
            } else if resolved.bundle.is_some() {
                2
            } else {
                1
            },
            processed: resolved.processed,
            source_build,
            install_id: format!("srv_{}", uuid::Uuid::new_v4().simple()),
            software,
            version: resolved.version,
            build: resolved.build,
            artifact: resolved.artifact,
            runtime,
            required_java: resolved.java,
            base: self.roots.base.clone(),
            forbidden: self.roots.forbidden.clone(),
            expires: now()?
                .checked_add(lifetime)
                .ok_or(InstallError::InvalidInput)?,
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
        Box::pin(install::execute(self, plan, approval, reporter)).await
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
