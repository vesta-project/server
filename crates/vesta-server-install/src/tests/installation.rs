use super::*;

#[tokio::test]
async fn managed_switch_recipes_preserve_existing_game_for_all_jar_providers() {
    for software in [
        Software::Vanilla,
        Software::Paper,
        Software::Folia,
        Software::Purpur,
    ] {
        let f = Fixture::new(software).await;
        let (p, _) = installed(&f).await;
        let game = tempfile::tempdir().unwrap();
        std::fs::write(game.path().join("server.properties"), b"motd=preserved\n").unwrap();
        std::fs::create_dir(game.path().join("world")).unwrap();
        std::fs::write(game.path().join("world/level.dat"), b"world preserved").unwrap();
        let launch = f.installer.managed_launch(&p, game.path()).await.unwrap();
        assert_eq!(
            launch.working_directory,
            dunce::canonicalize(game.path()).unwrap()
        );
        assert_eq!(
            launch.args,
            [
                "-jar".into(),
                p.destination()
                    .join(software.jar_path())
                    .to_string_lossy()
                    .into_owned(),
                "nogui".into()
            ]
        );
        assert_eq!(launch.required_files.len(), 1);
        assert_eq!(launch.artifacts.len(), 1);
        crate::verify_managed_artifacts(&launch.artifacts)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(game.path().join("world/level.dat")).unwrap(),
            b"world preserved"
        );
        assert_eq!(
            std::fs::read(game.path().join("server.properties")).unwrap(),
            b"motd=preserved\n"
        );
        assert!(matches!(
            f.installer
                .managed_launch(&p, &p.destination().join("game"))
                .await,
            Err(InstallError::PathRefused)
        ));
        std::fs::write(&launch.required_files[0], b"tampered").unwrap();
        assert!(matches!(
            crate::verify_managed_artifacts(&launch.artifacts).await,
            Err(InstallError::ArtifactChanged)
        ));
        assert!(f.installer.managed_launch(&p, game.path()).await.is_err());
    }
}

#[tokio::test]
async fn managed_rollback_evidence_refuses_invalid_paths_sizes_and_digest_before_reading() {
    use crate::ManagedArtifact;
    let file = tempfile::NamedTempFile::new().unwrap();
    for evidence in [
        ManagedArtifact {
            path: PathBuf::from("relative.jar"),
            size: 1,
            sha256: "f".repeat(64),
        },
        ManagedArtifact {
            path: file.path().into(),
            size: 0,
            sha256: "f".repeat(64),
        },
        ManagedArtifact {
            path: file.path().into(),
            size: 1,
            sha256: "bad".into(),
        },
        ManagedArtifact {
            path: file.path().into(),
            size: 1024 * 1024 * 1024 + 1,
            sha256: "f".repeat(64),
        },
    ] {
        assert!(matches!(
            crate::verify_managed_artifacts(&[evidence]).await,
            Err(InstallError::ReceiptInvalid)
        ));
    }
}
async fn installed(f: &Fixture) -> (InstallPlan, Receipt) {
    let p = f.plan().await;
    f.artifact().await;
    let result = f
        .installer
        .install(
            &p,
            Approval::review(&p, ACK).unwrap(),
            &SilentProgressReporter,
        )
        .await
        .unwrap();
    let InstallOutcome::Installed(r) = result else {
        panic!("expected completed receipt")
    };
    (p, r)
}
fn no_staged_files(f: &Fixture) {
    let path = f.root.path().join(".staging");
    if path.exists() {
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
    }
}
macro_rules! installation_case {
    ($name:ident,$software:expr) => {
        #[tokio::test]
        async fn $name() {
            let f = Fixture::new($software).await;
            let (p, r) = installed(&f).await;
            let verified = f.installer.verify(&p).await.unwrap();
            assert_eq!(verified.plan_hash, p.hash());
            assert_eq!(verified.observed_sha256, model::sha256(BODY));
            assert_eq!(
                std::fs::read(p.destination().join($software.jar_path())).unwrap(),
                BODY
            );
            assert_eq!(r.actual_size, BODY.len() as u64);
            assert_eq!(r.eula, "not_accepted");
            assert!(std::fs::read_dir(p.destination().join("game"))
                .unwrap()
                .next()
                .is_none());
            assert!(p.destination().join("control/receipt.json").exists());
            assert_eq!(r.launch.args, ["nogui"]);
            no_staged_files(&f);
            let paths = entries(f.root.path());
            assert!(!paths.iter().any(|v| v.ends_with("eula.txt")
                || v.ends_with("server.properties")
                || v.ends_with("config.json")));
        }
    };
}
installation_case!(
    vanilla_fixture_installs_verified_server_jar_only,
    Software::Vanilla
);
installation_case!(
    paper_fixture_installs_exact_immutable_build,
    Software::Paper
);
installation_case!(
    folia_fixture_installs_region_threaded_recipe_not_game,
    Software::Folia
);
installation_case!(
    purpur_fixture_retains_md5_and_observed_sha256_separately,
    Software::Purpur
);
#[tokio::test]
async fn dry_run_has_no_network_probe_or_writes_and_catalogue_is_not_dry_run() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let before = f.server.received_requests().await.unwrap().len();
    let r = Reporter {
        dry: true,
        ..Default::default()
    };
    assert!(matches!(
        f.installer
            .install(&p, Approval::review(&p, ACK).unwrap(), &r)
            .await
            .unwrap(),
        InstallOutcome::DryRun
    ));
    assert_eq!(f.server.received_requests().await.unwrap().len(), before);
    assert!(entries(f.root.path()).is_empty());
    assert_eq!(
        f.installer.versions(Software::Paper, &r).await.unwrap_err(),
        InstallError::InvalidInput
    );
    assert_eq!(
        f.installer
            .plan(
                Software::Paper,
                &Selection::default(),
                &std::env::current_exe().unwrap(),
                &r
            )
            .await
            .unwrap_err(),
        InstallError::InvalidInput
    );
}
#[tokio::test]
async fn approval_tampering_expiry_and_other_installer_context_fail_before_effects() {
    let mut f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let a = Approval::review(&p, ACK).unwrap();
    let mut changed = p.clone();
    changed.build = Some(6);
    assert_eq!(
        f.installer
            .install(&changed, a, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::ApprovalMismatch
    );
    let mut expired = p.clone();
    expired.expires = 0;
    let a = Approval {
        hash: expired.hash(),
    };
    assert_eq!(
        f.installer
            .install(&expired, a, &SilentProgressReporter)
            .await
            .unwrap_err(),
        InstallError::Expired
    );
    f.installer.http.policy_id.push('x');
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::ApprovalMismatch
    );
    assert!(entries(f.root.path()).is_empty());
}
#[tokio::test]
async fn approval_from_another_transport_context_fails_before_effects() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let mut second = ServerInstaller::with_limits(
        f.installer.roots.clone(),
        f.installer.limits,
        TransportConfig::direct(),
    )
    .unwrap();
    // Keep even a regressed refusal on owned fixtures, never a real provider/probe.
    second.http.fixture_origin = f.installer.http.fixture_origin.clone();
    second.fixture_java = f.installer.fixture_java;
    second.fixture_space = f.installer.fixture_space.clone();
    assert_ne!(f.installer.http.policy_id, second.http.policy_id);
    let requests_before = f.server.received_requests().await.unwrap().len();

    assert_eq!(
        second
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::ApprovalMismatch
    );
    assert_eq!(
        f.server.received_requests().await.unwrap().len(),
        requests_before
    );
    assert!(entries(f.root.path()).is_empty());
}
#[tokio::test]
async fn changed_runtime_evidence_refuses_even_with_matching_review_hash() {
    let f = Fixture::new(Software::Paper).await;
    let mut p = f.plan().await;
    p.runtime.bytes += 1;
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::ApprovalMismatch
    );
    assert!(entries(f.root.path()).is_empty());
}
#[tokio::test]
async fn colliding_empty_or_populated_destination_is_never_adopted() {
    for populated in [false, true] {
        let f = Fixture::new(Software::Paper).await;
        let p = f.plan().await;
        std::fs::create_dir_all(p.destination()).unwrap();
        if populated {
            std::fs::write(p.destination().join("sentinel"), b"user data").unwrap();
        }
        let before = entries(f.root.path());
        let count = f.server.received_requests().await.unwrap().len();
        assert_eq!(
            f.installer
                .install(
                    &p,
                    Approval::review(&p, ACK).unwrap(),
                    &SilentProgressReporter
                )
                .await
                .unwrap_err(),
            InstallError::DestinationExists
        );
        assert_eq!(entries(f.root.path()), before);
        assert_eq!(f.server.received_requests().await.unwrap().len(), count);
    }
}
#[tokio::test]
async fn completed_install_is_not_overwritten_or_reresolved() {
    let f = Fixture::new(Software::Paper).await;
    let (p, _) = installed(&f).await;
    let count = f.server.received_requests().await.unwrap().len();
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::DestinationExists
    );
    assert_eq!(f.server.received_requests().await.unwrap().len(), count);
    assert_eq!(
        f.installer.verify(&p).await.unwrap().observed_sha256,
        model::sha256(BODY)
    );
}
#[tokio::test]
async fn low_and_unknown_storage_fail_before_staging_or_transfer() {
    let mut f = Fixture::new(Software::Purpur).await;
    let p = f.plan().await;
    let count = f.server.received_requests().await.unwrap().len();
    f.installer.fixture_space = Some((
        f.root.path().to_path_buf(),
        p.limits.reserve_bytes + p.limits.artifact_bytes - 1,
    ));
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::StorageLow
    );
    f.installer.fixture_space_error = Some(InstallError::StorageUnknown);
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::StorageUnknown
    );
    assert!(entries(f.root.path()).is_empty());
    assert_eq!(f.server.received_requests().await.unwrap().len(), count);
}
#[test]
fn most_specific_volume_wins_independent_of_earlier_shallow_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let broad = dir.path().parent().unwrap().to_path_buf();
    let target = dir.path().to_path_buf();
    let volumes = vec![
        (broad.clone(), 100, false),
        (broad, 100, false),
        (target.clone(), 200, true),
    ];
    for shift in 0..3 {
        let mut v = volumes.clone();
        v.rotate_left(shift);
        let (chosen, free) = install::select_volume(&target, v).unwrap();
        assert_eq!(free, 200);
        assert!(install::overlap(&chosen, &target));
    }
    assert_eq!(
        install::select_volume(
            &target,
            vec![(target.clone(), 100, true), (target.clone(), 100, true)]
        )
        .unwrap_err(),
        InstallError::StorageUnknown
    );
    assert_eq!(
        install::select_volume(&target, vec![(target.clone(), 100, false)]).unwrap_err(),
        InstallError::StorageUnknown
    );
    assert_eq!(
        install::select_volume(&target, vec![]).unwrap_err(),
        InstallError::StorageUnknown
    );
}
#[test]
fn linux_mountinfo_rejects_hidden_network_and_unknown_mounts_before_parent_fallback() {
    let root = "20 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n";
    for filesystem in [
        "nfs", "nfs4", "cifs", "autofs", "tmpfs", "squashfs", "overlay", "fuse", "unknown",
    ] {
        let nested = format!("21 20 0:42 / /mnt/share rw - {filesystem} source rw\n");
        for data in [format!("{root}{nested}"), format!("{nested}{root}")] {
            assert_eq!(
                install::linux_mountinfo(Path::new("/mnt/share/servers"), data.as_bytes())
                    .unwrap_err(),
                InstallError::StorageUnknown,
                "{filesystem}"
            );
        }
    }
}
#[test]
fn linux_volume_capacity_must_match_the_unfiltered_mount() {
    let base = Path::new("/run/servers/base");
    let data =
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw\n21 20 8:2 / /run/servers rw - xfs /dev/sdb1 rw\n";
    let actual = install::linux_mountinfo(base, data).unwrap();
    assert_eq!(actual, PathBuf::from("/run/servers"));
    // sysinfo skips mounts below /run: a visible parent alone is not capacity evidence.
    assert_eq!(
        install::confirmed_volume(base, [(PathBuf::from("/"), 9999, true)], Some(&actual))
            .unwrap_err(),
        InstallError::StorageUnknown
    );
    let (mount, free) = install::confirmed_volume(
        base,
        [
            (PathBuf::from("/"), 9999, true),
            (actual.clone(), 200, true),
        ],
        Some(&actual),
    )
    .unwrap();
    assert_eq!(mount, actual);
    assert_eq!(free, 200);
    assert_eq!(
        install::confirmed_volume(base, [(actual.clone(), 200, false)], Some(&actual)).unwrap_err(),
        InstallError::StorageUnknown
    );
}
#[test]
fn linux_mountinfo_decodes_escaped_paths_once_and_uses_component_prefixes() {
    let root = "20 1 8:1 / / rw - ext4 /dev/sda1 rw\n";
    for (encoded, decoded) in [
        (r"/srv/space\040directory", "/srv/space directory"),
        (r"/srv/tab\011directory", "/srv/tab\tdirectory"),
        (r"/srv/line\012directory", "/srv/line\ndirectory"),
        (r"/srv/literal\134040", r"/srv/literal\040"),
    ] {
        let data = format!("{root}21 20 8:2 / {encoded} rw shared:2 - xfs /dev/sdb1 rw\n");
        let mount =
            install::linux_mountinfo(&Path::new(decoded).join("base"), data.as_bytes()).unwrap();
        assert_eq!(mount, PathBuf::from(decoded));
    }
    let data = format!("{root}21 20 0:42 / /srv/share rw - nfs source rw\n");
    assert_eq!(
        install::linux_mountinfo(Path::new("/srv/shared/base"), data.as_bytes()).unwrap(),
        PathBuf::from("/")
    );
}
#[test]
fn linux_mountinfo_is_bounded_and_malformed_or_ambiguous_data_fails_closed() {
    for data in [
        b"".as_slice(),
        b"\xff",
        b"invalid\n",
        b"20 1 8:1 / / rw - ext4 /dev/sda1\n",
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw trailing\n",
        b"20 1 8:1 / relative rw - ext4 /dev/sda1 rw\n",
        b"20 1 8:1 / /srv/../share rw - ext4 /dev/sda1 rw\n",
        b"20 1 8:1 / /srv/bad\\999 rw - ext4 /dev/sda1 rw\n",
        b"20 1 invalid / / rw - ext4 /dev/sda1 rw\n",
        b"bad 1 8:1 / / rw - ext4 /dev/sda1 rw\n",
    ] {
        assert_eq!(
            install::linux_mountinfo(Path::new("/srv/base"), data).unwrap_err(),
            InstallError::StorageUnknown
        );
    }
    let root = "20 1 8:1 / / rw - ext4 /dev/sda1 rw\n";
    let nested = "21 20 8:2 / /srv rw - xfs /dev/sdb1 rw\n";
    let duplicate = format!("{root}{nested}{nested}");
    assert_eq!(
        install::linux_mountinfo(Path::new("/srv/base"), duplicate.as_bytes()).unwrap_err(),
        InstallError::StorageUnknown
    );
    assert_eq!(
        install::linux_mountinfo(Path::new("/srv/base"), &vec![b'x'; 1024 * 1024 + 1]).unwrap_err(),
        InstallError::StorageUnknown
    );
    let many = (0..10_001)
        .map(|i| format!("{} 1 8:1 / /unrelated/{i} rw - ext4 /dev/sda1 rw\n", i + 20))
        .collect::<String>();
    assert_eq!(
        install::linux_mountinfo(Path::new("/srv/base"), many.as_bytes()).unwrap_err(),
        InstallError::StorageUnknown
    );
}
#[test]
fn linux_mountinfo_reader_caps_real_consumption_and_refuses_io_errors() {
    let base = Path::new("/srv/base");
    let data = b"20 1 8:1 / / rw - ext4 /dev/sda1 rw\n";
    assert_eq!(
        install::read_linux_mountinfo(base, std::io::Cursor::new(data)).unwrap(),
        PathBuf::from("/")
    );
    let mut input = std::io::Cursor::new(vec![b'x'; 2 * 1024 * 1024]);
    assert_eq!(
        install::read_linux_mountinfo(base, &mut input).unwrap_err(),
        InstallError::StorageUnknown
    );
    assert_eq!(input.position(), 1024 * 1024 + 1);
    struct FailedReader;
    impl std::io::Read for FailedReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture read failure"))
        }
    }
    assert_eq!(
        install::read_linux_mountinfo(base, FailedReader).unwrap_err(),
        InstallError::StorageUnknown
    );
}
#[test]
fn unknown_network_and_non_hardlink_filesystems_are_not_assumed_local() {
    for name in [
        "", "unknown", "cifs", "smbfs", "nfs", "nfs4", "fuse", "vfat", "exfat", "tmpfs",
    ] {
        assert!(!install::local_filesystem(name));
    }
    #[cfg(windows)]
    {
        assert!(install::local_filesystem("NTFS"));
        assert!(install::local_filesystem("ReFS"));
        assert!(!install::local_filesystem("ext4"));
    }
    #[cfg(not(windows))]
    {
        assert!(install::local_filesystem("ext4"));
        assert!(install::local_filesystem("apfs"));
    }
}
#[cfg(windows)]
#[test]
fn windows_native_volume_classification_resolves_the_local_fixture_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (mount, free) = install::disk_space(dir.path()).unwrap();
    assert!(mount.is_absolute());
    assert!(free > 0);
}
#[cfg(windows)]
#[test]
#[ignore = "requires VESTA_INSTALL_TEST_VOLUME pointing to an explicitly selected local test directory"]
fn windows_selected_volume_supports_space_and_full_file_identity() {
    let root = std::env::var_os("VESTA_INSTALL_TEST_VOLUME").expect("selected test directory");
    let dir = tempfile::tempdir_in(root).unwrap();
    let (mount, free) = install::disk_space(dir.path()).unwrap();
    assert!(dir.path().starts_with(mount));
    assert!(free > 0);
    let path = dir.path().join("identity-probe");
    std::fs::write(&path, b"owned storage fixture").unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let identity = crate::execution_workspace::file_identity_with_links(&file, 1).unwrap();
    let alias = dir.path().join("alias");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(crate::execution_workspace::file_identity_with_links(&file, 1).is_err());
    std::fs::remove_file(alias).unwrap();
    assert!(identity == crate::execution_workspace::file_identity_with_links(&file, 1).unwrap());
}
#[test]
fn reservations_and_concurrency_are_bounded_and_released_by_actual_guard_lifetime() {
    let c = install::Coordinator::new();
    let a = c.slot().unwrap();
    let b = c.slot().unwrap();
    assert_eq!(c.slot().err().unwrap(), InstallError::Busy);
    drop(a);
    assert!(c.slot().is_ok());
    drop(b);
    let volume = PathBuf::from("fixture-volume");
    let a = c.reserve(volume.clone(), 1000, 500, 100).unwrap();
    assert_eq!(
        c.reserve(volume.clone(), 1000, 401, 100).err().unwrap(),
        InstallError::StorageLow
    );
    let b = c.reserve(volume.clone(), 1000, 400, 100).unwrap();
    drop(a);
    drop(b);
    assert!(c.reserve(volume.clone(), 1000, 900, 100).is_ok());
    assert_eq!(
        c.reserve(volume, u64::MAX, u64::MAX, 1).err().unwrap(),
        InstallError::LimitExceeded
    );
}
#[tokio::test]
async fn concurrent_reviews_of_one_destination_publish_exactly_one_bundle() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    mount(
        &f.server,
        &f.artifact_path,
        ResponseTemplate::new(200)
            .set_body_bytes(BODY)
            .set_delay(Duration::from_millis(100)),
    )
    .await;
    let a = Approval::review(&p, ACK).unwrap();
    let b = Approval::review(&p, ACK).unwrap();
    let (a, b) = tokio::join!(
        f.installer.install(&p, a, &SilentProgressReporter),
        f.installer.install(&p, b, &SilentProgressReporter)
    );
    let results = [a, b];
    assert_eq!(
        results
            .iter()
            .filter(|v| matches!(v, Ok(InstallOutcome::Installed(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|v| matches!(v, Err(InstallError::DestinationExists)))
            .count(),
        1
    );
    assert!(f.installer.verify(&p).await.is_ok());
    no_staged_files(&f);
}
#[tokio::test]
async fn occupied_slots_refuse_without_staging_and_failure_releases_job() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let a = f.installer.coordinator.slot().unwrap();
    let b = f.installer.coordinator.slot().unwrap();
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::Busy
    );
    assert!(entries(f.root.path()).is_empty());
    drop(a);
    drop(b);
    mount(
        &f.server,
        &f.artifact_path,
        ResponseTemplate::new(200).set_body_bytes(b"incorrect"),
    )
    .await;
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::ArtifactChanged
    );
    let _a = f.installer.coordinator.slot().unwrap();
    let _b = f.installer.coordinator.slot().unwrap();
    no_staged_files(&f);
}
#[tokio::test]
async fn cancellation_before_or_during_download_has_no_completed_bundle() {
    for during in [false, true] {
        let f = Fixture::new(Software::Paper).await;
        let p = f.plan().await;
        f.artifact().await;
        let r = Reporter {
            cancel_at: if during { Some(1) } else { None },
            ..Default::default()
        };
        if !during {
            r.cancel.store(true, Ordering::SeqCst);
        }
        assert_eq!(
            f.installer
                .install(&p, Approval::review(&p, ACK).unwrap(), &r)
                .await
                .unwrap_err(),
            InstallError::Cancelled
        );
        assert!(!p.destination().exists());
        no_staged_files(&f);
    }
}
#[tokio::test]
async fn cancellation_after_receipt_publication_cannot_roll_back_success() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    f.artifact().await;
    let r = Reporter {
        cancel_on_done: true,
        ..Default::default()
    };
    assert!(matches!(
        f.installer
            .install(&p, Approval::review(&p, ACK).unwrap(), &r)
            .await
            .unwrap(),
        InstallOutcome::Installed(_)
    ));
    assert!(r.is_cancelled());
    assert!(f.installer.verify(&p).await.is_ok());
    no_staged_files(&f);
}
#[tokio::test]
async fn interrupted_publication_has_no_receipt_and_requires_explicit_reconciliation() {
    let mut f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    f.artifact().await;
    f.installer.fail_after_artifact = true;
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::PublishFailed
    );
    assert!(p.destination().join("software/paper.jar").exists());
    assert!(!p.destination().join("control/receipt.json").exists());
    assert!(f.installer.verify(&p).await.is_err());
    no_staged_files(&f);
    std::fs::write(p.destination().join("game/user-added.txt"), b"keep me").unwrap();
    f.installer.fail_after_artifact = false;
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::DestinationExists
    );
    assert_eq!(
        std::fs::read(p.destination().join("game/user-added.txt")).unwrap(),
        b"keep me"
    );
}
#[tokio::test]
async fn receipt_fields_unknown_data_and_oversized_documents_are_refused() {
    let f = Fixture::new(Software::Paper).await;
    let (p, r) = installed(&f).await;
    let path = p.destination().join("control/receipt.json");
    let good = serde_json::to_value(&r).unwrap();
    for (field, value) in [
        ("schema", json!(2)),
        ("plan_hash", json!("0".repeat(64))),
        ("eula", json!("accepted")),
        ("actual_size", json!(0)),
        ("completed_at", json!(0)),
        ("completed_at", json!(p.expires)),
        ("launch", json!({})),
        ("extra", json!(true)),
        ("observed_sha256", json!("x".repeat(64))),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(f.installer.verify(&p).await.is_err(), "{field}");
    }
    std::fs::write(&path, vec![b' '; 65537]).unwrap();
    assert_eq!(
        f.installer.verify(&p).await.unwrap_err(),
        InstallError::ReceiptInvalid
    );
}
#[tokio::test]
async fn artifact_rehash_is_bound_to_published_digest_not_just_mutable_receipt() {
    for software in [Software::Vanilla, Software::Paper, Software::Purpur] {
        let f = Fixture::new(software).await;
        let (p, mut r) = installed(&f).await;
        let changed = vec![b'x'; BODY.len()];
        std::fs::write(p.destination().join(software.jar_path()), &changed).unwrap();
        assert_eq!(
            f.installer.verify(&p).await.unwrap_err(),
            InstallError::ReceiptInvalid
        );
        r.observed_sha256 = model::sha256(&changed);
        std::fs::write(
            p.destination().join("control/receipt.json"),
            serde_json::to_vec(&r).unwrap(),
        )
        .unwrap();
        assert_eq!(
            f.installer.verify(&p).await.unwrap_err(),
            InstallError::ReceiptInvalid
        );
    }
}
#[test]
fn roots_exclude_client_profile_state_overlap_in_either_direction() {
    let root = tempfile::tempdir().unwrap();
    let server = root.path().join("servers");
    let clients = root.path().join("clients");
    std::fs::create_dir(&server).unwrap();
    std::fs::create_dir(&clients).unwrap();
    assert!(InstallRoots::new(&server, std::slice::from_ref(&clients)).is_ok());
    for excluded in [server.clone(), root.path().to_path_buf()] {
        assert!(matches!(
            InstallRoots::new(&server, &[excluded]),
            Err(InstallError::PathRefused)
        ));
    }
    let nested = server.join("profiles");
    std::fs::create_dir(&nested).unwrap();
    assert!(matches!(
        InstallRoots::new(&server, &[nested]),
        Err(InstallError::PathRefused)
    ));
    assert!(matches!(
        InstallRoots::new(Path::new("relative"), &[]),
        Err(InstallError::PathRefused)
    ));
}
#[tokio::test]
async fn client_fixture_bytes_are_unchanged_by_server_installation() {
    let f = Fixture::new(Software::Paper).await;
    let clients = tempfile::tempdir().unwrap();
    std::fs::write(clients.path().join("profile.json"), b"client fixture").unwrap();
    let (p, _) = installed(&f).await;
    assert!(f.installer.verify(&p).await.is_ok());
    assert_eq!(
        std::fs::read(clients.path().join("profile.json")).unwrap(),
        b"client fixture"
    );
}
#[tokio::test]
async fn selected_runtime_inside_install_root_is_refused_during_planning() {
    let f = Fixture::new(Software::Paper).await;
    let java = f.root.path().join("inert-java");
    std::fs::write(&java, b"not executed").unwrap();
    assert_eq!(
        f.installer
            .plan(
                Software::Paper,
                &Selection::default(),
                &java,
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert_eq!(std::fs::read(java).unwrap(), b"not executed");
}
#[tokio::test]
async fn failed_job_cleanup_never_sweeps_other_staging_or_user_files() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    let stale = f.root.path().join(".staging/job_unrecognized");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("artifact.part"), b"preserve unknown ownership").unwrap();
    mount(
        &f.server,
        &f.artifact_path,
        ResponseTemplate::new(200).set_body_bytes(b"wrong"),
    )
    .await;
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::ArtifactChanged
    );
    assert_eq!(
        std::fs::read(stale.join("artifact.part")).unwrap(),
        b"preserve unknown ownership"
    );
    assert_eq!(
        std::fs::read_dir(f.root.path().join(".staging"))
            .unwrap()
            .count(),
        1
    );
}
#[tokio::test]
async fn non_directory_container_is_refused_without_overwrite() {
    let f = Fixture::new(Software::Paper).await;
    let p = f.plan().await;
    f.artifact().await;
    std::fs::write(f.root.path().join("instances"), b"user file").unwrap();
    assert!(f
        .installer
        .install(
            &p,
            Approval::review(&p, ACK).unwrap(),
            &SilentProgressReporter
        )
        .await
        .is_err());
    assert_eq!(
        std::fs::read(f.root.path().join("instances")).unwrap(),
        b"user file"
    );
    no_staged_files(&f);
}
#[cfg(unix)]
#[tokio::test]
async fn linked_ancestors_and_receipt_leaves_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new(Software::Paper).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let p = f.plan().await;
    symlink(elsewhere.path(), f.root.path().join("instances")).unwrap();
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert!(entries(elsewhere.path()).is_empty());
    let f = Fixture::new(Software::Paper).await;
    let (p, _) = installed(&f).await;
    let receipt = p.destination().join("control/receipt.json");
    let target = elsewhere.path().join("receipt");
    std::fs::rename(&receipt, &target).unwrap();
    symlink(&target, &receipt).unwrap();
    assert_eq!(
        f.installer.verify(&p).await.unwrap_err(),
        InstallError::PathRefused
    );
}
#[cfg(windows)]
#[tokio::test]
async fn windows_junction_container_is_refused_before_transfer() {
    let f = Fixture::new(Software::Paper).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let p = f.plan().await;
    let junction = f.root.path().join("instances");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(elsewhere.path())
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(
        f.installer
            .install(
                &p,
                Approval::review(&p, ACK).unwrap(),
                &SilentProgressReporter
            )
            .await
            .unwrap_err(),
        InstallError::PathRefused
    );
    assert!(entries(elsewhere.path()).is_empty());
    std::fs::remove_dir(junction).unwrap();
}
