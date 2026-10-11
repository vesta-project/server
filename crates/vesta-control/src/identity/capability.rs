/// Exact server-local actions that may be granted or denied by identity policy.
///
/// These identifiers are persisted. Keep them stable and do not introduce wildcard or grouped
/// capabilities: adapters authorize the exact action they perform.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    // Status and observation.
    StatusView,
    StatusDetailed,
    MetricsView,
    PlayersView,
    ActivityView,
    TasksView,
    DiagnosticsView,
    LogsRead,
    ConsoleRead,
    ConsoleWrite,
    PlayersModerate,

    // Lifecycle.
    LifecycleStart,
    LifecycleStop,
    LifecycleRestart,
    LifecycleForceKill,
    MaintenanceToggle,

    // Files and content.
    FilesList,
    FilesRead,
    FilesCreate,
    FilesOverwrite,
    FilesRename,
    FilesMkdir,
    FilesDelete,
    ContentInstall,

    // Backups.
    BackupsView,
    BackupsCreate,
    BackupsVerify,
    BackupsRestore,
    RecoveryExport,

    // Schedules and configuration.
    SchedulesView,
    SchedulesEdit,
    SchedulesRun,
    ConfigView,
    ConfigEdit,
    IntegrationsEdit,

    // Pack.
    PackAccess,
    PackEdit,
    PackBuild,
    PackPublish,
    PackForceUpdate,

    // Identity and delegation.
    PrincipalsView,
    PrincipalsManage,
    DevicesRevoke,
    RolesCreate,
    RolesAssign,
    RolesRevoke,
    CredentialsManage,
    SecurityManage,
    AuditView,

    // Local-only hard invariants.
    OwnerRecovery,
    AgentInstall,
    // Unified public/pack permissions append to the legacy capability registry.
    PublicInfo,
    PackDownload,
    PackRealtime,
    RolesManage,
    AccessSourcesManage,
    InvitationsCreate,
}

impl Capability {
    /// Stable legacy registry order followed by the unified-access additions.
    pub const ALL: &'static [Self] = &[
        Self::StatusView,
        Self::StatusDetailed,
        Self::MetricsView,
        Self::PlayersView,
        Self::ActivityView,
        Self::TasksView,
        Self::DiagnosticsView,
        Self::LogsRead,
        Self::ConsoleRead,
        Self::ConsoleWrite,
        Self::PlayersModerate,
        Self::LifecycleStart,
        Self::LifecycleStop,
        Self::LifecycleRestart,
        Self::LifecycleForceKill,
        Self::MaintenanceToggle,
        Self::FilesList,
        Self::FilesRead,
        Self::FilesCreate,
        Self::FilesOverwrite,
        Self::FilesRename,
        Self::FilesMkdir,
        Self::FilesDelete,
        Self::ContentInstall,
        Self::BackupsView,
        Self::BackupsCreate,
        Self::BackupsVerify,
        Self::BackupsRestore,
        Self::RecoveryExport,
        Self::SchedulesView,
        Self::SchedulesEdit,
        Self::SchedulesRun,
        Self::ConfigView,
        Self::ConfigEdit,
        Self::IntegrationsEdit,
        Self::PackAccess,
        Self::PackEdit,
        Self::PackBuild,
        Self::PackPublish,
        Self::PackForceUpdate,
        Self::PrincipalsView,
        Self::PrincipalsManage,
        Self::DevicesRevoke,
        Self::RolesCreate,
        Self::RolesAssign,
        Self::RolesRevoke,
        Self::CredentialsManage,
        Self::SecurityManage,
        Self::AuditView,
        Self::OwnerRecovery,
        Self::AgentInstall,
        Self::PublicInfo,
        Self::PackDownload,
        Self::PackRealtime,
        Self::RolesManage,
        Self::AccessSourcesManage,
        Self::InvitationsCreate,
    ];

    /// The stable, exact identifier used in persisted identity records.
    pub const fn id(self) -> &'static str {
        match self {
            Self::PublicInfo => "public.info",
            Self::PackDownload => "pack.download",
            Self::PackRealtime => "pack.realtime",
            Self::RolesManage => "roles.manage",
            Self::AccessSourcesManage => "access-sources.manage",
            Self::InvitationsCreate => "invitations.create",
            Self::StatusView => "status.view",
            Self::StatusDetailed => "status.detailed",
            Self::MetricsView => "metrics.view",
            Self::PlayersView => "players.view",
            Self::ActivityView => "activity.view",
            Self::TasksView => "tasks.view",
            Self::DiagnosticsView => "diagnostics.view",
            Self::LogsRead => "logs.read",
            Self::ConsoleRead => "console.read",
            Self::ConsoleWrite => "console.write",
            Self::PlayersModerate => "players.moderate",
            Self::LifecycleStart => "lifecycle.start",
            Self::LifecycleStop => "lifecycle.stop",
            Self::LifecycleRestart => "lifecycle.restart",
            Self::LifecycleForceKill => "lifecycle.force_kill",
            Self::MaintenanceToggle => "maintenance.toggle",
            Self::FilesList => "files.list",
            Self::FilesRead => "files.read",
            Self::FilesCreate => "files.create",
            Self::FilesOverwrite => "files.overwrite",
            Self::FilesRename => "files.rename",
            Self::FilesMkdir => "files.mkdir",
            Self::FilesDelete => "files.delete",
            Self::ContentInstall => "content.install",
            Self::BackupsView => "backups.view",
            Self::BackupsCreate => "backups.create",
            Self::BackupsVerify => "backups.verify",
            Self::BackupsRestore => "backups.restore",
            Self::RecoveryExport => "recovery.export",
            Self::SchedulesView => "schedules.view",
            Self::SchedulesEdit => "schedules.edit",
            Self::SchedulesRun => "schedules.run",
            Self::ConfigView => "config.view",
            Self::ConfigEdit => "config.edit",
            Self::IntegrationsEdit => "integrations.edit",
            Self::PackAccess => "pack.access",
            Self::PackEdit => "pack.edit",
            Self::PackBuild => "pack.build",
            Self::PackPublish => "pack.publish",
            Self::PackForceUpdate => "pack.force_update",
            Self::PrincipalsView => "principals.view",
            Self::PrincipalsManage => "principals.manage",
            Self::DevicesRevoke => "devices.revoke",
            Self::RolesCreate => "roles.create",
            Self::RolesAssign => "roles.assign",
            Self::RolesRevoke => "roles.revoke",
            Self::CredentialsManage => "credentials.manage",
            Self::SecurityManage => "security.manage",
            Self::AuditView => "audit.view",
            Self::OwnerRecovery => "owner.recovery",
            Self::AgentInstall => "agent.install",
        }
    }

    /// Resolve only known persisted identifiers; unknown identifiers remain absent.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "public.info" => Some(Self::PublicInfo),
            "pack.download" => Some(Self::PackDownload),
            "pack.realtime" => Some(Self::PackRealtime),
            "roles.manage" => Some(Self::RolesManage),
            "access-sources.manage" => Some(Self::AccessSourcesManage),
            "invitations.create" => Some(Self::InvitationsCreate),
            "status.view" => Some(Self::StatusView),
            "status.detailed" => Some(Self::StatusDetailed),
            "metrics.view" => Some(Self::MetricsView),
            "players.view" => Some(Self::PlayersView),
            "activity.view" => Some(Self::ActivityView),
            "tasks.view" => Some(Self::TasksView),
            "diagnostics.view" => Some(Self::DiagnosticsView),
            "logs.read" => Some(Self::LogsRead),
            "console.read" => Some(Self::ConsoleRead),
            "console.write" => Some(Self::ConsoleWrite),
            "players.moderate" => Some(Self::PlayersModerate),
            "lifecycle.start" => Some(Self::LifecycleStart),
            "lifecycle.stop" => Some(Self::LifecycleStop),
            "lifecycle.restart" => Some(Self::LifecycleRestart),
            "lifecycle.force_kill" => Some(Self::LifecycleForceKill),
            "maintenance.toggle" => Some(Self::MaintenanceToggle),
            "files.list" => Some(Self::FilesList),
            "files.read" => Some(Self::FilesRead),
            "files.create" => Some(Self::FilesCreate),
            "files.overwrite" => Some(Self::FilesOverwrite),
            "files.rename" => Some(Self::FilesRename),
            "files.mkdir" => Some(Self::FilesMkdir),
            "files.delete" => Some(Self::FilesDelete),
            "content.install" => Some(Self::ContentInstall),
            "backups.view" => Some(Self::BackupsView),
            "backups.create" => Some(Self::BackupsCreate),
            "backups.verify" => Some(Self::BackupsVerify),
            "backups.restore" => Some(Self::BackupsRestore),
            "recovery.export" => Some(Self::RecoveryExport),
            "schedules.view" => Some(Self::SchedulesView),
            "schedules.edit" => Some(Self::SchedulesEdit),
            "schedules.run" => Some(Self::SchedulesRun),
            "config.view" => Some(Self::ConfigView),
            "config.edit" => Some(Self::ConfigEdit),
            "integrations.edit" => Some(Self::IntegrationsEdit),
            "pack.access" => Some(Self::PackAccess),
            "pack.edit" => Some(Self::PackEdit),
            "pack.build" => Some(Self::PackBuild),
            "pack.publish" => Some(Self::PackPublish),
            "pack.force_update" => Some(Self::PackForceUpdate),
            "principals.view" => Some(Self::PrincipalsView),
            "principals.manage" => Some(Self::PrincipalsManage),
            "devices.revoke" => Some(Self::DevicesRevoke),
            "roles.create" => Some(Self::RolesCreate),
            "roles.assign" => Some(Self::RolesAssign),
            "roles.revoke" => Some(Self::RolesRevoke),
            "credentials.manage" => Some(Self::CredentialsManage),
            "security.manage" => Some(Self::SecurityManage),
            "audit.view" => Some(Self::AuditView),
            "owner.recovery" => Some(Self::OwnerRecovery),
            "agent.install" => Some(Self::AgentInstall),
            _ => None,
        }
    }

    pub const fn scoped(self) -> bool {
        matches!(
            self,
            Self::LogsRead
                | Self::FilesList
                | Self::FilesRead
                | Self::FilesCreate
                | Self::FilesOverwrite
                | Self::FilesRename
                | Self::FilesMkdir
                | Self::FilesDelete
        )
    }

    pub const fn local_only(self) -> bool {
        matches!(self, Self::OwnerRecovery | Self::AgentInstall)
    }

    pub const fn owner_reserved(self) -> bool {
        matches!(self, Self::BackupsRestore | Self::RecoveryExport)
    }

    pub const fn admin_reserved(self) -> bool {
        matches!(self, Self::CredentialsManage | Self::SecurityManage)
    }

    /// Whether remote delegation may carry this capability.
    pub const fn delegable(self) -> bool {
        !self.local_only() && !self.owner_reserved() && !self.admin_reserved()
    }
}
