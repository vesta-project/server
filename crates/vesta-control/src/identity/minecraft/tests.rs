use super::*;
use std::{
    io::{Cursor, Read},
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const TEST_SERVER: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const TEST_PROFILE: &str = "11111111-2222-4333-8444-555555555555";
const TEST_PROFILE_ID: &str = "11111111222243338444555555555555";
const MOJANG_BODY: &str = r#"{"id":"11111111222243338444555555555555","name":"PlayerOne"}"#;

fn server(value: &str) -> ServerUuid {
    ServerUuid::parse(value).unwrap()
}

fn profile_uuid(value: &str) -> MinecraftUuid {
    MinecraftUuid::parse(value).unwrap()
}

fn name(value: &str) -> MinecraftName {
    MinecraftName::parse(value).unwrap()
}

fn joined(uuid: &str, profile_name: &str) -> JoinedProfile {
    JoinedProfile {
        uuid: profile_uuid(uuid),
        name: name(profile_name),
    }
}

struct FnProvider<F>(F);

impl<F> SessionProvider for FnProvider<F>
where
    F: Fn(&MinecraftName, &ServerHash) -> Result<Option<JoinedProfile>, MinecraftError>
        + Send
        + Sync,
{
    fn verify(
        &self,
        profile_name: &MinecraftName,
        server_hash: &ServerHash,
    ) -> Result<Option<JoinedProfile>, MinecraftError> {
        (self.0)(profile_name, server_hash)
    }
}

#[derive(Clone)]
struct MutableClock(Arc<Mutex<Instant>>);

impl MutableClock {
    fn at(now: Instant) -> Self {
        Self(Arc::new(Mutex::new(now)))
    }

    fn advance(&self, duration: Duration) {
        let mut now = self.0.lock().unwrap();
        *now += duration;
    }
}

impl Clock for MutableClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

fn limits(update: impl FnOnce(&mut TestLimits)) -> TestLimits {
    let mut limits = TestLimits::default();
    update(&mut limits);
    limits
}

fn compose(
    server: ServerUuid,
    provider: impl SessionProvider + 'static,
    clock: impl Clock + 'static,
    limits: TestLimits,
) -> MinecraftChallenges {
    MinecraftChallenges::compose(server, Arc::new(provider), Arc::new(clock), limits).unwrap()
}

fn default_challenges(
    provider: impl Fn(&MinecraftName, &ServerHash) -> Result<Option<JoinedProfile>, MinecraftError>
    + Send
    + Sync
    + 'static,
) -> MinecraftChallenges {
    compose(
        server(TEST_SERVER),
        FnProvider(provider),
        MutableClock::at(Instant::now()),
        TestLimits::default(),
    )
}

fn issue(challenges: &MinecraftChallenges, profile: &str, profile_name: &str) -> IssuedChallenge {
    challenges
        .issue(profile_uuid(profile), name(profile_name))
        .unwrap()
}

#[test]
fn completes_once_for_the_bound_server_and_profile() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let challenges = default_challenges(move |requested_name, server_hash| {
        calls_in_provider.fetch_add(1, Ordering::SeqCst);
        assert_eq!(requested_name.as_str(), "PlayerOne");
        assert_eq!(server_hash.as_str().len(), 40);
        assert!(
            server_hash
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
    });
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");

    let verified = challenges.complete(issued.challenge_id()).unwrap().unwrap();
    assert_eq!(verified.server_uuid(), server(TEST_SERVER));
    assert_eq!(verified.uuid(), profile_uuid(TEST_PROFILE));
    assert_eq!(verified.name().as_str(), "PlayerOne");
    assert!(
        challenges
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn rejects_wrong_server_and_consumes_attempt_across_shared_handles() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let challenges = default_challenges(move |_, _| {
        calls_in_provider.fetch_add(1, Ordering::SeqCst);
        Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
    });
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let other_server = ServerUuid::parse("bbbbbbbb-cccc-4ddd-8eee-ffffffffffff").unwrap();
    let other_handle = challenges.handle_for_server(other_server);

    assert!(
        other_handle
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
    assert!(
        challenges
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn rejects_mismatched_verified_uuid_and_consumes_attempt() {
    let challenges = default_challenges(|_, _| {
        Ok(Some(joined(
            "99999999-8888-4777-8666-555555555555",
            "PlayerOne",
        )))
    });
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");

    assert!(
        challenges
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
    assert!(
        challenges
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
}

#[test]
fn rejects_mismatched_verified_name_and_consumes_attempt() {
    let challenges = default_challenges(|_, _| Ok(Some(joined(TEST_PROFILE, "SomeoneElse"))));
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");

    assert!(
        challenges
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
    assert!(
        challenges
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
}

#[test]
fn name_binding_is_ascii_case_insensitive_but_returns_provider_canonical_spelling() {
    let challenges = default_challenges(|_, _| Ok(Some(joined(TEST_PROFILE, "pLaYeRoNe"))));
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");

    let verified = challenges.complete(issued.challenge_id()).unwrap().unwrap();
    assert_eq!(verified.name().as_str(), "pLaYeRoNe");
}

#[test]
fn expiry_at_the_exact_deadline_skips_provider_invocation() {
    let ttl = Duration::from_secs(5);
    let clock = MutableClock::at(Instant::now());
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let challenges = compose(
        server(TEST_SERVER),
        FnProvider(move |_: &MinecraftName, _: &ServerHash| {
            calls_in_provider.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }),
        clock.clone(),
        limits(|value| value.ttl = ttl),
    );
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
    clock.advance(ttl);

    assert!(
        challenges
            .complete(issued.challenge_id())
            .unwrap()
            .is_none()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn challenge_expiring_while_provider_is_blocked_is_not_returned_as_verified() {
    let ttl = Duration::from_secs(5);
    let clock = MutableClock::at(Instant::now());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let provider = FnProvider(move |_: &MinecraftName, _: &ServerHash| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .expect("test must release the provider before its bounded wait expires");
        Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
    });
    let challenges = compose(
        server(TEST_SERVER),
        provider,
        clock.clone(),
        limits(|value| value.ttl = ttl),
    );
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let completion_challenges = challenges.clone();
    let challenge_id = issued.challenge_id().clone();
    let completion = thread::spawn(move || completion_challenges.complete(&challenge_id));

    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    if entered {
        clock.advance(ttl);
    }
    let _ = release_tx.send(());
    let result = completion.join().unwrap();

    assert!(entered, "provider was not entered within the bounded wait");
    assert!(result.unwrap().is_none());
}

#[test]
fn provider_failure_after_deadline_remains_unavailable_not_expired_none() {
    let ttl = Duration::from_secs(5);
    let clock = MutableClock::at(Instant::now());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let provider = FnProvider(move |_: &MinecraftName, _: &ServerHash| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .expect("test must release the provider before its bounded wait expires");
        Err(MinecraftError::unavailable())
    });
    let challenges = compose(
        server(TEST_SERVER),
        provider,
        clock.clone(),
        limits(|value| value.ttl = ttl),
    );
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let completion_challenges = challenges.clone();
    let challenge_id = issued.challenge_id().clone();
    let completion = thread::spawn(move || completion_challenges.complete(&challenge_id));

    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    if entered {
        clock.advance(ttl);
    }
    let _ = release_tx.send(());
    let result = completion.join().unwrap();

    assert!(entered, "provider was not entered within the bounded wait");
    assert_eq!(result.unwrap_err().code(), "minecraft_unavailable");
}

#[test]
fn invalid_profile_names_are_rejected_before_issue_can_be_called() {
    for invalid in ["", "contains space", "é", "a/b", "seventeen_chars!!"] {
        assert!(
            MinecraftName::parse(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
}

#[test]
fn profile_names_are_ascii_and_not_trimmed() {
    for valid in ["A", "_", "Player_123", "SixteenChars___X"] {
        assert_eq!(name(valid).as_str(), valid);
    }
    assert!(MinecraftName::parse("SixteenChars___XX").is_err());
    assert!(MinecraftName::parse(" PlayerOne").is_err());
    assert!(MinecraftName::parse("PlayerOne ").is_err());
    assert!(MinecraftName::parse("Player\tOne").is_err());
}

#[test]
fn uuid_parsing_accepts_compact_and_canonical_forms_but_rejects_nil_or_malformed() {
    let compact = MinecraftUuid::parse(TEST_PROFILE_ID).unwrap();
    let hyphenated = MinecraftUuid::parse(TEST_PROFILE).unwrap();
    assert_eq!(compact.as_uuid(), hyphenated.as_uuid());
    assert_eq!(
        ServerUuid::parse(TEST_SERVER).unwrap().as_uuid(),
        Uuid::parse_str(TEST_SERVER).unwrap()
    );

    for invalid in [
        "00000000-0000-0000-0000-000000000000",
        "00000000000000000000000000000000",
        "11111111-2222-4333-8444-55555555555",
        "11111111-2222-4333-8444-555555555555-",
        "1111111122224333844455555555555g",
        "11111111222243338444555555555555 ",
        " 11111111222243338444555555555555",
    ] {
        assert!(
            MinecraftUuid::parse(invalid).is_err(),
            "accepted {invalid:?}"
        );
        assert!(
            ServerUuid::parse(invalid).is_err(),
            "accepted server UUID {invalid:?}"
        );
    }
    assert!(MinecraftUuid::from_uuid(Uuid::nil()).is_err());
    assert!(ServerUuid::from_uuid(Uuid::nil()).is_err());
}

#[test]
fn challenge_tokens_are_exactly_64_lowercase_hex_characters() {
    assert!(ChallengeId::parse(&"a".repeat(64)).is_ok());
    for invalid in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        assert!(
            ChallengeId::parse(&invalid).is_err(),
            "accepted challenge token {invalid:?}"
        );
    }
}

#[test]
fn issued_hashes_are_forty_lowercase_hex_characters_and_challenge_ids_are_fresh() {
    let challenges = default_challenges(|_, _| Ok(None));
    let mut ids = std::collections::HashSet::new();
    let mut hashes = std::collections::HashSet::new();
    for _ in 0..32 {
        let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
        let id = issued.challenge_id().as_str();
        let hash = issued.server_hash().as_str();
        assert_eq!(id.len(), 64);
        assert!(
            id.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(hash.len(), 40);
        assert!(
            hash.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert!(ids.insert(id.to_owned()));
        assert!(hashes.insert(hash.to_owned()));
    }
}

#[test]
fn uuid_profile_and_challenge_debug_values_are_redacted() {
    let challenges = default_challenges(|_, _| Ok(Some(joined(TEST_PROFILE, "PlayerOne"))));
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let verified = challenges.complete(issued.challenge_id()).unwrap().unwrap();
    let rendered = format!(
        "{:?} {:?} {:?} {:?} {:?}",
        issued,
        verified,
        server(TEST_SERVER),
        profile_uuid(TEST_PROFILE),
        name("PlayerOne")
    );

    for secret in [
        TEST_SERVER,
        TEST_PROFILE,
        TEST_PROFILE_ID,
        "PlayerOne",
        issued.challenge_id().as_str(),
        issued.server_hash().as_str(),
    ] {
        assert!(
            !rendered.contains(secret),
            "debug output disclosed {secret:?}"
        );
    }
}

#[test]
fn errors_have_only_fixed_codes_and_redacted_display_and_debug() {
    let error = MinecraftUuid::parse("caller-secret-invalid-uuid").unwrap_err();
    assert_eq!(error.code(), "minecraft_invalid_request");
    assert_eq!(error.to_string(), "Minecraft verification failed");
    assert!(!format!("{error:?}").contains("caller-secret"));
    for (error, code) in [
        (MinecraftError::invalid(), "minecraft_invalid_request"),
        (MinecraftError::busy(), "minecraft_busy"),
        (MinecraftError::rate_limited(), "minecraft_rate_limited"),
        (MinecraftError::unavailable(), "minecraft_unavailable"),
    ] {
        assert_eq!(error.code(), code);
        assert_eq!(error.to_string(), "Minecraft verification failed");
        assert!(std::error::Error::source(&error).is_none());
        assert_eq!(
            format!("{error:?}"),
            format!("MinecraftError {{ code: {code:?} }}")
        );
    }
}

#[test]
fn atomic_completion_invokes_the_provider_at_most_once() {
    const WORKERS: usize = 8;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let provider = FnProvider(move |_: &MinecraftName, _: &ServerHash| {
        calls_in_provider.fetch_add(1, Ordering::SeqCst);
        let _ = entered_tx.send(());
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .expect("test must release the provider before its bounded wait expires");
        Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
    });
    let challenges = compose(
        server(TEST_SERVER),
        provider,
        MutableClock::at(Instant::now()),
        TestLimits::default(),
    );
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let barrier = Arc::new(Barrier::new(WORKERS));
    let (result_tx, result_rx) = mpsc::channel();
    let mut workers = Vec::new();
    for _ in 0..WORKERS {
        let challenge_id = issued.challenge_id().clone();
        let challenges = challenges.clone();
        let barrier = barrier.clone();
        let result_tx = result_tx.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            let result = challenges.complete(&challenge_id);
            let _ = result_tx.send(result);
        }));
    }
    drop(result_tx);

    let provider_entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    // The entered provider remains blocked, so every other completed attempt must already fail.
    let mut early_results = Vec::new();
    if provider_entered {
        for _ in 1..WORKERS {
            match result_rx.recv_timeout(Duration::from_secs(2)) {
                Ok(result) => early_results.push(result),
                Err(_) => break,
            }
        }
    }
    let _ = release_tx.send(());
    let mut results = early_results;
    while results.len() < WORKERS {
        match result_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(result) => results.push(result),
            Err(_) => break,
        }
    }
    for worker in workers {
        worker.join().unwrap();
    }

    assert!(
        provider_entered,
        "provider was not entered within the bounded wait"
    );
    assert_eq!(results.len(), WORKERS);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(Some(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(None)))
            .count(),
        WORKERS - 1
    );
    assert!(results.iter().all(|result| result.is_ok()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn blocked_provider_does_not_hold_the_challenge_store_mutex() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let returned = Arc::new(AtomicBool::new(false));
    let returned_in_provider = returned.clone();
    let provider = FnProvider(move |_: &MinecraftName, _: &ServerHash| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .expect("test must release the provider before its bounded wait expires");
        returned_in_provider.store(true, Ordering::SeqCst);
        Ok(None)
    });
    let challenges = compose(
        server(TEST_SERVER),
        provider,
        MutableClock::at(Instant::now()),
        TestLimits::default(),
    );
    let issued = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let completion_challenges = challenges.clone();
    let challenge_id = issued.challenge_id().clone();
    let completion = thread::spawn(move || completion_challenges.complete(&challenge_id));
    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();

    let (issue_tx, issue_rx) = mpsc::channel();
    let issue_challenges = challenges.clone();
    let issue_worker = thread::spawn(move || {
        let result = issue_challenges.issue(profile_uuid(TEST_PROFILE), name("SecondPlayer"));
        let _ = issue_tx.send(result.map(|_| ()));
    });
    let progressed_while_blocked =
        matches!(issue_rx.recv_timeout(Duration::from_secs(2)), Ok(Ok(())));
    let provider_still_blocked = !returned.load(Ordering::SeqCst);
    let _ = release_tx.send(());
    let completion_result = completion.join().unwrap();
    issue_worker.join().unwrap();

    assert!(entered, "provider was not entered within the bounded wait");
    assert!(
        progressed_while_blocked && provider_still_blocked,
        "issue must progress while the provider has not returned"
    );
    assert!(completion_result.unwrap().is_none());
}

#[test]
fn aggregate_outstanding_limit_counts_inflight_verification() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let provider = FnProvider(move |_: &MinecraftName, _: &ServerHash| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .expect("test must release the provider before its bounded wait expires");
        Ok(None)
    });
    let challenges = compose(
        server(TEST_SERVER),
        provider,
        MutableClock::at(Instant::now()),
        limits(|value| {
            value.max_outstanding = 2;
            value.issue_burst = 10;
        }),
    );
    let first = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let _second = issue(&challenges, TEST_PROFILE, "SecondPlayer");
    let completion_challenges = challenges.clone();
    let first_id = first.challenge_id().clone();
    let completion = thread::spawn(move || completion_challenges.complete(&first_id));
    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();

    let issue_result = challenges.issue(profile_uuid(TEST_PROFILE), name("ThirdPlayer"));
    let _ = release_tx.send(());
    let completion_result = completion.join().unwrap();

    assert!(entered, "provider was not entered within the bounded wait");
    assert_eq!(issue_result.unwrap_err().code(), "minecraft_busy");
    assert!(completion_result.unwrap().is_none());
}

#[test]
fn active_verification_limit_rejects_and_consumes_the_busy_attempt() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let provider = FnProvider(move |_: &MinecraftName, _: &ServerHash| {
        entered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .expect("test must release the provider before its bounded wait expires");
        Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
    });
    let challenges = compose(
        server(TEST_SERVER),
        provider,
        MutableClock::at(Instant::now()),
        limits(|value| {
            value.max_outstanding = 4;
            value.max_active = 1;
            value.issue_burst = 10;
        }),
    );
    let first = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let second = issue(&challenges, TEST_PROFILE, "SecondPlayer");
    let completion_challenges = challenges.clone();
    let first_id = first.challenge_id().clone();
    let completion = thread::spawn(move || completion_challenges.complete(&first_id));
    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();

    let busy = challenges.complete(second.challenge_id());
    let replay = challenges.complete(second.challenge_id());
    let _ = release_tx.send(());
    let first_result = completion.join().unwrap();

    assert!(entered, "provider was not entered within the bounded wait");
    assert_eq!(busy.unwrap_err().code(), "minecraft_busy");
    assert!(
        replay.unwrap().is_none(),
        "busy attempt did not consume its challenge"
    );
    assert!(first_result.unwrap().is_some());
}

#[test]
fn both_active_slots_bound_a_third_attempt_and_failures_release_all_admission() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let provider = FnProvider(move |requested: &MinecraftName, _: &ServerHash| {
        let call = calls_in_provider.fetch_add(1, Ordering::SeqCst);
        if call < 4 {
            let _ = entered_tx.send(());
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .expect("test must release the provider before its bounded wait expires");
        }
        if call == 0 {
            return Err(MinecraftError::unavailable());
        }
        if call == 1 {
            panic!("test provider panic while both slots are occupied");
        }
        Ok(Some(joined(TEST_PROFILE, requested.as_str())))
    });
    let challenges = compose(
        server(TEST_SERVER),
        provider,
        MutableClock::at(Instant::now()),
        limits(|value| {
            value.max_outstanding = 4;
            value.max_active = 2;
            value.issue_burst = 10;
        }),
    );
    let first = issue(&challenges, TEST_PROFILE, "FirstPlayer");
    let second = issue(&challenges, TEST_PROFILE, "SecondPlayer");
    let third = issue(&challenges, TEST_PROFILE, "ThirdPlayer");
    let (result_tx, result_rx) = mpsc::channel();
    let mut workers = Vec::new();
    for challenge_id in [first.challenge_id().clone(), second.challenge_id().clone()] {
        let challenges = challenges.clone();
        let result_tx = result_tx.clone();
        workers.push(thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                challenges.complete(&challenge_id)
            }));
            let _ = result_tx.send(result);
        }));
    }
    drop(result_tx);
    let first_entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let second_entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();

    let (third_attempt, third_replay) = if first_entered && second_entered {
        let attempt = challenges.complete(third.challenge_id());
        let replay = challenges.complete(third.challenge_id());
        (Some(attempt), Some(replay))
    } else {
        (None, None)
    };
    let _ = release_tx.send(());
    let _ = release_tx.send(());
    let mut outcomes = Vec::new();
    for _ in 0..2 {
        if let Ok(outcome) = result_rx.recv_timeout(Duration::from_secs(4)) {
            outcomes.push(outcome);
        }
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let first_replay = challenges.complete(first.challenge_id());
    let second_replay = challenges.complete(second.challenge_id());
    // Two simultaneous replacements are necessary: one success can hide one leaked slot.
    let fourth = issue(&challenges, TEST_PROFILE, "FourthPlayer");
    let fifth = issue(&challenges, TEST_PROFILE, "FifthPlayer");
    let sixth = issue(&challenges, TEST_PROFILE, "SixthPlayer");
    let mut replacements = Vec::new();
    for challenge_id in [fourth.challenge_id().clone(), fifth.challenge_id().clone()] {
        let challenges = challenges.clone();
        replacements.push(thread::spawn(move || challenges.complete(&challenge_id)));
    }
    let fourth_entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let fifth_entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let (sixth_attempt, sixth_replay) = if fourth_entered && fifth_entered {
        (
            Some(challenges.complete(sixth.challenge_id())),
            Some(challenges.complete(sixth.challenge_id())),
        )
    } else {
        (None, None)
    };
    let _ = release_tx.send(());
    let _ = release_tx.send(());
    let replacement_results: Vec<_> = replacements
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();

    assert!(
        first_entered && second_entered,
        "both provider slots did not become active"
    );
    assert_eq!(outcomes.len(), 2);
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_err()).count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(Err(_))))
            .count(),
        1
    );
    assert_eq!(third_attempt.unwrap().unwrap_err().code(), "minecraft_busy");
    assert!(
        third_replay.unwrap().unwrap().is_none(),
        "busy third attempt was not consumed"
    );
    assert!(first_replay.unwrap().is_none());
    assert!(second_replay.unwrap().is_none());
    assert!(
        fourth_entered && fifth_entered,
        "provider failure/panic did not release both active slots"
    );
    for result in replacement_results {
        assert!(result.unwrap().is_some());
    }
    assert_eq!(sixth_attempt.unwrap().unwrap_err().code(), "minecraft_busy");
    assert!(sixth_replay.unwrap().unwrap().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[test]
fn issue_rate_limit_refills_at_the_exact_interval_boundary() {
    let now = Instant::now();
    let clock = MutableClock::at(now);
    let challenges = compose(
        server(TEST_SERVER),
        FnProvider(|_: &MinecraftName, _: &ServerHash| Ok(None)),
        clock.clone(),
        limits(|value| {
            value.issue_burst = 2;
            value.refill_interval = Duration::from_secs(10);
        }),
    );
    let _first = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let _second = issue(&challenges, TEST_PROFILE, "SecondPlayer");
    let limited = challenges.issue(profile_uuid(TEST_PROFILE), name("ThirdPlayer"));
    clock.advance(Duration::from_secs(10));
    let refilled = challenges.issue(profile_uuid(TEST_PROFILE), name("FourthPlayer"));

    assert_eq!(limited.unwrap_err().code(), "minecraft_rate_limited");
    assert!(
        refilled.is_ok(),
        "one token was not refilled at the boundary"
    );
}

#[test]
fn issue_capacity_rejection_spends_a_rate_token() {
    let challenges = compose(
        server(TEST_SERVER),
        FnProvider(|_: &MinecraftName, _: &ServerHash| Ok(None)),
        MutableClock::at(Instant::now()),
        limits(|value| {
            value.max_outstanding = 1;
            value.max_active = 1;
            value.issue_burst = 2;
        }),
    );
    let _first = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let capacity_rejected = challenges.issue(profile_uuid(TEST_PROFILE), name("SecondPlayer"));
    let rate_rejected = challenges.issue(profile_uuid(TEST_PROFILE), name("ThirdPlayer"));

    assert_eq!(capacity_rejected.unwrap_err().code(), "minecraft_busy");
    assert_eq!(rate_rejected.unwrap_err().code(), "minecraft_rate_limited");
}

#[test]
fn verify_rate_limit_consumes_the_challenge_without_calling_provider_again() {
    let now = Instant::now();
    let clock = MutableClock::at(now);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let challenges = compose(
        server(TEST_SERVER),
        FnProvider(move |_: &MinecraftName, _: &ServerHash| {
            calls_in_provider.fetch_add(1, Ordering::SeqCst);
            Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
        }),
        clock.clone(),
        limits(|value| {
            value.verify_burst = 1;
            value.refill_interval = Duration::from_secs(10);
        }),
    );
    let first = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let second = issue(&challenges, TEST_PROFILE, "SecondPlayer");
    assert!(challenges.complete(first.challenge_id()).unwrap().is_some());

    let limited = challenges.complete(second.challenge_id());
    let replay = challenges.complete(second.challenge_id());
    clock.advance(Duration::from_secs(10));
    let third = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let refilled = challenges.complete(third.challenge_id());

    assert_eq!(limited.unwrap_err().code(), "minecraft_rate_limited");
    assert!(replay.unwrap().is_none());
    assert!(refilled.unwrap().is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn provider_failure_releases_active_admission_and_does_not_leak_a_challenge() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let challenges = compose(
        server(TEST_SERVER),
        FnProvider(move |_: &MinecraftName, _: &ServerHash| {
            if calls_in_provider.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(MinecraftError::unavailable())
            } else {
                Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
            }
        }),
        MutableClock::at(Instant::now()),
        limits(|value| {
            value.max_outstanding = 3;
            value.max_active = 1;
            value.issue_burst = 10;
        }),
    );
    let failed = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let failed_result = challenges.complete(failed.challenge_id());
    assert!(
        challenges
            .complete(failed.challenge_id())
            .unwrap()
            .is_none()
    );
    let next = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let next_result = challenges.complete(next.challenge_id());

    assert_eq!(failed_result.unwrap_err().code(), "minecraft_unavailable");
    assert!(next_result.unwrap().is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn provider_panic_releases_active_admission_and_does_not_leak_a_challenge() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_provider = calls.clone();
    let challenges = compose(
        server(TEST_SERVER),
        FnProvider(move |_: &MinecraftName, _: &ServerHash| {
            if calls_in_provider.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("test provider panic");
            }
            Ok(Some(joined(TEST_PROFILE, "PlayerOne")))
        }),
        MutableClock::at(Instant::now()),
        limits(|value| {
            value.max_outstanding = 3;
            value.max_active = 1;
            value.issue_burst = 10;
        }),
    );
    let first = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        challenges.complete(first.challenge_id())
    }));
    let next = issue(&challenges, TEST_PROFILE, "PlayerOne");
    let next_result = challenges.complete(next.challenge_id());
    assert!(challenges.complete(first.challenge_id()).unwrap().is_none());

    assert!(
        panic_result.is_err(),
        "provider panic did not unwind through the test boundary"
    );
    assert!(
        next_result.unwrap().is_some(),
        "verification permit leaked after provider panic"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn composition_rejects_test_caps_above_production_bounds() {
    let too_many = MinecraftChallenges::compose(
        server(TEST_SERVER),
        Arc::new(FnProvider(|_: &MinecraftName, _: &ServerHash| Ok(None))),
        Arc::new(MutableClock::at(Instant::now())),
        limits(|value| value.max_outstanding = 1025),
    );
    let too_long = MinecraftChallenges::compose(
        server(TEST_SERVER),
        Arc::new(FnProvider(|_: &MinecraftName, _: &ServerHash| Ok(None))),
        Arc::new(MutableClock::at(Instant::now())),
        limits(|value| value.ttl = Duration::from_secs(121)),
    );

    assert!(matches!(too_many, Err(error) if error.code() == "minecraft_invalid_request"));
    assert!(matches!(too_long, Err(error) if error.code() == "minecraft_invalid_request"));
}

fn decode(status: u16, body: impl AsRef<[u8]>) -> Result<Option<JoinedProfile>, MinecraftError> {
    provider::decode_response(status, Cursor::new(body.as_ref()))
}

#[test]
fn provider_response_decodes_expected_profile_and_canonical_name() {
    let decoded = decode(200, MOJANG_BODY).unwrap().unwrap();
    assert_eq!(
        decoded.uuid.as_uuid(),
        Uuid::parse_str(TEST_PROFILE).unwrap()
    );
    assert_eq!(decoded.name.as_str(), "PlayerOne");
}

#[test]
fn provider_response_rejects_duplicate_fields_coercions_invalid_utf8_and_trailing_data() {
    let duplicate_id = r#"{"id":"11111111222243338444555555555555","id":"11111111222243338444555555555555","name":"PlayerOne"}"#;
    let duplicate_name =
        r#"{"id":"11111111222243338444555555555555","name":"PlayerOne","name":"PlayerOne"}"#;
    let numeric_id = r#"{"id":11111111222243338444555555555555,"name":"PlayerOne"}"#;
    let numeric_name = r#"{"id":"11111111222243338444555555555555","name":1}"#;
    for invalid in [
        duplicate_id.as_bytes(),
        duplicate_name.as_bytes(),
        numeric_id.as_bytes(),
        numeric_name.as_bytes(),
    ] {
        assert!(decode(200, invalid).is_err());
    }
    assert!(decode(200, [0xff, 0xfe]).is_err());
    assert!(decode(200, format!("{MOJANG_BODY} trailing")).is_err());
    assert!(decode(200, format!("{MOJANG_BODY}{MOJANG_BODY}")).is_err());
}

#[test]
fn provider_response_requires_a_root_object_and_bounds_unknown_property_depth() {
    let root_array = format!("[{MOJANG_BODY}]");
    assert!(decode(200, root_array).is_err());

    // Root object plus 31 arrays is the exact depth-32 bound, below serde's own cap.
    let at_bound = format!("{}null{}", "[".repeat(31), "]".repeat(31));
    let accepted =
        format!(r#"{{"id":"{TEST_PROFILE_ID}","name":"PlayerOne","extra":{at_bound}}}"#,);
    assert!(decode(200, accepted).unwrap().is_some());
    let over_bound = format!("{}null{}", "[".repeat(32), "]".repeat(32));
    let rejected =
        format!(r#"{{"id":"{TEST_PROFILE_ID}","name":"PlayerOne","extra":{over_bound}}}"#,);
    assert_eq!(
        decode(200, rejected).unwrap_err().code(),
        "minecraft_unavailable"
    );
}

#[test]
fn provider_response_rejects_deeply_nested_json() {
    let mut body = "[".repeat(256);
    body.push_str("null");
    body.push_str(&"]".repeat(256));
    assert!(decode(200, body).is_err());
}

#[test]
fn provider_body_limit_accepts_exactly_64_kib_and_rejects_overflow_despite_valid_prefix() {
    const MAX: usize = 64 * 1024;
    assert!(MOJANG_BODY.len() < MAX);
    let exact = format!("{}{}", MOJANG_BODY, " ".repeat(MAX - MOJANG_BODY.len()));
    assert_eq!(exact.len(), MAX);
    assert!(decode(200, exact.as_bytes()).unwrap().is_some());

    let over = format!("{}{}", MOJANG_BODY, " ".repeat(MAX + 1 - MOJANG_BODY.len()));
    assert_eq!(over.len(), MAX + 1);
    assert!(decode(200, over.as_bytes()).is_err());
}

#[test]
fn provider_response_accepts_ordinary_nested_mojang_properties_without_trusting_them() {
    let body = format!(
        r#"{{"id":"{TEST_PROFILE_ID}","name":"PlayerOne","properties":[{{"name":"textures","value":"opaque-property","signature":"opaque-signature"}}],"extra":{{"note":"brackets [] and escaped quote \" stay inside a string"}}}}"#,
    );
    let decoded = decode(200, body).unwrap().unwrap();
    assert_eq!(decoded.uuid, profile_uuid(TEST_PROFILE));
    assert_eq!(decoded.name.as_str(), "PlayerOne");
}

#[test]
fn provider_root_fields_and_key_bytes_have_exact_bounds_and_duplicates_are_rejected() {
    let mut body = MOJANG_BODY.trim_end_matches('}').to_owned();
    for index in 0..30 {
        body.push_str(&format!(r#", "extra{index}":null"#));
    }
    assert!(decode(200, format!("{body}}}")).unwrap().is_some());
    assert!(decode(200, format!(r#"{body},"overflow":null}}"#)).is_err());
    assert!(decode(200, format!(r#"{body},"extra0":false}}"#)).is_err());

    let key = "k".repeat(128);
    assert!(
        decode(
            200,
            format!(r#"{},"{key}":null}}"#, MOJANG_BODY.trim_end_matches('}'))
        )
        .unwrap()
        .is_some()
    );
    let key = "k".repeat(129);
    assert!(
        decode(
            200,
            format!(r#"{},"{key}":null}}"#, MOJANG_BODY.trim_end_matches('}'))
        )
        .is_err()
    );
    let duplicate = format!(
        r#"{},"extra":null,"extra":false}}"#,
        MOJANG_BODY.trim_end_matches('}')
    );
    assert!(decode(200, duplicate).is_err());
}

#[test]
fn provider_wire_identity_requires_compact_non_nil_uuid_and_valid_name() {
    for id in [
        TEST_PROFILE,
        "00000000000000000000000000000000",
        "1111111122224333844455555555555g",
    ] {
        let body = format!(r#"{{"id":"{id}","name":"PlayerOne"}}"#);
        assert_eq!(
            decode(200, body).unwrap_err().code(),
            "minecraft_unavailable"
        );
    }
    for invalid in ["", "Player One", "é", "PlayerOne ", "SeventeenChars__X"] {
        let body = format!(r#"{{"id":"{TEST_PROFILE_ID}","name":"{invalid}"}}"#);
        assert_eq!(
            decode(200, body).unwrap_err().code(),
            "minecraft_unavailable"
        );
    }
}

struct MustNotRead {
    reads: Arc<AtomicUsize>,
}

impl Read for MustNotRead {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other(
            "body must not be read for this status",
        ))
    }
}

#[test]
fn non_success_provider_statuses_are_handled_without_reading_the_body() {
    for status in [204, 400, 500] {
        let reads = Arc::new(AtomicUsize::new(0));
        let reader = MustNotRead {
            reads: reads.clone(),
        };
        let result = provider::decode_response(status, reader);
        if status == 204 {
            assert!(result.unwrap().is_none());
        } else {
            assert_eq!(result.unwrap_err().code(), "minecraft_unavailable");
        }
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "read body for status {status}"
        );
    }
}
