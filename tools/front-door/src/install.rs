//! Process-edge wiring for installation. State operations stay in config;
//! every child is supervised with a bounded wait and isolated environment.
use provenant_cli::{self as cli, install::InstallCheckError};
use provenant_config::{
    self as config, Action, InstallContext, InstallError, InstallFinding, InstallOutcome,
    InstallReporter, ManualInclude, Release, ReleaseSmoke, Targets,
};
use provenant_core::{CancelToken, Clock, Roots, WaitBudget};
use provenant_records::{CommandError, CommandResult, Harness, Outcome, VersionInfo};
use provenant_supervisor::{
    clock::SystemClock,
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{LocalFs, SafeFs},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
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

fn receipt_targets(globals: &cli::Globals, harnesses: &[Harness]) -> Result<Targets, EdgeError> {
    let environment = environment(globals, None)?;
    Ok(Targets::new(
        environment.roots,
        &environment.home,
        harnesses,
    ))
}

fn git_hooks_dir(checkout: &Path) -> Result<PathBuf, EdgeError> {
    let git = [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/opt/local/bin/git",
        "/usr/bin/git",
        "/bin/git",
    ]
    .into_iter()
    .map(Path::new)
    .find(|path| provenant_supervisor::sys::is_executable(path))
    .ok_or(EdgeError::Git)?;
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
    Ok(path)
}

#[derive(Deserialize)]
struct SmokeVersion {
    package_version: String,
    source_digest: String,
    asset_digest: String,
    target: String,
    toolchain: String,
    record_schema_version: u64,
    config_schema_version: u64,
}
#[derive(Deserialize)]
struct SmokeEnvelope<T> {
    schema_version: u64,
    command: String,
    status: String,
    data: T,
    error: Option<serde_json::Value>,
}

struct SystemSmoke {
    executor: SystemExecutor,
}
impl SystemSmoke {
    fn new() -> io::Result<Self> {
        let executor = SystemExecutor::new(1)
            .map_err(|_| io::Error::other("cannot start bounded release smoke executor"))?;
        Ok(Self { executor })
    }
    fn invoke<T: DeserializeOwned>(
        &self,
        release: &Release,
        args: &[&str],
        cancel: &CancelToken,
    ) -> io::Result<SmokeEnvelope<T>> {
        let executable = release.source().join("bin/provenant");
        let mut argv = vec![executable.into_os_string()];
        argv.extend(args.iter().map(OsString::from));
        let env: [(OsString, OsString); 0] = [];
        let mut options = ExecOptions::new(&argv, release.source(), Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 1_048_576 };
        options.stderr = StreamPolicy::Discard;
        options.cancel = Some(cancel);
        options.stop_on_output_limit = true;
        let result = self
            .executor
            .execute(options)
            .map_err(|_| io::Error::other("bounded release smoke command failed"))?;
        if result.termination != Termination::Exit(0)
            || result.timed_out
            || result.drain_truncated
            || result.stdout.truncated
            || result.stdout.read_failed
            || result.cancelled
        {
            return Err(io::Error::other(
                "release smoke command returned an invalid result",
            ));
        }
        provenant_records::decode(
            result.stdout.bytes.as_slice(),
            provenant_records::Limits::CONFIG,
        )
        .map_err(|_| io::Error::other("release smoke output is not a valid result envelope"))
    }
}
impl ReleaseSmoke for SystemSmoke {
    fn check(&self, release: &Release, cancel: &CancelToken) -> io::Result<VersionInfo> {
        let version = self.invoke::<SmokeVersion>(release, &["--version", "--json"], cancel)?;
        if version.schema_version != 1
            || version.command != "version"
            || version.status != "ok"
            || version.error.is_some()
            || version.data.package_version != env!("CARGO_PKG_VERSION")
        {
            return Err(io::Error::other(
                "release version identity envelope differs",
            ));
        }
        let manifest = release.manifest();
        let data = version.data;
        if data.source_digest != manifest.source_digest.to_string()
            || data.asset_digest != manifest.asset_digest.to_string()
            || data.target != manifest.target
            || data.toolchain != manifest.toolchain
            || data.record_schema_version != 1
            || data.config_schema_version != config::CONFIG_SCHEMA_VERSION
        {
            return Err(io::Error::other(
                "release version identity differs from manifest",
            ));
        }
        let help = self.invoke::<serde_json::Value>(release, &["--help", "--json"], cancel)?;
        if help.schema_version != 1
            || help.command != "help"
            || help.status != "ok"
            || help.error.is_some()
            || !help.data.is_object()
        {
            return Err(io::Error::other("release help identity envelope differs"));
        }
        Ok(VersionInfo {
            source_digest: provenant_records::SourceDigest::parse(&data.source_digest)
                .map_err(|_| io::Error::other("release source digest is invalid"))?,
            asset_digest: provenant_records::SourceDigest::parse(&data.asset_digest)
                .map_err(|_| io::Error::other("release asset digest is invalid"))?,
            target: data.target,
            toolchain: data.toolchain,
            record_schema_version: data.record_schema_version,
            config_schema_version: data.config_schema_version,
        })
    }
}

#[derive(Default)]
struct PreviewReporter;
impl InstallReporter for PreviewReporter {
    fn preview(&mut self, actions: &[Action]) -> io::Result<()> {
        write_preview("actions", actions)
    }
    fn findings(&mut self, findings: &[InstallFinding]) -> io::Result<()> {
        write_preview("findings", findings)
    }
    fn manual_includes(&mut self, includes: &[ManualInclude]) -> io::Result<()> {
        write_preview("manual_includes", includes)
    }
}
fn write_preview<T: Serialize + ?Sized>(kind: &str, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&serde_json::json!({"kind":kind,"items":value}))
        .map_err(|_| io::Error::other("cannot encode install preview"))?;
    bytes.push(b'\n');
    io::stderr().lock().write_all(&bytes)
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
fn install_error(command: &str, error: &InstallError, globals: &cli::Globals) -> u8 {
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
            let mut human = format!("{command}: {} action(s)\n", outcome.actions.len());
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
    let smoke = match SystemSmoke::new() {
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

fn check_error(error: InstallCheckError, globals: &cli::Globals) -> u8 {
    match error {
        InstallCheckError::Cursor => super::fail(
            "check install",
            Outcome::Invalid,
            "CURSOR_INVALID",
            "invalid check install cursor",
            globals,
        ),
        InstallCheckError::StaleCursor => super::fail(
            "check install",
            Outcome::Invalid,
            "CURSOR_STALE",
            "check install cursor is stale",
            globals,
        ),
        InstallCheckError::Install(error) => install_error("check install", &error, globals),
    }
}
pub(super) fn run_check_install(args: cli::InstallCheck, globals: &cli::Globals) -> u8 {
    let all = [
        Harness::Claude,
        Harness::Codex,
        Harness::Cursor,
        Harness::Agy,
        Harness::Opencode,
    ];
    let targets = match receipt_targets(globals, &all) {
        Ok(value) => value,
        Err(error) => return edge_error("check install", error, globals),
    };
    let fs = LocalFs;
    let cancel = CancelToken::new();
    let smoke = match SystemSmoke::new() {
        Ok(smoke) => smoke,
        Err(error) => {
            return super::fail(
                "check install",
                Outcome::Error,
                "SMOKE_EXECUTOR",
                &error.to_string(),
                globals,
            );
        }
    };
    let result = cli::install::execute_check_install(
        &args,
        &targets,
        &fs,
        &smoke,
        &cancel,
        env!("PROVENANT_TARGET_TRIPLE"),
    );
    let page = match result {
        Ok(page) => page,
        Err(error) => return check_error(error, globals),
    };
    let status = if page.count == 0 {
        Outcome::Ok
    } else {
        Outcome::Failed
    };
    let human = format!("check install: {} finding(s)\n", page.count);
    super::emit(
        &CommandResult::new("check install", status, page),
        globals,
        &human,
        true,
    )
}
