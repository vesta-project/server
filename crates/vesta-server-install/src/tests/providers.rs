use super::*;

#[tokio::test]
#[ignore = "live Forge/Mojang metadata resolution for RLCraft's exact legacy runtime"]
async fn live_rlcraft_legacy_forge_2860_resolves() {
    let root = tempfile::tempdir().unwrap();
    let installer = ServerInstaller::new(InstallRoots::new(root.path(), &[]).unwrap(), TransportConfig::direct()).unwrap();
    let selection = Selection { version: Some("1.12.2".into()), loader_version: Some("14.23.5.2860".into()), ..Selection::default() };
    let mut job = super::super::transfer::Job::new(Limits::default(), 0);
    let resolved = super::super::providers::resolve(&installer.http, Software::Forge, &selection, &mut job, &SilentProgressReporter).await.unwrap();
    assert_eq!(resolved.version, "1.12.2");
    assert_eq!(resolved.java.major, 8);
    assert!(resolved.processed.unwrap().legacy.is_some());
    eprintln!("RLCRAFT software plan: Forge 14.23.5.2860 / Minecraft 1.12.2 / Java 8");
}

#[tokio::test]
async fn catalogue_queries_finish_before_the_launcher_request_deadline() {
    let mut versions = Fixture::new(Software::Paper).await;
    let mut releases = Fixture::new(Software::Paper).await;
    // Installation jobs may run for ten minutes. Browsing catalogues must not
    // retain the agent query slot after its 15-second client has timed out.
    for fixture in [&mut versions, &mut releases] {
        fixture.installer.limits.total_ms = Limits::default().total_ms;
        fixture.installer.limits.idle_ms = Limits::default().idle_ms;
    }
    for (fixture, route) in [
        (&versions, "/v3/projects/paper"),
        (&releases, "/v3/projects/paper/versions/1.21.4"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .with_priority(1)
            .mount(&fixture.server)
            .await;
    }
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(14), async {
        tokio::join!(
            versions
                .installer
                .versions(Software::Paper, &SilentProgressReporter),
            releases
                .installer
                .releases(Software::Paper, "1.21.4", &SilentProgressReporter),
        )
    })
    .await
    .expect("catalogue outlived the launcher's request budget");
    assert_eq!(result.0.unwrap_err(), InstallError::Deadline);
    assert_eq!(result.1.unwrap_err(), InstallError::Deadline);
    assert!(start.elapsed() < Duration::from_secs(14));
    for fixture in [&versions, &releases] {
        assert_eq!(fixture.server.received_requests().await.unwrap().len(), 1);
        assert!(entries(fixture.root.path()).is_empty());
    }
}

#[tokio::test]
async fn catalogue_deadline_preserves_tighter_caller_limits() {
    let mut fixture = Fixture::new(Software::Paper).await;
    fixture.installer.limits.total_ms = 50;
    Mock::given(method("GET"))
        .and(path("/v3/projects/paper"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
        .with_priority(1)
        .mount(&fixture.server)
        .await;
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        fixture
            .installer
            .versions(Software::Paper, &SilentProgressReporter),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), InstallError::Deadline);
    assert_eq!(fixture.installer.limits.total_ms, 50);
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 1);
}

async fn override_json(f: &Fixture, route: &str, value: Value) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(value))
        .with_priority(1)
        .mount(&f.server)
        .await;
}
async fn override_raw(f: &Fixture, route: &str, value: Vec<u8>) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(value))
        .with_priority(1)
        .mount(&f.server)
        .await;
}
macro_rules! provider_case {
    ($name:ident,$s:expr) => {
        #[tokio::test]
        async fn $name() {
            let f = Fixture::new($s).await;
            let p = f.plan().await;
            assert_eq!(p.software(), $s);
            assert_eq!(p.version(), "1.21.4");
            assert_eq!(
                p.build(),
                if $s == Software::Vanilla {
                    None
                } else {
                    Some(7)
                }
            );
            assert_eq!(p.java_requirement().major, 21);
            assert!(entries(f.root.path()).is_empty());
            assert!(!f
                .server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path() == f.artifact_path));
        }
    };
}
provider_case!(
    vanilla_resolves_server_not_client_or_mappings,
    Software::Vanilla
);
provider_case!(paper_parses_real_fill_shape, Software::Paper);
provider_case!(
    folia_parses_grouped_versions_and_java_minimum,
    Software::Folia
);
provider_case!(
    purpur_pins_numeric_build_md5_and_unknown_size,
    Software::Purpur
);
#[tokio::test]
async fn legacy_vanilla_labels_do_not_break_modern_catalogues() {
    let f = Fixture::new(Software::Vanilla).await;
    override_json(&f,"/minecraft/v0/manifest.json",json!({"latest":{"release":"1.21.4"},"versions":[
        {"id":"1.21.4","type":"release","url":"https://launcher-meta.modrinth.com/minecraft/v0/versions/1.21.4.json","sha1":"a".repeat(40)},
        {"id":"3D Shareware v1.34","type":"snapshot","url":"https://untrusted.invalid/legacy.json","sha1":"a".repeat(40)}]})).await;
    assert_eq!(
        f.installer
            .versions(Software::Vanilla, &SilentProgressReporter)
            .await
            .unwrap(),
        ["1.21.4"]
    );
}
#[tokio::test]
async fn purpur_default_skips_failed_builds_with_bounded_search() {
    let f = Fixture::new(Software::Purpur).await;
    override_json(&f,"/v2/purpur/1.21.4/7",json!({"project":"purpur","version":"1.21.4","build":"7","result":"FAILURE","md5":digest(DigestAlgorithm::Md5,BODY)})).await;
    mount_json(&f.server,"/v2/purpur/1.21.4/6",json!({"project":"purpur","version":"1.21.4","build":"6","result":"SUCCESS","md5":digest(DigestAlgorithm::Md5,BODY)})).await;
    assert_eq!(f.plan().await.build(), Some(6));
}
#[tokio::test]
async fn experimental_opt_in_still_prefers_stable_unless_exact_build_chosen() {
    let f = Fixture::new(Software::Paper).await;
    let artifact = json!({"name":"paper-fixture.jar","size":BODY.len(),"checksums":{"sha256":model::sha256(BODY)},"url":format!("https://fill-data.papermc.io/v1/objects/{}/paper-fixture.jar",model::sha256(BODY))});
    override_json(
        &f,
        "/v3/projects/paper/versions/1.21.4/builds",
        json!([
        {"id":7,"channel":"ALPHA","downloads":{"server:default":artifact.clone()}},
        {"id":6,"channel":"STABLE","downloads":{"server:default":artifact}}]),
    )
    .await;
    let select = Selection {
        experimental: true,
        ..Default::default()
    };
    let p = f
        .installer
        .plan(
            Software::Paper,
            &select,
            &std::env::current_exe().unwrap(),
            &SilentProgressReporter,
        )
        .await
        .unwrap();
    assert_eq!(p.build(), Some(6));
    assert!(!p.warnings().contains(&Warning::ExperimentalBuild));
    let select = Selection {
        build: Some(7),
        experimental: true,
        ..Default::default()
    };
    let p = f
        .installer
        .plan(
            Software::Paper,
            &select,
            &std::env::current_exe().unwrap(),
            &SilentProgressReporter,
        )
        .await
        .unwrap();
    assert_eq!(p.build(), Some(7));
    assert!(p.warnings().contains(&Warning::ExperimentalBuild));
}
#[tokio::test]
async fn forge_versions_before_the_supported_baseline_are_refused_without_network_or_writes() {
    let f = Fixture::new(Software::Paper).await;
    for s in [Software::Forge, Software::NeoForge] {
        assert_eq!(
            f.installer
                .plan(
                    s,
                    &Selection {
                        version: Some("1.6.4".into()),
                        ..Selection::default()
                    },
                    &std::env::current_exe().unwrap(),
                    &SilentProgressReporter
                )
                .await
                .unwrap_err(),
            InstallError::Unavailable
        );
    }
    assert!(f.server.received_requests().await.unwrap().is_empty());
    assert!(entries(f.root.path()).is_empty());
}
#[tokio::test]
async fn identities_duplicates_missing_fields_and_utf8_are_rejected() {
    for data in [
        br#"{"project":{"id":"folia"},"versions":{"1.21":["1.21.4"]}}"#.to_vec(),
        br#"{"project":{"id":"paper","id":"paper"},"versions":{"1.21":["1.21.4"]}}"#.to_vec(),
        br#"{"project":{"id":"paper"},"versions":{"1.21":["1.21.4","1.21.4"]}}"#.to_vec(),
        br#"{"project":{"id":"paper"},"versions":{"1.21":["1.21.4"]}} {}"#.to_vec(),
        br#"{"project":null,"versions":{}}"#.to_vec(),
        vec![0xff],
    ] {
        let f = Fixture::new(Software::Paper).await;
        override_raw(&f, "/v3/projects/paper", data).await;
        assert_eq!(
            f.plan_result().await.unwrap_err(),
            InstallError::InvalidMetadata
        );
    }
}
#[tokio::test]
async fn metadata_stream_cap_and_collection_visitor_cap_are_real() {
    let mut f = Fixture::new(Software::Paper).await;
    f.installer.limits.metadata_bytes = 16;
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::LimitExceeded
    );
    let mut f = Fixture::new(Software::Paper).await;
    f.installer.limits.metadata_bytes = 16 * 1024 * 1024;
    f.installer.limits.total_bytes = 4 * 1024 * 1024;
    let list: Vec<_> = (0..10_001).map(|i| format!("1.21.{}", i)).collect();
    override_json(
        &f,
        "/v3/projects/paper",
        json!({"project":{"id":"paper"},"versions":{"1.21":list}}),
    )
    .await;
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::InvalidMetadata
    );
}
#[tokio::test]
async fn vanilla_mirror_hash_is_not_original_sha1() {
    let f = Fixture::new(Software::Vanilla).await;
    let p = f.plan().await;
    assert_eq!(p.digest().algorithm(), DigestAlgorithm::Sha1);
    assert!(p.warnings().contains(&Warning::LegacyChecksum));
    override_json(&f,"/minecraft/v0/manifest.json",json!({"latest":{"release":"1.21.4"},"versions":[{
        "id":"1.21.4","type":"release","url":"https://launcher-meta.modrinth.com/minecraft/v0/versions/1.21.4.json",
        "sha1":"0".repeat(40),"originalSha1":"0".repeat(40)}]})).await;
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::ArtifactChanged
    );
}
#[tokio::test]
async fn unsafe_selected_versions_are_refused_before_requests() {
    let f = Fixture::new(Software::Paper).await;
    for v in ["../server", "a/b", "a\\b", "x%2f..", "x\n", ""] {
        let s = Selection {
            version: Some(v.into()),
            ..Default::default()
        };
        assert_eq!(
            f.installer
                .plan(
                    Software::Paper,
                    &s,
                    &std::env::current_exe().unwrap(),
                    &SilentProgressReporter
                )
                .await
                .unwrap_err(),
            InstallError::InvalidInput
        );
    }
    assert!(f.server.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn fill_unmatched_build_ids_unsafe_filename_and_hash_path_mismatch_reject() {
    for (id, name, hash, urlhash) in [
        (8, "paper.jar", "a".repeat(64), "a".repeat(64)),
        (7, "../paper.jar", "a".repeat(64), "a".repeat(64)),
        (7, "paper.jar", "a".repeat(64), "b".repeat(64)),
    ] {
        let f = Fixture::new(Software::Paper).await;
        override_json(&f,"/v3/projects/paper/versions/1.21.4/builds",json!([{"id":id,"channel":"STABLE","downloads":{"server:default":{
            "name":name,"size":5,"checksums":{"sha256":hash},"url":format!("https://fill-data.papermc.io/v1/objects/{}/paper.jar",urlhash)}}}])).await;
        let error = f.plan_result().await.unwrap_err();
        assert!(matches!(
            error,
            InstallError::InvalidMetadata | InstallError::SourceRefused
        ));
    }
}
#[tokio::test]
async fn missing_runtime_declaration_is_not_guessed() {
    let f = Fixture::new(Software::Paper).await;
    override_json(
        &f,
        "/v3/projects/paper/versions/1.21.4",
        json!({"version":{"id":"1.21.4","java":null},"builds":[7]}),
    )
    .await;
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::RuntimeUnknown
    );
}
#[test]
fn purpur_runtime_policy_has_explicit_reviewed_cutoffs() {
    for v in ["1.20", "1.20.4", "1.21.11"] {
        assert_eq!(super::super::providers::purpur_java(v).unwrap().major, 21);
    }
    for v in ["26.1", "26.1.2", "26.3"] {
        assert_eq!(super::super::providers::purpur_java(v).unwrap().major, 25);
    }
    for v in ["1.19.4", "1.21.12", "26.3.1", "26.4", "bad"] {
        assert_eq!(
            super::super::providers::purpur_java(v).unwrap_err(),
            InstallError::RuntimeUnknown
        );
    }
}
#[tokio::test]
async fn version_order_is_numeric_not_lexicographic() {
    let f = Fixture::new(Software::Paper).await;
    override_json(
        &f,
        "/v3/projects/paper",
        json!({"project":{"id":"paper"},"versions":{"1.9":["1.9"],"1.21":["1.21.4"]}}),
    )
    .await;
    assert_eq!(f.plan().await.version(), "1.21.4");
}
#[tokio::test]
async fn purpur_explicit_failed_build_never_substitutes_another_build() {
    let f = Fixture::new(Software::Purpur).await;
    override_json(
        &f,
        "/v2/purpur/1.21.4/7",
        json!({"project":"purpur","version":"1.21.4","build":"7","result":"FAILURE","md5":null}),
    )
    .await;
    let selection = Selection {
        build: Some(7),
        ..Default::default()
    };
    assert_eq!(
        f.installer
            .plan(
                Software::Purpur,
                &selection,
                &std::env::current_exe().unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::Unavailable
    );
    assert!(!f
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.path().ends_with("/6")));
}
#[tokio::test]
async fn purpur_default_search_is_capped_at_five_candidate_requests() {
    let f = Fixture::new(Software::Purpur).await;
    override_json(&f,"/v2/purpur/1.21.4",json!({"project":"purpur","version":"1.21.4","builds":{"all":["1","2","3","4","5","6","7"]}})).await;
    for id in 3..=7 {
        override_json(&f,&format!("/v2/purpur/1.21.4/{id}"),json!({"project":"purpur","version":"1.21.4","build":id.to_string(),"result":"FAILURE"})).await;
    }
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::Unavailable
    );
    assert_eq!(f.server.received_requests().await.unwrap().len(), 7);
}
#[tokio::test]
async fn purpur_success_requires_checksum_and_canonical_identity() {
    for data in [
        json!({"project":"purpur","version":"1.21.4","build":"7","result":"SUCCESS"}),
        json!({"project":"purpur","version":"1.21.4","build":"07","result":"SUCCESS","md5":"a".repeat(32)}),
        json!({"project":"paper","version":"1.21.4","build":"7","result":"SUCCESS","md5":"a".repeat(32)}),
    ] {
        let f = Fixture::new(Software::Purpur).await;
        override_json(&f, "/v2/purpur/1.21.4/7", data).await;
        assert_eq!(
            f.plan_result().await.unwrap_err(),
            InstallError::InvalidMetadata
        );
    }
}
#[tokio::test]
async fn fill_experimental_default_is_unavailable_without_explicit_opt_in() {
    let f = Fixture::new(Software::Paper).await;
    let artifact = json!({"name":"paper-fixture.jar","size":BODY.len(),"checksums":{"sha256":model::sha256(BODY)},"url":format!("https://fill-data.papermc.io/v1/objects/{}/paper-fixture.jar",model::sha256(BODY))});
    override_json(
        &f,
        "/v3/projects/paper/versions/1.21.4/builds",
        json!([{"id":7,"channel":"ALPHA","downloads":{"server:default":artifact}}]),
    )
    .await;
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::Unavailable
    );
    let s = Selection {
        experimental: true,
        ..Default::default()
    };
    let p = f
        .installer
        .plan(
            Software::Paper,
            &s,
            &std::env::current_exe().unwrap(),
            &SilentProgressReporter,
        )
        .await
        .unwrap();
    assert!(p.warnings().contains(&Warning::ExperimentalBuild));
}
#[tokio::test]
async fn duplicate_legacy_vanilla_id_is_not_hidden_by_filtering() {
    let f = Fixture::new(Software::Vanilla).await;
    let entry = json!({"id":"3D Shareware v1.34","type":"snapshot","url":"https://untrusted.invalid/x","sha1":"a".repeat(40)});
    override_json(
        &f,
        "/minecraft/v0/manifest.json",
        json!({"latest":{"release":"1.21.4"},"versions":[entry.clone(),entry]}),
    )
    .await;
    assert_eq!(
        f.plan_result().await.unwrap_err(),
        InstallError::InvalidMetadata
    );
}
#[tokio::test]
async fn unsupported_upstream_state_requires_warning_review() {
    let f = Fixture::new(Software::Paper).await;
    override_json(&f,"/v3/projects/paper/versions/1.21.4",json!({"version":{"id":"1.21.4","java":{"version":{"minimum":21}},"support":{"status":"UNSUPPORTED"}},"builds":[7]})).await;
    assert!(f
        .plan()
        .await
        .warnings()
        .contains(&Warning::UnsupportedUpstream));
}

#[tokio::test]
async fn release_catalogues_are_metadata_only_and_builds_are_descending() {
    for software in [Software::Paper, Software::Folia, Software::Purpur] {
        let f = Fixture::new(software).await;
        let choices = f
            .installer
            .releases(software, "1.21.4", &SilentProgressReporter)
            .await
            .unwrap();
        assert_eq!(choices[0].build, Some(7));
        assert!(choices.windows(2).all(|w| w[0].build > w[1].build));
        assert!(choices.iter().all(|v| v.loader_version.is_none()));
        assert!(entries(f.root.path()).is_empty());
        assert!(!f
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == f.artifact_path));
    }
}
#[tokio::test]
async fn release_catalogue_rejects_duplicate_builds_and_unsafe_version() {
    let f = Fixture::new(Software::Paper).await;
    override_json(
        &f,
        "/v3/projects/paper/versions/1.21.4",
        json!({"version":{"id":"1.21.4"},"builds":[7,7]}),
    )
    .await;
    assert!(matches!(
        f.installer
            .releases(Software::Paper, "1.21.4", &SilentProgressReporter)
            .await,
        Err(InstallError::InvalidMetadata)
    ));
    let count = f.server.received_requests().await.unwrap().len();
    assert!(matches!(
        f.installer
            .releases(Software::Paper, "../x", &SilentProgressReporter)
            .await,
        Err(InstallError::InvalidInput)
    ));
    assert_eq!(count, f.server.received_requests().await.unwrap().len());
}
