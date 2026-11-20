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
    os::unix::ffi::OsStrExt,
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
    outcome: JobOutcome,
    place: ExecPlace,
    override_argv: Option<Vec<OsString>>,
    waited_ms: u64,
    override_env: Option<Vec<(OsString, OsString)>>,
) -> Result<JobResult, super::DispatchFailure> {
    execute_configured(
        job,
        ctx,
        outcome,
        place,
        override_argv,
        waited_ms,
        override_env,
        None,
        None,
    )
}

pub(super) fn execute_in_slot(
    job: &mut Job,
    ctx: &RunContext,
    outcome: JobOutcome,
    index: u64,
    root: &Path,
    head: &str,
    env: Vec<(OsString, OsString)>,
    worktree: &Path,
    on_spawn: &mut dyn FnMut(u32),
) -> Result<JobResult, super::DispatchFailure> {
    execute_configured(
        job,
        ctx,
        outcome,
        ExecPlace::Slot,
        None,
        0,
        Some(env),
        Some((index, root, head, worktree)),
        Some(on_spawn),
    )
}

fn execute_configured(
    job: &mut Job,
    ctx: &RunContext,
    mut outcome: JobOutcome,
    place: ExecPlace,
    override_argv: Option<Vec<OsString>>,
    waited_ms: u64,
    override_env: Option<Vec<(OsString, OsString)>>,
    slot: Option<(u64, &Path, &str, &Path)>,
    mut on_spawn: Option<&mut dyn FnMut(u32)>,
) -> Result<JobResult, super::DispatchFailure> {
    let root = slot.map_or_else(
        || super::job_root(job).to_path_buf(),
        |(_, root, _, _)| root.to_path_buf(),
    );
    if job.steps.is_empty() {
        let result = finish(&mut outcome, 0, None, None);
        return Err((
            BuildError::Config("build job has no steps".into()),
            Box::new(result),
        ));
    }
    let site = ExecSite {
        root: root.clone(),
        head: slot.map(|(_, _, head, _)| head.to_owned()),
        slot: slot.map(|(index, _, _, _)| index).or(outcome.slot),
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
        let mut argv = if index == 0 {
            override_argv.clone().unwrap_or_else(|| step.argv.clone())
        } else {
            step.argv.clone()
        };
        if let Some((_, _, _, worktree)) = slot {
            argv = super::rust::rewrite_argv(&argv, worktree, &root);
        }
        if argv.is_empty() {
            outcome.infra = None;
            let result = finish(&mut outcome, elapsed_ms(&clock, started_micros), None, None);
            return Err((
                BuildError::Config("build step has empty argv".into()),
                Box::new(result),
            ));
        }
        let cwd = if let Some((_, _, _, worktree)) = slot {
            let original = if step.cwd_rel.is_absolute() {
                step.cwd_rel.clone()
            } else {
                worktree.join(&step.cwd_rel)
            };
            super::rust::rewrite_argv(&[original.into_os_string()], worktree, &root)
                .into_iter()
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join(&step.cwd_rel))
        } else if step.cwd_rel.is_absolute() {
            step.cwd_rel.clone()
        } else {
            root.join(&step.cwd_rel)
        };
        let remaining =
            Duration::from_micros(deadline_micros.saturating_sub(clock.mono_now().as_micros()));
        let mut notify_spawn = |pid| {
            if let Some(callback) = on_spawn.as_mut() {
                callback(pid);
            }
        };
        let mut options = ExecOptions::new(&argv, &cwd, remaining);
        options.env = Some(&env);
        options.process_group = true;
        options.cleanup_process_group = true;
        options.group_watchdog = false;
        options.terminate_grace = Duration::from_secs(10);
        options.cancel = Some(&ctx.cancel);
        options.on_spawn = Some(&mut notify_spawn);
        let mut stdout_filter =
            slot.map(|(_, _, _, worktree)| StreamPathFilter::new(&root, worktree));
        let mut stderr_filter =
            slot.map(|(_, _, _, worktree)| StreamPathFilter::new(&root, worktree));
        let stdout_handle = std::io::stdout();
        let stderr_handle = std::io::stderr();
        let mut stdout_lock = stdout_handle.lock();
        let mut stderr_lock = stderr_handle.lock();
        match job.output {
            Output::Stream => {
                if slot.is_some() {
                    options.stdout = StreamPolicy::Capture { limit: 0 };
                    options.stderr = StreamPolicy::Capture { limit: 0 };
                    options.stdout_writer = Some(&mut stdout_lock);
                    options.stderr_writer = Some(&mut stderr_lock);
                    options.stdout_filter = stdout_filter
                        .as_mut()
                        .map(|filter| filter as &mut dyn provenant_os::exec::CaptureFilter);
                    options.stderr_filter = stderr_filter
                        .as_mut()
                        .map(|filter| filter as &mut dyn provenant_os::exec::CaptureFilter);
                } else {
                    options.stdout = StreamPolicy::Inherit;
                    options.stderr = StreamPolicy::Inherit;
                }
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
        if output.stdout.sink_failed
            || output.stderr.sink_failed
            || output.stdout.filter_failed
            || output.stderr.filter_failed
        {
            outcome.infra = Some(InfraKind::SlotLayout);
            outcome.exit = None;
            let result = finish(&mut outcome, elapsed_ms(&clock, started_micros), None, None);
            return Err((
                BuildError::Infra {
                    kind: InfraKind::SlotLayout,
                    detail: "slot output mapping or writing failed after spawn".into(),
                },
                Box::new(result),
            ));
        }
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
        if !(slot.is_some() && matches!(job.output, Output::Stream))
            && let Some(capture) = capture(output.stdout, output.stderr)
        {
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

struct StreamPathFilter {
    from: Vec<u8>,
    to: Vec<u8>,
    line: Vec<u8>,
    passthrough: bool,
}
impl StreamPathFilter {
    fn new(from: &Path, to: &Path) -> Self {
        Self {
            from: from.as_os_str().as_bytes().to_vec(),
            to: to.as_os_str().as_bytes().to_vec(),
            line: Vec::new(),
            passthrough: false,
        }
    }
    fn emit_chunks(bytes: &[u8], sink: &mut dyn FnMut(&[u8])) {
        for chunk in bytes.chunks(provenant_os::exec::FRAME_LIMIT) {
            sink(chunk);
        }
    }
    fn emit_line(&mut self, sink: &mut dyn FnMut(&[u8])) {
        if self.passthrough {
            Self::emit_chunks(&self.line, sink);
        } else {
            let mut mapped = Vec::new();
            if super::pathmap::map(self.line.as_slice(), &mut mapped, &self.from, &self.to).is_ok()
            {
                Self::emit_chunks(&mapped, sink);
            } else {
                Self::emit_chunks(&self.line, sink);
            }
        }
        self.line.clear();
        self.passthrough = false;
    }
}
impl provenant_os::exec::CaptureFilter for StreamPathFilter {
    fn filter(
        &mut self,
        bytes: &[u8],
        sink: &mut dyn FnMut(&[u8]),
    ) -> Result<(), provenant_os::exec::FilterError> {
        const LIMIT: usize = 1024 * 1024;
        let mut start = 0;
        for (index, byte) in bytes.iter().enumerate() {
            if self.passthrough {
                if *byte == b'\n' {
                    Self::emit_chunks(&bytes[start..=index], sink);
                    start = index + 1;
                    self.passthrough = false;
                }
                continue;
            }
            self.line.push(*byte);
            if self.line.len() > LIMIT {
                Self::emit_chunks(&self.line, sink);
                self.line.clear();
                self.passthrough = true;
                start = index + 1;
            } else if *byte == b'\n' {
                self.emit_line(sink);
                start = index + 1;
            }
        }
        if self.passthrough && start < bytes.len() {
            Self::emit_chunks(&bytes[start..], sink);
        }
        Ok(())
    }
    fn finish(
        &mut self,
        sink: &mut dyn FnMut(&[u8]),
    ) -> Result<(), provenant_os::exec::FilterError> {
        if !self.line.is_empty() {
            self.emit_line(sink);
        }
        Ok(())
    }
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
