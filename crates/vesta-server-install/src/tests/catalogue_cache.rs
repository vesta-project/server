use super::*;
use crate::catalogue_cache::{CatalogueCache, CatalogueRecord, CatalogueStore};
use std::{collections::HashMap, sync::Arc};

#[derive(Default)]
struct Store(Mutex<HashMap<String, CatalogueRecord>>);

#[test]
fn forge_browsing_mirror_never_grants_artifact_or_arbitrary_endpoint_access() {
    use crate::transfer::{validate_family_url, Role};
    let source = "https://launcher-meta.modrinth.com/forge/v0/manifest.json";
    assert!(
        validate_family_url(source, SourceFamily::ModrinthForge, Role::Metadata, false).is_ok()
    );
    assert!(
        validate_family_url(source, SourceFamily::ModrinthForge, Role::Artifact, false).is_err()
    );
    for source in [
        "https://launcher-meta.modrinth.com/forge/v0/server.jar",
        "https://launcher-meta.modrinth.com/forge/v0/no/../manifest.json",
        "https://untrusted.invalid/forge/v0/manifest.json",
    ] {
        assert!(
            validate_family_url(source, SourceFamily::ModrinthForge, Role::Metadata, false)
                .is_err()
        );
    }
}
impl CatalogueStore for Store {
    fn load(&self, key: &str) -> Option<CatalogueRecord> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn save(&self, records: &[(String, CatalogueRecord)]) {
        self.0.lock().unwrap().extend(records.iter().cloned());
    }
}

#[tokio::test]
async fn forge_versions_and_loader_choices_reuse_one_validated_source_document() {
    let fixture = Fixture::new(Software::Paper).await;
    Mock::given(method("GET")).and(path("/forge/v0/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"gameVersions":[{"id":"1.21.4","loaders":[{"id":"54.1.0","url":"https://untrusted.invalid/jar","stable":false},{"id":"54.1.1","stable":false}]}]})))
        .mount(&fixture.server).await;
    assert_eq!(
        fixture
            .installer
            .versions(Software::Forge, &SilentProgressReporter)
            .await
            .unwrap(),
        ["1.21.4"]
    );
    let releases = fixture
        .installer
        .releases(Software::Forge, "1.21.4", &SilentProgressReporter)
        .await
        .unwrap();
    assert_eq!(
        releases
            .iter()
            .map(|release| release.id.as_str())
            .collect::<Vec<_>>(),
        ["54.1.1", "54.1.0"]
    );
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn same_key_readers_share_one_fetch_and_warm_reads_do_no_network() {
    let fixture = Fixture::new(Software::Paper).await;
    let results = futures::future::join_all((0..20).map(|_| {
        fixture
            .installer
            .versions(Software::Paper, &SilentProgressReporter)
    }))
    .await;
    let mut accepted = 0;
    for result in results {
        match result {
            Ok(values) => {
                assert_eq!(values, ["1.21.4"]);
                accepted += 1;
            }
            Err(error) => assert_eq!(error, InstallError::Busy),
        }
    }
    assert_eq!(accepted, 16);
    fixture
        .installer
        .versions(Software::Paper, &SilentProgressReporter)
        .await
        .unwrap();
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 1);
    assert!(entries(fixture.root.path()).is_empty());
}

#[tokio::test]
async fn invalid_metadata_is_not_cached_and_planning_never_uses_browsing_data() {
    let fixture = Fixture::new(Software::Paper).await;
    Mock::given(method("GET"))
        .and(path("/v3/projects/paper"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"project":{"id":"wrong"},"versions":{"1.21":["1.21.4"]}})),
        )
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&fixture.server)
        .await;
    assert_eq!(
        fixture
            .installer
            .versions(Software::Paper, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::InvalidMetadata
    );
    assert_eq!(
        fixture
            .installer
            .versions(Software::Paper, &SilentProgressReporter)
            .await
            .unwrap(),
        ["1.21.4"]
    );
    Mock::given(method("GET"))
        .and(path("/v3/projects/paper"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"bad":"fresh source"})))
        .with_priority(1)
        .mount(&fixture.server)
        .await;
    assert_eq!(
        fixture.plan_result().await.unwrap_err(),
        InstallError::InvalidMetadata
    );
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn origin_no_store_and_no_cache_are_honoured_for_raw_and_normalized_results() {
    for policy in ["no-store", "no-cache", "max-age=0", "max-age=bad"] {
        let fixture = Fixture::new(Software::Paper).await;
        Mock::given(method("GET"))
            .and(path("/v3/projects/paper"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("cache-control", policy)
                    .set_body_json(
                        json!({"project":{"id":"paper"},"versions":{"1.21":["1.21.4"]}}),
                    ),
            )
            .with_priority(1)
            .mount(&fixture.server)
            .await;
        for _ in 0..2 {
            fixture
                .installer
                .versions(Software::Paper, &SilentProgressReporter)
                .await
                .unwrap();
        }
        assert_eq!(
            fixture.server.received_requests().await.unwrap().len(),
            2,
            "{policy}"
        );
    }
}

#[tokio::test]
async fn protected_snapshots_survive_cache_recreation_but_corruption_is_rejected() {
    let mut fixture = Fixture::new(Software::Paper).await;
    let store = Arc::new(Store::default());
    fixture.installer.http.catalogues = CatalogueCache::new(Some(store.clone()));
    fixture
        .installer
        .versions(Software::Paper, &SilentProgressReporter)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.0.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.installer.http.catalogues = CatalogueCache::new(Some(store.clone()));
    fixture
        .installer
        .versions(Software::Paper, &SilentProgressReporter)
        .await
        .unwrap();
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 1);
    for record in store.0.lock().unwrap().values_mut() {
        record.bytes = b"poisoned".to_vec();
    }
    fixture.installer.http.catalogues = CatalogueCache::new(Some(store));
    fixture
        .installer
        .versions(Software::Paper, &SilentProgressReporter)
        .await
        .unwrap();
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn cancellation_while_waiting_releases_query_admission() {
    let cache = CatalogueCache::new(None);
    let job = crate::transfer::Job::new(Limits::default(), 0);
    let owner = cache
        .query("same", &job, &SilentProgressReporter)
        .await
        .unwrap();
    let reporter = Reporter::default();
    let waiting = cache.query("same", &job, &reporter);
    let cancel = async {
        tokio::task::yield_now().await;
        reporter.cancel.store(true, Ordering::SeqCst);
    };
    let (result, _) = tokio::join!(waiting, cancel);
    assert!(matches!(result, Err(InstallError::Cancelled)));
    drop(owner);
    cache
        .query("same", &job, &SilentProgressReporter)
        .await
        .unwrap();
}
