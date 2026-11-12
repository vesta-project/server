//! Resource-scope family: canonical `Resource` components, `ResourceScope` forms,
//! segment-bounded matching, the deliberate allow/deny case asymmetry, `covers` /
//! `overlaps`, and the proof that delegation can never widen delegated authority.
//!
//! Java reference:
//! `IdentityAuthorizationTest.scopesAreCanonicalSegmentBoundedAndCannotWidenDelegatedAuthority`.
//! The Rust model is a deliberate conservative replacement for the Java string
//! emulation: there is no `trim()`, no separator rewriting and no `toLowerCase()`.
//! Canonical means exactly one spelling per resource, or the value is refused.
//!
//! Sections A-H follow the family briefing. Section C (segment boundaries) and
//! section F (delegation cannot widen) are the safety-critical ones.

use super::support::Fixture;
use crate::identity::{
    AuthorityChange, BuiltinRole, Capability, CustomRank, CustomRole, Decision, DenyReason, Effect,
    Expiry, PolicySnapshot, RemoteCommand, Resource, ResourceScope, RoleGrant, RoleId, RoleName,
    Subject,
};
use vesta_state::audit::MAX_METADATA_VALUE_UNITS;

// ------------------------------------------------------------------ helpers

fn resource(text: &str) -> Resource {
    Resource::parse(text).unwrap_or_else(|_| panic!("{text:?} must be a canonical resource"))
}

fn scope(text: &str) -> ResourceScope {
    ResourceScope::parse(text).unwrap_or_else(|_| panic!("{text:?} must be a canonical scope"))
}

fn snapshot(fixture: &Fixture, subject: &Subject) -> PolicySnapshot {
    fixture
        .identities
        .current_policy(Some(subject))
        .expect("current policy is observable")
}

fn viewer(fixture: &Fixture) -> Subject {
    fixture.subject(RoleId::from(BuiltinRole::Viewer))
}

fn pack_user(fixture: &Fixture) -> Subject {
    fixture.subject(RoleId::from(BuiltinRole::PackUser))
}

fn grant(capability: Capability, text: &str) -> RoleGrant {
    RoleGrant::new(capability, scope(text)).expect("canonical role grant")
}

fn set_allow(
    fixture: &Fixture,
    actor: &Subject,
    target: &Subject,
    capability: Capability,
    text: &str,
) -> crate::Result<AuthorityChange> {
    fixture.identities.apply_remote(
        actor,
        RemoteCommand::SetOverride {
            principal: target.principal.clone(),
            grant: grant(capability, text),
            effect: Effect::Allow,
            expiry: Expiry::Never,
        },
    )
}

/// A principal whose only `files.read` allow is `allow_scope`, which also holds
/// `roles.assign` at the whole scope. Delegation attempts are therefore made by an
/// actor that genuinely holds the narrowed authority, so the decision it receives is
/// about scope containment rather than about a missing capability.
fn narrowed_delegate(fixture: &Fixture, owner: &Subject, name: &str, allow_scope: &str) -> Subject {
    let change = fixture
        .identities
        .apply_remote(
            owner,
            RemoteCommand::CreateRole(
                CustomRole::new(
                    RoleName::parse(name).expect("custom role name"),
                    CustomRank::new(15).expect("custom rank below the administrator tier"),
                    vec![
                        RoleGrant::new(Capability::RolesAssign, ResourceScope::ALL)
                            .expect("global grant"),
                        grant(Capability::FilesRead, allow_scope),
                    ],
                )
                .expect("a two-grant custom role"),
            ),
        )
        .expect("create narrowed custom role");
    let AuthorityChange::Role(role) = change else {
        panic!("create role must report the new role, got {change:?}");
    };
    let actor = pack_user(fixture);
    fixture
        .identities
        .apply_remote(
            owner,
            RemoteCommand::AssignRole {
                principal: actor.principal.clone(),
                role,
                expiry: Expiry::Never,
            },
        )
        .expect("assign the narrowed delegate role");
    actor
}

/// A path whose components have exactly these byte lengths, joined with `/`.
fn path_with_components(lengths: &[usize]) -> String {
    lengths
        .iter()
        .map(|length| "a".repeat(*length))
        .collect::<Vec<String>>()
        .join("/")
}

/// The longest exact scope still accepted. A scope's text is recorded verbatim in the audit
/// record of every grant that carries it, so its bound is the audit metadata value bound, not
/// `Resource`'s own generous path bound. Three of those units are spent on a `/**` suffix.
const LONGEST_ACCEPTED_PATH: [usize; 2] = [MAX_METADATA_VALUE_UNITS - 5, 1];

/// Scope/resource pairs used for the general "a deny is never narrower than its
/// own allow" law.
const PROBE_SCOPES: [&str; 7] = [
    "*",
    "worlds",
    "worlds/**",
    "worlds/nether",
    "worlds/nether/**",
    "worlds2",
    "worlds2/**",
];
const PROBE_RESOURCES: [&str; 9] = [
    "",
    "worlds",
    "Worlds",
    "WORLDS",
    "worlds/nether",
    "worlds/nether/region",
    "worlds2",
    "worldsX/nether",
    "worldX",
];

// =============================== A: Resource::parse canonical component rules

#[test]
fn root_and_canonical_components_are_accepted_verbatim() {
    for text in [
        "worlds",
        "worlds/nether",
        "a/b/c",
        "config/options.txt",
        "logs/latest.log",
        "world-1.2",
        // `..hidden` and `a..b` contain dots but are neither `.` nor `..`, and
        // `..hidden` does not end with a dot: both are ordinary components.
        "..hidden",
        "a..b",
        ".hidden",
        // Interior spaces are legal; only leading and trailing ones are refused.
        "file with spaces.txt",
        // Case is preserved verbatim. Whether two spellings compare equal is a
        // matching question (section D), not a canonicalisation question.
        "Worlds",
        "WORLDS/NETHER",
        "nether-1.20.1",
        // The device rule is a bounded alias list, not a prefix rule.
        "conx",
        "a-b_c.d",
        "~backup",
    ] {
        let parsed = resource(text);
        assert_eq!(
            parsed.as_str(),
            text,
            "a canonical resource is stored verbatim, never trimmed or recased"
        );
        assert!(!parsed.is_root(), "{text:?} is not the root resource");
    }

    assert_eq!(Resource::ROOT.as_str(), "");
    assert!(Resource::ROOT.is_root());
    assert_eq!(resource("").as_str(), "");
    assert!(resource("").is_root());
    assert_eq!(Resource::ROOT, resource(""), "the empty text is the root");
}

#[test]
fn a_resource_rejects_empty_components_dot_segments_controls_and_reserved_characters() {
    for text in [
        // Empty components: `a//b`, a leading `/` and a trailing `/` are all the
        // same defect and none of them is trimmed away.
        "a//b",
        "a///b",
        "/worlds",
        "worlds/",
        "/",
        "//",
        "a/",
        "/a",
        // `.` and `..` are refused anywhere in the path, not just at the front.
        ".",
        "..",
        "./worlds",
        "worlds/.",
        "worlds/..",
        "../etc",
        "worlds/../etc",
        "a/./b",
        "a/../b",
        // Control characters.
        "a\u{0}b",
        "a\nb",
        "a\tb",
        "a\rb",
        "a\u{1}b",
        "a\u{1b}b",
        "a\u{7f}b",
        // `:` `*` `?` `\` `<` `>` `|` `"`.
        "a:b",
        "a*b",
        "a?b",
        "a\\b",
        "a<b",
        "a>b",
        "a|b",
        "a\"b",
        // The two Windows spellings a POSIX-only lexical rule would otherwise miss.
        "C:/root",
        "//host/share",
        "config/a:stream",
        // Leading space, trailing space, trailing dot.
        " worlds",
        "worlds ",
        "worlds /a",
        "worlds.",
        "a/b.",
        "a/.",
    ] {
        assert!(Resource::parse(text).is_err(), "{text:?} must be rejected");
        assert!(
            ResourceScope::parse(text).is_err(),
            "{text:?} must be rejected as a scope too"
        );
    }
}

#[test]
fn a_resource_rejects_non_ascii_of_any_kind() {
    for text in [
        "ä¸–ç•Œ",
        "ä¸–ç•Œ/ãƒ•ã‚¡ã‚¤ãƒ«",
        "cafÃ©",
        "naÃ¯ve",
        "straÃŸe",
        "Î©",
        "Âµ",
        "Ã—",
        "saves/æ—¥æœ¬/ä¸–ç•Œ",
        // A combining mark and a non-BMP scalar are refused exactly like a
        // precomposed one; there is no normalization step to rescue them.
        "e\u{301}",
        "emoji-\u{1f3ae}",
        "\u{a0}nbsp",
        "\u{202e}rtl",
    ] {
        assert!(!text.is_ascii(), "the sample must actually be non-ASCII");
        assert!(
            Resource::parse(text).is_err(),
            "{text:?} must be rejected as non-ASCII"
        );
        assert!(
            ResourceScope::parse(text).is_err(),
            "{text:?} must be rejected as a scope too"
        );
    }
    // One non-ASCII byte anywhere in the path is enough.
    assert!(Resource::parse("worlds/æ—¥æœ¬").is_err());
}

#[test]
fn a_resource_enforces_the_component_and_path_byte_limits() {
    // Component limit: 255 bytes accepted, 256 refused.
    let at_component_limit = "a".repeat(255);
    assert_eq!(at_component_limit.len(), 255);
    assert_eq!(resource(&at_component_limit).as_str(), at_component_limit);
    assert!(Resource::parse(&"a".repeat(256)).is_err());

    // A `Resource` only has to name the thing being authorized, so its own total bound
    // stays generous and independent of the audit bound that limits a *scope* — as long as
    // every component is inside the 255-byte component limit. 16 x 255 + 15 separators
    // = 4095 bytes is the largest path those component limits allow, so 4096 exactly is
    // unreachable; assert the largest reachable value and that one component more is refused.
    let deep = path_with_components(&[255; 16]);
    assert_eq!(deep.len(), 4_095);
    assert_eq!(resource(&deep).as_str(), deep);
    assert_eq!(resource(&"a".repeat(255)).as_str(), "a".repeat(255));
    assert!(
        Resource::parse(&format!("{deep}/a")).is_err(),
        "{} bytes is over the 4096-byte path bound",
        deep.len() + 2
    );

    // A `ResourceScope` is recorded verbatim in the audit record of every grant carrying it,
    // and `AuditRecord::new` refuses a value over `MAX_METADATA_VALUE_UNITS` UTF-16 units.
    // Scopes are ASCII-only, so bytes and units are the same count; reserving the three
    // bytes of the "/**" subtree suffix is what makes `path/**` fit too.
    let at_bound = path_with_components(&LONGEST_ACCEPTED_PATH);
    assert_eq!(at_bound.len(), MAX_METADATA_VALUE_UNITS - "/**".len());
    assert_eq!(scope(&at_bound).text(), at_bound);
    assert_eq!(
        scope(&format!("{at_bound}/**")).text().len(),
        MAX_METADATA_VALUE_UNITS
    );
    assert!(scope(&format!("{at_bound}/**")).allows(&resource(&at_bound)));

    // Exactly at the audit bound is accepted, with every component still legal, so only
    // the scope text bound can be what refuses the case below.
    let at_exact_bound = format!("{at_bound}/aa");
    assert_eq!(at_exact_bound.len(), MAX_METADATA_VALUE_UNITS);
    assert!(
        at_exact_bound.split('/').all(|part| part.len() <= 255),
        "the at-bound scope must not be refused for a component"
    );
    assert_eq!(scope(&at_exact_bound).text(), at_exact_bound);

    // One unit over the audit bound is refused as a scope...
    let over = format!("{at_exact_bound}a");
    assert_eq!(over.len(), MAX_METADATA_VALUE_UNITS + 1);
    assert!(
        over.split('/').all(|part| part.len() <= 255),
        "the over-bound scope must not be refused for a component"
    );
    assert!(ResourceScope::parse(&over).is_err());
    // ... while the same text is still a perfectly good thing to authorize against.
    assert_eq!(resource(&over).as_str(), over);
}

/// Regression for a real defect found in review: a scope longer than one audit metadata
/// value used to parse successfully, insert its override row, and only then fail the
/// success audit — rolling the grant back with an infrastructure error for an input the
/// caller was entitled to send. Over-long text is now refused at construction, so the
/// failure can never surface as a mysterious rollback and no row is ever written.
#[test]
fn an_unrecordable_scope_is_refused_before_any_row_or_audit_is_attempted() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let target = viewer(&fixture);
    let audit_before = fixture.count("audit_events");

    // Legal components, but more text than one audit metadata value can hold. As a scope
    // this is refused at construction; as a thing to authorize against it is still fine.
    let too_long = (0..40)
        .map(|index| format!("level{index}"))
        .collect::<Vec<_>>()
        .join("/");
    assert!(too_long.len() > MAX_METADATA_VALUE_UNITS);
    let refusal =
        ResourceScope::parse(&too_long).expect_err("an unrecordable scope is not constructible");
    assert_eq!(
        refusal.code(),
        "invalid_request",
        "a typed domain rejection at construction, never a later audit failure"
    );
    assert!(ResourceScope::parse(&format!("{too_long}/**")).is_err());
    assert_eq!(resource(&too_long).as_str(), too_long);

    // The boundary itself is accepted, and delegating it really does commit exactly one
    // override row and one audit record. So the bound above is what refuses the failing
    // case, not some unrelated restriction.
    let at_bound = format!(
        "{}/**",
        resource(&path_with_components(&LONGEST_ACCEPTED_PATH)).as_str()
    );
    assert_eq!(at_bound.len(), MAX_METADATA_VALUE_UNITS);
    let change = fixture
        .identities
        .apply_remote(
            &owner,
            RemoteCommand::SetOverride {
                principal: target.principal.clone(),
                grant: RoleGrant::new(Capability::FilesRead, scope(&at_bound))
                    .expect("a scope at the audit bound is constructible"),
                effect: Effect::Allow,
                expiry: Expiry::Never,
            },
        )
        .expect("owner delegates a scope that fits the audit bound");
    assert!(matches!(change, AuthorityChange::Override(_)));

    assert_eq!(fixture.count("principal_overrides"), 1);
    assert_eq!(fixture.count("audit_events"), audit_before + 1);
    assert!(fixture.audit.verify_chain().expect("audit chain").valid);
    assert_eq!(
        fixture.audit_actions().last().map(String::as_str),
        Some("authority.override.set")
    );
}

#[test]
fn a_resource_rejects_every_win32_device_alias_case_insensitively() {
    for alias in [
        "con", "Con", "CON", "PRN", "prn", "AUX", "aux", "NUL", "nul", "CLOCK$", "clock$",
        "CONIN$", "conin$", "CONOUT$", "conout$", "com1", "COM1", "Com9", "COM9", "lpt1", "LPT1",
        "Lpt9", "LPT9",
    ] {
        assert!(
            Resource::parse(alias).is_err(),
            "{alias:?} is a reserved Win32 device alias"
        );
    }

    // An alias stays an alias behind an extension, at any depth, and behind a
    // trailing dot or space.
    for disguised in [
        "con.txt",
        "NUL.json",
        "COM1.dat",
        "LPT9.log",
        "aux.txt",
        "con.tar.gz",
        "worlds/CON",
        "a/nul/b",
        "CON.",
        "con ",
    ] {
        assert!(
            Resource::parse(disguised).is_err(),
            "{disguised:?} is a device alias or an illegal basename"
        );
    }

    // Space-obscured basenames are refused outright rather than trimmed into an
    // alias the OS might interpret differently.
    for obscured in [" CON", "CON ", "worlds/ CON.txt", "worlds/AUX .txt"] {
        assert!(
            Resource::parse(obscured).is_err(),
            "{obscured:?} must not be trimmed into an alias"
        );
    }

    // The alias list is bounded: only the seven named aliases and COM1-COM9 /
    // LPT1-LPT9. These are deliberately accepted names, not oversights.
    for accepted in [
        "COM0",
        "COM10",
        "LPT0",
        "LPT10",
        "CONX",
        "cons",
        "null",
        "com",
        "lpt",
        "auxiliary",
        "CLOCK",
        "CLOCK$$",
        "NUL1",
        "a.b.con",
    ] {
        assert!(
            Resource::parse(accepted).is_ok(),
            "{accepted:?} is not a reserved Win32 device alias"
        );
    }
}

// ============================ B: ResourceScope::parse form rules and round-trip

#[test]
fn only_a_whole_star_is_all_and_every_other_star_is_refused() {
    assert!(ResourceScope::ALL.is_all());
    assert_eq!(ResourceScope::ALL.text(), "*");
    assert_eq!(scope("*"), ResourceScope::ALL);

    // `*` is a reserved character inside a component, so it is never an ordinary
    // component and never a partial wildcard: it is either the whole string
    // (ALL) or the value is refused.
    for text in [
        "*/**",
        "**",
        "*a",
        "a*",
        "a**",
        "*/*",
        "*/",
        "**/a",
        "a/*/b",
        "worlds/*",
        "worlds/*/nether",
        "a/**/**",
        "worlds//**",
    ] {
        assert!(
            ResourceScope::parse(text).is_err(),
            "{text:?} must not be constructible as a scope"
        );
    }

    // Exact and subtree are the only two other forms, and the difference is only
    // observable through matching: a trailing `/**` is the whole difference.
    assert!(scope("worlds").allows(&resource("worlds")));
    assert!(!scope("worlds").allows(&resource("worlds/x")));
    assert!(scope("worlds/**").allows(&resource("worlds")));
    assert!(scope("worlds/**").allows(&resource("worlds/x")));
    assert!(!scope("worlds").is_all());
    assert!(!scope("worlds/**").is_all());
}

#[test]
fn scope_text_round_trips_and_a_root_path_scope_is_not_constructible() {
    for text in [
        "*",
        "worlds",
        "worlds/**",
        "a/b/c",
        "a/b/c/**",
        "config/options.txt",
        "world-1.2",
        "world-1.2/**",
        "Worlds",
        "file with spaces.txt",
        "..hidden",
        "a..b",
    ] {
        let parsed = scope(text);
        assert_eq!(
            parsed.text(),
            text,
            "an accepted scope must round-trip to exactly the text it was parsed from"
        );
    }

    // The root is expressed as the empty `Resource` or as `*` (ALL). It is never
    // a path scope, so `/**` alone and `*/**` are both unconstructible.
    for text in ["/**", "*/**", "//**", "/**/", "", "/", "a/**/"] {
        assert!(
            ResourceScope::parse(text).is_err(),
            "{text:?} must not name a root path scope"
        );
    }
}

// ======================================= C: segment-boundary matching (core safety)

#[test]
fn a_subtree_scope_matches_only_at_component_boundaries_not_at_a_string_prefix() {
    // Single-segment scope root.
    let worlds = scope("worlds/**");
    for inside in [
        "worlds",               // its own root
        "worlds/nether",        // one component below
        "worlds/nether/region", // several components below
    ] {
        assert!(worlds.allows(&resource(inside)), "{inside:?} is inside");
    }
    for prefix_only in [
        "worlds2",        // NO: the character after the prefix is not a separator
        "worldsX",        // NO
        "worldsX/nether", // NO: the second component cannot be a continuation
        "worldX",         // NO: a shorter prefix is not a descendant
        "worlds.nether",  // NO: a dot is not a separator
        "worlds_backup",  // NO
        "world",          // NO: an ancestor is not inside
        "",               // NO: the root is inside only ALL
    ] {
        assert!(
            !worlds.allows(&resource(prefix_only)),
            "{prefix_only:?} only shares a string prefix with the scope root"
        );
    }

    // Multi-segment scope root: the same rule one component deeper.
    let nether = scope("worlds/nether/**");
    for inside in [
        "worlds/nether",
        "worlds/nether/x",
        "worlds/nether/x/y/z.dat",
    ] {
        assert!(nether.allows(&resource(inside)), "{inside:?} is inside");
    }
    for outside in [
        "worlds",            // the ancestor scope root
        "worlds/nether2",    // NO: prefix of the resource, not a descendant
        "worlds/the_nether", // NO: a different component with the same suffix
        "worlds/netherX",    // NO: the character after the scope root is not a separator
        "nether",            // NO
        "worlds2/nether",    // NO: the first component differs
    ] {
        assert!(
            !nether.allows(&resource(outside)),
            "{outside:?} is outside the subtree"
        );
    }
}

#[test]
fn the_service_applies_the_same_segment_boundary_to_a_stored_subtree_allow() {
    let fixture = Fixture::open();

    let one_segment = viewer(&fixture);
    fixture.stored_override(
        &one_segment,
        Capability::FilesRead,
        "worlds/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot(&fixture, &one_segment);
    for inside in [
        "worlds",
        "worlds/nether",
        "worlds/nether/region",
        "worlds/nether/region/f.dat",
    ] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(inside))),
            Decision::Allow,
            "{inside:?} is inside the granted subtree"
        );
    }
    for prefix_only in [
        "",
        "worlds2",
        "worldsX/nether",
        "worldX",
        "worlds.nether",
        "world",
    ] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(prefix_only))),
            Decision::Deny(DenyReason::NotGranted),
            "{prefix_only:?} only shares a string prefix with the granted root"
        );
    }

    let two_segments = viewer(&fixture);
    fixture.stored_override(
        &two_segments,
        Capability::FilesRead,
        "worlds/nether/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot(&fixture, &two_segments);
    for inside in ["worlds/nether", "worlds/nether/x"] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(inside))),
            Decision::Allow,
            "{inside:?} is inside the granted subtree"
        );
    }
    for outside in [
        "worlds",
        "worlds/nether2",
        "worlds/the_nether",
        "worlds2/nether",
    ] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(outside))),
            Decision::Deny(DenyReason::NotGranted),
            "{outside:?} is outside the granted subtree"
        );
    }
}

// ================== D: asymmetric case sensitivity (deliberate, not a Java quirk)

#[test]
fn allows_are_case_sensitive_and_denies_are_ascii_case_insensitive() {
    let exact = scope("worlds");
    assert!(exact.allows(&resource("worlds")));
    for variant in ["Worlds", "WORLDS", "wOrLdS"] {
        assert!(
            !exact.allows(&resource(variant)),
            "an allow for `worlds` must not allow {variant:?}"
        );
        assert!(
            exact.denies(&resource(variant)),
            "a deny for `worlds` must deny {variant:?}"
        );
    }

    let subtree = scope("worlds/**");
    assert!(subtree.allows(&resource("worlds/nether/x")));
    assert!(!subtree.allows(&resource("Worlds/nether/x")));
    assert!(subtree.denies(&resource("WORLDS/NETHER/X")));

    // The whole asymmetry in one law: a deny is never narrower than the allow it
    // constrains, so case folding can only ever *add* denials. It fails closed.
    for scope_text in PROBE_SCOPES {
        let subject = scope(scope_text);
        for resource_text in PROBE_RESOURCES {
            let target = resource(resource_text);
            if subject.allows(&target) {
                assert!(
                    subject.denies(&target),
                    "scope {scope_text:?} allows {resource_text:?}, so it must also deny it"
                );
            }
        }
    }
}

#[test]
fn the_service_keeps_the_case_asymmetry_between_a_stored_allow_and_a_stored_deny() {
    let fixture = Fixture::open();

    let allowed = viewer(&fixture);
    fixture.stored_override(
        &allowed,
        Capability::FilesRead,
        "worlds/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot(&fixture, &allowed);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds/nether/x"))),
        Decision::Allow
    );
    for variant in ["Worlds/nether/x", "WORLDS/nether/x", "Worlds"] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(variant))),
            Decision::Deny(DenyReason::NotGranted),
            "an allow for `worlds/**` must not allow {variant:?}"
        );
    }

    let denied = viewer(&fixture);
    fixture.stored_override(
        &denied,
        Capability::FilesRead,
        "worlds/**",
        Effect::Deny,
        None,
    );
    let policy = snapshot(&fixture, &denied);
    for variant in ["worlds/nether/x", "Worlds/nether/x", "WORLDS/NETHER/X"] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(variant))),
            Decision::Deny(DenyReason::ExplicitDeny),
            "a deny for `worlds/**` must deny {variant:?}"
        );
    }
    // Case folding does not weaken the segment boundary: a differently cased but
    // still out-of-scope resource is outside the deny as well.
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds2/nether"))),
        Decision::Deny(DenyReason::NotGranted),
        "the deny is still segment bounded"
    );
}

// ===================================================== E: covers and overlaps

#[test]
fn covers_is_single_grant_containment_at_component_boundaries() {
    // ALL covers everything, and only ALL covers ALL.
    for other in [
        "*",
        "worlds",
        "worlds/**",
        "worlds/nether",
        "worlds/nether/**",
        "worlds2",
    ] {
        assert!(
            ResourceScope::ALL.covers(&scope(other)),
            "ALL must cover {other:?}"
        );
    }
    for holder in ["worlds", "worlds/**", "worlds2", "worlds2/**"] {
        assert!(
            !scope(holder).covers(&ResourceScope::ALL),
            "{holder:?} must not cover the whole scope"
        );
    }

    // An exact scope covers only the identical exact scope.
    assert!(scope("worlds").covers(&scope("worlds")));
    assert!(!scope("worlds").covers(&scope("worlds2")));
    assert!(!scope("worlds").covers(&scope("worlds/nether")));
    // An exact scope never covers a subtree, not even one rooted at the same path.
    assert!(!scope("worlds").covers(&scope("worlds/**")));
    assert!(!scope("worlds/nether").covers(&scope("worlds/nether/**")));
    assert!(!scope("config/a.txt").covers(&scope("config/a.txt/**")));

    // A subtree covers its own root plus everything strictly inside it.
    let subtree = scope("worlds/**");
    for inside in [
        "worlds",
        "worlds/nether",
        "worlds/nether/**",
        "worlds/nether/region",
        "worlds/nether/region/**",
    ] {
        assert!(
            subtree.covers(&scope(inside)),
            "worlds/** must cover {inside:?}"
        );
    }
    for outside in [
        "worlds2",
        "worlds2/**",
        "worldsX",
        "worldsX/**",
        "worl",
        "world",
    ] {
        assert!(
            !subtree.covers(&scope(outside)),
            "worlds/** must not cover the sibling {outside:?}"
        );
    }

    // Partial allows are never unioned: two narrow scopes never add up to a wider one.
    assert!(!scope("worlds/a/**").covers(&scope("worlds/**")));
    assert!(!scope("worlds/a/**").covers(&scope("worlds/b")));
    assert!(!scope("worlds/a/**").covers(&scope("worlds/b/**")));
}

#[test]
fn overlaps_is_a_conservative_deny_intersection_at_component_boundaries() {
    // ALL overlaps everything.
    assert!(ResourceScope::ALL.overlaps(&scope("worlds/a/b")));
    assert!(scope("worlds/a/b").overlaps(&ResourceScope::ALL));

    // Containment in either direction overlaps.
    assert!(scope("worlds/**").overlaps(&scope("worlds/a")));
    assert!(scope("worlds/a").overlaps(&scope("worlds/**")));
    assert!(scope("worlds/**").overlaps(&scope("worlds")));
    assert!(scope("worlds/a/**").overlaps(&scope("worlds/a/b/c/**")));

    // Disjoint siblings do not overlap, in either spelling of the root.
    assert!(!scope("worlds/a/**").overlaps(&scope("worlds/b/**")));
    assert!(!scope("worlds/a").overlaps(&scope("worlds/b")));
    assert!(!scope("worlds/a/**").overlaps(&scope("worlds/a2/**")));

    // `worlds` does NOT overlap `worlds2`. Overlap is containment at component
    // boundaries, never a character-prefix test, so this is intended and not a
    // conservative over-approximation: the two scopes really are disjoint.
    assert!(!scope("worlds/**").overlaps(&scope("worlds2")));
    assert!(!scope("worlds").overlaps(&scope("worlds2/**")));
    assert!(!scope("worlds/a/**").overlaps(&scope("worlds2/a/**")));
    assert!(!scope("worlds/nether/**").overlaps(&scope("worlds/nether2")));

    // Overlap is evaluated on the *deny* side of the model, which is ASCII
    // case-insensitive, so case variants DO overlap even where the allow side
    // would not match. That is the intended conservatism: the deny used when
    // deciding delegation is never narrower than the allow it constrains.
    assert!(scope("worlds/**").overlaps(&scope("Worlds")));
    assert!(scope("worlds/a/**").overlaps(&scope("WORLDS/A")));
    assert!(scope("worlds").overlaps(&scope("WORLDS")));
    assert!(!scope("worlds/**").overlaps(&scope("WORLDS2")));
}

#[test]
fn a_set_override_of_a_disjoint_scope_is_refused_as_not_delegable() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let delegate = narrowed_delegate(&fixture, &owner, "Disjoint delegate", "worlds/a/**");
    let target = pack_user(&fixture);

    // `worlds/b/**` does not overlap the delegate's `worlds/a/**`, so the actor
    // does not hold the delegated scope at all.
    let error = set_allow(
        &fixture,
        &delegate,
        &target,
        Capability::FilesRead,
        "worlds/b/**",
    )
    .expect_err("a disjoint scope must not be delegated");
    assert_eq!(error.code(), "forbidden");
    assert_eq!(error.reason(), Some(DenyReason::NotDelegable));
    assert_eq!(
        fixture.count("principal_overrides"),
        0,
        "the refused grant must have stored nothing"
    );
    assert!(
        fixture
            .audit_actions()
            .iter()
            .any(|action| action == "authority.override.set"),
        "the refusal must be audited under the same action"
    );

    // The contained scope is accepted by the same actor, so the refusal is about
    // scope containment and not about the actor's rank or capability.
    set_allow(
        &fixture,
        &delegate,
        &target,
        Capability::FilesRead,
        "worlds/a/**",
    )
    .expect("a contained scope is delegable");
    assert_eq!(fixture.count("principal_overrides"), 1);
}

// ================================================ F: delegation cannot widen

#[test]
fn an_owner_may_grant_a_narrow_subtree_that_then_allows_exactly_that_subtree() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let subject = viewer(&fixture);

    set_allow(
        &fixture,
        &owner,
        &subject,
        Capability::FilesRead,
        "worlds/**",
    )
    .expect("an owner may grant a scoped capability over a subtree");
    assert_eq!(fixture.count("principal_overrides"), 1);

    let policy = snapshot(&fixture, &subject);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds/nether/x"))),
        Decision::Allow
    );
    for outside in ["worlds2/x", "Worlds/x", "other/worlds/x", "worldsx"] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(outside))),
            Decision::Deny(DenyReason::NotGranted),
            "{outside:?} is outside the granted subtree"
        );
    }

    // The grantee may hand on what it holds and nothing beyond it.
    assert!(policy.holds_entirely(Capability::FilesRead, &scope("worlds/nether/**")));
    assert!(!policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));
    assert!(!policy.holds_entirely(Capability::FilesRead, &scope("worlds2/**")));
}

#[test]
fn an_actor_whose_only_allow_is_a_subtree_cannot_delegate_the_whole_scope() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    // This actor holds `roles.assign` at the whole scope, so the delegation check
    // is reached; the only thing it lacks is `files.read` outside `worlds/**`.
    let delegate = narrowed_delegate(&fixture, &owner, "Narrow delegate", "worlds/**");
    let target = pack_user(&fixture);

    // Widening to `*` is refused because the actor must hold the whole scope.
    // It is the *actor's* missing authority, not the target's, that refuses it.
    let error = set_allow(&fixture, &delegate, &target, Capability::FilesRead, "*")
        .expect_err("a subtree holder cannot delegate the whole scope");
    assert_eq!(error.code(), "forbidden");
    assert_eq!(error.reason(), Some(DenyReason::NotDelegable));
    assert_eq!(
        fixture.count("principal_overrides"),
        0,
        "the refused widening must have stored nothing"
    );

    // Not even a sibling subtree is delegable by this actor: `worlds2/**` shares
    // only a string prefix with the held root, so it is not contained in it.
    let error = set_allow(
        &fixture,
        &delegate,
        &target,
        Capability::FilesRead,
        "worlds2/**",
    )
    .expect_err("a sibling subtree is not held");
    assert_eq!(error.reason(), Some(DenyReason::NotDelegable));

    // The exact scope at the delegate's own root IS contained in `worlds/**`, so
    // it is delegable: narrowing is allowed even though widening is not.
    set_allow(
        &fixture,
        &delegate,
        &target,
        Capability::FilesRead,
        "worlds",
    )
    .expect("the exact root is a narrowing of the held subtree");

    // The same actor may delegate a strictly contained scope, which is what makes
    // the refusal a containment rule rather than a blanket ban on delegation.
    set_allow(
        &fixture,
        &delegate,
        &target,
        Capability::FilesRead,
        "worlds/nether/**",
    )
    .expect("a contained scope is still delegable");
    let policy = snapshot(&fixture, &target);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds/nether/x"))),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds/other"))),
        Decision::Deny(DenyReason::NotGranted)
    );
}

#[test]
fn widening_is_refused_for_the_lacking_actor_not_for_the_owner() {
    let fixture = Fixture::open();
    let owner = fixture.owner();
    let viewer_subject = viewer(&fixture);
    let target = pack_user(&fixture);

    set_allow(
        &fixture,
        &owner,
        &viewer_subject,
        Capability::FilesRead,
        "worlds/**",
    )
    .expect("the owner grants a narrow subtree");
    let delegate = narrowed_delegate(&fixture, &owner, "Widening probe", "worlds/**");
    let other_target = pack_user(&fixture);

    // A Viewer holds no `roles.assign` at all, so it fails earlier with
    // `not_granted` and never reaches the containment check.
    let error = set_allow(
        &fixture,
        &viewer_subject,
        &target,
        Capability::FilesRead,
        "worlds/**",
    )
    .expect_err("a viewer cannot delegate anything");
    assert_eq!(error.code(), "forbidden");
    assert_eq!(error.reason(), Some(DenyReason::NotGranted));

    // The Owner holds `files.read` at the whole scope from its built-in role, so
    // the Owner is NOT refused for `*`. Widening is refused for the actor that
    // lacks it (`not_delegable`), which is the only direction that can ever lose
    // authority.
    set_allow(&fixture, &owner, &other_target, Capability::FilesRead, "*")
        .expect("the owner, holding the whole scope, may delegate the whole scope");
    let policy = snapshot(&fixture, &other_target);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("anywhere/at/all"))),
        Decision::Allow
    );
    // The narrowed delegate is refused for the very same request.
    let error = set_allow(&fixture, &delegate, &target, Capability::FilesRead, "*")
        .expect_err("a narrowed delegate may not widen");
    assert_eq!(error.reason(), Some(DenyReason::NotDelegable));
    assert_eq!(
        fixture.count("principal_overrides"),
        2,
        "only the two successful grants are stored"
    );
}

// ========================================= G: stored-scope fail-closed (service)

#[test]
fn an_unreadable_stored_deny_blocks_its_capability_and_is_never_dropped() {
    let fixture = Fixture::open();
    let subject = viewer(&fixture);
    let unreadable = [
        String::new(),
        "../etc".to_owned(),
        "a//b".to_owned(),
        "ä¸–ç•Œ".to_owned(),
        "a".repeat(256),
        "worlds/NUL.json".to_owned(),
    ];
    for stored in &unreadable {
        assert!(
            ResourceScope::parse(stored).is_err(),
            "{stored:?} must be an unreadable stored scope"
        );
        fixture.stored_override(&subject, Capability::FilesRead, stored, Effect::Deny, None);
    }
    assert_eq!(
        fixture.count("principal_overrides"),
        unreadable.len() as i64,
        "every unreadable row is still stored"
    );

    let policy = snapshot(&fixture, &subject);
    for text in ["config/options.txt", "worlds", "anything/at/all", ""] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(text))),
            Decision::Deny(DenyReason::ExplicitDeny),
            "{text:?}: an unreadable deny must never be silently dropped"
        );
    }
    // An unreadable deny is interpreted at the widest possible scope, so it also
    // removes the ability to delegate the capability.
    assert!(!policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));
    assert!(!policy.holds_entirely(Capability::FilesRead, &scope("worlds/nether/**")));

    // The same is true of a global capability: an unreadable deny blocks the whole
    // capability rather than being discarded for want of a resource.
    let global_subject = pack_user(&fixture);
    fixture.stored_override(
        &global_subject,
        Capability::StatusView,
        "worlds/NUL.json",
        Effect::Deny,
        None,
    );
    let policy = snapshot(&fixture, &global_subject);
    assert_eq!(
        policy.check(Capability::StatusView, None),
        Decision::Deny(DenyReason::ExplicitDeny),
        "an unreadable deny of a global capability must block it"
    );
    assert!(!policy.holds_entirely(Capability::StatusView, &ResourceScope::ALL));
}

#[test]
fn an_unreadable_stored_allow_never_grants_anything() {
    let fixture = Fixture::open();
    let subject = viewer(&fixture);
    let unreadable = [
        String::new(),
        "../etc".to_owned(),
        "a//b".to_owned(),
        "ä¸–ç•Œ".to_owned(),
        "a".repeat(256),
        "worlds/NUL.json".to_owned(),
    ];
    for stored in &unreadable {
        fixture.stored_override(&subject, Capability::FilesRead, stored, Effect::Allow, None);
    }
    assert_eq!(
        fixture.count("principal_overrides"),
        unreadable.len() as i64
    );

    let policy = snapshot(&fixture, &subject);
    for text in ["config/options.txt", "worlds", "a", ""] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(text))),
            Decision::Deny(DenyReason::NotGranted),
            "{text:?}: an unreadable allow must never grant"
        );
    }
    assert!(!policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));

    // A later readable row proves the resolution itself is healthy and that the
    // unreadable rows contributed nothing beyond their own absence.
    fixture.stored_override(
        &subject,
        Capability::FilesRead,
        "worlds/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot(&fixture, &subject);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds/nether/x"))),
        Decision::Allow
    );
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds2/nether"))),
        Decision::Deny(DenyReason::NotGranted),
        "an unreadable allow cannot have widened the readable one"
    );
    assert!(!policy.holds_entirely(Capability::FilesRead, &ResourceScope::ALL));
    assert!(!policy.holds_entirely(Capability::FilesRead, &scope("worlds2/**")));
}

// ================================================ H: recorded gap, not a requirement

#[test]
fn recorded_gap_a_non_ascii_resource_scope_cannot_be_represented_at_all() {
    // RECORDED GAP. This test documents a missing capability, not a satisfied
    // requirement. A server owner whose worlds live in a non-ASCII directory has
    // no way to express ANY scope for them in this increment: `Resource::parse`
    // refuses non-ASCII, so neither an exact nor a subtree scope text exists, and
    // `RoleGrant` therefore cannot even be constructed. Closing this needs a
    // canonical form that can represent such a path (normalization or an escaped
    // encoding) with the same segment-boundary guarantees.
    for text in [
        "ä¸–ç•Œ",
        "ä¸–ç•Œ/ãƒ•ã‚¡ã‚¤ãƒ«",
        "ä¸–ç•Œ/**",
        "saves/ä¸–ç•Œ",
        "saves/ä¸–ç•Œ/**",
    ] {
        assert!(
            Resource::parse(text).is_err(),
            "{text:?} has no canonical Resource value"
        );
        assert!(
            ResourceScope::parse(text).is_err(),
            "{text:?} has no canonical ResourceScope value"
        );
        // An unusable scope cannot become a grant either.
        assert!(
            ResourceScope::parse(text)
                .and_then(|parsed| RoleGrant::new(Capability::FilesRead, parsed))
                .is_err(),
            "RoleGrant cannot carry {text:?}"
        );
    }

    // The only observable effect of such a path in the stored state is a
    // restriction, which is safe but is not the feature the owner needs. The
    // intended resource `ä¸–ç•Œ` cannot even be named in a check, because there is
    // no `Resource` value for it: the caller cannot ask the question at all.
    let fixture = Fixture::open();
    let allowed = viewer(&fixture);
    fixture.stored_override(
        &allowed,
        Capability::FilesRead,
        "ä¸–ç•Œ/**",
        Effect::Allow,
        None,
    );
    let policy = snapshot(&fixture, &allowed);
    assert!(
        Resource::parse("ä¸–ç•Œ").is_err(),
        "the resource the allow was meant to cover cannot be represented"
    );
    for text in ["worlds/a", "worlds", ""] {
        assert_eq!(
            policy.check(Capability::FilesRead, Some(&resource(text))),
            Decision::Deny(DenyReason::NotGranted),
            "{text:?}: the intended allow is unavailable, so nothing is granted"
        );
    }

    let denied = viewer(&fixture);
    fixture.stored_override(
        &denied,
        Capability::FilesRead,
        "ä¸–ç•Œ/**",
        Effect::Deny,
        None,
    );
    let policy = snapshot(&fixture, &denied);
    assert_eq!(
        policy.check(Capability::FilesRead, Some(&resource("worlds/a"))),
        Decision::Deny(DenyReason::ExplicitDeny),
        "a non-ASCII deny still fails closed"
    );
}
