//! Forge 1.7–1.16: embedded universal JARs, optional processors and Java-8 launch.
use super::*;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct EmbeddedLibrary {
    pub entry: String,
    pub digest: PublishedDigest,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LegacyLaunch {
    pub main: String,
    pub classpath: Vec<String>,
    pub embedded: BTreeMap<String, EmbeddedLibrary>,
    pub minecraft_alias: String,
}
pub(super) struct Request {
    pub version: String,
    pub coordinate: String,
    pub loader_version: String,
    pub embedded: BTreeMap<String, Vec<u8>>,
    pub installer: ArtifactPin,
    pub metadata: MetadataRevision,
}
#[derive(Deserialize)]
struct V1Profile {
    install: V1Install,
    #[serde(rename = "versionInfo")]
    version: V1Version,
}
#[derive(Deserialize)]
struct V1Install {
    minecraft: String,
    path: String,
    #[serde(rename = "filePath")]
    file: String,
}
#[derive(Deserialize)]
struct V1Version {
    libraries: List<V1Library>,
}
#[derive(Deserialize)]
struct V1Library {
    name: String,
    #[serde(default)]
    serverreq: bool,
    url: Option<String>,
    #[serde(default)]
    checksums: Vec<String>,
}
#[derive(Deserialize)]
struct V2Profile {
    spec: u32,
    minecraft: String,
    libraries: List<Library>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    data: BTreeMap<String, Sided>,
}

#[derive(Deserialize)]
struct VersionLibraries {
    libraries: List<Library>,
}

/// Read only the main manifest section; signed per-entry sections can be huge.
fn manifest(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut jar =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| InstallError::InvalidMetadata)?;
    let entry = jar
        .by_name("META-INF/MANIFEST.MF")
        .map_err(|_| InstallError::InvalidMetadata)?;
    let mut header = Vec::new();
    entry
        .take(128 * 1024)
        .read_to_end(&mut header)
        .map_err(|_| InstallError::InvalidMetadata)?;
    let end = header
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| header.windows(2).position(|w| w == b"\n\n"))
        .ok_or(InstallError::InvalidMetadata)?;
    let header = std::str::from_utf8(&header[..end]).map_err(|_| InstallError::InvalidMetadata)?;
    let header = header
        .split("\r\n\r\n")
        .next()
        .unwrap_or(header)
        .split("\n\n")
        .next()
        .ok_or(InstallError::InvalidMetadata)?
        .replace("\r\n ", "")
        .replace("\n ", "");
    let mut fields = BTreeMap::new();
    for line in header.lines() {
        let (key, value) = line.split_once(": ").ok_or(InstallError::InvalidMetadata)?;
        if fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(InstallError::InvalidMetadata);
        }
    }
    Ok(fields)
}
fn embedded_library(
    entries: &BTreeMap<String, Vec<u8>>,
    entry: &str,
    expected: Option<PublishedDigest>,
) -> Result<EmbeddedLibrary> {
    let bytes = entries.get(entry).ok_or(InstallError::InvalidMetadata)?;
    let digest = expected.unwrap_or(PublishedDigest::new(
        DigestAlgorithm::Sha256,
        sha256(bytes),
    )?);
    use sha1::{Digest as _, Sha1};
    let actual = match digest.algorithm {
        DigestAlgorithm::Sha1 => format!("{:x}", Sha1::digest(bytes)),
        DigestAlgorithm::Sha256 => sha256(bytes),
        _ => return Err(InstallError::InvalidMetadata),
    };
    if actual != digest.hex {
        return Err(InstallError::ArtifactChanged);
    }
    Ok(EmbeddedLibrary {
        entry: entry.into(),
        digest,
        size: bytes.len() as u64,
    })
}
fn launch(
    entries: &BTreeMap<String, Vec<u8>>,
    main_path: &str,
    embedded: BTreeMap<String, EmbeddedLibrary>,
    minecraft: &str,
) -> Result<LegacyLaunch> {
    let entry = &embedded
        .get(main_path)
        .ok_or(InstallError::InvalidMetadata)?
        .entry;
    let fields = manifest(entries.get(entry).ok_or(InstallError::InvalidMetadata)?)?;
    let main = fields
        .get("Main-Class")
        .ok_or(InstallError::InvalidMetadata)?
        .clone();
    if !matches!(
        main.as_str(),
        "cpw.mods.fml.relauncher.ServerLaunchWrapper"
            | "net.minecraftforge.fml.relauncher.ServerLaunchWrapper"
            | "net.minecraftforge.server.ServerMain"
    ) {
        return Err(InstallError::Unavailable);
    }
    let minecraft_alias = format!("minecraft_server.{minecraft}.jar");
    relative(&minecraft_alias)?;
    let mut classpath = vec![main_path.to_owned()];
    for entry in fields
        .get("Class-Path")
        .ok_or(InstallError::InvalidMetadata)?
        .split_whitespace()
    {
        relative(entry)?;
        if (!entry.starts_with("libraries/") && entry != minecraft_alias)
            || !entry.ends_with(".jar")
        {
            return Err(InstallError::InvalidMetadata);
        }
        if !classpath.iter().any(|p| p == entry) {
            classpath.push(entry.into());
        }
        if classpath.len() > 256 {
            return Err(InstallError::LimitExceeded);
        }
    }
    Ok(LegacyLaunch {
        main,
        classpath,
        embedded,
        minecraft_alias,
    })
}

pub(super) async fn resolve(
    http: &Transport,
    request: Request,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Resolved> {
    let Request {
        version,
        coordinate,
        loader_version,
        embedded,
        installer,
        metadata,
    } = request;
    let bytes = embedded
        .get("install_profile.json")
        .ok_or(InstallError::InvalidMetadata)?;
    let shape: serde_json::Value = parse(bytes)?;
    let (vanilla, mut revisions) = vanilla(http, &version, job, reporter).await?;
    revisions.push(metadata);
    let mut artifacts = vec![
        BundleArtifact {
            destination: "minecraft.jar".into(),
            family: SourceFamily::MojangObjects,
            pin: vanilla,
        },
        BundleArtifact {
            destination: "installer.jar".into(),
            family: SourceFamily::ForgeMaven,
            pin: installer,
        },
    ];
    let mut contained = BTreeMap::new();
    let (main, processors, mut data) = if shape.get("spec").is_some() {
        let mut profile: V2Profile = parse(bytes)?;
        if !matches!(profile.spec, 0 | 1)
            || profile.minecraft != version
            || profile.libraries.0.len() > MAX_FILES
            || profile.processors.len() > MAX_PROCESSORS
            || profile.data.len() > 128
        {
            return Err(InstallError::InvalidMetadata);
        }
        // Forge 1.12.2 build 2860 uses spec 0: install_profile lists the
        // embedded Forge JAR and MCP metadata, while version.json holds the
        // launch libraries. No processors are needed for this profile.
        if profile.spec == 0 {
            if !profile.processors.is_empty() || !profile.data.is_empty() {
                return Err(InstallError::InvalidMetadata);
            }
            let version: VersionLibraries = parse(
                embedded
                    .get("version.json")
                    .ok_or(InstallError::InvalidMetadata)?,
            )?;
            if version.libraries.0.len() > MAX_FILES {
                return Err(InstallError::LimitExceeded);
            }
            profile.libraries = version.libraries;
        }
        let universal = format!("net.minecraftforge:forge:{coordinate}:universal");
        let bare = format!("net.minecraftforge:forge:{coordinate}");
        let main = if profile.libraries.0.iter().any(|l| l.name == universal) {
            universal
        } else {
            bare
        };
        let required = if profile.spec == 0 {
            let path = format!("maven/{}", maven(&main)?);
            let fields = manifest(embedded.get(&path).ok_or(InstallError::InvalidMetadata)?)?;
            Some(
                fields
                    .get("Class-Path")
                    .ok_or(InstallError::InvalidMetadata)?
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<HashSet<_>>(),
            )
        } else {
            None
        };
        for library in profile.libraries.0 {
            let path = maven(&library.name)?;
            if required.as_ref().is_some_and(|required| {
                library.name != main && !required.contains(&format!("libraries/{path}"))
            }) {
                continue;
            }
            let item = library
                .downloads
                .artifact
                .ok_or(InstallError::InvalidMetadata)?;
            if path != item.path || item.size == 0 || item.size > job.limits.artifact_bytes {
                return Err(InstallError::InvalidMetadata);
            }
            let destination = format!("libraries/{path}");
            let digest = PublishedDigest::new(DigestAlgorithm::Sha1, item.sha1)?;
            if item.url.is_empty() || embedded.contains_key(&format!("maven/{path}")) {
                let entry = format!("maven/{path}");
                let value = embedded_library(&embedded, &entry, Some(digest))?;
                if value.size != item.size || contained.insert(destination, value).is_some() {
                    return Err(InstallError::InvalidMetadata);
                }
            } else {
                artifacts.push(BundleArtifact {
                    destination,
                    family: family(&item.url)?,
                    pin: ArtifactPin {
                        url: item.url,
                        digest,
                        size: Some(item.size),
                    },
                });
            }
        }
        if let Some(required) = required {
            let alias = format!("minecraft_server.{version}.jar");
            if required.iter().any(|path| {
                path != &alias
                    && !contained.contains_key(path)
                    && !artifacts
                        .iter()
                        .any(|artifact| &artifact.destination == path)
            }) {
                return Err(InstallError::InvalidMetadata);
            }
        }
        (
            format!("libraries/{}", maven(&main)?),
            profile.processors,
            profile.data,
        )
    } else {
        let profile: V1Profile = parse(bytes)?;
        if profile.install.minecraft != version
            || profile.install.path != format!("net.minecraftforge:forge:{coordinate}")
            || profile.version.libraries.0.len() > MAX_FILES
        {
            return Err(InstallError::InvalidMetadata);
        }
        relative(&profile.install.file)?;
        let main = format!("libraries/{}", maven(&profile.install.path)?);
        contained.insert(
            main.clone(),
            embedded_library(&embedded, &profile.install.file, None)?,
        );
        for library in profile
            .version
            .libraries
            .0
            .into_iter()
            .filter(|l| l.serverreq)
        {
            let path = maven(&library.name)?;
            let base = match library
                .url
                .as_deref()
                .unwrap_or("https://libraries.minecraft.net/")
            {
                "https://maven.minecraftforge.net/"
                | "http://maven.minecraftforge.net/"
                | "https://files.minecraftforge.net/maven/"
                | "http://files.minecraftforge.net/maven/" => "https://maven.minecraftforge.net/",
                "https://libraries.minecraft.net/" | "http://libraries.minecraft.net/" => {
                    "https://libraries.minecraft.net/"
                }
                _ => return Err(InstallError::SourceRefused),
            };
            let url = format!("{base}{path}");
            let family = family(&url)?;
            if library.checksums.len() > 8 {
                return Err(InstallError::InvalidMetadata);
            }
            let digest = if let Some(expected) = library.checksums.first() {
                PublishedDigest::new(DigestAlgorithm::Sha1, expected.clone())?
            } else {
                let sidecar = http
                    .family_metadata(&(url.clone() + ".sha1"), family, Some(256), job, reporter)
                    .await?;
                PublishedDigest::new(
                    DigestAlgorithm::Sha1,
                    String::from_utf8(sidecar.data)
                        .map_err(|_| InstallError::InvalidMetadata)?
                        .trim()
                        .into(),
                )?
            };
            let size = http.head_size(&url, family, job, reporter).await?;
            artifacts.push(BundleArtifact {
                destination: format!("libraries/{path}"),
                family,
                pin: ArtifactPin {
                    url,
                    digest,
                    size: Some(size),
                },
            });
        }
        (main, Vec::new(), BTreeMap::new())
    };
    let mut seen = HashSet::new();
    for path in artifacts
        .iter()
        .map(|a| &a.destination)
        .chain(contained.keys())
    {
        relative(path)?;
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(InstallError::InvalidMetadata);
        }
    }
    if !data.contains_key("PATCHED") {
        data.insert(
            "PATCHED".into(),
            Sided {
                server: format!("'{{ROOT}}/{main}'"),
            },
        );
    }
    let legacy = launch(&embedded, &main, contained, &version)?;
    let mut plan = recipe::prepare(
        Software::Forge,
        &version,
        &coordinate,
        loader_version,
        recipe::RecipeInput {
            processors,
            data,
            legacy: Some(legacy),
        },
        embedded,
        artifacts,
    )?;
    let total = plan.artifacts.iter().try_fold(0u64, |n, a| {
        n.checked_add(a.pin.size.unwrap_or(0))
            .ok_or(InstallError::LimitExceeded)
    })?;
    if total > job.limits.total_bytes {
        return Err(InstallError::LimitExceeded);
    }
    plan.disk_cap = total
        .checked_mul(3)
        .and_then(|n| n.checked_add(128 * 1024 * 1024))
        .ok_or(InstallError::LimitExceeded)?
        .min(job.limits.total_bytes);
    Ok(Resolved {
        source_build: None,
        version,
        build: None,
        artifact: None,
        bundle: None,
        processed: Some(plan),
        java: JavaRequirement {
            major: 8,
            evidence: "forge_legacy_java8_adapter".into(),
        },
        warnings: vec![Warning::LegacyChecksum, Warning::FirstRunEffects],
        metadata: revisions,
    })
}

async fn vanilla(
    http: &Transport,
    version: &str,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<(ArtifactPin, Vec<MetadataRevision>)> {
    let mut revisions = Vec::new();
    let catalogue = catalogue(http, Software::Vanilla, job, reporter, &mut revisions).await?;
    let entry = catalogue
        .vanilla
        .ok_or(InstallError::InvalidMetadata)?
        .versions
        .0
        .into_iter()
        .find(|v| v.id == version && v.kind == "release")
        .ok_or(InstallError::Unavailable)?;
    let bytes = http
        .receive(
            Source {
                url: &entry.url,
                software: Software::Vanilla,
            },
            Role::Metadata,
            None,
            job,
            reporter,
            None,
        )
        .await?;
    use sha1::{Digest as _, Sha1};
    if format!("{:x}", Sha1::digest(&bytes.data)) != entry.sha1 {
        return Err(InstallError::ArtifactChanged);
    }
    let detail: VanillaDetail = parse(&bytes.data)?;
    if detail.id != version {
        return Err(InstallError::InvalidMetadata);
    }
    let server = detail.downloads.server.ok_or(InstallError::Unavailable)?;
    let url = url::Url::parse(&server.url).map_err(|_| InstallError::SourceRefused)?;
    let pin = PublishedDigest::new(DigestAlgorithm::Sha1, server.sha1)?;
    if !matches!(
        url.host_str(),
        Some("launcher.mojang.com" | "piston-data.mojang.com")
    ) || url.path() != format!("/v1/objects/{}/server.jar", pin.hex)
        || server.size == 0
        || server.size > job.limits.artifact_bytes
    {
        return Err(InstallError::SourceRefused);
    }
    revisions.push(MetadataRevision {
        url: entry.url,
        observed_sha256: bytes.observed_sha256,
    });
    Ok((
        ArtifactPin {
            url: format!(
                "https://piston-data.mojang.com/v1/objects/{}/server.jar",
                pin.hex
            ),
            digest: pin,
            size: Some(server.size),
        },
        revisions,
    ))
}

pub(super) async fn materialize(plan: &ProcessorPlan, root: &std::path::Path) -> Result<()> {
    let legacy = plan.legacy.as_ref().ok_or(InstallError::InvalidInput)?;
    for (destination, item) in &legacy.embedded {
        let bytes = plan
            .embedded
            .get(&item.entry)
            .ok_or(InstallError::InvalidMetadata)?;
        install::write(&root.join(destination), bytes).await?;
    }
    let mut source = tokio::fs::File::open(root.join("minecraft.jar"))
        .await
        .map_err(|_| InstallError::ArtifactChanged)?;
    let mut target = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join(&legacy.minecraft_alias))
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    tokio::io::copy(&mut source, &mut target)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    target
        .sync_all()
        .await
        .map_err(|_| InstallError::PublishFailed)
}
pub(super) async fn verify_layout(
    plan: &ProcessorPlan,
    root: &std::path::Path,
    cap: u64,
) -> Result<()> {
    let legacy = plan.legacy.as_ref().ok_or(InstallError::InvalidInput)?;
    for (path, item) in &legacy.embedded {
        let bytes = plan
            .embedded
            .get(&item.entry)
            .ok_or(InstallError::ReceiptInvalid)?;
        if bytes.len() as u64 != item.size {
            return Err(InstallError::ArtifactChanged);
        }
        let pin = ArtifactPin {
            url: String::new(),
            digest: item.digest.clone(),
            size: Some(item.size),
        };
        crate::bundle::rehash(&root.join(path), &pin, item.size, &sha256(bytes), 1).await?;
    }
    let vanilla = plan.artifacts.first().ok_or(InstallError::ReceiptInvalid)?;
    let (size, hash) = install::digest(&root.join("minecraft.jar"), cap).await?;
    crate::bundle::rehash(
        &root.join(&legacy.minecraft_alias),
        &vanilla.pin,
        size,
        &hash,
        1,
    )
    .await?;
    for path in &legacy.classpath {
        relative(path)?;
        crate::install::plain_file(&root.join(path))?;
    }
    let patched = recipe::resolve(plan, "{PATCHED}", root)?;
    let patched = std::path::Path::new(&patched);
    if !patched.starts_with(root) {
        return Err(InstallError::PathRefused);
    }
    crate::install::plain_file(patched)?;
    let metadata = std::fs::metadata(patched).map_err(|_| InstallError::ArtifactChanged)?;
    if metadata.len() > cap {
        return Err(InstallError::LimitExceeded);
    }
    let archive = zip::ZipArchive::new(
        std::fs::File::open(patched).map_err(|_| InstallError::ArtifactChanged)?,
    )
    .map_err(|_| InstallError::ArtifactChanged)?;
    if archive.is_empty() || archive.len() > 100_000 {
        return Err(InstallError::ArtifactChanged);
    }
    Ok(())
}

/// Java 8 cannot consume @argfiles. A generated manifest-only pathing JAR keeps
/// long/Unicode classpaths out of the native command line without a Java wrapper.
pub(super) fn pathing_jar(plan: &ProcessorPlan, root: &std::path::Path) -> Result<Vec<u8>> {
    use std::io::Write;
    let legacy = plan.legacy.as_ref().ok_or(InstallError::InvalidInput)?;
    let mut manifest = String::from("Manifest-Version: 1.0\r\n");
    fn header(out: &mut String, key: &str, value: &str) -> Result<()> {
        if !value.is_ascii() || value.chars().any(char::is_control) {
            return Err(InstallError::InvalidInput);
        }
        let mut line = format!("{key}: {value}");
        while line.len() > 70 {
            out.push_str(&line[..70]);
            out.push_str("\r\n");
            line = format!(" {}", &line[70..]);
        }
        out.push_str(&line);
        out.push_str("\r\n");
        Ok(())
    }
    header(&mut manifest, "Main-Class", &legacy.main)?;
    let classpath = legacy
        .classpath
        .iter()
        .map(|p| {
            url::Url::from_file_path(root.join(p))
                .map(|u| u.to_string())
                .map_err(|_| InstallError::PathRefused)
        })
        .collect::<Result<Vec<_>>>()?;
    header(&mut manifest, "Class-Path", &classpath.join(" "))?;
    manifest.push_str("\r\n");
    if manifest.len() > 128 * 1024 {
        return Err(InstallError::LimitExceeded);
    }
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    zip.start_file("META-INF/MANIFEST.MF", options)
        .map_err(|_| InstallError::PublishFailed)?;
    zip.write_all(manifest.as_bytes())
        .map_err(|_| InstallError::PublishFailed)?;
    Ok(zip
        .finish()
        .map_err(|_| InstallError::PublishFailed)?
        .into_inner())
}
