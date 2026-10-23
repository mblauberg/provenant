//! Temporary safe OS helpers until the supervisor exposes these contracts.
#[path = "python_unicode.rs"]
mod python_unicode;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

// port: belongs in provenant-supervisor
pub(crate) fn lock(path: &Path) -> io::Result<File> {
    fs::create_dir_all(parent(path))?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

// port: belongs in provenant-core
pub(crate) fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

// port: belongs in provenant-supervisor
pub(crate) fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record exceeds size limit",
        ));
    }
    Ok(bytes)
}

// port: belongs in provenant-supervisor
/// A custody capability owns its lock and cannot be constructed from a recorded PID.
/// Implementations must retain it through publication and terminal cleanup.
pub trait OwnerCustody: std::fmt::Debug {}
// port: belongs in provenant-supervisor
/// Safe process-census boundary. Match kill(pid, 0): permission errors are false.
/// This is an existence probe, never authority to signal a recorded PID.
pub trait ProcessProbe {
    fn process_exists(&mut self, pid: &str) -> bool;
}

// port: belongs in provenant-supervisor
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerExit {
    Exited(i32),
    Signalled(i32),
}

// port: belongs in provenant-supervisor
/// The handle owns only the worker it launched, including reaping responsibility.
pub trait SessionWorker {
    fn pid(&self) -> u32;
    /// Observe for at most `timeout`, retaining custody on timeout. A quiet worker
    /// stays live; this poll never authorizes signalling or loses its handle.
    fn wait_for(&mut self, timeout: std::time::Duration) -> io::Result<Option<WorkerExit>>;
    /// Used only if the worker PID cannot be persisted, matching the old wrapper.
    /// Send SIGTERM to this owned worker (not SIGKILL or its process group), then
    /// wait and reap it. An already-exited worker is still reaped successfully.
    fn terminate_and_wait(&mut self, timeout: std::time::Duration) -> io::Result<()>;
}

// port: belongs in provenant-supervisor
/// Spawn in a fresh session with /dev/null stdin and both output streams attached
/// to the same append-only file description. No detached threads or inherited stdin.
/// Dropping the wrapper must not kill the worker. A failed spawn creates no worker.
pub trait SessionHost {
    type Worker: SessionWorker;
    fn writable(&mut self, path: &Path) -> bool;
    fn spawn(
        &mut self,
        command: &[std::ffi::OsString],
        transcript: &File,
    ) -> io::Result<Self::Worker>;
    /// strerror without Rust's '(os error N)' suffix; needed for the retained transcript.
    fn spawn_error_text(&self, error: &io::Error) -> String;
}

// port: belongs in provenant-supervisor
/// Detached markers deliberately use the source's deterministic .tmp.<pid> name,
/// default creation permissions and rename, without the lease writer's fsync.
pub(crate) fn publish_marker(path: &Path, wrapper_pid: u32, bytes: &[u8]) -> io::Result<()> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::other("missing marker filename"))?
        .to_owned();
    name.push(format!(".tmp.{wrapper_pid}"));
    let temporary = parent(path).join(name);
    let result = fs::write(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if let Err(error) = result {
        match fs::remove_file(&temporary) {
            Ok(()) => (),
            Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => (),
            Err(cleanup) => return Err(cleanup),
        }
        return Err(error);
    }
    Ok(())
}

// port: belongs in provenant-core
pub(crate) fn python_isdigit(text: &str) -> bool {
    !text.is_empty() && text.chars().all(python_unicode::isdigit)
}

// port: belongs in provenant-core
// Unicode 16.0 decimal-zero code points, generated from Python's unicodedata.
const DECIMAL_ZEROES: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

// Match Python int()'s Unicode Nd digit conversion. Superscript digits, for
// example, satisfy str.isdigit() but are not valid integer-string digits.
// port: belongs in provenant-core
pub(crate) fn python_decimal_digit(character: char) -> Option<u8> {
    let codepoint = u32::from(character);
    DECIMAL_ZEROES.iter().find_map(|zero| {
        if codepoint >= *zero && codepoint < *zero + 10 {
            Some((codepoint - *zero) as u8)
        } else {
            None
        }
    })
}

// port: belongs in provenant-core
pub(crate) fn epoch_seconds(time: std::time::SystemTime) -> f64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    }
}
