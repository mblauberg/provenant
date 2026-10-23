use provenant_supervisor::{exec::*, fixture};
use std::error::Error;
use std::sync::Mutex;
use std::time::Duration;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
struct Decoder(Mutex<Vec<Vec<u8>>>);
impl FrameSink for Decoder {
    fn try_frame(&self, bytes: &[u8]) -> std::result::Result<(), QueueLoss> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
}
fn suite() -> Result<()> {
    let root = tempfile::tempdir()?;
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
    assert!(decoder
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|frame| frame == b"terminal"));
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
    println!("process fixtures passed");
    Ok(())
}
fn main() -> Result<()> {
    if fixture::dispatch()? {
        return Ok(());
    }
    suite()
}
