use super::capability::Capability;

const OWNER_CAPABILITIES: &[Capability] = &[
    Capability::StatusView,
    Capability::StatusDetailed,
    Capability::MetricsView,
    Capability::PlayersView,
    Capability::ActivityView,
    Capability::TasksView,
    Capability::DiagnosticsView,
    Capability::LogsRead,
    Capability::ConsoleRead,
    Capability::ConsoleWrite,
    Capability::PlayersModerate,
    Capability::LifecycleStart,
    Capability::LifecycleStop,
    Capability::LifecycleRestart,
    Capability::LifecycleForceKill,
    Capability::MaintenanceToggle,
    Capability::FilesList,
    Capability::FilesRead,
    Capability::FilesCreate,
    Capability::FilesOverwrite,
    Capability::FilesRename,
    Capability::FilesMkdir,
    Capability::FilesDelete,
    Capability::ContentInstall,
    Capability::BackupsView,
    Capability::BackupsCreate,
    Capability::BackupsVerify,
    Capability::BackupsRestore,
    Capability::RecoveryExport,
    Capability::SchedulesView,
    Capability::SchedulesEdit,
    Capability::SchedulesRun,
    Capability::ConfigView,
    Capability::ConfigEdit,
    Capability::IntegrationsEdit,
    Capability::PublicInfo,
    Capability::PackDownload,
    Capability::PackRealtime,
    Capability::PackAccess,
    Capability::PackEdit,
    Capability::PackBuild,
    Capability::PackPublish,
    Capability::PackForceUpdate,
    Capability::RolesManage,
    Capability::AccessSourcesManage,
    Capability::InvitationsCreate,
    Capability::AutomationView,
    Capability::AutomationManage,
    Capability::NotificationsView,
    Capability::BackupsConfigure,
    Capability::PrincipalsView,
    Capability::PrincipalsManage,
    Capability::DevicesRevoke,
    Capability::RolesCreate,
    Capability::RolesAssign,
    Capability::RolesRevoke,
    Capability::CredentialsManage,
    Capability::SecurityManage,
    Capability::AuditView,
];

const ADMINISTRATOR_CAPABILITIES: &[Capability] = &[
    Capability::StatusView,
    Capability::StatusDetailed,
    Capability::MetricsView,
    Capability::PlayersView,
    Capability::ActivityView,
    Capability::TasksView,
    Capability::DiagnosticsView,
    Capability::LogsRead,
    Capability::ConsoleRead,
    Capability::ConsoleWrite,
    Capability::PlayersModerate,
    Capability::LifecycleStart,
    Capability::LifecycleStop,
    Capability::LifecycleRestart,
    Capability::LifecycleForceKill,
    Capability::MaintenanceToggle,
    Capability::FilesList,
    Capability::FilesRead,
    Capability::FilesCreate,
    Capability::FilesOverwrite,
    Capability::FilesRename,
    Capability::FilesMkdir,
    Capability::FilesDelete,
    Capability::ContentInstall,
    Capability::BackupsView,
    Capability::BackupsCreate,
    Capability::BackupsVerify,
    Capability::SchedulesView,
    Capability::SchedulesEdit,
    Capability::SchedulesRun,
    Capability::ConfigView,
    Capability::ConfigEdit,
    Capability::IntegrationsEdit,
    Capability::PublicInfo,
    Capability::PackDownload,
    Capability::PackRealtime,
    Capability::PackAccess,
    Capability::PackEdit,
    Capability::PackBuild,
    Capability::PackPublish,
    Capability::PackForceUpdate,
    Capability::RolesManage,
    Capability::AccessSourcesManage,
    Capability::InvitationsCreate,
    Capability::AutomationView,
    Capability::AutomationManage,
    Capability::NotificationsView,
    Capability::PrincipalsView,
    Capability::PrincipalsManage,
    Capability::DevicesRevoke,
    Capability::RolesCreate,
    Capability::RolesAssign,
    Capability::RolesRevoke,
    Capability::CredentialsManage,
    Capability::SecurityManage,
    Capability::AuditView,
];

const MAINTAINER_CAPABILITIES: &[Capability] = &[
    Capability::StatusView,
    Capability::StatusDetailed,
    Capability::MetricsView,
    Capability::PlayersView,
    Capability::ActivityView,
    Capability::TasksView,
    Capability::DiagnosticsView,
    Capability::LogsRead,
    Capability::ConsoleRead,
    Capability::ConsoleWrite,
    Capability::LifecycleStart,
    Capability::LifecycleStop,
    Capability::LifecycleRestart,
    Capability::LifecycleForceKill,
    Capability::MaintenanceToggle,
    Capability::FilesList,
    Capability::FilesRead,
    Capability::FilesCreate,
    Capability::FilesOverwrite,
    Capability::FilesRename,
    Capability::FilesMkdir,
    Capability::FilesDelete,
    Capability::ContentInstall,
    Capability::BackupsView,
    Capability::BackupsCreate,
    Capability::BackupsVerify,
    Capability::SchedulesView,
    Capability::SchedulesEdit,
    Capability::SchedulesRun,
];

const MODERATOR_CAPABILITIES: &[Capability] = &[
    Capability::NotificationsView,
    Capability::StatusView,
    Capability::PlayersView,
    Capability::PlayersModerate,
];

const VIEWER_CAPABILITIES: &[Capability] = &[
    Capability::StatusView,
    Capability::StatusDetailed,
    Capability::MetricsView,
    Capability::PlayersView,
    Capability::ActivityView,
    Capability::TasksView,
];

const PACK_USER_CAPABILITIES: &[Capability] = &[
    Capability::StatusView,
    Capability::PublicInfo,
    Capability::PackDownload,
    Capability::PackRealtime,
    Capability::PackAccess,
];

/// The six versioned built-in role presets.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BuiltinRole {
    Owner,
    Administrator,
    Maintainer,
    Moderator,
    Viewer,
    PackUser,
}

impl BuiltinRole {
    pub const ALL: &'static [Self] = &[
        Self::Owner,
        Self::Administrator,
        Self::Maintainer,
        Self::Moderator,
        Self::Viewer,
        Self::PackUser,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Administrator => "administrator",
            Self::Maintainer => "maintainer",
            Self::Moderator => "moderator",
            Self::Viewer => "viewer",
            Self::PackUser => "pack-user",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Owner => "Owner",
            Self::Administrator => "Administrator",
            Self::Maintainer => "Maintainer",
            Self::Moderator => "Moderator",
            Self::Viewer => "Viewer",
            Self::PackUser => "Pack User",
        }
    }

    pub const fn rank(self) -> i32 {
        match self {
            Self::Owner => 100,
            Self::Administrator => 80,
            Self::Maintainer => 60,
            Self::Moderator => 40,
            Self::Viewer => 20,
            Self::PackUser => 10,
        }
    }

    pub const fn capabilities(self) -> &'static [Capability] {
        match self {
            Self::Owner => OWNER_CAPABILITIES,
            Self::Administrator => ADMINISTRATOR_CAPABILITIES,
            Self::Maintainer => MAINTAINER_CAPABILITIES,
            Self::Moderator => MODERATOR_CAPABILITIES,
            Self::Viewer => VIEWER_CAPABILITIES,
            Self::PackUser => PACK_USER_CAPABILITIES,
        }
    }

    pub const fn local_grant_only(self) -> bool {
        matches!(self, Self::Owner | Self::Administrator)
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "owner" => Some(Self::Owner),
            "administrator" => Some(Self::Administrator),
            "maintainer" => Some(Self::Maintainer),
            "moderator" => Some(Self::Moderator),
            "viewer" => Some(Self::Viewer),
            "pack-user" => Some(Self::PackUser),
            _ => None,
        }
    }
}
