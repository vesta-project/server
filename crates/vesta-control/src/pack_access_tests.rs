//! Pure in-memory fixtures. Proof objects are built with test-only constructors; no
//! Mojang request, database, listener or game is involved.
use super::*;

const SERVER: &str = "11111111-2222-4333-8444-555555555555";
const OTHER_SERVER: &str = "99999999-2222-4333-8444-555555555555";
const PLAYER: &str = "550e8400-e29b-41d4-a716-446655440000";
const STRANGER: &str = "123e4567-e89b-42d3-a456-426614174000";

fn server() -> ServerUuid {
    ServerUuid::parse(SERVER).unwrap()
}
fn player(text: &str) -> MinecraftUuid {
    MinecraftUuid::parse(text).unwrap()
}
fn properties(whitelist: bool, online: bool) -> ServerProperties {
    ServerProperties::parse(format!("white-list={whitelist}\nonline-mode={online}\n").as_bytes())
        .unwrap()
}
fn whitelist(players: &[&str]) -> Whitelist {
    let entries: Vec<String> = players
        .iter()
        .map(|uuid| format!("{{\"uuid\":\"{uuid}\"}}"))
        .collect();
    Whitelist::parse(format!("[{}]", entries.join(",")).as_bytes()).unwrap()
}
fn bans(players: &[&str]) -> BanList {
    let entries: Vec<String> = players
        .iter()
        .map(|uuid| format!("{{\"uuid\":\"{uuid}\",\"expires\":\"forever\"}}"))
        .collect();
    BanList::parse(format!("[{}]", entries.join(",")).as_bytes()).unwrap()
}
fn revision(value: u64) -> PasswordRevision {
    PasswordRevision::new(value).unwrap()
}

struct Admission {
    whitelist: Whitelist,
    bans: BanList,
}
impl Admission {
    fn new(whitelisted: &[&str], banned: &[&str]) -> Self {
        Self {
            whitelist: whitelist(whitelisted),
            bans: bans(banned),
        }
    }
    fn policy(
        &self,
        mode: PackAccessMode,
        properties: Option<ServerProperties>,
        password: Option<PasswordRevision>,
    ) -> PackAccessPolicy<'_> {
        PackAccessPolicy {
            server: server(),
            mode,
            properties,
            whitelist: &self.whitelist,
            bans: &self.bans,
            password,
        }
    }
}

#[test]
fn minecraft_policy_uses_the_servers_real_whitelist_bans_and_online_mode() {
    let admission = Admission::new(&[PLAYER], &[]);
    let mode = PackAccessMode::Minecraft;
    let open = admission.policy(mode, Some(properties(false, true)), None);
    assert!(open.admits(player(PLAYER)));
    assert!(open.admits(player(STRANGER)));

    let whitelisted = admission.policy(mode, Some(properties(true, true)), None);
    assert!(whitelisted.admits(player(PLAYER)));
    assert!(!whitelisted.admits(player(STRANGER)));

    // Offline-mode identity is not trustworthy and unknown properties fail closed.
    assert!(
        !admission
            .policy(mode, Some(properties(false, false)), None)
            .admits(player(PLAYER))
    );
    assert!(!admission.policy(mode, None, None).admits(player(PLAYER)));

    let banned = Admission::new(&[PLAYER], &[PLAYER]);
    assert!(
        !banned
            .policy(mode, Some(properties(false, true)), None)
            .admits(player(PLAYER))
    );
}

#[test]
fn minecraft_grants_require_this_servers_proof_and_admission() {
    let admission = Admission::new(&[PLAYER], &[]);
    let policy = admission.policy(
        PackAccessMode::Minecraft,
        Some(properties(true, true)),
        None,
    );
    let proof = VerifiedMinecraftProfile::for_test(server(), player(PLAYER));
    assert_eq!(
        policy.grant_minecraft(&proof),
        Some(PackGrant::Minecraft(player(PLAYER)))
    );
    let foreign = VerifiedMinecraftProfile::for_test(
        ServerUuid::parse(OTHER_SERVER).unwrap(),
        player(PLAYER),
    );
    assert_eq!(policy.grant_minecraft(&foreign), None);
    let outsider = VerifiedMinecraftProfile::for_test(server(), player(STRANGER));
    assert_eq!(policy.grant_minecraft(&outsider), None);
}

#[test]
fn password_grants_need_password_mode_a_match_and_the_current_revision() {
    let admission = Admission::new(&[], &[]);
    let matched =
        PasswordVerification::for_test(SERVER, revision(3), PasswordVerificationStatus::Match);
    let both = admission.policy(PackAccessMode::MinecraftOrPassword, None, Some(revision(3)));
    assert_eq!(
        both.grant_password(&matched),
        Some(PackGrant::Password(revision(3)))
    );
    let minecraft_only = admission.policy(PackAccessMode::Minecraft, None, Some(revision(3)));
    assert_eq!(minecraft_only.grant_password(&matched), None);
    let rotated = admission.policy(PackAccessMode::MinecraftOrPassword, None, Some(revision(4)));
    assert_eq!(rotated.grant_password(&matched), None);
    let revoked = admission.policy(PackAccessMode::MinecraftOrPassword, None, None);
    assert_eq!(revoked.grant_password(&matched), None);
    for status in [
        PasswordVerificationStatus::Mismatch,
        PasswordVerificationStatus::Changed,
        PasswordVerificationStatus::Unavailable,
        PasswordVerificationStatus::RateLimited,
    ] {
        let attempt = PasswordVerification::for_test(SERVER, revision(3), status);
        assert_eq!(both.grant_password(&attempt), None, "{status:?}");
    }
    let foreign = PasswordVerification::for_test(
        OTHER_SERVER,
        revision(3),
        PasswordVerificationStatus::Match,
    );
    assert_eq!(both.grant_password(&foreign), None);
}

#[test]
fn sessions_authenticate_until_expiry_and_secrets_are_single_purpose() {
    let admission = Admission::new(&[], &[]);
    let policy = admission.policy(
        PackAccessMode::Minecraft,
        Some(properties(false, true)),
        None,
    );
    let sessions = PackSessions::new(server(), SessionLimits::default());
    let start = Instant::now();
    let grant = PackGrant::Minecraft(player(PLAYER));
    let (secret, session) = sessions.issue(grant, start).unwrap();
    assert_eq!(secret.expose().len(), 43);
    assert_eq!(session.grant(), grant);
    assert_eq!(session.expires_at(), start + Duration::from_secs(3_600));
    assert_eq!(
        sessions
            .authenticate(secret.expose(), start, &policy)
            .unwrap(),
        session
    );
    let almost = start + Duration::from_secs(3_599);
    assert!(
        sessions
            .authenticate(secret.expose(), almost, &policy)
            .is_ok()
    );
    let expired = start + Duration::from_secs(3_600);
    assert_eq!(
        sessions.authenticate(secret.expose(), expired, &policy),
        Err(PackAccessError::Invalid)
    );
    // Expired sessions are removed, not merely refused.
    assert_eq!(sessions.len().unwrap(), (0, 0));

    let (other, _) = sessions.issue(grant, start).unwrap();
    assert_ne!(other.expose(), secret.expose());
    for bogus in [
        "",
        "short",
        &"A".repeat(43).replace('A', "+"),
        &"A".repeat(44),
    ] {
        assert_eq!(
            sessions.authenticate(bogus, start, &policy),
            Err(PackAccessError::Invalid)
        );
    }
    assert_eq!(
        sessions.authenticate(&"A".repeat(43), start, &policy),
        Err(PackAccessError::Invalid)
    );
}

#[test]
fn a_ban_whitelist_removal_or_password_rotation_ends_existing_sessions() {
    let sessions = PackSessions::new(server(), SessionLimits::default());
    let now = Instant::now();
    let (minecraft, _) = sessions
        .issue(PackGrant::Minecraft(player(PLAYER)), now)
        .unwrap();
    let (password, _) = sessions
        .issue(PackGrant::Password(revision(2)), now)
        .unwrap();
    let allowed = Admission::new(&[PLAYER], &[]);
    let policy = allowed.policy(
        PackAccessMode::MinecraftOrPassword,
        Some(properties(true, true)),
        Some(revision(2)),
    );
    assert!(
        sessions
            .authenticate(minecraft.expose(), now, &policy)
            .is_ok()
    );
    assert!(
        sessions
            .authenticate(password.expose(), now, &policy)
            .is_ok()
    );

    let banned = Admission::new(&[PLAYER], &[PLAYER]);
    let after_ban = banned.policy(
        PackAccessMode::MinecraftOrPassword,
        Some(properties(true, true)),
        Some(revision(3)),
    );
    assert_eq!(
        sessions.authenticate(minecraft.expose(), now, &after_ban),
        Err(PackAccessError::Invalid)
    );
    assert_eq!(
        sessions.authenticate(password.expose(), now, &after_ban),
        Err(PackAccessError::Invalid)
    );
    // Refused sessions are gone even if the policy is restored.
    assert_eq!(
        sessions.authenticate(minecraft.expose(), now, &policy),
        Err(PackAccessError::Invalid)
    );
    assert_eq!(sessions.len().unwrap(), (0, 0));

    let (unlisted, _) = sessions
        .issue(PackGrant::Minecraft(player(PLAYER)), now)
        .unwrap();
    let removed = Admission::new(&[], &[]);
    let after_removal = removed.policy(
        PackAccessMode::Minecraft,
        Some(properties(true, true)),
        None,
    );
    assert_eq!(
        sessions.authenticate(unlisted.expose(), now, &after_removal),
        Err(PackAccessError::Invalid)
    );
}

#[test]
fn tickets_are_single_use_short_lived_and_never_outlive_their_session() {
    let admission = Admission::new(&[], &[]);
    let policy = admission.policy(
        PackAccessMode::Minecraft,
        Some(properties(false, true)),
        None,
    );
    let limits =
        SessionLimits::new(Duration::from_secs(40), Duration::from_secs(30), 8, 8).unwrap();
    let sessions = PackSessions::new(server(), limits);
    let start = Instant::now();
    let grant = PackGrant::Minecraft(player(PLAYER));
    let (secret, session) = sessions.issue(grant, start).unwrap();

    let ticket = sessions
        .issue_ticket(secret.expose(), start, &policy)
        .unwrap();
    assert_ne!(ticket.expose(), secret.expose());
    // A ticket is not a session secret and a session secret is not a ticket.
    assert_eq!(
        sessions.authenticate(ticket.expose(), start, &policy),
        Err(PackAccessError::Invalid)
    );
    assert_eq!(
        sessions.redeem_ticket(secret.expose(), start, &policy),
        Err(PackAccessError::Invalid)
    );
    assert_eq!(
        sessions
            .redeem_ticket(ticket.expose(), start, &policy)
            .unwrap(),
        session
    );
    assert_eq!(
        sessions.redeem_ticket(ticket.expose(), start, &policy),
        Err(PackAccessError::Invalid)
    );

    let late = sessions
        .issue_ticket(secret.expose(), start, &policy)
        .unwrap();
    assert_eq!(
        sessions.redeem_ticket(late.expose(), start + Duration::from_secs(30), &policy),
        Err(PackAccessError::Invalid)
    );

    // Issued 20 s in, a 30 s ticket is capped at the session's 40 s expiry.
    let capped = sessions
        .issue_ticket(secret.expose(), start + Duration::from_secs(20), &policy)
        .unwrap();
    assert_eq!(
        sessions.redeem_ticket(capped.expose(), start + Duration::from_secs(40), &policy),
        Err(PackAccessError::Invalid)
    );

    // Revoking the session invalidates its outstanding tickets.
    let pending = sessions
        .issue_ticket(secret.expose(), start, &policy)
        .unwrap();
    assert!(sessions.revoke(secret.expose()).unwrap());
    assert!(!sessions.revoke(secret.expose()).unwrap());
    assert_eq!(
        sessions.redeem_ticket(pending.expose(), start, &policy),
        Err(PackAccessError::Invalid)
    );
}

#[test]
fn stores_are_bounded_and_refuse_rather_than_evict_live_sessions() {
    let admission = Admission::new(&[], &[]);
    let policy = admission.policy(
        PackAccessMode::Minecraft,
        Some(properties(false, true)),
        None,
    );
    let limits = SessionLimits::new(Duration::from_secs(10), Duration::from_secs(5), 2, 1).unwrap();
    let sessions = PackSessions::new(server(), limits);
    let start = Instant::now();
    let grant = PackGrant::Minecraft(player(PLAYER));
    let (first, _) = sessions.issue(grant, start).unwrap();
    sessions.issue(grant, start).unwrap();
    assert_eq!(
        sessions.issue(grant, start).unwrap_err(),
        PackAccessError::Saturated
    );
    assert!(
        sessions
            .authenticate(first.expose(), start, &policy)
            .is_ok()
    );

    sessions
        .issue_ticket(first.expose(), start, &policy)
        .unwrap();
    assert_eq!(
        sessions
            .issue_ticket(first.expose(), start, &policy)
            .unwrap_err(),
        PackAccessError::Saturated
    );
    // Expired entries free capacity.
    let later = start + Duration::from_secs(10);
    sessions.issue(grant, later).unwrap();
    assert_eq!(sessions.len().unwrap(), (1, 0));
}

#[test]
fn secrets_are_bound_to_the_issuing_server() {
    let admission = Admission::new(&[], &[]);
    let policy = admission.policy(
        PackAccessMode::Minecraft,
        Some(properties(false, true)),
        None,
    );
    let here = PackSessions::new(server(), SessionLimits::default());
    let elsewhere = PackSessions::new(
        ServerUuid::parse(OTHER_SERVER).unwrap(),
        SessionLimits::default(),
    );
    let now = Instant::now();
    let (secret, _) = here
        .issue(PackGrant::Minecraft(player(PLAYER)), now)
        .unwrap();
    assert_eq!(
        elsewhere.authenticate(secret.expose(), now, &policy),
        Err(PackAccessError::Invalid)
    );
    assert_ne!(here.key(secret.expose()), elsewhere.key(secret.expose()));
}

#[test]
fn limits_are_validated_and_output_is_redacted() {
    for (ttl, ticket, sessions, tickets) in [
        (Duration::ZERO, Duration::from_secs(1), 1, 1),
        (
            MAX_SESSION_TTL + Duration::from_secs(1),
            Duration::from_secs(1),
            1,
            1,
        ),
        (Duration::from_secs(1), Duration::ZERO, 1, 1),
        (
            Duration::from_secs(1),
            MAX_TICKET_TTL + Duration::from_secs(1),
            1,
            1,
        ),
        (Duration::from_secs(1), Duration::from_secs(1), 0, 1),
        (
            Duration::from_secs(1),
            Duration::from_secs(1),
            MAX_SESSIONS + 1,
            1,
        ),
        (Duration::from_secs(1), Duration::from_secs(1), 1, 0),
        (
            Duration::from_secs(1),
            Duration::from_secs(1),
            1,
            MAX_TICKETS + 1,
        ),
    ] {
        assert_eq!(
            SessionLimits::new(ttl, ticket, sessions, tickets),
            Err(PackAccessError::InvalidLimits)
        );
    }
    let sessions = PackSessions::new(server(), SessionLimits::default());
    let (secret, session) = sessions
        .issue(PackGrant::Minecraft(player(PLAYER)), Instant::now())
        .unwrap();
    let text = format!("{secret:?} {session:?} {sessions:?}");
    assert!(!text.contains(secret.expose()));
    assert!(!text.contains(PLAYER));
    assert_eq!(PackAccessError::Invalid.to_string(), "pack_session_invalid");
}

#[test]
fn realtime_tickets_are_purpose_bound_single_use_and_recheck_live_sessions() {
    let admission = Admission::new(&[], &[]);
    let policy = admission.policy(
        PackAccessMode::Minecraft,
        Some(properties(false, true)),
        None,
    );
    let sessions = PackSessions::new(server(), SessionLimits::default());
    let now = Instant::now();
    let (secret, session) = sessions
        .issue(PackGrant::Minecraft(player(PLAYER)), now)
        .unwrap();
    let download = sessions
        .issue_ticket(secret.expose(), now, &policy)
        .unwrap();
    assert_eq!(
        sessions.redeem_realtime(download.expose(), now, &policy),
        Err(PackAccessError::Invalid)
    );
    assert_eq!(
        sessions.redeem_ticket(download.expose(), now, &policy),
        Err(PackAccessError::Invalid)
    );
    let wrong = sessions
        .issue_ticket_for(secret.expose(), TicketPurpose::Realtime, now, &policy)
        .unwrap();
    assert_eq!(
        sessions.redeem_ticket(wrong.expose(), now, &policy),
        Err(PackAccessError::Invalid)
    );
    assert_eq!(
        sessions.redeem_realtime(wrong.expose(), now, &policy),
        Err(PackAccessError::Invalid)
    );
    let ticket = sessions
        .issue_ticket_for(secret.expose(), TicketPurpose::Realtime, now, &policy)
        .unwrap();
    let (reference, redeemed) = sessions
        .redeem_realtime(ticket.expose(), now, &policy)
        .unwrap();
    assert_eq!(redeemed, session);
    assert_eq!(
        sessions.redeem_realtime(ticket.expose(), now, &policy),
        Err(PackAccessError::Invalid)
    );
    assert_eq!(
        sessions.recheck_realtime(reference, now, &policy).unwrap(),
        session
    );
    assert_eq!(
        sessions.recheck_realtime(reference, session.expires_at(), &policy),
        Err(PackAccessError::Invalid)
    );
    let (secret, _) = sessions
        .issue(PackGrant::Minecraft(player(PLAYER)), now)
        .unwrap();
    let ticket = sessions
        .issue_ticket_for(secret.expose(), TicketPurpose::Realtime, now, &policy)
        .unwrap();
    let (reference, _) = sessions
        .redeem_realtime(ticket.expose(), now, &policy)
        .unwrap();
    sessions.revoke(secret.expose()).unwrap();
    assert_eq!(
        sessions.recheck_realtime(reference, now, &policy),
        Err(PackAccessError::Invalid)
    );
    let (secret, _) = sessions
        .issue(PackGrant::Minecraft(player(PLAYER)), now)
        .unwrap();
    let ticket = sessions
        .issue_ticket_for(secret.expose(), TicketPurpose::Realtime, now, &policy)
        .unwrap();
    let (reference, _) = sessions
        .redeem_realtime(ticket.expose(), now, &policy)
        .unwrap();
    let banned = Admission::new(&[], &[PLAYER]);
    let denied = banned.policy(
        PackAccessMode::Minecraft,
        Some(properties(false, true)),
        None,
    );
    assert_eq!(
        sessions.recheck_realtime(reference, now, &denied),
        Err(PackAccessError::Invalid)
    );
}
