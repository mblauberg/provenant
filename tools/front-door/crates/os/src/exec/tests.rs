use super::*;
use std::sync::Mutex;
struct Decoder(Mutex<Vec<Vec<u8>>>);

#[test]
fn detached_worker_refuses_to_relaunch_a_test_harness() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let argv = [
        std::env::current_exe()?.into_os_string(),
        OsString::from("internal"),
        OsString::from("settle"),
    ];
    assert!(matches!(
        spawn_detached(&argv, temp.path()),
        Err(ExecError::TestHarness)
    ));
    assert!(!is_test_harness(Path::new(
        "/Users/me/.local/bin/provenant"
    )));
    assert!(!is_test_harness(Path::new("target/debug/provenant")));
    Ok(())
}

#[test]
fn detached_worker_refuses_near_the_process_limit() {
    let near = |used, limit| check_process_budget(Ok(ProcessBudget { used, limit }));
    assert!(matches!(
        near(2601, 2666),
        Err(ExecError::ProcessBudget {
            used: 2601,
            limit: 2666
        })
    ));
    assert!(matches!(
        near(2400, 2666),
        Err(ExecError::ProcessBudget { .. })
    ));
    assert!(near(2399, 2666).is_ok());
    assert!(near(100, 2666).is_ok());
    assert!(check_process_budget(Ok(ProcessBudget::UNLIMITED)).is_ok());
    // An unreadable budget does not stop a worker the kernel would allow.
    assert!(check_process_budget(Err(io::Error::other("probe"))).is_ok());
}

#[test]
fn inherited_lane_lease_survives_the_callers_handle() -> Result<(), Box<dyn std::error::Error>> {
    use crate::fs::LockMode;
    let temp = tempfile::tempdir()?;
    let lease = crate::provider_fence::acquire(temp.path(), "codex", LockMode::Shared)?;
    let reaper = Reaper::new(1)?;
    let permit = reaper.reserve().ok_or("missing executor permit")?;
    let argv = [
        OsString::from("/bin/sh"),
        OsString::from("-c"),
        OsString::from("read reply"),
    ];
    let mut child = RawChild::spawn(
        permit,
        crate::sys::raw_exec::SpawnSpec {
            argv: &argv,
            env: None,
            cwd: temp.path(),
            input: true,
            secret_input: None,
            inherited_lease: Some(&lease),
            inherited_ports: None,
            process_group: false,
            capture_stdout: false,
            capture_stderr: false,
        },
    )?;
    drop(lease);
    assert!(matches!(
        crate::provider_fence::acquire(temp.path(), "codex", LockMode::Exclusive),
        Err(crate::fs::FsError::LockTimeout { .. })
    ));
    child
        .stdin
        .take()
        .ok_or("missing child stdin")?
        .write_all(b"finish\n")?;
    // The OS lock boundary provides a bounded wait, including scheduler delay.
    let clock = crate::clock::SystemClock::new();
    use provenant_core::Clock;
    let _available = crate::provider_fence::wait(
        temp.path(),
        "codex",
        LockMode::Exclusive,
        provenant_core::WaitBudget::cli(clock.mono_now(), 5)?,
        &CancelToken::new(),
        &clock,
    )?;
    let _ = child.observe()?;
    Ok(())
}
#[test]
fn inherited_port_block_outlives_the_callers_handle() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let plan = crate::lane_ports::Plan {
        state: temp.path().to_owned(),
        base: 30000,
        size: 16,
        blocks: 1,
    };
    let lease = plan.try_acquire()?.ok_or("no block")?;
    let reaper = Reaper::new(1)?;
    let permit = reaper.reserve().ok_or("missing executor permit")?;
    let argv = [
        OsString::from("/bin/sh"),
        OsString::from("-c"),
        OsString::from("read reply"),
    ];
    let mut child = RawChild::spawn(
        permit,
        crate::sys::raw_exec::SpawnSpec {
            argv: &argv,
            env: None,
            cwd: temp.path(),
            input: true,
            secret_input: None,
            inherited_lease: None,
            inherited_ports: Some(lease.guard()),
            process_group: false,
            capture_stdout: false,
            capture_stderr: false,
        },
    )?;
    // The guardian dying drops its handle; the running process keeps the block.
    drop(lease);
    assert!(plan.try_acquire()?.is_none());
    child
        .stdin
        .take()
        .ok_or("missing child stdin")?
        .write_all(b"finish\n")?;
    let _ = child.observe()?;
    for _ in 0..500 {
        if plan.try_acquire()?.is_some() {
            return Ok(());
        }
        crate::sys::fs::wait(std::time::Duration::from_millis(10));
    }
    Err("block never released after the process exited".into())
}
impl FrameSink for Decoder {
    fn try_frame(&self, bytes: &[u8]) -> Result<(), QueueLoss> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
}
#[test]
fn decoder_sees_terminal_beyond_retention_and_refuses_oversized_frame() {
    let sink = Decoder(Mutex::new(Vec::new()));
    let mut frames = Frames::default();
    let mut capture = CapturedStream::default();
    frames.decode(&vec![b'x'; FRAME_LIMIT + 1], Some(&sink), &mut capture);
    frames.decode(b"\nterminal\n", Some(&sink), &mut capture);
    assert_eq!(capture.oversized_frames, 1);
    assert_eq!(*sink.0.lock().unwrap(), vec![b"terminal".to_vec()]);
}
#[test]
fn tee_has_a_fixed_queue_and_reports_full_and_closed() {
    let (sink, receiver) = tee_queue(1).unwrap();
    assert_eq!(
        sink.try_send(&vec![b'x'; FRAME_LIMIT + 1]),
        Err(QueueLoss::Full)
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(sink.try_send(b"one"), Ok(()));
    assert_eq!(sink.try_send(b"two"), Err(QueueLoss::Full));
    assert_eq!(receiver.try_recv().unwrap(), b"one");
    drop(receiver);
    assert_eq!(sink.try_send(b"three"), Err(QueueLoss::Closed));
    assert!(tee_queue(0).is_err());
    assert!(tee_queue(1025).is_err());
}
#[test]
fn reaper_reserves_before_spawn_and_releases_unused_admission() {
    let reaper = Reaper::new(1).unwrap();
    let first = reaper.reserve().unwrap();
    assert!(reaper.reserve().is_none());
    assert_eq!(reaper.occupied(), 1);
    drop(first);
    assert!(reaper.reserve().is_some());
}

#[test]
fn reaper_worker_is_shared_between_executor_states() {
    let first = Reaper::new(1).unwrap();
    let second = Reaper::new(2).unwrap();
    assert!(first.shares_process_worker(&second));
}

#[cfg(any(test, feature = "fixture"))]
#[test]
fn post_spawn_setup_failure_kills_child_and_releases_unknown_wait_slot() {
    use std::ffi::OsString;

    let reaper = Reaper::new_paused(1).unwrap();
    let permit = reaper.reserve().unwrap();
    let argv = [OsString::from("/bin/sleep"), OsString::from("5")];
    let mut pid = None;
    let spawn = crate::sys::raw_exec::OwnedPid::spawn_with_setup(
        permit,
        crate::sys::raw_exec::SpawnSpec {
            argv: &argv,
            env: None,
            cwd: Path::new("/"),
            input: false,
            secret_input: None,
            inherited_lease: None,
            inherited_ports: None,
            process_group: false,
            capture_stdout: false,
            capture_stderr: false,
        },
        |child| {
            pid = child.pid_for_test();
            Err(io::Error::other("injected nonblocking setup failure"))
        },
    );
    assert!(spawn.is_err());
    let pid = pid.expect("child was spawned before setup failed");
    assert_eq!(reaper.occupied(), 1);

    let status = crate::sys::raw_exec::reap_pid_for_test(pid).unwrap();
    assert!(libc::WIFSIGNALED(status));
    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
    assert!(crate::sys::raw_exec::pid_is_absent_for_test(pid).unwrap());

    reaper.resume();
    assert_eq!(
        reaper.wait_for_occupied(0, Duration::from_secs(5)).unwrap(),
        0
    );
    assert_eq!(reaper.unknown_outcomes(), 1);
    assert!(
        reaper.reserve().is_some(),
        "admission slot was not released"
    );
}

#[test]
fn decoder_and_filter_failures_are_observable_without_fabricating_frames() {
    struct ClosedDecoder;
    impl FrameSink for ClosedDecoder {
        fn try_frame(&self, _: &[u8]) -> Result<(), QueueLoss> {
            Err(QueueLoss::Closed)
        }
    }
    let mut frames = Frames::default();
    let mut capture = CapturedStream::default();
    frames.decode(b"terminal\n", Some(&ClosedDecoder), &mut capture);
    assert_eq!(capture.decoder_lost_frames, 1);

    struct FailedFilter;
    impl CaptureFilter for FailedFilter {
        fn filter(&mut self, _: &[u8]) -> Result<Vec<u8>, FilterError> {
            Err(FilterError)
        }
    }
    let decoder = Decoder(Mutex::new(Vec::new()));
    let mut filter = FailedFilter;
    let mut pump = Pump {
        policy: StreamPolicy::Capture { limit: 32 },
        decoder: Some(&decoder),
        filter: Some(&mut filter),
        frames: Frames::default(),
    };
    let mut pipe = Some(io::Cursor::new(b"terminal\n"));
    let mut capture = CapturedStream::default();
    pump.read(&mut pipe, &mut capture);
    pump.read(&mut pipe, &mut capture);
    assert!(pipe.is_none());
    assert!(capture.filter_failed);
    assert_eq!(capture.total_bytes, 9);
    assert!(capture.bytes.is_empty());
    assert_eq!(*decoder.0.lock().unwrap(), vec![b"terminal".to_vec()]);
}

#[test]
fn pipe_read_failure_is_observable_and_closes_the_failed_stream() {
    struct FailedRead;
    impl Read for FailedRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }
    let mut pump = Pump {
        policy: StreamPolicy::default(),
        decoder: None,
        filter: None,
        frames: Frames::default(),
    };
    let mut pipe = Some(FailedRead);
    let mut capture = CapturedStream::default();
    pump.read(&mut pipe, &mut capture);
    assert!(pipe.is_none());
    assert!(capture.read_failed);
    assert_eq!(capture.total_bytes, 0);
    assert!(capture.bytes.is_empty());
}
#[test]
fn bounds_and_pre_cancel_refuse_without_spawn() {
    let executor = SystemExecutor::new(1).unwrap();
    let argv = ["/must/not/spawn".into()];
    let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::ZERO);
    options.stdout = StreamPolicy::Capture {
        limit: MAX_CAPTURE_LIMIT + 1,
    };
    assert!(matches!(
        executor.execute(options),
        Err(ExecError::InvalidLimit)
    ));
    let token = CancelToken::new();
    token.cancel();
    let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::ZERO);
    options.cancel = Some(&token);
    assert!(matches!(
        executor.execute(options),
        Err(ExecError::Cancelled)
    ));
    assert!(matches!(
        executor.execute(ExecOptions::new(&[], Path::new("/"), Duration::ZERO)),
        Err(ExecError::EmptyArgv)
    ));
}

#[test]
fn secret_input_arrives_by_descriptor_with_eof_and_no_token_environment() {
    let root = tempfile::tempdir().unwrap();
    let argv = [
        OsString::from("/bin/sh"),
        OsString::from("-c"),
        OsString::from(
            "test -z \"${CLAUDE_CODE_OAUTH_TOKEN+x}\" && exec /bin/cat /dev/fd/$CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        ),
    ];
    let env = vec![("HOME".into(), root.path().as_os_str().to_owned())];
    let secret = SecretInput {
        descriptor_env: "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR".into(),
        bytes: b"synthetic-oauth-token".to_vec(),
    };
    let mut options = ExecOptions::new(&argv, root.path(), Duration::from_secs(2));
    options.env = Some(&env);
    options.secret_input = Some(&secret);
    let outcome = SystemExecutor::new(1).unwrap().execute(options).unwrap();
    assert_eq!(outcome.termination, Termination::Exit(0));
    assert!(!outcome.timed_out);
    assert_eq!(outcome.stdout.bytes, secret.bytes);
    assert!(outcome.stderr.bytes.is_empty());
}

// Supervision deadlines use the operating system's monotonic clock, as in exec.rs.
#[test]
#[allow(clippy::disallowed_methods)]
fn timeout_and_cancel_kill_the_whole_process_group() -> Result<(), Box<dyn std::error::Error>> {
    use crate::sys::raw_exec::pid_is_absent_for_test;
    let executor = SystemExecutor::new(1)?;
    let temp = tempfile::tempdir()?;
    for cancel in [false, true] {
        let pid_file = temp.path().join(format!("grandchild-{cancel}.pid"));
        let script = format!("/bin/sleep 60 & echo $! > '{}'; wait", pid_file.display());
        let argv = [
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from(script),
        ];
        let token = CancelToken::new();
        let canceller = token.clone();
        let timeout = Duration::from_secs(if cancel { 60 } else { 1 });
        let mut options = ExecOptions::new(&argv, temp.path(), timeout);
        options.process_group = true;
        options.cancel = Some(&token);
        let outcome = std::thread::scope(|scope| {
            if cancel {
                scope.spawn(|| {
                    let stop = Instant::now() + Duration::from_secs(10);
                    while !pid_file.exists() && Instant::now() < stop {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    canceller.cancel();
                });
            }
            executor.execute(options)
        })?;
        if cancel {
            assert!(outcome.cancelled, "cancel must reach the child");
        } else {
            assert!(outcome.timed_out, "timeout must reach the child");
        }
        let pid: u32 = std::fs::read_to_string(&pid_file)?.trim().parse()?;
        // The grandchild is reparented and reaped asynchronously after the group dies.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_is_absent_for_test(pid)? {
            assert!(Instant::now() < deadline, "grandchild {pid} survived");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)]
fn normal_and_failed_exit_clean_the_process_group_before_reaping()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::sys::raw_exec::pid_is_absent_for_test;
    let executor = SystemExecutor::new(1)?;
    let temp = tempfile::tempdir()?;
    for code in [0, 7] {
        let pid_file = temp.path().join(format!("leftover-{code}.pid"));
        let script = format!(
            "/bin/sleep 60 & echo $! > '{}'; exit {code}",
            pid_file.display()
        );
        let argv = [OsString::from("/bin/sh"), "-c".into(), script.into()];
        let mut options = ExecOptions::new(&argv, temp.path(), Duration::from_secs(5));
        options.cleanup_process_group = true;
        let outcome = executor.execute(options)?;
        assert_eq!(outcome.termination, Termination::Exit(code));
        assert!(!outcome.drain_truncated && !outcome.timed_out);
        let pid = std::fs::read_to_string(pid_file)?.trim().parse()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_is_absent_for_test(pid)? {
            assert!(
                Instant::now() < deadline,
                "group member survived normal exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}
