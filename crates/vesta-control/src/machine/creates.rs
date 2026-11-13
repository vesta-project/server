//! Machine-level creation metadata. Catalogue access never needs a worker or
//! executes a runtime. Installer plans must independently resolve fresh evidence.
use super::{
    HostState,
    imports::jobs::{ImportJobs, JobError, JobResult},
};
use serde_json::{Value, json};
use std::path::Path;
use vesta_server_install::{
    InstallRoots, MinecraftVersion, ServerInstaller, SilentProgressReporter, Software,
    SoftwareRelease, TransportConfig,
};

/// Narrow deterministic seam for host catalogue/router tests.
pub trait Catalogue: Send + Sync {
    fn minecraft(&self) -> JobResult<Vec<MinecraftVersion>>;
    fn releases(&self, kind: Software, minecraft: &str) -> JobResult<Vec<SoftwareRelease>>;
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
        runtime()?
            .block_on(self.0.releases(kind, minecraft, &SilentProgressReporter))
            .map_err(|_| JobError("provider_unavailable"))
    }
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
    let (versions, releases) = std::thread::scope(|scope| {
        let minecraft = scope.spawn(|| catalogue.minecraft());
        let pending: Vec<_> = Software::ALL
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    scope.spawn(move || {
                        if kind == Software::Vanilla {
                            vec![SoftwareRelease {
                                id: version.to_owned(),
                                build: None,
                                loader_version: None,
                            }]
                        } else {
                            catalogue.releases(kind, version).unwrap_or_default()
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
            let builds: Vec<_> = releases.into_iter().enumerate().map(|(i, b)| json!({
            "id":b.id,"build":b.build,"loaderVersion":b.loader_version,
            "stable":!experimental,"experimental":experimental,"recommended":i==0 && !experimental
        })).collect();
            json!({"kind":kind.id(),"label":label(kind),"stable":!experimental,
            "experimental":experimental,"recommended":kind==Software::Vanilla && !experimental,
            "available":!builds.is_empty(),"builds":builds})
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
