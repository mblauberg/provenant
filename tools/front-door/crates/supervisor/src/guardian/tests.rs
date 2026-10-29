use super::*;
use crate::census::{Census, ProcessFact};
use std::collections::BTreeSet;

#[test]
fn settlement_lock_clone_failure_retains_exclusion_and_returns_reconcile_hint() {
    use provenant_core::{AttemptId, Clock, RunId, WaitBudget};
    use provenant_os::{
        clock::SystemClock,
        fs::{FsError, LockMode},
    };
    let root = tempfile::tempdir().unwrap();
    let directory = Directory::open_anchor(root.path()).unwrap();
    let clock = SystemClock;
    let scope = Scope {
        run: RunId::from_bytes([1; 16]),
        attempt: AttemptId::from_bytes([2; 16]),
        generation: 1,
    };
    let mut writer = crate::fixture::acquire_writer(
        directory.try_clone().unwrap(),
        scope,
        WaitBudget::nonblocking(clock.mono_now()),
        &CancelToken::new(),
        &clock,
    )
    .unwrap();
    let before = writer
        .directory
        .read_bounded(OsStr::new("reservation.json"), RECORD_LIMIT)
        .unwrap();
    let error = settlement_recovery(
        &mut writer.lease,
        writer.directory.try_clone().unwrap(),
        Err(FsError::Io {
            operation: "clone directory",
            path: root.path().into(),
            source: io::Error::from_raw_os_error(libc::EMFILE),
        }),
        &writer.reservation,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("provenant reconcile --id {}", scope.run))
    );
    assert!(writer.lease.is_some());
    assert!(matches!(
        directory.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive),
        Err(FsError::LockTimeout { .. })
    ));
    assert_eq!(
        writer
            .directory
            .read_bounded(OsStr::new("reservation.json"), RECORD_LIMIT)
            .unwrap(),
        before
    );
    writer.withdraw_unlaunched().unwrap();
}

#[test]
fn terminal_retries_wait_until_the_holder_can_publish() {
    use std::cell::{Cell, RefCell};
    let elapsed = Cell::new(Duration::ZERO);
    let attempts = RefCell::new(Vec::new());
    let delays = RefCell::new(Vec::new());
    assert!(retry_terminal(
        || {
            attempts.borrow_mut().push(elapsed.get());
            elapsed.get() >= Duration::from_secs(35)
        },
        |delay| {
            delays.borrow_mut().push(delay);
            elapsed.set(elapsed.get() + delay);
        },
    ));
    assert_eq!(
        *attempts.borrow(),
        [
            Duration::ZERO,
            Duration::from_secs(5),
            Duration::from_secs(35),
        ]
    );
    assert_eq!(
        *delays.borrow(),
        [Duration::from_secs(5), Duration::from_secs(30),]
    );
}

#[test]
fn terminal_retry_exhaustion_keeps_the_failure_bounded() {
    use std::cell::{Cell, RefCell};
    let attempts = Cell::new(0);
    let delays = RefCell::new(Vec::new());
    assert!(!retry_terminal(
        || {
            attempts.set(attempts.get() + 1);
            false
        },
        |delay| delays.borrow_mut().push(delay),
    ));
    assert_eq!(attempts.get(), 4);
    assert_eq!(
        *delays.borrow(),
        [
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(120),
        ]
    );
}

#[test]
fn successful_terminal_publication_does_not_wait() {
    assert!(retry_terminal(
        || true,
        |_| panic!("a successful terminal publication must not wait"),
    ));
}

#[test]
fn handoff_round_trips_one_mib_stdin_with_claude_argv_and_environment() {
    let message = LaunchMessage {
        priority: 0,
        schema_version: 1,
        scope: Scope {
            run: provenant_core::RunId::from_bytes([1; 16]),
            attempt: provenant_core::AttemptId::from_bytes([2; 16]),
            generation: 1,
        },
        resolved_route: "claude/claude-sonnet-4-6@high".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([3; 32]),
        task: provenant_core::TaskId::from_bytes([4; 16]),
        requester: provenant_core::ActorId::from_bytes([5; 16]),
        binding: StoredWriterBinding::ReadOnly,
        executable_snapshot: ExecutableSnapshot {
            device: 1,
            inode: 2,
            length: 1024,
            modified_seconds: 1,
            modified_nanoseconds: 0,
        },
        protected_commands: vec!["provenant".into(), "git".into()],
        argv: [
            "/fixture/claude/bin/claude",
            "-p",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--permission-prompts",
            "none",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            "01234567-89ab-4cde-8f01-23456789abcd",
            "--permission-mode",
            "dontAsk",
            "--tools",
            "Read,Grep,Glob",
            "--model",
            "claude-sonnet-4-6",
            "--effort",
            "high",
            "--mcp-config",
            "/fixture/attempt/provider-state/fabric-mcp.json",
        ]
        .map(|arg| arg.as_bytes().to_vec())
        .to_vec(),
        environment: [
            ("HOME", "/fixture/home"),
            ("USER", "fixture"),
            ("PATH", "/usr/bin:/bin"),
            ("TMPDIR", "/fixture/attempt/tmp/candidate-0"),
            ("LANG", "en_US.UTF-8"),
            ("CLAUDE_CODE_TMPDIR", "/fixture/attempt/tmp/candidate-0"),
            ("DISABLE_AUTOUPDATER", "1"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("CLAUDE_CONFIG_DIR", "/fixture/home/.claude"),
        ]
        .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
        .to_vec(),
        cwd: b"/fixture/repo".to_vec(),
        stdin: vec![0xff; 1024 * 1024],
        secret_input: Some(provenant_os::exec::SecretInput {
            descriptor_env: "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR".into(),
            bytes: b"fixture-token".to_vec(),
        }),
        holder_context: Vec::new(),
        capture: CapturePlan::default(),
    };
    let payload = serde_json::to_vec(&message).unwrap();
    let (mut sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    let timeout = Duration::from_secs(10);
    let sending = std::thread::spawn(move || {
        crate::process_os::handoff::send(&mut sender, &payload, timeout, &CancelToken::new())
    });
    let received = crate::process_os::handoff::receive(&mut receiver, timeout);
    sending.join().unwrap().unwrap();
    let received = received.unwrap();
    let decoded: LaunchMessage = decode_v1(received.as_slice(), LAUNCH_LIMITS).unwrap();
    assert_eq!(decoded.stdin, message.stdin);
    assert_eq!(decoded.argv, message.argv);
    assert_eq!(decoded.environment, message.environment);
}

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
            .try_lock(
                OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )
            .unwrap();
        let inherited = lease.handoff_file().try_clone().unwrap();
        let expected = provenant_os::fs::Metadata::from_std(inherited.metadata().unwrap());
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
        .try_lock(
            OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .unwrap();
    let expected = provenant_os::fs::Metadata::from_std(lease.handoff_file().metadata().unwrap());
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

#[test]
fn published_disk_admission_refusal_names_figures_and_repair() {
    let reason = super::admission_refusal(&super::CustodyError::AdmissionDisk {
        free: 99,
        floor: 100,
    });
    assert_eq!(
        reason,
        "insufficient disk space: 99 bytes available, gc.free_floor 100 bytes; run `provenant gc --apply`"
    );
}

#[test]
fn admission_configuration_and_guardian_failures_are_named() {
    let configuration = super::admission_refusal(&super::CustodyError::AdmissionBuildSettings);
    assert!(configuration.contains("domains.fabric.lanes.max_concurrent_builds"));
    let guardian = super::admission_refusal(&super::CustodyError::AdmissionGuardianIdentity);
    assert!(guardian.contains("guardian process identity unavailable"));
}
