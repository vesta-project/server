use serde_json::{Value, json};
use vesta_control::{identity::Capability, machine::roles::host_delegable};

pub(super) fn capability_catalog() -> Value {
    json!({"capabilities":Capability::ALL.iter().map(|&capability| {
        let (group, label, description) = metadata(capability);
        json!({"id":capability.id(),"group":group,"label":label,"description":description,"delegable":host_delegable(capability)})
    }).collect::<Vec<_>>()})
}

fn metadata(capability: Capability) -> (&'static str, &'static str, &'static str) {
    use Capability::*;
    match capability {
        StatusView => ("Status", "View status", "Read basic server status."),
        StatusDetailed => (
            "Status",
            "View detailed status",
            "Read lifecycle and readiness details.",
        ),
        MetricsView => (
            "Status",
            "View metrics",
            "Read resource and performance metrics.",
        ),
        ActivityView => ("Status", "View activity", "Read server activity history."),
        TasksView => ("Status", "View tasks", "Read task progress and results."),
        DiagnosticsView => (
            "Status",
            "View diagnostics",
            "Read operational diagnostics.",
        ),
        LogsRead => (
            "Console",
            "Read logs",
            "Read server logs, which may contain sensitive information.",
        ),
        ConsoleRead => ("Console", "Read console", "View Minecraft console output."),
        ConsoleWrite => (
            "Console",
            "Send console commands",
            "Send raw commands with the Minecraft console's privileges.",
        ),
        PlayersView => (
            "Players",
            "View players",
            "Read the player list and online player identities.",
        ),
        PlayersModerate => (
            "Players",
            "Moderate players",
            "Use typed moderation actions such as kicks and bans.",
        ),
        PlayersOperator => (
            "Players",
            "Manage Minecraft operators",
            "Grant or revoke Minecraft operator status.",
        ),
        LifecycleStart => ("Lifecycle", "Start server", "Start the Minecraft process."),
        LifecycleStop => (
            "Lifecycle",
            "Stop server",
            "Gracefully stop the Minecraft process.",
        ),
        LifecycleRestart => (
            "Lifecycle",
            "Restart server",
            "Stop and start the Minecraft process.",
        ),
        LifecycleForceKill => (
            "Lifecycle",
            "Force stop",
            "Terminate Minecraft without a graceful shutdown.",
        ),
        MaintenanceToggle => (
            "Lifecycle",
            "Toggle maintenance",
            "Enter or leave maintenance mode.",
        ),
        FilesList => ("Files", "List files", "List paths and directory entries."),
        FilesRead => ("Files", "Read files", "View or download server files."),
        FilesCreate => ("Files", "Create files", "Upload or write new files."),
        FilesOverwrite => (
            "Files",
            "Overwrite files",
            "Replace the contents of existing files.",
        ),
        FilesRename => ("Files", "Rename paths", "Rename or move files and folders."),
        FilesMkdir => (
            "Files",
            "Create folders",
            "Create directories in the game tree.",
        ),
        FilesDelete => (
            "Files",
            "Delete files",
            "Move server files or folders into trash.",
        ),
        ContentInstall => (
            "Files",
            "Install content",
            "Install server content, including executable mods or plugins.",
        ),
        StorageView => (
            "Files",
            "View storage",
            "Read server storage usage and limits.",
        ),
        BackupsView => (
            "Backups",
            "View backups",
            "Read backup profiles, snapshots and job history.",
        ),
        BackupsCreate => ("Backups", "Create backups", "Start a backup job."),
        BackupsVerify => (
            "Backups",
            "Verify backups",
            "Check the integrity of stored backup data.",
        ),
        BackupsRestore => (
            "Backups",
            "Restore backups",
            "Restore data from a backup snapshot.",
        ),
        RecoveryExport => (
            "Backups",
            "Export recovery data",
            "Export a full recovery package.",
        ),
        BackupsConfigure => (
            "Backups",
            "Configure backups",
            "Change backup profiles, retention and limits.",
        ),
        SchedulesView => (
            "Automation",
            "View schedules",
            "Read scheduled actions and their state.",
        ),
        SchedulesEdit => (
            "Automation",
            "Edit schedules",
            "Create, change or delete scheduled actions.",
        ),
        SchedulesRun => (
            "Automation",
            "Run schedules",
            "Run scheduled workflows on demand.",
        ),
        AutomationView => (
            "Automation",
            "View workflows",
            "Read workflow definitions and execution history.",
        ),
        AutomationManage => (
            "Automation",
            "Manage workflows",
            "Create, edit or enable automation workflows.",
        ),
        NotificationsView => (
            "Automation",
            "View notifications",
            "Read and acknowledge server notifications.",
        ),
        ConfigView => ("Config", "View configuration", "Read server settings."),
        ConfigEdit => ("Config", "Edit configuration", "Change server settings."),
        IntegrationsEdit => (
            "Config",
            "Edit integrations",
            "Configure external service integrations.",
        ),
        PackAccess => (
            "Packs",
            "Access packs",
            "Use the limited pack access surface.",
        ),
        PackEdit => (
            "Packs",
            "Edit packs",
            "Change pack configuration and contents.",
        ),
        PackBuild => ("Packs", "Build packs", "Prepare a pack release."),
        PackPublish => (
            "Packs",
            "Publish packs",
            "Make a pack release available to players.",
        ),
        PackForceUpdate => (
            "Packs",
            "Require pack updates",
            "Change mandatory client pack revision.",
        ),
        PackDownload => ("Packs", "Download packs", "Download the published pack."),
        PackRealtime => (
            "Packs",
            "Receive pack updates",
            "Subscribe to live pack change events.",
        ),
        PublicInfo => (
            "Status",
            "View public information",
            "Read the public server identity and status.",
        ),
        PrincipalsView => (
            "Access",
            "View users",
            "Read server users and their role assignments.",
        ),
        PrincipalsManage => ("Access", "Manage users", "Manage server principal records."),
        DevicesRevoke => ("Access", "Revoke devices", "Revoke a device's access."),
        RolesCreate => (
            "Access",
            "Create roles",
            "Create custom roles within delegation authority.",
        ),
        RolesAssign => (
            "Access",
            "Assign roles",
            "Grant roles to lower-ranked users.",
        ),
        RolesRevoke => (
            "Access",
            "Revoke roles",
            "Remove roles from lower-ranked users.",
        ),
        RolesManage => (
            "Access",
            "Manage roles",
            "Edit or delete delegated roles and update user overrides.",
        ),
        CredentialsManage => (
            "Access",
            "Manage credentials",
            "Manage authentication credentials.",
        ),
        SecurityManage => (
            "Access",
            "Manage security",
            "Change server security policy.",
        ),
        AuditView => (
            "Access",
            "View audit history",
            "Read security audit events.",
        ),
        OwnerRecovery => (
            "Access",
            "Recover ownership",
            "Recover owner access using local authority.",
        ),
        AgentInstall => (
            "Access",
            "Install the agent",
            "Manage local agent installation.",
        ),
        AccessSourcesManage => (
            "Access",
            "Manage access sources",
            "Configure host-to-Minecraft access synchronization.",
        ),
        InvitationsCreate => (
            "Access",
            "Create invitations",
            "Issue invitations for eligible lower-ranked roles.",
        ),
    }
}
