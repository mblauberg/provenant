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
    ffi::{OsStr, OsString},
    io,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug)]
enum EdgeError {
    Root(super::roots::RootError),
    Install(InstallError),
    Git,
    Home,
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
struct PreviewReporter;
impl InstallReporter for PreviewReporter {
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
}
impl InstallData {
    fn empty() -> Self {
        Self {
            outcome: None,
            completed: Vec::new(),
            remaining: Vec::new(),
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
    globals: &cli::Globals,
) -> u8 {
    match result {
        Ok(outcome) => {
            let mut counts = [0usize; 3];
            for action in &outcome.actions {
                counts[match action.disposition {
                    provenant_config::Disposition::Create => 0,
                    provenant_config::Disposition::Replace => 1,
                    provenant_config::Disposition::Remove => 2,
                }] += 1;
            }
            let mut human = format!(
                "{command}: {} action(s) ({} create, {} replace, {} remove)\n",
                outcome.actions.len(),
                counts[0],
                counts[1],
                counts[2]
            );
            const LISTED: usize = 20;
            for action in outcome.actions.iter().take(LISTED) {
                let path = action
                    .path
                    .to_path_buf()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                human.push_str(&format!("{:?}: {path}\n", action.disposition));
            }
            if outcome.actions.len() > LISTED {
                human.push_str(&format!(
                    "... and {} more (use --json for the full list)\n",
                    outcome.actions.len() - LISTED
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
            for finding in &outcome.findings {
                human.push_str(&format!(
                    "{}: {}\n",
                    finding.rule,
                    finding
                        .path
                        .to_path_buf()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ));
            }
            let read_only = outcome.dry_run;
            super::emit(
                &CommandResult::new(command, Outcome::Ok, InstallData::success(outcome)),
                globals,
                &human,
                read_only,
            )
        }
        Err(error) => install_error(command, &error, globals),
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
    let mut reporter = PreviewReporter;
    let result = cli::install::execute_install(
        &args,
        targets,
        &context,
        &mut reporter,
        &smoke,
        env!("PROVENANT_TARGET_TRIPLE"),
    );
    output_install("install", result, globals)
}

pub(super) fn run_uninstall(args: cli::Uninstall, globals: &cli::Globals) -> u8 {
    let targets = match receipt_targets(globals, &args.harness) {
        Ok(value) => value,
        Err(error) => return edge_error("uninstall", error, globals),
    };
    let fs = LocalFs;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let context = match context(&fs, &clock, &cancel) {
        Ok(context) => context,
        Err(error) => {
            return super::fail("uninstall", Outcome::Error, "WAIT_BUDGET", &error, globals);
        }
    };
    let mut reporter = PreviewReporter;
    output_install(
        "uninstall",
        cli::install::execute_uninstall(&args, &targets, &context, &mut reporter),
        globals,
    )
}
