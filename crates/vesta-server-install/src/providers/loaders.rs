//! Download-only loader recipes. Publisher membership/order is evidence, never launch policy.
use super::super::transfer::validate_family_url;
use super::*;
use crate::{bundle, paths};

pub(super) const MAX_LOADER_LIBRARIES: usize = 128;
/// Minimum Java whose launcher accepts `@argfile` arguments.
pub(super) const ARGFILE_JAVA: u32 = 9;
const TOKEN: &str = "${modrinth.gameVersion}";

#[derive(Deserialize)]
struct Manifest {
    #[serde(rename = "gameVersions")]
    games: List<Game>,
    #[serde(default, rename = "versionGroups")]
    groups: Vec<Group>,
}
#[derive(Deserialize)]
struct Group {
    id: String,
    loaders: List<Loader>,
}
#[derive(Deserialize)]
struct Game {
    id: String,
    loaders: List<Loader>,
    #[serde(rename = "versionGroup")]
    group: Option<String>,
}
#[derive(Clone, Deserialize)]
struct Loader {
    id: String,
    url: String,
    stable: bool,
}
#[derive(Deserialize)]
struct Profile {
    #[serde(rename = "mainClass")]
    main: String,
    libraries: Libraries<ClientLibrary>,
}
#[derive(Deserialize)]
struct ClientLibrary {
    name: String,
    url: String,
    include_in_classpath: bool,
    downloadable: bool,
}
#[derive(Deserialize)]
struct ServerProfile {
    #[serde(rename = "mainClass")]
    main: String,
    libraries: Libraries<ServerLibrary>,
}
#[derive(Deserialize)]
struct ServerLibrary {
    name: String,
    url: String,
    sha256: Option<String>,
    size: Option<u64>,
}
#[derive(Deserialize)]
struct QuiltInfo {
    loader: QuiltPin,
    hashed: QuiltPin,
}
#[derive(Deserialize)]
struct QuiltPin {
    maven: String,
    version: String,
    file_size: u64,
    hashes: QuiltHashes,
}
#[derive(Deserialize)]
struct QuiltHashes {
    sha256: String,
}
struct Libraries<T>(Vec<T>);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Libraries<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Libraries<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("bounded loader libraries")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = a.next_element()? {
                    if out.len() == MAX_LOADER_LIBRARIES {
                        return Err(serde::de::Error::custom("library limit"));
                    }
                    out.push(v);
                }
                Ok(Libraries(out))
            }
        }
        d.deserialize_seq(V(PhantomData))
    }
}
fn families(s: Software) -> Result<(SourceFamily, SourceFamily)> {
    match s {
        Software::Fabric => Ok((SourceFamily::ModrinthFabric, SourceFamily::FabricMeta)),
        Software::Quilt => Ok((SourceFamily::ModrinthQuilt, SourceFamily::QuiltMeta)),
        _ => Err(InstallError::Unavailable),
    }
}
async fn fetch<T: for<'de> Deserialize<'de>>(
    http: &Transport,
    url: &str,
    family: SourceFamily,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
    revisions: &mut Vec<MetadataRevision>,
) -> Result<T> {
    let data = http
        .family_metadata(url, family, None, job, reporter)
        .await?;
    revisions.push(MetadataRevision {
        url: url.into(),
        observed_sha256: data.observed_sha256,
    });
    parse(&data.data)
}
async fn catalogue(
    http: &Transport,
    s: Software,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
    revisions: &mut Vec<MetadataRevision>,
    target: Option<&str>,
) -> Result<(Vec<String>, Vec<Loader>)> {
    let (family, _) = families(s)?;
    let m: Manifest = fetch(
        http,
        &format!(
            "https://launcher-meta.modrinth.com/{}/{}/manifest.json",
            s.id(),
            if s == Software::Quilt { "v1" } else { "v0" }
        ),
        family,
        job,
        reporter,
        revisions,
    )
    .await?;
    if m.games.0.len() > job.limits.max_entries || m.groups.len() > 16 {
        return Err(InstallError::InvalidMetadata);
    }
    let mut seen = HashSet::new();
    let mut versions = Vec::new();
    let mut loaders = None;
    let mut groups = BTreeMap::new();
    for mut group in m.groups {
        if s != Software::Quilt {
            return Err(InstallError::InvalidMetadata);
        }
        if !identifier(&group.id) || groups.contains_key(&group.id) {
            return Err(InstallError::InvalidMetadata);
        }
        let mut ids = HashSet::new();
        for loader in &group.loaders.0 {
            let expected = format!(
                "https://launcher-meta.modrinth.com/quilt/v1/version-group/{}/loader-version/{}",
                group.id, loader.id
            );
            if !loader_identifier(&loader.id) || !ids.insert(&loader.id) || loader.url != expected {
                return Err(InstallError::InvalidMetadata);
            }
            validate_family_url(&loader.url, family, Role::Metadata, false)?;
        }
        // The current Quilt mirror marks all loaders false, including releases.
        // Numeric release IDs exclude beta/RC builds; planning still verifies
        // their membership and libraries against the official Quilt service.
        for loader in &mut group.loaders.0 {
            loader.stable = loader
                .id
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
        }
        groups.insert(group.id, group.loaders.0);
    }
    for game in m.games.0 {
        if !seen.insert(game.id.clone()) {
            return Err(InstallError::InvalidMetadata);
        }
        if game.id == TOKEN {
            if game.loaders.0.is_empty() || game.loaders.0.len() > job.limits.max_entries {
                return Err(InstallError::InvalidMetadata);
            }
            let mut ids = HashSet::new();
            for l in &game.loaders.0 {
                if !loader_identifier(&l.id)
                    || !ids.insert(&l.id)
                    || l.url
                        != format!(
                            "https://launcher-meta.modrinth.com/{}/v0/versions/{}.json",
                            s.id(),
                            l.id
                        )
                {
                    return Err(InstallError::InvalidMetadata);
                }
                validate_family_url(&l.url, family, Role::Metadata, false)?;
            }
            loaders = Some(game.loaders.0);
        } else {
            if game.id.is_empty()
                || game.id.len() > 64
                || game.id.chars().any(char::is_control)
                || !game.loaders.0.is_empty()
            {
                return Err(InstallError::InvalidMetadata);
            }
            // Historical display labels are not selectable version identifiers.
            if !identifier(&game.id) {
                continue;
            }
            if let Some(group) = game.group {
                let choices = groups.get(&group).ok_or(InstallError::InvalidMetadata)?;
                if target.map_or(loaders.is_none(), |id| id == game.id) {
                    loaders = Some(choices.clone());
                }
            } else if s == Software::Quilt {
                return Err(InstallError::InvalidMetadata);
            }
            versions.push(game.id);
        }
    }
    check_list(&versions, job.limits)?;
    Ok((versions, loaders.ok_or(InstallError::InvalidMetadata)?))
}
pub(super) async fn versions(
    http: &Transport,
    s: Software,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<String>> {
    Ok(catalogue(http, s, job, reporter, &mut Vec::new(), None)
        .await?
        .0)
}
pub(super) async fn releases(
    http: &Transport,
    software: Software,
    version: &str,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<releases::SoftwareRelease>> {
    let (versions, loaders) = catalogue(
        http,
        software,
        job,
        reporter,
        &mut Vec::new(),
        Some(version),
    )
    .await?;
    if !versions.iter().any(|v| v == version) {
        return Err(InstallError::Unavailable);
    }
    Ok(loaders
        .into_iter()
        .filter(|l| l.stable)
        .map(|l| releases::SoftwareRelease {
            id: l.id.clone(),
            build: None,
            loader_version: Some(l.id),
        })
        .collect())
}
/// Narrow Maven grammar, no general interpolation, classifiers, extensions or ranges.
/// Classifiers are deliberately unsupported at this checkpoint.
pub(crate) fn maven_path(coordinate: &str) -> Result<String> {
    if coordinate.len() > 256 {
        return Err(InstallError::InvalidMetadata);
    }
    let parts: Vec<_> = coordinate.split(':').collect();
    if parts.len() != 3 {
        return Err(InstallError::InvalidMetadata);
    }
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty()
            || p.len() > 64
            || p.starts_with('.')
            || p.ends_with('.')
            || p.contains("..")
            || !p.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'.' | b'_' | b'-')
                    || (i == 2 && b == b'+')
            })
        {
            return Err(InstallError::InvalidMetadata);
        }
    }
    let path = format!(
        "{}/{}/{}/{}-{}.jar",
        parts[0].replace('.', "/"),
        parts[1],
        parts[2],
        parts[1],
        parts[2]
    );
    paths::destination(&format!("software/libraries/{}", path))?;
    Ok(path)
}
fn repository(s: Software, url: &str) -> Result<SourceFamily> {
    match (s, url) {
        (Software::Fabric | Software::Quilt, "https://maven.fabricmc.net/") => {
            Ok(SourceFamily::FabricMaven)
        }
        (Software::Quilt, "https://maven.quiltmc.org/repository/release/") => {
            Ok(SourceFamily::QuiltMaven)
        }
        _ => Err(InstallError::SourceRefused),
    }
}
/// Exactly one 64-hex digest, optional ASCII whitespace/filename, <=256 bytes.
pub(crate) fn sidecar_digest(bytes: &[u8]) -> Result<PublishedDigest> {
    if bytes.len() < 64 || bytes.len() > 256 || !bytes.is_ascii() {
        return Err(InstallError::InvalidMetadata);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| InstallError::InvalidMetadata)?;
    let (hex, suffix) = text.split_at(64);
    if !suffix.is_empty() && !suffix.as_bytes()[0].is_ascii_whitespace() {
        return Err(InstallError::InvalidMetadata);
    }
    let filename = suffix.trim_matches(|c: char| c.is_ascii_whitespace());
    if filename
        .bytes()
        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(InstallError::InvalidMetadata);
    }
    PublishedDigest::new(DigestAlgorithm::Sha256, hex.into())
}
async fn sidecar(
    http: &Transport,
    url: &str,
    family: SourceFamily,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
    revisions: &mut Vec<MetadataRevision>,
) -> Result<PublishedDigest> {
    let url = format!("{}.sha256", url);
    let data = http
        .family_metadata(
            &url,
            family,
            Some(256u64.min(job.limits.metadata_bytes)),
            job,
            reporter,
        )
        .await?;
    revisions.push(MetadataRevision {
        url,
        observed_sha256: data.observed_sha256,
    });
    sidecar_digest(&data.data)
}
pub(super) async fn resolve(
    http: &Transport,
    s: Software,
    selection: &Selection,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Resolved> {
    if selection.build.is_some() {
        return Err(InstallError::InvalidInput);
    }
    let (profile_family, meta_family) = families(s)?;
    let mut revisions = Vec::new();
    let (versions, loaders) = catalogue(
        http,
        s,
        job,
        reporter,
        &mut revisions,
        selection.version.as_deref(),
    )
    .await?;
    let version = select_version(
        &Catalogue {
            versions,
            vanilla: None,
            latest: None,
        },
        selection,
    )?;
    let chosen = if let Some(id) = &selection.loader_version {
        loaders.iter().find(|l| &l.id == id)
    } else {
        loaders.iter().find(|l| l.stable)
    }
    .ok_or(InstallError::Unavailable)?;
    // An explicitly named unstable loader still needs the experimental opt-in, as for Vanilla.
    if !chosen.stable && !selection.experimental {
        return Err(InstallError::Unavailable);
    }
    let client: Profile = fetch(
        http,
        &chosen.url,
        profile_family,
        job,
        reporter,
        &mut revisions,
    )
    .await?;
    if client.main != bundle::client_main(s)? {
        return Err(InstallError::InvalidMetadata);
    }
    let server_url = match s {
        Software::Fabric => format!(
            "https://meta.fabricmc.net/v2/versions/loader/{}/{}/server/json",
            version, chosen.id
        ),
        Software::Quilt => format!(
            "https://meta.quiltmc.org/v3/versions/loader/{}/{}/server/json",
            version, chosen.id
        ),
        _ => return Err(InstallError::Unavailable),
    };
    let server: ServerProfile = fetch(
        http,
        &server_url,
        meta_family,
        job,
        reporter,
        &mut revisions,
    )
    .await?;
    if server.main != bundle::server_main(s)?
        || server.libraries.0.is_empty()
        || client.libraries.0.is_empty()
    {
        return Err(InstallError::InvalidMetadata);
    }
    let mut official = BTreeMap::new();
    for lib in server.libraries.0 {
        maven_path(&lib.name)?;
        repository(s, &lib.url)?;
        if official.insert(lib.name.clone(), lib).is_some() {
            return Err(InstallError::InvalidMetadata);
        }
    }
    let mut order = Vec::new();
    let mut names = HashSet::new();
    for lib in client.libraries.0 {
        // Mirror repository is never used as an artifact source.
        if !lib.include_in_classpath
            || !lib.downloadable
            || lib.url != "https://launcher-meta.modrinth.com/maven/"
        {
            return Err(InstallError::InvalidMetadata);
        }
        let name = lib.name.replace(TOKEN, &version);
        maven_path(&name)?;
        if !names.insert(name.clone()) || !official.contains_key(&name) {
            return Err(InstallError::InvalidMetadata);
        }
        order.push(name);
    }
    if names.len() != official.len() || order.len() > job.limits.max_entries {
        return Err(InstallError::InvalidMetadata);
    }
    let quilt = if s == Software::Quilt {
        let info_url = server_url
            .strip_suffix("/server/json")
            .ok_or(InstallError::InvalidMetadata)?;
        let info: QuiltInfo =
            fetch(http, info_url, meta_family, job, reporter, &mut revisions).await?;
        if info.loader.version != chosen.id
            || info.hashed.version != version
            || info.loader.maven != format!("org.quiltmc:quilt-loader:{}", chosen.id)
            || info.hashed.maven != format!("org.quiltmc:hashed:{}", version)
            || !official.contains_key(&info.loader.maven)
            || !official.contains_key(&info.hashed.maven)
        {
            return Err(InstallError::InvalidMetadata);
        }
        Some(info)
    } else {
        None
    };
    // Reuse Vanilla's original digest, runtime declaration, warnings and metadata revision checks.
    let vanilla = super::resolve_single(
        http,
        Software::Vanilla,
        &Selection {
            version: Some(version.clone()),
            experimental: selection.experimental,
            ..Selection::default()
        },
        job,
        reporter,
    )
    .await?;
    let vanilla_pin = vanilla.artifact.ok_or(InstallError::InvalidMetadata)?;
    let mut artifacts = vec![BundleArtifact {
        destination: "software/vanilla.jar".into(),
        pin: vanilla_pin,
        family: SourceFamily::MojangObjects,
    }];
    revisions.extend(vanilla.metadata);
    for name in order {
        let lib = official.get(&name).ok_or(InstallError::InvalidMetadata)?;
        let path = maven_path(&name)?;
        let family = repository(s, &lib.url)?;
        let url = format!("{}{}", lib.url, path);
        validate_family_url(&url, family, Role::Artifact, false)?;
        let special = quilt
            .as_ref()
            .and_then(|q| [&q.loader, &q.hashed].into_iter().find(|p| p.maven == name));
        let (digest, size) = if let Some(q) = special {
            if family != SourceFamily::QuiltMaven {
                return Err(InstallError::SourceRefused);
            }
            let digest = PublishedDigest::new(DigestAlgorithm::Sha256, q.hashes.sha256.clone())?;
            if digest != sidecar(http, &url, family, job, reporter, &mut revisions).await? {
                return Err(InstallError::IntegrityConflict);
            }
            (digest, q.file_size)
        } else if s == Software::Fabric && lib.sha256.is_some() && lib.size.is_some() {
            (
                PublishedDigest::new(
                    DigestAlgorithm::Sha256,
                    lib.sha256.clone().ok_or(InstallError::InvalidMetadata)?,
                )?,
                lib.size.ok_or(InstallError::InvalidMetadata)?,
            )
        } else {
            let digest = sidecar(http, &url, family, job, reporter, &mut revisions).await?;
            if s == Software::Fabric
                && lib
                    .sha256
                    .as_ref()
                    .is_some_and(|h| !h.eq_ignore_ascii_case(digest.hex()))
            {
                return Err(InstallError::IntegrityConflict);
            }
            let size = http.head_size(&url, family, job, reporter).await?;
            if s == Software::Fabric && lib.size.is_some_and(|n| n != size) {
                return Err(InstallError::IntegrityConflict);
            }
            (digest, size)
        };
        if size == 0 || size > job.limits.artifact_bytes {
            return Err(InstallError::LimitExceeded);
        }
        artifacts.push(BundleArtifact {
            destination: format!("software/libraries/{}", path),
            pin: ArtifactPin {
                url,
                digest,
                size: Some(size),
            },
            family,
        });
    }
    let classpath = (1..artifacts.len()).collect();
    let mut download = DownloadBundle {
        loader_version: chosen.id.clone(),
        artifacts,
        classpath,
        generated: GeneratedFile {
            path: "software/launch.args".into(),
            size: 0,
            sha256: String::new(),
        },
        classpath_separator: if cfg!(windows) { ";" } else { ":" }.into(),
    };
    let args = bundle::argfile(s, &download)?;
    download.generated.size = args.len() as u64;
    download.generated.sha256 = sha256(&args);
    bundle::validate(s, &download, job.limits)?;
    let mut warnings = vanilla.warnings;
    if !chosen.stable && !warnings.contains(&Warning::ExperimentalBuild) {
        warnings.push(Warning::ExperimentalBuild);
    }
    Ok(Resolved {
        source_build: None,
        processed: None,
        version,
        build: None,
        artifact: None,
        bundle: Some(download),
        // The compiled launch is a Java @argfile, which the Java 8 launcher does not support.
        java: if vanilla.java.major < ARGFILE_JAVA {
            JavaRequirement {
                major: ARGFILE_JAVA,
                evidence: "java-argfile launch".into(),
            }
        } else {
            vanilla.java
        },
        warnings,
        metadata: revisions,
    })
}
