//! Quiescent-state inspection only. Never use Host::open or StateDatabase::open here.
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{fs, path::Path, process::ExitCode};
use vesta_control::config::{HostConfiguration, ListenerDescription, PackAccessMode};
use vesta_state::{CURRENT_SCHEMA_VERSION, StateDirectory};

pub fn run(source: &Path, base: &Path) -> ExitCode {
    match inspect(source, base) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(code) => {
            eprintln!("status_failed: {code}");
            ExitCode::FAILURE
        }
    }
}

fn inspect(source: &Path, base: &Path) -> Result<String, &'static str> {
    let configuration =
        HostConfiguration::load_file(source, base).map_err(|_| "status_config_unavailable")?;
    let paths = configuration
        .validate_host_paths()
        .map_err(|_| "status_config_unavailable")?;
    let state = paths.state_directory();
    if !plain_file(state.database_file())? {
        return Err("status_state_missing");
    }
    state
        .verify_database_files()
        .map_err(|_| "status_state_unavailable")?;
    require_quiescent(state)?;
    let mut uri =
        url::Url::from_file_path(state.database_file()).map_err(|_| "status_state_unavailable")?;
    uri.set_query(Some("mode=ro&immutable=1"));
    // Immutable avoids SQLite's read-only WAL reader creating/writing SHM. It is
    // valid only for quiescent state; companions are refused, never ignored.
    let connection = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "status_state_unavailable")?;
    let output = read_snapshot(&connection, &configuration, state)
        .map_err(|_| "status_state_unavailable")?;
    require_quiescent(state)?;
    state
        .verify_database_files()
        .map_err(|_| "status_state_unavailable")?;
    Ok(output)
}

fn require_quiescent(state: &StateDirectory) -> Result<(), &'static str> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = state.database_file().as_os_str().to_owned();
        name.push(suffix);
        match fs::symlink_metadata(Path::new(&name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err("status_requires_quiescent_state"),
            Err(_) => return Err("status_state_unavailable"),
        }
    }
    Ok(())
}

fn plain_file(path: &Path) -> Result<bool, &'static str> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if plain(&metadata) && metadata.is_file() => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => Err("status_state_unavailable"),
    }
}

fn plain(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reject every reparse point, not only symlinks.
        metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        !metadata.file_type().is_symlink()
    }
}

fn read_snapshot(
    connection: &Connection,
    configuration: &HostConfiguration,
    state: &StateDirectory,
) -> Result<String, Box<dyn std::error::Error>> {
    connection.execute_batch("PRAGMA query_only=ON; BEGIN;")?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != CURRENT_SCHEMA_VERSION {
        return Err("unsupported schema".into());
    }
    let server: String = connection.query_row(
        "SELECT server_id FROM server_identity WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    uuid::Uuid::parse_str(&server)?;
    let release = connection
        .query_row(
            "SELECT revision,minimum_revision,format,sha256,size FROM pack_releases WHERE server_id=? ORDER BY revision DESC LIMIT 1",
            [&server],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?)),
        )
        .optional()?;
    let (hosted, artifact) = match release {
        None => ("none".to_owned(), "not_applicable"),
        Some((revision, minimum, format, hash, size)) => {
            if minimum <= 0
                || minimum > revision
                || revision > 9_007_199_254_740_991
                || !matches!(
                    format.as_str(),
                    "vestapack" | "zip" | "mrpack" | "catalogue"
                )
                || hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("invalid release".into());
            }
            let size = u64::try_from(size)?;
            let root = state.path().join("artifacts");
            let present = match fs::symlink_metadata(&root) {
                Ok(metadata) if plain(&metadata) && metadata.is_dir() => {
                    let path = root.join(format!("{hash}.{format}"));
                    if plain_file(&path)? {
                        fs::metadata(path)?.len() == size
                    } else {
                        false
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                _ => return Err("invalid artifacts directory".into()),
            };
            (
                format!("revision={revision}; minimum={minimum}"),
                if present {
                    "present"
                } else {
                    "missing_or_size_mismatch"
                },
            )
        }
    };
    let file_pending = count(
        connection,
        "SELECT count(*) FROM file_intents WHERE server_id=? AND status='pending'",
        &server,
    )?;
    let file_recovery = count(
        connection,
        "SELECT count(*) FROM file_intents WHERE server_id=? AND status='recovery-required'",
        &server,
    )?;
    let operation_pending = count(
        connection,
        "SELECT count(*) FROM operations WHERE server_id=? AND state IN ('queued','validating','awaiting_confirmation','running','cancelling')",
        &server,
    )?;
    let operation_recovery = count(
        connection,
        "SELECT count(*) FROM operations WHERE server_id=? AND state='recovery_required'",
        &server,
    )?;
    let audit = count(
        connection,
        "SELECT COALESCE(max(seq),0) FROM audit_events WHERE server_id=?",
        &server,
    )?;
    let mode = match configuration.pack_access() {
        PackAccessMode::Minecraft => "minecraft",
        PackAccessMode::MinecraftOrPassword => "minecraft-or-password",
    };
    let listener = match configuration.listener() {
        ListenerDescription::Disabled => "disabled",
        ListenerDescription::Tls(_) => "tls",
    };
    connection.execute_batch("ROLLBACK;")?;
    Ok(format!(
        "status_scope=quiescent_state; runtime_health=not_observed\nserver_id=[redacted]\nconfigured_pack_access={mode}; configured_listener={listener}\nconfigured_pack_revision={}; configured_pack_minimum={}\nhosted_pack={hosted}\nhosted_artifact={artifact}; digest=not_verified\nfile_intents_pending={file_pending}; file_intents_recovery_required={file_recovery}\noperations_pending={operation_pending}; operations_recovery_required={operation_recovery}\nlast_audit_sequence={audit}\n",
        configuration.pack().revision(),
        configuration.pack().minimum_revision(),
    ))
}

fn count(connection: &Connection, sql: &str, server: &str) -> rusqlite::Result<u64> {
    connection.query_row(sql, [server], |row| {
        let value: i64 = row.get(0)?;
        u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
    })
}
