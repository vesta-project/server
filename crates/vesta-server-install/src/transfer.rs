use super::model::*;
use crate::{progress::ProgressReporter, TransportConfig};
use futures::Future;
use sha1::Sha1;
use sha2::{Digest as _, Sha256};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    fs::File,
    io::{AsyncSeekExt, AsyncWriteExt},
    time::Instant,
};
use url::Url;

pub(crate) struct Source<'a> {
    pub url: &'a str,
    pub software: Software,
}

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Metadata,
    Artifact,
}

pub(crate) struct Job {
    pub deadline: Instant,
    pub bytes: u64,
    pub limits: Limits,
    pub catalogue: bool,
    pub fresh_until: Instant,
    pub pending_cache: Vec<(String, crate::catalogue_cache::CatalogueRecord)>,
    pending_bytes: usize,
}
impl Job {
    pub fn new(limits: Limits, bytes: u64) -> Self {
        Self {
            deadline: Instant::now() + limits.duration(),
            bytes,
            limits,
            catalogue: false,
            fresh_until: Instant::now() + Duration::from_secs(300),
            pending_cache: Vec::new(),
            pending_bytes: 0,
        }
    }
    pub fn cache_candidate(&mut self, key: String, bytes: &[u8], fresh_for: Duration) {
        self.fresh_until = self.fresh_until.min(Instant::now() + fresh_for);
        if self.pending_bytes.saturating_add(bytes.len())
            > crate::catalogue_cache::CATALOGUE_CACHE_MEMORY_BYTES
        {
            return;
        }
        if let Some(record) = crate::catalogue_cache::record(bytes.to_vec(), fresh_for) {
            self.pending_bytes += record.bytes.len();
            self.pending_cache.push((key, record));
        }
    }
    pub fn charge(&mut self, n: usize) -> Result<()> {
        let next = self
            .bytes
            .checked_add(n as u64)
            .ok_or(InstallError::LimitExceeded)?;
        if next > self.limits.total_bytes {
            return Err(InstallError::LimitExceeded);
        }
        self.bytes = next;
        Ok(())
    }
    pub async fn ready(&self, reporter: &dyn ProgressReporter) -> Result<()> {
        loop {
            if reporter.is_cancelled() {
                return Err(InstallError::Cancelled);
            }
            if Instant::now() >= self.deadline {
                return Err(InstallError::Deadline);
            }
            if !reporter.is_paused() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

pub(crate) async fn controlled<T>(
    future: impl Future<Output = Result<T>>,
    deadline: Instant,
    reporter: &dyn ProgressReporter,
) -> Result<T> {
    tokio::pin!(future);
    loop {
        if reporter.is_cancelled() {
            return Err(InstallError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(InstallError::Deadline);
        }
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_millis(25))) => {}
        }
    }
}

#[derive(Clone)]
pub(crate) struct Transport {
    proxy: Option<String>,
    pub policy_id: String,
    pub catalogues: std::sync::Arc<crate::catalogue_cache::CatalogueCache>,
    #[cfg(test)]
    pub fixture_origin: Option<Url>,
}
impl Transport {
    pub fn new(config: TransportConfig) -> Self {
        Self {
            proxy: config.into_proxy(),
            policy_id: uuid::Uuid::new_v4().simple().to_string(),
            catalogues: crate::catalogue_cache::CatalogueCache::new(None),
            #[cfg(test)]
            fixture_origin: None,
        }
    }
    pub fn uses_proxy(&self) -> bool {
        self.proxy.is_some()
    }
    pub fn catalogue_key(&self, selection: &str) -> String {
        // Stable across restarts, partitioned by transport credentials/policy.
        // Approval policy_id remains independently random and never cache-derived.
        #[cfg(test)]
        let origin = self
            .fixture_origin
            .as_ref()
            .map(|url| url.as_str())
            .unwrap_or("");
        #[cfg(not(test))]
        let origin = "";
        let mut digest = Sha256::new();
        for part in [
            "vesta.catalogue.v1",
            selection,
            self.proxy.as_deref().unwrap_or(""),
            origin,
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }

    async fn request(
        &self,
        logical: &Url,
        head: bool,
        limits: Limits,
        reporter: &dyn ProgressReporter,
        deadline: Instant,
    ) -> Result<reqwest::Response> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_millis(limits.connect_ms))
            .user_agent(concat!(
                "VestaServerInstall/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/Vesta-Project/launcher)"
            ));
        let target = logical.clone();
        #[cfg(test)]
        let target = if let Some(base) = &self.fixture_origin {
            base.join(logical.path())
                .map_err(|_| InstallError::SourceRefused)?
        } else {
            target
        };
        #[cfg(test)]
        let fixture = self.fixture_origin.is_some();
        #[cfg(not(test))]
        let fixture = false;
        if !fixture {
            builder = builder.https_only(true);
            if let Some(proxy) = &self.proxy {
                builder = builder
                    .proxy(reqwest::Proxy::all(proxy).map_err(|_| InstallError::TransportFailed)?);
            } else {
                let host = logical.host_str().ok_or(InstallError::SourceRefused)?;
                let addresses: Vec<SocketAddr> = controlled(
                    async {
                        let values: Vec<_> = tokio::net::lookup_host((host, 443))
                            .await
                            .map_err(|_| InstallError::TransportFailed)?
                            .collect();
                        if values.is_empty() || values.iter().any(|v| !public_ip(v.ip())) {
                            return Err(InstallError::SourceRefused);
                        }
                        Ok(values)
                    },
                    deadline,
                    reporter,
                )
                .await?;
                builder = builder.resolve_to_addrs(host, &addresses);
            }
        }
        let client = builder.build().map_err(|_| InstallError::TransportFailed)?;
        controlled(
            async {
                tokio::time::timeout(
                    Duration::from_millis(limits.idle_ms),
                    client
                        .request(
                            if head {
                                reqwest::Method::HEAD
                            } else {
                                reqwest::Method::GET
                            },
                            target,
                        )
                        .header(reqwest::header::ACCEPT_ENCODING, "identity")
                        .send(),
                )
                .await
                .map_err(|_| InstallError::TransportFailed)?
                .map_err(|_| InstallError::TransportFailed)
            },
            deadline,
            reporter,
        )
        .await
    }

    pub async fn receive(
        &self,
        source: Source<'_>,
        role: Role,
        pin: Option<&ArtifactPin>,
        job: &mut Job,
        reporter: &dyn ProgressReporter,
        file: Option<&mut File>,
    ) -> Result<Received> {
        let Source { url, software } = source;
        self.receive_policy(
            url, software, role, pin, job, reporter, file, None, None, false,
        )
        .await
    }
    /// Family-bound requests share the caller's byte budget and absolute deadline.
    pub async fn family_metadata(
        &self,
        url: &str,
        family: SourceFamily,
        cap: Option<u64>,
        job: &mut Job,
        reporter: &dyn ProgressReporter,
    ) -> Result<Received> {
        self.receive_policy(
            url,
            Software::Fabric,
            Role::Metadata,
            None,
            job,
            reporter,
            None,
            Some(family),
            cap,
            false,
        )
        .await
    }
    pub async fn family_artifact(
        &self,
        pin: &ArtifactPin,
        family: SourceFamily,
        job: &mut Job,
        reporter: &dyn ProgressReporter,
        file: &mut File,
    ) -> Result<Received> {
        self.receive_policy(
            &pin.url,
            Software::Fabric,
            Role::Artifact,
            Some(pin),
            job,
            reporter,
            Some(file),
            Some(family),
            None,
            false,
        )
        .await
    }
    pub async fn head_size(
        &self,
        url: &str,
        family: SourceFamily,
        job: &mut Job,
        reporter: &dyn ProgressReporter,
    ) -> Result<u64> {
        Ok(self
            .receive_policy(
                url,
                Software::Fabric,
                Role::Metadata,
                None,
                job,
                reporter,
                None,
                Some(family),
                Some(job.limits.artifact_bytes),
                true,
            )
            .await?
            .size)
    }
    #[allow(clippy::too_many_arguments)]
    async fn receive_policy(
        &self,
        url: &str,
        software: Software,
        role: Role,
        pin: Option<&ArtifactPin>,
        job: &mut Job,
        reporter: &dyn ProgressReporter,
        mut file: Option<&mut File>,
        family: Option<SourceFamily>,
        cap_override: Option<u64>,
        head: bool,
    ) -> Result<Received> {
        match (role, pin, file.is_some()) {
            (Role::Artifact, Some(p), true) if p.url == url => p.digest.validate()?,
            (Role::Metadata, None, false) => {}
            _ => return Err(InstallError::InvalidInput),
        }
        let initial = match family {
            Some(f) => validate_family_url(url, f, role, head)?,
            None => validate_url(url, software, role)?,
        };
        let cap = cap_override.unwrap_or(if matches!(role, Role::Artifact) {
            job.limits.artifact_bytes
        } else {
            job.limits.metadata_bytes
        });
        if pin.and_then(|p| p.size).is_some_and(|s| s == 0 || s > cap) {
            return Err(InstallError::LimitExceeded);
        }
        let cache_key = (job.catalogue && matches!(role, Role::Metadata) && !head).then(|| {
            self.catalogue_key(&format!("raw:{software:?}:{family:?}:{}", initial.as_str()))
        });
        if let Some(key) = &cache_key {
            let cached = controlled(
                async { Ok(self.catalogues.get(key).await) },
                job.deadline,
                reporter,
            )
            .await?;
            if let Some(cached) = cached {
                if cached.bytes.len() as u64 > cap {
                    return Err(InstallError::LimitExceeded);
                }
                job.charge(cached.bytes.len())?;
                let fresh_for = crate::catalogue_cache::remaining(&cached);
                job.fresh_until = job.fresh_until.min(Instant::now() + fresh_for);
                return Ok(Received {
                    data: cached.bytes.clone(),
                    size: cached.bytes.len() as u64,
                    observed_sha256: cached.sha256.clone(),
                    fresh_for,
                });
            }
        }
        for attempt in 0..job.limits.attempts {
            job.ready(reporter).await?;
            if let Some(f) = file.as_deref_mut() {
                f.set_len(0)
                    .await
                    .map_err(|_| InstallError::PublishFailed)?;
                f.seek(std::io::SeekFrom::Start(0))
                    .await
                    .map_err(|_| InstallError::PublishFailed)?;
            }
            let result = self
                .attempt(
                    initial.clone(),
                    software,
                    role,
                    pin,
                    cap,
                    job,
                    reporter,
                    file.as_deref_mut(),
                    family,
                    head,
                )
                .await;
            match result {
                Ok(value) => {
                    if let Some(key) = cache_key.clone() {
                        job.cache_candidate(key, &value.data, value.fresh_for);
                    }
                    return Ok(value);
                }
                Err(InstallError::TransportFailed) if attempt + 1 < job.limits.attempts => {
                    controlled(
                        async {
                            tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1)))
                                .await;
                            Ok(())
                        },
                        job.deadline,
                        reporter,
                    )
                    .await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(InstallError::TransportFailed)
    }

    #[allow(clippy::too_many_arguments)]
    async fn attempt(
        &self,
        mut url: Url,
        software: Software,
        role: Role,
        pin: Option<&ArtifactPin>,
        cap: u64,
        job: &mut Job,
        reporter: &dyn ProgressReporter,
        mut file: Option<&mut File>,
        family: Option<SourceFamily>,
        head: bool,
    ) -> Result<Received> {
        let mut response = None;
        for hop in 0..=job.limits.redirects {
            job.ready(reporter).await?;
            let r = self
                .request(&url, head, job.limits, reporter, job.deadline)
                .await?;
            if matches!(r.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                if hop == job.limits.redirects {
                    return Err(InstallError::SourceRefused);
                }
                let location = r
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .ok_or(InstallError::SourceRefused)?;
                if location.len() > 2048 || location.chars().any(char::is_control) {
                    return Err(InstallError::SourceRefused);
                }
                let next = url
                    .join(location)
                    .map_err(|_| InstallError::SourceRefused)?;
                url = match family {
                    Some(f) => validate_family_url(next.as_str(), f, role, head)?,
                    None => validate_url(next.as_str(), software, role)?,
                };
            } else {
                response = Some(r);
                break;
            }
        }
        let mut r = response.ok_or(InstallError::SourceRefused)?;
        let freshness_start = Instant::now();
        let fresh_for = crate::catalogue_cache::freshness(r.headers());
        if r.status().as_u16() == 429 || r.status().is_server_error() {
            return Err(InstallError::TransportFailed);
        }
        if r.status() != reqwest::StatusCode::OK {
            return Err(InstallError::ArtifactChanged);
        }
        if r.headers().contains_key(reqwest::header::CONTENT_ENCODING)
            || r.headers().contains_key(reqwest::header::CONTENT_RANGE)
        {
            return Err(InstallError::SourceRefused);
        }
        let length = if head {
            // Response::content_length reports decoded body size (zero for HEAD).
            let mut values = r.headers().get_all(reqwest::header::CONTENT_LENGTH).iter();
            let raw = values
                .next()
                .and_then(|v| v.to_str().ok())
                .ok_or(InstallError::InvalidMetadata)?;
            if values.next().is_some()
                || raw.is_empty()
                || raw.len() > 20
                || !raw.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(InstallError::InvalidMetadata);
            }
            Some(
                raw.parse::<u64>()
                    .map_err(|_| InstallError::InvalidMetadata)?,
            )
        } else {
            r.content_length()
        };
        if length.is_some_and(|n| n > cap) {
            return Err(InstallError::LimitExceeded);
        }
        if let (Some(expected), Some(actual)) = (pin.and_then(|p| p.size), length) {
            if expected != actual {
                return Err(InstallError::ArtifactChanged);
            }
        }
        if head {
            let size = length.ok_or(InstallError::InvalidMetadata)?;
            if size == 0 {
                return Err(InstallError::InvalidMetadata);
            }
            return Ok(Received {
                data: Vec::new(),
                size,
                observed_sha256: String::new(),
                fresh_for: Duration::ZERO,
            });
        }
        let mut strong = Sha256::new();
        let mut legacy = Sha1::new();
        let mut weak = md5::Md5::new();
        let mut size = 0u64;
        let mut data = Vec::new();
        loop {
            job.ready(reporter).await?;
            let chunk = controlled(
                async {
                    tokio::time::timeout(Duration::from_millis(job.limits.idle_ms), r.chunk())
                        .await
                        .map_err(|_| InstallError::TransportFailed)?
                        .map_err(|_| InstallError::TransportFailed)
                },
                job.deadline,
                reporter,
            )
            .await?;
            let Some(chunk) = chunk else { break };
            // Count consumed bytes even when this chunk is refused before any disk write.
            job.charge(chunk.len())?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or(InstallError::LimitExceeded)?;
            if size > cap {
                return Err(InstallError::LimitExceeded);
            }
            if pin.and_then(|p| p.size).is_some_and(|n| size > n) {
                return Err(InstallError::ArtifactChanged);
            }
            strong.update(&chunk);
            legacy.update(&chunk);
            weak.update(&chunk);
            if let Some(f) = file.as_deref_mut() {
                controlled(
                    async {
                        f.write_all(&chunk)
                            .await
                            .map_err(|_| InstallError::PublishFailed)
                    },
                    job.deadline,
                    reporter,
                )
                .await?;
            } else {
                data.extend_from_slice(&chunk);
            }
            reporter.update_bytes(size, pin.and_then(|p| p.size).or(length));
        }
        let observed_sha256 = format!("{:x}", strong.finalize());
        if let Some(p) = pin {
            if size == 0 {
                return Err(InstallError::ArtifactChanged);
            }
            let digest = match p.digest.algorithm {
                DigestAlgorithm::Sha256 => observed_sha256.clone(),
                DigestAlgorithm::Sha1 => format!("{:x}", legacy.finalize()),
                DigestAlgorithm::Md5 => format!("{:x}", weak.finalize()),
            };
            if p.size.is_some_and(|n| n != size) || digest != p.digest.hex {
                return Err(InstallError::ArtifactChanged);
            }
        }
        if let Some(f) = file {
            controlled(
                async {
                    f.flush().await.map_err(|_| InstallError::PublishFailed)?;
                    f.sync_all().await.map_err(|_| InstallError::PublishFailed)
                },
                job.deadline,
                reporter,
            )
            .await?;
        }
        Ok(Received {
            data,
            size,
            observed_sha256,
            fresh_for: fresh_for.saturating_sub(freshness_start.elapsed()),
        })
    }
}

pub(crate) struct Received {
    pub data: Vec<u8>,
    pub size: u64,
    pub observed_sha256: String,
    pub fresh_for: Duration,
}

pub(crate) fn validate_url(raw: &str, software: Software, role: Role) -> Result<Url> {
    if raw.len() > 2048
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.contains('\\')
        || raw.contains('%')
    {
        return Err(InstallError::SourceRefused);
    }
    let u = Url::parse(raw).map_err(|_| InstallError::SourceRefused)?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.query().is_some()
        || u.port_or_known_default() != Some(443)
    {
        return Err(InstallError::SourceRefused);
    }
    let (host, prefix) = match (software, role) {
        (Software::Vanilla, Role::Metadata) => ("launcher-meta.modrinth.com", "/minecraft/v0/"),
        (Software::Vanilla, Role::Artifact) => ("piston-data.mojang.com", "/v1/objects/"),
        (Software::Paper | Software::Folia, Role::Metadata) => ("fill.papermc.io", "/v3/projects/"),
        (Software::Paper | Software::Folia, Role::Artifact) => {
            ("fill-data.papermc.io", "/v1/objects/")
        }
        (Software::Purpur, _) => ("api.purpurmc.org", "/v2/purpur/"),
        _ => return Err(InstallError::Unavailable),
    };
    let catalogue_root =
        software == Software::Purpur && matches!(role, Role::Metadata) && u.path() == "/v2/purpur";
    if u.host_str() != Some(host)
        || (!u.path().starts_with(prefix) && !catalogue_root)
        || u.path().contains('%')
    {
        return Err(InstallError::SourceRefused);
    }
    Ok(u)
}

pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !v.is_private()
                && !v.is_loopback()
                && !v.is_link_local()
                && !v.is_multicast()
                && !v.is_broadcast()
                && !v.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (18..=19).contains(&b))
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

/// Validate the raw spelling before URL normalization, then the compiled family.
pub(crate) fn validate_family_url(
    raw: &str,
    family: SourceFamily,
    role: Role,
    head: bool,
) -> Result<Url> {
    if raw.len() > 2048
        || !raw.is_ascii()
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.bytes().any(|b| b == 92 || b == 37)
    {
        return Err(InstallError::SourceRefused);
    }
    let u = Url::parse(raw).map_err(|_| InstallError::SourceRefused)?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.query().is_some()
        || u.port_or_known_default() != Some(443)
    {
        return Err(InstallError::SourceRefused);
    }
    if matches!(
        family,
        SourceFamily::ForgeMaven
            | SourceFamily::NeoForgeMaven
            | SourceFamily::MinecraftLibraries
            | SourceFamily::ModrinthNeoForge
    ) {
        let (host, prefix) = match family {
            SourceFamily::ForgeMaven => ("maven.minecraftforge.net", "/"),
            SourceFamily::NeoForgeMaven => ("maven.neoforged.net", "/releases/"),
            SourceFamily::MinecraftLibraries => ("libraries.minecraft.net", "/"),
            _ => ("launcher-meta.modrinth.com", "/neo/v0/"),
        };
        if u.host_str() != Some(host) || !u.path().starts_with(prefix) {
            return Err(InstallError::SourceRefused);
        }
        let path = u
            .path()
            .strip_prefix(prefix)
            .ok_or(InstallError::SourceRefused)?;
        super::paths::relative(path, |_, _| true).map_err(|_| InstallError::SourceRefused)?;
        let artifact = path.ends_with(".jar") || path.ends_with(".zip") || path.ends_with(".txt");
        let metadata = artifact
            || path.ends_with(".jar.sha1")
            || path.ends_with("maven-metadata.xml")
            || family == SourceFamily::ModrinthNeoForge && path == "manifest.json";
        if matches!(role, Role::Artifact) && !artifact
            || matches!(role, Role::Metadata) && !metadata
        {
            return Err(InstallError::SourceRefused);
        }
        return Ok(u);
    }
    if family == SourceFamily::ModrinthForge {
        return if matches!(role, Role::Metadata)
            && !head
            && u.host_str() == Some("launcher-meta.modrinth.com")
            && u.path() == "/forge/v0/manifest.json"
            && !raw
                .split('/')
                .any(|part| part == "." || part.contains(".."))
        {
            Ok(u)
        } else {
            Err(InstallError::SourceRefused)
        };
    }
    let (host, prefix) = match family {
        SourceFamily::ForgeMaven
        | SourceFamily::NeoForgeMaven
        | SourceFamily::MinecraftLibraries
        | SourceFamily::ModrinthNeoForge => return Err(InstallError::SourceRefused),
        SourceFamily::ModrinthForge => return Err(InstallError::SourceRefused),
        SourceFamily::MojangObjects => ("piston-data.mojang.com", "/v1/objects/"),
        SourceFamily::ModrinthFabric => ("launcher-meta.modrinth.com", "/fabric/v0/"),
        SourceFamily::ModrinthQuilt => ("launcher-meta.modrinth.com", "/quilt/v1/"),
        SourceFamily::FabricMeta => ("meta.fabricmc.net", "/v2/versions/loader/"),
        SourceFamily::FabricMaven => ("maven.fabricmc.net", "/"),
        SourceFamily::QuiltMeta => ("meta.quiltmc.org", "/v3/versions/loader/"),
        SourceFamily::QuiltMaven => ("maven.quiltmc.org", "/repository/release/"),
    };
    if u.host_str() != Some(host)
        || !u.path().starts_with(prefix)
        || raw
            .split('/')
            .any(|p| p == "." || p == ".." || p.contains(".."))
    {
        return Err(InstallError::SourceRefused);
    }
    if matches!(
        family,
        SourceFamily::ModrinthFabric | SourceFamily::ModrinthQuilt
    ) {
        let suffix = u
            .path()
            .strip_prefix(prefix)
            .ok_or(InstallError::SourceRefused)?;
        if suffix != "manifest.json"
            && !(family == SourceFamily::ModrinthQuilt && {
                let parts: Vec<_> = suffix.split('/').collect();
                parts.len() == 4
                    && parts[0] == "version-group"
                    && identifier(parts[1])
                    && parts[2] == "loader-version"
                    && loader_identifier(parts[3])
            })
            && !suffix
                .strip_prefix("versions/")
                .and_then(|s| s.strip_suffix(".json"))
                .is_some_and(loader_identifier)
        {
            return Err(InstallError::SourceRefused);
        }
    }
    if matches!(family, SourceFamily::FabricMeta | SourceFamily::QuiltMeta) {
        let suffix = u
            .path()
            .strip_prefix(prefix)
            .ok_or(InstallError::SourceRefused)?;
        let parts: Vec<_> = suffix.split('/').collect();
        if parts.len() < 2
            || !identifier(parts[0])
            || !loader_identifier(parts[1])
            || !(parts.len() == 4 && parts[2..] == ["server", "json"]
                || family == SourceFamily::QuiltMeta && parts.len() == 2)
        {
            return Err(InstallError::SourceRefused);
        }
    }
    let maven = matches!(family, SourceFamily::FabricMaven | SourceFamily::QuiltMaven);
    let objects = family == SourceFamily::MojangObjects;
    match role {
        Role::Artifact
            if !(maven || objects)
                || !(u.path().ends_with(".jar") || objects && u.path().ends_with(".txt")) =>
        {
            return Err(InstallError::SourceRefused)
        }
        Role::Metadata if head && !maven => return Err(InstallError::SourceRefused),
        Role::Metadata if objects => return Err(InstallError::SourceRefused),
        Role::Metadata
            if maven
                && !(if head {
                    u.path().ends_with(".jar")
                } else {
                    u.path().ends_with(".jar.sha256")
                }) =>
        {
            return Err(InstallError::SourceRefused)
        }
        _ => {}
    }
    if maven {
        let path = u
            .path()
            .strip_prefix(prefix)
            .ok_or(InstallError::SourceRefused)?;
        let path = path.strip_suffix(".sha256").unwrap_or(path);
        super::paths::destination(&format!("software/libraries/{}", path))
            .map_err(|_| InstallError::SourceRefused)?;
    }
    Ok(u)
}
