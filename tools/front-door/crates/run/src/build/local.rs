//! Process execution shared by the lane, lane_seed and command strategies.

use super::{
    BuildError, Captured, ExecPlace, ExecSite, ExitStatus, InfraKind, Job, JobOutcome, JobResult,
    Output, RunContext,
};
use provenant_core::Clock;
use provenant_os::exec::{
    ExecOptions, MAX_CAPTURE_LIMIT, StreamPolicy, SystemExecutor, Termination,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

/// Execute every step locally in one process group, forwarding signals and
/// retaining at most the caller's configured capture bound.
pub(super) fn execute(
    job: &mut Job,
    ctx: &RunContext,
    outcome: JobOutcome,
    place: ExecPlace,
    override_argv: Option<Vec<OsString>>,
    waited_ms: u64,
) -> Result<JobResult, super::DispatchFailure> {
    execute_with_env(job, ctx, outcome, place, override_argv, waited_ms, None)
}

pub(super) fn execute_with_env(
    job: &mut Job,
    ctx: &RunContext,
    mut outcome: JobOutcome,
    place: ExecPlace,
    override_argv: Option<Vec<OsString>>,
    waited_ms: u64,
    override_env: Option<Vec<(OsString, OsString)>>,
) -> Result<JobResult, super::DispatchFailure> {
    let root = super::job_root(job).to_path_buf();
    if job.steps.is_empty() {
        let result = finish(&mut outcome, 0, None, None);
        return Err((
            BuildError::Config("build job has no steps".into()),
            Box::new(result),
        ));
    }
    let site = ExecSite {
        root: root.clone(),
        head: None,
        slot: outcome.slot,
    };
    let mut env = override_env.unwrap_or_else(|| job.env.clone());
    if let Some(prelude) = job.prelude.take() {
        match prelude(&site) {
            Ok(extra) => {
                for (key, value) in extra {
                    super::set_env(&mut env, key, value);
                }
            }
            Err(error) => {
                outcome.infra = Some(InfraKind::SlotLayout);
                return Err((error, Box::new(finish(&mut outcome, 0, None, None))));
            }
        }
    }

    let timeout = job
        .timeout
        .unwrap_or_else(|| Duration::from_secs(provenant_core::MAX_LANE_RUNTIME_SECONDS));
    let executor = match SystemExecutor::new(4) {
        Ok(executor) => executor,
        Err(error) => {
            outcome.infra = Some(InfraKind::SlotLayout);
            return Err((
                BuildError::Unavailable(error.to_string()),
                Box::new(finish(&mut outcome, 0, None, None)),
            ));
        }
    };
    let clock = provenant_os::clock::SystemClock::new();
    let started_micros = clock.mono_now().as_micros();
    let timeout_micros = timeout.as_micros().min(u128::from(u64::MAX)) as u64;
    let deadline_micros = started_micros.saturating_add(timeout_micros);
    outcome.exec = place;
    outcome.waited_ms = waited_ms;
    let mut exits = Vec::new();
    let mut captures: Option<Captured> = None;

    for (index, step) in job.steps.iter().enumerate() {
        let argv = if index == 0 {
            override_argv.clone().unwrap_or_else(|| step.argv.clone())
        } else {
            step.argv.clone()
        };
        if argv.is_empty() {
            outcome.infra = None;
            let result = finish(&mut outcome, elapsed_ms(&clock, started_micros), None, None);
            return Err((
                BuildError::Config("build step has empty argv".into()),
                Box::new(result),
            ));
        }
        let cwd = if step.cwd_rel.is_absolute() {
            step.cwd_rel.clone()
        } else {
            root.join(&step.cwd_rel)
        };
        let remaining =
            Duration::from_micros(deadline_micros.saturating_sub(clock.mono_now().as_micros()));
        let mut options = ExecOptions::new(&argv, &cwd, remaining);
        options.env = Some(&env);
        options.process_group = true;
        options.cleanup_process_group = true;
        options.group_watchdog = false;
        options.terminate_grace = Duration::from_secs(10);
        options.cancel = Some(&ctx.cancel);
        match job.output {
            Output::Stream => {
                options.stdout = StreamPolicy::Inherit;
                options.stderr = StreamPolicy::Inherit;
            }
            Output::Capture { limit_bytes } => {
                let limit = limit_bytes.min(MAX_CAPTURE_LIMIT);
                options.stdout = StreamPolicy::Capture { limit };
                options.stderr = StreamPolicy::Capture { limit };
            }
        }
        let output = match executor.execute_forwarding_group(options) {
            Ok(output) => output,
            Err(error) => {
                if matches!(error, provenant_os::exec::ExecError::Cancelled) {
                    outcome.cancelled = true;
                    let result = finish(
                        &mut outcome,
                        elapsed_ms(&clock, started_micros),
                        None,
                        captures,
                    );
                    return Err((BuildError::Cancelled, Box::new(result)));
                }
                outcome.infra = Some(InfraKind::SlotLayout);
                let result = finish(
                    &mut outcome,
                    elapsed_ms(&clock, started_micros),
                    None,
                    captures,
                );
                return Err((
                    BuildError::Infra {
                        kind: InfraKind::SlotLayout,
                        detail: error.to_string(),
                    },
                    Box::new(result),
                ));
            }
        };
        let exit = match output.termination {
            Termination::Exit(code) => ExitStatus::code(i32::try_from(code).unwrap_or(i32::MAX)),
            Termination::Signal(signal) => ExitStatus::signal(i32::try_from(signal).unwrap_or(0)),
        };
        if let Some(signal) = exit.signal
            && matches!(
                signal,
                libc::SIGSEGV | libc::SIGBUS | libc::SIGILL | libc::SIGABRT
            )
        {
            outcome.infra = Some(InfraKind::CrashSignal);
        }
        if output.timed_out {
            outcome.timed_out = true;
        }
        if output.cancelled {
            outcome.cancelled = true;
        }
        exits.push(Some(exit));
        if let Some(capture) = capture(output.stdout, output.stderr) {
            let bound = match job.output {
                Output::Capture { limit_bytes } => limit_bytes.min(MAX_CAPTURE_LIMIT),
                Output::Stream => 0,
            };
            captures = Some(merge_capture(captures, capture, bound));
        }
        if job.stop_on_failure && exit.process_code() != 0 {
            break;
        }
    }

    outcome.exit = exits.last().copied().flatten();
    let elapsed_ms = elapsed_ms(&clock, started_micros);
    outcome.wall_ms = elapsed_ms;
    outcome.finished_unix_ms = super::ledger::now_unix_ms();
    Ok(JobResult {
        outcome,
        step_exits: exits,
        captured: captures,
    })
}

/// Return the execution root for local command paths.
pub fn execution_root(job: &Job) -> PathBuf {
    super::job_root(job).to_path_buf()
}

/// Complete a successful slot execution result; S1 supplies the populated
/// result and S0 keeps this seam explicit.
pub(super) fn finish_slot_execution(
    execution: super::slots::SlotExecution,
    mut outcome: JobOutcome,
) -> Result<JobResult, super::DispatchFailure> {
    outcome.exec = ExecPlace::Slot;
    outcome.snapshot = execution.snapshot;
    outcome.prev_snapshot = execution.prev_snapshot;
    outcome.slot_changes = execution.slot_changes;
    outcome.exit = execution.exit;
    outcome.finished_unix_ms = super::ledger::now_unix_ms();
    Ok(JobResult {
        outcome,
        step_exits: execution.step_exits,
        captured: execution.captured,
    })
}

fn capture(
    stdout: provenant_os::exec::CapturedStream,
    stderr: provenant_os::exec::CapturedStream,
) -> Option<Captured> {
    if stdout.total_bytes == 0 && stderr.total_bytes == 0 {
        return None;
    }
    Some(Captured {
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        stdout_total_bytes: stdout.total_bytes,
        stderr_total_bytes: stderr.total_bytes,
    })
}

fn merge_capture(current: Option<Captured>, next: Captured, bound: usize) -> Captured {
    let Some(mut current) = current else {
        return next;
    };
    current.stdout_truncated |= next.stdout_truncated;
    current.stderr_truncated |= next.stderr_truncated;
    current.stdout_total_bytes = current
        .stdout_total_bytes
        .saturating_add(next.stdout_total_bytes);
    current.stderr_total_bytes = current
        .stderr_total_bytes
        .saturating_add(next.stderr_total_bytes);
    current.stdout.extend(next.stdout);
    current.stderr.extend(next.stderr);
    if current.stdout.len() > bound {
        current.stdout.truncate(bound);
        current.stdout_truncated = true;
    }
    if current.stderr.len() > bound {
        current.stderr.truncate(bound);
        current.stderr_truncated = true;
    }
    current
}

fn finish(
    outcome: &mut JobOutcome,
    elapsed_ms: u64,
    exit: Option<ExitStatus>,
    _captured: Option<Captured>,
) -> JobOutcome {
    outcome.wall_ms = elapsed_ms;
    outcome.finished_unix_ms = super::ledger::now_unix_ms();
    outcome.exit = exit;
    outcome.clone()
}

fn elapsed_ms(clock: &provenant_os::clock::SystemClock, started_micros: u64) -> u64 {
    clock.mono_now().as_micros().saturating_sub(started_micros) / 1_000
}

/// Build a command's final working directory without canonicalising it.
pub fn step_cwd(root: &Path, cwd_rel: &Path) -> PathBuf {
    if cwd_rel.is_absolute() {
        cwd_rel.to_path_buf()
    } else {
        root.join(cwd_rel)
    }
}
