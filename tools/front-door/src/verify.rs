//! Run the effective repository verification port.
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_config::{
    CommandExitCodes, ExitDisposition, PortCommand, PortCommandDisposition, PortCommandExecution,
    PortSource, PortTable,
};
use provenant_core::CancelToken;
use provenant_os::exec::{CapturedStream, ExecOutcome, Termination};
use provenant_records::{CanonicalV1, CommandError, CommandResult, Outcome};
use provenant_run::build::{self, Job, JobKind, Origin, Output, Priority, SnapshotSource, Step};
use serde::Serialize;
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const COMMAND: &str = "verify";
use provenant_core::consumer_env::{
    BUILD_CONTEXT, VERIFY_EFFECTIVE_CONFIG_JSON, VERIFY_PROJECT_ROOT, VERIFY_SELECTION, VERIFY_TIER,
};

#[derive(Serialize)]
struct Report {
    tier: String,
    backend: &'static str,
    source: PortSource,
    disposition: Option<PortCommandDisposition>,
    exit_code: Option<i64>,
    elapsed_micros: Option<u64>,
    capture_complete: bool,
    commit: Option<String>,
    strategy: Option<build::StrategyName>,
    slot: Option<u64>,
    exception: Option<build::Exception>,
}

#[allow(
    clippy::disallowed_methods,
    reason = "verify selection samples the production wall clock"
)]
pub fn run(tier: Option<&str>, commit: Option<&str>, affected: bool, globals: &Globals) -> u8 {
    let resolved = match roots::resolve(
        globals,
        roots::Workspace::Checkout,
        roots::Product::Optional,
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            return fail(
                COMMAND,
                Outcome::Invalid,
                "ROOT",
                &error.to_string(),
                globals,
            );
        }
    };
    let project = resolved.project;
    let ports = match PortTable::read_for(Some(&project), resolved.roots.state.as_path()) {
        Ok(ports) => ports,
        Err(error) => {
            return fail(
                COMMAND,
                Outcome::Invalid,
                "PORT_CONFIG",
                &format!("cannot read verify port: {error}"),
                globals,
            );
        }
    };
    let full_after = ports.verify_affected_full_after_duration();
    for warning in &full_after.warnings {
        eprintln!("warning: {warning}");
    }
    let declaration = ports.verify.clone();
    let tier = tier.unwrap_or(&declaration.default_tier).to_owned();
    if !declaration.tiers.contains_key(&tier) {
        return fail(
            COMMAND,
            Outcome::Invalid,
            "VERIFY_TIER",
            &format!("verify tier {tier:?} is not declared by the effective verify port"),
            globals,
        );
    }
    let effective_config = match serde_json::to_string(&declaration) {
        Ok(config) => OsString::from(config),
        Err(error) => {
            return fail(
                COMMAND,
                Outcome::Error,
                "PORT_CONFIG",
                &format!("cannot encode effective verify config for the verify engine: {error}"),
                globals,
            );
        }
    };
    let stored_command_digest = provenant_config::dispatch_port_backend(
        &declaration.backend,
        || None,
        |command| Some(command.digest()),
        || None,
    );
    let ambient: Vec<_> = std::env::vars_os().collect();
    let lane = std::env::var(BUILD_CONTEXT).ok();
    let context = match lane.as_deref() {
        Some(value) => build::BuildContext::from_json(value).map_err(|e| e.to_string()),
        None => crate::admit::host_build_context(&ambient, Some(resolved.roots.state.as_path())),
    };
    let context = match context {
        Ok(context) => context,
        Err(error) => return fail(COMMAND, Outcome::Invalid, "PORT_CONFIG", &error, globals),
    };
    let git = match context.git.clone() {
        Some(git) => git,
        None => {
            return fail(
                COMMAND,
                Outcome::Unavailable,
                "VERIFY_ENGINE_UNAVAILABLE",
                "Git binding unavailable",
                globals,
            );
        }
    };
    let commit = match commit {
        Some(rev) => match resolve_bound_commit(&git, &context.worktree, rev) {
            Some(commit) => Some(commit),
            None => {
                return fail(
                    COMMAND,
                    Outcome::Invalid,
                    "VERIFY_COMMIT",
                    "cannot resolve verify commit",
                    globals,
                );
            }
        },
        None => None,
    };
    let checkout = match commit.as_deref() {
        Some(commit) => match DetachedCheckout::new(&project, &ports, commit, "verify") {
            Ok(checkout) => Some(checkout),
            Err(error) => {
                return fail(
                    COMMAND,
                    Outcome::Unavailable,
                    "VERIFY_ENGINE_UNAVAILABLE",
                    &error,
                    globals,
                );
            }
        },
        None => None,
    };
    let source = match &commit {
        Some(commit) => SnapshotSource::Commit {
            repo_common_dir: git.common_dir.clone(),
            commit: commit.clone(),
        },
        None => SnapshotSource::WorkingTree {
            worktree: context.worktree.clone(),
            git,
        },
    };
    let (backend, command, should_record) = match &declaration.backend {
        provenant_config::PortBackend::Builtin => {
            let script = resolved.assets.join("scripts/verify-tier");
            if !script.is_file() {
                return fail(
                    COMMAND,
                    Outcome::Unavailable,
                    "VERIFY_ENGINE_UNAVAILABLE",
                    "the builtin verify tier engine is not present in the selected product assets",
                    globals,
                );
            }
            (
                "builtin",
                PortCommand {
                    argv: vec![
                        "python3".into(),
                        script.to_string_lossy().into_owned(),
                        "--project".into(),
                        ".".into(),
                        "--tier".into(),
                        tier.clone(),
                    ],
                    exit_codes: CommandExitCodes {
                        success: vec![0],
                        unknown: Vec::new(),
                        other: ExitDisposition::Refused,
                    },
                },
                false,
            )
        }
        provenant_config::PortBackend::Command { command } => {
            ("command", substitute_tier(command, &tier), true)
        }
        provenant_config::PortBackend::None => {
            return render(
                Report {
                    tier: tier.clone(),
                    backend: "none",
                    source: declaration.source,
                    disposition: None,
                    exit_code: None,
                    elapsed_micros: None,
                    capture_complete: false,
                    commit: None,
                    strategy: None,
                    slot: None,
                    exception: None,
                },
                Outcome::Unavailable,
                Some((
                    "VERIFY_UNAVAILABLE",
                    "the effective verify port is disabled".into(),
                )),
                None,
                globals,
            );
        }
    };
    let lane_markers: Vec<_> = ambient
        .iter()
        .filter(|(key, _)| {
            [
                "PROVENANT_ADMISSION_WAIT_DIR",
                "PROVENANT_ADMISSION_CONFIG",
                "PROVENANT_ADMISSION_SLOTS",
                "PROVENANT_ADMISSION_WAIT_SECONDS",
                "PROVENANT_ATTEMPT_ID",
                "PROVENANT_PARENT_ATTEMPT_ID",
            ]
            .iter()
            .any(|name| key == name)
        })
        .cloned()
        .collect();
    let mut env = provenant_config::port_command_environment(
        ambient,
        &[
            (
                OsString::from(VERIFY_EFFECTIVE_CONFIG_JSON),
                effective_config.clone(),
            ),
            (OsString::from(VERIFY_TIER), OsString::from(&tier)),
        ],
    );
    env.extend(lane_markers);
    if let Some(checkout) = &checkout {
        env.push((
            "PROVENANT_BUILD_LOCAL_ROOT".into(),
            checkout.path.as_os_str().to_owned(),
        ));
    }
    let workspace = declaration.workspace.clone();
    let base = context
        .verify_base
        .as_ref()
        .map(|base| base.commit.clone())
        .unwrap_or_else(|| declaration.base.clone());
    let policy = provenant_select::Policy {
        global_paths: declaration.affected.global_paths.clone(),
        full_every: u32::try_from(declaration.affected.full_every).unwrap_or(u32::MAX),
        full_after: full_after.value,
    };
    let do_select = affected || declaration.affected.enabled;
    let state = resolved.roots.state.as_path().to_path_buf();
    let project_key = context.project_key.clone();
    let explicit_head = commit.clone();
    let prelude_tier = tier.clone();
    let prelude = Box::new(move |site: &build::ExecSite| {
        let mut extra = vec![
            (
                OsString::from(VERIFY_PROJECT_ROOT),
                site.root.as_os_str().to_owned(),
            ),
            (
                OsString::from(VERIFY_EFFECTIVE_CONFIG_JSON),
                effective_config,
            ),
            (OsString::from(VERIFY_TIER), OsString::from(prelude_tier)),
        ];
        if do_select {
            let head = site
                .head
                .as_deref()
                .or(explicit_head.as_deref())
                .unwrap_or("HEAD");
            match provenant_select::select(&site.root.join(workspace), &base, head, &policy, &state, &project_key, SystemTime::now()) {
                Ok(selection) => extra.push((OsString::from(VERIFY_SELECTION), OsString::from(serde_json::json!({"packages": selection.packages, "full": selection.full, "reason": selection.reason.to_string()}).to_string()))),
                Err(error) => eprintln!("warning: affected selection unavailable: {error}"),
            }
        }
        Ok(extra)
    });
    let priority = context.port.priority_for("tier");
    for warning in &priority.warnings {
        eprintln!("warning: {warning}");
    }
    let priority = match priority.value {
        provenant_config::BuildPriority::Interactive => Priority::Interactive,
        provenant_config::BuildPriority::Test => Priority::Test,
        provenant_config::BuildPriority::Train => Priority::Train,
        provenant_config::BuildPriority::Prewarm => Priority::Prewarm,
    };
    let job = Job {
        id: format!(
            "verify-{}-{}",
            std::process::id(),
            build::ledger::now_unix_ms()
        ),
        project_key: context.project_key.clone(),
        project: context.project.clone(),
        origin: if lane.is_some() {
            Origin::Lane
        } else {
            Origin::Chair
        },
        owner: context
            .attempt
            .clone()
            .unwrap_or_else(|| format!("chair-{}", std::process::id())),
        kind: JobKind::Tier { name: tier.clone() },
        priority,
        source,
        verify_base: context.verify_base.clone(),
        steps: vec![Step {
            cwd_rel: PathBuf::new(),
            argv: command.argv.iter().map(OsString::from).collect(),
        }],
        stop_on_failure: true,
        env,
        output: Output::Capture {
            limit_bytes: 8 * 1024 * 1024,
        },
        timeout: Some(Duration::from_secs(
            provenant_core::MAX_LANE_RUNTIME_SECONDS,
        )),
        sandboxed: context.sandboxed,
        prelude: Some(prelude),
    };
    let run_context = build::RunContext {
        state: resolved.roots.state.as_path().to_path_buf(),
        home: context.host_home,
        strategy: build::strategy(&context.port).value,
        cancel: CancelToken::new(),
    };
    let result = build::run(job, &run_context);
    let (mut execution, build_outcome) = match result {
        Ok(mut result) => {
            if result.outcome.snapshot.is_none() {
                result.outcome.snapshot = commit.clone();
            }
            (
                execution_from_job(&command, &result, backend == "builtin"),
                Some(result.outcome),
            )
        }
        Err(build::BuildError::Cancelled) => {
            let mut execution = PortCommandExecution::failed(&command);
            execution.disposition = PortCommandDisposition::Cancelled;
            (execution, None)
        }
        Err(error) if backend == "builtin" => {
            return fail(
                COMMAND,
                Outcome::Unavailable,
                "VERIFY_ENGINE_UNAVAILABLE",
                &error.to_string(),
                globals,
            );
        }
        Err(_) => (PortCommandExecution::failed(&command), None),
    };
    if let Some(digest) = stored_command_digest {
        execution.command_digest = digest;
    }
    let selected: Result<_, u8> = Ok((backend, declaration.source, execution, should_record));
    let (backend, source, mut execution, should_record) = match selected {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if execution.disposition == PortCommandDisposition::Succeeded && !execution.capture_complete() {
        execution.disposition = PortCommandDisposition::Unknown;
    }
    if should_record {
        let key = format!(
            "{}:{tier}",
            CanonicalV1::file_digest(project.as_os_str().as_encoded_bytes())
        );
        let publication = match provenant_config::record_port_command(
            resolved.roots.state.as_path(),
            "verify",
            source,
            "tier",
            &key,
            &execution,
            provenant_config::PortReceiptLifetime::AgeDays {
                days: provenant_config::DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS,
            },
        ) {
            Ok(publication) => publication,
            Err(error) => {
                return render(
                    report(&tier, backend, source, &execution, build_outcome.as_ref()),
                    Outcome::Error,
                    Some((
                        "VERIFY_EVIDENCE",
                        format!("cannot record verify command evidence: {error}"),
                    )),
                    Some(&execution),
                    globals,
                );
            }
        };
        if let provenant_config::PortCommandPublication::VisibleDurabilityUnconfirmed { cause } =
            publication
        {
            return render(
                report(&tier, backend, source, &execution, build_outcome.as_ref()),
                Outcome::Error,
                Some((
                    "VERIFY_EVIDENCE_VISIBLE",
                    format!("verify receipt is visible but durability is unconfirmed: {cause}"),
                )),
                Some(&execution),
                globals,
            );
        }
    }
    let status = outcome(execution.disposition);
    let message = failure_message(execution.disposition);
    render(
        report(&tier, backend, source, &execution, build_outcome.as_ref()),
        status,
        message,
        Some(&execution),
        globals,
    )
}

fn execution_from_job(
    command: &PortCommand,
    result: &build::JobResult,
    builtin: bool,
) -> PortCommandExecution {
    let outcome = &result.outcome;
    let exit = outcome.exit.unwrap_or_default();
    let termination = exit
        .code
        .map(|code| Termination::Exit(i64::from(code)))
        .unwrap_or_else(|| Termination::Signal(i64::from(exit.signal.unwrap_or(0))));
    let captured = result.captured.clone().unwrap_or_default();
    let disposition = if outcome.timed_out {
        PortCommandDisposition::TimedOut
    } else if outcome.cancelled {
        PortCommandDisposition::Cancelled
    } else if let Some(code) = exit.code {
        if command.exit_codes.success.contains(&code) {
            PortCommandDisposition::Succeeded
        } else if command.exit_codes.unknown.contains(&code)
            || command.exit_codes.other == ExitDisposition::Unknown
        {
            PortCommandDisposition::Unknown
        } else if builtin {
            PortCommandDisposition::Failed
        } else {
            PortCommandDisposition::Refused
        }
    } else {
        PortCommandDisposition::Failed
    };
    let stdout = CapturedStream {
        bytes: captured.stdout,
        total_bytes: captured.stdout_total_bytes,
        truncated: captured.stdout_truncated,
        ..Default::default()
    };
    let mut stderr_bytes = captured.stderr;
    let stderr_truncated = captured.stderr_truncated || stderr_bytes.len() > 64 * 1024;
    if stderr_bytes.len() > 64 * 1024 {
        stderr_bytes.drain(..stderr_bytes.len() - 64 * 1024);
    }
    let stderr = CapturedStream {
        bytes: stderr_bytes,
        total_bytes: captured.stderr_total_bytes,
        truncated: stderr_truncated,
        ..Default::default()
    };
    PortCommandExecution {
        command_digest: command.digest(),
        disposition,
        outcome: Some(ExecOutcome {
            termination,
            stdout,
            stderr,
            timed_out: outcome.timed_out,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: outcome.cancelled,
            elapsed_micros: outcome.wall_ms.saturating_mul(1000),
        }),
    }
}

/// Resolve revisions in the lane's frozen Git view, not the host checkout's HEAD.
#[allow(
    clippy::disallowed_methods,
    reason = "resolve a revision at the Git boundary"
)]
fn resolve_bound_commit(
    git: &build::GitBinding,
    worktree: &Path,
    revision: &str,
) -> Option<String> {
    let mut command = std::process::Command::new(&git.git);
    command
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(format!("{revision}^{{commit}}"));
    command
        .env("GIT_DIR", &git.git_dir)
        .env("GIT_COMMON_DIR", &git.common_dir)
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_INDEX_FILE", &git.index_file);
    if let Some(object_dir) = &git.object_dir {
        command.env("GIT_OBJECT_DIRECTORY", object_dir);
    } else {
        command.env_remove("GIT_OBJECT_DIRECTORY");
    }
    if let Some(config) = &git.config_global {
        command.env("GIT_CONFIG_GLOBAL", config);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!commit.is_empty() && commit.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(commit)
}

/// Temporary detached checkout shared by verify and command gates.
pub(crate) struct DetachedCheckout {
    project: PathBuf,
    pub(crate) path: PathBuf,
}
impl DetachedCheckout {
    #[allow(clippy::disallowed_methods, reason = "git worktree lifecycle")]
    pub(crate) fn new(
        project: &Path,
        ports: &PortTable,
        commit: &str,
        purpose: &str,
    ) -> Result<Self, String> {
        let branch = format!(
            "{purpose}-{}-{}-{}",
            &commit[..commit.len().min(12)],
            std::process::id(),
            build::ledger::now_unix_ms()
        );
        let path = project.join(
            ports
                .managed_writer_path(&branch)
                .map_err(|e| e.to_string())?,
        );
        if path.exists() {
            return Err("checkout already exists".into());
        }
        std::fs::create_dir_all(path.parent().ok_or("checkout has no parent")?)
            .map_err(|e| e.to_string())?;
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(project)
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .arg(commit)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "cannot create checkout: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(Self {
            project: project.to_path_buf(),
            path,
        })
    }
}
impl Drop for DetachedCheckout {
    #[allow(clippy::disallowed_methods, reason = "git worktree lifecycle")]
    fn drop(&mut self) {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.project)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        if !matches!(result, Ok(output) if output.status.success()) {
            eprintln!(
                "warning: failed to remove temporary checkout {}",
                self.path.display()
            );
        }
    }
}

fn substitute_tier(command: &PortCommand, tier: &str) -> PortCommand {
    let mut expanded = command.clone();
    expanded.argv = command
        .argv
        .iter()
        .map(|part| part.replace("{tier}", tier))
        .collect();
    expanded
}

fn report(
    tier: &str,
    backend: &'static str,
    source: PortSource,
    execution: &PortCommandExecution,
    build_outcome: Option<&build::JobOutcome>,
) -> Report {
    Report {
        tier: tier.to_owned(),
        backend,
        source,
        disposition: Some(execution.disposition),
        exit_code: execution
            .outcome
            .as_ref()
            .and_then(|outcome| match outcome.termination {
                Termination::Exit(code) => Some(code),
                Termination::Signal(_) => None,
            }),
        elapsed_micros: execution
            .outcome
            .as_ref()
            .map(|outcome| outcome.elapsed_micros),
        capture_complete: execution.capture_complete(),
        commit: build_outcome.and_then(|o| o.snapshot.clone()),
        strategy: build_outcome.map(|o| o.strategy),
        slot: build_outcome.and_then(|o| o.slot),
        exception: build_outcome.and_then(|o| o.exception),
    }
}

fn outcome(disposition: PortCommandDisposition) -> Outcome {
    match disposition {
        PortCommandDisposition::Succeeded => Outcome::Ok,
        PortCommandDisposition::Refused => Outcome::Refused,
        PortCommandDisposition::TimedOut => Outcome::Timeout,
        PortCommandDisposition::Cancelled => Outcome::Cancelled,
        PortCommandDisposition::Unknown | PortCommandDisposition::Failed => Outcome::Error,
    }
}

fn failure_message(disposition: PortCommandDisposition) -> Option<(&'static str, String)> {
    let (code, message) = match disposition {
        PortCommandDisposition::Succeeded => return None,
        PortCommandDisposition::Unknown => ("VERIFY_UNKNOWN", "verify command result is unknown"),
        PortCommandDisposition::Refused => {
            ("VERIFY_REFUSED", "verify command refused the selected tier")
        }
        PortCommandDisposition::TimedOut => (
            "VERIFY_TIMEOUT",
            "verify command exceeded its execution time limit",
        ),
        PortCommandDisposition::Cancelled => ("VERIFY_CANCELLED", "verify command was cancelled"),
        PortCommandDisposition::Failed => ("VERIFY_FAILED", "verification failed"),
    };
    Some((code, message.into()))
}

fn render(
    report: Report,
    status: Outcome,
    error: Option<(&'static str, String)>,
    execution: Option<&PortCommandExecution>,
    globals: &Globals,
) -> u8 {
    let output_error = if !globals.json {
        execution
            .and_then(|execution| execution.outcome.as_ref())
            .and_then(|outcome| {
                std::io::stdout()
                    .lock()
                    .write_all(&outcome.stdout.bytes)
                    .and_then(|()| std::io::stderr().lock().write_all(&outcome.stderr.bytes))
                    .err()
            })
    } else {
        None
    };
    let (status, error) = match output_error {
        Some(error) => (
            Outcome::Error,
            Some((
                "VERIFY_OUTPUT",
                format!("cannot render verify output: {error}"),
            )),
        ),
        None => (status, error),
    };
    let message = match &error {
        Some((_, message)) => format!("{message}: tier {}", report.tier),
        None => format!(
            "verify tier {} completed via {} ({:?})",
            report.tier, report.backend, report.source
        ),
    };
    let mut result = CommandResult::new(COMMAND, status, report);
    if let Some((code, message)) = error {
        result.error = Some(CommandError {
            diagnostic: None,
            code: code.into(),
            message,
            cause: None,
            repair: None,
        });
    }
    emit(&result, globals, &message, false)
}
