use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicI64, Ordering},
    },
};
use vesta_state::{Clock, StateDirectory};
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn millis(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}
#[derive(Default)]
struct Fake {
    outcomes: Mutex<VecDeque<Result<u16, ()>>>,
    bodies: Mutex<Vec<Vec<u8>>>,
    delay: std::sync::atomic::AtomicU64,
    unsupported: std::sync::atomic::AtomicBool,
}
impl Sender for Fake {
    fn post<'a>(&'a self, _: &'a str, body: &'a [u8]) -> SendFuture<'a> {
        Box::pin(async move {
            self.bodies.lock().unwrap().push(body.to_vec());
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(204))
                .map(|status| vesta_server_install::outbound::Response {
                    status,
                    retry_after_seconds: match self.delay.load(Ordering::Relaxed) {
                        0 => None,
                        n => Some(n),
                    },
                    retry_after_unsupported: self.unsupported.load(Ordering::Relaxed),
                })
        })
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    db: StateDatabase,
    clock: Arc<TestClock>,
    fake: Arc<Fake>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(100000)));
        let db = StateDatabase::open(
            StateDirectory::new(root.path().join("state"), None).unwrap(),
            clock.clone(),
        )
        .unwrap();
        Self {
            _root: root,
            db,
            clock,
            fake: Arc::default(),
        }
    }
    fn notice(&self, title: &str, body: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.db.write(|c|{c.execute("INSERT INTO notifications(id,created_at,severity,title,body,source) VALUES(?,?,'warning',?,?,'fixture')",params![id,self.clock.millis(),title,body])?;Ok(())}).unwrap();
        id
    }
    fn settings() -> Settings {
        Settings {
            sinks: vec![Sink {
                id: "owner".into(),
                url: "https://notifications.example/secret-token".into(),
                format: Format::Json,
                include_message: false,
            }],
        }
    }
    fn open(&self, s: Settings) -> Deliveries {
        Deliveries::open(self.db.clone(), s, self.fake.clone()).unwrap()
    }
    fn status(&self) -> DeliveryStatus {
        self.db.read(status_in).unwrap().remove(0)
    }
    fn advance(&self) {
        self.clock.0.fetch_add(30000, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn future_notifications_reuse_inbox_redact_by_default_and_do_not_resend_after_reopen() {
    let f = Fixture::new();
    f.notice("old", "history must not be sent");
    let settings = Fixture::settings();
    assert!(!format!("{settings:?}").contains("secret-token"));
    let d = f.open(settings.clone());
    d.tick().await.unwrap();
    assert!(f.fake.bodies.lock().unwrap().is_empty());
    let id = f.notice("fake-private-title", "fake-private-body");
    d.tick().await.unwrap();
    d.tick().await.unwrap();
    {
        let bodies = f.fake.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        let text = std::str::from_utf8(&bodies[0]).unwrap();
        assert!(!text.contains("fake-private"));
        assert!(!text.contains("secret-token"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bodies[0]).unwrap()["event_id"],
            id
        );
    }
    assert_eq!(f.status().delivered, 1);
    drop(d);
    let d = f.open(settings);
    d.tick().await.unwrap();
    assert_eq!(f.fake.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn bounded_retry_reopens_with_the_same_event_identity_and_spaces_attempts() {
    let f = Fixture::new();
    f.fake.outcomes.lock().unwrap().extend([Err(()), Ok(204)]);
    let settings = Fixture::settings();
    let d = f.open(settings.clone());
    f.notice("title", "body");
    d.tick().await.unwrap();
    assert_eq!(f.status().state, "retry");
    assert_eq!(f.status().delivered, 0);
    d.tick().await.unwrap();
    assert_eq!(f.fake.bodies.lock().unwrap().len(), 1);
    drop(d);
    let d = f.open(settings);
    f.advance();
    d.tick().await.unwrap();
    let bodies = f.fake.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(f.status().delivered, 1);
}

#[tokio::test]
async fn changed_destination_does_not_replay_old_messages_and_lock_excludes_other_drivers() {
    let f = Fixture::new();
    let mut settings = Fixture::settings();
    let d = f.open(settings.clone());
    assert!(Deliveries::open(f.db.clone(), settings.clone(), f.fake.clone()).is_err());
    f.fake.outcomes.lock().unwrap().push_back(Err(()));
    f.notice("old", "old destination only");
    d.tick().await.unwrap();
    drop(d);
    settings.sinks[0].url = "https://another.example/new-token".into();
    let d = f.open(settings);
    f.advance();
    d.tick().await.unwrap();
    assert_eq!(f.fake.bodies.lock().unwrap().len(), 1);
    assert!(f.status().pending_cursor.is_none());
    let id = f.notice("new", "new destination");
    d.tick().await.unwrap();
    let bodies = f.fake.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bodies[1]).unwrap()["event_id"],
        id
    );
}

#[tokio::test]
async fn permanent_failure_and_retry_exhaustion_advance_without_false_success() {
    let f = Fixture::new();
    f.fake
        .outcomes
        .lock()
        .unwrap()
        .extend([Ok(302), Ok(503), Ok(503), Ok(503)]);
    let d = f.open(Fixture::settings());
    f.notice("redirect", "never follow");
    d.tick().await.unwrap();
    assert_eq!(f.status().failed, 1);
    assert_eq!(f.status().delivered, 0);
    f.notice("unavailable", "bounded retries");
    for _ in 0..4 {
        d.tick().await.unwrap();
        f.advance();
    }
    assert_eq!(f.status().failed, 2);
    assert_eq!(f.fake.bodies.lock().unwrap().len(), 4);
    f.notice("later", "not blocked forever");
    d.tick().await.unwrap();
    assert_eq!(f.status().delivered, 1);
}

#[tokio::test]
async fn discord_mentions_disabled_and_oversize_message_uses_explicit_details_notice() {
    let f = Fixture::new();
    let mut s = Fixture::settings();
    s.sinks[0].format = Format::Discord;
    s.sinks[0].include_message = true;
    let d = f.open(s);
    f.notice("@everyone", "normal");
    d.tick().await.unwrap();
    f.notice("large", &"x".repeat(2100));
    d.tick().await.unwrap();
    let bodies = f.fake.bodies.lock().unwrap();
    for b in bodies.iter() {
        let v: serde_json::Value = serde_json::from_slice(b).unwrap();
        assert_eq!(v["allowed_mentions"]["parse"], serde_json::json!([]));
        assert!(v["content"].as_str().unwrap().encode_utf16().count() <= 2000);
    }
    assert!(
        std::str::from_utf8(&bodies[1])
            .unwrap()
            .contains("Open Vesta for the full message")
    );
}

#[test]
fn configuration_is_strict_and_disabled_starts_no_worker_or_delivery_state() {
    let f = Fixture::new();
    assert!(
        Worker::start(f.db.clone(), Settings::default())
            .unwrap()
            .is_none()
    );
    assert!(f.db.read(status_in).unwrap().is_empty());
    for url in [
        "http://example.test/hook",
        "https://user:secret@example.test/hook",
        "https://example.test:8443/hook",
        "https://example.test/hook#fragment",
    ] {
        let mut s = Fixture::settings();
        s.sinks[0].url = url.into();
        assert!(!s.valid());
    }
    let mut s = Fixture::settings();
    s.sinks.push(s.sinks[0].clone());
    assert!(!s.valid());
    assert!(serde_json::from_str::<Settings>(r#"{"unexpected":true}"#).is_err());
}

#[tokio::test]
async fn rate_limits_are_not_shortened_and_unknown_retry_after_fails_closed() {
    let f = Fixture::new();
    f.fake.outcomes.lock().unwrap().extend([Ok(429), Ok(204)]);
    f.fake.delay.store(120, Ordering::Relaxed);
    let d = f.open(Fixture::settings());
    f.notice("rate limit", "body");
    d.tick().await.unwrap();
    assert_eq!(f.status().next_attempt_at_ms, 220000);
    f.advance();
    d.tick().await.unwrap();
    assert_eq!(f.fake.bodies.lock().unwrap().len(), 1);
    f.clock.0.fetch_add(90000, Ordering::Relaxed);
    d.tick().await.unwrap();
    assert_eq!(f.status().delivered, 1);
    f.fake.outcomes.lock().unwrap().push_back(Ok(429));
    f.fake.unsupported.store(true, Ordering::Relaxed);
    f.notice("unknown retry format", "do not guess");
    d.tick().await.unwrap();
    assert_eq!(f.status().failed, 1);
    assert!(f.status().pending_cursor.is_none());
}

#[test]
fn active_delivery_lifetime_prevents_database_close() {
    let f = Fixture::new();
    let d = f.open(Fixture::settings());
    assert!(f.db.close().is_err());
    drop(d);
    assert!(f.db.close().is_ok());
}

#[tokio::test]
async fn disabling_and_reenabling_does_not_replay_notifications_from_disabled_period() {
    let f = Fixture::new();
    let settings = Fixture::settings();
    let d = f.open(settings.clone());
    f.notice("first", "body");
    d.tick().await.unwrap();
    drop(d);
    assert!(
        Worker::start(f.db.clone(), Settings::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(f.status().state, "disabled");
    f.notice("disabled", "must not be sent");
    let d = f.open(settings);
    d.tick().await.unwrap();
    assert_eq!(f.fake.bodies.lock().unwrap().len(), 1);
    f.notice("enabled again", "body");
    d.tick().await.unwrap();
    assert_eq!(f.fake.bodies.lock().unwrap().len(), 2);
}

#[test]
fn host_configuration_parses_sinks_but_never_displays_destination_secrets() {
    let root = tempfile::tempdir().unwrap();
    let b = root.path();
    let quoted = |p: &std::path::Path| {
        serde_json::to_string(&p.to_string_lossy().replace('\\', "/")).unwrap()
    };
    let config = format!(
        "schema-version=1\n[paths]\nstate={}\ngame={}\nscan-roots=[{}]\nlaunch-definition={}\n[pack]\nname='fixture'\nversion='1'\nrevision=1\nminimum-revision=1\nminecraft-version='1.21.1'\n[pack-access]\nmode='minecraft'\n[listener]\nmode='disabled'\n[notifications]\n[[notifications.sinks]]\nid='owner'\nurl='https://notifications.example/private-fake-token'\nformat='discord'\ninclude-message=false\n",
        quoted(&b.join("state")),
        quoted(&b.join("game")),
        quoted(&b.join("game/config")),
        quoted(&b.join("launch.toml"))
    );
    let parsed =
        crate::config::HostConfiguration::parse_toml(config.as_bytes(), b, &b.join("host.toml"))
            .unwrap();
    assert_eq!(parsed.notifications().sinks.len(), 1);
    assert!(!format!("{parsed:?}").contains("private-fake-token"));
    let bad = format!("{config}unexpected=true\n");
    assert!(
        crate::config::HostConfiguration::parse_toml(bad.as_bytes(), b, &b.join("host.toml"))
            .is_err()
    );
}

#[tokio::test]
async fn sending_intent_survives_real_database_close_reopen_without_new_event_identity() {
    let mut f = Fixture::new();
    let settings = Fixture::settings();
    let d = f.open(settings.clone());
    f.fake.outcomes.lock().unwrap().push_back(Err(()));
    f.notice("ambiguous", "body");
    d.tick().await.unwrap();
    f.db.write(|c| {
        c.execute(
            "UPDATE external_alert_delivery SET state='sending',next_at=0",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    drop(d);
    f.db.close().unwrap();
    f.db = StateDatabase::open(
        StateDirectory::new(f._root.path().join("state"), None).unwrap(),
        f.clock.clone(),
    )
    .unwrap();
    let d = f.open(settings);
    d.tick().await.unwrap();
    let bodies = f.fake.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(f.status().delivered, 1);
}

struct WaitingSender {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Sender for WaitingSender {
    fn post<'a>(&'a self, _: &'a str, _: &'a [u8]) -> SendFuture<'a> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(vesta_server_install::outbound::Response {
                status: 204,
                retry_after_seconds: None,
                retry_after_unsupported: false,
            })
        })
    }
}
#[tokio::test]
async fn slow_sink_does_not_hold_database_transaction_or_block_other_work() {
    let f = Fixture::new();
    let sender = Arc::new(WaitingSender {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let d = Arc::new(Deliveries::open(f.db.clone(), Fixture::settings(), sender.clone()).unwrap());
    f.notice("pending", "body");
    let task = {
        let d = d.clone();
        tokio::spawn(async move { d.tick().await })
    };
    sender.entered.notified().await;
    let db = f.db.clone();
    let read = tokio::task::spawn_blocking(move || {
        db.write(|c| {
            c.execute("UPDATE external_alert_delivery SET retention_gap=1", [])?;
            Ok(status_in(c)?.remove(0).state)
        })
    });
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), read)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        "sending"
    );
    sender.release.notify_one();
    task.await.unwrap().unwrap();
    assert_eq!(f.status().delivered, 1);
}
