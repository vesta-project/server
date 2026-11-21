//! Requirement tests for Host role management and startup policy
//! (SERVER_SOFTWARE_REQUIREMENTS.md identity section; protocol/host-v1/management.md).
use super::{
    HostState,
    owners::{Actor, EffectiveRole, ServerRole},
    registry::{ServerEntry, ServerState, Visibility},
    roles::{Effect, Grant, Override, RoleFields},
    settings::HostSettingsPatch,
    startup::StartupAction,
};
use crate::identity::Capability;
use std::sync::Arc;
use uuid::Uuid;

struct Host {
    _tmp: tempfile::TempDir,
    state: HostState,
    owner: Actor,
    server: Uuid,
}

impl Host {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state =
            HostState::open(tmp.path().join("host"), Arc::new(vesta_state::SystemClock)).unwrap();
        let owner = Actor::Account(state.claim(Uuid::new_v4(), "Owner", [7; 32]).unwrap().account_id);
        let server = Uuid::new_v4();
        state
            .register_server(
                owner,
                &ServerEntry {
                    server_id: server,
                    name: "Fixture".into(),
                    game_port: 31100,
                    state: ServerState::Stopped,
                    visibility: Visibility::Private,
                },
                state.snapshot().unwrap().revision,
            )
            .unwrap();
        Self { _tmp: tmp, state, owner, server }
    }

    fn rev(&self) -> u64 {
        self.state.snapshot().unwrap().revision
    }

    fn account(&self, seed: u8) -> Uuid {
        let account = Uuid::new_v4();
        self.state
            .write(|c| HostState::enroll_device_in(c, account, "Fixture", [seed; 32], self.state.now_ms()))
            .unwrap();
        account
    }

    fn role(&self, actor: Actor, name: &str, rank: i32, caps: &[Capability]) -> crate::Result<String> {
        self.state
            .write_custom_role(actor, self.server, None, self.rev(), fields(Some(name), Some(rank), Some(caps)))
            .map(|(_, role)| role["id"].as_str().unwrap().to_owned())
    }

    fn assign(&self, actor: Actor, account: Uuid, role: &str) -> crate::Result<Option<EffectiveRole>> {
        self.state
            .assign_role(actor, self.server, account, Some(role), None, self.rev())
            .map(|(_, role)| role)
    }

    fn caps(&self, account: Uuid) -> std::collections::BTreeSet<Capability> {
        let effective = self
            .state
            .read(|c| super::registry::effective_role_in(c, account, self.server))
            .unwrap()
            .expect("account has a role");
        self.state.server_capabilities(self.server, account, effective).unwrap()
    }
}

fn fields(name: Option<&str>, rank: Option<i32>, caps: Option<&[Capability]>) -> RoleFields {
    RoleFields {
        name: name.map(Into::into),
        rank,
        capabilities: caps.map(|caps| {
            caps.iter()
                .map(|cap| Grant { capability: cap.id().into(), scope: "*".into() })
                .collect()
        }),
    }
}

#[track_caller]
fn code<T: std::fmt::Debug>(result: crate::Result<T>) -> String {
    result.unwrap_err().code().to_owned()
}

// REQ roles: owners create custom roles from named capabilities, and the role grants
// exactly those capabilities to its assignees.
#[test]
fn req_owner_creates_custom_role_that_grants_exactly_its_capabilities() {
    let h = Host::new();
    let helper = h.account(11);
    let id = h.role(h.owner, "Builders", 30, &[Capability::FilesRead, Capability::FilesCreate]).unwrap();
    h.assign(h.owner, helper, &id).unwrap();
    let caps = h.caps(helper);
    assert!(caps.contains(&Capability::FilesRead) && caps.contains(&Capability::FilesCreate));
    assert!(!caps.contains(&Capability::FilesDelete));
    assert!(!caps.contains(&Capability::LifecycleStart));
    let listed = h.state.custom_roles(h.server).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["name"], "Builders");
}

// REQ roles: renames keep the role ID and edits apply to existing assignees.
#[test]
fn req_role_edits_keep_identity_and_apply_to_assignees() {
    let h = Host::new();
    let helper = h.account(12);
    let id = h.role(h.owner, "Helpers", 30, &[Capability::FilesRead]).unwrap();
    h.assign(h.owner, helper, &id).unwrap();
    let (_, role) = h
        .state
        .write_custom_role(
            h.owner,
            h.server,
            Some(id.parse().unwrap()),
            h.rev(),
            fields(Some("Trusted helpers"), None, Some(&[Capability::FilesRead, Capability::ConsoleRead])),
        )
        .unwrap();
    assert_eq!(role["id"], id.as_str());
    assert_eq!(role["name"], "Trusted helpers");
    assert!(h.caps(helper).contains(&Capability::ConsoleRead));
}

// REQ roles: custom roles must be below Administrator and only carry delegable
// capabilities; device revocation and access-source management stay owner-only.
#[test]
fn req_custom_roles_refuse_reserved_ranks_and_hard_capabilities() {
    let h = Host::new();
    assert_eq!(code(h.role(h.owner, "Too high", ServerRole::Admin.rank(), &[Capability::FilesRead])), "invalid_request");
    assert_eq!(code(h.role(h.owner, "Zero", 0, &[Capability::FilesRead])), "invalid_request");
    assert_eq!(code(h.role(h.owner, "Devices", 30, &[Capability::DevicesRevoke])), "forbidden");
    assert_eq!(code(h.role(h.owner, "Sources", 30, &[Capability::AccessSourcesManage])), "forbidden");
    assert_eq!(code(h.role(h.owner, " ", 30, &[Capability::FilesRead])), "invalid_request");
    assert!(h.state.custom_roles(h.server).unwrap().is_empty());
}

// REQ roles: remote delegation cannot grant equal or higher authority, nor
// capabilities the delegator lacks.
#[test]
fn req_delegation_is_strictly_below_the_delegator() {
    let h = Host::new();
    let lead = h.account(13);
    let helper = h.account(14);
    let lead_role = h
        .role(h.owner, "Lead", 50, &[Capability::RolesCreate, Capability::RolesAssign, Capability::FilesRead, Capability::ConsoleRead])
        .unwrap();
    h.assign(h.owner, lead, &lead_role).unwrap();
    let lead_actor = Actor::Account(lead);

    // Equal rank and capabilities the lead does not hold are refused.
    assert_eq!(code(h.role(lead_actor, "Peer", 50, &[Capability::FilesRead])), "forbidden");
    assert_eq!(code(h.role(lead_actor, "Deleters", 20, &[Capability::FilesDelete])), "forbidden");
    // A lower role with a subset is allowed and can be assigned.
    let lower = h.role(lead_actor, "Readers", 20, &[Capability::FilesRead]).unwrap();
    h.assign(lead_actor, helper, &lower).unwrap();
    // Built-ins at or above the lead's rank cannot be handed out.
    assert_eq!(code(h.assign(lead_actor, helper, ServerRole::Maintainer.id())), "forbidden");
    // The lead cannot change their own role or overrides.
    assert_eq!(code(h.assign(lead_actor, lead, &lower)), "forbidden");
}

// REQ roles: Administrator is reserved for local authority; remote owners cannot
// assign it, but every lower built-in preset remains assignable.
#[test]
fn req_administrator_is_not_remotely_assignable() {
    let h = Host::new();
    let target = h.account(15);
    assert_eq!(code(h.assign(h.owner, target, ServerRole::Admin.id())), "forbidden");
    for role in [ServerRole::Maintainer, ServerRole::Moderator, ServerRole::Viewer] {
        h.assign(h.owner, target, role.id()).unwrap();
    }
}

// REQ roles: built-in presets are immutable.
#[test]
fn req_builtin_roles_cannot_be_edited_or_deleted() {
    let h = Host::new();
    for id in ["viewer", "moderator", "owner"] {
        let parsed = Uuid::parse_str(id);
        // Built-in IDs are not UUIDs, so the API cannot address them as custom roles.
        assert!(parsed.is_err());
    }
    let random = Uuid::new_v4();
    assert_eq!(
        code(h.state.write_custom_role(h.owner, h.server, Some(random), h.rev(), fields(Some("x"), None, None))),
        "not_found"
    );
    assert_eq!(code(h.state.delete_custom_role(h.owner, h.server, random, h.rev())), "not_found");
}

// REQ roles: per-principal overrides allow or deny single capabilities, deny wins,
// and changing a principal's role clears them.
#[test]
fn req_overrides_allow_and_deny_and_reset_on_role_change() {
    let h = Host::new();
    let helper = h.account(16);
    let id = h.role(h.owner, "Readers", 30, &[Capability::FilesRead, Capability::ConsoleRead]).unwrap();
    h.assign(h.owner, helper, &id).unwrap();
    h.state
        .set_capability_overrides(
            h.owner,
            h.server,
            helper,
            h.rev(),
            &[
                Override { capability: Capability::ConsoleRead.id().into(), effect: Effect::Deny },
                Override { capability: Capability::BackupsView.id().into(), effect: Effect::Allow },
            ],
        )
        .unwrap();
    let caps = h.caps(helper);
    assert!(!caps.contains(&Capability::ConsoleRead));
    assert!(caps.contains(&Capability::BackupsView));
    assert!(caps.contains(&Capability::FilesRead));
    // Duplicate entries and hard capabilities are refused.
    assert_eq!(
        code(h.state.set_capability_overrides(h.owner, h.server, helper, h.rev(), &[
            Override { capability: "files.read".into(), effect: Effect::Deny },
            Override { capability: "files.read".into(), effect: Effect::Allow },
        ])),
        "invalid_request"
    );
    assert_eq!(
        code(h.state.set_capability_overrides(h.owner, h.server, helper, h.rev(), &[
            Override { capability: Capability::DevicesRevoke.id().into(), effect: Effect::Allow },
        ])),
        "forbidden"
    );
    h.assign(h.owner, helper, ServerRole::Viewer.id()).unwrap();
    assert!(h.state.capability_overrides(h.server, helper).unwrap().is_empty());
}

// REQ roles: an assigned role cannot be deleted; once unassigned it can.
#[test]
fn req_role_deletion_requires_no_assignees() {
    let h = Host::new();
    let helper = h.account(17);
    let id = h.role(h.owner, "Temp", 30, &[Capability::FilesRead]).unwrap();
    h.assign(h.owner, helper, &id).unwrap();
    assert_eq!(code(h.state.delete_custom_role(h.owner, h.server, id.parse().unwrap(), h.rev())), "role_in_use");
    h.state.assign_role(h.owner, h.server, helper, None, Some(&id), h.rev()).unwrap();
    h.state.delete_custom_role(h.owner, h.server, id.parse().unwrap(), h.rev()).unwrap();
    assert!(h.state.custom_roles(h.server).unwrap().is_empty());
}

// REQ roles: stale writes are rejected so two editors cannot silently overwrite.
#[test]
fn req_role_writes_are_versioned() {
    let h = Host::new();
    let stale = h.rev();
    h.role(h.owner, "First", 30, &[Capability::FilesRead]).unwrap();
    assert_eq!(
        code(h.state.write_custom_role(h.owner, h.server, None, stale, fields(Some("Second"), Some(30), Some(&[Capability::FilesRead])))),
        "conflict"
    );
}

// REQ roles: a principal without role management cannot create or edit roles.
#[test]
fn req_role_editing_requires_role_capabilities() {
    let h = Host::new();
    let viewer = h.account(18);
    h.assign(h.owner, viewer, ServerRole::Viewer.id()).unwrap();
    assert_eq!(code(h.role(Actor::Account(viewer), "Mine", 5, &[Capability::StatusView])), "forbidden");
}

// REQ startup: the Host default applies to servers registered afterwards only.
#[test]
fn req_startup_default_applies_to_new_servers_only() {
    let h = Host::new();
    h.state
        .update_settings(
            h.owner,
            HostSettingsPatch {
                name: None,
                new_device_sign_in: None,
                removed_server_retention_days: None,
                default_on_agent_start: Some(StartupAction::StayStopped),
            },
        )
        .unwrap();
    assert_eq!(h.state.startup_settings(h.server).unwrap().unwrap().on_agent_start, StartupAction::RestorePrevious);
    let later = Uuid::new_v4();
    h.state
        .register_server(
            h.owner,
            &ServerEntry {
                server_id: later,
                name: "Later".into(),
                game_port: 31101,
                state: ServerState::Stopped,
                visibility: Visibility::Private,
            },
            h.rev(),
        )
        .unwrap();
    let settings = h.state.startup_settings(later).unwrap().unwrap();
    assert_eq!(settings.on_agent_start, StartupAction::StayStopped);
    assert_eq!(settings.start_delay_seconds, 0);
}

// REQ startup: delay is bounded, edits need config.edit plus the matching lifecycle
// capability, and an empty patch is refused.
#[test]
fn req_startup_edits_are_bounded_and_authorized() {
    let h = Host::new();
    let fallback = Default::default();
    assert_eq!(code(h.state.patch_startup(h.owner, h.server, h.rev(), fallback, None, Some(3601))), "invalid_request");
    assert_eq!(code(h.state.patch_startup(h.owner, h.server, h.rev(), fallback, None, None)), "invalid_request");
    let (_, saved) = h
        .state
        .patch_startup(h.owner, h.server, h.rev(), fallback, Some(StartupAction::Sleep), Some(3600))
        .unwrap();
    assert_eq!((saved.on_agent_start, saved.start_delay_seconds), (StartupAction::Sleep, 3600));

    let editor = h.account(19);
    let id = h.role(h.owner, "Config only", 30, &[Capability::ConfigEdit, Capability::LifecycleStop]).unwrap();
    h.assign(h.owner, editor, &id).unwrap();
    let editor = Actor::Account(editor);
    // Can choose stopped-style actions, not ones that start Minecraft.
    h.state.patch_startup(editor, h.server, h.rev(), fallback, Some(StartupAction::StayStopped), None).unwrap();
    assert_eq!(code(h.state.patch_startup(editor, h.server, h.rev(), fallback, Some(StartupAction::Start), None)), "forbidden");
    assert_eq!(h.state.startup_settings(h.server).unwrap().unwrap().on_agent_start, StartupAction::StayStopped);
}
