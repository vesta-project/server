use super::*;
use std::{sync::mpsc, thread, time::Duration};

#[test]
fn empty_pulse_does_not_wait_behind_an_unrelated_writer() {
    let f = Fixture::new();
    let db = f.db.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        db.write(|_| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok(())
        })
        .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let automation = f.automation.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let pulse = thread::spawn(move || done_tx.send(automation.tick(NOW)).unwrap());
    let result = done_rx.recv_timeout(Duration::from_secs(2));
    // Always release and join the owned threads before reporting a regression.
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    pulse.join().unwrap();
    result
        .expect("an empty pulse must not request writer admission")
        .unwrap();
}

#[test]
fn empty_pulse_does_not_cache_away_new_work_or_closed_state() {
    let f = Fixture::new();
    f.pulse(0);
    let workflow = f.create(definition(Trigger::Manual, vec![step("notice", notice())]));
    let id = f.manual(&workflow);
    f.pulse(1);
    assert_eq!(f.run(&id).state, "succeeded");
    assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 1);
    f.db.close().unwrap();
    assert!(f.automation.tick(NOW + 2).is_err());
}

#[test]
fn retention_still_runs_without_enabled_workflows() {
    let f = Fixture::new();
    f.automation
        .notify(Severity::Info, "Fresh", "Keep me", "test")
        .unwrap();
    assert!(
        !f.automation
            .read(|c| f.automation.tick_needed(c, NOW))
            .unwrap()
    );
    let after_expiry = inbox::HISTORY_MS + 1;
    assert!(
        f.automation
            .read(|c| f.automation.tick_needed(c, NOW + after_expiry))
            .unwrap()
    );
    f.pulse(after_expiry);
    assert_eq!(f.automation.unread_count(&f.owner).unwrap(), 0);
    assert!(
        !f.automation
            .read(|c| f.automation.tick_needed(c, NOW + after_expiry))
            .unwrap()
    );
}

#[test]
fn finished_history_expiry_is_detected_after_workflow_is_deleted() {
    let f = Fixture::new();
    let workflow = f.create(definition(Trigger::Manual, vec![step("notice", notice())]));
    let id = f.manual(&workflow);
    f.pulse(0);
    f.db.write(|c| {
        c.execute("DELETE FROM workflows WHERE id=?", [&workflow.id])?;
        c.execute("DELETE FROM notifications", [])?;
        Ok(())
    })
    .unwrap();
    assert!(
        !f.automation
            .read(|c| f.automation.tick_needed(c, NOW))
            .unwrap()
    );
    f.pulse(inbox::HISTORY_MS + 1);
    assert!(f.automation.get_run(&f.owner, &id).is_err());
}
