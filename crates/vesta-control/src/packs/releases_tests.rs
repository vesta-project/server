//! Real owned temporary state, policy rows and published artifacts. No route,
//! listener, provider download or game execution.
use super::*;
use crate::identity::{DeviceId, PrincipalId};
use crate::packs::{PackPreparation, SnapshotLimits, VestaBuildLimits, build_vesta};
use sha2::{Digest, Sha256 as DigestSha256};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use vesta_protocol::pack::sources::SourceRegistry;
use vesta_state::{Clock, StateDirectory};

struct FixedClock;
impl Clock for FixedClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}
struct Fixture {
    temp: TempDir,
    game: PathBuf,
    db: StateDatabase,
    owner: Subject,
    releases: PackReleases,
}
impl Fixture {
    fn open() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir_all(game.join("archive")).unwrap();
        let directory =
            StateDirectory::new(temp.path().join("private-state"), Some(&game)).unwrap();
        let db = StateDatabase::open(directory, Arc::new(FixedClock)).unwrap();
        let owner = seed(&db, true);
        let releases = PackReleases::open(&db).unwrap();
        Self {
            temp,
            game,
            db,
            owner,
            releases,
        }
    }
    fn archive(&self, bytes: &[u8]) -> PreparedArchive {
        std::fs::write(self.game.join("archive/pack.zip"), bytes).unwrap();
        self.preparation()
            .archive(&crate::files::RelativePath::parse("archive/pack.zip").unwrap())
            .unwrap()
    }
    fn preparation(&self) -> PackPreparation {
        PackPreparation::open(
            &self.db,
            &self.game,
            &[],
            SnapshotLimits::new(1_024, 1_024, 8, 64).unwrap(),
        )
        .unwrap()
    }
    fn publish(
        &self,
        subject: &Subject,
        policy: UpdatePolicy,
        version: &str,
        bytes: &[u8],
    ) -> Result<PackRelease, ReleaseError> {
        let expected = self
            .releases
            .current()
            .unwrap()
            .map_or(0, |release| release.revision());
        let plan = self.releases.plan(subject, expected, policy)?;
        self.releases
            .publish_archive(subject, &plan, &self.archive(bytes), version, false)
    }
    fn grant(&self, subject: &Subject, capability: Capability, effect: &str) {
        self.db.write(|c| {
            c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?,?,'*',?,'fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), capability.id(), effect, self.db.clock().millis()])?;
            Ok(())
        }).unwrap();
    }
    fn rows(&self) -> i64 {
        self.db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM pack_releases", [], |row| row.get(0))?))
            .unwrap()
    }
    fn audit(&self) -> Vec<(String, String)> {
        AuditLog::new(self.db.clone())
            .unwrap()
            .page(0, 100)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.action == PUBLISH)
            .map(|entry| (entry.outcome, entry.metadata_json))
            .collect()
    }
}
fn seed(db: &StateDatabase, owner: bool) -> Subject {
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)",
            params![principal.as_str(), db.server_id(), db.clock().millis(), db.clock().millis()])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)",
            params![device.as_str(), db.server_id(), principal.as_str(), hex::encode(DigestSha256::digest(device.as_str().as_bytes())), db.clock().millis()])?;
        if owner {
            c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)",
                params![uuid::Uuid::new_v4().to_string(), principal.as_str(), db.clock().millis()])?;
        }
        Ok(())
    }).unwrap();
    Subject { principal, device }
}
#[track_caller]
fn refused<T: fmt::Debug>(result: Result<T, ReleaseError>, code: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), code),
        Ok(value) => panic!("expected {code}, produced {value:?}"),
    }
}

#[test]
fn revisions_increase_monotonically_and_the_minimum_follows_the_chosen_policy() {
    let f = Fixture::open();
    assert!(f.releases.current().unwrap().is_none());

    // Even an "optional" first release has nothing older to stay compatible with.
    let first = f
        .publish(&f.owner, UpdatePolicy::Optional, "1.0", b"one")
        .unwrap();
    assert_eq!((first.revision(), first.minimum_revision()), (1, 1));
    let second = f
        .publish(&f.owner, UpdatePolicy::Optional, "1.1", b"two")
        .unwrap();
    assert_eq!((second.revision(), second.minimum_revision()), (2, 1));
    let third = f
        .publish(&f.owner, UpdatePolicy::Required, "2.0", b"three")
        .unwrap();
    assert_eq!((third.revision(), third.minimum_revision()), (3, 3));
    let fourth = f
        .publish(&f.owner, UpdatePolicy::Optional, "2.1", b"four")
        .unwrap();
    assert_eq!((fourth.revision(), fourth.minimum_revision()), (4, 3));

    let current = f.releases.current().unwrap().unwrap();
    assert_eq!(current, fourth);
    assert_eq!(current.display_version(), "2.1");
    assert_eq!(current.published_at(), 1_791_115_200_000);
    let loaded = f
        .releases
        .artifacts()
        .load(current.artifact(), crate::packs::MAX_ARTIFACT_BYTES)
        .unwrap();
    assert_eq!(loaded.bytes(), b"four");
    assert_eq!(f.rows(), 4);

    let audit = f.audit();
    assert_eq!(audit.len(), 4);
    assert!(audit.iter().all(|(outcome, _)| outcome == "success"));
    assert!(audit[2].1.contains("\"minimum_revision\":\"3\""));
    assert!(audit[2].1.contains("\"previous_revision\":\"2\""));
}

#[test]
fn installed_revisions_map_to_current_available_or_required() {
    let f = Fixture::open();
    f.publish(&f.owner, UpdatePolicy::Optional, "1", b"1")
        .unwrap();
    f.publish(&f.owner, UpdatePolicy::Required, "2", b"2")
        .unwrap();
    let release = f
        .publish(&f.owner, UpdatePolicy::Optional, "3", b"3")
        .unwrap();
    assert_eq!((release.revision(), release.minimum_revision()), (3, 2));
    for (installed, expected) in [
        (0, UpdateStatus::UpdateRequired),
        (1, UpdateStatus::UpdateRequired),
        (2, UpdateStatus::UpdateAvailable),
        (3, UpdateStatus::Current),
        (4, UpdateStatus::UpdateRequired),
        (u64::MAX, UpdateStatus::UpdateRequired),
    ] {
        assert_eq!(
            release.status_for(installed),
            expected,
            "installed {installed}"
        );
    }
    assert_eq!(UpdateStatus::Current.wire(), "current");
    assert_eq!(UpdateStatus::UpdateAvailable.wire(), "update_available");
    assert_eq!(UpdateStatus::UpdateRequired.wire(), "update_required");
}

#[test]
fn a_stale_expected_revision_conflicts_without_recording_anything() {
    let f = Fixture::open();
    refused(
        f.releases.plan(&f.owner, 1, UpdatePolicy::Optional),
        "pack_release_conflict",
    );
    let a = f
        .releases
        .plan(&f.owner, 0, UpdatePolicy::Optional)
        .unwrap();
    let b = f
        .releases
        .plan(&f.owner, 0, UpdatePolicy::Required)
        .unwrap();
    f.releases
        .publish_archive(&f.owner, &a, &f.archive(b"winner"), "1.0", false)
        .unwrap();
    // The second plan was reserved against the same base and must not reuse revision 1.
    refused(
        f.releases
            .publish_archive(&f.owner, &b, &f.archive(b"loser"), "1.0", false),
        "pack_release_conflict",
    );
    refused(
        f.releases.plan(&f.owner, 0, UpdatePolicy::Optional),
        "pack_release_conflict",
    );
    assert_eq!(f.rows(), 1);
    let current = f.releases.current().unwrap().unwrap();
    assert_eq!(
        f.releases
            .artifacts()
            .load(current.artifact(), 64)
            .unwrap()
            .bytes(),
        b"winner"
    );
}

#[test]
fn publication_requires_pack_publish_and_raising_the_minimum_requires_force_update() {
    let f = Fixture::open();
    let stranger = seed(&f.db, false);
    refused(
        f.publish(&stranger, UpdatePolicy::Optional, "1", b"x"),
        "forbidden",
    );
    assert_eq!(f.rows(), 0);
    assert_eq!(f.audit().len(), 1);
    assert_eq!(f.audit()[0].0, "denied");

    let maintainer = seed(&f.db, false);
    f.grant(&maintainer, Capability::PackPublish, "allow");
    // The first release raises nothing, so publish alone suffices even when "required".
    f.publish(&maintainer, UpdatePolicy::Required, "1", b"a")
        .unwrap();
    f.publish(&maintainer, UpdatePolicy::Optional, "2", b"b")
        .unwrap();
    refused(
        f.releases.plan(&maintainer, 2, UpdatePolicy::Required),
        "forbidden",
    );
    f.grant(&maintainer, Capability::PackForceUpdate, "allow");
    let forced = f
        .publish(&maintainer, UpdatePolicy::Required, "3", b"c")
        .unwrap();
    assert_eq!(forced.minimum_revision(), 3);
    assert_eq!(f.rows(), 3);
}

#[test]
fn authority_revoked_after_planning_is_rechecked_in_the_recording_transaction() {
    let f = Fixture::open();
    let maintainer = seed(&f.db, false);
    f.grant(&maintainer, Capability::PackPublish, "allow");
    let plan = f
        .releases
        .plan(&maintainer, 0, UpdatePolicy::Optional)
        .unwrap();
    let archive = f.archive(b"bytes");
    f.grant(&maintainer, Capability::PackPublish, "deny");
    refused(
        f.releases
            .publish_archive(&maintainer, &plan, &archive, "1", false),
        "forbidden",
    );
    assert_eq!(f.rows(), 0);
    assert_eq!(f.audit().last().unwrap().0, "denied");
}

#[test]
fn a_lower_or_incomparable_display_version_needs_explicit_acknowledgement() {
    let f = Fixture::open();
    f.publish(&f.owner, UpdatePolicy::Optional, "1.9", b"a")
        .unwrap();
    f.publish(&f.owner, UpdatePolicy::Optional, "1.10", b"b")
        .unwrap();
    f.publish(&f.owner, UpdatePolicy::Optional, "1.10.0", b"c")
        .unwrap();
    refused(
        f.publish(&f.owner, UpdatePolicy::Optional, "1.2", b"d"),
        "pack_release_version_not_increasing",
    );
    refused(
        f.publish(&f.owner, UpdatePolicy::Optional, "beta", b"d"),
        "pack_release_version_not_increasing",
    );
    assert_eq!(f.rows(), 3);
    let plan = f
        .releases
        .plan(&f.owner, 3, UpdatePolicy::Optional)
        .unwrap();
    let lowered = f
        .releases
        .publish_archive(&f.owner, &plan, &f.archive(b"d"), "1.2", true)
        .unwrap();
    // Revision, not display ordering, remains the update signal.
    assert_eq!(lowered.revision(), 4);
    assert_eq!(lowered.display_version(), "1.2");
}

#[test]
fn display_versions_are_bounded_trimmed_text() {
    let f = Fixture::open();
    let long = "1".repeat(MAX_TEXT_BYTES + 1);
    for version in ["", " 1", "1 ", "1\n", "1\u{7f}", long.as_str()] {
        refused(
            f.publish(&f.owner, UpdatePolicy::Optional, version, b"x"),
            "pack_release_version_invalid",
        );
    }
    assert_eq!(f.rows(), 0);
    f.publish(
        &f.owner,
        UpdatePolicy::Optional,
        &"1".repeat(MAX_TEXT_BYTES),
        b"x",
    )
    .unwrap();
}

#[test]
fn built_packs_must_embed_the_planned_revisions() {
    let f = Fixture::open();
    std::fs::create_dir_all(f.game.join("config")).unwrap();
    std::fs::write(f.game.join("config/options.txt"), b"embedded").unwrap();
    let host = crate::config::HostConfiguration::parse_toml(
        br#"schema-version = 1
[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game/config']
launch-definition = 'private/launch.toml'
[pack]
name = 'Published'
version = '1.0.0'
revision = 7
minimum-revision = 7
minecraft-version = '1.21.1'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'disabled'
"#,
        f.temp.path(),
        std::path::Path::new("host.toml"),
    )
    .unwrap();
    let preparation = f.preparation();
    let registry = SourceRegistry::new(Vec::new()).unwrap();
    let build = |metadata: &PackMetadata| {
        let manifest = ManifestMetadata::new(
            metadata.name(),
            metadata.version(),
            metadata.revision(),
            metadata.minimum_revision(),
        )
        .unwrap();
        let snapshot = preparation
            .snapshot_with_disposition(
                crate::config::UnresolvedDisposition::EmbedPublic,
                manifest,
                &registry,
            )
            .unwrap();
        build_vesta(&snapshot, metadata, VestaBuildLimits::production()).unwrap()
    };
    let plan = f
        .releases
        .plan(&f.owner, 0, UpdatePolicy::Optional)
        .unwrap();

    // Configuration revisions are not authoritative; a pack built from them refuses.
    let stale = build(host.pack());
    refused(
        f.releases.publish_vesta(&f.owner, &plan, &stale, false),
        "pack_release_revision_mismatch",
    );
    assert!(
        f.releases
            .artifacts()
            .load(
                &PublishedArtifact::from_parts(
                    ArtifactFormat::Vesta,
                    *stale.sha256(),
                    stale.size()
                ),
                crate::packs::MAX_ARTIFACT_BYTES
            )
            .is_err()
    );

    let metadata = plan.pack_metadata(host.pack());
    assert_eq!(
        plan.manifest_metadata(host.pack()).unwrap().revision(),
        plan.revision()
    );
    let built = build(&metadata);
    let release = f
        .releases
        .publish_vesta(&f.owner, &plan, &built, false)
        .unwrap();
    assert_eq!((release.revision(), release.minimum_revision()), (1, 1));
    assert_eq!(release.display_version(), "1.0.0");
    assert_eq!(release.artifact().format(), ArtifactFormat::Vesta);
    assert_eq!(release.artifact().sha256(), built.sha256());
}

#[test]
fn an_artifact_removed_before_recording_is_never_made_the_hosted_release() {
    let f = Fixture::open();
    let plan = f
        .releases
        .plan(&f.owner, 0, UpdatePolicy::Optional)
        .unwrap();
    let artifact = f
        .releases
        .artifacts()
        .publish_archive(&f.archive(b"vanishing"))
        .unwrap();
    let name = format!("{}.zip", artifact.sha256().to_hex());
    std::fs::remove_file(f.db.directory().path().join("artifacts").join(name)).unwrap();
    refused(
        f.releases.record(&f.owner, &plan, artifact, "1", false),
        "pack_artifact_not_published",
    );
    assert_eq!(f.rows(), 0);
    assert!(f.releases.current().unwrap().is_none());
}

#[test]
fn debug_output_omits_versions_digests_and_subjects() {
    let f = Fixture::open();
    let release = f
        .publish(&f.owner, UpdatePolicy::Optional, "secret-label", b"x")
        .unwrap();
    let text = format!("{release:?} {:?} {:?}", f.releases, ReleaseError::Conflict);
    assert!(!text.contains("secret-label"));
    assert!(!text.contains(&release.artifact().sha256().to_hex()));
    assert!(!text.contains(f.owner.principal.as_str()));
}

fn artifact_names(f: &Fixture) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(f.db.directory().path().join("artifacts"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn pruning_keeps_only_the_hosted_artifact_and_never_touches_foreign_names() {
    let f = Fixture::open();
    f.publish(&f.owner, UpdatePolicy::Optional, "1", b"old one")
        .unwrap();
    f.publish(&f.owner, UpdatePolicy::Optional, "2", b"old two")
        .unwrap();
    let hosted = f
        .publish(&f.owner, UpdatePolicy::Optional, "3", b"hosted")
        .unwrap();
    let directory = f.db.directory().path().join("artifacts");
    let stale_stage = format!("stage-{}.tmp", "0a".repeat(16));
    let foreign = [
        "notes.txt".to_owned(),
        "stage-0a.tmp".to_owned(),
        format!("{}.jar", "ab".repeat(32)),
        format!("{}.zip", "AB".repeat(32)),
    ];
    std::fs::write(directory.join(&stale_stage), b"abandoned").unwrap();
    for name in &foreign {
        std::fs::write(directory.join(name), b"foreign").unwrap();
    }

    let report = f.releases.prune(&f.owner).unwrap();
    assert_eq!((report.removed(), report.retained()), (3, 0));
    let mut expected: Vec<String> = foreign.to_vec();
    expected.push(format!("{}.zip", hosted.artifact().sha256().to_hex()));
    expected.sort();
    assert_eq!(artifact_names(&f), expected);
    for name in &foreign {
        assert_eq!(std::fs::read(directory.join(name)).unwrap(), b"foreign");
    }
    assert_eq!(
        f.releases
            .artifacts()
            .load(hosted.artifact(), 64)
            .unwrap()
            .bytes(),
        b"hosted"
    );
    // Release history rows are retained even though only the hosted bytes remain.
    assert_eq!(f.rows(), 3);
    let again = f.releases.prune(&f.owner).unwrap();
    assert_eq!((again.removed(), again.retained()), (0, 0));

    let audit = AuditLog::new(f.db.clone()).unwrap().page(0, 100).unwrap();
    let prunes: Vec<_> = audit.iter().filter(|entry| entry.action == PRUNE).collect();
    assert_eq!(prunes.len(), 2);
    assert!(prunes[0].metadata_json.contains("\"removed\":\"3\""));
}

#[test]
fn pruning_without_a_release_removes_every_owned_name() {
    let f = Fixture::open();
    f.releases
        .artifacts()
        .publish_archive(&f.archive(b"unreferenced"))
        .unwrap();
    assert_eq!(artifact_names(&f).len(), 1);
    let report = f.releases.prune(&f.owner).unwrap();
    assert_eq!((report.removed(), report.retained()), (1, 0));
    assert!(artifact_names(&f).is_empty());
}

#[test]
fn pruning_requires_pack_publish_and_refusals_are_audited() {
    let f = Fixture::open();
    f.publish(&f.owner, UpdatePolicy::Optional, "1", b"first")
        .unwrap();
    f.publish(&f.owner, UpdatePolicy::Optional, "2", b"second")
        .unwrap();
    let before = artifact_names(&f);
    let stranger = seed(&f.db, false);
    refused(f.releases.prune(&stranger), "forbidden");
    assert_eq!(artifact_names(&f), before);
    let audit = AuditLog::new(f.db.clone()).unwrap().page(0, 100).unwrap();
    let denied = audit.last().unwrap();
    assert_eq!(
        (denied.action.as_str(), denied.outcome.as_str()),
        (PRUNE, "denied")
    );
}

#[test]
fn an_unrecorded_publication_removed_by_pruning_fails_its_recording_safely() {
    let f = Fixture::open();
    let hosted = f
        .publish(&f.owner, UpdatePolicy::Optional, "1", b"hosted")
        .unwrap();
    let plan = f
        .releases
        .plan(&f.owner, 1, UpdatePolicy::Optional)
        .unwrap();
    let pending = f
        .releases
        .artifacts()
        .publish_archive(&f.archive(b"pending"))
        .unwrap();
    assert_eq!(f.releases.prune(&f.owner).unwrap().removed(), 1);
    refused(
        f.releases.record(&f.owner, &plan, pending, "2", false),
        "pack_artifact_not_published",
    );
    assert_eq!(f.releases.current().unwrap().unwrap(), hosted);
    // A retry republishes the bytes and records normally.
    let retried = f
        .releases
        .publish_archive(&f.owner, &plan, &f.archive(b"pending"), "2", false)
        .unwrap();
    assert_eq!(retried.revision(), 2);
}

#[test]
fn release_notifications_coalesce_to_latest_committed_revision_only() {
    let f = Fixture::open();
    let independent = PackReleases::open(&f.db).unwrap();
    let mut changes = independent.subscribe();
    assert!(!changes.has_changed().unwrap());
    f.publish(&f.owner, UpdatePolicy::Optional, "1", b"first")
        .unwrap();
    f.publish(&f.owner, UpdatePolicy::Optional, "2", b"second")
        .unwrap();
    f.publish(&f.owner, UpdatePolicy::Required, "3", b"third")
        .unwrap();
    // A reader which missed intermediate commits gets one latest state, not a queue.
    assert_eq!(*changes.borrow_and_update(), 3);
    assert!(!changes.has_changed().unwrap());
    assert_eq!(
        independent.current().unwrap().unwrap().minimum_revision(),
        3
    );
    assert!(
        f.publish(&f.owner, UpdatePolicy::Optional, "1", b"refused")
            .is_err()
    );
    assert!(!changes.has_changed().unwrap());
    assert_eq!(*changes.borrow(), 3);
}
