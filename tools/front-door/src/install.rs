//! Process-edge wiring for installation. State operations stay in config;
//! every child is supervised with a bounded wait and isolated environment.
use crate::release_smoke::SystemSmoke;
use provenant_cli as cli;
use provenant_config::{
    Action, InstallContext, InstallError, InstallFinding, InstallOutcome, InstallReporter,
    ManualInclude, Targets,
};
use provenant_core::{CancelToken, Clock, Roots, WaitBudget};
use provenant_records::{CommandError, CommandResult, Harness, Outcome};
use provenant_supervisor::{
    clock::SystemClock,
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{LocalFs, SafeFs},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::Duration,
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
    Root(super::roots::RootError),
    Install(InstallError),
    Git,
    Home,
    Peer(super::peer_agent::AgentError),
}

fn prepare_peer_agent(targets: &mut Targets) -> Result<(), EdgeError> {
    let state = targets.roots.state.as_path();
    if super::peer_agent::eligible(state, &targets.home) {
        let program = targets.bin_dir.join("provenant");
        let previous = provenant_config::installed_binary(targets, &LocalFs)
            .map_err(EdgeError::Install)?
            .unwrap_or_else(|| program.clone());
        targets.peer_agent =
            Some(super::peer_agent::install_plist(&program, state).map_err(EdgeError::Peer)?);
        targets.peer_agent_previous =
            super::peer_agent::previous_plist(&previous, state, &targets.home)
                .map_err(EdgeError::Peer)?;
    }
    Ok(())
}

struct Environment {
    home: PathBuf,
    roots: Roots,
    assets: PathBuf,
    claude: Option<PathBuf>,
    codex: Option<PathBuf>,
    git_hooks_dir: Option<PathBuf>,
}

fn environment(
    globals: &cli::Globals,
    legacy_checkout: Option<&Path>,
) -> Result<Environment, EdgeError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(EdgeError::Home)?;
    let assets = super::roots::product_root().map_err(EdgeError::Root)?;
    let project = super::roots::selected_project(globals.project.as_deref(), false)
        .map_err(EdgeError::Root)?;
    let roots = super::roots::roots(globals, &assets, &project).map_err(EdgeError::Root)?;
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    let codex = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let git_hooks_dir = legacy_checkout.map(git_hooks_dir).transpose()?;
    Ok(Environment {
        home,
        roots,
        assets,
        claude,
        codex,
        git_hooks_dir,
    })
}

fn selected_targets(
    globals: &cli::Globals,
    harnesses: &[Harness],
    legacy_checkout: Option<&Path>,
) -> Result<(Environment, Targets), EdgeError> {
    let environment = environment(globals, legacy_checkout)?;
    let fs = LocalFs;
    let mut targets = cli::install::configured_targets(
        environment.roots.clone(),
        &environment.home,
        harnesses,
        &fs,
        environment.claude.as_deref(),
        environment.codex.as_deref(),
        environment.git_hooks_dir.clone(),
    )
    .map_err(EdgeError::Install)?;
    if !environment
        .assets
        .starts_with(targets.roots.state.as_path().join("releases"))
    {
        targets.protected_roots.push(environment.assets.clone());
    }
    if let Some(legacy) = legacy_checkout {
        let canonical = provenant_core::PathResolver::resolve_existing_ancestor(&LocalFs, legacy)
            .map_err(|source| {
            EdgeError::Install(InstallError::Fs(provenant_supervisor::fs::FsError::Io {
                operation: "resolve legacy checkout",
                path: legacy.into(),
                source,
            }))
        })?;
        targets.protected_roots.push(canonical);
    }
    Ok((environment, targets))
}

/// Resolves targets as install does, so `CLAUDE_CONFIG_DIR`/`CODEX_HOME`
/// select the matching recorded target. Without them, uninstall acts on every
/// recorded target of each harness, at its recorded paths.
fn receipt_targets(globals: &cli::Globals, harnesses: &[Harness]) -> Result<Targets, EdgeError> {
    let environment = environment(globals, None)?;
    let mut targets = cli::install::configured_targets(
        environment.roots,
        &environment.home,
        harnesses,
        &LocalFs,
        environment.claude.as_deref(),
        environment.codex.as_deref(),
        None,
    )
    .map_err(EdgeError::Install)?;
    // Re-derive the legacy hooks directory rather than trust the receipt's
    // copy. A checkout that is gone yields none, so uninstall reports its
    // launcher instead of removing it.
    targets.git_hooks_dir = match provenant_config::recorded_hook_checkout(&targets, &LocalFs)
        .map_err(EdgeError::Install)?
    {
        Some(checkout) => match std::fs::metadata(&checkout) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            _ => Some(git_hooks_dir(&checkout)?),
        },
        None => None,
    };
    targets
        .resolve_destinations(&LocalFs)
        .map_err(EdgeError::Install)?;
    Ok(targets)
}

fn git_hooks_dir(checkout: &Path) -> Result<PathBuf, EdgeError> {
    let git = super::roots::git_binary().ok_or(EdgeError::Git)?;
    let argv: Vec<OsString> = [
        git.as_os_str(),
        OsStr::new("--no-pager"),
        OsStr::new("--no-optional-locks"),
        OsStr::new("-c"),
        OsStr::new("core.fsmonitor=false"),
        OsStr::new("rev-parse"),
        OsStr::new("--path-format=absolute"),
        OsStr::new("--git-path"),
        OsStr::new("hooks"),
    ]
    .into_iter()
    .map(OsStr::to_owned)
    .collect();
    let env: Vec<(OsString, OsString)> = [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .map(|(key, value)| (key.into(), value.into()))
    .into();
    let mut options = ExecOptions::new(&argv, checkout, Duration::from_secs(2));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let executor = SystemExecutor::new(1).map_err(|_| EdgeError::Git)?;
    let result = executor.execute(options).map_err(|_| EdgeError::Git)?;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.drain_truncated
        || result.stdout.truncated
        || result.stdout.read_failed
    {
        return Err(EdgeError::Git);
    }
    let bytes = result
        .stdout
        .bytes
        .strip_suffix(b"\n")
        .ok_or(EdgeError::Git)?;
    if bytes.is_empty() || bytes.contains(&0) || bytes.contains(&b'\n') {
        return Err(EdgeError::Git);
    }
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    if !path.is_absolute() {
        return Err(EdgeError::Git);
    }
    // core.hooksPath is repository-controlled evidence, not authority to
    // write another directory. Bind aliases before enforcing the selected root.
    let checkout = std::fs::canonicalize(checkout).map_err(|_| EdgeError::Git)?;
    let path = provenant_core::PathResolver::resolve_existing_ancestor(&LocalFs, &path)
        .map_err(|_| EdgeError::Git)?;
    if !path.starts_with(&checkout) {
        return Err(EdgeError::Git);
    }
    Ok(path)
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
        if action.selector == "peer-agent" {
            let home = self
                .home
                .as_deref()
                .ok_or_else(|| io::Error::other("missing agent home"))?;
            let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            if action.before.is_some()
                && action.path.to_path_buf().ok().as_ref() != Some(&super::peer_agent::plist(&home))
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
            super::peer_agent::stop(home).map_err(io::Error::other)
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
    peer_agent: Option<super::peer_agent::Installed>,
}
impl InstallData {
    fn empty() -> Self {
        Self {
            outcome: None,
            completed: Vec::new(),
            remaining: Vec::new(),
            peer_agent: None,
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
            },
            _ => Self::empty(),
        }
    }
}

fn error_class(error: &InstallError) -> (Outcome, &'static str) {
    match error {
        InstallError::HooksUnavailable | InstallError::LegacyUnresolved(_) => {
            (Outcome::Refused, "HOOKS_UNAVAILABLE")
        }
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
        EdgeError::Git => super::fail(
            command,
            Outcome::Invalid,
            "LEGACY_CHECKOUT",
            "cannot resolve legacy checkout Git hooks directory",
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
    fs: &'a dyn SafeFs,
    clock: &'a SystemClock,
    cancel: &'a CancelToken,
) -> Result<InstallContext<'a>, String> {
    let budget = WaitBudget::cli(clock.mono_now(), 30)
        .map_err(|_| "invalid install wait budget".to_owned())?;
    Ok(InstallContext {
        fs,
        clock,
        budget,
        cancel,
    })
}
fn output_install(
    command: &str,
    result: Result<InstallOutcome, InstallError>,
    peer_agent: Option<super::peer_agent::Installed>,
    globals: &cli::Globals,
) -> u8 {
    match result {
        Ok(outcome) => {
            let mut counts = BTreeMap::<&str, InstallCounts>::new();
            for action in &outcome.actions {
                let target = action
                    .path
                    .to_path_buf()
                    .map(|path| install_target(&path))
                    .unwrap_or("target");
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
                    .map(|path| install_target(&path))
                    .unwrap_or("target");
                counts.entry(target).or_default().notes += 1;
            }
            for finding in &outcome.findings {
                let target = finding
                    .path
                    .to_path_buf()
                    .map(|path| install_target(&path))
                    .unwrap_or("target");
                counts.entry(target).or_default().findings += 1;
            }
            if counts.is_empty() {
                counts.entry("target").or_default();
            }
            let mut human = format!("{command}:\n");
            for finding in &outcome.findings {
                if let Ok(path) = finding.path.to_path_buf()
                    && path.file_name() == Some(OsStr::new("dev.provenant.peer.plist"))
                {
                    human.push_str(&format!(
                        "  LaunchAgent plist: preserved {} ({})\n",
                        path.display(),
                        finding.rule
                    ));
                }
            }
            for action in &outcome.actions {
                if action.selector == "peer-agent" {
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
            for (target, count) in counts {
                human.push_str(&format!(
                    "  {target}: {} changed links, {} changed hooks, {} changed MCP, {} notes; {} findings; {} other\n",
                    count.links, count.hooks, count.mcp, count.notes, count.findings, count.other
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
            let read_only = outcome.dry_run;
            let mut data = InstallData::success(outcome);
            data.peer_agent = peer_agent;
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

pub(super) fn run_install(mut args: cli::Install, globals: &cli::Globals) -> u8 {
    match provenant_core::PathResolver::resolve_existing_ancestor(&LocalFs, &args.source) {
        Ok(source) => args.source = source,
        Err(source) => {
            return install_error(
                "install",
                &InstallError::Fs(provenant_supervisor::fs::FsError::Io {
                    operation: "resolve install source",
                    path: args.source.clone(),
                    source,
                }),
                globals,
            );
        }
    }
    if let Some(checkout) = &args.legacy_checkout {
        match provenant_core::PathResolver::resolve_existing_ancestor(&LocalFs, checkout) {
            Ok(canonical) => args.legacy_checkout = Some(canonical),
            Err(source) => {
                return install_error(
                    "install",
                    &InstallError::Fs(provenant_supervisor::fs::FsError::Io {
                        operation: "resolve legacy checkout",
                        path: checkout.clone(),
                        source,
                    }),
                    globals,
                );
            }
        }
    }
    let (_environment, mut targets) =
        match selected_targets(globals, &args.harness, args.legacy_checkout.as_deref()) {
            Ok(value) => value,
            Err(error) => return edge_error("install", error, globals),
        };
    targets.bin_dir = args.bin_dir.clone().unwrap_or(targets.bin_dir);
    if let Err(error) = targets.resolve_destinations(&LocalFs) {
        return install_error("install", &error, globals);
    }
    args.bin_dir = Some(targets.bin_dir.clone());
    if let Some(checkout) = &args.legacy_checkout {
        let resolve = |path: &Path| {
            provenant_core::PathResolver::resolve_existing_ancestor(&LocalFs, path).map_err(
                |source| {
                    EdgeError::Install(InstallError::Fs(provenant_supervisor::fs::FsError::Io {
                        operation: "resolve install source boundary",
                        path: path.into(),
                        source,
                    }))
                },
            )
        };
        match (resolve(checkout), resolve(&args.source)) {
            (Ok(checkout), Ok(source)) if source.starts_with(&checkout) => {
                return install_error(
                    "install",
                    &InstallError::Conflict(args.source.clone()),
                    globals,
                );
            }
            (Err(error), _) | (_, Err(error)) => return edge_error("install", error, globals),
            _ => {}
        }
    }
    let fs = LocalFs;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = match context(&fs, &clock, &cancel) {
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
    if let Err(error) = prepare_peer_agent(&mut targets) {
        return edge_error("install", error, globals);
    }
    let mut reporter = PreviewReporter::default();
    let result = cli::install::execute_install(
        &args,
        targets,
        &context,
        &mut reporter,
        &smoke,
        env!("PROVENANT_TARGET_TRIPLE"),
    );
    // The release is installed first; the agent then serves the new binary.
    let peer_agent = match &result {
        Ok(outcome) if !outcome.dry_run => Some(super::peer_agent::install(
            &state,
            &home,
            outcome
                .actions
                .iter()
                .any(|action| action.selector == "peer-agent"),
        )),
        _ => None,
    };
    output_install("install", result, peer_agent, globals)
}

pub(super) fn run_uninstall(args: cli::Uninstall, globals: &cli::Globals) -> u8 {
    let mut targets = match receipt_targets(globals, &args.harness) {
        Ok(value) => value,
        Err(error) => return edge_error("uninstall", error, globals),
    };
    if let Err(error) = prepare_peer_agent(&mut targets) {
        return edge_error("uninstall", error, globals);
    }
    let fs = LocalFs;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = match context(&fs, &clock, &cancel) {
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
        cli::install::execute_uninstall(&args, &targets, &context, &mut reporter),
        None,
        globals,
    )
}

#[cfg(test)]
mod tests {
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
            path: AbsolutePath::new(&crate::peer_agent::plist(&home))?,
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
