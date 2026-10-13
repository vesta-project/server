mod support;
use support::Fixture;
use vesta_state::StateDatabase;

#[test]
fn original_v11_upgrades_access_defaults_without_weakening_builtin_protection() {
    let f = Fixture::open();
    let server = f.db.server_id().to_owned();
    let directory = f.db.directory().clone();
    f.db.write(|c| {
        c.execute_batch(support::REMOVE_V23_SCHEMA)?; c.execute_batch(support::REMOVE_V22_SCHEMA)?; c.execute_batch(support::REMOVE_V18_SCHEMA)?; c.execute_batch(support::REMOVE_V16_SCHEMA)?; c.execute_batch("DROP TABLE management_file_outcomes; DROP TABLE management_pack_outcomes; DROP TABLE pack_minimum_policy; DROP TABLE management_device_keys; DROP TABLE management_pairing_codes; DROP TABLE management_owner_links; DROP TABLE user_minecraft_accounts; DROP TABLE user_default_roles; DROP TABLE device_password_grants; DROP TABLE access_role_sources; DELETE FROM role_capabilities WHERE role_id IN ('public','player'); DELETE FROM roles WHERE id IN ('public','player'); DROP TRIGGER role_capabilities_builtin_no_delete; DELETE FROM role_capabilities WHERE capability IN ('public.info','pack.download','pack.realtime'); CREATE TRIGGER role_capabilities_builtin_no_delete BEFORE DELETE ON role_capabilities WHEN (SELECT builtin FROM roles WHERE id=OLD.role_id)=1 BEGIN SELECT RAISE(ABORT,'builtin roles are immutable'); END; PRAGMA user_version=11;")?;
        Ok(())
    }).unwrap();
    f.db.close().unwrap();
    let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
    assert_eq!(upgraded.server_id(), server);
    upgraded.read(|c| {
        let version:u32=c.query_row("PRAGMA user_version",[],|r|r.get(0))?;assert_eq!(version,vesta_state::CURRENT_SCHEMA_VERSION);
        let settings:String=c.query_row("SELECT settings_json FROM access_role_sources",[],|r|r.get(0))?;
        assert_eq!(settings,"{\"anonymous\":\"public\",\"new_user\":\"public\",\"whitelist\":\"player\",\"whitelist_enabled\":true,\"password\":\"player\"}");
        let grants:u32=c.query_row("SELECT count(*) FROM role_capabilities WHERE role_id='owner' AND capability IN ('public.info','pack.download','pack.realtime')",[],|r|r.get(0))?;assert_eq!(grants,3);
        Ok(())
    }).unwrap();
    assert!(
        upgraded
            .write(|c| {
                c.execute(
                    "INSERT INTO role_capabilities VALUES ('owner','fixture.unsafe','*')",
                    [],
                )?;
                Ok(())
            })
            .is_err()
    );
    assert!(
        upgraded
            .write(|c| {
                c.execute("DELETE FROM role_capabilities WHERE role_id='owner'", [])?;
                Ok(())
            })
            .is_err()
    );
    upgraded.close().unwrap();
}

#[test]
fn populated_v11_name_and_identifier_collisions_preserve_custom_management_roles() {
    for (public_name, player_name, public_id, player_id) in [
        ("Public", "Player", "legacy-public", "legacy-player"),
        ("pUbLiC", "pLaYeR", "public", "player"),
    ] {
        let f = Fixture::open();
        let directory = f.db.directory().clone();
        let server = f.db.server_id().to_owned();
        f.db.close().unwrap();
        let old = rusqlite::Connection::open(directory.database_file()).unwrap();
        support::remove_v12_schema(&old);
        old.execute(
            "INSERT INTO principals VALUES ('legacy-person',?,'Preserved','active',17,18)",
            [&server],
        )
        .unwrap();
        for (id, name) in [
            (public_id, public_name),
            (player_id, player_name),
            ("suffix-public", "Public (Vesta default 1)"),
            ("suffix-player", "Player (Vesta default 1)"),
        ] {
            old.execute(
                "INSERT INTO roles VALUES (?,?,55,0,'legacy-owner',23)",
                rusqlite::params![id, name],
            )
            .unwrap();
            old.execute(
                "INSERT INTO role_capabilities VALUES (?,'status.view','*')",
                [id],
            )
            .unwrap();
            old.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES (?,'legacy-person',?,'legacy-owner',24)",rusqlite::params![format!("assignment-{id}"),id]).unwrap();
        }
        old.execute_batch("PRAGMA user_version=11;").unwrap();
        drop(old);
        let upgraded = StateDatabase::open(directory, f.clock.clone()).unwrap();
        upgraded.read(|c| {
            for (id,name) in [(public_id,public_name),(player_id,player_name),("suffix-public","Public (Vesta default 1)"),("suffix-player","Player (Vesta default 1)")] {
                assert_eq!(c.query_row("SELECT name,rank,builtin,created_by,created_at FROM roles WHERE id=?",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u32>(1)?,r.get::<_,u32>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?)))?,(name.into(),55,0,"legacy-owner".into(),23));
                assert_eq!(c.query_row("SELECT role_id,granted_by,created_at,revoked_at FROM role_assignments WHERE id=?",[format!("assignment-{id}")],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,Option<i64>>(3)?)))?,(id.into(),"legacy-owner".into(),24,None));
                let mut q=c.prepare("SELECT capability FROM role_capabilities WHERE role_id=? ORDER BY capability")?;
                assert_eq!(q.query_map([id],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?,vec!["status.view"]);
            }
            let settings:String=c.query_row("SELECT settings_json FROM access_role_sources",[],|r|r.get(0))?;
            let settings:serde_json::Value=serde_json::from_str(&settings).unwrap();
            for (source,expected) in [("anonymous",vec!["public.info"]),("new_user",vec!["public.info"]),("whitelist",vec!["pack.download","pack.realtime","public.info"]),("password",vec!["pack.download","pack.realtime","public.info"])] {
                let id=settings[source].as_str().unwrap();assert!(id!=public_id && id!=player_id);
                let mut q=c.prepare("SELECT capability FROM role_capabilities WHERE role_id=? ORDER BY capability")?;
                assert_eq!(q.query_map([id],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?,expected);
                assert_eq!(c.query_row("SELECT builtin FROM roles WHERE id=?",[id],|r|r.get::<_,u32>(0))?,0);
            }
            Ok(())
        }).unwrap();
        upgraded.close().unwrap();
    }
}
