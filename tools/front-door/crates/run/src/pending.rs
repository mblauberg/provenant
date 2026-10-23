use serde::{Deserialize, Serialize};
pub(crate) use provenant_core::py::{python_repr, python_truth as json_truthy};
// Temporary safe OS helpers until the supervisor exposes these contracts.
#[path = "python_unicode.rs"]
mod python_unicode;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
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
pub(crate) fn epoch_seconds(time: std::time::SystemTime) -> f64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    }
}

// port: belongs in provenant-supervisor
pub(crate) fn publish_lease(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::create_dir_all(parent(path))?;
    // create_new gives independent calls distinct temporary files without global state.
    for serial in 0..1024 {
        let mut name = std::ffi::OsString::from(".");
        name.push(
            path.file_name()
                .ok_or_else(|| io::Error::other("missing lease filename"))?,
        );
        name.push(format!(".{}.{serial}", std::process::id()));
        let temp = parent(path).join(name);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        let cleanup = fs::remove_file(&temp);
        return match (result, cleanup) {
            (Err(e), _) => Err(e),
            (Ok(()), Err(e)) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "lease temporary files exhausted",
    ))
}

// port: belongs in provenant-records
pub(crate) fn ascii_json(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub schema_version: u8,
    pub status: String,
    pub holder: String,
    pub previous_holder: String,
    pub generation: i64,
    pub updated_at: String,
    pub expires_at: String,
    pub handoff_evidence: String,
}

