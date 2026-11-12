//! Opt-in bounded live compatibility, never run by the ordinary fixture suite.
use super::*;
use crate::content::{self, ExpectedHash, Provider, ProviderClient, UreqProviderClient};
use serde_json::{Value, json};
use sha2::Digest;
use std::sync::atomic::{AtomicUsize, Ordering};
const ROOT: &str = "V:/BIG_BURGS-backend-pass/public-compat-fixtures";
const CAP: usize = 2 * 1024 * 1024;
static REQUESTS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
fn save(label: &str, value: Value) {
    std::fs::write(
        format!("{ROOT}/{label}.json"),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
}
fn get(http: &UreqDownloadHttp, raw: &str) -> std::result::Result<(Vec<u8>, usize), String> {
    let mut url = Url::parse(raw).map_err(|_| "url")?;
    let start = Instant::now();
    for hops in 0..=3 {
        if REQUESTS.fetch_add(1, Ordering::SeqCst) >= 20 {
            return Err("request_cap".into());
        }
        let mut response = http
            .get_with_deadline(&url, start + Duration::from_secs(20))
            .map_err(|_| "hardened_get_failed")?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            if hops == 3 {
                return Err("redirect_cap".into());
            }
            url = url
                .join(response.location.as_deref().ok_or("location")?)
                .map_err(|_| "location")?;
            continue;
        }
        if response.status != 200 {
            return Err(format!("http_status_{}", response.status));
        }
        if response.content_length.is_some_and(|n| n > CAP as u64) {
            return Err("declared_cap".into());
        }
        let mut bytes = Vec::new();
        response
            .body
            .by_ref()
            .take(CAP as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "body_read")?;
        if bytes.len() > CAP
            || BYTES.fetch_add(bytes.len(), Ordering::SeqCst) + bytes.len() > 8 * 1024 * 1024
        {
            return Err("actual_byte_cap".into());
        }
        return Ok((bytes, hops));
    }
    unreachable!()
}
fn verify(bytes: &[u8], hash: &ExpectedHash) {
    let actual = match hash {
        ExpectedHash::Sha1(_) => hex::encode(sha1::Sha1::digest(bytes)),
        ExpectedHash::Sha512(_) => hex::encode(sha2::Sha512::digest(bytes)),
    };
    let expected = match hash {
        ExpectedHash::Sha1(v) | ExpectedHash::Sha512(v) => v,
    };
    assert_eq!(
        actual.to_lowercase(),
        expected.to_lowercase(),
        "artifact_digest"
    );
}
fn hash_json(hash: &ExpectedHash) -> Value {
    match hash {
        ExpectedHash::Sha1(v) => json!({"algorithm":"sha1","value":v}),
        ExpectedHash::Sha512(v) => json!({"algorithm":"sha512","value":v}),
    }
}
fn provider(
    label: &str,
    kind: Provider,
    project: &str,
    version: &str,
) -> std::result::Result<(), String> {
    let start = Instant::now();
    let client = UreqProviderClient::configured()
        .with_deadline(start + Duration::from_secs(20))
        .map_err(|_| "provider_config")?;
    let source = client
        .resolve(kind, project, version)
        .map_err(|_| "provider_metadata_or_binding")?;
    if label == "modrinth" {
        assert!(
            matches!(&source.expected_hash,ExpectedHash::Sha512(v) if v=="ccb53610de6d477da5a307f75e74c4ec68984afff00e9a4497dd4c9c77d2d6bfd55973ebc760102a4e6a93c0cac41fb0f5c2b7aec03e5c824cdf6f6c7847efbd")
        );
    }
    let (bytes, redirects) = get(&UreqDownloadHttp::for_job(), &source.download_url)?;
    verify(&bytes, &source.expected_hash);
    let metadata_url = if label == "modrinth" {
        format!("https://api.modrinth.com/v2/version/{version}")
    } else {
        format!("https://api.curseforge.com/v1/mods/{project}/files/{version}")
    };
    save(
        label,
        json!({"label":label,"url":source.download_url,"hash":hash_json(&source.expected_hash),"bytes":bytes.len(),"project":project,"version":version,"metadata_url":metadata_url,"metadata_requires_cf_key":label=="curseforge","server_ms":start.elapsed().as_secs_f64()*1000.0,"server_redirects":redirects,"metadata_requests":client.metadata_requests()}),
    );
    eprintln!(
        "PUBLIC_COMPAT provider={label} bytes={} redirects={redirects} elapsed_ms={:.2}",
        bytes.len(),
        start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}
#[test]
#[ignore = "bounded public compatibility; max2MiB bodies; no install"]
fn live_public_compat_modrinth() -> std::result::Result<(), String> {
    provider("modrinth", Provider::Modrinth, "hXvXSxxg", "B5tawRRL")
}
#[test]
#[ignore = "requires existing runtime CurseForge test dotenv; no secret diagnostics"]
fn live_public_compat_curseforge() -> std::result::Result<(), String> {
    provider("curseforge", Provider::Curseforge, "638111", "6372979")
}
#[test]
#[ignore = "bounded Mojang metadata/logging artifact; no client/server JAR download"]
fn live_public_compat_mojang() -> std::result::Result<(), String> {
    let http = UreqDownloadHttp::for_job();
    let (master, _) = get(
        &http,
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
    )?;
    let index: Value = serde_json::from_slice(&master).map_err(|_| "json")?;
    let version = index["versions"]
        .as_array()
        .ok_or("versions")?
        .iter()
        .find(|v| v["id"] == "1.21.1")
        .ok_or("1.21.1")?;
    let meta = version["url"].as_str().ok_or("url")?;
    let (bytes, _) = get(&http, meta)?;
    verify(
        &bytes,
        &ExpectedHash::Sha1(version["sha1"].as_str().ok_or("sha1")?.into()),
    );
    let detail: Value = serde_json::from_slice(&bytes).map_err(|_| "detail")?;
    let log = &detail["logging"]["client"]["file"];
    let url = log["url"].as_str().ok_or("logging_url")?;
    let hash = ExpectedHash::Sha1(log["sha1"].as_str().ok_or("logging_sha1")?.into());
    let (artifact, redirects) = get(&http, url)?;
    verify(&artifact, &hash);
    save(
        "mojang",
        json!({"label":"mojang","url":url,"hash":hash_json(&hash),"bytes":artifact.len(),"metadata_url":meta,"metadata_hash":version["sha1"],"server_redirects":redirects,"version":"1.21.1"}),
    );
    eprintln!(
        "PUBLIC_COMPAT provider=mojang full_metadata_bytes={} full_artifact_bytes={} redirects={redirects}",
        master.len() + bytes.len(),
        artifact.len()
    );
    Ok(())
}
#[test]
#[ignore = "bounded Fabric metadata/POM+SHA1, no loader execution"]
fn live_public_compat_loader() -> std::result::Result<(), String> {
    let http = UreqDownloadHttp::for_job();
    let metadata_url = "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.18.4/server/json";
    let (metadata, _) = get(&http, metadata_url)?;
    let profile: Value = serde_json::from_slice(&metadata).map_err(|_| "loader_json")?;
    assert_eq!(
        profile["mainClass"],
        "net.fabricmc.loader.impl.launch.knot.KnotServer"
    );
    let url = "https://maven.fabricmc.net/net/fabricmc/intermediary/1.21.1/intermediary-1.21.1.pom";
    let (sidecar, _) = get(&http, &format!("{url}.sha1"))?;
    let hash = ExpectedHash::Sha1(
        std::str::from_utf8(&sidecar)
            .map_err(|_| "sha1_utf8")?
            .trim()
            .to_owned(),
    );
    let (artifact, redirects) = get(&http, url)?;
    verify(&artifact, &hash);
    save(
        "loader",
        json!({"label":"loader","url":url,"hash":hash_json(&hash),"bytes":artifact.len(),"metadata_url":metadata_url,"server_redirects":redirects,"loader":"fabric0.18.4","minecraft":"1.21.1"}),
    );
    eprintln!(
        "PUBLIC_COMPAT provider=loader metadata_bytes={} artifact_bytes={} redirects={redirects}",
        metadata.len(),
        artifact.len()
    );
    Ok(())
}
#[test]
#[ignore = "bounded live Modrinth ranges, never reads full104MB archive"]
fn live_public_compat_range() -> std::result::Result<(), String> {
    let source = content::ResolvedSource {
        provider: Provider::Modrinth,
        filename: "Homestead 2.0.1.mrpack".into(),
        download_url:
            "https://cdn.modrinth.com/data/6HvKwSky/versions/WjiUurYs/Homestead%202.0.1.mrpack"
                .into(),
        expected_hash: ExpectedHash::Sha1("554468613c025e1e7148c5fe7c0e802f7ada9aaa".into()),
    };
    let mut remote = RemoteZip::open(&source).map_err(|_| "range_open")?;
    // Precharge the existing48-request guard to allow at most16additional requests.
    // This is solely a stricter test budget; production range policy is untouched.
    remote.requests = 32;
    let mut zip = zip::ZipArchive::new(remote).map_err(|_| "range_zip")?;
    let mut file = zip
        .by_name("modrinth.index.json")
        .map_err(|e| format!("range_manifest:{e}"))?;
    if file.size() > 256 * 1024 {
        return Err("range_metadata_cap".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "range_body")?;
    drop(file);
    assert!(bytes.len() <= 256 * 1024);
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "range_json")?;
    assert_eq!(value["game"], "minecraft");
    let remote = zip.into_inner();
    let requests = 2 + remote.requests - 32;
    save(
        "ranges",
        json!({"requests":requests,"max_received_body_bytes":requests*65537,"manifest_bytes":bytes.len(),"whole_archive_digest_verified":false,"source":"Homestead2.0.1","transport":"hardened control RemoteZip"}),
    );
    eprintln!(
        "PUBLIC_COMPAT ranges requests={requests} manifest_bytes={} whole_archive_digest=false",
        bytes.len()
    );
    Ok(())
}
#[test]
#[ignore = "two small public GETs through hardened job pool; counts actual dial attempts"]
fn live_public_compat_pool() -> std::result::Result<(), String> {
    let http = UreqDownloadHttp::for_job();
    let before = content::public_transport::LIVE_DIAL_ATTEMPTS.load(Ordering::SeqCst);
    let mut timings = Vec::new();
    for _ in 0..2 {
        let start = Instant::now();
        let (bytes, _) = get(
            &http,
            "https://maven.fabricmc.net/net/fabricmc/intermediary/1.21.1/intermediary-1.21.1.pom",
        )?;
        assert!(!bytes.is_empty());
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let dials = content::public_transport::LIVE_DIAL_ATTEMPTS.load(Ordering::SeqCst) - before;
    save(
        "server-pool",
        json!({"requests":2,"dial_attempts":dials,"wall_ms":timings,"pool":"hardened job agent idle2","note":"dial attempts may exceed connections if an address fails"}),
    );
    eprintln!("PUBLIC_COMPAT job_pool requests=2 dial_attempts={dials} wall_ms={timings:?}");
    Ok(())
}
