use super::*;
use std::sync::Mutex;
struct Decoder(Mutex<Vec<Vec<u8>>>);
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

#[cfg(feature = "fixture")]
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
