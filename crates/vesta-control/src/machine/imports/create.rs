//! Owner server creation jobs, kind `new` (`protocol/host-v1/create.md`).
//!
//! Creates share the durable job store with imports ([`Kind::Create`] rows):
//! the same reservations, idempotent apply, cancellation, resume, single
//! registration, worker creation and trusted start. Planning resolves the
//! exact software artifacts from the providers (metadata only, no downloads or
//! execution) and chooses a compatible Java major. Apply re-checks owner, plan,
//! warnings and EULA, reserves name/port/memory atomically, then: downloads
//! verified Java when needed, installs through `vesta-server-install` (fresh
//! plan bound to the reviewed artifact digest, verified downloads and its own
//! staging/publish under `<serverId>/install`), stages the game directory
//! (owner EULA record, reserved port) and publishes it as `<serverId>/game`,
//! freezes the launch built from the installer's own result, registers once and
//! leaves the server `ready`. Nothing from a provider is run as a script.
use super::*;
use crate::machine::creates::{Catalogue, CreateInstaller, CreateProgress, InstallRequest};
use crate::worker::WorkerHandle;
use vesta_server_install::{ResolvedPreview, Selection, Software, Warning};

/// Space kept for a managed Java download (archive plus extraction).
const JAVA_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
/// A managed JDK (CraftBukkit/Spigot builds) is larger than the JRE.
const JDK_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;
/// Minimum interval between durable byte-progress writes.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoftwareChoice {
    pub kind: String,
    pub build: Option<u32>,
    pub loader_version: Option<String>,
}
/// `{"kind":"new",...}` plan request. Null port/memory take the defaults.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewServerRequest {
    pub kind: String,
    pub name: String,
    pub minecraft_version: String,
    pub software: SoftwareChoice,
    pub game_port: Option<u16>,
    #[serde(rename = "memoryMiB", alias = "memoryMib")]
    pub memory_mib: Option<u64>,
    /// Accepted for symmetry with apply; planning never accepts the EULA.
    #[serde(default)]
    pub eula_accepted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateApplyRequest {
    pub plan_id: String,
    pub confirmation: String,
    pub acknowledged_warnings: Vec<String>,
    pub eula_accepted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateReview {
    pub plan_id: String,
    /// Null when blocked or no longer awaiting review.
    pub confirmation: Option<String>,
    /// Milliseconds since the Unix epoch; transport renders RFC 3339.
    pub expires_at: u64,
    pub summary: CreateSummary,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}
impl Reviewed for CreateReview {
    fn clear_confirmation(&mut self) {
        self.confirmation = None;
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateSummary {
    pub kind: String,
    pub name: String,
    pub minecraft_version: String,
    pub software: SummarySoftware,
    pub resources: ReviewResources,
    pub runtime: CreateRuntime,
    pub destination: ReviewDestination,
    pub pack: Option<serde_json::Value>,
    pub restore: Option<serde_json::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SummarySoftware {
    pub kind: String,
    /// The software's own version: loader version, else build, else (Vanilla)
    /// the Minecraft version. Null while unresolved.
    pub version: Option<String>,
    pub build: Option<u32>,
    pub loader_version: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateRuntime {
    pub required_major: Option<u32>,
    pub download_required: bool,
    pub automatic: bool,
}
pub type CreateJob = Job<CreateReview>;

/// Host-private half of a create plan; never returned to callers.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CreatePrivate {
    preview: Option<ResolvedPreview>,
    experimental: bool,
    /// Mojang publishes SHA-1 only for its official objects; the host accepts
    /// that legacy digest on the owner's behalf instead of asking (audited).
    #[serde(default)]
    mojang_sha1_accepted: bool,
    java: ReviewRuntime,
    /// The reviewed pack (modpack plans only).
    #[serde(default)]
    pack: Option<super::modpack::PackPrivate>,
    /// The reviewed backup snapshot (restore plans only).
    #[serde(default)]
    pub(super) restore: Option<super::restore::RestorePrivate>,
    plan_hash: String,
}
impl CreatePrivate {
    /// A sealed restore plan: no software preview, the planned Java and the
    /// reviewed snapshot, bound to the review by the plan hash.
    pub(super) fn sealed_restore(
        binding: &OwnerBinding,
        review: &CreateReview,
        java: ReviewRuntime,
        restore: super::restore::RestorePrivate,
    ) -> JobResult<Self> {
        let private = Self {
            preview: None,
            experimental: false,
            mojang_sha1_accepted: false,
            java,
            pack: None,
            restore: Some(restore),
            plan_hash: String::new(),
        };
        Ok(Self {
            plan_hash: create_plan_hash(binding, review, &private)?,
            ..private
        })
    }
    pub(super) fn java(&self) -> &ReviewRuntime {
        &self.java
    }
}

/// Facts about a committed server for the owner's server list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerFacts {
    /// `(kind, version)`; the version may be unknown for imports.
    pub software: Option<(String, Option<String>)>,
    pub minecraft_version: Option<String>,
}

/// The software's own version for an imported server, from the facts its
/// import review recorded (detected loader, Minecraft version and the exact
/// launch file), with the same meaning as `summary.software.version`: the
/// loader version for loaders, the build for Paper/Purpur, the Minecraft
/// version for Vanilla. `None` when those facts do not say.
pub(crate) fn imported_software_version(detected: &ReviewDetected) -> Option<String> {
    let minecraft = detected.minecraft_version.as_deref();
    let launch = detected.launch.as_ref().map(|l| l.relative_path.as_str());
    let version = |s: &str| {
        (!s.is_empty()
            && s.len() <= 64
            && s.bytes().any(|b| b.is_ascii_digit())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b)))
        .then(|| s.to_owned())
    };
    // `libraries/<group path>/<version dir>/<argfile>`.
    let library_dir = |group: &str| {
        let rest = launch?.strip_prefix(group)?;
        let (dir, file) = rest.split_once('/')?;
        matches!(file, "unix_args.txt" | "win_args.txt").then_some(dir)
    };
    match detected.loader.as_deref()? {
        "vanilla" => minecraft.and_then(version),
        "paper" | "purpur" => {
            let prefix = format!("{}-", detected.loader.as_deref()?);
            let (mc, build) = launch?
                .strip_prefix(&prefix)?
                .strip_suffix(".jar")?
                .rsplit_once('-')?;
            (minecraft.is_none_or(|m| m == mc)
                && !build.is_empty()
                && build.len() <= 9
                && build.bytes().all(|b| b.is_ascii_digit()))
            .then(|| build.trim_start_matches('0').to_owned())
            .filter(|b| !b.is_empty())
        }
        "fabric" => {
            let rest = launch?.strip_prefix("fabric-server-mc.")?;
            let (mc, rest) = rest.split_once("-loader.")?;
            let loader = rest
                .split_once("-launcher.")
                .map_or(rest.strip_suffix(".jar")?, |(l, _)| l);
            minecraft.is_none_or(|m| m == mc).then_some(())?;
            version(loader)
        }
        "forge" => {
            let coordinate = library_dir("libraries/net/minecraftforge/forge/").or_else(|| {
                let stem = launch?.strip_prefix("forge-")?.strip_suffix(".jar")?;
                Some(
                    stem.strip_suffix("-shim")
                        .or_else(|| stem.strip_suffix("-universal"))
                        .unwrap_or(stem),
                )
            })?;
            let (mc, loader) = coordinate.split_once('-')?;
            minecraft.is_none_or(|m| m == mc).then_some(())?;
            version(loader)
        }
        "neoforge" => library_dir("libraries/net/neoforged/neoforge/")
            .or_else(|| library_dir("libraries/net/neoforged/forge/"))
            .and_then(version),
        _ => None,
    }
}

pub(crate) fn software_kind(id: &str) -> Option<Software> {
    Software::ALL.into_iter().find(|s| s.id() == id)
}

pub(super) fn validate_new(request: &NewServerRequest) -> JobResult<Software> {
    let name = request.name.trim();
    let version = &request.minecraft_version;
    if request.kind != "new"
        || name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
        || version.is_empty()
        || version.len() > 64
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        || request.game_port == Some(0)
    {
        return Err(JobError::INVALID);
    }
    let software = software_kind(&request.software.kind).ok_or(JobError::INVALID)?;
    let loader = request.software.loader_version.as_deref();
    if request.software.build == Some(0)
        || loader.is_some_and(|l| {
            l.is_empty()
                || l.len() > 64
                || !l
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        })
        // Vanilla has neither builds nor loaders.
        || (software == Software::Vanilla && (request.software.build.is_some() || loader.is_some()))
    {
        return Err(JobError::INVALID);
    }
    Ok(software)
}

fn summary_version(minecraft: &str, preview: &ResolvedPreview) -> String {
    preview
        .loader_version
        .clone()
        .or_else(|| preview.build.map(|b| b.to_string()))
        .unwrap_or_else(|| minecraft.to_owned())
}

/// Binds actor, plan id, expiry, the reviewed summary (destination, resources,
/// runtime, exact software), the reviewed artifact digest and warning set.
fn create_plan_hash(
    binding: &OwnerBinding,
    review: &CreateReview,
    private: &CreatePrivate,
) -> JobResult<String> {
    let mut bound = serde_json::json!({
        "kind": "create",
        "host": binding.host_id, "account": binding.account_id, "device": binding.device_id,
        "planId": review.plan_id, "expiresAt": review.expires_at,
        "summary": review.summary, "warnings": review.warnings, "blockers": review.blockers,
        "confirmation": review.confirmation,
        "artifacts": private.preview.as_ref().map(|p| &p.artifacts_sha256),
        "experimental": private.experimental, "mojangSha1": private.mojang_sha1_accepted, "java": private.java,
    });
    if let Some(pack) = &private.pack {
        // Only modpack plans carry it, so earlier plans keep their hash.
        bound["pack"] = serde_json::to_value(pack).map_err(|_| JobError::UNAVAILABLE)?;
    }
    if let Some(restore) = &private.restore {
        bound["restore"] = serde_json::to_value(restore).map_err(|_| JobError::UNAVAILABLE)?;
    }
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&bound).map_err(|_| JobError::UNAVAILABLE)?,
    )))
}

/// Durable byte progress and cooperative cancellation for an install.
struct InstallProgress<'a> {
    jobs: &'a ImportJobs,
    job_id: &'a str,
    cancel: &'a AtomicBool,
    last: Mutex<Option<std::time::Instant>>,
}
impl CreateProgress for InstallProgress<'_> {
    fn phase(&self, phase: &'static str) {
        if matches!(phase, "downloading_software" | "installing") {
            let _ = self.jobs.set_phase(self.job_id, "applying", phase);
            let _ = self
                .jobs
                .progress(self.job_id, CopyProgress::default(), 0, 0);
        }
    }
    fn bytes(&self, done: u64, total: Option<u64>) {
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        let complete = total.is_some_and(|t| done >= t);
        if !complete && last.is_some_and(|l| now.duration_since(l) < PROGRESS_INTERVAL) {
            return;
        }
        *last = Some(now);
        let progress = CopyProgress {
            files: 0,
            bytes: done,
        };
        let _ = self
            .jobs
            .progress(self.job_id, progress, 0, total.unwrap_or(0));
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire) || self.jobs.cancel_requested(self.job_id)
    }
}

impl ImportJobs {
    /// Validate and record a planning create job (the 202 response).
    pub fn begin_create(
        &self,
        binding: &OwnerBinding,
        request: &NewServerRequest,
    ) -> JobResult<PlanAccepted> {
        validate_new(request)?;
        let request = NewServerRequest {
            name: request.name.trim().to_owned(),
            ..request.clone()
        };
        let raw = serde_json::to_string(&request).map_err(|_| JobError::INVALID)?;
        self.insert_planning(binding, Kind::Create, "resolving", &raw)
    }

    /// Resolve software, Java and resources; ends in `review` or `blocked`.
    pub fn run_create_plan(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        installer: &dyn CreateInstaller,
        catalogue: &dyn Catalogue,
    ) -> JobResult<CreateJob> {
        let raw: String = self.read(|c| {
            self.owner(c, binding)?;
            let row: Option<(String, String, String)> = c
                .query_row(
                    "SELECT request,state,account_id FROM machine_import_jobs WHERE job_id=?1 AND kind='create'",
                    [job_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            match row {
                Some((raw, state, account))
                    if state == "planning" && account == binding.account_id =>
                {
                    Ok(raw)
                }
                Some(_) => Err(JobError::CONFLICT),
                None => Err(JobError::CREATE_NOT_FOUND),
            }
        })?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|_| JobError::UNAVAILABLE)?;
        let outcome = if value["kind"] == "restore" {
            let request: super::restore::RestoreServerRequest =
                serde_json::from_value(value).map_err(|_| JobError::UNAVAILABLE)?;
            self.compute_restore_plan(binding, &request)
        } else if value["kind"] == "modpack" {
            let request: super::modpack::ModpackServerRequest =
                serde_json::from_value(value).map_err(|_| JobError::UNAVAILABLE)?;
            self.compute_modpack_plan(binding, &request, installer, catalogue)
        } else {
            let request: NewServerRequest =
                serde_json::from_value(value).map_err(|_| JobError::UNAVAILABLE)?;
            self.compute_create_plan(binding, &request, installer, catalogue)
        };
        let now = self.0.host.now_ms();
        self.write(|c| {
            match &outcome {
                Ok((review, private)) => {
                    let state = if review.blockers.is_empty() {
                        "review"
                    } else {
                        "blocked"
                    };
                    c.execute(
                        "UPDATE machine_import_jobs SET state=?1,phase='resolving',plan_id=?2,review=?3,private=?4,updated_ms=?5 WHERE job_id=?6 AND state='planning'",
                        params![
                            state,
                            review.plan_id,
                            serde_json::to_string(review).map_err(|_| JobError::UNAVAILABLE)?,
                            serde_json::to_string(private).map_err(|_| JobError::UNAVAILABLE)?,
                            now,
                            job_id
                        ],
                    )?;
                }
                Err(error) => {
                    c.execute(
                        "UPDATE machine_import_jobs SET state='failed',error=?1,updated_ms=?2 WHERE job_id=?3 AND state='planning'",
                        params![error.code(), now, job_id],
                    )?;
                }
            }
            Ok(())
        })?;
        self.create_status(binding, job_id)
    }

    fn compute_create_plan(
        &self,
        binding: &OwnerBinding,
        request: &NewServerRequest,
        installer: &dyn CreateInstaller,
        catalogue: &dyn Catalogue,
    ) -> JobResult<(CreateReview, CreatePrivate)> {
        let software = validate_new(request)?;
        self.compute_create_core(binding, request, Some(software), None, installer, catalogue)
    }

    /// Shared planning core for `new` and `modpack`. `software` is `None`
    /// only for a pack that could not be read (the pack's blockers explain it).
    pub(super) fn compute_create_core(
        &self,
        binding: &OwnerBinding,
        request: &NewServerRequest,
        software: Option<Software>,
        pack: Option<super::modpack::PackPlan>,
        installer: &dyn CreateInstaller,
        catalogue: &dyn Catalogue,
    ) -> JobResult<(CreateReview, CreatePrivate)> {
        let minecraft = request.minecraft_version.clone();
        let mut blockers: BTreeSet<&'static str> = BTreeSet::new();
        let mut warnings: BTreeSet<&'static str> = BTreeSet::new();
        if let Some(pack) = &pack {
            blockers.extend(pack.blockers.iter().copied());
            warnings.extend(pack.warnings.iter().copied());
        }
        // Only versions the catalogue offers; snapshots are explicit experiments.
        let experimental = match software.map(|_| catalogue.minecraft()) {
            None => None,
            Some(Ok(versions)) => match versions.iter().find(|v| v.id == minecraft) {
                Some(v) => Some(v.experimental),
                None => {
                    blockers.insert("software_unresolved");
                    None
                }
            },
            Some(Err(_)) => {
                blockers.insert("provider_unavailable");
                None
            }
        };
        // CraftBukkit/Spigot are compiled by BuildTools, which needs git and a
        // POSIX shell from this machine: block now rather than fail the install.
        let source_built = software.is_some_and(crate::machine::creates::source_built);
        let tools_missing = source_built && !installer.source_build_tools();
        if tools_missing {
            blockers.insert("requires_git");
        }
        let preview = match (experimental, software) {
            _ if tools_missing => None,
            (Some(experimental), Some(software)) => {
                let selection = Selection {
                    version: Some(minecraft.clone()),
                    loader_version: request.software.loader_version.clone(),
                    build: request.software.build,
                    experimental,
                };
                match installer.resolve(software, &selection) {
                    // The provider must have pinned exactly what was chosen.
                    Ok(p)
                        if p.software == software
                            && p.version == minecraft
                            && request.software.build.is_none_or(|b| p.build == Some(b))
                            && request
                                .software
                                .loader_version
                                .as_ref()
                                .is_none_or(|l| p.loader_version.as_ref() == Some(l)) =>
                    {
                        Some(p)
                    }
                    Ok(_) => {
                        blockers.insert("software_unresolved");
                        None
                    }
                    Err(code) => {
                        blockers.insert(code);
                        None
                    }
                }
            }
            _ => None,
        };
        if let Some(p) = &preview {
            if experimental == Some(true)
                || p.warnings.contains(&Warning::ExperimentalBuild)
                || p.warnings.contains(&Warning::RegionThreading)
            {
                warnings.insert("experimental_software");
            }
            // Legacy digests from any source other than Mojang's own objects
            // host need the owner's acknowledgement.
            if p.warnings.contains(&Warning::LegacyChecksum)
                || (p.legacy_checksum && !p.legacy_checksum_mojang_only)
            {
                warnings.insert("legacy_checksum");
            }
            if p.warnings.contains(&Warning::UnsupportedUpstream) {
                warnings.insert("unsupported_upstream");
            }
        }
        // The host accepts Mojang's SHA-1-only objects on the owner's behalf
        // only when Mojang's own manifest publishes exactly those objects.
        let mut mojang_sha1_accepted = false;
        if let Some(p) = &preview
            && p.legacy_checksum
            && !warnings.contains("legacy_checksum")
        {
            match installer.verify_mojang(p) {
                Ok(()) => mojang_sha1_accepted = true,
                Err(code) => {
                    blockers.insert(code);
                }
            }
        }
        let java = match &preview {
            Some(p) => installer.plan_java(p.required_java.major, source_built),
            None => crate::java::HostJavaPlan {
                required_major: None,
                selected_runtime_id: None,
                download_required: false,
                verified: false,
                status: "unresolved",
                blocker: None,
            },
        };
        if let Some(blocker) = java.blocker {
            blockers.insert(blocker);
        }
        // Resources against live reservations, exactly as apply re-checks them.
        let (held, name_used) =
            self.read(|c| Ok((reservations_in(c, None)?, name_in_use(c, &request.name)?)))?;
        if name_used {
            blockers.insert("name_in_use");
        }
        let env = self.0.env.clone();
        let port = resources::plan_port_with(&held, request.game_port, |p| env.port_state(p));
        if let Some(blocker) = port.blocker {
            blockers.insert(blocker);
        }
        let memory =
            resources::plan_memory(self.0.env.memory_total_bytes(), &held, request.memory_mib);
        if let Some(blocker) = memory.blocker {
            blockers.insert(blocker);
        }
        let free = self.0.env.free_bytes(&self.0.servers);
        let needed = preview
            .as_ref()
            .and_then(|p| p.download_bytes)
            .unwrap_or(0)
            .saturating_add(pack.as_ref().map_or(0, |p| p.content_bytes))
            .saturating_add(if java.download_required {
                if source_built { JDK_RESERVE_BYTES } else { JAVA_RESERVE_BYTES }
            } else {
                0
            })
            .saturating_add(SPACE_MARGIN_BYTES);
        if free.is_some_and(|free| free < needed) {
            blockers.insert("insufficient_space");
        }

        let server_id = Uuid::new_v4();
        let now = super::super::browse::now_ms();
        let mut review = CreateReview {
            plan_id: Uuid::new_v4().to_string(),
            confirmation: None,
            expires_at: now.saturating_add(PLAN_TTL_MS),
            summary: CreateSummary {
                kind: if pack.is_some() { "modpack" } else { "new" }.into(),
                name: request.name.trim().to_owned(),
                minecraft_version: minecraft.clone(),
                software: SummarySoftware {
                    kind: software.map_or_else(|| request.software.kind.clone(), |s| s.id().into()),
                    version: preview.as_ref().map(|p| summary_version(&minecraft, p)),
                    build: preview.as_ref().and_then(|p| p.build),
                    loader_version: preview.as_ref().and_then(|p| p.loader_version.clone()),
                },
                resources: ReviewResources {
                    game_port: port.game_port,
                    memory_mib: memory.memory_mib,
                },
                runtime: CreateRuntime {
                    required_major: java.required_major,
                    download_required: java.download_required,
                    automatic: true,
                },
                destination: ReviewDestination {
                    server_id,
                    display_path: display(&self.server_dir(server_id).join("game")),
                    free_bytes: free,
                },
                pack: pack.as_ref().map(|p| p.summary.clone()),
                restore: None,
            },
            blockers: blockers.iter().map(|b| (*b).to_owned()).collect(),
            warnings: warnings.iter().map(|w| (*w).to_owned()).collect(),
        };
        if review.blockers.is_empty() {
            review.confirmation = Some(random_token()?);
        }
        let private = CreatePrivate {
            preview,
            experimental: experimental.unwrap_or(false),
            mojang_sha1_accepted,
            java: ReviewRuntime {
                required_major: java.required_major,
                selected_runtime_id: java.selected_runtime_id.clone(),
                download_required: java.download_required,
                verified: java.verified,
                jdk: source_built,
            },
            pack: pack.map(|p| p.private),
            restore: None,
            plan_hash: String::new(),
        };
        let private = CreatePrivate {
            plan_hash: create_plan_hash(binding, &review, &private)?,
            ..private
        };
        Ok((review, private))
    }

    /// Durable create job view for an owner.
    pub fn create_status(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<CreateJob> {
        self.read(|c| {
            self.owner(c, binding)?;
            job_in_kind(c, job_id, Kind::Create)
        })
    }

    /// Owner confirms the exact reviewed plan with the EULA. Re-checks name,
    /// port and memory against live reservations and reserves them with the
    /// server id in one transaction; the same key and body returns the same
    /// reply, a different body with that key conflicts. The install itself
    /// runs in [`Self::run_create_apply`].
    pub fn create_apply(
        &self,
        binding: &OwnerBinding,
        request: &CreateApplyRequest,
        idempotency_key: &str,
    ) -> JobResult<ApplyAccepted> {
        if idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || idempotency_key.chars().any(char::is_control)
        {
            return Err(JobError::INVALID);
        }
        let body_hash = hex::encode(Sha256::digest(
            serde_json::to_vec(request).map_err(|_| JobError::INVALID)?,
        ));
        let now = self.0.host.now_ms();
        self.write(|c| {
            let account = self.owner(c, binding)?;
            if let Some(accepted) =
                Self::apply_retry(c, binding, Kind::Create, idempotency_key, &body_hash)?
            {
                return Ok(accepted);
            }
            #[allow(clippy::type_complexity)]
            let row: Option<(String, String, String, String, Option<String>, Option<String>)> = c
                .query_row(
                    "SELECT job_id,state,account_id,device_id,review,private FROM machine_import_jobs WHERE plan_id=?1 AND kind='create'",
                    [&request.plan_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            let Some((job_id, state, job_account, job_device, review, private)) = row else {
                return Err(JobError::CREATE_NOT_FOUND);
            };
            if job_account != binding.account_id || job_device != binding.device_id {
                return Err(JobError::FORBIDDEN);
            }
            match state.as_str() {
                "review" => {}
                "blocked" => return Err(JobError::BLOCKED),
                _ => return Err(JobError::CONFLICT),
            }
            let review: CreateReview = decode(review)?;
            let private: CreatePrivate = decode(private)?;
            if super::super::browse::now_ms() >= review.expires_at {
                return Err(JobError::EXPIRED);
            }
            let confirmation = review.confirmation.as_deref().ok_or(JobError::BLOCKED)?;
            if !constant_eq(confirmation.as_bytes(), request.confirmation.as_bytes())
                || create_plan_hash(binding, &review, &private)? != private.plan_hash
            {
                return Err(JobError::CONFIRMATION);
            }
            let acknowledged: BTreeSet<&str> =
                request.acknowledged_warnings.iter().map(String::as_str).collect();
            let expected: BTreeSet<&str> = review.warnings.iter().map(String::as_str).collect();
            if acknowledged != expected {
                return Err(JobError::WARNINGS);
            }
            if !request.eula_accepted {
                return Err(JobError::EULA);
            }
            let summary = &review.summary;
            let server_id = summary.destination.server_id;
            let port = summary.resources.game_port.ok_or(JobError::BLOCKED)?;
            let memory = summary.resources.memory_mib.ok_or(JobError::BLOCKED)?;
            self.reserve_in(c, &job_id, &summary.name, server_id, port, memory)?;
            c.execute(
                "UPDATE machine_import_jobs SET state='applying',phase='resolving',error=NULL,server_id=?1,apply_key=?2,apply_hash=?3,eula_accepted_ms=?4,eula_account=?5,updated_ms=?4,files_done=0,files_total=0,bytes_done=0,bytes_total=0 WHERE job_id=?6",
                params![server_id.to_string(), idempotency_key, body_hash, now, account.to_string(), job_id],
            )?;
            audit(c, "create.apply", &binding.account_id, &job_id, now)?;
            if private.mojang_sha1_accepted {
                audit(c, "create.mojang_sha1_accepted", "host", &job_id, now)?;
            }
            Ok(ApplyAccepted { job_id, server_id })
        })
    }

    /// Run (or continue) an accepted create until it is registered. Blocking.
    pub fn run_create_apply(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        installer: &dyn CreateInstaller,
    ) -> JobResult<CreateJob> {
        let _running = {
            let mut running = self.0.running.lock().map_err(|_| JobError::UNAVAILABLE)?;
            if !running.insert(job_id.to_owned()) {
                return Err(JobError::CONFLICT);
            }
            Running {
                jobs: self,
                id: job_id.to_owned(),
            }
        };
        let flag = Arc::new(AtomicBool::new(false));
        self.0
            .cancels
            .lock()
            .map_err(|_| JobError::UNAVAILABLE)?
            .insert(job_id.to_owned(), flag.clone());
        #[allow(clippy::type_complexity)]
        let (phase, review, private, eula_ms, eula_account) = self.read(|c| {
            self.owner(c, binding)?;
            let row: (String, String, Option<String>, Option<String>, Option<i64>, Option<String>) =
                c.query_row(
                    "SELECT state,phase,review,private,eula_accepted_ms,eula_account FROM machine_import_jobs WHERE job_id=?1 AND kind='create'",
                    [job_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?
                .ok_or(JobError::CREATE_NOT_FOUND)?;
            if row.0 != "applying" {
                return Err(JobError::CONFLICT);
            }
            Ok((
                row.1,
                decode::<CreateReview>(row.2)?,
                decode::<CreatePrivate>(row.3)?,
                row.4.ok_or(JobError::EULA)?,
                row.5.ok_or(JobError::EULA)?,
            ))
        })?;
        let server_id = review.summary.destination.server_id;
        if Kind::Create.cancellable(&phase)
            && let Err(outcome) = if private.restore.is_some() {
                self.restore_install(job_id, &review, &private, (eula_ms, &eula_account), &flag)
            } else {
                self.create_install(
                    binding,
                    job_id,
                    &review,
                    &private,
                    (eula_ms, &eula_account),
                    installer,
                    &flag,
                )
            }
        {
            self.settle_create(job_id, server_id, outcome)?;
            return self.create_status(binding, job_id);
        }
        if let Err(error) = self.create_register(job_id, &review, &eula_account) {
            // Installed and frozen; the owner resolves the error and resumes.
            let now = self.0.host.now_ms();
            self.write(|c| {
                c.execute(
                    "UPDATE machine_import_jobs SET state='interrupted',error=?1,updated_ms=?2 WHERE job_id=?3",
                    params![error.code(), now, job_id],
                )?;
                Ok(())
            })?;
        }
        self.create_status(binding, job_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_install(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        review: &CreateReview,
        private: &CreatePrivate,
        eula: (i64, &str),
        installer: &dyn CreateInstaller,
        cancel: &AtomicBool,
    ) -> Result<(), Outcome> {
        let summary = &review.summary;
        let server_id = summary.destination.server_id;
        let port = summary
            .resources
            .game_port
            .ok_or(Outcome::Failed("plan_blocked"))?;
        let memory = summary
            .resources
            .memory_mib
            .ok_or(Outcome::Failed("plan_blocked"))?;
        let preview = private
            .preview
            .as_ref()
            .ok_or(Outcome::Failed("plan_blocked"))?;
        let stop = || cancel.load(Ordering::Acquire) || self.cancel_requested(job_id);
        self.set_phase(job_id, "applying", "resolving")?;
        self.progress(job_id, CopyProgress::default(), 0, 0)?;
        // A fresh, never-registered destination: anything left by an
        // interrupted attempt is ours and is discarded, never adopted.
        if self
            .0
            .host
            .server(server_id)
            .map_err(|_| Outcome::Interrupted("state_unavailable"))?
            .is_some()
        {
            return Err(Outcome::Failed("destination_conflict"));
        }
        self.remove_unregistered(server_id);
        let dir = self.server_dir(server_id);
        let install = dir.join("install");
        files::create_local_directory(&dir, true)
            .and_then(|()| files::create_local_directory(&install, true))
            .and_then(|()| files::create_local_directory(&dir.join("private"), true))
            .map_err(|_| Outcome::Failed("install_failed"))?;
        if stop() {
            return Err(Outcome::Cancelled);
        }

        self.set_phase(job_id, "applying", "downloading_java")?;
        let java = installer
            .ensure_java(&private.java)
            .map_err(Outcome::Failed)?;
        if stop() {
            return Err(Outcome::Cancelled);
        }

        self.set_phase(job_id, "applying", "downloading_software")?;
        self.progress(
            job_id,
            CopyProgress::default(),
            0,
            preview.download_bytes.unwrap_or(0),
        )?;
        let progress = InstallProgress {
            jobs: self,
            job_id,
            cancel,
            last: Mutex::new(None),
        };
        let selection = Selection {
            version: Some(preview.version.clone()),
            loader_version: preview.loader_version.clone(),
            build: preview.build,
            experimental: private.experimental,
        };
        let game = dir.join("game");
        let mut publish = || self.publish_game(&dir, port, eula);
        let launch = installer
            .install(
                &InstallRequest {
                    preview,
                    selection: &selection,
                    java: &java,
                    install_root: &install,
                    legacy_checksum_acknowledged: private.mojang_sha1_accepted
                        || review.warnings.iter().any(|w| w == "legacy_checksum"),
                },
                &progress,
                &mut publish,
            )
            .map_err(|code| match code {
                "cancelled" => Outcome::Cancelled,
                code => Outcome::Failed(code),
            })?;
        // The trusted launch is the installer's own result for this game tree.
        if !fs::symlink_metadata(&game).is_ok_and(|m| m.is_dir())
            || !files::same_directory(&launch.working_directory, &game).unwrap_or(false)
        {
            return Err(Outcome::Failed("install_failed"));
        }
        let heap = installer.heap_args(memory);
        let document = crate::host_software::managed_launch_document(&launch, &heap, &dir)
            .map_err(|_| Outcome::Failed("install_failed"))?;
        commit::write_atomic(&dir.join("private/launch.toml"), document.as_bytes())
            .map_err(|_| Outcome::Failed("install_failed"))?;
        let mut pack = serde_json::json!({
            "name": summary.name, "version": "initial", "revision": 1, "minimum-revision": 1,
            "minecraft-version": preview.version,
        });
        if let Some(loader) = &preview.loader_version
            && matches!(
                preview.software,
                Software::Fabric | Software::Quilt | Software::Forge | Software::NeoForge
            )
        {
            pack["loader"] = serde_json::json!({"kind": preview.software.id(), "version": loader});
        }
        let value = serde_json::json!({
            "schema-version": 1,
            "paths": {"state": "private/state", "game": "game", "scan-roots": ["game"], "launch-definition": "private/launch.toml"},
            "pack": pack,
            "pack-access": {"mode": "minecraft"},
            "listener": {"mode": "disabled"},
        });
        let host_toml = crate::onboarding::provisioning::host_document_toml(value, &dir)
            .map_err(|_| Outcome::Failed("install_failed"))?;
        commit::write_atomic(&dir.join("host.toml"), host_toml.as_bytes())
            .map_err(|_| Outcome::Failed("install_failed"))?;
        if stop() {
            return Err(Outcome::Cancelled);
        }
        if let Some(pack) = &private.pack {
            let installed = crate::machine::creates::CreatedSoftware {
                software: preview.software,
                version: preview.version.clone(),
                build: preview.build,
                loader_version: preview.loader_version.clone(),
                artifacts: launch.artifacts.clone(),
            };
            self.create_pack_content(
                binding, job_id, server_id, &dir, pack, &installed, installer, &progress,
            )?;
        }
        // Last safe boundary: after this the server is registered.
        if stop() {
            return Err(Outcome::Cancelled);
        }
        Ok(())
    }

    /// Install the reviewed pack's content into the still unregistered server
    /// through the server's own pack engine (see [`CreateInstaller::install_pack`]).
    #[allow(clippy::too_many_arguments)]
    fn create_pack_content(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        server_id: Uuid,
        dir: &Path,
        pack: &super::modpack::PackPrivate,
        installed: &crate::machine::creates::CreatedSoftware,
        installer: &dyn CreateInstaller,
        progress: &dyn CreateProgress,
    ) -> Result<(), Outcome> {
        let review = pack.review.as_ref().ok_or(Outcome::Failed("plan_blocked"))?;
        let upload = match &pack.source {
            super::modpack::PackSourceRequest::Upload { upload_id } => {
                let (path, sha) = self
                    .read(|c| self.create_upload_in(c, &binding.account_id, upload_id))
                    .map_err(|_| Outcome::Failed("upload_unavailable"))?;
                if sha != review.source_digest {
                    return Err(Outcome::Failed("integrity_conflict"));
                }
                Some(path)
            }
            super::modpack::PackSourceRequest::Provider { .. } => None,
        };
        self.set_phase(job_id, "applying", "downloading_software")?;
        self.progress(job_id, CopyProgress::default(), 0, 0)?;
        let policy = commit::owner_policy(&self.0.host, server_id)
            .map_err(|_| Outcome::Interrupted("state_unavailable"))?;
        let capabilities = policy
            .grants
            .iter()
            .find(|g| g.account_id.to_string() == binding.account_id)
            .map(|g| g.capabilities.clone())
            .ok_or(Outcome::Failed("install_failed"))?;
        let (server_subject, subject) = self
            .subject(binding, server_id, capabilities)
            .map_err(|_| Outcome::Failed("install_failed"))?;
        let host_id = Uuid::parse_str(&binding.host_id).map_err(|_| Outcome::Failed("install_failed"))?;
        installer
            .install_pack(
                &crate::machine::creates::PackInstallRequest {
                    server_id,
                    host_id,
                    server_dir: dir,
                    policy,
                    server_subject,
                    subject,
                    source: &pack.source,
                    upload: upload.as_deref(),
                    review,
                    installed,
                },
                progress,
            )
            .map_err(|code| match code {
                "cancelled" => Outcome::Cancelled,
                code => Outcome::Failed(code),
            })
    }

    /// Stage the new game directory (owner EULA record, reserved port with RCON
    /// and query off) and publish it as `game` with one rename.
    fn publish_game(
        &self,
        dir: &Path,
        port: u16,
        eula: (i64, &str),
    ) -> Result<PathBuf, &'static str> {
        let staging = dir.join("staging");
        let game = dir.join("game");
        if fs::symlink_metadata(&game).is_ok() {
            return Err("destination_conflict");
        }
        let _ = fs::remove_dir_all(&staging);
        files::create_local_directory(&staging, true).map_err(|_| "install_failed")?;
        let (ms, account) = eula;
        commit::write_atomic(
            &staging.join("eula.txt"),
            format!(
                "# Accepted by Vesta Server owner {account} at {ms} (https://aka.ms/MinecraftEULA)\neula=true\n"
            )
            .as_bytes(),
        )
        .map_err(|_| "install_failed")?;
        commit::write_atomic(
            &staging.join("server.properties"),
            commit::rewrite_properties("", port).as_bytes(),
        )
        .map_err(|_| "install_failed")?;
        fs::rename(&staging, &game).map_err(|_| "install_failed")?;
        Ok(game)
    }

    fn create_register(
        &self,
        job_id: &str,
        review: &CreateReview,
        eula_account: &str,
    ) -> JobResult<()> {
        let summary = &review.summary;
        let server_id = summary.destination.server_id;
        let dir = self.server_dir(server_id);
        if !dir.join("host.toml").is_file() || !dir.join("private/launch.toml").is_file() {
            return Err(JobError("install_failed"));
        }
        self.set_phase(job_id, "applying", "registering")?;
        let port = summary.resources.game_port.ok_or(JobError::BLOCKED)?;
        self.register_once(job_id, server_id, summary.name.clone(), port, eula_account)?;
        let now = self.0.host.now_ms();
        self.write(|c| {
            c.execute(
                "UPDATE machine_import_jobs SET state='ready',phase='registering',error=NULL,updated_ms=?1 WHERE job_id=?2",
                params![now, job_id],
            )?;
            audit(c, "create.registered", eula_account, job_id, now)?;
            Ok(())
        })
    }

    /// Record a non-success install outcome. Nothing is registered yet, so the
    /// destination is removed and the reservation released, except for an
    /// interruption that the owner resumes.
    fn settle_create(&self, job_id: &str, server_id: Uuid, outcome: Outcome) -> JobResult<()> {
        let now = self.0.host.now_ms();
        let (state, error, release) = match outcome {
            Outcome::Cancelled => ("cancelled", None, true),
            Outcome::Blocked(code) => ("blocked", Some(code), true),
            Outcome::SourceChangedDuringCopy => ("failed", Some("install_failed"), true),
            Outcome::Failed(code) => ("failed", Some(code), true),
            Outcome::Interrupted(code) => ("interrupted", Some(code), false),
        };
        if release {
            self.remove_unregistered(server_id);
        }
        self.write(|c| {
            c.execute(
                "UPDATE machine_import_jobs SET state=?1,error=?2,updated_ms=?3 WHERE job_id=?4",
                params![state, error, now, job_id],
            )?;
            if release {
                c.execute(
                    "UPDATE machine_import_reservations SET released=1 WHERE job_id=?1",
                    [job_id],
                )?;
            }
            audit(c, &format!("create.{state}"), "host", job_id, now)?;
            Ok(())
        })
    }

    pub fn create_cancel(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<CancelResponse> {
        self.cancel_kind(binding, job_id, Kind::Create)
    }
    pub fn create_resume(&self, binding: &OwnerBinding, job_id: &str) -> JobResult<CreateJob> {
        self.resume_kind(binding, job_id, Kind::Create)?;
        self.create_status(binding, job_id)
    }
    pub fn create_start(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        worker: &WorkerHandle,
    ) -> JobResult<CreateJob> {
        self.start_kind(binding, job_id, worker, Kind::Create)?;
        self.create_status(binding, job_id)
    }
    pub fn create_readiness(
        &self,
        binding: &OwnerBinding,
        job_id: &str,
        worker: &WorkerHandle,
    ) -> JobResult<CreateJob> {
        self.readiness_kind(binding, job_id, worker, Kind::Create)?;
        self.create_status(binding, job_id)
    }

    /// Software and Minecraft version of every server a job committed, for the
    /// owner's server list. Servers registered otherwise are absent (unknown).
    pub fn server_facts(&self) -> JobResult<HashMap<Uuid, ServerFacts>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT kind,server_id,review FROM machine_import_jobs WHERE server_id IS NOT NULL AND review IS NOT NULL AND state IN ('ready','starting','online','interrupted')",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut facts = HashMap::new();
            for (kind, server, review) in rows {
                let Ok(server) = Uuid::parse_str(&server) else {
                    continue;
                };
                let fact = if kind == "create" {
                    let Ok(review) = serde_json::from_str::<CreateReview>(&review) else {
                        continue;
                    };
                    let s = review.summary;
                    // A restore of a backup whose facts were unknown says so.
                    ServerFacts {
                        software: (s.software.kind != "unknown")
                            .then_some((s.software.kind, s.software.version)),
                        minecraft_version: Some(s.minecraft_version).filter(|v| !v.is_empty()),
                    }
                } else {
                    let Ok(review) = serde_json::from_str::<Review>(&review) else {
                        continue;
                    };
                    let version = imported_software_version(&review.detected);
                    ServerFacts {
                        software: review.detected.loader.map(|l| (l, version)),
                        minecraft_version: review.detected.minecraft_version,
                    }
                };
                facts.insert(server, fact);
            }
            Ok(facts)
        })
    }
}

#[cfg(test)]
mod imported_version_tests {
    use super::*;

    fn detected(loader: &str, minecraft: Option<&str>, launch: Option<&str>) -> ReviewDetected {
        ReviewDetected {
            kind: "minecraft".into(),
            instance_name: None,
            minecraft_version: minecraft.map(Into::into),
            loader: Some(loader.into()),
            launch: launch.map(|path| LaunchChoice {
                kind: if path.ends_with(".txt") {
                    "java_argfile"
                } else {
                    "jar"
                }
                .into(),
                relative_path: path.into(),
            }),
        }
    }

    #[test]
    fn imported_servers_report_the_software_version_their_facts_show() {
        let mc = Some("1.21.1");
        for (loader, minecraft, launch, expected) in [
            ("vanilla", mc, Some("server.jar"), Some("1.21.1")),
            ("vanilla", None, Some("server.jar"), None),
            ("paper", mc, Some("paper-1.21.1-130.jar"), Some("130")),
            ("purpur", mc, Some("purpur-1.21.1-2329.jar"), Some("2329")),
            // Jar and detected Minecraft version disagree: unknown.
            ("paper", mc, Some("paper-1.20.4-499.jar"), None),
            ("paper", mc, Some("paper.jar"), None),
            (
                "fabric",
                mc,
                Some("fabric-server-mc.1.21.1-loader.0.16.10-launcher.1.0.1.jar"),
                Some("0.16.10"),
            ),
            ("fabric", mc, Some("fabric-server-launch.jar"), None),
            (
                "forge",
                Some("1.20.1"),
                Some("libraries/net/minecraftforge/forge/1.20.1-47.3.0/unix_args.txt"),
                Some("47.3.0"),
            ),
            (
                "forge",
                Some("1.12.2"),
                Some("forge-1.12.2-14.23.5.2860.jar"),
                Some("14.23.5.2860"),
            ),
            (
                "neoforge",
                mc,
                Some("libraries/net/neoforged/neoforge/21.1.77/win_args.txt"),
                Some("21.1.77"),
            ),
            (
                "neoforge",
                Some("1.20.1"),
                Some("libraries/net/neoforged/forge/1.20.1-47.1.106/unix_args.txt"),
                Some("1.20.1-47.1.106"),
            ),
            ("quilt", mc, Some("quilt-server-launch.jar"), None),
            ("forge", mc, None, None),
        ] {
            assert_eq!(
                imported_software_version(&detected(loader, minecraft, launch)).as_deref(),
                expected,
                "{loader} {launch:?}"
            );
        }
        let mut none = detected("vanilla", mc, None);
        none.loader = None;
        assert_eq!(imported_software_version(&none), None);
    }
}
