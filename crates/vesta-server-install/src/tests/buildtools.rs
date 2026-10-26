use super::*;

fn metadata() -> Value {
    json!({"name":"4534","refs":{"BuildData":"a".repeat(40),"Bukkit":"b".repeat(40),"CraftBukkit":"c".repeat(40),"Spigot":"d".repeat(40)},"toolsVersion":181,"javaVersions":[65,69],"hashes":{"CraftBukkit":"e".repeat(64),"Spigot":"f".repeat(64)}})
}
#[tokio::test]
async fn both_targets_share_catalogue_and_version_documents_without_build_execution() {
    let fixture = Fixture::new(Software::Paper).await;
    mount(&fixture.server, "/versions/", ResponseTemplate::new(200).set_body_string(
        r#"<a href="1.9.4.json">old</a><a href="1.21.8.json">new</a><a href="latest.json">alias</a><a href="../../bad.json">bad</a>"#,
    )).await;
    mount_json(&fixture.server, "/versions/1.21.8.json", metadata()).await;
    for target in [Software::Bukkit, Software::Spigot] {
        assert_eq!(
            fixture
                .installer
                .versions(target, &SilentProgressReporter)
                .await
                .unwrap(),
            ["1.21.8", "1.9.4"]
        );
        let releases = fixture
            .installer
            .releases(target, "1.21.8", &SilentProgressReporter)
            .await
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].build, Some(4534));
        assert!(!target.descriptor().automatic_backend);
        assert_eq!(
            fixture
                .installer
                .plan(
                    target,
                    &Selection {
                        version: Some("1.21.8".into()),
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
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 2);
    assert!(entries(fixture.root.path()).is_empty());
}
#[tokio::test]
async fn target_without_published_output_digest_is_not_a_build_choice() {
    let fixture = Fixture::new(Software::Paper).await;
    let mut source = metadata();
    source["hashes"]
        .as_object_mut()
        .unwrap()
        .remove("CraftBukkit");
    mount_json(&fixture.server, "/versions/1.21.8.json", source).await;
    assert!(fixture
        .installer
        .releases(Software::Bukkit, "1.21.8", &SilentProgressReporter)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .installer
            .releases(Software::Spigot, "1.21.8", &SilentProgressReporter)
            .await
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn buildtools_metadata_policy_refuses_artifacts_aliases_and_url_tricks() {
    use crate::transfer::{validate_family_url, Role};
    let valid = "https://hub.spigotmc.org/versions/1.21.8.json";
    assert!(
        validate_family_url(valid, SourceFamily::SpigotVersions, Role::Metadata, false).is_ok()
    );
    assert!(
        validate_family_url(valid, SourceFamily::SpigotVersions, Role::Artifact, false).is_err()
    );
    for invalid in [
        "https://hub.spigotmc.org/versions/latest.json",
        "https://hub.spigotmc.org/versions/../versions/1.21.8.json",
        "https://hub.spigotmc.org/jenkins/job/BuildTools/201/artifact/target/BuildTools.jar",
        "https://evil.invalid/versions/1.21.8.json",
    ] {
        assert!(
            validate_family_url(invalid, SourceFamily::SpigotVersions, Role::Metadata, false)
                .is_err()
        );
    }
}
