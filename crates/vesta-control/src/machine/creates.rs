//! Machine-level creation metadata. Catalogue access never needs a worker or
//! executes a runtime. Installer plans must independently resolve fresh evidence.
use super::{
    HostState,
    imports::jobs::{ImportJobs, JobError, JobResult},
};
use crate::java::HostJavaPlan;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub use super::imports::jobs::{
    CreateApplyRequest, CreateJob, CreateReview, CreateRuntime, CreateSummary, NewServerRequest,
    ReviewRuntime, ServerFacts, SoftwareChoice, SummarySoftware,
};
use vesta_server_install::{
    Acknowledgments, Approval, InstallError, InstallOutcome, InstallRoots, ManagedLaunch,
    MinecraftVersion, ProgressReporter, ResolvedPreview, Selection, ServerInstaller,
    SilentProgressReporter, Software, SoftwareRelease, TransportConfig,
};

/// Narrow deterministic seam for host catalogue/router tests.
pub trait Catalogue: Send + Sync {
    fn minecraft(&self) -> JobResult<Vec<MinecraftVersion>>;
    fn releases(&self, kind: Software, minecraft: &str) -> JobResult<Vec<SoftwareRelease>>;
    /// Whether CraftBukkit/Spigot source builds can run on this machine (git
    /// and a POSIX shell available to the service).
    fn source_build_tools(&self) -> bool;
}

/// What an install needs: the reviewed resolution, the exact selection that
/// produced it, a trusted Java executable and a fresh installation root.
pub struct InstallRequest<'a> {
    pub preview: &'a ResolvedPreview,
    pub selection: &'a Selection,
    pub java: &'a Path,
    pub install_root: &'a Path,
    /// The owner acknowledged a legacy checksum at review, or the host accepted
    /// Mojang's SHA-1-only official objects automatically (audited).
    pub legacy_checksum_acknowledged: bool,
}
/// Install progress and cooperative cancellation, owned by the create job.
pub trait CreateProgress: Send + Sync {
    /// `downloading_software` or `installing`.
    fn phase(&self, phase: &'static str);
    fn bytes(&self, done: u64, total: Option<u64>);
    fn cancelled(&self) -> bool;
}
/// Server software installation seam for creation jobs. The live
/// implementation drives `vesta-server-install` and the managed Java cache;
/// tests substitute a deterministic fake. Error values are fixed job codes.
pub trait CreateInstaller: Send + Sync {
    /// Fresh metadata-only resolution of the exact artifacts:
    /// `software_unresolved` or `provider_unavailable` on failure.
    fn resolve(&self, software: Software, selection: &Selection)
    -> Result<ResolvedPreview, &'static str>;
    /// Confirm against Mojang's own manifest (piston-meta) that every SHA-1-only
    /// Mojang object in the preview is exactly what Mojang publishes for that
    /// version. Only then does the host accept those digests automatically.
    /// `integrity_conflict` on any difference, `provider_unavailable` when
    /// Mojang cannot be reached or answers badly.
    fn verify_mojang(&self, preview: &ResolvedPreview) -> Result<(), &'static str>;
    /// Whether this service can run CraftBukkit/Spigot source builds: git and
    /// a POSIX shell on its own PATH (the JDK is managed by Vesta).
    fn source_build_tools(&self) -> bool;
    /// Choose a trusted runtime of exactly this major (existing or to
    /// download); `jdk` requires `javac` (managed JDK when none is installed).
    fn plan_java(&self, required_major: u32, jdk: bool) -> HostJavaPlan;
    /// Make the planned runtime available: `java_unresolved` on failure.
    fn ensure_java(&self, runtime: &ReviewRuntime) -> Result<PathBuf, &'static str>;
    /// Install the reviewed artifacts under `install_root` (refusing anything
    /// that differs from the review: `integrity_conflict`), then call
    /// `publish` exactly once, which creates the game directory, and return the
    /// trusted launch for that directory built by the installer itself.
    /// `cancelled`, `download_failed`, `integrity_conflict`, `java_unresolved`,
    /// `insufficient_space` or `install_failed` on failure.
    fn install(
        &self,
        request: &InstallRequest<'_>,
        progress: &dyn CreateProgress,
        publish: &mut dyn FnMut() -> Result<PathBuf, &'static str>,
    ) -> Result<ManagedLaunch, &'static str>;
    /// JVM options Vesta adds for the reserved memory.
    fn heap_args(&self, memory_mib: u64) -> Vec<String> {
        vec![format!("-Xmx{memory_mib}M")]
    }
}

/// Live installer: provider downloads verified by `vesta-server-install` and
/// checksum-verified managed Java in the host's shared cache.
pub struct LiveCreateInstaller {
    java_cache: PathBuf,
    /// Managed server storage; Java found there is never executed.
    excluded: Vec<PathBuf>,
    /// Long-lived metadata client so Mojang's manifest is cached briefly
    /// between plans, like the other catalogues. Never installs.
    metadata: Option<ServerInstaller>,
}
impl LiveCreateInstaller {
    pub fn new(java_cache: PathBuf, servers_root: PathBuf) -> Self {
        let metadata = InstallRoots::new(&servers_root, &[])
            .and_then(|roots| ServerInstaller::new(roots, TransportConfig::direct()))
            .ok();
        Self {
            java_cache,
            excluded: vec![servers_root],
            metadata,
        }
    }
}
/// Maps installer steps onto the create job's phases and progress.
struct Reporter<'a>(&'a dyn CreateProgress);
impl ProgressReporter for Reporter<'_> {
    fn start_step(&self, name: &str, _: Option<u32>) {
        self.0.phase(if name.contains("download") {
            "downloading_software"
        } else {
            "installing"
        });
    }
    fn update_bytes(&self, transferred: u64, total: Option<u64>) {
        self.0.bytes(transferred, total);
    }
    fn done(&self, _: bool, _: Option<&str>) {}
    fn is_cancelled(&self) -> bool {
        self.0.cancelled()
    }
    fn is_paused(&self) -> bool {
        false
    }
}
fn install_code(error: InstallError) -> &'static str {
    match error {
        InstallError::Cancelled => "cancelled",
        InstallError::Unavailable
        | InstallError::TransportFailed
        | InstallError::Deadline
        | InstallError::LimitExceeded
        | InstallError::SourceRefused
        | InstallError::InvalidMetadata => "download_failed",
        InstallError::ArtifactChanged | InstallError::IntegrityConflict => "integrity_conflict",
        InstallError::RuntimeUnknown
        | InstallError::RuntimeIncompatible
        | InstallError::RuntimeFailed => "java_unresolved",
        InstallError::StorageLow => "insufficient_space",
        _ => "install_failed",
    }
}
impl CreateInstaller for LiveCreateInstaller {
    fn resolve(
        &self,
        software: Software,
        selection: &Selection,
    ) -> Result<ResolvedPreview, &'static str> {
        // Metadata only: like the catalogue, the managed storage root satisfies
        // the installer's path rules and nothing is written to it.
        let installer = InstallRoots::new(&self.excluded[0], &[])
            .and_then(|roots| ServerInstaller::new(roots, TransportConfig::direct()))
            .map_err(|_| "provider_unavailable")?;
        runtime()
            .map_err(|_| "provider_unavailable")?
            .block_on(installer.resolve_preview(software, selection, &SilentProgressReporter))
            .map_err(|e| match e {
                InstallError::Unavailable
                | InstallError::InvalidInput
                | InstallError::RuntimeUnknown => "software_unresolved",
                _ => "provider_unavailable",
            })
    }
    fn verify_mojang(&self, preview: &ResolvedPreview) -> Result<(), &'static str> {
        runtime()
            .map_err(|_| "provider_unavailable")?
            .block_on(
                self.metadata
                    .as_ref()
                    .ok_or("provider_unavailable")?
                    .verify_mojang_objects(preview, &SilentProgressReporter),
            )
            .map_err(|e| match e {
                InstallError::IntegrityConflict | InstallError::ArtifactChanged => {
                    "integrity_conflict"
                }
                _ => "provider_unavailable",
            })
    }
    fn source_build_tools(&self) -> bool {
        vesta_server_install::source_build_tools_available()
    }
    fn plan_java(&self, required_major: u32, jdk: bool) -> HostJavaPlan {
        match runtime() {
            Ok(rt) => rt.block_on(crate::java::plan_host_java_image(
                &self.java_cache,
                Some(required_major),
                &self.excluded,
                jdk,
            )),
            Err(_) => crate::java::plan_for_major(&[], None),
        }
    }
    fn ensure_java(&self, runtime_plan: &ReviewRuntime) -> Result<PathBuf, &'static str> {
        let plan = HostJavaPlan {
            required_major: runtime_plan.required_major,
            selected_runtime_id: runtime_plan.selected_runtime_id.clone(),
            download_required: runtime_plan.download_required,
            verified: runtime_plan.verified,
            status: "resolved",
            blocker: None,
        };
        runtime()
            .map_err(|_| "java_unresolved")?
            .block_on(crate::java::ensure_host_java_image(
                &self.java_cache,
                &plan,
                &self.excluded,
                runtime_plan.jdk,
            ))
            .map(|java| PathBuf::from(java.path))
            .map_err(|_| "java_unresolved")
    }
    fn install(
        &self,
        request: &InstallRequest<'_>,
        progress: &dyn CreateProgress,
        publish: &mut dyn FnMut() -> Result<PathBuf, &'static str>,
    ) -> Result<ManagedLaunch, &'static str> {
        let installer = InstallRoots::new(request.install_root, &[])
            .and_then(|roots| ServerInstaller::new(roots, TransportConfig::direct()))
            .map_err(|_| "install_failed")?;
        let reporter = Reporter(progress);
        runtime().map_err(|_| "install_failed")?.block_on(async {
            let preview = request.preview;
            // Fresh provider evidence and a probed runtime, bound to the review.
            let plan = installer
                .plan(preview.software, request.selection, request.java, &reporter)
                .await
                .map_err(install_code)?;
            if plan.artifacts_sha256() != preview.artifacts_sha256
                || plan.version() != preview.version
                || plan.loader_version() != preview.loader_version.as_deref()
                || plan.warnings() != preview.warnings.as_slice()
            {
                return Err("integrity_conflict");
            }
            // The owner acknowledged every reviewed warning at apply and this plan
            // matches that review exactly; a legacy checksum is acknowledged only
            // by the owner or, for Mojang's own objects, by the host.
            let approval = Approval::review(
                &plan,
                Acknowledgments {
                    legacy_checksum: request.legacy_checksum_acknowledged,
                    warnings: true,
                },
            )
            .map_err(install_code)?;
            match installer
                .install(&plan, approval, &reporter)
                .await
                .map_err(install_code)?
            {
                InstallOutcome::Installed(_) => {}
                InstallOutcome::DryRun => return Err("install_failed"),
            }
            if progress.cancelled() {
                return Err("cancelled");
            }
            let game = publish()?;
            // Re-verifies the installed bytes before building the launch.
            installer
                .managed_launch(&plan, &game)
                .await
                .map_err(install_code)
        })
    }
}

pub struct LiveCatalogue(ServerInstaller);
impl LiveCatalogue {
    pub fn open(root: &Path) -> JobResult<Self> {
        let roots = InstallRoots::new(root, &[]).map_err(|_| JobError::UNAVAILABLE)?;
        ServerInstaller::new(roots, TransportConfig::direct())
            .map(Self)
            .map_err(|_| JobError::UNAVAILABLE)
    }
}
fn runtime() -> JobResult<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| JobError::UNAVAILABLE)
}
impl Catalogue for LiveCatalogue {
    fn minecraft(&self) -> JobResult<Vec<MinecraftVersion>> {
        runtime()?
            .block_on(self.0.minecraft_catalogue(&SilentProgressReporter))
            .map_err(|_| JobError("provider_unavailable"))
    }
    fn releases(&self, kind: Software, minecraft: &str) -> JobResult<Vec<SoftwareRelease>> {
        match runtime()?.block_on(self.0.releases(kind, minecraft, &SilentProgressReporter)) {
            Ok(releases) => Ok(releases),
            // The provider answered: it has no server for this Minecraft version.
            Err(InstallError::Unavailable | InstallError::InvalidInput) => Ok(Vec::new()),
            Err(_) => Err(JobError("provider_unavailable")),
        }
    }
    fn source_build_tools(&self) -> bool {
        vesta_server_install::source_build_tools_available()
    }
}
/// CraftBukkit and Spigot are built from source by BuildTools.
pub fn source_built(kind: Software) -> bool {
    matches!(kind, Software::Bukkit | Software::Spigot)
}

pub fn minecraft(catalogue: &dyn Catalogue) -> JobResult<Value> {
    let versions = catalogue.minecraft()?;
    let latest = versions
        .iter()
        .find(|v| v.recommended && v.stable)
        .ok_or(JobError("provider_unavailable"))?;
    Ok(json!({"latestStable":latest.id,"versions":versions}))
}

pub fn software(catalogue: &dyn Catalogue, version: &str) -> JobResult<Value> {
    if version.is_empty()
        || version.len() > 64
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    {
        return Err(JobError::INVALID);
    }
    // Query providers concurrently, including Minecraft, so the independent
    // 12-second provider deadlines fit the launcher's 15-second request budget.
    let build_tools = catalogue.source_build_tools();
    let (versions, releases) = std::thread::scope(|scope| {
        let minecraft = scope.spawn(|| catalogue.minecraft());
        let pending: Vec<_> = Software::ALL
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    scope.spawn(move || {
                        if source_built(kind) && !build_tools {
                            // Not offered: the machine cannot build it.
                            Err(JobError("requires_git"))
                        } else if kind == Software::Vanilla {
                            Ok(vec![SoftwareRelease {
                                id: version.to_owned(),
                                build: None,
                                loader_version: None,
                            }])
                        } else {
                            // An outage is reported as such, never as "no builds".
                            catalogue.releases(kind, version)
                        }
                    }),
                )
            })
            .collect();
        let versions = minecraft.join().map_err(|_| JobError::UNAVAILABLE)??;
        let releases = pending
            .into_iter()
            .map(|(kind, h)| {
                h.join()
                    .map(|r| (kind, r))
                    .map_err(|_| JobError::UNAVAILABLE)
            })
            .collect::<JobResult<Vec<_>>>()?;
        Ok::<_, JobError>((versions, releases))
    })?;
    let selected = versions
        .iter()
        .find(|v| v.id == version)
        .ok_or(JobError::INVALID)?;
    let kinds: Vec<_> = releases
        .into_iter()
        .map(|(kind, releases)| {
            let experimental = selected.experimental || matches!(kind, Software::Folia);
            let (releases, unavailable) = match releases {
                Ok(r) if r.is_empty() => (r, Some("unsupported_version")),
                Ok(r) => (r, None),
                Err(JobError("requires_git")) => (Vec::new(), Some("requires_git")),
                Err(_) => (Vec::new(), Some("provider_unavailable")),
            };
            // Every provider returns its choices newest first (sorted explicitly
            // in `vesta-server-install`), so the first is the newest build.
            let builds: Vec<_> = releases.into_iter().enumerate().map(|(i, b)| json!({
            "id":b.id,"build":b.build,"loaderVersion":b.loader_version,
            "stable":!experimental,"experimental":experimental,"recommended":i==0 && !experimental
        })).collect();
            json!({"kind":kind.id(),"label":label(kind),"stable":!experimental,
            "experimental":experimental,"recommended":kind==Software::Vanilla && !experimental,
            "available":!builds.is_empty(),"unavailableReason":unavailable,"builds":builds})
        })
        .collect();
    Ok(json!({"minecraftVersion":version,"kinds":kinds}))
}
fn label(kind: Software) -> &'static str {
    match kind {
        Software::Vanilla => "Vanilla",
        Software::Paper => "Paper",
        Software::Folia => "Folia",
        Software::Purpur => "Purpur",
        Software::Fabric => "Fabric",
        Software::Quilt => "Quilt",
        Software::Forge => "Forge",
        Software::NeoForge => "NeoForge",
        Software::Bukkit => "CraftBukkit",
        Software::Spigot => "Spigot",
    }
}

pub fn defaults(
    host: &HostState,
    jobs: &ImportJobs,
    catalogue: &dyn Catalogue,
) -> JobResult<Value> {
    let name = jobs.suggested_create_name()?;
    // Check availability using the same reservations and observations as apply.
    let (memory, port) = jobs.resource_plans(None, None)?;
    if let Some(code) = memory.blocker.or(port.blocker) {
        return Err(JobError(code));
    }
    let _ = host.snapshot()?;
    let versions = minecraft(catalogue)?;
    Ok(
        json!({"name":name,"minecraftVersion":versions["latestStable"],
        "software":{"kind":"vanilla","build":null,"loaderVersion":null},
        "gamePort":port.game_port,"memoryMiB":memory.memory_mib,"java":"automatic"}),
    )
}
