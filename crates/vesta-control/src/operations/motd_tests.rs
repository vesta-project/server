use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

#[test]
fn motd_icon_projects_while_stopped_and_clears_on_stop() {
    let mut h = Harness::new(false);
    let files = crate::files::FileBrowser::open(h.db.clone(), &h.cwd, &[]).unwrap();
    h.operations = h.operations.clone().with_content(files);
    let png = crate::motd::fallback_icon();
    let document = crate::motd::Document {
        normal_icon_png: Some(STANDARD.encode(&png)),
        ..crate::motd::Document::default()
    };
    let owned = h.runtime(Duration::from_secs(2));
    let icon = h.cwd.join(vesta_state::server_icon::FILE_NAME);
    let revision = h
        .operations
        .set_motd_policy(&h.subject, None, document.clone())
        .unwrap();
    wait_for(|| icon.is_file());
    assert_eq!(fs::read(&icon).unwrap(), png);

    start(&h, &owned.runtime);
    assert_eq!(fs::read(&icon).unwrap(), png);
    let revision = h
        .operations
        .set_motd_policy(&h.subject, Some(revision), crate::motd::Document::default())
        .unwrap();
    assert!(icon.is_file());
    owned
        .runtime
        .local_handle()
        .request(LocalAction::Stop)
        .unwrap();
    wait_for(|| !icon.is_file());

    fs::write(&icon, b"user supplied").unwrap();
    h.operations
        .set_motd_policy(&h.subject, Some(revision), document)
        .unwrap();
    start(&h, &owned.runtime);
    assert_eq!(fs::read(&icon).unwrap(), b"user supplied");
}

#[test]
fn motd_policy_is_revisioned_audited_and_rolls_back_failed_audit() {
    let h = Harness::new(false);
    let doc = crate::motd::Document::default();
    assert_eq!(h.operations.motd_policy(&h.subject).unwrap().revision, None);
    assert_eq!(
        h.operations
            .set_motd_policy(&h.subject, None, doc.clone())
            .unwrap(),
        1
    );
    assert_eq!(
        h.operations
            .set_motd_policy(&h.subject, None, doc.clone())
            .unwrap_err()
            .code(),
        "source_revision_unavailable"
    );
    h.db.write(|c|{c.execute_batch("CREATE TRIGGER refuse_motd_audit BEFORE INSERT ON audit_events WHEN NEW.action='motd.policy.update' AND NEW.outcome='accepted' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
    let mut changed = doc;
    changed
        .states
        .get_mut(&crate::motd::State::Offline)
        .unwrap()
        .template = "Changed".into();
    assert!(
        h.operations
            .set_motd_policy(&h.subject, Some(1), changed)
            .is_err()
    );
    let after = h.operations.motd_policy(&h.subject).unwrap();
    assert_eq!(after.revision, Some(1));
    assert_ne!(
        after.document.states[&crate::motd::State::Offline].template,
        "Changed"
    );
}
#[test]
fn motd_policy_requires_exact_current_configuration_authority() {
    let h = Harness::new(false);
    let doc = crate::motd::Document::default();
    h.db.write(|c|{c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'config.edit','*','deny','fixture',?)",params![uuid::Uuid::new_v4().to_string(),h.subject.principal.as_str(),h.db.clock().millis()])?;Ok(())}).unwrap();
    assert_eq!(
        h.operations
            .set_motd_policy(&h.subject, None, doc)
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert!(h.operations.motd_policy(&h.subject).is_ok());
    h.db.write(|c|{c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES(?,?,'config.view','*','deny','fixture',?)",params![uuid::Uuid::new_v4().to_string(),h.subject.principal.as_str(),h.db.clock().millis()])?;Ok(())}).unwrap();
    assert_eq!(
        h.operations.motd_policy(&h.subject).err().unwrap().code(),
        "forbidden"
    );
}
