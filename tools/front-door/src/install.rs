//! Process-edge wiring for installation. State operations stay in config;
//! every child is supervised with a bounded wait and isolated environment.
use crate::release_smoke::SystemSmoke;
use crate::roots::{self, Product, Resolved, Workspace};
use provenant_cli as cli;
use provenant_config::{
    Action, HarnessTarget, InstallContext, InstallError, InstallFinding, InstallOutcome,
    InstallReporter, ManualInclude, Targets,
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_runtime::peer_agent::{AgentError, launchctl};
use provenant_os::clock::SystemClock;
use provenant_records::{CommandError, CommandResult, Harness, Outcome};
use serde::Serialize;
use std::ffi::OsStr;
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
enum EdgeError {
    Root(roots::RootError),
    Install(InstallError),
    Home,
    Peer(provenant_fabric_runtime::peer_agent::AgentError),
}

fn prepare_agents(targets: &mut Targets) -> Result<(), EdgeError> {
    let state = targets.roots.state.as_path();
    if provenant_fabric_runtime::peer_agent::eligible(state, &targets.home) {
        let program = targets.bin_dir.join("provenant");
        let previous = provenant_config::installed_binary(targets)
            .map_err(EdgeError::Install)?
            .unwrap_or_else(|| program.clone());
        targets.peer_agent = Some(
            provenant_fabric_runtime::peer_agent::install_plist(
                &program,
                &targets.roots,
                peer_agent_registry_name,
            )
            .map_err(EdgeError::Peer)?,
        );
        targets.peer_agent_previous =
            provenant_fabric_runtime::peer_agent::previous_plist(&previous, state, &targets.home)
                .map_err(EdgeError::Peer)?;
        let config = provenant_config::read_with_domains(&targets.roots, &cli::domains::SECTIONS)
            .map_err(|error| EdgeError::Install(InstallError::Config(error)))?;
        if config.gc.schedule {
            targets.gc_agent = Some(
                super::gc_agent::render(&program, state, &config.gc).map_err(EdgeError::Peer)?,
            );
        }
    }
    Ok(())
}

fn peer_agent_registry_name(path: &Path) -> std::ffi::OsString {
    provenant_confinement::worktrees::state_dir_name(path)
}

fn environment(globals: &cli::Globals) -> Result<Resolved, EdgeError> {
    roots::resolve(globals, Workspace::Project, Product::Required).map_err(|error| match error {
        roots::RootError::Home => EdgeError::Home,
        error => EdgeError::Root(error),
    })
}

fn selected_targets(
    globals: &cli::Globals,
    harnesses: &[Harness],
) -> Result<(Resolved, Targets), EdgeError> {
    let environment = environment(globals)?;
    let mut targets = cli::install::configured_targets(
        environment.roots.clone(),
        &environment.home,
        harnesses,
        environment.claude_config.as_deref(),
        environment.codex_home.as_deref(),
    )
    .map_err(EdgeError::Install)?;
    if !environment
        .assets
        .starts_with(targets.roots.state.as_path().join("releases"))
    {
        targets.protected_roots.push(environment.assets.clone());
    }
    Ok((environment, targets))
}

/// Resolves targets as install does, so `CLAUDE_CONFIG_DIR`/`CODEX_HOME`
/// select the matching recorded target. Without them, uninstall acts on every
/// recorded target of each harness, at its recorded paths.
fn receipt_targets(globals: &cli::Globals, harnesses: &[Harness]) -> Result<Targets, EdgeError> {
    let environment = environment(globals)?;
    let mut targets = cli::install::configured_targets(
        environment.roots,
        &environment.home,
        harnesses,
        environment.claude_config.as_deref(),
        environment.codex_home.as_deref(),
    )
    .map_err(EdgeError::Install)?;
    targets
        .resolve_destinations(&provenant_os::fs::SystemPathResolver)
        .map_err(EdgeError::Install)?;
    Ok(targets)
}

#[derive(Default)]
struct PreviewReporter {
    home: Option<PathBuf>,
}
impl PreviewReporter {
    fn before_remove_with(
        &self,
        action: &Action,
        stop: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        if matches!(action.selector.as_str(), "peer-agent" | "gc-agent") {
            let home = self
                .home
                .as_deref()
                .ok_or_else(|| io::Error::other("missing agent home"))?;
            let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            let expected = if action.selector == "gc-agent" {
                super::gc_agent::plist(&home)
            } else {
                provenant_fabric_runtime::peer_agent::plist(&home)
            };
            if action.before.is_some() && action.path.to_path_buf().ok().as_ref() != Some(&expected)
            {
                return Err(io::Error::other("unexpected agent plist path"));
            }
            stop(&home)?;
        }
        Ok(())
    }
}
impl InstallReporter for PreviewReporter {
    fn before_remove(&mut self, action: &Action) -> io::Result<()> {
        self.before_remove_with(action, |home| {
            if action.selector == "gc-agent" {
                super::gc_agent::stop(home).map_err(io::Error::other)
            } else {
                provenant_fabric_runtime::peer_agent::stop(home).map_err(io::Error::other)
            }
        })
    }
    fn preview(&mut self, _: &[Action]) -> io::Result<()> {
        Ok(())
    }
    fn findings(&mut self, _: &[InstallFinding]) -> io::Result<()> {
        Ok(())
    }
    fn manual_includes(&mut self, _: &[ManualInclude]) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct InstallData {
    outcome: Option<InstallOutcome>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    completed: Vec<Action>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    remaining: Vec<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gc_agent: Option<super::gc_agent::Installed>,
}
impl InstallData {
    fn empty() -> Self {
        Self {
            outcome: None,
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
            gc_agent: None,
        }
    }
    fn success(outcome: InstallOutcome) -> Self {
        Self {
            outcome: Some(outcome),
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
            gc_agent: None,
        }
    }
    fn failure(error: &InstallError) -> Self {
        match error {
            InstallError::Partial {
                completed,
                remaining,
                ..
            } => Self {
                outcome: None,
                completed: completed.clone(),
                remaining: remaining.clone(),
                peer_agent: None,
                gc_agent: None,
            },
            _ => Self::empty(),
        }
    }
}

fn error_class(error: &InstallError) -> (Outcome, &'static str) {
    match error {
        InstallError::HooksUnavailable => (Outcome::Refused, "HOOKS_UNAVAILABLE"),
        InstallError::Conflict(_)
        | InstallError::PeerAgentConflict(_)
        | InstallError::Drift(_)
        | InstallError::Tampered(_)
        | InstallError::ReleaseConflict { .. } => (Outcome::Refused, "INSTALL_REFUSED"),
        InstallError::Partial { source, .. } => error_class(source),
        InstallError::Config(provenant_config::ConfigError::Read { .. }) => {
            (Outcome::Error, "INSTALL_ERROR")
        }
        InstallError::Config(_) => (Outcome::Invalid, "INSTALL_INVALID"),
        InstallError::Path(_)
        | InstallError::Manifest(_)
        | InstallError::Platform
        | InstallError::HarnessSelection
        | InstallError::InvalidConfig(_)
        | InstallError::ConfigFile(_)
        | InstallError::Decode(_)
        | InstallError::ReceiptDecode(_)
        | InstallError::Encode(_)
        | InstallError::PathWire(_) => (Outcome::Invalid, "INSTALL_INVALID"),
        InstallError::Fs(_)
        | InstallError::Preview(_)
        | InstallError::Smoke(_)
        | InstallError::AgentRetirement(_) => (Outcome::Error, "INSTALL_ERROR"),
    }
}
pub(super) fn install_error(command: &str, error: &InstallError, globals: &cli::Globals) -> u8 {
    let result = install_error_result(command, error);
    super::emit(&result, globals, "", false)
}
fn install_error_result(command: &str, error: &InstallError) -> CommandResult<InstallData> {
    let (status, code) = error_class(error);
    let mut result = CommandResult::new(command, status, InstallData::failure(error));
    result.error = Some(CommandError {
        code: code.into(),
        message: error.to_string(),
        cause: None,
        repair: matches!(error, InstallError::Partial { .. })
            .then(|| "Resolve the failure and re-apply the same install command".into()),
    });
    result
}
fn edge_error(command: &str, error: EdgeError, globals: &cli::Globals) -> u8 {
    match error {
        EdgeError::Install(error) => install_error(command, &error, globals),
        EdgeError::Root(error) => super::fail(
            command,
            Outcome::Invalid,
            "INSTALL_ROOT",
            &error.to_string(),
            globals,
        ),
        EdgeError::Peer(error) => super::fail(
            command,
            Outcome::Error,
            "PEER_AGENT",
            &error.to_string(),
            globals,
        ),
        EdgeError::Home => super::fail(
            command,
            Outcome::Invalid,
            "HOME",
            "HOME must be an absolute directory",
            globals,
        ),
    }
}

fn context<'a>(
    clock: &'a SystemClock,
    cancel: &'a CancelToken,
) -> Result<InstallContext<'a>, String> {
    let budget = WaitBudget::cli(clock.mono_now(), 30)
        .map_err(|_| "invalid install wait budget".to_owned())?;
    Ok(InstallContext {
        clock,
        budget,
        cancel,
    })
}
fn output_install(
    command: &str,
    summaries: &[HarnessTarget],
    result: Result<InstallOutcome, InstallError>,
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    gc_agent: Option<super::gc_agent::Installed>,
    globals: &cli::Globals,
) -> u8 {
    match result {
        Ok(outcome) => {
            let mut human = format!(
                "{command}: {} changes; {} targets; {} findings; {} manual includes\n",
                outcome.actions.len(),
                summaries.len(),
                outcome.findings.len(),
                outcome.manual_includes.len(),
            );
            if !outcome
                .findings
                .iter()
                .any(|finding| finding.rule == "state_root_moved")
                && outcome
                    .findings
                    .iter()
                    .any(|finding| finding.rule == "relocated_stale_entry")
            {
                human.push_str("  Home moved; old paths left untouched\n");
            }
            for finding in &outcome.findings {
                let path = finding
                    .path
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                if finding.rule == "state_root_moved" {
                    human.push_str(&format!(
                        "  State root moved from {path}; old paths left untouched\n"
                    ));
                } else {
                    human.push_str(&format!("  {}: {path}\n", finding.rule));
                }
            }
            for manual in &outcome.manual_includes {
                let path = manual
                    .path
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                let include = manual
                    .include
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                human.push_str(&format!(
                    "manual include required: add {include} to {path}\n"
                ));
            }
            if let Some(provenant_fabric_runtime::peer_agent::Installed::Failed { message }) =
                &peer_agent
            {
                human.push_str(&format!("peer agent: failed: {message}\n"));
            }
            if let Some(super::gc_agent::Installed::Failed { message }) = &gc_agent {
                human.push_str(&format!("scheduled gc: failed: {message}\n"));
            }
            let read_only = outcome.dry_run;
            let mut data = InstallData::success(outcome);
            data.peer_agent = peer_agent;
            data.gc_agent = gc_agent;
            super::emit(
                &CommandResult::new(command, Outcome::Ok, data),
                globals,
                &human,
                read_only,
            )
        }
        Err(error) => install_error(command, &error, globals),
    }
}

fn read_install_receipt(
    targets: &Targets,
) -> Result<Option<provenant_records::InstallReceipt>, InstallError> {
    let path = targets.roots.state.as_path().join("install/receipt.json");
    match std::fs::File::open(&path) {
        Ok(file) => provenant_records::decode_v1(file, provenant_records::Limits::CONFIG)
            .map(Some)
            .map_err(InstallError::ReceiptDecode),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::Fs(provenant_os::fs::FsError::Io {
            operation: "read install receipt",
            path,
            source,
        })),
    }
}

fn install_batches(globals: &cli::Globals, args: &cli::Install) -> Result<Vec<Targets>, EdgeError> {
    if !args.harness.is_empty() {
        return selected_targets(globals, &args.harness).map(|(_, targets)| vec![targets]);
    }
    // Resolve config and roots without validating an empty selection.
    let (environment, base) = selected_targets(globals, &[])?;
    if let Some(receipt) = read_install_receipt(&base).map_err(EdgeError::Install)? {
        let bin = receipt
            .bin_dir
            .to_path_buf()
            .map_err(InstallError::from)
            .map_err(EdgeError::Install)?;
        return receipt
            .harness_targets
            .into_iter()
            .map(|record| {
                let mut batch = base.clone();
                batch.bin_dir = bin.clone();
                let convert = || -> Result<HarnessTarget, InstallError> {
                    Ok(HarnessTarget {
                        harness: record.harness,
                        skills_dir: record.skills_dir.to_path_buf()?,
                        mcp_file: record.mcp_file.to_path_buf()?,
                        hook_file: record
                            .hook_file
                            .map(|path| path.to_path_buf())
                            .transpose()?,
                        instruction: record
                            .instruction
                            .map(|path| path.to_path_buf())
                            .transpose()?,
                    })
                };
                batch.harnesses = vec![convert().map_err(EdgeError::Install)?];
                Ok(batch)
            })
            .collect();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut batches = Vec::new();
    for (harness, executable) in [
        (Harness::Claude, "claude"),
        (Harness::Codex, "codex"),
        (Harness::Cursor, "cursor-agent"),
        (Harness::Agy, "agy"),
        (Harness::Opencode, "opencode"),
    ] {
        if std::env::split_paths(&path)
            .any(|dir| provenant_os::sys::is_executable(&dir.join(executable)))
        {
            batches.push(selected_targets(globals, &[harness])?.1);
        }
    }
    let mut directories: Vec<_> = std::fs::read_dir(&environment.home)
        .map_err(|source| EdgeError::Install(InstallError::Smoke(source)))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".claude-")
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .map(|entry| entry.path())
        .collect();
    directories.sort();
    for directory in directories {
        let mut batch = base.clone();
        let mut defaults = Targets::new(
            environment.roots.clone(),
            &environment.home,
            &[Harness::Claude],
        );
        defaults
            .apply_environment(Some(&directory), None)
            .map_err(EdgeError::Install)?;
        batch.harnesses = defaults.harnesses;
        if !batches
            .iter()
            .any(|existing| existing.harnesses[0].skills_dir == batch.harnesses[0].skills_dir)
        {
            batches.push(batch);
        }
    }
    Ok(batches)
}

pub(super) fn run_install(mut args: cli::Install, globals: &cli::Globals) -> u8 {
    match provenant_core::PathResolver::resolve_existing_ancestor(
        &provenant_os::fs::SystemPathResolver,
        &args.source,
    ) {
        Ok(source) => args.source = source,
        Err(source) => {
            return install_error(
                "install",
                &InstallError::Fs(provenant_os::fs::FsError::Io {
                    operation: "resolve install source",
                    path: args.source.clone(),
                    source,
                }),
                globals,
            );
        }
    }
    let mut batches = match install_batches(globals, &args) {
        Ok(value) => value,
        Err(error) => return edge_error("install", error, globals),
    };
    for targets in &mut batches {
        targets.bin_dir = args
            .bin_dir
            .clone()
            .unwrap_or_else(|| targets.bin_dir.clone());
        if let Err(error) = targets.resolve_destinations(&provenant_os::fs::SystemPathResolver) {
            return install_error("install", &error, globals);
        }
    }
    let summaries: Vec<_> = batches
        .iter()
        .flat_map(|batch| batch.harnesses.clone())
        .collect();
    let Some(mut targets) = batches.last().cloned() else {
        return super::fail(
            "install",
            Outcome::Invalid,
            "NO_HARNESSES",
            "No harness CLIs or Claude config directories found; select --harness",
            globals,
        );
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = match context(&clock, &cancel) {
        Ok(context) => context,
        Err(error) => {
            return super::fail("install", Outcome::Error, "WAIT_BUDGET", &error, globals);
        }
    };
    let smoke = match SystemSmoke::new(None) {
        Ok(smoke) => smoke,
        Err(error) => {
            return super::fail(
                "install",
                Outcome::Error,
                "SMOKE_EXECUTOR",
                &error.to_string(),
                globals,
            );
        }
    };
    let state = targets.roots.state.as_path().to_path_buf();
    let home = targets.home.clone();
    if let Err(error) = prepare_agents(&mut targets) {
        return edge_error("install", error, globals);
    }
    let gc_enabled = targets.gc_agent.is_some();
    let mut reporter = PreviewReporter {
        home: Some(home.clone()),
    };
    let result = (|| {
        let mut combined = InstallOutcome {
            actions: Vec::new(),
            findings: Vec::new(),
            manual_includes: Vec::new(),
            dry_run: args.dry_run,
        };
        // Refuse predictable conflicts before publishing the first target's receipt.
        for batch in &mut batches {
            prepare_agents(batch).map_err(|error| match error {
                EdgeError::Install(error) => error,
                _ => InstallError::InvalidConfig("cannot prepare install agents"),
            })?;
        }
        let supports_hooks = |batch: &Targets| {
            batch.harnesses.iter().any(|target| {
                cli::hooks::HOOK_SPECS
                    .iter()
                    .any(|spec| spec.registration.harness == Some(target.harness))
            })
        };
        if matches!(args.hooks, Some(cli::Switch::On)) && !batches.iter().any(supports_hooks) {
            return Err(InstallError::HooksUnavailable);
        }
        let batch_args = |batch: &Targets, dry_run| cli::Install {
            bin_dir: Some(batch.bin_dir.clone()),
            hooks: if matches!(args.hooks, Some(cli::Switch::On)) && !supports_hooks(batch) {
                None
            } else {
                args.hooks
            },
            dry_run,
            ..args.clone()
        };
        for batch in &batches {
            cli::install::execute_install(
                &batch_args(batch, true),
                batch.clone(),
                &context,
                &mut reporter,
                &smoke,
                env!("PROVENANT_TARGET_TRIPLE"),
            )?;
        }
        if !args.dry_run && provenant_fabric_runtime::peer_agent::eligible(&state, &home) {
            retire_disk_guard(&home)
                .map_err(|error| InstallError::AgentRetirement(error.to_string()))?;
        }
        for batch in batches {
            // Publish one receipt target at a time, using config's existing lock and ownership rules.
            let settings = batch_args(&batch, args.dry_run);
            let outcome = cli::install::execute_install(
                &settings,
                batch,
                &context,
                &mut reporter,
                &smoke,
                env!("PROVENANT_TARGET_TRIPLE"),
            )
            .map_err(|error| {
                let (mut completed, remaining, source) = match error {
                    InstallError::Partial {
                        completed,
                        remaining,
                        source,
                    } => (completed, remaining, source),
                    error => (Vec::new(), Vec::new(), Box::new(error)),
                };
                completed.splice(0..0, combined.actions.clone());
                InstallError::Partial {
                    completed,
                    remaining,
                    source,
                }
            })?;
            combined.actions.extend(outcome.actions);
            combined.findings.extend(outcome.findings);
            combined.manual_includes.extend(outcome.manual_includes);
        }
        if !args.dry_run {
            let inspection = provenant_config::inspect_install(
                &targets,
                None,
                env!("PROVENANT_TARGET_TRIPLE"),
                &smoke,
                &cancel,
            )?;
            if let Some(error) = inspection.error {
                return Err(error);
            }
            combined.findings.extend(inspection.findings);
        }
        Ok(combined)
    })();
    // The release is installed first; the agent then serves the new binary.
    let peer_agent = match &result {
        Ok(outcome) if !outcome.dry_run => Some(provenant_fabric_runtime::peer_agent::install(
            &state,
            &home,
            outcome
                .actions
                .iter()
                .any(|action| action.selector == "peer-agent"),
        )),
        _ => None,
    };
    let gc_agent = match &result {
        Ok(outcome) if !outcome.dry_run => Some(super::gc_agent::install(
            &state,
            &home,
            outcome
                .actions
                .iter()
                .any(|action| action.selector == "gc-agent"),
            gc_enabled,
        )),
        _ => None,
    };
    output_install("install", &summaries, result, peer_agent, gc_agent, globals)
}

struct UpgradeRunner {
    executor: provenant_os::exec::SystemExecutor,
    environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    cwd: PathBuf,
}
impl UpgradeRunner {
    fn execute(
        &self,
        argv: &[std::ffi::OsString],
        input: Option<&[u8]>,
        limit: usize,
        seconds: u64,
    ) -> Result<Vec<u8>, String> {
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
        let mut options =
            ExecOptions::new(argv, &self.cwd, std::time::Duration::from_secs(seconds));
        options.env = Some(&self.environment);
        options.stdin = input;
        options.stdout = StreamPolicy::Capture { limit };
        options.stderr = StreamPolicy::Capture { limit: 1_048_576 };
        options.stop_on_output_limit = true;
        let result = self
            .executor
            .execute(options)
            .map_err(|error| error.to_string())?;
        if result.termination != Termination::Exit(0)
            || result.timed_out
            || result.cancelled
            || result.drain_truncated
            || result.stdout.truncated
            || result.stderr.truncated
            || result.stdout.read_failed
            || result.stderr.read_failed
        {
            return Err(format!(
                "Command failed ({:?}): {}{}",
                result.termination,
                String::from_utf8_lossy(&result.stdout.bytes),
                String::from_utf8_lossy(&result.stderr.bytes)
            ));
        }
        Ok(result.stdout.bytes)
    }
}

#[derive(Serialize)]
struct HostUpgrade {
    host: String,
    success: bool,
    output: String,
}

pub(super) fn run_upgrade(args: cli::Upgrade, globals: &cli::Globals) -> u8 {
    let (_, targets) = match selected_targets(globals, &[]) {
        Ok(value) => value,
        Err(error) => return edge_error("upgrade", error, globals),
    };
    match read_install_receipt(&targets) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return super::fail(
                "upgrade",
                Outcome::Invalid,
                "UPGRADE_RECEIPT",
                "No install receipt; run install first",
                globals,
            );
        }
        Err(error) => return install_error("upgrade", &error, globals),
    }
    let upgraded = (|| -> Result<Vec<HostUpgrade>, String> {
        let config = provenant_config::read_with_domains(&targets.roots, &cli::domains::SECTIONS)
            .map_err(|error| error.to_string())?;
        let fabric = config.domains.fabric;
        let runner = UpgradeRunner {
            executor: provenant_os::exec::SystemExecutor::new(1)
                .map_err(|error| error.to_string())?,
            environment: std::env::vars_os().collect(),
            cwd: targets.home.clone(),
        };
        // Resolve latest once so every host downloads the same tag, even during a release.
        let mut install_args: Vec<std::ffi::OsString> = vec!["--yes".into(), "--upgrade".into()];
        let mut source_archive = None;
        let script;
        if let Some(source) = &args.from_source {
            script = std::fs::read(source.join("install.sh")).map_err(|error| error.to_string())?;
            install_args.extend(["--from-source".into(), source.as_os_str().to_owned()]);
            if !fabric.peers.is_empty() {
                let files = runner.execute(
                    &[
                        provenant_os::git::git_executable()
                            .ok_or("Git unavailable")?
                            .into_os_string(),
                        "-C".into(),
                        source.as_os_str().to_owned(),
                        "ls-files".into(),
                        "-z".into(),
                    ],
                    None,
                    4_194_304,
                    30,
                )?;
                source_archive = Some(runner.execute(
                    &[
                        "/usr/bin/tar".into(),
                        "-czf".into(),
                        "-".into(),
                        "-C".into(),
                        source.as_os_str().to_owned(),
                        "--null".into(),
                        "-T".into(),
                        "-".into(),
                    ],
                    Some(&files),
                    67_108_864,
                    120,
                )?);
            }
        } else {
            let latest = runner.execute(
                &[
                    "curl".into(),
                    "-fLsS".into(),
                    "--proto".into(),
                    "=https".into(),
                    "https://api.github.com/repos/mblauberg/provenant/releases/latest".into(),
                ],
                None,
                1_048_576,
                30,
            )?;
            let release: serde_json::Value =
                serde_json::from_slice(&latest).map_err(|error| error.to_string())?;
            let tag = release["tag_name"]
                .as_str()
                .filter(|tag| {
                    !tag.is_empty()
                        && tag
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                })
                .ok_or("Latest release has no valid tag")?;
            install_args.extend(["--tag".into(), tag.into()]);
            script = format!(
                "release_tag='{tag}'\n{}",
                include_str!("upgrade-release.sh")
            )
            .into_bytes();
        }
        let ssh = if fabric.peers.is_empty() {
            None
        } else {
            Some(
                provenant_fabric_runtime::hosts::discover_ssh_executable(
                    fabric.ssh_executable.as_deref(),
                )
                .map_err(|error| error.to_string())?,
            )
        };
        // Pass complete shell code as an argument, leaving stdin closed for children.
        let script = String::from_utf8(script).map_err(|error| error.to_string())?;
        let mut local: Vec<std::ffi::OsString> = vec![
            "/bin/sh".into(),
            "-c".into(),
            script.clone().into(),
            "install.sh".into(),
        ];
        local.extend(install_args.clone());
        local.extend([
            "--state-dir".into(),
            targets.roots.state.as_path().as_os_str().to_owned(),
        ]);
        let report = |host: String, result: Result<Vec<u8>, String>| match result {
            Ok(bytes) => HostUpgrade {
                host,
                success: true,
                output: String::from_utf8_lossy(&bytes).into_owned(),
            },
            Err(output) => HostUpgrade {
                host,
                success: false,
                output,
            },
        };
        let mut results = vec![report(
            "local".into(),
            runner.execute(&local, None, 1_048_576, 3600),
        )];
        // SSH uses Fabric's validated destination, batch mode and connection deadline.
        for (name, peer) in fabric.peers {
            let (command, input) = if let Some(archive) = &source_archive {
                ("set -eu; scratch=$(mktemp -d); trap 'rm -rf \"$scratch\"' 0; tar -xzf - -C \"$scratch\"; git -C \"$scratch\" init -q </dev/null; git -C \"$scratch\" add -f . </dev/null; sh \"$scratch/install.sh\" --yes --upgrade --from-source \"$scratch\" </dev/null".to_owned(), archive.as_slice())
            } else {
                let command = format!(
                    "sh -c '{}' install.sh {} </dev/null",
                    script.replace('\'', "'\\''"),
                    install_args
                        .iter()
                        .map(|arg| {
                            format!("'{}'", arg.to_string_lossy().replace('\'', "'\\''"))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                (command, &[][..])
            };
            let argv = vec![
                ssh.as_ref()
                    .ok_or("SSH unavailable")?
                    .as_os_str()
                    .to_owned(),
                "-T".into(),
                "-oBatchMode=yes".into(),
                format!("-oConnectTimeout={}", peer.connect_timeout_seconds).into(),
                "--".into(),
                peer.destination.into(),
                command.into(),
            ];
            results.push(report(
                name,
                runner.execute(&argv, Some(input), 1_048_576, 3600),
            ));
        }
        Ok(results)
    })();
    match upgraded {
        Err(message) => super::fail(
            "upgrade",
            Outcome::Error,
            "UPGRADE_FAILED",
            &message,
            globals,
        ),
        Ok(hosts) => {
            let success = hosts.iter().all(|host| host.success);
            let human = hosts
                .iter()
                .map(|host| {
                    format!(
                        "{}: {}\n{}\n",
                        host.host,
                        if host.success { "upgraded" } else { "failed" },
                        host.output
                    )
                })
                .collect::<String>();
            let mut result = CommandResult::new(
                "upgrade",
                if success { Outcome::Ok } else { Outcome::Error },
                hosts,
            );
            if !success {
                result.error = Some(CommandError {
                    code: "UPGRADE_FAILED".into(),
                    message: "Some hosts failed to upgrade; inspect host results and rerun upgrade"
                        .into(),
                    cause: None,
                    repair: None,
                });
            }
            super::emit(&result, globals, &human, false)
        }
    }
}

pub(super) fn run_uninstall(args: cli::Uninstall, globals: &cli::Globals) -> u8 {
    let mut targets = match receipt_targets(globals, &args.harness) {
        Ok(value) => value,
        Err(error) => return edge_error("uninstall", error, globals),
    };
    if let Err(error) = prepare_agents(&mut targets) {
        return edge_error("uninstall", error, globals);
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = match context(&clock, &cancel) {
        Ok(context) => context,
        Err(error) => {
            return super::fail("uninstall", Outcome::Error, "WAIT_BUDGET", &error, globals);
        }
    };
    let mut reporter = PreviewReporter {
        home: Some(targets.home.clone()),
    };
    if provenant_fabric_runtime::peer_agent::eligible(targets.roots.state.as_path(), &targets.home)
        && let Err(error) = retire_disk_guard(&targets.home)
    {
        return edge_error("uninstall", EdgeError::Peer(error), globals);
    }
    output_install(
        "uninstall",
        &[],
        cli::install::execute_uninstall(&args, &targets, &context, &mut reporter),
        None,
        None,
        globals,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn peer_agent_registry_name_uses_the_worktree_registry_key() {
        let path = std::path::Path::new("/worktrees/project-a");
        assert_eq!(
            super::peer_agent_registry_name(path),
            provenant_confinement::worktrees::state_dir_name(path)
        );
    }

    #[test]
    fn peer_agent_removal_accepts_canonical_home_and_refuses_other_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use provenant_config::Disposition;
        use provenant_records::AbsolutePath;
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().canonicalize()?.join("home");
        std::fs::create_dir(&home)?;
        let alias = tmp.path().join("home-alias");
        std::os::unix::fs::symlink(&home, &alias)?;
        let reporter = PreviewReporter { home: Some(alias) };
        let mut action = Action {
            path: AbsolutePath::new(&provenant_fabric_runtime::peer_agent::plist(&home))?,
            disposition: Disposition::Remove,
            selector: "peer-agent".into(),
            before: None,
            after: None,
            identity: None,
            edits: Vec::new(),
        };
        let mut stops = 0;
        reporter.before_remove_with(&action, |cwd| {
            assert_eq!(cwd, home);
            stops += 1;
            Ok(())
        })?;
        assert_eq!(stops, 1);
        action.path = AbsolutePath::new(&home.join("foreign.plist"))?;
        reporter.before_remove_with(&action, |cwd| {
            assert_eq!(cwd, home);
            stops += 1;
            Ok(())
        })?;
        assert_eq!(stops, 2);
        action.before = Some(provenant_records::OwnedValue::File {
            digest: provenant_records::CanonicalV1::file_digest(b"rendered plist\n"),
            mode: 0o600,
        });
        assert!(
            reporter
                .before_remove_with(&action, |_| {
                    stops += 1;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(stops, 2);
        Ok(())
    }
}

fn retire_disk_guard(home: &Path) -> Result<(), AgentError> {
    retire_disk_guard_with(home, launchctl)
}

fn retire_disk_guard_with(
    home: &Path,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    let plist = home.join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
    match std::fs::symlink_metadata(&plist) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    // Keep the old checker's recovery custody until it has resumed its builds.
    // Holding the same lock also prevents a final pause racing with bootout.
    let _recovery = legacy_guard_ready(home)?;
    let service = format!(
        "gui/{}/dev.provenant.disk-guard",
        provenant_os::fs::effective_uid()
    );
    if run(&["print".as_ref(), service.as_ref()], home)?.0 {
        let (ok, error) = run(&["bootout".as_ref(), service.as_ref()], home)?;
        if !ok {
            return Err(AgentError::Launchctl("bootout", error));
        }
    }
    std::fs::remove_file(plist)?;
    Ok(())
}

fn legacy_guard_ready(home: &Path) -> io::Result<provenant_os::fs::LockGuard> {
    use provenant_os::fs::LockMode;
    let path = home.join(".local/share/provenant/v1/disk-guard");
    let guard = provenant_config::private_directory(&path, true).map_err(io::Error::other)?;
    let repair = "run `provenant gc --apply`, let the existing disk guard resume its saved builds, then retry";
    let lock = guard
        .try_lock("guard.lock".as_ref(), LockMode::Exclusive)
        .map_err(|error| {
            io::Error::other(format!(
                "disk guard still owns recovery custody: {error}; {repair}"
            ))
        })?;
    for name in ["paused.json", "state.json"] {
        let bytes = match guard.read_stable(name.as_ref(), 1024 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => continue,
            Err(error) => {
                return Err(io::Error::other(format!(
                    "cannot inspect disk guard recovery: {error}; {repair}"
                )));
            }
        };
        let state: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if !state["paused"].as_array().is_some_and(Vec::is_empty) {
            return Err(io::Error::other(format!(
                "disk guard has saved paused builds in {}; {repair}",
                path.join(name).display()
            )));
        }
        // paused.json superseded the earlier state.json in the old checker.
        break;
    }
    Ok(lock)
}

#[cfg(test)]
mod retired_disk_guard_tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::*;

    #[test]
    fn retirement_stops_and_deletes_the_old_agent_once() {
        let home = tempfile::tempdir().unwrap();
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"old plist").unwrap();
        let mut calls = Vec::new();
        for _ in 0..2 {
            retire_disk_guard_with(home.path(), |args, _| {
                calls.push(args[0].to_owned());
                let path = home.path().join(".local/share/provenant/v1/disk-guard");
                assert!(
                    path.is_dir(),
                    "retirement must establish recovery custody before bootout"
                );
                let directory = provenant_os::fs::Directory::open_anchor(&path).unwrap();
                assert!(
                    directory
                        .try_lock("guard.lock".as_ref(), provenant_os::fs::LockMode::Exclusive)
                        .is_err()
                );
                Ok((true, String::new()))
            })
            .unwrap();
        }
        assert_eq!(calls, [OsStr::new("print"), OsStr::new("bootout")]);
        assert!(!plist.exists());
    }

    #[test]
    fn failed_bootout_preserves_the_plist_for_retry() {
        let home = tempfile::tempdir().unwrap();
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"old plist").unwrap();
        let result = retire_disk_guard_with(home.path(), |args, _| {
            Ok((args[0] == "print", "busy".into()))
        });
        assert!(matches!(result, Err(AgentError::Launchctl("bootout", _))));
        assert!(plist.exists());
    }
}

#[cfg(test)]
mod legacy_pause_retirement_tests {
    #[allow(unused_imports)]
    use super::*;
    #[test]
    fn retirement_keeps_a_guard_with_saved_paused_builds_and_names_recovery()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir()?;
        let plist = home
            .path()
            .join("Library/LaunchAgents/dev.provenant.disk-guard.plist");
        std::fs::create_dir_all(plist.parent().ok_or("plist parent")?)?;
        std::fs::write(&plist, "old plist")?;
        let guard = home.path().join(".local/share/provenant/v1/disk-guard");
        std::fs::create_dir_all(&guard)?;
        std::fs::set_permissions(&guard, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(guard.join("paused.json"), br#"{"paused":[{"pid":42}]}"#)?;
        let mut calls = 0;
        let result = retire_disk_guard_with(home.path(), |_, _| {
            calls += 1;
            Ok((true, String::new()))
        });
        assert!(matches!(result, Err(AgentError::Io(_))));
        let error = result.err().ok_or("retired paused guard")?;
        let result =
            install_error_result("install", &InstallError::AgentRetirement(error.to_string()));
        let mut rendered = Vec::new();
        provenant_cli::render(
            &result,
            provenant_cli::RenderMode::Human {
                text: "",
                read_only: false,
            },
            &mut rendered,
        )?;
        let rendered = String::from_utf8(rendered)?;
        assert!(rendered.contains("saved paused builds"), "{rendered}");
        assert!(rendered.contains("provenant gc --apply"), "{rendered}");
        assert_eq!(calls, 0, "the existing checker must keep recovery custody");
        assert!(plist.exists());
        Ok(())
    }
}
