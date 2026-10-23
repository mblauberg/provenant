//! Durable detached-worker evidence and owner orchestration.
use crate::pending;
pub use crate::pending::{ProcessProbe, SessionHost, SessionWorker, WorkerExit};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arguments {
    pub validate: bool,
    pub run_dir: Option<OsString>,
    pub command: Vec<OsString>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("--run-dir needs a value")]
    MissingValue,
    #[error("unknown argument: {}", .0.to_string_lossy())]
    UnknownArgument(OsString),
    #[error("--run-dir is required")]
    MissingDirectory,
    #[error("run directory does not exist: {}", .0.display())]
    MissingRun(PathBuf),
    #[error("--validate does not accept a worker command")]
    ValidationCommand,
    #[error("worker command is required after --")]
    MissingCommand,
    #[error("cannot create run-directory parent: {}", path.display())]
    Parent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("run directory already exists or cannot be claimed: {}", path.display())]
    Claim {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("run directory is not writable: {}", .0.display())]
    NotWritable(PathBuf),
    #[error("cannot persist wrapper PID before launch")]
    WrapperPid(#[source] io::Error),
    #[error("run transcript is not writable: {}", path.display())]
    Transcript {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot persist worker PID after launch; terminating worker")]
    WorkerPid {
        #[source]
        source: io::Error,
        cleanup: Option<io::Error>,
    },
    #[error("cannot persist durable completion evidence")]
    Completion(#[source] io::Error),
    #[error("worker launch or wait failed: {0}")]
    Worker(#[source] io::Error),
    #[error("completion evidence is not UTF-8: {path}")]
    Encoding {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::WorkerPid { .. } | Self::Completion(_) => 1,
            _ => 2,
        }
    }
}

pub fn parse_args(args: &[OsString]) -> Result<Arguments, Error> {
    let mut parsed = Arguments {
        validate: false,
        run_dir: None,
        command: Vec::new(),
    };
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--validate" {
            parsed.validate = true;
            index += 1;
        } else if args[index] == "--run-dir" {
            parsed.run_dir = Some(args.get(index + 1).ok_or(Error::MissingValue)?.clone());
            index += 2;
        } else if args[index] == "--" {
            parsed.command = args[index + 1..].to_vec();
            break;
        } else {
            return Err(Error::UnknownArgument(args[index].clone()));
        }
    }
    Ok(parsed)
}

impl Arguments {
    /// Match entry-point validation order before any claim or launch effects.
    pub fn check(&self) -> Result<&Path, Error> {
        let directory = self
            .run_dir
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(Path::new)
            .ok_or(Error::MissingDirectory)?;
        if self.validate {
            if !directory.is_dir() {
                return Err(Error::MissingRun(directory.into()));
            }
            if !self.command.is_empty() {
                return Err(Error::ValidationCommand);
            }
        } else if self.command.is_empty() {
            return Err(Error::MissingCommand);
        }
        Ok(directory)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum EvidenceError {
    #[error("completion evidence missing wrapper PID")]
    MissingWrapper,
    #[error("completion evidence has invalid wrapper PID")]
    InvalidWrapper,
    #[error("completion evidence missing worker PID")]
    MissingWorker,
    #[error("completion evidence has invalid worker PID")]
    InvalidWorker,
    #[error("completion evidence missing: wrapper exited without marker")]
    MissingMarker,
    #[error("completion evidence marker has invalid exit status")]
    InvalidExit,
    #[error("completion evidence marker is malformed or does not match persisted PIDs")]
    Malformed,
    #[error("completion evidence worker PID is still live")]
    LiveWorker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Validation {
    Starting,
    Complete {
        worker_pid: String,
        wrapper_pid: String,
        exit_status: String,
    },
}

impl Validation {
    pub fn exit_code(&self) -> i32 {
        if matches!(self, Self::Starting) { 1 } else { 0 }
    }
    pub fn stdout(&self) -> String {
        match self {
            Self::Starting => String::new(),
            Self::Complete {
                worker_pid,
                wrapper_pid,
                exit_status,
            } => format!("{worker_pid} {wrapper_pid} {exit_status}\n"),
        }
    }
}

/// Observations are values: policy tests need neither wall-clock waits nor global patches.
#[derive(Clone, Debug, Default)]
pub struct Evidence {
    pub wrapper: Option<String>,
    pub worker: Option<String>,
    pub done: Option<String>,
    pub done_exists: bool,
    pub done_size: Option<u64>,
    pub directory_age_seconds: Option<f64>,
}

fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Preserve the legacy rule: exactly one newline; bytes following it are ignored.
pub fn single_value(text: &str) -> Option<String> {
    let text = universal_newlines(text);
    if text.matches('\n').count() != 1 {
        return None;
    }
    text.split_once('\n')
        .map(|(value, _)| value)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn digits(value: &str) -> bool {
    pending::python_isdigit(value)
}

pub fn validate(
    evidence: &Evidence,
    probe: &mut impl ProcessProbe,
) -> Result<Validation, EvidenceError> {
    let wrapper = evidence.wrapper.as_deref().and_then(single_value);
    let Some(wrapper) = wrapper else {
        if !evidence.done_exists
            && evidence
                .directory_age_seconds
                .is_some_and(|age| (0.0..=2.0).contains(&age))
        {
            return Ok(Validation::Starting);
        }
        return Err(EvidenceError::MissingWrapper);
    };
    if !digits(&wrapper) {
        return Err(EvidenceError::InvalidWrapper);
    }
    let worker = evidence.worker.as_deref().and_then(single_value);
    let Some(worker) = worker else {
        if probe.process_exists(&wrapper) && evidence.done_size.is_none_or(|size| size == 0) {
            return Ok(Validation::Starting);
        }
        return Err(EvidenceError::MissingWorker);
    };
    if !digits(&worker) {
        return Err(EvidenceError::InvalidWorker);
    }
    let done = universal_newlines(evidence.done.as_deref().unwrap_or(""));
    if done.is_empty() {
        if probe.process_exists(&wrapper) {
            return Ok(Validation::Starting);
        }
        return Err(EvidenceError::MissingMarker);
    }
    if !done.split('\n').any(|line| {
        line.strip_prefix("exit=")
            .is_some_and(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
    }) {
        return Err(EvidenceError::InvalidExit);
    }
    let mut wrappers = Vec::new();
    let mut workers = Vec::new();
    let mut exits = Vec::new();
    let lines: Vec<_> = done
        .split_terminator([
            '\n', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
        ])
        .collect();
    for line in &lines {
        if let Some((key, value)) = line.split_once('=') {
            if digits(value) {
                match key {
                    "wrapper_pid" => wrappers.push(value),
                    "worker_pid" => workers.push(value),
                    "exit" => exits.push(value),
                    _ => (),
                }
            }
        }
    }
    if done.matches('\n').count() != 3
        || lines.len() != 3
        || wrappers != [wrapper.as_str()]
        || workers != [worker.as_str()]
        || exits.len() != 1
    {
        return Err(EvidenceError::Malformed);
    }
    if probe.process_exists(&wrapper) {
        return Ok(Validation::Starting);
    }
    if probe.process_exists(&worker) {
        return Err(EvidenceError::LiveWorker);
    }
    Ok(Validation::Complete {
        worker_pid: worker,
        wrapper_pid: wrapper,
        exit_status: exits[0].into(),
    })
}

pub fn validate_run(
    run_dir: &Path,
    now: SystemTime,
    probe: &mut impl ProcessProbe,
) -> Result<Validation, Error> {
    let read = |name| -> Result<Option<String>, Error> {
        let path = run_dir.join(name);
        match pending::read_bounded(&path, 1024 * 1024) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|source| Error::Encoding { path, source }),
            Err(_) => Ok(None),
        }
    };
    // Read lazily, in the source's refusal order. In particular an invalid
    // worker/done file cannot mask an invalid or absent wrapper PID.
    let wrapper = read("wrapper.pid")?;
    let wrapper_value = wrapper.as_deref().and_then(single_value);
    if wrapper_value.as_deref().is_none_or(|value| !digits(value)) {
        let age = if wrapper_value.is_none() {
            fs::metadata(run_dir)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|modified| pending::epoch_seconds(now) - pending::epoch_seconds(modified))
        } else {
            None
        };
        return validate(
            &Evidence {
                wrapper,
                done_exists: run_dir.join("done").exists(),
                directory_age_seconds: age,
                ..Evidence::default()
            },
            probe,
        )
        .map_err(Error::Evidence);
    }
    let worker = read("worker.pid")?;
    if worker.as_deref().and_then(single_value).is_none() {
        if wrapper_value
            .as_deref()
            .is_some_and(|pid| probe.process_exists(pid))
            && fs::metadata(run_dir.join("done"))
                .map(|m| m.len() == 0)
                .unwrap_or(true)
        {
            return Ok(Validation::Starting);
        }
        return Err(EvidenceError::MissingWorker.into());
    }
    if worker
        .as_deref()
        .and_then(single_value)
        .is_some_and(|pid| !digits(&pid))
    {
        return Err(EvidenceError::InvalidWorker.into());
    }
    validate(
        &Evidence {
            wrapper,
            worker,
            done: read("done")?,
            ..Evidence::default()
        },
        probe,
    )
    .map_err(Error::Evidence)
}

/// Run to completion in the foreground; the safe supervisor adapter owns the worker.
/// `helper_name` is the compatibility script's argv[0] used in exec-failure transcripts.
pub fn execute(
    run_dir: &Path,
    command: &[OsString],
    wrapper_pid: u32,
    helper_name: &OsStr,
    host: &mut impl SessionHost,
) -> Result<i32, Error> {
    if command.is_empty() {
        return Err(Error::MissingCommand);
    }
    let parent = pending::parent(run_dir);
    fs::create_dir_all(parent).map_err(|source| Error::Parent {
        path: parent.into(),
        source,
    })?;
    fs::create_dir(run_dir).map_err(|source| Error::Claim {
        path: run_dir.into(),
        source,
    })?;
    if !host.writable(run_dir) {
        return Err(Error::NotWritable(run_dir.into()));
    }
    pending::publish_marker(
        &run_dir.join("wrapper.pid"),
        wrapper_pid,
        format!("{wrapper_pid}\n").as_bytes(),
    )
    .map_err(Error::WrapperPid)?;
    let transcript = run_dir.join("transcript.txt");
    fs::write(&transcript, []).map_err(|source| Error::Transcript {
        path: transcript.clone(),
        source,
    })?;
    let mut output = OpenOptions::new()
        .append(true)
        .open(&transcript)
        .map_err(|source| Error::Transcript {
            path: transcript.clone(),
            source,
        })?;
    let mut worker = match host.spawn(command, &output) {
        Ok(worker) => worker,
        Err(error) => {
            let diagnostic = format!(
                "{}: line 172: {}: {}\n",
                helper_name.to_string_lossy(),
                command[0].to_string_lossy(),
                host.spawn_error_text(&error)
            );
            output
                .write_all(diagnostic.as_bytes())
                .map_err(|source| Error::Transcript {
                    path: transcript.clone(),
                    source,
                })?;
            let status = if error.kind() == io::ErrorKind::NotFound {
                "exit 127"
            } else {
                "exit 126"
            };
            host.spawn(&["/bin/bash".into(), "-c".into(), status.into()], &output)
                .map_err(Error::Worker)?
        }
    };
    let worker_pid = worker.pid();
    if let Err(source) = pending::publish_marker(
        &run_dir.join("worker.pid"),
        wrapper_pid,
        format!("{worker_pid}\n").as_bytes(),
    ) {
        let cleanup = worker
            .terminate_and_wait(std::time::Duration::from_secs(5))
            .err();
        return Err(Error::WorkerPid { source, cleanup });
    }
    // Each observation is bounded; the source imposes no execution deadline.
    // Silence never changes this loop into a stop request.
    let exit = loop {
        if let Some(exit) = worker
            .wait_for(std::time::Duration::from_secs(1))
            .map_err(Error::Worker)?
        {
            break exit;
        }
    };
    let exit_status = match exit {
        WorkerExit::Exited(code) => code,
        WorkerExit::Signalled(signal) => 128 + signal,
    };
    let done = format!("wrapper_pid={wrapper_pid}\nworker_pid={worker_pid}\nexit={exit_status}\n");
    pending::publish_marker(&run_dir.join("done"), wrapper_pid, done.as_bytes())
        .map_err(Error::Completion)?;
    Ok(exit_status)
}

#[cfg(test)]
#[path = "detached_tests.rs"]
mod tests;
