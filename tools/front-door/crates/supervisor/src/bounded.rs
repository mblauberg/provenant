//! File-backed bounded utility execution. Not the S2 provider lifecycle owner.
use crate::capture::{read_bounded_output, BoundedProcessResult, CaptureError};
use crate::identity::Signal;
use crate::sys::{anonymous_spool, OwnedChild};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BoundedError {
    #[error("bounded command is empty")]
    EmptyCommand,
    #[error("invalid timeout")]
    InvalidTimeout,
    #[error("bounded process {operation} failed")]
    Io { operation: &'static str, #[source] source: io::Error },
    #[error(transparent)]
    Capture(#[from] CaptureError),
}

pub struct Options<'a> {
    pub cwd: &'a Path,
    pub timeout_seconds: f64,
    pub env: Option<&'a BTreeMap<OsString, OsString>>,
    pub output_limit_bytes: i64,
    pub merge_stderr: bool,
    pub pass_fds: &'a [BorrowedFd<'a>],
}

impl<'a> Options<'a> {
    pub fn new(cwd: &'a Path, timeout_seconds: f64) -> Self {
        Self { cwd, timeout_seconds, env: None, output_limit_bytes: 1_048_576,
            merge_stderr: true, pass_fds: &[] }
    }
}

/// Sans-IO deadline/cleanup policy, in seconds since the wait began. Cleanup
/// happens on successful child exit too: a descendant can still hold the spool.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Waiting,
    Grace { deadline: f64 },
    Reap,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Wait,
    Terminate { timed_out: bool },
    Kill,
    Reap,
}

impl State {
    pub fn step(&mut self, now: f64, timeout: f64, child_exited: bool, group_exists: bool) -> Effect {
        match *self {
            Self::Waiting if child_exited || now >= timeout => {
                *self = Self::Grace { deadline: now + 0.25 };
                Effect::Terminate { timed_out: !child_exited }
            }
            Self::Grace { deadline } if !group_exists || now >= deadline => {
                *self = Self::Reap;
                if group_exists { Effect::Kill } else { Effect::Reap }
            }
            Self::Reap => Effect::Reap,
            _ => Effect::Wait,
        }
    }
}

fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> BoundedError {
    move |source| BoundedError::Io { operation, source }
}

pub fn run_bounded(command: &[OsString], options: Options<'_>) -> Result<BoundedProcessResult, BoundedError> {
    let Some(program) = command.first() else { return Err(BoundedError::EmptyCommand); };
    if !options.timeout_seconds.is_finite() { return Err(BoundedError::InvalidTimeout); }
    let started = Instant::now();
    let mut stdout = anonymous_spool().map_err(io_error("create stdout spool"))?;
    let mut stderr = if options.merge_stderr { None } else {
        Some(anonymous_spool().map_err(io_error("create stderr spool"))?)
    };
    let mut launch = Command::new(program);
    launch.args(&command[1..]).current_dir(options.cwd)
        .stdout(Stdio::from(stdout.try_clone().map_err(io_error("clone stdout spool"))?))
        .stderr(Stdio::from(stderr.as_ref().unwrap_or(&stdout).try_clone().map_err(io_error("clone stderr spool"))?));
    if let Some(env) = options.env { launch.env_clear().envs(env); }
    let mut child = OwnedChild::spawn(&mut launch, options.pass_fds).map_err(io_error("spawn"))?;
    // Python starts its wait timeout after Popen has returned.
    let waiting = Instant::now();
    let mut state = State::Waiting;
    let mut timed_out = false;
    loop {
        let exited = child.exited().map_err(io_error("observe child"))?;
        match state.step(waiting.elapsed().as_secs_f64(), options.timeout_seconds, exited, child.group_exists()) {
            Effect::Wait => std::thread::sleep(Duration::from_millis(10)),
            Effect::Terminate { timed_out: expired } => {
                timed_out = expired;
                if child.signal(Signal::Terminate).is_err() && !exited {
                    child.kill_direct().map_err(io_error("kill direct child"))?;
                }
            }
            Effect::Kill => {
                if child.signal(Signal::Kill).is_err() && !exited {
                    child.kill_direct().map_err(io_error("kill direct child"))?;
                }
            },
            Effect::Reap => break,
        }
    }
    let status = child.reap().map_err(io_error("reap child"))?;
    let output = read_bounded_output(&mut stdout, options.output_limit_bytes)?;
    let error_output = stderr.as_mut().map(|spool| read_bounded_output(spool, options.output_limit_bytes)).transpose()?;
    Ok(BoundedProcessResult {
        returncode: status.code().unwrap_or_else(|| -status.signal().unwrap_or(0)),
        stdout: error_output.as_ref().map(|_| output.text.clone()),
        stdout_bytes: error_output.as_ref().map(|_| output.bytes),
        stdout_truncated: error_output.as_ref().map(|_| output.truncated),
        stderr: error_output.as_ref().map(|view| view.text.clone()),
        stderr_bytes: error_output.as_ref().map(|view| view.bytes),
        stderr_truncated: error_output.as_ref().map(|view| view.truncated),
        output: output.text, output_bytes: output.bytes, output_truncated: output.truncated,
        timed_out, elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}

/// Edge rendering for BudgetArgumentParser's one-line errors (exit status 2).
pub fn argument_error(command: &str, message: &str) -> String {
    let clean = message.replace('\n', " ");
    let clean = clean.trim();
    let fix = if let Some(args) = clean.strip_prefix("the following arguments are required:") {
        format!("pass {}", args.trim())
    } else if clean.contains("one of the arguments") && clean.contains("is required") {
        let args = clean.split_once("arguments").map_or("", |(_, rest)| rest)
            .split_once("is required").map_or("", |(args, _)| args).trim();
        format!("pass {args}")
    } else { format!("provenant help {command}") };
    format!("error: {clean}; fix: {fix}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsFd, AsRawFd};

    fn shell(code: &str, merge: bool, limit: i64) -> BoundedProcessResult {
        let mut options = Options::new(Path::new("/"), 5.0);
        options.merge_stderr = merge;
        options.output_limit_bytes = limit;
        run_bounded(&["/bin/sh".into(), "-c".into(), code.into()], options).unwrap()
    }

    #[test]
    fn merged_order_and_separated_streams_match_the_policy() {
        let merged = shell("printf OUT; printf ERR >&2", true, 128);
        assert_eq!(merged.output, "OUTERR");
        assert_eq!(merged.output_bytes, 6);
        assert_eq!(merged.stdout, None);
        assert_eq!(merged.stderr, None);
        let separate = shell("printf OUT; printf ERR >&2", false, 128);
        assert_eq!(separate.output, "OUT");
        assert_eq!(separate.stdout.as_deref(), Some("OUT"));
        assert_eq!(separate.stderr.as_deref(), Some("ERR"));
        assert_eq!(separate.stdout_bytes, Some(3));
        assert_eq!(separate.stderr_bytes, Some(3));
        let error_only = shell("printf ERR >&2", false, 128);
        assert_eq!(error_only.output, "");
        assert_eq!(error_only.stderr.as_deref(), Some("ERR"));
    }

    #[test]
    fn large_output_bounds_each_stream_without_pipe_deadlock() {
        let output = shell("printf HEAD; head -c 262144 /dev/zero | tr '\\000' x; printf TAIL; printf ERROR >&2", false, 128);
        assert_eq!(output.returncode, 0);
        assert_eq!(output.output.len(), 128);
        assert!(output.output.starts_with("HEAD"));
        assert!(output.output.ends_with("TAIL"));
        assert_eq!(output.stdout_bytes, Some(262152));
        assert_eq!(output.stderr.as_deref(), Some("ERROR"));
    }

    #[test]
    fn natural_signal_has_negative_status_without_timeout() {
        let output = shell("kill -TERM $$", true, 128);
        assert_eq!(output.returncode, -15);
        assert!(!output.timed_out);
        assert_eq!(output.terminating_signal(), Some(15));
    }

    #[test]
    fn explicit_deadlines_clean_up_after_both_exit_and_timeout() {
        let mut state = State::Waiting;
        assert_eq!(state.step(0.0, 5.0, false, true), Effect::Wait);
        assert_eq!(state.step(5.0, 5.0, false, true), Effect::Terminate { timed_out: true });
        assert_eq!(state.step(5.249, 5.0, true, true), Effect::Wait);
        assert_eq!(state.step(5.25, 5.0, true, true), Effect::Kill);
        assert_eq!(state.step(5.25, 5.0, true, true), Effect::Reap);
        let mut state = State::Waiting;
        assert_eq!(state.step(1.0, 5.0, true, true), Effect::Terminate { timed_out: false });
        assert_eq!(state.step(1.0, 5.0, true, false), Effect::Reap);
    }

    #[test]
    fn zero_deadline_reaps_child_and_returns_negative_status() {
        let mut options = Options::new(Path::new("/"), 0.0);
        options.output_limit_bytes = 128;
        let output = run_bounded(&["/bin/sh".into(), "-c".into(), "while :; do :; done".into()], options).unwrap();
        assert!(output.timed_out);
        assert!(output.returncode < 0);
    }

    #[test]
    fn pass_fds_and_exact_environment_are_preserved() {
        use std::io::{Seek, Write};
        let dir = tempfile::tempdir().unwrap();
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(b"PASSED").unwrap();
        input.rewind().unwrap();
        let fds = [input.as_fd()];
        let env = BTreeMap::from([("ONLY".into(), "kept".into())]);
        let mut options = Options::new(dir.path(), 5.0);
        options.pass_fds = &fds;
        options.env = Some(&env);
        let output = run_bounded(&["/bin/sh".into(), "-c".into(),
            format!("test \"$ONLY\" = kept && test -z \"$HOME\" && cat /dev/fd/{}", input.as_raw_fd()).into()], options).unwrap();
        assert_eq!(output.returncode, 0);
        assert_eq!(output.output, "PASSED");
    }

    #[test]
    fn one_line_argument_errors_keep_required_argument_fixes() {
        assert_eq!(argument_error("custom-cmd", "unrecognized arguments: --unknown\n"),
            "error: unrecognized arguments: --unknown; fix: provenant help custom-cmd\n");
        assert_eq!(argument_error("cmd", "the following arguments are required: --arg"),
            "error: the following arguments are required: --arg; fix: pass --arg\n");
        assert_eq!(argument_error("cmd", "one of the arguments --a --b is required"),
            "error: one of the arguments --a --b is required; fix: pass --a --b\n");
    }

    #[test]
    fn differential_runner_records_match_python_except_measured_elapsed_time() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(&root, r#"
import json
from dataclasses import asdict
from pathlib import Path
from skills._shared.bounded_process import run_bounded
out = []
for code, merge, limit in [('printf OUT; printf ERR >&2', True, 128), ('printf OUT; printf ERR >&2', False, 128), ('printf ERR >&2', False, 0), ('kill -TERM $$', True, 128), ('printf HEADxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxTAIL', True, 30)]:
    result = asdict(run_bounded(['/bin/sh', '-c', code], cwd=Path('/'), timeout_seconds=5, merge_stderr=merge, output_limit_bytes=limit))
    del result['elapsed_seconds'] # This is the one explicitly measured clock field.
    out.append(result)
print(json.dumps(out))
"#) else { return; };
        let actual: Vec<_> = [("printf OUT; printf ERR >&2", true, 128), ("printf OUT; printf ERR >&2", false, 128),
            ("printf ERR >&2", false, 0), ("kill -TERM $$", true, 128), ("printf HEADxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxTAIL", true, 30)]
            .into_iter().map(|(code, merge, limit)| {
                let mut value = serde_json::to_value(shell(code, merge, limit)).unwrap();
                value.as_object_mut().unwrap().remove("elapsed_seconds");
                value
            }).collect();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}
