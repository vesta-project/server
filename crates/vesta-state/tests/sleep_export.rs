use std::sync::Arc;
use vesta_state::{
    StateDatabase, StateDirectory, SystemClock,
    export::{read_recovery_export, write_recovery_export},
};
#[test]
fn sleep_recovery_exports_policy_but_never_replays_transient_wake_stop_intents() {
    let root = tempfile::tempdir().unwrap();
    let db = StateDatabase::open(
        StateDirectory::new(root.path().join("state"), None).unwrap(),
        Arc::new(SystemClock),
    )
    .unwrap();
    db.write(|c|{for key in ["sleep-policy","sleep-runtime","whitelist-runtime"]{c.execute("INSERT INTO settings(server_id,key,desired_value,desired_revision,apply_mode,effective_revision,updated_at) VALUES(?,?,'{}',1,'immediate',0,0)",rusqlite::params![db.server_id(),key])?;}Ok(())}).unwrap();
    let mut out = Vec::new();
    write_recovery_export(&db, &mut out, None).unwrap();
    let mut export = read_recovery_export(&mut out.as_slice(), None).unwrap();
    let settings = export.tables.get_mut("settings").unwrap();
    assert!(settings.rows.iter().any(|r| r[1] == "sleep-policy"));
    assert!(!settings.rows.iter().any(|r| r[1] == "sleep-runtime"));
    assert!(!settings.rows.iter().any(|r| r[1] == "whitelist-runtime"));
    let mut whitelist_forged = export.clone();
    let settings = whitelist_forged.tables.get_mut("settings").unwrap();
    let mut forged = settings.rows[0].clone();
    forged[1] = serde_json::json!("whitelist-runtime");
    settings.rows.push(forged);
    assert!(
        read_recovery_export(
            &mut serde_json::to_vec(&whitelist_forged).unwrap().as_slice(),
            None
        )
        .is_err()
    );
    let settings = export.tables.get_mut("settings").unwrap();
    let mut forged = settings.rows[0].clone();
    forged[1] = serde_json::json!("sleep-runtime");
    settings.rows.push(forged);
    let forged = serde_json::to_vec(&export).unwrap();
    assert!(read_recovery_export(&mut forged.as_slice(), None).is_err());
    db.close().unwrap();
}
