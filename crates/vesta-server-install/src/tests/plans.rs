use super::*;

type LimitsMutation = Box<dyn Fn(&mut Limits)>;
type InstallPlanMutation = Box<dyn Fn(&mut InstallPlan)>;

#[test]
fn canonical_snapshot_matches_independent_node_hash_vector() {
    let p = InstallPlan {
        source_build: None,
        processed: None,
        schema: 1,
        install_id: "srv_fixture".into(),
        software: Software::Paper,
        version: "1.21.4".into(),
        build: Some(7),
        artifact: Some(model::ArtifactPin {
            url: format!(
                "https://fill-data.papermc.io/v1/objects/{}/paper.jar",
                "a".repeat(64)
            ),
            digest: model::PublishedDigest::new(DigestAlgorithm::Sha256, "a".repeat(64)).unwrap(),
            size: Some(35),
        }),
        runtime: super::super::runtime::RuntimeBinding {
            path: PathBuf::from("/java"),
            major: 21,
            bytes: 42,
            modified_ns: 123,
        },
        required_java: JavaRequirement {
            major: 21,
            evidence: "fixture".into(),
        },
        base: PathBuf::from("/server-base"),
        forbidden: vec![PathBuf::from("/client-root")],
        expires: 1900000000,
        warnings: vec![Warning::FirstRunEffects],
        metadata: vec![model::MetadataRevision {
            url: "https://fill.papermc.io/v3/projects/paper".into(),
            observed_sha256: "b".repeat(64),
        }],
        transferred: 200,
        limits: Limits::default(),
        connection_policy: "fixture-policy".into(),
        bundle: None,
    };
    assert_eq!(
        p.hash(),
        "3a8e8d96353035b0cd4542ec4cb671c1165d19197d3bdf3d48298bb3fecbd7ed"
    );
}

#[test]
fn descriptors_include_both_shared_buildtools_targets() {
    let mut ids = std::collections::HashSet::new();
    for s in Software::ALL {
        let d = s.descriptor();
        assert!(ids.insert(s.id()));
        assert!(d.automatic_backend);
        assert!(d.unavailable_reason.is_none());
        for url in [d.source, d.licence, d.eula] {
            assert!(url.starts_with("https://"));
        }
    }
    assert_eq!(ids.len(), 10);
    for id in [
        "vanilla", "paper", "folia", "purpur", "fabric", "quilt", "forge", "neoforge", "bukkit",
        "spigot",
    ] {
        assert!(ids.contains(id));
    }
    assert_eq!(
        Software::Folia.descriptor().caution,
        Some("region_threading_compatibility")
    );
}
#[test]
fn published_hash_validation_and_strength_are_exact() {
    for algorithm in [
        DigestAlgorithm::Sha256,
        DigestAlgorithm::Sha1,
        DigestAlgorithm::Md5,
    ] {
        let good = digest(algorithm, BODY);
        assert_eq!(
            model::PublishedDigest::new(algorithm, good.to_uppercase())
                .unwrap()
                .hex(),
            good
        );
        for bad in [
            String::new(),
            "0".into(),
            good.clone() + "0",
            good.replace('a', "g") + "g",
        ] {
            assert_eq!(
                model::PublishedDigest::new(algorithm, bad).unwrap_err(),
                InstallError::InvalidMetadata
            );
        }
        assert_eq!(algorithm.legacy(), algorithm != DigestAlgorithm::Sha256);
    }
}
#[test]
fn limits_only_tighten_and_keep_the_storage_floor() {
    let defaults = Limits::default();
    assert!(defaults.validate().is_ok());
    let mutations: Vec<LimitsMutation> = vec![
        Box::new(|v| v.metadata_bytes = 0),
        Box::new(|v| v.artifact_bytes += 1),
        Box::new(|v| v.total_bytes += 1),
        Box::new(|v| v.reserve_bytes -= 1),
        Box::new(|v| v.max_entries = 0),
        Box::new(|v| v.attempts = 4),
        Box::new(|v| v.redirects = 4),
        Box::new(|v| v.connect_ms = 0),
        Box::new(|v| v.idle_ms = 0),
        Box::new(|v| v.total_ms = 0),
    ];
    for change in mutations {
        let mut v = defaults;
        change(&mut v);
        assert_eq!(v.validate().unwrap_err(), InstallError::InvalidInput);
    }
}
#[test]
fn error_messages_carry_no_input_or_causes() {
    use std::error::Error;
    for e in [
        InstallError::InvalidMetadata,
        InstallError::SourceRefused,
        InstallError::TransportFailed,
        InstallError::RuntimeFailed,
        InstallError::PublishFailed,
    ] {
        assert!(e.to_string().starts_with("server_install."));
        assert!(e.to_string().len() < 64);
        assert!(e.source().is_none());
    }
}
#[tokio::test]
async fn all_effective_plan_fields_and_expiry_are_bound() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let hash = p.hash();
    let changes: Vec<InstallPlanMutation> = vec![
        Box::new(|v| v.schema += 1),
        Box::new(|v| v.install_id.push('a')),
        Box::new(|v| v.software = Software::Folia),
        Box::new(|v| v.version.push('1')),
        Box::new(|v| v.build = Some(8)),
        Box::new(|v| v.artifact.as_mut().unwrap().url.push('a')),
        Box::new(|v| v.artifact.as_mut().unwrap().size = Some(1)),
        Box::new(|v| v.artifact.as_mut().unwrap().digest.hex = "0".repeat(64)),
        Box::new(|v| v.artifact.as_mut().unwrap().digest.algorithm = DigestAlgorithm::Sha1),
        Box::new(|v| v.runtime.major += 1),
        Box::new(|v| v.runtime.path.push("changed")),
        Box::new(|v| v.runtime.bytes += 1),
        Box::new(|v| v.runtime.modified_ns += 1),
        Box::new(|v| v.required_java.major += 1),
        Box::new(|v| v.required_java.evidence.push('a')),
        Box::new(|v| v.base.push("changed")),
        Box::new(|v| v.forbidden.push(PathBuf::from("/forbidden"))),
        Box::new(|v| v.expires += 1),
        Box::new(|v| v.warnings.clear()),
        Box::new(|v| v.metadata[0].observed_sha256 = "0".repeat(64)),
        Box::new(|v| v.metadata[0].url.push('a')),
        Box::new(|v| v.transferred += 1),
        Box::new(|v| v.limits.total_bytes += 1),
        Box::new(|v| v.connection_policy.push('a')),
    ];
    for change in changes {
        let mut changed = p.clone();
        change(&mut changed);
        assert_ne!(changed.hash(), hash);
    }
    assert_eq!(p.clone().hash(), hash);
    assert!(entries(f.root.path()).is_empty());
}
#[tokio::test]
async fn review_requires_both_legacy_and_warning_acknowledgment() {
    let f = Fixture::new(Software::Purpur).await;
    let p = f.plan().await;
    for ack in [
        Acknowledgments::default(),
        Acknowledgments {
            legacy_checksum: true,
            warnings: false,
        },
        Acknowledgments {
            legacy_checksum: false,
            warnings: true,
        },
    ] {
        assert_eq!(
            Approval::review(&p, ack).unwrap_err(),
            InstallError::ApprovalRequired
        );
    }
    assert!(Approval::review(&p, ACK).is_ok());
    let mut expired = p.clone();
    expired.expires = 0;
    assert_eq!(
        Approval::review(&expired, ACK).unwrap_err(),
        InstallError::Expired
    );
}
#[tokio::test]
async fn plans_do_not_accept_jvm_flags_or_eula_and_have_a_fixed_literal_recipe() {
    let f = Fixture::new(Software::Folia).await;
    let p = f.plan().await;
    let r = p.launch_recipe();
    assert_eq!(r.kind, "java-jar");
    assert_eq!(r.args, ["nogui"]);
    assert_eq!(r.stop_mode, "console-stop");
    assert_eq!(r.artifact, Some(p.destination().join("software/folia.jar")));
    assert_eq!(r.argfile, None);
    assert_eq!(r.working_directory, p.destination().join("game"));
    let wire = serde_json::to_string(&p).unwrap();
    assert!(!wire.contains("javaagent"));
    assert!(!wire.contains("eula=true"));
    assert!(!format!("{p:?}").contains(&f.root.path().to_string_lossy().to_string()));
}
