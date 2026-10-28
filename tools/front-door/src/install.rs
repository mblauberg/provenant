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
use provenant_os::clock::SystemClock;
use provenant_records::{CommandError, CommandResult, Harness, Outcome};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default)]
struct InstallCounts {
    links: usize,
    hooks: usize,
    mcp: usize,
    notes: usize,
    findings: usize,
    other: usize,
}

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
        if config.disk_guard.enabled {
            targets.disk_guard_agent = Some(
                super::disk_guard_agent::render(&program, state, &config.disk_guard)
                    .map_err(EdgeError::Peer)?,
            );
        }
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
        if matches!(
            action.selector.as_str(),
            "peer-agent" | "gc-agent" | "disk-guard-agent"
        ) {
            let home = self
                .home
                .as_deref()
                .ok_or_else(|| io::Error::other("missing agent home"))?;
            let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            let expected = if action.selector == "disk-guard-agent" {
                super::disk_guard_agent::plist(&home)
            } else if action.selector == "gc-agent" {
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
            if action.selector == "disk-guard-agent" {
                super::disk_guard_agent::stop(home).map_err(io::Error::other)
            } else if action.selector == "gc-agent" {
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
    completed: Vec<Action>,
    remaining: Vec<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_agent: Option<provenant_fabric_runtime::peer_agent::Installed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gc_agent: Option<super::gc_agent::Installed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disk_guard_agent: Option<super::disk_guard_agent::Installed>,
}
impl InstallData {
    fn empty() -> Self {
        Self {
            outcome: None,
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
            gc_agent: None,
            disk_guard_agent: None,
        }
    }
    fn success(outcome: InstallOutcome) -> Self {
        let completed = if outcome.dry_run {
            Vec::new()
        } else {
            outcome.actions.clone()
        };
        Self {
            outcome: Some(outcome),
            completed,
            remaining: Vec::new(),
            peer_agent: None,
            gc_agent: None,
            disk_guard_agent: None,
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
                disk_guard_agent: None,
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
        InstallError::Fs(_) | InstallError::Preview(_) | InstallError::Smoke(_) => {
            (Outcome::Error, "INSTALL_ERROR")
        }
    }
}
pub(super) fn install_error(command: &str, error: &InstallError, globals: &cli::Globals) -> u8 {
    let (status, code) = error_class(error);
    let mut result = CommandResult::new(command, status, InstallData::failure(error));
    result.error = Some(CommandError {
        code: code.into(),
        message: error.to_string(),
        cause: None,
        repair: matches!(error, InstallError::Partial { .. })
            .then(|| "Resolve the failure and re-apply the same install command".into()),
    });
    super::emit(&result, globals, "", false)
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
    disk_guard_agent: Option<super::disk_guard_agent::Installed>,
    globals: &cli::Globals,
) -> u8 {
    match result {
        Ok(outcome) => {
            let label = |target: &HarnessTarget| {
                format!(
                    "{:?} ({})",
                    target.harness,
                    target
                        .skills_dir
                        .parent()
                        .unwrap_or(&target.skills_dir)
                        .display()
                )
            };
            let target_name = |path: &Path| {
                summaries
                    .iter()
                    .find(|target| {
                        path.starts_with(&target.skills_dir)
                            || path == target.mcp_file
                            || target.hook_file.as_deref() == Some(path)
                            || target.instruction.as_deref().is_some_and(|p| {
                                p == path || p.with_file_name("HARNESS.md") == path
                            })
                    })
                    .map(label)
                    .unwrap_or_else(|| install_target(path).to_owned())
            };
            let mut counts = BTreeMap::<String, InstallCounts>::new();
            for target in summaries {
                counts.entry(label(target)).or_default();
            }
            for action in &outcome.actions {
                let target = action
                    .path
                    .to_path_buf()
                    .map(|path| target_name(&path))
                    .unwrap_or_else(|_| "target".into());
                let selector = action.selector.to_ascii_lowercase();
                let target_counts = counts.entry(target).or_default();
                if selector.contains("hook") {
                    target_counts.hooks += 1;
                } else if selector.contains("mcp") {
                    target_counts.mcp += 1;
                } else if matches!(
                    action.before.as_ref().or(action.after.as_ref()),
                    Some(provenant_records::OwnedValue::Link { .. })
                ) {
                    target_counts.links += 1;
                } else {
                    target_counts.other += 1;
                }
            }
            for manual in &outcome.manual_includes {
                let target = manual
                    .path
                    .to_path_buf()
                    .map(|path| target_name(&path))
                    .unwrap_or_else(|_| "target".into());
                counts.entry(target).or_default().notes += 1;
            }
            for finding in &outcome.findings {
                let target = finding
                    .path
                    .to_path_buf()
                    .map(|path| target_name(&path))
                    .unwrap_or_else(|_| "target".into());
                counts.entry(target).or_default().findings += 1;
            }
            if counts.is_empty() {
                counts.entry("target".into()).or_default();
            }
            let mut human = format!("{command}:\n");
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
                if finding.rule == "state_root_moved"
                    && let Ok(path) = finding.path.to_path_buf()
                {
                    human.push_str(&format!(
                        "  State root moved from {}; old paths left untouched\n",
                        path.display()
                    ));
                }
                if let Ok(path) = finding.path.to_path_buf()
                    && matches!(
                        path.file_name().and_then(OsStr::to_str),
                        Some("dev.provenant.peer.plist" | "dev.provenant.gc.plist")
                    )
                {
                    human.push_str(&format!(
                        "  LaunchAgent plist: preserved {} ({})\n",
                        path.display(),
                        finding.rule
                    ));
                }
            }
            for action in &outcome.actions {
                if matches!(action.selector.as_str(), "peer-agent" | "gc-agent") {
                    let path = action
                        .path
                        .to_path_buf()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    human.push_str(&format!(
                        "  LaunchAgent plist: {:?} {path}\n",
                        action.disposition
                    ));
                }
            }
            for finding in &outcome.findings {
                human.push_str(&format!(
                    "  finding: {} ({:?})\n",
                    finding.rule,
                    finding.path.to_path_buf().ok()
                ));
            }
            for (target, count) in counts {
                if !summaries.is_empty() && !summaries.iter().any(|item| label(item) == target) {
                    continue;
                }
                human.push_str(&format!(
                    "  {target}: {} changed links, {} changed hooks, {} changed MCP, {} notes; {} other\n",
                    count.links, count.hooks, count.mcp, count.notes, count.other
                ));
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
            if let Some(agent) = &peer_agent {
                human.push_str(&format!("peer agent: {}\n", agent.describe()));
            }
            if let Some(agent) = &gc_agent {
                human.push_str(&format!("scheduled gc: {}\n", agent.describe()));
            }
            let read_only = outcome.dry_run;
            if let Some(agent) = &disk_guard_agent {
                human.push_str(&format!("disk guard: {}\n", agent.describe()));
            }
            let mut data = InstallData::success(outcome);
            data.peer_agent = peer_agent;
            data.gc_agent = gc_agent;
            data.disk_guard_agent = disk_guard_agent;
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

fn install_target(path: &Path) -> &'static str {
    let path = path.to_string_lossy();
    if path.contains("/.claude-") {
        "Claude account"
    } else if path.contains("/.claude/")
        || path.ends_with("/.claude")
        || path.ends_with("/.claude.json")
    {
        provenant_providers::claude::REGISTRATION.display_name
    } else if path.contains("/.codex/") || path.ends_with("/.codex") {
        provenant_providers::codex::REGISTRATION.display_name
    } else if path.contains("/.cursor/") || path.ends_with("/.cursor") {
        provenant_providers::cursor::REGISTRATION.display_name
    } else if path.contains("/.config/opencode/") {
        provenant_providers::opencode::REGISTRATION.display_name
    } else if path.contains("/.config/agents/") {
        "Agents"
    } else {
        "Other"
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
    let disk_guard_enabled = targets.disk_guard_agent.is_some();
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
    let disk_guard_agent = match &result {
        Ok(outcome) if !outcome.dry_run => Some(super::disk_guard_agent::install(
            &state,
            &home,
            outcome
                .actions
                .iter()
                .any(|action| action.selector == "disk-guard-agent"),
            disk_guard_enabled,
        )),
        _ => None,
    };
    output_install(
        "install",
        &summaries,
        result,
        peer_agent,
        gc_agent,
        disk_guard_agent,
        globals,
    )
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
            script = runner.execute(
                &[
                    "curl".into(),
                    "-fLsS".into(),
                    "--proto".into(),
                    "=https".into(),
                    format!(
                        "https://raw.githubusercontent.com/mblauberg/provenant/{tag}/install.sh"
                    )
                    .into(),
                ],
                None,
                1_048_576,
                30,
            )?;
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
        let mut local: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-s".into(), "--".into()];
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
            runner.execute(&local, Some(&script), 1_048_576, 3600),
        )];
        // SSH uses Fabric's validated destination, batch mode and connection deadline.
        for (name, peer) in fabric.peers {
            let (command, input) = if let Some(archive) = &source_archive {
                ("set -eu; scratch=$(mktemp -d); trap 'rm -rf \"$scratch\"' 0; tar -xzf - -C \"$scratch\"; git -C \"$scratch\" init -q; git -C \"$scratch\" add -f .; sh \"$scratch/install.sh\" --yes --upgrade --from-source \"$scratch\"".to_owned(), archive.as_slice())
            } else {
                let command = format!(
                    "sh -s -- {}",
                    install_args
                        .iter()
                        .map(|arg| {
                            format!("'{}'", arg.to_string_lossy().replace('\'', "'\\''"))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                (command, script.as_slice())
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
    output_install(
        "uninstall",
        &[],
        cli::install::execute_uninstall(&args, &targets, &context, &mut reporter),
        None,
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
