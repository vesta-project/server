use super::tests::Fixture;
use super::*;
use crate::packs::payload::Store;
use std::io::{Read, Seek, SeekFrom, Write};

#[test]
fn large_archive_upload_plans_and_applies_every_file_without_sql_payloads() {
    let f = Fixture::new();
    let archive_path = f.temp.path().join("large.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let block = vec![0x5a; 1024 * 1024];
    zip.start_file("config/large.bin", options).unwrap();
    for _ in 0..70 {
        zip.write_all(&block).unwrap();
    }
    for i in 0..300 {
        zip.start_file(format!("config/part-{i}.json"), options)
            .unwrap();
        zip.write_all(b"{}\n").unwrap();
    }
    zip.finish().unwrap();
    let mut source = std::fs::File::open(&archive_path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 512 * 1024];
    loop {
        let n = source.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let size = source.metadata().unwrap().len();
    assert!(size > 68 * 1024 * 1024);
    f.workspace
        .upload_begin(
            &f.subject,
            "begin",
            &UploadBegin {
                upload_id: "large".into(),
                total_size: size.to_string(),
                sha256: hex::encode(hash.finalize()),
                archive: true,
            },
        )
        .unwrap();
    source.seek(SeekFrom::Start(0)).unwrap();
    let mut offset = 0;
    loop {
        let n = source.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        f.workspace
            .upload_chunk(
                &f.subject,
                &format!("chunk-{offset}"),
                &UploadChunk {
                    upload_id: "large".into(),
                    offset: offset.to_string(),
                    content_base64: STANDARD.encode(&buffer[..n]),
                },
            )
            .unwrap();
        offset += n as u64;
    }
    f.workspace
        .upload_finish(&f.subject, "finish", "large")
        .unwrap();
    assert_eq!(
        f.db.read(|c| c
            .query_row("SELECT length(body) FROM pack_workspace_uploads", [], |r| r
                .get::<_, i64>(0))
            .map_err(Into::into))
            .unwrap(),
        0
    );
    let game = f.temp.path().join("game");
    std::fs::create_dir(&game).unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    let releases = super::super::PackReleases::open(&f.db).unwrap();
    let providers = crate::content::UreqProviderClient::new(None);
    let inspection = f
        .workspace
        .inspect_archive(
            &f.subject,
            "1.21.1",
            &InspectRequest {
                upload_id: "large".into(),
            },
            &providers,
        )
        .unwrap();
    assert_eq!(inspection.entry_count, 301);
    let plan = f
        .workspace
        .install_plan(
            &f.subject,
            "1.21.1",
            Some("vanilla"),
            &PackInstallRequest {
                source: InstallSource::UploadedArchive {
                    upload_id: "large".into(),
                },
            },
            &files,
            &releases,
            &providers,
        )
        .unwrap();
    assert_eq!(plan.files.len(), 301);
    assert!(plan.valid);
    let stored =
        f.db.read(|c| {
            c.query_row(
                "SELECT body FROM pack_install_plans WHERE id=?",
                [&plan.plan_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert!(stored.len() < 1024 * 1024, "review retained payload bytes");
    assert!(
        serde_json::from_str::<serde_json::Value>(&stored)
            .unwrap()
            .get("bytes")
            .is_none()
    );
    let outcome = f
        .workspace
        .install_apply(
            &f.subject,
            "1.21.1",
            Some("vanilla"),
            "apply",
            &PackInstallApply {
                acknowledge_warnings: true,
                acknowledge_software_switch: false,
                plan_id: plan.plan_id,
                confirmation_token: plan.confirmation_token,
                choices: vec![],
                set_as_modpack: false,
            },
            &files,
        )
        .unwrap();
    assert_eq!(outcome.pending_ids.len(), 301);
    let world = files.world(&f.subject).unwrap();
    let inventory = files
        .content_inventory(&f.subject, Some("vanilla".into()), "1.21.1".into(), &world)
        .unwrap();
    assert_eq!(inventory.items.len(), 0);
    assert!(
        !inventory
            .items
            .iter()
            .any(|item| item.path == "config/large.bin")
    );
    assert_eq!(outcome.state, "installed");
    assert_eq!(inventory.deferred_writes, 0);
    f.workspace
        .upload_cancel(&f.subject, "cancel", "large")
        .unwrap();
    let reopened = PackWorkspace::open(&f.db).unwrap();
    assert!(reopened.server_link(&f.subject).unwrap().is_some());
    files.apply_pending_content().unwrap();
    assert_eq!(
        std::fs::metadata(game.join("config/large.bin"))
            .unwrap()
            .len(),
        70 * 1024 * 1024
    );
    for i in 0..300 {
        assert_eq!(
            std::fs::read(game.join(format!("config/part-{i}.json"))).unwrap(),
            b"{}\n"
        );
    }
    assert_eq!(
        f.db.read(|c| c
            .query_row(
                "SELECT count(*) FROM server_content_changes WHERE state='pending'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .map_err(Into::into))
            .unwrap(),
        0
    );
    assert_eq!(
        std::fs::read_dir(f.db.directory().path().join("pack-payloads"))
            .unwrap()
            .count(),
        0,
        "applied/expired payloads leaked"
    );
}

#[test]
fn spooled_upload_retries_and_unacknowledged_tails_preserve_exact_bytes() {
    let f = Fixture::new();
    let sha = digest(b"abcdef");
    f.workspace
        .upload_begin(
            &f.subject,
            "begin",
            &UploadBegin {
                upload_id: "resume".into(),
                total_size: "6".into(),
                sha256: sha.clone(),
                archive: true,
            },
        )
        .unwrap();
    let chunk = UploadChunk {
        upload_id: "resume".into(),
        offset: "0".into(),
        content_base64: STANDARD.encode(b"abc"),
    };
    assert_eq!(
        f.workspace
            .upload_chunk(&f.subject, "chunk1", &chunk)
            .unwrap()
            .size,
        "3"
    );
    assert_eq!(
        f.workspace
            .upload_chunk(&f.subject, "chunk1", &chunk)
            .unwrap()
            .size,
        "3"
    );
    let store = Store::open(&f.db).unwrap();
    let name =
        f.db.read(|c| {
            c.query_row("SELECT name FROM pack_spooled_uploads", [], |r| {
                r.get::<_, String>(0)
            })
            .map_err(Into::into)
        })
        .unwrap();
    store.upload_append(&name, 3, b"uncommitted tail").unwrap();
    f.workspace
        .upload_chunk(
            &f.subject,
            "chunk2",
            &UploadChunk {
                upload_id: "resume".into(),
                offset: "3".into(),
                content_base64: STANDARD.encode(b"def"),
            },
        )
        .unwrap();
    f.workspace
        .upload_finish(&f.subject, "finish", "resume")
        .unwrap();
    assert_eq!(
        f.workspace.archive_upload(&f.subject, "resume").unwrap(),
        b"abcdef"
    );
    f.workspace
        .upload_cancel(&f.subject, "cancel", "resume")
        .unwrap();
    assert_eq!(
        std::fs::read_dir(f.db.directory().path().join("pack-payloads"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn tampered_disk_payload_refuses_activation_and_preserves_existing_file() {
    let f = Fixture::new();
    let game = f.temp.path().join("game");
    std::fs::create_dir_all(game.join("config")).unwrap();
    std::fs::write(game.join("config/a"), b"old").unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    let store = Store::open(&f.db).unwrap();
    let payload = store.put(&mut std::io::Cursor::new(b"new")).unwrap();
    f.db.write_with(|tx| {
        files.stage_pack_payloads_in(
            tx.connection(),
            &f.subject,
            "stage",
            &[(
                "config/a".into(),
                Some(digest(b"old")),
                payload.clone(),
                None,
            )],
        )
    })
    .unwrap();
    store
        .open_file(&payload)
        .unwrap()
        .write_all(b"bad")
        .unwrap();
    assert!(files.apply_pending_content().is_err());
    assert_eq!(std::fs::read(game.join("config/a")).unwrap(), b"old");
    store.collect().unwrap();
    assert!(
        store.open_file(&payload).is_ok(),
        "pending retry payload removed"
    );
}

#[test]
fn inventory_lists_large_mod_directories_without_the_old_thousand_item_cutoff() {
    let f = Fixture::new();
    let game = f.temp.path().join("game");
    std::fs::create_dir_all(game.join("mods")).unwrap();
    for i in 0..1100 {
        std::fs::write(game.join(format!("mods/mod-{i}.jar")), b"").unwrap();
    }
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    let world = files.world(&f.subject).unwrap();
    let inventory = files
        .content_inventory(&f.subject, Some("fabric".into()), "1.21.1".into(), &world)
        .unwrap();
    assert_eq!(inventory.items.len(), 1100);
    assert!(!inventory.truncated);
}

#[test]
fn pack_effects_do_not_consume_the_interactive_file_receipt_quota() {
    let f = Fixture::new();
    let game = f.temp.path().join("game");
    std::fs::create_dir_all(game.join("config")).unwrap();
    let files = crate::files::FileBrowser::open(f.db.clone(), &game, &[]).unwrap();
    f.db.write(|c| {
        c.execute("WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM seq WHERE n<10000) INSERT INTO management_file_outcomes(server_id,principal_id,device_id,request_key,input_sha256,authority_json,outcome_json,completed_at) SELECT ?,?,?,printf('prior-%d',n),?, '[]','{}',? FROM seq",params![f.db.server_id(),f.subject.principal.as_str(),f.subject.device.as_str(),"a".repeat(64),f.db.clock().millis()])?;Ok(())
    }).unwrap();
    let store = Store::open(&f.db).unwrap();
    let payload = store.put(&mut std::io::Cursor::new(b"pack")).unwrap();
    f.db.write_with(|tx| {
        files.stage_pack_payloads_in(
            tx.connection(),
            &f.subject,
            "stage",
            &[("config/a".into(), None, payload, None)],
        )
    })
    .unwrap();
    files.apply_pending_content().unwrap();
    assert_eq!(std::fs::read(game.join("config/a")).unwrap(), b"pack");
}
