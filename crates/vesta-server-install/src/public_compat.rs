//! Opt-in small public transfers via normal family/pin/deadline/stream contracts.
use super::*;
use crate::progress::SilentProgressReporter;
use serde_json::{json, Value};
const ROOT: &str = "V:/BIG_BURGS-backend-pass/public-compat-fixtures";
fn job() -> Job {
    Job::new(
        Limits {
            metadata_bytes: 2 * 1024 * 1024,
            artifact_bytes: 2 * 1024 * 1024,
            total_bytes: 4 * 1024 * 1024,
            attempts: 1,
            redirects: 1,
            connect_ms: 5000,
            idle_ms: 10000,
            total_ms: 20000,
            ..Limits::default()
        },
        0,
    )
}
#[tokio::test]
#[ignore = "small real public metadata+SHA1-bound artifact, no runtime execution"]
async fn live_public_compat_runtime_mojang() -> Result<()> {
    let start = std::time::Instant::now();
    let transport = Transport::new(TransportConfig::direct());
    let mut job = job();
    let reporter = SilentProgressReporter;
    let meta = transport
        .receive(
            Source {
                url: "https://launcher-meta.modrinth.com/minecraft/v0/versions/1.21.1.json",
                software: Software::Vanilla,
            },
            Role::Metadata,
            None,
            &mut job,
            &reporter,
            None,
        )
        .await?;
    let value: Value =
        serde_json::from_slice(&meta.data).map_err(|_| InstallError::InvalidMetadata)?;
    assert_eq!(value["id"], "1.21.1");
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(format!("{ROOT}/mojang.json")).map_err(|_| InstallError::InvalidInput)?,
    )
    .map_err(|_| InstallError::InvalidInput)?;
    let pin = ArtifactPin {
        url: fixture["url"].as_str().unwrap().into(),
        digest: PublishedDigest::new(
            DigestAlgorithm::Sha1,
            fixture["hash"]["value"].as_str().unwrap().into(),
        )?,
        size: fixture["bytes"].as_u64(),
    };
    let root = tempfile::tempdir().map_err(|_| InstallError::PublishFailed)?;
    let path = root.path().join("inert-log.xml");
    let mut file = File::create(&path)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    let artifact = transport
        .receive(
            Source {
                url: &pin.url,
                software: Software::Vanilla,
            },
            Role::Artifact,
            Some(&pin),
            &mut job,
            &reporter,
            Some(&mut file),
        )
        .await?;
    assert_eq!(artifact.size, 888);
    assert_eq!(std::fs::metadata(path).unwrap().len(), 888);
    let result = json!({"label":"mojang","requests_minimum":2,"metadata_bytes":meta.size,"artifact_bytes":artifact.size,"digest_verified":true,"streamed_job_bytes":job.bytes,"wall_ms":start.elapsed().as_secs_f64()*1000.0});
    std::fs::write(
        format!("{ROOT}/runtime-mojang.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    eprintln!("PUBLIC_COMPAT_RUNTIME {result}");
    Ok(())
}
#[tokio::test]
#[ignore = "small real Fabric metadata and hash-bound Maven JAR, no execution"]
async fn live_public_compat_runtime_fabric() -> Result<()> {
    let start = std::time::Instant::now();
    let transport = Transport::new(TransportConfig::direct());
    let mut job = job();
    let reporter = SilentProgressReporter;
    let meta = transport
        .family_metadata(
            "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.18.4/server/json",
            SourceFamily::FabricMeta,
            None,
            &mut job,
            &reporter,
        )
        .await?;
    let value: Value =
        serde_json::from_slice(&meta.data).map_err(|_| InstallError::InvalidMetadata)?;
    assert_eq!(
        value["mainClass"],
        "net.fabricmc.loader.impl.launch.knot.KnotServer"
    );
    let url = "https://maven.fabricmc.net/net/fabricmc/intermediary/1.21.1/intermediary-1.21.1.jar";
    let digest = transport
        .family_metadata(
            &format!("{url}.sha256"),
            SourceFamily::FabricMaven,
            Some(256),
            &mut job,
            &reporter,
        )
        .await?;
    let hash = std::str::from_utf8(&digest.data)
        .map_err(|_| InstallError::InvalidMetadata)?
        .trim();
    let pin = ArtifactPin {
        url: url.into(),
        digest: PublishedDigest::new(DigestAlgorithm::Sha256, hash.into())?,
        size: None,
    };
    let root = tempfile::tempdir().map_err(|_| InstallError::PublishFailed)?;
    let path = root.path().join("inert-intermediary.jar");
    let mut file = File::create(&path)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    let artifact = transport
        .family_artifact(
            &pin,
            SourceFamily::FabricMaven,
            &mut job,
            &reporter,
            &mut file,
        )
        .await?;
    assert_eq!(std::fs::metadata(path).unwrap().len(), artifact.size);
    let result = json!({"label":"fabric","requests_minimum":3,"metadata_bytes":meta.size+digest.size,"artifact_bytes":artifact.size,"digest_verified":true,"streamed_job_bytes":job.bytes,"wall_ms":start.elapsed().as_secs_f64()*1000.0});
    std::fs::write(
        format!("{ROOT}/runtime-fabric.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    eprintln!("PUBLIC_COMPAT_RUNTIME {result}");
    Ok(())
}
