use super::*;
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
