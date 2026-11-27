use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn native_cleanup_targets_require_owned_identity_and_preserve_exclusions() {
    let identity = |pid: i64| ProcessIdentity {
        boot: "fixture".into(),
        pid,
        native_start: pid as u64,
        group_id: pid,
        session_id: pid,
    };
    let fact = |pid, parent_pid| crate::census::ProcessFact {
        identity: identity(pid),
        parent_pid,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let mut reused = fact(50, 1);
    reused.identity.native_start += 100;
    let mut zombie = fact(90, 1);
    zombie.zombie = true;
    let census = Census::from_observations(
        [
            fact(1, 0),
            fact(20, 1), // Owned descendant after reparenting and setsid.
            fact(30, 1), // Protected owner.
            fact(40, 30),
            reused,
            fact(70, 1), // Unrelated live process.
            fact(80, 1), // Previously spared descendant.
            zombie,
        ],
        [(60, crate::census::ObservationIssue::Io)],
        [],
        true,
        false,
        16,
    )
    .expect("valid census");
    let owned = [20, 40, 50, 60, 80, 90].map(identity).into();
    let targets = owned_cleanup_targets(
        &owned,
        &[identity(80)].into(),
        &census,
        &[identity(30)].into(),
    );
    assert_eq!(targets, [identity(20)]);
}

#[test]
fn reactor_waits_never_shrink_and_polls_never_block() {
    assert_eq!(poll_timeout_ms(Duration::ZERO), 0);
    for wait in [
        Duration::from_nanos(1),
        Duration::from_micros(999),
        Duration::from_millis(1),
        Duration::from_nanos(1_000_001),
        Duration::from_millis(250),
    ] {
        let rounded = i128::from(poll_timeout_ms(wait));
        assert!(rounded > 0, "{wait:?} became a nonblocking poll");
        assert!(
            rounded * 1_000_000 >= wait.as_nanos() as i128,
            "{wait:?} was cut short"
        );
    }
}

#[test]
fn cancellation_directory_events_do_not_request_a_process_census() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let watched = root.path().join("attempt");
    std::fs::create_dir(&watched)?;
    let directory = provenant_os::fs::Directory::open_anchor(&watched).map_err(io::Error::other)?;
    let mut reactor = GuardianReactor::install()?;
    reactor.watch_cancellation_directory(&directory)?;
    std::fs::write(watched.join("cancel.new"), b"request")?;
    std::fs::rename(watched.join("cancel.new"), watched.join("cancel.json"))?;
    reactor.wait(&[], Duration::ZERO)?;
    let event = reactor.take_events();
    assert!(event.cancel_changed);
    assert!(!event.cancel_watch_failed);
    assert!(!event.process_changed);
    assert!(!event.process_overflow);

    // Losing the watched pathname requests the cancellation polling
    // fallback without triggering a process census.
    std::fs::rename(&watched, root.path().join("moved-attempt"))?;
    reactor.wait(&[], Duration::ZERO)?;
    let event = reactor.take_events();
    assert!(event.cancel_changed);
    assert!(event.cancel_watch_failed);
    assert!(!event.process_changed);
    assert!(!event.process_overflow);
    Ok(())
}

/// kqueue reports an edge once. A wait whose caller discards its result,
/// as a pacing pause does, must leave that edge for the next taker (#3585).
#[test]
fn a_discarded_wait_keeps_every_drained_edge_for_the_next_take() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let directory =
        provenant_os::fs::Directory::open_anchor(root.path()).map_err(io::Error::other)?;
    let mut reactor = GuardianReactor::install()?;
    reactor.watch_cancellation_directory(&directory)?;
    std::fs::write(root.path().join("cancel.json"), b"request")?;
    let _ = reactor.wait(&[], Duration::ZERO);
    reactor.pause(Duration::ZERO);
    assert!(reactor.pending().cancel_changed);
    let event = reactor.take_events();
    assert!(event.cancel_changed);
    assert!(!event.supervisor_signal);
    assert!(!reactor.signalled());
    assert_eq!(reactor.take_events(), ReactorEvent::default());
    Ok(())
}

/// A supervisor signal stays visible after a loop takes its event, so a
/// later loop that never saw the edge still ends (#3585).
#[test]
fn a_supervisor_signal_stays_latched_after_its_event_is_taken() -> io::Result<()> {
    // EVFILT_SIGNAL records only process-directed signals, and one sent to
    // this process would reach every reactor in it. A child process runs
    // this test alone and signals itself.
    const CHILD: &str = "PROVENANT_TEST_LATCHED_SIGNAL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "process_os::provider::tests::a_supervisor_signal_stays_latched_after_its_event_is_taken",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .status()?;
        assert!(status.success(), "latched-signal child failed: {status}");
        return Ok(());
    }
    extern "C" fn absorb(_: libc::c_int) {}
    let mut reactor = GuardianReactor::install()?;
    // The reactor blocks SIGTERM on this thread only; the handler absorbs
    // delivery on the harness's other threads instead of the default exit.
    // SAFETY: a zeroed sigaction is an empty mask with no flags; the
    // handler set below is a valid async-signal-safe function.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = absorb as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: action is initialised and outlives the call; no previous
    // action is requested.
    if unsafe { libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: getpid has no preconditions and names this process.
    let pid = unsafe { libc::getpid() };
    // SAFETY: the signal targets this child process alone, whose SIGTERM
    // handler is installed above.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let _ = reactor.wait(&[], Duration::from_secs(5))?;
    assert!(reactor.signalled());
    assert!(reactor.take_events().supervisor_signal);
    assert!(reactor.signalled());
    assert_eq!(reactor.take_events(), ReactorEvent::default());
    assert!(reactor.signalled());
    Ok(())
}

#[test]
fn reactor_reports_pipe_work_only_for_readiness_including_eof() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let directory =
        provenant_os::fs::Directory::open_anchor(root.path()).map_err(io::Error::other)?;
    let mut reactor = GuardianReactor::install()?;
    reactor.watch_cancellation_directory(&directory)?;
    let (mut reader, mut writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    let watches = [Some(Watch::read(reader.as_fd())), None, None];
    assert!(!reactor.wait(&watches, Duration::ZERO)?);
    std::fs::write(root.path().join("checkpoint"), b"changed")?;
    assert!(!reactor.wait(&watches, Duration::ZERO)?);
    assert!(reactor.take_events().cancel_changed);
    writer.write_all(b"x")?;
    assert!(reactor.wait(&watches, Duration::ZERO)?);
    let mut byte = [0];
    reader.read_exact(&mut byte)?;
    let watches = [Some(Watch::read(reader.as_fd())), None, None];
    assert!(!reactor.wait(&watches, Duration::ZERO)?);
    drop(writer);
    assert!(reactor.wait(&watches, Duration::ZERO)?);
    assert_eq!(reader.read(&mut byte)?, 0);
    // A later wait with no pipe must not reuse its old readiness.
    assert!(!reactor.wait(&[], Duration::ZERO)?);
    Ok(())
}

#[test]
fn process_watch_failures_do_not_hide_later_pids() {
    let mut watched = BTreeSet::new();
    let mut exited = BTreeSet::new();
    let pids = BTreeSet::from([1, 2, 3, 4, 5]);
    let mut attempted = Vec::new();
    let result = register_process_watches(
        &mut watched,
        &mut exited,
        &pids,
        |pid| {
            attempted.push(pid);
            match pid {
                1 | 3 => Err(io::Error::from_raw_os_error(libc::EPERM)),
                2 => Err(io::Error::from_raw_os_error(libc::ESRCH)),
                5 => Err(io::Error::from_raw_os_error(libc::EACCES)),
                _ => Ok(()),
            }
        },
        |pid| pid == 3,
    )
    .unwrap();
    assert_eq!(attempted, vec![1, 2, 3, 4, 5]);
    assert_eq!(
        result,
        ProcessWatches {
            added: true,
            gone: true,
            failures: 2,
        }
    );
    assert!(watched.contains(&4));
}

/// An exit before (or racing) registration sends no kqueue event, so it
/// is reported as one, once per PID while the caller still requests it.
#[test]
fn a_pid_gone_at_registration_is_reported_once() {
    let mut watched = BTreeSet::new();
    let mut exited = BTreeSet::new();
    let pids = BTreeSet::from([7, 8]);
    let esrch = |pid| {
        if pid == 7 {
            Err(io::Error::from_raw_os_error(libc::ESRCH))
        } else {
            Ok(())
        }
    };
    // 8 registers, then is found gone: its exit may predate the filter.
    let first =
        register_process_watches(&mut watched, &mut exited, &pids, esrch, |pid| pid == 8).unwrap();
    assert_eq!(
        first,
        ProcessWatches {
            added: true,
            gone: true,
            failures: 0,
        }
    );
    let again =
        register_process_watches(&mut watched, &mut exited, &pids, esrch, |_| true).unwrap();
    assert_eq!(again, ProcessWatches::default());
    // Dropped by the caller, then requested again (a reused PID): new.
    register_process_watches(&mut watched, &mut exited, &BTreeSet::new(), esrch, |_| true).unwrap();
    let reused =
        register_process_watches(&mut watched, &mut exited, &pids, esrch, |_| false).unwrap();
    assert!(reused.gone && reused.added, "{reused:?}");
}

#[test]
fn provider_exec_is_refused_when_attribution_cannot_be_published()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let directory = provenant_os::fs::Directory::open_anchor(root.path())?;
    let lease = directory.try_lock(
        std::ffi::OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let marker = root.path().join("provider-executed");
    let argv: Vec<OsString> = [
        "/bin/sh".into(),
        "-c".into(),
        format!("printf started > '{}'", marker.display()).into(),
    ]
    .into_iter()
    .collect();
    let result = ProviderGroup::spawn(
        provenant_os::sys::raw_exec::SpawnSpec {
            argv: &argv,
            env: Some(&[]),
            cwd: root.path(),
            input: false,
            secret_input: None,
            inherited_lease: None,
            inherited_profile: None,
            inherited_ports: None,
            process_group: false,
            capture_stdout: false,
            capture_stderr: false,
        },
        &lease,
        &super::super::census::SystemProcessObserver::boot_identity()?,
        0,
        |_| Err(io::Error::other("injected durable reservation failure")),
    );
    assert!(
        result.is_err(),
        "failed reservation publication must reject spawn"
    );
    assert!(!marker.exists(), "provider executable must not start");
    Ok(())
}

#[test]
fn lane_descriptors_are_cloexec() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let directory = provenant_os::fs::Directory::open_anchor(root.path())?;
    assert!(provenant_os::sys::fs::is_cloexec(directory.handoff_file()));
    for name in [
        "writer.lock",
        "ports.lock",
        "custody.lock",
        "writer-probe.lock",
    ] {
        let lease = directory.try_lock(
            std::ffi::OsStr::new(name),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        assert!(provenant_os::sys::fs::is_cloexec(lease.handoff_file()));
    }
    Ok(())
}

#[test]
fn orphaned_provider_descendant_does_not_retain_writer_flock()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, CancelToken, Clock, WaitBudget};
    use provenant_os::{clock::SystemClock, fs::Directory};
    use provenant_records::custody::Scope;

    let root = tempfile::tempdir()?;
    let directory = Directory::open_anchor(root.path())?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let scope = Scope {
        attempt: AttemptId::from_bytes([2; 16]),
        generation: 1,
    };
    let writer = crate::fixture::acquire_writer(
        directory.try_clone()?,
        scope,
        WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
    )?;
    let lease = writer.lease.as_ref().ok_or("missing writer lease")?;
    // SAFETY: the lease owns this live fd; F_GETFD only reads its flags.
    let flags = unsafe { libc::fcntl(lease.handoff_file().as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    let argv: Vec<OsString> = ["/bin/sh", "-c", "/bin/sh -c 'printf R; read line'; :"]
        .into_iter()
        .map(Into::into)
        .collect();
    let env = [(OsString::from("PATH"), OsString::from("/bin:/usr/bin"))];
    let boot = super::super::census::SystemProcessObserver::boot_identity()?;
    let mut group = ProviderGroup::spawn(
        provenant_os::sys::raw_exec::SpawnSpec {
            argv: &argv,
            env: Some(&env),
            cwd: root.path(),
            input: true,
            secret_input: None,
            inherited_lease: None,
            inherited_profile: None,
            inherited_ports: None,
            process_group: false,
            capture_stdout: true,
            capture_stderr: false,
        },
        lease,
        &boot,
        0,
        |_| Ok(()),
    )?;
    let mut ready = [0];
    let stdout = group.stdout.as_mut().ok_or("missing stdout")?;
    let read = provenant_test_support::wait_for(
        "descendant readiness",
        Duration::from_secs(5),
        || match stdout.read(&mut ready) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
            other => Some(other),
        },
        Option::is_some,
    )?;
    if !matches!(read, Some(Ok(1))) {
        return Err(format!("descendant readiness: {read:?}").into());
    }
    assert_eq!(ready, [b'R']);
    // Close the guardian's copy and kill the provider. Its orphaned
    // exec'd descendant remains alive, but must not retain lane custody.
    writer.withdraw_unlaunched()?;
    let provider = group.child.as_mut().ok_or("missing provider")?;
    provider.kill()?;
    provider.wait()?;
    let retry = Scope {
        attempt: AttemptId::from_bytes([3; 16]),
        ..scope
    };
    let admit = |budget| {
        crate::fixture::acquire_writer(directory.try_clone()?, retry, budget, &cancel, &clock)
    };
    admit(WaitBudget::nonblocking(clock.mono_now()))?.withdraw_unlaunched()?;
    drop(group.stdin.take());
    let mut tail = Vec::new();
    let mut stdout = group.stdout.take().ok_or("missing stdout")?;
    let read = provenant_test_support::wait_for(
        "descendant exit",
        Duration::from_secs(5),
        || match stdout.read_to_end(&mut tail) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
            other => Some(other),
        },
        Option::is_some,
    )?;
    read.ok_or("missing descendant exit")??;
    admit(WaitBudget::cli(clock.mono_now(), 5)?)?.withdraw_unlaunched()?;
    Ok(())
}

#[test]
fn orphan_term_resumes_a_stopped_process_before_terminating_it()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::process::ExitStatusExt;
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = tempfile::tempdir()?;
    let ready = root.path().join("ready");
    let mut child = OwnedChild(
        Command::new("/bin/sh")
            .args(["-c", "printf ready > \"$1\"; read value", "orphan"])
            .arg(&ready)
            .stdin(Stdio::piped())
            .spawn()?,
    );
    provenant_test_support::wait_for(
        "orphan readiness",
        Duration::from_secs(3),
        || ready.exists(),
        |ready| *ready,
    )?;
    let ProcessObservation::Present(fact) =
        super::super::census::SystemProcessObserver.observe(i64::from(child.0.id()))
    else {
        return Err("orphan identity unavailable".into());
    };
    assert!(signal_build(&fact.identity, libc::SIGSTOP)?);
    assert!(signal_identity(&fact.identity, false));
    let status = provenant_test_support::wait_for(
        "stopped orphan termination",
        Duration::from_secs(3),
        || child.0.try_wait(),
        |result| !matches!(result, Ok(None)),
    )??
    .ok_or("missing orphan status")?;
    assert_eq!(status.signal(), Some(libc::SIGTERM));
    Ok(())
}

#[test]
fn provider_creates_private_state_regardless_of_parent_umask() {
    let root = tempfile::tempdir().unwrap();
    let directory = provenant_os::fs::Directory::open_anchor(root.path()).unwrap();
    let lease = directory
        .try_lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .unwrap();
    let argv: Vec<OsString> = ["/bin/sh", "-c", "umask; mkdir made && : > made.txt"]
        .into_iter()
        .map(Into::into)
        .collect();
    let env = [(OsString::from("PATH"), OsString::from("/bin:/usr/bin"))];
    let boot = super::super::census::SystemProcessObserver::boot_identity().unwrap();
    let mut group = ProviderGroup::spawn(
        provenant_os::sys::raw_exec::SpawnSpec {
            argv: &argv,
            env: Some(&env),
            cwd: root.path(),
            input: false,
            secret_input: None,
            inherited_lease: None,
            inherited_profile: None,
            inherited_ports: None,
            process_group: false,
            capture_stdout: true,
            capture_stderr: false,
        },
        &lease,
        &boot,
        0,
        |_| Ok(()),
    )
    .unwrap();
    assert!(group.child.as_mut().unwrap().wait().unwrap().success());
    let mut stdout = String::new();
    group
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert_eq!(stdout.trim(), "0077");
    let mode = |name: &str| {
        std::fs::metadata(root.path().join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("made"), 0o700);
    assert_eq!(mode("made.txt"), 0o600);
}

#[test]
fn priority_failures_degrade_and_preserve_evidence() {
    assert_eq!(
        priority_with_ops(10, || Ok(5), |_| Err(libc::EPERM)),
        (Some(5), Some(libc::EPERM))
    );
    assert_eq!(
        priority_with_ops(10, || Ok(15), |_| panic!("must not raise priority")),
        (Some(15), None)
    );
    assert_eq!(
        priority_with_ops(10, || Err(libc::EACCES), |_| panic!("unknown priority")),
        (None, Some(libc::EACCES))
    );
    assert_eq!(
        priority_with_ops(5, || Ok(-1), |_| Err(libc::EPERM)),
        (Some(-1), Some(libc::EPERM))
    );
}

#[test]
fn provider_and_descendant_inherit_priority() {
    let root = tempfile::tempdir().unwrap();
    let directory = provenant_os::fs::Directory::open_anchor(root.path()).unwrap();
    let lease = directory
        .try_lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .unwrap();
    let argv: Vec<OsString> = [
        "/bin/sh",
        "-c",
        "ps -o nice= -p $$; /bin/sh -c 'ps -o nice= -p $$'",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let env = [(OsString::from("PATH"), OsString::from("/bin:/usr/bin"))];
    let boot = super::super::census::SystemProcessObserver::boot_identity().unwrap();
    let mut group = ProviderGroup::spawn(
        provenant_os::sys::raw_exec::SpawnSpec {
            argv: &argv,
            env: Some(&env),
            cwd: root.path(),
            input: false,
            secret_input: None,
            inherited_lease: None,
            inherited_profile: None,
            inherited_ports: None,
            process_group: false,
            capture_stdout: true,
            capture_stderr: false,
        },
        &lease,
        &boot,
        10,
        |_| Ok(()),
    )
    .unwrap();
    assert!(group.child.as_mut().unwrap().wait().unwrap().success());
    let mut stdout = String::new();
    group
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let values: Vec<i32> = stdout
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    assert_eq!(values.len(), 2);
    assert_eq!(Some(values[0]), group.priority);
    if group.priority_error.is_none() {
        assert!(values[0] >= 10);
    }
    assert_eq!(values[0], values[1]);
}
