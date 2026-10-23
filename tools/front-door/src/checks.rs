//! Binary edge for checks: capture roots once, then render the owner's page.
use crate::{emit, fail, roots};
use provenant_cli::{
    Check, Globals,
    check::{CheckError, CheckPage, GitContext},
};
use provenant_config::{Release, ReleaseSmoke, Targets};
use provenant_core::CancelToken;
use provenant_records::{CommandResult, Outcome, VersionInfo};
use provenant_supervisor::{
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{self, LocalFs, SafeFs},
};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    Check(#[from] CheckError),
    #[error(transparent)]
    Root(#[from] roots::RootError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Install(#[from] provenant_config::InstallError),
    #[error(transparent)]
    Filesystem(#[from] fs::FsError),
    #[error("cannot prepare check context")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Utility(#[from] provenant_confinement::utility::UtilityError),
    #[error(transparent)]
    Exec(#[from] provenant_supervisor::exec::ExecError),
}
impl Error {
    fn outcome(&self) -> Outcome {
        match self {
            Self::Check(error) => error.outcome(),
            Self::Root(roots::RootError::Checkout | roots::RootError::State(_)) => Outcome::Invalid,
            _ => Outcome::Error,
        }
    }
}
pub fn run(check: Check, globals: &Globals) -> u8 {
    let name = match check {
        Check::Config(_) => "check config",
        Check::Install { .. } => "check install",
        Check::Harness(_) => "check harness",
        Check::Release { .. } => "check release",
        Check::Writing { .. } => "check writing",
    };
    if matches!(check, Check::Writing { .. }) {
        return crate::unavailable(name, "workflow", globals);
    }
    let result = inspect(check, globals);
    match result {
        Ok(page) => match page.human() {
            Ok(human) => emit(
                &CommandResult::new(name, page.outcome(), page),
                globals,
                &human,
                true,
            ),
            Err(_) => fail(
                name,
                Outcome::Error,
                "ENCODE",
                "cannot encode check findings",
                globals,
            ),
        },
        Err(error) => fail(
            name,
            error.outcome(),
            if error.outcome() == Outcome::Invalid {
                "INVALID"
            } else {
                "EVIDENCE"
            },
            &error.to_string(),
            globals,
        ),
    }
}
fn inspect(check: Check, globals: &Globals) -> Result<CheckPage, Error> {
    let assets = roots::product_root()?;
    let project = roots::selected_project(
        globals.project.as_deref(),
        matches!(check, Check::Harness(_) | Check::Release { .. }),
    )?;
    let roots = roots::roots(globals, &assets, &project)?;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or(roots::RootError::Home)?);
    let selector = (
        env!("PROVENANT_SOURCE_DIGEST"),
        assets.as_os_str().as_encoded_bytes(),
        roots.state.as_path().as_os_str().as_encoded_bytes(),
        project.as_os_str().as_encoded_bytes(),
    );
    match check {
        Check::Config(page) => {
            let inspection = provenant_config::inspect_config(
                &roots,
                &LocalFs,
                &provenant_config::DomainSections::<provenant_config::NoDomains>::new(&[]),
            )?;
            Ok(provenant_cli::check::config_page(
                inspection,
                ("config", selector),
                &page,
            )?)
        }
        Check::Install { source, page } => private_home(|private| {
            let executor = SystemExecutor::new(1)?;
            let smoke = Smoke {
                executor: &executor,
                home: private,
            };
            let cancel = CancelToken::new();
            let release = source
                .as_deref()
                .map(|path| Release::read(path, env!("PROVENANT_TARGET_TRIPLE"), &LocalFs))
                .transpose()?;
            let inspection = provenant_config::inspect_install(
                &Targets::new(roots.clone(), &home, &[]),
                release.as_ref(),
                env!("PROVENANT_TARGET_TRIPLE"),
                &LocalFs,
                &smoke,
                &cancel,
            )?;
            Ok(provenant_cli::check::install_page(
                inspection,
                &home,
                roots.state.as_path(),
                (
                    "install",
                    &source.as_ref().map(|p| p.as_os_str().as_encoded_bytes()),
                    selector,
                ),
                &page,
            )?)
        }),
        Check::Harness(page) => git_page(&project, |git| {
            provenant_cli::check::harness_page(git, ("harness", selector), &page)
        }),
        Check::Release { base, head, page } => {
            let range = base.zip(head);
            git_page(&project, |git| {
                provenant_cli::check::release_page(
                    git,
                    range,
                    (
                        "release",
                        range.map(|(a, b)| (a.to_string(), b.to_string())),
                        selector,
                    ),
                    &page,
                )
            })
        }
        Check::Writing { .. } => Err(CheckError::Unavailable("workflow").into()),
    }
}
fn private_home<T>(operation: impl FnOnce(&Path) -> Result<T, Error>) -> Result<T, Error> {
    let temp = fs::canonicalize(Path::new("/tmp"))?;
    let anchor = LocalFs.open_anchor(&temp)?;
    let name = LocalFs.temporary_name()?;
    anchor.mkdir(&name, 0o700)?;
    let result = operation(&temp.join(&name));
    let cleanup = anchor.rmdir(&name);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(value), Ok(())) => Ok(value),
    }
}
fn git_page(
    project: &Path,
    operation: impl FnOnce(&GitContext<'_>) -> Result<CheckPage, CheckError>,
) -> Result<CheckPage, Error> {
    private_home(|home| {
        let fs: Arc<dyn SafeFs> = Arc::new(LocalFs);
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
        .ok_or(CheckError::Evidence("Git executable unavailable"))?;
        let runtime = provenant_confinement::UtilityRuntime::new(fs, git, home)?;
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let repo = runtime.open_read_repo(
            &executor,
            project,
            provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
            &cancel,
        )?;
        Ok(operation(&GitContext {
            repo: &repo,
            runtime: &runtime,
            executor: &executor,
            cancel: &cancel,
        })?)
    })
}
struct Smoke<'a> {
    executor: &'a dyn Executor,
    home: &'a Path,
}
impl ReleaseSmoke for Smoke<'_> {
    fn check(&self, release: &Release, cancel: &CancelToken) -> io::Result<VersionInfo> {
        let env: Vec<(OsString, OsString)> = [
            ("HOME", self.home.as_os_str()),
            ("XDG_CONFIG_HOME", self.home.as_os_str()),
            ("CLAUDE_CONFIG_DIR", self.home.as_os_str()),
            ("CODEX_HOME", self.home.as_os_str()),
            ("PROVENANT_STATE_DIR", self.home.as_os_str()),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.to_owned()))
        .collect();
        let mut version = None;
        for args in [&["--version", "--json"][..], &["help"][..]] {
            let argv: Vec<_> =
                std::iter::once(release.source().join("bin/provenant").into_os_string())
                    .chain(args.iter().map(OsString::from))
                    .collect();
            let mut options = ExecOptions::new(&argv, self.home, Duration::from_secs(30));
            options.env = Some(&env);
            options.cancel = Some(cancel);
            options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
            options.stderr = StreamPolicy::Capture { limit: 8192 };
            options.stop_on_output_limit = true;
            let output = self.executor.execute(options).map_err(io::Error::other)?;
            if output.termination != Termination::Exit(0)
                || output.timed_out
                || output.cancelled
                || output.drain_truncated
                || output.overflow_stopped
                || output.stdout.truncated
                || output.stdout.read_failed
                || output.stderr.read_failed
            {
                return Err(io::Error::other("release smoke failed"));
            }
            if args.first() == Some(&"--version") {
                #[derive(serde::Deserialize)]
                struct VersionEnvelope {
                    command: String,
                    status: String,
                    data: VersionWire,
                }
                #[derive(serde::Deserialize)]
                struct VersionWire {
                    source_digest: provenant_records::SourceDigest,
                    asset_digest: provenant_records::SourceDigest,
                    target: String,
                    toolchain: String,
                    record_schema_version: u64,
                    config_schema_version: u64,
                }
                let envelope: VersionEnvelope = serde_json::from_slice(&output.stdout.bytes)
                    .map_err(|_| io::Error::other("release version record"))?;
                if envelope.command != "version" || envelope.status != "ok" {
                    return Err(io::Error::other("release version outcome"));
                }
                let data = envelope.data;
                version = Some(VersionInfo {
                    source_digest: data.source_digest,
                    asset_digest: data.asset_digest,
                    target: data.target,
                    toolchain: data.toolchain,
                    record_schema_version: data.record_schema_version,
                    config_schema_version: data.config_schema_version,
                });
            }
        }
        version.ok_or_else(|| io::Error::other("release identity unavailable"))
    }
}
