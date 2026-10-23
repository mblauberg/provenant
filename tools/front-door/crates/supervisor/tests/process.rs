use provenant_core::CancelToken;
use provenant_supervisor::{exec::*, fixture};
use std::error::Error;
use std::ffi::OsString;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Decoder(Mutex<Vec<Vec<u8>>>);
impl FrameSink for Decoder {
    fn try_frame(&self, bytes: &[u8]) -> std::result::Result<(), QueueLoss> {
        self.0
            .lock()
            .map_err(|_| QueueLoss::Closed)?
            .push(bytes.to_vec());
        Ok(())
    }
}

fn read_byte(stream: &mut UnixStream, expected: u8) -> io::Result<()> {
    if fixture::fixture_read_byte(stream, Duration::from_secs(5))? == expected {
        Ok(())
    } else {
        Err(io::Error::from(io::ErrorKind::InvalidData))
    }
}
fn fixture_arg(path: &Path) -> OsString {
    path.as_os_str().to_owned()
}

fn suite() -> Result<()> {
    let root = tempfile::Builder::new().prefix("s1-").tempdir_in("/tmp")?;
    let env = fixture::environment(root.path());
    let executor = SystemExecutor::new(4)?;
    let args = fixture::argv("bytes")?;
    let decoder = Decoder(Mutex::new(Vec::new()));
    let (sink, _receiver) = tee_queue(1)?;
    let mut options = ExecOptions::new(&args, root.path(), Duration::from_secs(3));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Tee {
        limit: 3,
        sink: &sink,
    };
    options.stdout_decoder = Some(&decoder);
    let outcome = executor.execute(options)?;
    assert_eq!(outcome.termination, Termination::Exit(0));
    assert_eq!(outcome.stdout.bytes, b"a\0b");
    assert_eq!(outcome.stdout.total_bytes, 14);
    assert!(outcome.stdout.truncated);
    assert_eq!(outcome.stderr.bytes, b"err\r\n");
    assert!(!outcome.timed_out);
    assert!(
        decoder
            .0
            .lock()
            .map_err(|_| io::Error::other("decoder mutex poisoned"))?
            .iter()
            .any(|frame| frame == b"terminal")
    );
    let bytes = vec![0x9a; 256 * 1024];
    let args = fixture::argv("echo")?;
    let mut options = ExecOptions::new(&args, root.path(), Duration::from_secs(5));
    options.env = Some(&env);
    options.stdin = Some(&bytes);
    let outcome = executor.execute(options)?;
    assert_eq!(outcome.stdout.bytes, bytes);
    assert_eq!(outcome.stderr.bytes, b"done\r\n");

    let args = fixture::argv("flood")?;
    let mut options = ExecOptions::new(&args, root.path(), Duration::from_secs(5));
    options.env = Some(&env);
    options.stderr = StreamPolicy::Discard;
    options.stdout = StreamPolicy::Capture { limit: 128 };
    let outcome = executor.execute(options)?;
    assert_eq!(outcome.stdout.bytes.len(), 128);
    assert_eq!(outcome.stdout.total_bytes, 262144);
    assert_eq!(outcome.stderr.total_bytes, 0);

    let args = fixture::argv("blocked")?;
    let mut options = ExecOptions::new(&args, root.path(), Duration::from_secs(1));
    options.env = Some(&env);
    options.terminate_grace = Duration::from_millis(20);
    let outcome = executor.execute(options)?;
    assert!(outcome.timed_out);
    assert_eq!(outcome.termination, Termination::Signal(9));

    tee_loss_does_not_stall_capture(&executor, root.path(), &env)?;
    cancellation_is_scoped_to_the_owned_child(root.path(), &env, false)?;
    cancellation_is_scoped_to_the_owned_child(root.path(), &env, true)?;
    natural_exit_beats_pipe_drain_expiry(root.path(), &env)?;
    reaper_transfers_and_polls_fairly(root.path())?;
    Ok(())
}

fn tee_loss_does_not_stall_capture(
    executor: &SystemExecutor,
    root: &Path,
    env: &[(OsString, OsString)],
) -> Result<()> {
    let args = fixture::argv("flood")?;
    let (sink, _receiver) = tee_queue(1)?;
    let mut options = ExecOptions::new(&args, root, Duration::from_secs(5));
    options.env = Some(env);
    options.stderr = StreamPolicy::Discard;
    options.stdout = StreamPolicy::Tee {
        limit: 128,
        sink: &sink,
    };
    let full = executor.execute(options)?;
    assert_eq!(full.termination, Termination::Exit(0));
    assert_eq!(full.stdout.total_bytes, 262144);
    assert_eq!(full.stdout.bytes.len(), 128);
    assert!(full.stdout.truncated);
    assert!(full.stdout.sink_lost_bytes > 0);
    assert!(!full.stdout.sink_failed);

    let (closed_sink, receiver) = tee_queue(1)?;
    drop(receiver);
    let mut options = ExecOptions::new(&args, root, Duration::from_secs(5));
    options.env = Some(env);
    options.stderr = StreamPolicy::Discard;
    options.stdout = StreamPolicy::Tee {
        limit: 128,
        sink: &closed_sink,
    };
    let closed = executor.execute(options)?;
    assert_eq!(closed.termination, Termination::Exit(0));
    assert_eq!(closed.stdout.total_bytes, 262144);
    assert!(closed.stdout.sink_lost_bytes > 0);
    assert!(closed.stdout.sink_failed);
    assert!(!closed.timed_out && !closed.cancelled);
    Ok(())
}

fn cancellation_is_scoped_to_the_owned_child(
    root: &Path,
    env: &[(OsString, OsString)],
    exhaust_reap: bool,
) -> Result<()> {
    let socket = root.join(if exhaust_reap {
        "pending.sock"
    } else {
        "cancel.sock"
    });
    let listener = UnixListener::bind(&socket)?;
    let token = CancelToken::new();
    let helper_token = token.clone();
    let helper = thread::spawn(move || -> io::Result<UnixStream> {
        let mut stream = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
        read_byte(&mut stream, b'R')?;
        helper_token.cancel();
        Ok(stream)
    });
    let args = fixture::argv_with_args("socket-blocked", &[fixture_arg(&socket)])?;
    let executor = SystemExecutor::new(1)?;
    let mut options = ExecOptions::new(&args, root, Duration::from_secs(5));
    options.env = Some(env);
    options.cancel = Some(&token);
    options.terminate_grace = Duration::from_millis(25);
    options.reap_timeout = if exhaust_reap {
        Duration::ZERO
    } else {
        Duration::from_secs(1)
    };
    let result = executor.execute(options);
    let held_peer = helper
        .join()
        .map_err(|_| io::Error::other("cancellation handshake thread panicked"))??;
    drop(held_peer);
    if exhaust_reap {
        assert!(matches!(result, Err(ExecError::CleanupPending)));
        // The transferred child must be reaped and its reserved admission freed.
        let args = fixture::argv("exit")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let mut options = ExecOptions::new(&args, root, Duration::from_secs(1));
            options.env = Some(env);
            match executor.execute(options) {
                Err(ExecError::Busy) if std::time::Instant::now() < deadline => thread::yield_now(),
                value => {
                    assert_eq!(value?.termination, Termination::Exit(0));
                    break;
                }
            }
        }
        return Ok(());
    }
    let outcome = result?;
    assert!(outcome.cancelled);
    assert!(!outcome.timed_out);
    assert_eq!(outcome.termination, Termination::Signal(9));
    Ok(())
}

fn natural_exit_beats_pipe_drain_expiry(root: &Path, env: &[(OsString, OsString)]) -> Result<()> {
    let held_path = root.join("held.sock");
    let exit_path = root.join("parent-exit.sock");
    let held_listener = UnixListener::bind(&held_path)?;
    let exit_listener = UnixListener::bind(&exit_path)?;
    let args = fixture::argv_with_args(
        "exit-with-descendant",
        &[fixture_arg(&held_path), fixture_arg(&exit_path)],
    )?;
    let root = root.to_path_buf();
    let env = env.to_vec();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = (|| {
            let mut options = ExecOptions::new(&args, &root, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 32 };
            options.reap_timeout = Duration::from_millis(50);
            SystemExecutor::new(1)?.execute(options)
        })();
        let _ = sender.send(result);
    });
    let mut held = fixture::fixture_accept(&held_listener, Duration::from_secs(5))?;
    read_byte(&mut held, b'R')?;
    let mut parent_exit = fixture::fixture_accept(&exit_listener, Duration::from_secs(5))?;
    read_byte(&mut parent_exit, b'E')?;
    // Execution must finish while the independent-session descendant still
    // holds stdout. Release it only after the bounded result wait, even on failure.
    let result = receiver.recv_timeout(Duration::from_secs(3));
    drop(held);
    let mut completed = fixture::fixture_accept(&held_listener, Duration::from_secs(5))?;
    read_byte(&mut completed, b'D')?;
    worker
        .join()
        .map_err(|_| io::Error::other("drain worker panicked"))?;
    let outcome = result??;
    assert_eq!(outcome.termination, Termination::Exit(0));
    assert!(!outcome.timed_out);
    assert!(outcome.drain_truncated);
    assert!(outcome.elapsed_micros < 3_000_000);
    Ok(())
}

fn reaper_transfers_and_polls_fairly(root: &Path) -> Result<()> {
    let path = root.join("reaper.sock");
    let listener = UnixListener::bind(&path)?;
    let reaper = fixture::ReaperFixture::new(2)?;
    let socket_arg = fixture_arg(&path);
    reaper.transfer("wait-on-socket", std::slice::from_ref(&socket_arg), root)?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    read_byte(&mut held, b'R')?;
    assert_eq!(reaper.occupied(), 1);

    reaper.transfer("exit", &[], root)?;
    assert_eq!(reaper.occupied(), 2);
    assert!(
        matches!(reaper.transfer("exit", &[], root), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );

    reaper.resume();
    assert_eq!(reaper.wait_for_occupied(1, Duration::from_secs(5))?, 1);
    // Slot zero remains held while the later, already-exited slot is reclaimed.
    reaper.transfer("exit", &[], root)?;
    assert_eq!(reaper.occupied(), 2);
    drop(held);
    assert_eq!(reaper.wait_for_occupied(0, Duration::from_secs(5))?, 0);
    Ok(())
}

fn main() -> Result<()> {
    if fixture::dispatch()? {
        return Ok(());
    }
    suite()
}
