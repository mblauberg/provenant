use super::*;
use provenant_core::AttemptId;
use provenant_records::policy::{LEASE_HOLDER_UNREACHABLE, WORKTREE_SHARING_REFUSED};
use provenant_records::provenance::{Layer, Source};

fn sharing(value: Sharing) -> Sourced<Sharing> {
    Sourced {
        value,
        source: Source::at(Layer::Host, "config.toml#lanes.worktree.sharing"),
    }
}

fn holder(byte: u8) -> AttemptId {
    AttemptId::from_bytes([byte; 16])
}

fn clock() -> WallTime {
    WallTime::from_micros(0).unwrap_or(WallTime::MAX)
}

fn tree() -> CustodySubject {
    CustodySubject {
        kind: provenant_records::custody::SubjectKind::Worktree,
        id: "/fixture/shared".to_owned(),
    }
}

#[test]
fn live_lease_warns_by_default_and_allows_explicitly() {
    let tree = tree();
    let observation = lease_observation(holder(1), clock(), Some(true), clock(), Sharing::Inherit);
    assert!(matches!(
        admit_shared_writer(&tree, Some(&observation), &sharing(Sharing::Warn)),
        Admit::Proceed { warning: Some(_) }
    ));
    assert!(matches!(
        admit_shared_writer(&tree, Some(&observation), &sharing(Sharing::Allow)),
        Admit::Proceed { warning: None }
    ));
    assert!(matches!(
        admit_shared_writer(&tree, None, &sharing(Sharing::Warn)),
        Admit::Proceed { warning: None }
    ));
}

#[test]
fn lapsed_lease_never_refuses() {
    let tree = tree();
    let observation = lease_observation(holder(2), clock(), Some(false), clock(), Sharing::Inherit);
    assert!(matches!(
        admit_shared_writer(&tree, Some(&observation), &sharing(Sharing::Refuse)),
        Admit::Proceed { warning: None }
    ));
}

#[test]
fn owner_refuse_wins_over_configured_allow() {
    let tree = tree();
    let observation = lease_observation(holder(3), clock(), Some(true), clock(), Sharing::Refuse);
    let Admit::Refuse { error } =
        admit_shared_writer(&tree, Some(&observation), &sharing(Sharing::Allow))
    else {
        panic!("owner refusal should win over configured allow");
    };
    assert_eq!(error.code, WORKTREE_SHARING_REFUSED);
    assert_eq!(error.setting.as_ref().unwrap().layer, Layer::Parent);
    assert_eq!(error.setting.as_ref().unwrap().origin, "lease.json#sharing");
}

#[test]
fn unreachable_holder_is_reported_even_under_warn() {
    let tree = tree();
    let observation = lease_observation(holder(4), clock(), None, clock(), Sharing::Inherit);
    let admit = admit_shared_writer(&tree, Some(&observation), &sharing(Sharing::Warn));
    let Admit::Proceed {
        warning: Some(warning),
    } = admit
    else {
        panic!("unreachable holder should warn and proceed");
    };
    assert_eq!(warning.code.as_ref(), LEASE_HOLDER_UNREACHABLE);
    assert_eq!(warning.setting.as_ref().unwrap().layer, Layer::Default);
    assert_eq!(
        warning.setting.as_ref().unwrap().key,
        LEASE_UNREACHABLE_SECONDS_KEY
    );
}

#[test]
fn unreachable_grace_persists_first_observation_and_guards_destruction() {
    let temp = tempfile::tempdir().unwrap();
    let slot = Directory::open_anchor(temp.path()).unwrap();
    provenant_supervisor::custody::prepare_writer_observation(&slot).unwrap();
    let attempt = holder(8);
    let started = WallTime::from_unix_seconds(1_700_000_000).unwrap();
    let grace = Sourced {
        value: 10,
        source: Source::at(
            Layer::Host,
            "config.toml#domains.fabric.lanes.lease_unreachable_seconds",
        ),
    };

    let first = observe_reservation_liveness(&slot, attempt, 1, None, started, &grace).unwrap();
    let LeaseState::Unreachable {
        retry_at: first_deadline,
        ..
    } = first
    else {
        panic!("a new unreachable observation must start its grace");
    };
    let within_grace = started
        .checked_add(provenant_core::DurationMicros::from_seconds(9).unwrap())
        .unwrap();
    let resumed =
        observe_reservation_liveness(&slot, attempt, 1, None, within_grace, &grace).unwrap();
    assert!(matches!(
        &resumed,
        LeaseState::Unreachable { retry_at, .. } if *retry_at == first_deadline
    ));
    assert!(matches!(
        guard_destructive_worktree(
            "worktree remove",
            &tree(),
            protects_holder(None, Some(&resumed)),
            Some(attempt),
        ),
        Err(WorktreeError::CustodyRefused {
            code: DESTROYS_LIVE_WORK,
            holder: Some(seen),
            ..
        }) if seen == attempt
    ));

    let after_grace = started
        .checked_add(provenant_core::DurationMicros::from_seconds(10).unwrap())
        .unwrap();
    assert!(matches!(
        observe_reservation_liveness(&slot, attempt, 1, None, after_grace, &grace),
        Some(LeaseState::LapsedUnreachable { retry_at, .. }) if retry_at == first_deadline
    ));
}

#[test]
fn unreadable_or_missing_holder_observation_stays_protected() {
    assert!(protects_holder(None, None));
    assert!(protects_holder(Some(true), None));
    assert!(!protects_holder(Some(false), None));
    let default_grace = default_unreachable_grace();
    assert!(protects_holder(
        None,
        Some(&LeaseState::Unreachable {
            retry_at: WallTime::from_micros(1).unwrap(),
            setting: unreachable_setting_ref(&default_grace),
        })
    ));

    let temp = tempfile::tempdir().unwrap();
    let slot = Directory::open_anchor(temp.path()).unwrap();
    provenant_supervisor::custody::prepare_writer_observation(&slot).unwrap();
    std::fs::write(temp.path().join(UNREACHABLE_MARKER_FILE), b"not json").unwrap();
    let now = WallTime::from_unix_seconds(1_700_000_000).unwrap();
    assert!(matches!(
        observe_reservation_liveness(&slot, holder(9), 1, None, now, &default_grace),
        Some(LeaseState::Unreachable { .. })
    ));
}

#[test]
fn unreachable_grace_marker_is_scoped_to_holder_incarnation() {
    let temp = tempfile::tempdir().unwrap();
    let slot = Directory::open_anchor(temp.path()).unwrap();
    provenant_supervisor::custody::prepare_writer_observation(&slot).unwrap();
    let grace = Sourced {
        value: 10,
        source: Source::at(Layer::Host, "fixture"),
    };
    let now = WallTime::from_unix_seconds(1_700_000_000).unwrap();
    let attempt = holder(10);
    observe_reservation_liveness(&slot, attempt, 1, None, now, &grace).unwrap();
    let later = now
        .checked_add(provenant_core::DurationMicros::from_seconds(20).unwrap())
        .unwrap();
    assert!(matches!(
        observe_reservation_liveness(&slot, attempt, 2, None, later, &grace),
        Some(LeaseState::Unreachable { retry_at, .. })
            if retry_at == later.checked_add(provenant_core::DurationMicros::from_seconds(10).unwrap()).unwrap()
    ));
}

#[test]
fn destruction_guard_only_refuses_live_work() {
    let tree = tree();
    assert!(guard_destructive_worktree("worktree remove", &tree, false, Some(holder(5))).is_ok());
    let refused = guard_destructive_worktree("worktree remove", &tree, true, Some(holder(5)));
    assert!(matches!(
        refused,
        Err(WorktreeError::CustodyRefused {
            code: DESTROYS_LIVE_WORK,
            holder: Some(seen),
            ..
        }) if seen == holder(5)
    ));
}

#[test]
fn missing_host_config_defaults_to_warn() {
    let absent = std::path::Path::new("/provenant/no-such-host-state-fixture");
    assert_eq!(
        configured_sharing(absent, Path::new("/provenant/home"), Path::new("/project")).value,
        Sharing::Warn
    );
}

#[test]
fn unreachable_grace_reads_the_host_knob_and_preserves_its_source()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = crate::test_support::Fixture::new()?;
    let test = crate::worktrees::lifecycle_tests::TestContext::new(&fixture)?;
    let context = test.context();
    // The grace is read from the context's host state, the parent of the
    // project state directory, not from the binding roots.
    std::fs::write(
        context.host_state()?.join("config.toml"),
        "schema_version = 1\n[knobs]\n'domains.fabric.lanes.lease_unreachable_seconds' = 12\n",
    )?;

    let grace = configured_unreachable_grace(&context);
    assert_eq!(grace.value, 12);
    assert_eq!(grace.source.layer, Layer::Host);
    assert!(
        grace
            .source
            .origin
            .as_deref()
            .is_some_and(|origin| origin.contains("config.toml"))
    );
    Ok(())
}

#[test]
fn writer_occupancy_contention_keeps_the_durable_holder() {
    let path = Path::new("/fixture/lane/.provenant/state/writer.lock");
    let owner = holder(7);
    let timeout = provenant_supervisor::custody::CustodyError::Filesystem(FsError::LockTimeout {
        path: path.to_path_buf(),
        timeout: std::time::Duration::from_secs(1),
    });
    assert!(
        matches!(
            writer_occupancy_refusal(timeout, Some(owner), path),
            WorktreeError::WriterBusy { holder: Some(seen) } if seen == owner
        ),
        "a contended writer slot keeps the recorded holder"
    );
    let cancelled = provenant_supervisor::custody::CustodyError::Filesystem(FsError::Cancelled {
        path: path.to_path_buf(),
    });
    assert!(matches!(
        writer_occupancy_refusal(cancelled, None, path),
        WorktreeError::Cancelled
    ));
    let fenced = provenant_supervisor::custody::CustodyError::Fenced;
    assert!(matches!(
        writer_occupancy_refusal(fenced, None, path),
        WorktreeError::UnknownOccupancy { .. }
    ));
}
