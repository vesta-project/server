//! Mirror-resolved Vanilla objects are confirmed against Mojang's own manifest
//! before a host may accept their SHA-1 digests automatically.
use super::*;

const VERSION: &str = "1.21.4";

async fn vanilla_preview(f: &Fixture) -> ResolvedPreview {
    f.installer
        .resolve_preview(
            Software::Vanilla,
            &Selection {
                version: Some(VERSION.into()),
                ..Selection::default()
            },
            &SilentProgressReporter,
        )
        .await
        .unwrap()
}

/// Mount Mojang's manifest and version document; `server` is what Mojang
/// itself publishes. Returns the version document's SHA-1 path.
async fn mount_mojang(f: &Fixture, server: Value, manifest_sha1: Option<&str>) -> String {
    let detail = serde_json::to_vec(&json!({"id":VERSION,"downloads":{
        "server":server,
        "client":{"url":"https://piston-data.mojang.com/v1/objects/aa/client.jar","sha1":"bad"}}}))
    .unwrap();
    let sha1 = digest(DigestAlgorithm::Sha1, &detail);
    let route = format!("/v1/packages/{sha1}/{VERSION}.json");
    mount(
        &f.server,
        &route,
        ResponseTemplate::new(200).set_body_bytes(detail),
    )
    .await;
    mount_json(
        &f.server,
        "/mc/game/version_manifest_v2.json",
        json!({
        "latest":{"release":VERSION},
        "versions":[{"id":VERSION,"type":"release",
            "url":format!("https://piston-meta.mojang.com{route}"),
            "sha1":manifest_sha1.unwrap_or(&sha1)}]}),
    )
    .await;
    route
}

fn official(preview: &ResolvedPreview) -> Value {
    let object = &preview.mojang_objects[0];
    json!({"url":object.url,"sha1":object.sha1,"size":BODY.len()})
}

#[tokio::test]
async fn matching_mojang_data_is_accepted() {
    let f = Fixture::new(Software::Vanilla).await;
    let preview = vanilla_preview(&f).await;
    assert!(preview.legacy_checksum && preview.legacy_checksum_mojang_only);
    assert_eq!(preview.mojang_objects.len(), 1);
    assert_eq!(
        preview.mojang_objects[0].url,
        format!("https://piston-data.mojang.com{}", f.artifact_path)
    );
    mount_mojang(&f, official(&preview), None).await;
    f.installer
        .verify_mojang_objects(&preview, &SilentProgressReporter)
        .await
        .unwrap();
    // The manifest is cached briefly like the other catalogues.
    f.installer
        .verify_mojang_objects(&preview, &SilentProgressReporter)
        .await
        .unwrap();
    let manifests = f
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/mc/game/version_manifest_v2.json")
        .count();
    assert_eq!(manifests, 1);
}

#[tokio::test]
async fn a_mirror_that_differs_from_mojang_is_an_integrity_conflict() {
    // Different SHA-1 for the same URL.
    let f = Fixture::new(Software::Vanilla).await;
    let preview = vanilla_preview(&f).await;
    let mut server = official(&preview);
    server["sha1"] = json!(digest(DigestAlgorithm::Sha1, b"another jar"));
    mount_mojang(&f, server, None).await;
    assert_eq!(
        f.installer
            .verify_mojang_objects(&preview, &SilentProgressReporter)
            .await,
        Err(InstallError::IntegrityConflict)
    );

    // Same SHA-1 at a different URL.
    let f = Fixture::new(Software::Vanilla).await;
    let preview = vanilla_preview(&f).await;
    let mut server = official(&preview);
    server["url"] = json!(format!(
        "https://piston-data.mojang.com/v1/objects/{}/other.jar",
        preview.mojang_objects[0].sha1
    ));
    mount_mojang(&f, server, None).await;
    assert_eq!(
        f.installer
            .verify_mojang_objects(&preview, &SilentProgressReporter)
            .await,
        Err(InstallError::IntegrityConflict)
    );

    // The version document does not match the SHA-1 in Mojang's manifest.
    let f = Fixture::new(Software::Vanilla).await;
    let preview = vanilla_preview(&f).await;
    let wrong = digest(DigestAlgorithm::Sha1, b"not the document");
    mount_mojang(&f, official(&preview), Some(&wrong)).await;
    assert_eq!(
        f.installer
            .verify_mojang_objects(&preview, &SilentProgressReporter)
            .await,
        Err(InstallError::IntegrityConflict)
    );

    // Mojang does not publish this version at all.
    let f = Fixture::new(Software::Vanilla).await;
    let preview = vanilla_preview(&f).await;
    mount_json(
        &f.server,
        "/mc/game/version_manifest_v2.json",
        json!({"latest":{"release":"1.0"},"versions":[]}),
    )
    .await;
    assert_eq!(
        f.installer
            .verify_mojang_objects(&preview, &SilentProgressReporter)
            .await,
        Err(InstallError::IntegrityConflict)
    );
}

#[tokio::test]
async fn a_version_document_with_the_wrong_digest_is_refused() {
    let f = Fixture::new(Software::Vanilla).await;
    let preview = vanilla_preview(&f).await;
    let route = mount_mojang(&f, official(&preview), None).await;
    // Serve a different document at the content-addressed path Mojang names.
    Mock::given(method("GET"))
        .and(path(route.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"{\"id\":\"1.21.4\"}".to_vec()))
        .with_priority(1)
        .mount(&f.server)
        .await;
    assert_eq!(
        f.installer
            .verify_mojang_objects(&preview, &SilentProgressReporter)
            .await,
        Err(InstallError::IntegrityConflict)
    );
}

#[tokio::test]
async fn unreachable_mojang_is_never_silent_acceptance() {
    let f = Fixture::new(Software::Vanilla).await;
    let preview = vanilla_preview(&f).await;
    Mock::given(method("GET"))
        .and(path("/mc/game/version_manifest_v2.json"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&f.server)
        .await;
    let result = f
        .installer
        .verify_mojang_objects(&preview, &SilentProgressReporter)
        .await;
    assert!(result.is_err());
    assert_ne!(result, Err(InstallError::IntegrityConflict));
}

#[tokio::test]
async fn previews_without_mojang_objects_are_not_verifiable() {
    let f = Fixture::new(Software::Paper).await;
    let preview = f
        .installer
        .resolve_preview(
            Software::Paper,
            &Selection::default(),
            &SilentProgressReporter,
        )
        .await
        .unwrap();
    assert!(preview.mojang_objects.is_empty());
    assert_eq!(
        f.installer
            .verify_mojang_objects(&preview, &SilentProgressReporter)
            .await,
        Err(InstallError::InvalidInput)
    );
}

#[test]
fn mojang_metadata_urls_are_confined() {
    use crate::transfer::{validate_family_url, Role};
    let ok = [
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
        "https://piston-meta.mojang.com/v1/packages/0123456789abcdef0123456789abcdef01234567/1.21.4.json",
    ];
    for url in ok {
        assert!(validate_family_url(url, SourceFamily::MojangMeta, Role::Metadata, false).is_ok());
        assert!(validate_family_url(url, SourceFamily::MojangMeta, Role::Artifact, false).is_err());
        assert!(validate_family_url(url, SourceFamily::MojangMeta, Role::Metadata, true).is_err());
    }
    for url in [
        "https://evil.example/mc/game/version_manifest_v2.json",
        "https://piston-meta.mojang.com/mc/game/version_manifest.json",
        "https://piston-meta.mojang.com/v1/packages/abc/1.21.4.json",
        "https://piston-meta.mojang.com/v1/packages/0123456789abcdef0123456789abcdef01234567/../x.json",
        "https://piston-data.mojang.com/v1/objects/0123456789abcdef0123456789abcdef01234567/server.jar",
    ] {
        assert!(
            validate_family_url(url, SourceFamily::MojangMeta, Role::Metadata, false).is_err(),
            "{url}"
        );
    }
}
