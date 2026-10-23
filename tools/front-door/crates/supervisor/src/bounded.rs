//! File-backed bounded utility execution. Not the S2 provider lifecycle owner.
use crate::capture::{
    read_bounded_output, read_bounded_text, BoundedProcessResult, CaptureError, CapturedOutput,
};
use crate::identity::Signal;
use crate::sys::{anonymous_spool, OwnedChild};
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
    /// NaN: Python's wait never expires on NaN; refuse rather than hang.
    #[error("invalid timeout")]
    InvalidTimeout,
    #[error("bounded process {operation} failed")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Wait(#[from] crate::sys::ChildWaitError),
}

pub struct Options<'a> {
    pub cwd: &'a Path,
    pub timeout_seconds: f64,
    pub env: Option<&'a [(OsString, OsString)]>,
    pub output_limit_bytes: i64,
    pub merge_stderr: bool,
    /// Fabric utilities ignore stdin; Python bounded_process inherits it.
    pub null_stdin: bool,
    pub pass_fds: &'a [BorrowedFd<'a>],
}

impl<'a> Options<'a> {
    pub fn new(cwd: &'a Path, timeout_seconds: f64) -> Self {
        Self {
            cwd,
            timeout_seconds,
            env: None,
            output_limit_bytes: 1_048_576,
            merge_stderr: true,
            null_stdin: false,
            pass_fds: &[],
        }
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
    pub fn step(
        &mut self,
        now: f64,
        timeout: f64,
        child_exited: bool,
        group_exists: bool,
    ) -> Effect {
        self.step_with_output_limit(now, timeout, child_exited, group_exists, false)
    }

    fn step_with_output_limit(
        &mut self,
        now: f64,
        timeout: f64,
        child_exited: bool,
        group_exists: bool,
        output_exceeded: bool,
    ) -> Effect {
        if output_exceeded && *self == Self::Waiting {
            *self = Self::Grace {
                deadline: now + 0.25,
            };
            return Effect::Terminate { timed_out: false };
        }
        match *self {
            Self::Waiting if child_exited || now >= timeout => {
                *self = Self::Grace {
                    deadline: now + 0.25,
                };
                Effect::Terminate {
                    timed_out: !child_exited,
                }
            }
            Self::Grace { deadline } if !group_exists || now >= deadline => {
                *self = Self::Reap;
                if group_exists {
                    Effect::Kill
                } else {
                    Effect::Reap
                }
            }
            Self::Reap => Effect::Reap,
            _ => Effect::Wait,
        }
    }
}

fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> BoundedError {
    move |source| BoundedError::Io { operation, source }
}

pub fn run_bounded(
    command: &[OsString],
    options: Options<'_>,
) -> Result<BoundedProcessResult, BoundedError> {
    run(command, options, CaptureMode::Replace, false)
}

pub(crate) fn run_bounded_text(
    command: &[OsString],
    options: Options<'_>,
) -> Result<BoundedProcessResult, BoundedError> {
    run(command, options, CaptureMode::Text, false)
}

/// Node's execFileSync aborts when its stdout buffer crosses maxBuffer.
pub(crate) fn run_bounded_limited(
    command: &[OsString],
    options: Options<'_>,
) -> Result<BoundedProcessResult, BoundedError> {
    run(command, options, CaptureMode::Replace, true)
}

#[derive(Clone, Copy)]
enum CaptureMode {
    Replace,
    Text,
}
impl CaptureMode {
    fn read(self, spool: &mut std::fs::File, limit: i64) -> Result<CapturedOutput, CaptureError> {
        match self {
            Self::Replace => read_bounded_output(spool, limit),
            Self::Text => read_bounded_text(spool, limit),
        }
    }
}

fn run(
    command: &[OsString],
    options: Options<'_>,
    mode: CaptureMode,
    stop_on_output_limit: bool,
) -> Result<BoundedProcessResult, BoundedError> {
    let Some(program) = command.first() else {
        return Err(BoundedError::EmptyCommand);
    };
    // Infinity waits without a deadline, as Python's wait(timeout=inf) does.
    if options.timeout_seconds.is_nan() {
        return Err(BoundedError::InvalidTimeout);
    }
    let started = Instant::now();
    let mut stdout = anonymous_spool().map_err(io_error("create stdout spool"))?;
    let mut stderr = if options.merge_stderr {
        None
    } else {
        Some(anonymous_spool().map_err(io_error("create stderr spool"))?)
    };
    let mut launch = Command::new(program);
    launch
        .args(&command[1..])
        .current_dir(options.cwd)
        .stdout(Stdio::from(
            stdout.try_clone().map_err(io_error("clone stdout spool"))?,
        ))
        .stderr(if stop_on_output_limit {
            // execFileSync's ps bridge ignores stderr, even on failure.
            Stdio::null()
        } else {
            Stdio::from(
                stderr
                    .as_ref()
                    .unwrap_or(&stdout)
                    .try_clone()
                    .map_err(io_error("clone stderr spool"))?,
            )
        });
    if options.null_stdin {
        launch.stdin(Stdio::null());
    }
    let mut child =
        OwnedChild::spawn(&mut launch, options.pass_fds, options.env).map_err(io_error("spawn"))?;
    let (stopped, timed_out) = wait_and_stop(
        &mut child,
        options.timeout_seconds,
        stop_on_output_limit.then_some((&stdout, options.output_limit_bytes.max(0) as u64)),
    )?;
    let status = stopped.status;
    let output = mode.read(&mut stdout, options.output_limit_bytes)?;
    let error_output = stderr
        .as_mut()
        .map(|spool| mode.read(spool, options.output_limit_bytes))
        .transpose()?;
    Ok(BoundedProcessResult {
        returncode: status
            .code()
            .unwrap_or_else(|| -status.signal().unwrap_or(0)),
        stdout: error_output.as_ref().map(|_| output.text.clone()),
        stdout_bytes: error_output.as_ref().map(|_| output.bytes),
        stdout_truncated: error_output.as_ref().map(|_| output.truncated),
        stderr: error_output.as_ref().map(|view| view.text.clone()),
        stderr_bytes: error_output.as_ref().map(|view| view.bytes),
        stderr_truncated: error_output.as_ref().map(|view| view.truncated),
        output: output.text,
        output_bytes: output.bytes,
        output_truncated: output.truncated,
        timed_out,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}

/// An owned utility session. Only sessions created here can be stopped through
/// this API; it cannot adopt a provider PID or a caller's arbitrary process.
pub struct UtilityChild(OwnedChild);

impl UtilityChild {
    /// Spawn a private session. `env` is the full ordered environment; None
    /// inherits the current environment. Command-specific env changes are ignored.
    pub fn spawn(
        command: &mut Command,
        pass_fds: &[BorrowedFd<'_>],
        env: Option<&[(OsString, OsString)]>,
    ) -> Result<Self, BoundedError> {
        OwnedChild::spawn(command, pass_fds, env)
            .map(Self)
            .map_err(io_error("spawn"))
    }
}

#[derive(Debug)]
pub struct GroupStop {
    pub status: std::process::ExitStatus,
    /// Whether cleanup attempted SIGKILL, including direct-child fallback.
    pub escalated: bool,
}

/// SIGTERM, a bounded group grace, optional SIGKILL, and two bounded waits.
/// This is bounded_process.stop_process_group's utility-only API.
pub fn stop_process_group(child: &mut UtilityChild) -> Result<GroupStop, BoundedError> {
    wait_and_stop(&mut child.0, 0.0, None).map(|(stopped, _)| stopped)
}

fn wait_and_stop(
    child: &mut OwnedChild,
    timeout: f64,
    stdout_limit: Option<(&std::fs::File, u64)>,
) -> Result<(GroupStop, bool), BoundedError> {
    // Python starts its wait timeout after Popen has returned.
    let waiting = Instant::now();
    let mut state = State::Waiting;
    let mut timed_out = false;
    let mut escalated = false;
    loop {
        // Reap before group probing: a dead leader must not consume the grace.
        let exited = child.exited().map_err(io_error("observe child"))?;
        let group_exists = child.group_exists();
        let now = waiting.elapsed().as_secs_f64();
        let output_exceeded = match stdout_limit {
            Some((file, limit)) if state == State::Waiting => {
                file.metadata()
                    .map_err(io_error("observe stdout size"))?
                    .len()
                    > limit
            }
            _ => false,
        };
        let effect =
            state.step_with_output_limit(now, timeout, exited, group_exists, output_exceeded);
        match effect {
            Effect::Wait => std::thread::sleep(Duration::from_millis(10)),
            Effect::Terminate { timed_out: expired } => {
                timed_out = expired;
                if child.signal(Signal::Terminate).is_err() && !exited {
                    escalated = true;
                    child.kill_direct().map_err(io_error("kill direct child"))?;
                }
            }
            Effect::Kill => {
                escalated = true;
                if child.signal(Signal::Kill).is_err() && !exited {
                    child.kill_direct().map_err(io_error("kill direct child"))?;
                }
            }
            Effect::Reap => break,
        }
    }
    let grace = Duration::from_millis(250);
    let status = match child.reap(grace) {
        Ok(status) => Ok(status),
        Err(crate::sys::ChildWaitError::Timeout { .. }) => {
            escalated = true;
            child.kill_direct().map_err(io_error("kill direct child"))?;
            child.reap(grace)
        }
        Err(error) => Err(error),
    };
    // Both bounded waits are exhausted on a timeout; Drop must not add a third.
    child.finish_cleanup();
    let status = status?;
    Ok((GroupStop { status, escalated }, timed_out))
}

/// BudgetArgumentParser's edge effect, without terminating the embedding process.
/// The CLI exits with the returned status after this fallible stderr write.
pub fn write_argument_error(
    command_name: Option<&str>,
    program: &str,
    message: &str,
    stderr: &mut impl std::io::Write,
) -> Result<u8, BoundedError> {
    let command = command_name
        .filter(|name| !name.is_empty())
        .unwrap_or(program);
    stderr
        .write_all(argument_error(command, message).as_bytes())
        .map_err(io_error("write argument error"))?;
    Ok(2)
}

/// Edge rendering for BudgetArgumentParser's one-line errors (exit status 2).
pub fn argument_error(command: &str, message: &str) -> String {
    let clean = message.replace('\n', " ");
    let clean = provenant_core::py::python_strip(&clean);
    let fix = if let Some(args) = clean.strip_prefix("the following arguments are required:") {
        format!("pass {}", provenant_core::py::python_strip(args))
    } else if clean.contains("one of the arguments") && clean.contains("is required") {
        let args = clean
            .split_once("arguments")
            .map_or("", |(_, rest)| rest)
            .split_once("is required")
            .map_or("", |(args, _)| args)
            .trim_matches(provenant_core::py::python_whitespace);
        format!("pass {args}")
    } else {
        format!("provenant help {command}")
    };
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
    fn trivial_run_and_public_cleanup_do_not_escalate() {
        let output = shell("exit 0", true, 128);
        assert_eq!(output.returncode, 0);
        assert_eq!(output.terminating_signal(), None);
        assert!(!output.timed_out);
        let mut child = UtilityChild::spawn(&mut Command::new("/usr/bin/true"), &[], None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !child.0.exited().unwrap() {
            assert!(Instant::now() < deadline, "fixture failed to exit");
            std::thread::yield_now();
        }
        let stopped = stop_process_group(&mut child).unwrap();
        assert!(!stopped.escalated);
        assert_eq!(stopped.status.code(), Some(0));
    }

    #[test]
    fn explicit_environment_retains_insertion_order() {
        let env = vec![
            ("Z_LAST".into(), "first".into()),
            ("A_FIRST".into(), "second".into()),
        ];
        let mut options = Options::new(Path::new("/"), 5.0);
        options.env = Some(&env);
        let result = run_bounded(&["/usr/bin/env".into()], options).unwrap();
        assert_eq!(result.output, "Z_LAST=first\nA_FIRST=second\n");
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
        let output = shell(
            "printf HEAD; head -c 262144 /dev/zero | tr '\\000' x; printf TAIL; printf ERROR >&2",
            false,
            128,
        );
        assert_eq!(output.returncode, 0);
        assert_eq!(output.output.len(), 128);
        assert!(output.output.starts_with("HEAD"));
        assert!(output.output.ends_with("TAIL"));
        assert_eq!(output.stdout_bytes, Some(262152));
        assert_eq!(output.stderr.as_deref(), Some("ERROR"));
    }

    #[test]
    fn infinite_timeout_waits_like_python_and_nan_is_refused() {
        let mut options = Options::new(Path::new("/"), f64::INFINITY);
        options.merge_stderr = true;
        let output =
            run_bounded(&["/bin/sh".into(), "-c".into(), "exit 0".into()], options).unwrap();
        assert_eq!(output.returncode, 0);
        assert!(!output.timed_out);
        let options = Options::new(Path::new("/"), f64::NAN);
        assert!(matches!(
            run_bounded(&["/bin/sh".into(), "-c".into(), "exit 0".into()], options),
            Err(BoundedError::InvalidTimeout)
        ));
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
        assert_eq!(
            state.step(5.0, 5.0, false, true),
            Effect::Terminate { timed_out: true }
        );
        assert_eq!(state.step(5.249, 5.0, true, true), Effect::Wait);
        assert_eq!(state.step(5.25, 5.0, true, true), Effect::Kill);
        assert_eq!(state.step(5.25, 5.0, true, true), Effect::Reap);
        let mut state = State::Waiting;
        assert_eq!(
            state.step(1.0, 5.0, true, true),
            Effect::Terminate { timed_out: false }
        );
        assert_eq!(state.step(1.0, 5.0, true, false), Effect::Reap);
    }

    #[test]
    fn zero_deadline_reaps_child_and_returns_negative_status() {
        let mut options = Options::new(Path::new("/"), 0.0);
        options.output_limit_bytes = 128;
        let output = run_bounded(
            &["/bin/sh".into(), "-c".into(), "while :; do :; done".into()],
            options,
        )
        .unwrap();
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
        let env = Vec::from([("ONLY".into(), "kept".into())]);
        let mut options = Options::new(dir.path(), 5.0);
        options.pass_fds = &fds;
        options.env = Some(&env);
        let output = run_bounded(
            &[
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "test \"$ONLY\" = kept && test -z \"$HOME\" && cat /dev/fd/{}",
                    input.as_raw_fd()
                )
                .into(),
            ],
            options,
        )
        .unwrap();
        assert_eq!(output.returncode, 0);
        assert_eq!(output.output, "PASSED");
    }

    #[test]
    fn one_line_argument_errors_keep_required_argument_fixes() {
        assert_eq!(
            argument_error("custom-cmd", "unrecognized arguments: --unknown\n"),
            "error: unrecognized arguments: --unknown; fix: provenant help custom-cmd\n"
        );
        assert_eq!(
            argument_error(
                "cmd",
                "\u{1c}the following arguments are required: \u{1c}--arg\u{1c}"
            ),
            "error: the following arguments are required: \u{1c}--arg; fix: pass --arg\n"
        );
        assert_eq!(
            argument_error("cmd", "one of the arguments --a --b is required"),
            "error: one of the arguments --a --b is required; fix: pass --a --b\n"
        );
    }

    #[test]
    fn differential_runner_records_match_python_except_measured_elapsed_time() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
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
"#,
        ) else {
            return;
        };
        let actual: Vec<_> = [
            ("printf OUT; printf ERR >&2", true, 128),
            ("printf OUT; printf ERR >&2", false, 128),
            ("printf ERR >&2", false, 0),
            ("kill -TERM $$", true, 128),
            ("printf HEADxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxTAIL", true, 30),
        ]
        .into_iter()
        .map(|(code, merge, limit)| {
            let mut value = serde_json::to_value(shell(code, merge, limit)).unwrap();
            value.as_object_mut().unwrap().remove("elapsed_seconds");
            value
        })
        .collect();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}

#[cfg(test)]
mod spawn_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn shebangless_program_is_not_retried_through_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("tool");
        std::fs::write(&program, "echo must-not-run\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error =
            run_bounded(&[program.into_os_string()], Options::new(dir.path(), 5.0)).unwrap_err();
        match error {
            BoundedError::Io {
                source,
                operation: "spawn",
            } => assert_eq!(source.raw_os_error(), Some(crate::sys::errno::ENOEXEC)),
            other => panic!("expected spawn ENOEXEC, got {other:?}"),
        }
    }

    #[test]
    fn relative_path_search_honours_cwd_and_the_supplied_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let program = dir.path().join("bin/tool");
        std::fs::write(&program, "#!/bin/sh\nprintf '%s' \"$KEPT\"\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = Vec::from([
            ("PATH".into(), "bin".into()),
            ("KEPT".into(), "value".into()),
        ]);
        let mut options = Options::new(dir.path(), 5.0);
        options.env = Some(&env);
        let output = run_bounded(&["tool".into()], options).unwrap();
        assert_eq!(output.output, "value");
        assert_eq!(output.returncode, 0);
    }
}

#[cfg(test)]
mod parser_edge_tests {
    #[test]
    fn parser_error_effect_matches_python_stderr_and_exit_status() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json, io
from contextlib import redirect_stderr
from skills._shared.bounded_process import BudgetArgumentParser
out=[]
for command in [None, '', 'custom-cmd']:
    for message in ['unrecognized arguments: --bad\n', 'the following arguments are required: --arg', 'one of the arguments --a --b is required', '\x1c bad\nmessage \x1f']:
        stream=io.StringIO()
        try:
            with redirect_stderr(stream):
                BudgetArgumentParser(command, prog='fixture').error(message)
        except SystemExit as error:
            out.append([error.code,stream.getvalue()])
print(json.dumps(out))
"#,
        ) else {
            return;
        };
        let mut actual = Vec::new();
        for command in [None, Some(""), Some("custom-cmd")] {
            for message in [
                "unrecognized arguments: --bad\n",
                "the following arguments are required: --arg",
                "one of the arguments --a --b is required",
                "\u{1c} bad\nmessage \u{1f}",
            ] {
                let mut output = Vec::new();
                let code =
                    super::write_argument_error(command, "fixture", message, &mut output).unwrap();
                actual.push(serde_json::json!([
                    code,
                    String::from_utf8(output).unwrap()
                ]));
            }
        }
        assert_eq!(serde_json::json!(actual), expected);
        assert!(super::write_argument_error(None, "fixture", "bad", &mut std::io::sink()).is_ok());
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(super::write_argument_error(None, "fixture", "bad", &mut Broken).is_err());
    }
}

#[cfg(test)]
mod descendant_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    const FIXTURE: &str = "bounded::descendant_tests::descendant_fixture";

    #[test]
    fn descendant_fixture() {
        let Some(role) = std::env::var_os("PROVENANT_UTILITY_ROLE") else {
            return;
        };
        let socket = std::env::var_os("PROVENANT_UTILITY_SOCKET").unwrap();
        let ready = std::env::var_os("PROVENANT_UTILITY_READY").unwrap();
        if role == "grandchild" {
            let mut stream = UnixStream::connect(socket).unwrap();
            stream.write_all(&std::process::id().to_ne_bytes()).unwrap();
            let mut buffer = [0];
            // Root holds this connection open until group cleanup closes it.
            let _ = stream.read(&mut buffer);
            return;
        }
        let executable = std::env::current_exe().unwrap();
        let script = crate::process_info::shell_join_os([
            executable.as_os_str(),
            std::ffi::OsStr::new("--exact"),
            std::ffi::OsStr::new(FIXTURE),
            std::ffi::OsStr::new("--nocapture"),
        ]);
        // The clean-parent fixture deliberately exits while its child lives;
        // only the utility owner may reap/clean the abandoned group.
        #[allow(clippy::zombie_processes)]
        let _child = Command::new("/bin/sh")
            .args([
                "-c",
                &format!("trap '' TERM; exec {}", script.to_str().unwrap()),
            ])
            .env("PROVENANT_UTILITY_ROLE", "grandchild")
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !Path::new(&ready).exists() {
            assert!(Instant::now() < deadline, "grandchild did not become ready");
            std::thread::yield_now();
        }
        if role == "parent-timeout" {
            // A wait fixture, never a timing assertion. The root's bounded runner
            // terminates this process only after the readiness handshake.
            std::thread::park_timeout(Duration::from_secs(60));
        }
    }

    fn cleanup_fixture(timeout: bool) {
        // macOS sockaddr_un has a small sun_path; the managed TMPDIR is long.
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let socket = temp.path().join("ready.sock");
        let ready = temp.path().join("ready");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let env = [
            (
                "PROVENANT_UTILITY_ROLE".into(),
                if timeout {
                    "parent-timeout"
                } else {
                    "parent-exit"
                }
                .into(),
            ),
            ("PROVENANT_UTILITY_SOCKET".into(), socket.into_os_string()),
            (
                "PROVENANT_UTILITY_READY".into(),
                ready.clone().into_os_string(),
            ),
        ];
        let executable = std::env::current_exe().unwrap();
        let worker = std::thread::spawn(move || {
            let mut options = Options::new(Path::new("/"), 5.0);
            options.env = Some(&env);
            run_bounded(
                &[
                    executable.into_os_string(),
                    "--exact".into(),
                    FIXTURE.into(),
                    "--nocapture".into(),
                ],
                options,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "grandchild handshake timed out");
                    std::thread::yield_now();
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        // Darwin accepts inherit the listener's nonblocking descriptor flag.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut pid = [0; 4];
        stream.read_exact(&mut pid).unwrap();
        assert_ne!(u32::from_ne_bytes(pid), 0);
        std::fs::write(&ready, b"ready").unwrap();
        let result = worker.join().unwrap().unwrap();
        assert_eq!(result.timed_out, timeout);
        if !timeout {
            assert_eq!(result.returncode, 0);
        }
        // EOF proves the SIGTERM-ignoring descendant released its inherited
        // descriptors; it cannot be a zombie-only PID-probe false positive.
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    }
    #[test]
    fn clean_parent_exit_still_cleans_its_same_group_descendant() {
        cleanup_fixture(false);
    }
    #[test]
    fn timeout_cleans_its_same_group_descendant() {
        cleanup_fixture(true);
    }
}
