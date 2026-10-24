use super::*;
use crate::census::{Census, ProcessFact};
use std::collections::BTreeSet;

fn identity(pid: i64, start: u64, group: i64, session: i64) -> ProcessIdentity {
    ProcessIdentity {
        boot: "darwin:test".into(),
        pid,
        native_start: start,
        group_id: group,
        session_id: session,
    }
}
fn fact(identity: ProcessIdentity, parent_pid: i64) -> ProcessFact {
    ProcessFact {
        identity,
        parent_pid,
        zombie: false,
        cpu_micros: None,
        command: None,
    }
}
fn census(facts: Vec<ProcessFact>, complete: bool) -> Census {
    let absent: Vec<_> = [20, 30]
        .into_iter()
        .filter(|pid| !facts.iter().any(|fact| fact.identity.pid == *pid))
        .collect();
    Census::from_observations(facts, [], absent, complete, false, 100).unwrap()
}
#[test]
fn fresh_writer_lease_refuses_a_replaced_or_unlinked_original() {
    for unlink in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(root.path()).unwrap();
        let lease = directory
            .try_lock(OsStr::new("writer.lock"), crate::fs::LockMode::Exclusive)
            .unwrap();
        let inherited = lease.handoff_file().try_clone().unwrap();
        let expected = crate::fs::Metadata::from_std(inherited.metadata().unwrap());
        if unlink {
            std::fs::remove_file(root.path().join("writer.lock")).unwrap();
        } else {
            std::fs::rename(root.path().join("writer.lock"), root.path().join("held")).unwrap();
        }
        drop(lease);
        assert!(try_writer_lease(&directory, expected).is_err());
        drop(inherited);
    }
    let root = tempfile::tempdir().unwrap();
    let directory = Directory::open_anchor(root.path()).unwrap();
    let lease = directory
        .try_lock(OsStr::new("writer.lock"), crate::fs::LockMode::Exclusive)
        .unwrap();
    let expected = crate::fs::Metadata::from_std(lease.handoff_file().metadata().unwrap());
    assert!(try_writer_lease(&directory, expected).is_err());
    drop(lease);
    assert!(try_writer_lease(&directory, expected).is_ok());
}

#[test]
fn darwin_settlement_requires_complete_absence_of_members_and_recorded_escapes() {
    let guardian = identity(10, 1, 10, 10);
    let provider = identity(20, 2, 20, 10);
    let escaped = identity(30, 3, 30, 30);
    let history = BTreeSet::from([escaped.clone()]);
    let clear = vec![fact(guardian.clone(), 1)];
    assert!(darwin_census_clear(
        &census(clear.clone(), true),
        &provider,
        &guardian,
        &history
    ));
    assert!(!darwin_census_clear(
        &census(clear.clone(), false),
        &provider,
        &guardian,
        &history
    ));
    for survivor in [
        provider.clone(),
        identity(21, 4, 20, 99),
        identity(22, 4, 99, 10),
        escaped.clone(),
    ] {
        let mut facts = clear.clone();
        facts.push(fact(survivor, 1));
        assert!(!darwin_census_clear(
            &census(facts, true),
            &provider,
            &guardian,
            &history
        ));
    }
    // Complete enumeration alone does not prove a known PID's absence.
    let unknown_history = BTreeSet::from([identity(40, 3, 40, 40)]);
    assert!(!darwin_census_clear(
        &census(clear.clone(), true),
        &provider,
        &guardian,
        &unknown_history
    ));
    // Reused PID with a different start stamp is not the recorded daemon.
    let mut facts = clear;
    facts.push(fact(identity(30, 40, 30, 30), 1));
    assert!(darwin_census_clear(
        &census(facts, true),
        &provider,
        &guardian,
        &history
    ));
}
