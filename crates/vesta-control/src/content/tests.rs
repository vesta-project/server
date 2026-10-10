use super::*;
use crate::identity::{DeviceId, PrincipalId};
use rusqlite::params;
use sha2::Sha256;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fs,
    io::{self, Cursor},
    path::PathBuf,
    sync::Arc,
};
use tempfile::TempDir;
use vesta_state::{Clock, StateDatabase, StateDirectory};

struct FixedClock;
impl Clock for FixedClock {
    fn millis(&self) -> i64 {
        1_791_115_200_000
    }
}
struct Fixture {
    _temp: TempDir,
    game: PathBuf,
    db: StateDatabase,
    owner: Subject,
    files: FileBrowser,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(game.join("mods")).unwrap();
        let db = StateDatabase::open(
            StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap(),
            Arc::new(FixedClock),
        )
        .unwrap();
        let owner = seed(&db, true);
        let files = FileBrowser::open(db.clone(), &game, &[]).unwrap();
        Self {
            _temp: temp,
            game,
            db,
            owner,
            files,
        }
    }
    fn audits(&self) -> i64 {
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT count(*) FROM audit_events WHERE action='files.write'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap()
    }
    fn no_effect(&self) {
        assert_eq!(fs::read_dir(self.game.join("mods")).unwrap().count(), 0);
        assert_eq!(self.audits(), 0);
    }
}
fn seed(db: &StateDatabase, owner: bool) -> Subject {
    let principal = PrincipalId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    let device = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    db.write(|c| {
        c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)", params![principal.as_str(), db.server_id(), db.clock().millis(), db.clock().millis()])?;
        c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)", params![device.as_str(), db.server_id(), principal.as_str(), hex::encode(Sha256::digest(device.as_str().as_bytes())), db.clock().millis()])?;
        if owner { c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,'owner','fixture',?)", params![uuid::Uuid::new_v4().to_string(), principal.as_str(), db.clock().millis()])?; }
        Ok(())
    }).unwrap();
    Subject { principal, device }
}
struct FakeProvider {
    source: ResolvedSource,
    calls: Cell<usize>,
}
impl ProviderClient for FakeProvider {
    fn resolve(&self, provider: Provider, project: &str, version: &str) -> Result<ResolvedSource> {
        assert_eq!(provider, self.source.provider);
        assert_eq!((project, version), ("project1", "version1"));
        self.calls.set(self.calls.get() + 1);
        Ok(self.source.clone())
    }
}
struct FakeHttp {
    responses: RefCell<VecDeque<HttpResponse>>,
    urls: RefCell<Vec<String>>,
}
impl FakeHttp {
    fn new(responses: Vec<HttpResponse>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            urls: RefCell::new(Vec::new()),
        }
    }
}
impl DownloadHttp for FakeHttp {
    fn get(&self, url: &Url) -> Result<HttpResponse> {
        self.urls.borrow_mut().push(url.as_str().into());
        self.responses
            .borrow_mut()
            .pop_front()
            .ok_or(ContentError::DownloadFailed)
    }
}
fn source(provider: Provider, bytes: &[u8]) -> ResolvedSource {
    ResolvedSource {
        provider,
        filename: "demo.jar".into(),
        download_url: match provider {
            Provider::Modrinth => "https://cdn.modrinth.com/demo.jar",
            Provider::Curseforge => "https://edge.forgecdn.net/demo.jar",
        }
        .into(),
        expected_hash: match provider {
            Provider::Modrinth => ExpectedHash::Sha512(hex::encode(Sha512::digest(bytes))),
            Provider::Curseforge => ExpectedHash::Sha1(hex::encode(Sha1::digest(bytes))),
        },
    }
}
fn response(bytes: &[u8]) -> HttpResponse {
    HttpResponse {
        status: 200,
        location: None,
        content_length: None,
        content_encoding: None,
        body: Box::new(Cursor::new(bytes.to_vec())),
    }
}
fn redirect(location: &str) -> HttpResponse {
    HttpResponse {
        status: 302,
        location: Some(location.into()),
        ..response(b"")
    }
}
fn request(provider: Provider) -> InstallRequest {
    InstallRequest::Provider {
        provider,
        project_id: "project1".into(),
        version_id: "version1".into(),
    }
}
fn install(
    f: &Fixture,
    source: ResolvedSource,
    http: &FakeHttp,
    precondition: &WritePrecondition,
) -> Result<FileChange> {
    let provider = source.provider;
    let client = FakeProvider {
        source,
        calls: Cell::new(0),
    };
    ContentInstaller::new(&f.files, &client, http, &TrustedSourceRegistry::default()).install(
        &f.owner,
        &request(provider),
        precondition,
    )
}

#[test]
fn success_for_both_providers_has_exact_bytes_digest_and_one_audit() {
    for provider in [Provider::Modrinth, Provider::Curseforge] {
        let f = Fixture::new();
        let bytes = b"verified mod";
        let change = install(
            &f,
            source(provider, bytes),
            &FakeHttp::new(vec![response(bytes)]),
            &WritePrecondition::Absent,
        )
        .unwrap();
        assert_eq!(fs::read(f.game.join("mods/demo.jar")).unwrap(), bytes);
        assert_eq!(change.sha256, hex::encode(Sha256::digest(bytes)));
        assert_eq!(change.size, bytes.len() as u64);
        assert!(change.trash.is_none());
        assert_eq!(f.audits(), 1);
    }
}
#[test]
fn hash_mismatch_never_stages_or_writes() {
    let f = Fixture::new();
    assert_eq!(
        install(
            &f,
            source(Provider::Modrinth, b"expected"),
            &FakeHttp::new(vec![response(b"wrong")]),
            &WritePrecondition::Absent
        ),
        Err(ContentError::HashMismatch)
    );
    f.no_effect();
}
#[test]
fn oversize_stream_and_declared_size_refuse() {
    for declared in [false, true] {
        let f = Fixture::new();
        let mut r = response(b"");
        if declared {
            r.content_length = Some(MAX_WRITE_BYTES as u64 + 1);
        } else {
            r.body = Box::new(io::repeat(0));
        }
        assert_eq!(
            install(
                &f,
                source(Provider::Modrinth, b""),
                &FakeHttp::new(vec![r]),
                &WritePrecondition::Absent
            ),
            Err(ContentError::TooLarge)
        );
        f.no_effect();
    }
}
#[test]
fn disallowed_urls_are_rejected_before_http() {
    for url in [
        "http://cdn.modrinth.com/mod.jar",
        "https://evil.test/mod.jar",
        "https://cdn.modrinth.com.evil.test/mod.jar",
        "https://cdn.modrinth.com:444/mod.jar",
        "https://user@cdn.modrinth.com/mod.jar",
        "https://cdn.modrinth.com/mod.jar#fragment",
        "https://edge.forgecdn.net/mod.jar",
    ] {
        let f = Fixture::new();
        let mut s = source(Provider::Modrinth, b"mod");
        s.download_url = url.into();
        let http = FakeHttp::new(vec![]);
        assert_eq!(
            install(&f, s, &http, &WritePrecondition::Absent),
            Err(ContentError::DisallowedUrl)
        );
        assert!(http.urls.borrow().is_empty());
        f.no_effect();
    }
}
#[test]
fn redirects_are_rechecked_and_bounded() {
    for location in [
        "https://evil.test/mod.jar",
        "http://cdn.modrinth.com/mod.jar",
        "https://cdn.modrinth.com:444/mod.jar",
    ] {
        let f = Fixture::new();
        let http = FakeHttp::new(vec![redirect(location)]);
        assert_eq!(
            install(
                &f,
                source(Provider::Modrinth, b"mod"),
                &http,
                &WritePrecondition::Absent
            ),
            Err(ContentError::DisallowedUrl)
        );
        assert_eq!(http.urls.borrow().len(), 1);
        f.no_effect();
    }
    let f = Fixture::new();
    let http = FakeHttp::new((0..=MAX_REDIRECTS).map(|_| redirect("/again")).collect());
    assert_eq!(
        install(
            &f,
            source(Provider::Modrinth, b"mod"),
            &http,
            &WritePrecondition::Absent
        ),
        Err(ContentError::TooManyRedirects)
    );
    assert_eq!(http.urls.borrow().len(), MAX_REDIRECTS + 1);
    f.no_effect();
}
#[test]
fn allowed_relative_redirect_succeeds() {
    let f = Fixture::new();
    let http = FakeHttp::new(vec![redirect("/next.jar"), response(b"mod")]);
    install(
        &f,
        source(Provider::Modrinth, b"mod"),
        &http,
        &WritePrecondition::Absent,
    )
    .unwrap();
    assert_eq!(
        http.urls.borrow().as_slice(),
        [
            "https://cdn.modrinth.com/demo.jar",
            "https://cdn.modrinth.com/next.jar"
        ]
    );
}
#[test]
fn unauthorized_subject_cannot_download_or_write() {
    let f = Fixture::new();
    let subject = seed(&f.db, false);
    let client = FakeProvider {
        source: source(Provider::Modrinth, b"mod"),
        calls: Cell::new(0),
    };
    let http = FakeHttp::new(vec![]);
    let registry = TrustedSourceRegistry::default();
    let error = ContentInstaller::new(&f.files, &client, &http, &registry)
        .install(
            &subject,
            &request(Provider::Modrinth),
            &WritePrecondition::Absent,
        )
        .unwrap_err();
    assert!(matches!(error, ContentError::File(FileError::Control(_))));
    assert!(http.urls.borrow().is_empty());
    f.no_effect();
}
#[test]
fn precondition_conflict_keeps_original_without_success_audit() {
    for precondition in [
        WritePrecondition::Absent,
        WritePrecondition::Matches("0".repeat(64)),
    ] {
        let f = Fixture::new();
        fs::write(f.game.join("mods/demo.jar"), b"original").unwrap();
        let error = install(
            &f,
            source(Provider::Modrinth, b"new"),
            &FakeHttp::new(vec![response(b"new")]),
            &precondition,
        )
        .unwrap_err();
        assert!(matches!(error, ContentError::File(FileError::Conflict(_))));
        assert_eq!(fs::read(f.game.join("mods/demo.jar")).unwrap(), b"original");
        assert_eq!(f.audits(), 0);
        assert_eq!(fs::read_dir(f.game.join("mods")).unwrap().count(), 1);
    }
}
#[test]
fn replacement_preserves_previous_bytes_in_trash_and_audits_once() {
    let f = Fixture::new();
    fs::write(f.game.join("mods/demo.jar"), b"original").unwrap();
    let change = install(
        &f,
        source(Provider::Modrinth, b"new"),
        &FakeHttp::new(vec![response(b"new")]),
        &WritePrecondition::Matches(hex::encode(Sha256::digest(b"original"))),
    )
    .unwrap();
    let trash = f.files.trash_entries(&f.owner).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(Some(&trash[0].id), change.trash.as_ref());
    assert_eq!(trash[0].sha256, hex::encode(Sha256::digest(b"original")));
    assert_eq!(f.audits(), 1);
    fs::remove_file(f.game.join("mods/demo.jar")).unwrap();
    f.files.restore(&f.owner, &trash[0].id).unwrap();
    assert_eq!(fs::read(f.game.join("mods/demo.jar")).unwrap(), b"original");
}
#[test]
fn unsafe_provider_filenames_never_reach_http() {
    for name in [
        "../outside.jar",
        "nested/mod.jar",
        "nested\\mod.jar",
        "/absolute.jar",
        "C:mod.jar",
        "CON.jar",
        "demo.jar ",
        "demo.jar.",
        "résumé.jar",
        "mod\n.jar",
        "mod.exe",
    ] {
        let f = Fixture::new();
        let mut s = source(Provider::Modrinth, b"mod");
        s.filename = name.into();
        let http = FakeHttp::new(vec![]);
        assert_eq!(
            install(&f, s, &http, &WritePrecondition::Absent),
            Err(ContentError::UnsafeFilename)
        );
        assert!(http.urls.borrow().is_empty());
        f.no_effect();
    }
}
#[test]
fn direct_reference_must_exist_in_trusted_registry() {
    let f = Fixture::new();
    let client = FakeProvider {
        source: source(Provider::Modrinth, b"not used"),
        calls: Cell::new(0),
    };
    let http = FakeHttp::new(vec![response(b"trusted")]);
    let mut registry = TrustedSourceRegistry::default();
    registry
        .register("manifest-source", source(Provider::Modrinth, b"trusted"))
        .unwrap();
    let installer = ContentInstaller::new(&f.files, &client, &http, &registry);
    assert_eq!(
        installer.install(
            &f.owner,
            &InstallRequest::Catalogue {
                source_id: "unknown".into()
            },
            &WritePrecondition::Absent
        ),
        Err(ContentError::UnknownSource)
    );
    installer
        .install(
            &f.owner,
            &InstallRequest::Catalogue {
                source_id: "manifest-source".into(),
            },
            &WritePrecondition::Absent,
        )
        .unwrap();
    assert_eq!(client.calls.get(), 0);
    assert_eq!(f.audits(), 1);
}
#[test]
fn invalid_ids_and_provider_digest_are_rejected() {
    let f = Fixture::new();
    let mut s = source(Provider::Modrinth, b"mod");
    s.expected_hash = ExpectedHash::Sha1("a".repeat(40));
    assert_eq!(
        install(&f, s, &FakeHttp::new(vec![]), &WritePrecondition::Absent),
        Err(ContentError::InvalidMetadata)
    );
    for id in ["", "../version", "https://evil.test", "version?url=evil"] {
        assert_eq!(validate_id(id), Err(ContentError::InvalidRequest));
    }
    let mut s = source(Provider::Modrinth, b"mod");
    s.expected_hash = ExpectedHash::Sha512("z".repeat(128));
    assert_eq!(
        install(&f, s, &FakeHttp::new(vec![]), &WritePrecondition::Absent),
        Err(ContentError::InvalidMetadata)
    );
    f.no_effect();
}
#[test]
fn read_failure_encoding_and_status_never_write() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("peer failure"))
        }
    }
    for r in [
        HttpResponse {
            body: Box::new(Broken),
            ..response(b"")
        },
        HttpResponse {
            status: 404,
            ..response(b"mod")
        },
        HttpResponse {
            content_encoding: Some("gzip".into()),
            ..response(b"mod")
        },
    ] {
        let f = Fixture::new();
        assert_eq!(
            install(
                &f,
                source(Provider::Modrinth, b"mod"),
                &FakeHttp::new(vec![r]),
                &WritePrecondition::Absent
            ),
            Err(ContentError::DownloadFailed)
        );
        f.no_effect();
    }
}

#[test]
fn provider_resolution_requires_current_identity_and_write_capability() {
    for provider in [Provider::Modrinth, Provider::Curseforge] {
        for failure in ["ungranted", "revoked", "disabled", "wrong-write-capability"] {
            let f = Fixture::new();
            let subject = if failure == "ungranted" {
                seed(&f.db, false)
            } else {
                f.owner.clone()
            };
            f.db.write(|c| {
                match failure {
                    "revoked" => { c.execute("UPDATE devices SET revoked_at=? WHERE id=?", params![f.db.clock().millis(), subject.device.as_str()])?; }
                    "disabled" => { c.execute("UPDATE principals SET status='disabled' WHERE id=?", [subject.principal.as_str()])?; }
                    "wrong-write-capability" => { c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?, 'files.overwrite','*','deny','fixture',?)", params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), f.db.clock().millis()])?; }
                    _ => {}
                }
                Ok(())
            }).unwrap();
            let client = FakeProvider {
                source: source(provider, b"mod"),
                calls: Cell::new(0),
            };
            let http = FakeHttp::new(vec![]);
            let registry = TrustedSourceRegistry::default();
            let condition = if failure == "wrong-write-capability" {
                WritePrecondition::Matches("0".repeat(64))
            } else {
                WritePrecondition::Absent
            };
            let result = ContentInstaller::new(&f.files, &client, &http, &registry).install(
                &subject,
                &request(provider),
                &condition,
            );
            assert!(matches!(
                result,
                Err(ContentError::File(FileError::Control(_)))
            ));
            assert_eq!(client.calls.get(), 0, "provider must not run for {failure}");
            assert!(http.urls.borrow().is_empty());
            f.no_effect();
        }
    }
}

fn grant(f: &Fixture, subject: &Subject, scope: &str, effect: &str) {
    f.db.write(|c| {
        c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES (?,?, 'files.create',?,?,'fixture',?)", params![uuid::Uuid::new_v4().to_string(), subject.principal.as_str(), scope, effect, f.db.clock().millis()])?;
        Ok(())
    }).unwrap();
}
#[test]
fn provider_admission_respects_scopes_without_losing_exact_file_grants() {
    for scope in ["config/**", "mods/nested/**", "mods/not-a-jar.txt"] {
        let f = Fixture::new();
        let subject = seed(&f.db, false);
        grant(&f, &subject, scope, "allow");
        let client = FakeProvider {
            source: source(Provider::Modrinth, b"mod"),
            calls: Cell::new(0),
        };
        let http = FakeHttp::new(vec![]);
        let registry = TrustedSourceRegistry::default();
        assert!(
            ContentInstaller::new(&f.files, &client, &http, &registry)
                .install(
                    &subject,
                    &request(Provider::Modrinth),
                    &WritePrecondition::Absent
                )
                .is_err()
        );
        assert_eq!(client.calls.get(), 0);
        f.no_effect();
    }
    for scope in ["mods/**", "mods/demo.jar"] {
        let f = Fixture::new();
        let subject = seed(&f.db, false);
        grant(&f, &subject, scope, "allow");
        // An unrelated exact deny must not prohibit provider work for demo.jar.
        grant(&f, &subject, "mods/vesta-admission-0.jar", "deny");
        let client = FakeProvider {
            source: source(Provider::Modrinth, b"mod"),
            calls: Cell::new(0),
        };
        let http = FakeHttp::new(vec![response(b"mod")]);
        let registry = TrustedSourceRegistry::default();
        ContentInstaller::new(&f.files, &client, &http, &registry)
            .install(
                &subject,
                &request(Provider::Modrinth),
                &WritePrecondition::Absent,
            )
            .unwrap();
        assert_eq!(client.calls.get(), 1);
        assert_eq!(f.audits(), 1);
    }
    let f = Fixture::new();
    grant(&f, &f.owner, "mods/**", "deny");
    let client = FakeProvider {
        source: source(Provider::Modrinth, b"mod"),
        calls: Cell::new(0),
    };
    let http = FakeHttp::new(vec![]);
    let registry = TrustedSourceRegistry::default();
    assert!(
        ContentInstaller::new(&f.files, &client, &http, &registry)
            .install(
                &f.owner,
                &request(Provider::Modrinth),
                &WritePrecondition::Absent
            )
            .is_err()
    );
    assert_eq!(client.calls.get(), 0);
    f.no_effect();
}

#[test]
fn interrupted_reads_retry_without_changing_hash_or_size_cap() {
    struct Interrupting {
        reader: Box<dyn Read>,
        interrupted: bool,
    }
    impl Read for Interrupting {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                self.interrupted = false;
                self.reader.read(buffer)
            }
        }
    }
    for provider in [Provider::Modrinth, Provider::Curseforge] {
        let f = Fixture::new();
        let bytes = b"mod with interrupted read";
        let mut r = response(bytes);
        r.body = Box::new(Interrupting {
            reader: r.body,
            interrupted: false,
        });
        install(
            &f,
            source(provider, bytes),
            &FakeHttp::new(vec![r]),
            &WritePrecondition::Absent,
        )
        .unwrap();
        assert_eq!(fs::read(f.game.join("mods/demo.jar")).unwrap(), bytes);
        assert_eq!(f.audits(), 1);
    }
    let f = Fixture::new();
    let mut r = response(b"");
    r.body = Box::new(Interrupting {
        reader: Box::new(io::repeat(0)),
        interrupted: false,
    });
    assert_eq!(
        install(
            &f,
            source(Provider::Modrinth, b""),
            &FakeHttp::new(vec![r]),
            &WritePrecondition::Absent
        ),
        Err(ContentError::TooLarge)
    );
    f.no_effect();
}

#[test]
fn remote_provider_installs_are_durable_idempotent_and_audited() {
    for provider in [Provider::Modrinth, Provider::Curseforge] {
        let f = Fixture::new();
        let bytes = b"verified remote artifact";
        let client = FakeProvider {
            source: source(provider, bytes),
            calls: Cell::new(0),
        };
        let http = FakeHttp::new(vec![response(bytes)]);
        let registry = TrustedSourceRegistry::default();
        let installer = ContentInstaller::new(&f.files, &client, &http, &registry);
        let first = installer
            .install_remote(&f.owner, &request(provider), "remote-provider-key-01")
            .unwrap();
        assert_eq!(first["sha256"], hex::encode(Sha256::digest(bytes)));
        assert_eq!(
            first,
            installer
                .install_remote(&f.owner, &request(provider), "remote-provider-key-01")
                .unwrap()
        );
        assert_eq!(client.calls.get(), 1);
        assert_eq!(http.urls.borrow().len(), 1);
        assert_eq!(f.audits(), 1);
        let changed = InstallRequest::Provider {
            provider,
            project_id: "project1".into(),
            version_id: "changed".into(),
        };
        assert_eq!(
            installer.install_remote(&f.owner, &changed, "remote-provider-key-01"),
            Err(ContentError::File(FileError::IdempotencyConflict))
        );
        let notices:i64=f.db.read(|c| Ok(c.query_row("SELECT count(*) FROM audit_events WHERE action='content.install' AND outcome='success'",[],|r|r.get(0))?)).unwrap();
        assert_eq!(notices, 1);
        f.db.write(|c| { c.execute("INSERT INTO principal_overrides (id,principal_id,capability,scope,effect,granted_by,created_at) VALUES ('aaaaaaaa-2222-4333-8444-555555555555',?,'content.install','*','deny','fixture',?)",params![f.owner.principal.as_str(),f.db.clock().millis()])?; Ok(()) }).unwrap();
        assert!(matches!(
            installer.install_remote(&f.owner, &request(provider), "remote-provider-key-01"),
            Err(ContentError::File(FileError::Control(_)))
        ));
        assert_eq!(client.calls.get(), 1);
    }
}
#[test]
fn remote_provider_allowlist_and_hash_refusals_have_no_effect() {
    for invalid in ["host", "hash"] {
        let f = Fixture::new();
        let mut metadata = source(Provider::Modrinth, b"expected");
        if invalid == "host" {
            metadata.download_url = "https://untrusted.invalid/mod.jar".into();
        }
        let client = FakeProvider {
            source: metadata,
            calls: Cell::new(0),
        };
        let http = FakeHttp::new(vec![response(b"wrong")]);
        let registry = TrustedSourceRegistry::default();
        let installer = ContentInstaller::new(&f.files, &client, &http, &registry);
        assert!(
            installer
                .install_remote(
                    &f.owner,
                    &request(Provider::Modrinth),
                    "remote-provider-key-02"
                )
                .is_err()
        );
        if invalid == "host" {
            assert!(http.urls.borrow().is_empty());
        }
        f.no_effect();
        let outcomes: i64 =
            f.db.read(|c| {
                Ok(
                    c.query_row("SELECT count(*) FROM management_file_outcomes", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(outcomes, 0);
    }
}

#[test]
fn slow_remote_provider_does_not_block_an_unrelated_file_write() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    struct Slow {
        source: ResolvedSource,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl ProviderClient for Slow {
        fn resolve(&self, _: Provider, _: &str, _: &str) -> Result<ResolvedSource> {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(self.source.clone())
        }
    }
    let f = Fixture::new();
    let files = f.files.clone();
    let owner = f.owner.clone();
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let client = Slow {
            source: source(Provider::Modrinth, b"verified"),
            entered,
            release: blocked,
        };
        let http = FakeHttp::new(vec![response(b"verified")]);
        let registry = TrustedSourceRegistry::default();
        ContentInstaller::new(&files, &client, &http, &registry).install_remote(
            &owner,
            &request(Provider::Modrinth),
            "slow-install-key-0001",
        )
    });
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    let start = Instant::now();
    let changed = f.files.mutate_remote(
        &f.owner,
        "unrelated-write-key1",
        crate::files::RemoteFileMutation::Write {
            path: &RelativePath::parse("mods/unrelated.txt").unwrap(),
            precondition: &WritePrecondition::Absent,
            bytes: b"unrelated",
        },
    );
    let elapsed = start.elapsed();
    let _ = release.send(());
    let installed = worker.join().unwrap();
    changed.unwrap();
    installed.unwrap();
    assert!(
        elapsed < Duration::from_secs(2),
        "file write waited for provider"
    );
}
#[test]
fn whole_install_deadline_refuses_expired_metadata_body_and_effect() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let client = FakeProvider {
        source: source(Provider::Modrinth, b"verified"),
        calls: Cell::new(0),
    };
    let http = FakeHttp::new(vec![response(b"verified")]);
    let registry = TrustedSourceRegistry::default();
    let installer = ContentInstaller::new(&f.files, &client, &http, &registry);
    assert!(
        installer
            .install_with_files(
                &f.files,
                &f.owner,
                &request(Provider::Modrinth),
                &WritePrecondition::Absent,
                Some(Instant::now() - Duration::from_secs(1))
            )
            .is_err()
    );
    assert_eq!(client.calls.get(), 0);
    assert!(http.urls.borrow().is_empty());
    f.no_effect();
    let mut body = DeadlineRead {
        reader: Box::new(std::io::Cursor::new(b"verified")),
        deadline: Instant::now() - Duration::from_secs(1),
    };
    assert!(body.read(&mut [0u8; 8]).is_err());
    let files = f.files.for_content(Instant::now() - Duration::from_secs(1));
    let installed = files.execute_remote(
        &f.owner,
        "expired-effect-key01",
        serde_json::json!(["fixture"]),
        || {
            Ok(vec![(
                crate::identity::Capability::FilesCreate.id().into(),
                "mods/demo.jar".into(),
            )])
        },
        |files| {
            files.write(
                &f.owner,
                &RelativePath::parse("mods/demo.jar").unwrap(),
                &WritePrecondition::Absent,
                b"verified",
            )?;
            Ok(serde_json::json!({}))
        },
    );
    assert!(installed.is_err());
    f.no_effect();
}
#[test]
fn separate_install_admission_is_bounded_and_released() {
    let first = InstallPermit::acquire().unwrap();
    let second = InstallPermit::acquire().unwrap();
    assert!(InstallPermit::acquire().is_err());
    drop(first);
    assert!(InstallPermit::acquire().is_ok());
    drop(second);
}
