//! Remote recovery export: owner-only, audited, optionally passphrase-encrypted.
use std::sync::Arc;
use vesta_control::{
    identity::{DeviceId, PrincipalId, Subject},
    recovery_export::{RecoveryExportError, export_for},
};
use vesta_state::{StateDatabase, StateDirectory, SystemClock};

const OWNER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OWNER_DEVICE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const ADMIN: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const ADMIN_DEVICE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

fn subject(principal: &str, device: &str) -> Subject {
    Subject {
        principal: PrincipalId::parse(principal).unwrap(),
        device: DeviceId::parse(device).unwrap(),
    }
}

fn database(temp: &tempfile::TempDir) -> StateDatabase {
    let game = temp.path().join("game");
    std::fs::create_dir_all(&game).unwrap();
    let state = StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap();
    let db = StateDatabase::open(state, Arc::new(SystemClock)).unwrap();
    for (index, (principal, device, role)) in [
        (OWNER, OWNER_DEVICE, "owner"),
        (ADMIN, ADMIN_DEVICE, "administrator"),
    ]
    .into_iter()
    .enumerate()
    {
        db.write(|c| {
            let t = db.clock().millis();
            c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'User','active',?,?)", rusqlite::params![principal, db.server_id(), t, t])?;
            c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Device',?,?)", rusqlite::params![device, db.server_id(), principal, index.to_string().repeat(64), t])?;
            c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,?,?,'fixture',?)", rusqlite::params![format!("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeee{index}"), principal, role, t])?;
            Ok(())
        })
        .unwrap();
    }
    db
}

fn audited(db: &StateDatabase) -> Vec<String> {
    db.read(|c| {
        let mut s = c.prepare(
            "SELECT metadata_json FROM audit_events WHERE action='recovery.export' ORDER BY seq",
        )?;
        Ok(s.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?)
    })
    .unwrap()
}

#[test]
fn owners_export_plain_or_encrypted_and_every_export_is_audited() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(&temp);
    let owner = subject(OWNER, OWNER_DEVICE);

    // Administrators cannot export: recovery export is owner-reserved.
    assert_eq!(
        export_for(&db, &subject(ADMIN, ADMIN_DEVICE), None),
        Err(RecoveryExportError::Forbidden)
    );
    assert!(audited(&db).is_empty(), "refusals produce no export");

    let plain = export_for(&db, &owner, None).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(json["format"], "vesta-recovery-export");
    assert_eq!(json["server_id"], db.server_id());

    // A passphrase produces the encrypted format, readable only with it.
    assert_eq!(
        export_for(&db, &owner, Some(b"short")),
        Err(RecoveryExportError::Invalid)
    );
    let sealed = export_for(&db, &owner, Some(b"correct horse battery")).unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&sealed).is_err());
    assert!(
        !String::from_utf8_lossy(&sealed).contains(OWNER),
        "no plaintext identity in the encrypted export"
    );
    assert!(vesta_state::export::read_recovery_export(&mut sealed.as_slice(), None).is_err());
    assert!(
        vesta_state::export::read_recovery_export(
            &mut sealed.as_slice(),
            Some(b"wrong passphrase")
        )
        .is_err()
    );
    let opened = vesta_state::export::read_recovery_export(
        &mut sealed.as_slice(),
        Some(b"correct horse battery"),
    )
    .unwrap();
    assert_eq!(opened.server_id, db.server_id());

    let audit = audited(&db);
    assert_eq!(audit.len(), 2);
    assert!(audit[0].contains("false") && audit[1].contains("true"));
    assert!(
        audit.iter().all(|m| !m.contains("horse")),
        "passphrase never audited"
    );
}
