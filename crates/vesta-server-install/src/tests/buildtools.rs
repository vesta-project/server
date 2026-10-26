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
        assert!(target.descriptor().automatic_backend);
    }
    assert_eq!(fixture.server.received_requests().await.unwrap().len(), 2);
    assert!(entries(fixture.root.path()).is_empty());
}
#[tokio::test]
async fn a_target_without_its_reference_hash_is_not_offered_as_a_verified_build() {
    let fixture = Fixture::new(Software::Paper).await;
    let mut source = metadata();
    source["hashes"]
        .as_object_mut()
        .unwrap()
        .remove("CraftBukkit");
    mount_json(&fixture.server, "/versions/1.21.8.json", source).await;
    assert!(
        fixture
            .installer
            .releases(Software::Bukkit, "1.21.8", &SilentProgressReporter)
            .await
            .unwrap()
            .is_empty()
    );
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
    use crate::transfer::{Role, validate_family_url};
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

#[tokio::test]
async fn source_plans_bind_target_hash_refs_tools_and_java_without_executing() {
    let fixture = Fixture::new(Software::Paper).await;
    mount_json(&fixture.server, "/versions/1.21.8.json", metadata()).await;
    for target in [Software::Bukkit, Software::Spigot] {
        let mut job = crate::transfer::Job::new(Limits::default(), 0);
        let resolved = crate::providers::resolve(
            &fixture.installer.http,
            target,
            &Selection {
                version: Some("1.21.8".into()),
                build: Some(4534),
                ..Selection::default()
            },
            &mut job,
            &SilentProgressReporter,
        )
        .await
        .unwrap();
        let recipe = resolved.source_build.unwrap();
        assert_eq!(recipe.java_min, 21);
        assert_eq!(recipe.java_max, 25);
        let value = serde_json::to_value(recipe).unwrap();
        assert_eq!(value["refs"]["BuildData"], "a".repeat(40));
        assert_eq!(value["tool"]["digest"]["algorithm"], "sha256");
        assert_eq!(value["seconds"], 3600);
        assert_eq!(
            value["reference_hash"].as_str().unwrap(),
            if target == Software::Bukkit {
                "e".repeat(64)
            } else {
                "f".repeat(64)
            }
        );
        assert!(resolved.warnings.contains(&Warning::SourceBuild));
    }
    assert!(entries(fixture.root.path()).is_empty());
}

#[tokio::test]
async fn source_plans_reject_unreviewed_builds_tools_and_unknown_java() {
    for mode in ["build", "tools", "java", "legacy"] {
        let fixture = Fixture::new(Software::Paper).await;
        let mut source = metadata();
        if mode == "tools" {
            source["toolsVersion"] = json!(202);
        }
        if mode == "java" {
            source.as_object_mut().unwrap().remove("javaVersions");
        }
        if mode == "legacy" {
            source["toolsVersion"] = json!(100);
        }
        mount_json(&fixture.server, "/versions/1.21.8.json", source).await;
        let result = crate::providers::resolve(
            &fixture.installer.http,
            Software::Spigot,
            &Selection {
                version: Some("1.21.8".into()),
                build: (mode == "build").then_some(4533),
                ..Selection::default()
            },
            &mut crate::transfer::Job::new(Limits::default(), 0),
            &SilentProgressReporter,
        )
        .await;
        assert!(result.is_err(), "{mode}");
        assert!(entries(fixture.root.path()).is_empty());
    }
}

#[test]
fn build_tool_policy_only_accepts_the_reviewed_immutable_release() {
    use crate::transfer::{Role, validate_family_url};
    let url = "https://hub.spigotmc.org/jenkins/job/BuildTools/201/artifact/target/BuildTools.jar";
    assert!(
        validate_family_url(url, SourceFamily::SpigotBuildTools, Role::Artifact, false).is_ok()
    );
    for value in [
        url.replace("201", "202"),
        url.replace("201", "lastSuccessfulBuild"),
        url.replace("hub.spigotmc.org", "evil.invalid"),
        format!("{url}?download=1"),
        url.replace("target/", "target/../target/"),
    ] {
        assert!(
            validate_family_url(
                &value,
                SourceFamily::SpigotBuildTools,
                Role::Artifact,
                false
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn built_receipts_launch_only_the_reverified_jar_and_reject_tampering() {
    for (software, mismatch) in [
        (Software::Bukkit, false),
        (Software::Spigot, false),
        (Software::Bukkit, true),
        (Software::Spigot, true),
    ] {
        let mut fixture = Fixture::new(Software::Paper).await;
        fixture.installer.limits.artifact_bytes = 4 * 1024 * 1024;
        let mut plan = fixture.plan().await;
        let mut source = metadata();
        let reference = if mismatch {
            "0".repeat(64)
        } else {
            model::sha256(BODY)
        };
        source["hashes"]["CraftBukkit"] = json!(reference);
        source["hashes"]["Spigot"] = json!(reference);
        mount_json(&fixture.server, "/versions/1.21.8.json", source).await;
        let resolved = crate::providers::resolve(
            &fixture.installer.http,
            software,
            &Selection {
                version: Some("1.21.8".into()),
                ..Selection::default()
            },
            &mut crate::transfer::Job::new(Limits::default(), 0),
            &SilentProgressReporter,
        )
        .await
        .unwrap();
        plan.schema = 4;
        plan.software = software;
        plan.source_build = resolved.source_build;
        plan.artifact = resolved.artifact;
        plan.build = resolved.build;
        plan.version = resolved.version;
        plan.metadata = resolved.metadata;
        let (sources, source_url, source_sha256) = plan
            .source_build
            .as_ref()
            .unwrap()
            .source_evidence(&plan)
            .unwrap();
        let pin = plan.artifact.as_ref().unwrap();
        let destination = plan.destination();
        for directory in [destination.join("control"), destination.join("software")] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let jar = destination.join(software.jar_path());
        std::fs::write(&jar, BODY).unwrap();
        let receipt = BuiltReceipt {
            schema: 4,
            install_id: plan.install_id.clone(),
            plan_hash: plan.hash(),
            software,
            version: plan.version.clone(),
            build: plan.build.unwrap(),
            sources,
            source_url,
            source_sha256,
            selected_java_major: plan.runtime.major,
            reference_sha256: plan.source_build.as_ref().unwrap().output_reference(),
            input: ArtifactRecord {
                path: "build/BuildTools.jar".into(),
                url: pin.url.clone(),
                family: SourceFamily::SpigotBuildTools,
                published_size: pin.size.unwrap(),
                actual_size: pin.size.unwrap(),
                published_digest: pin.digest.clone(),
                observed_sha256: pin.digest.hex().into(),
            },
            output: GeneratedFile {
                path: software.jar_path(),
                size: BODY.len() as u64,
                sha256: model::sha256(BODY),
            },
            java_requirement: plan.required_java.clone(),
            launch: plan.launch_recipe(),
            warnings: crate::providers::buildtools::output_warnings(&plan, &model::sha256(BODY)),
            completed_at: crate::now().unwrap(),
        };
        std::fs::write(
            destination.join("control/receipt.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        let game = tempfile::tempdir().unwrap();
        let launch = fixture
            .installer
            .managed_launch(&plan, game.path())
            .await
            .unwrap();
        assert_eq!(launch.args, ["-jar", jar.to_str().unwrap(), "nogui"]);
        assert_eq!(launch.required_files.as_slice(), std::slice::from_ref(&jar));
        assert_eq!(launch.artifacts.len(), 1);
        let verified = fixture.installer.verify(&plan).await.unwrap();
        assert_eq!(
            verified
                .warnings
                .contains(&Warning::SourceBuildReferenceMismatch),
            mismatch
        );
        if mismatch {
            let mut hidden = receipt.clone();
            hidden
                .warnings
                .retain(|warning| warning != &Warning::SourceBuildReferenceMismatch);
            std::fs::write(
                destination.join("control/receipt.json"),
                serde_json::to_vec(&hidden).unwrap(),
            )
            .unwrap();
            assert!(fixture.installer.verify(&plan).await.is_err());
            std::fs::write(
                destination.join("control/receipt.json"),
                serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(jar, b"tampered").unwrap();
        assert!(fixture.installer.verify(&plan).await.is_err());
    }
}
