//! Binary edge for checks: capture roots once, then render the owner's page.
use crate::{emit, fail, roots};
use provenant_cli::{
    Check, Globals,
    check::{CheckError, CheckPage, GitContext},
};
use provenant_config::{
    PortBackend, PortCommandDisposition, PortSource, PortTable, Release, Targets,
};
use provenant_core::CancelToken;
use provenant_os::exec::SystemExecutor;
use provenant_os::fs;
use provenant_records::{CanonicalV1, CommandResult, Finding, Outcome, PathWire};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
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
    Exec(#[from] provenant_os::exec::ExecError),
    #[error(transparent)]
    Writing(#[from] provenant_workflow::writing::WritingError),
    #[error(transparent)]
    Port(#[from] ReleasePortFailure),
}
#[derive(Debug, Error)]
enum ReleasePortFailure {
    #[error("release port is disabled")]
    Disabled,
    #[error("release command returned {0}")]
    Command(&'static str),
    #[error("release command returned incomplete or truncated output")]
    Incomplete,
    #[error("release command returned invalid finding JSON")]
    InvalidOutput,
    #[error("cannot record release command evidence")]
    Evidence,
    #[error("cannot start release command executor")]
    Executor,
}
impl Error {
    fn outcome(&self) -> Outcome {
        match self {
            Self::Check(error) => error.outcome(),
            Self::Port(_) => Outcome::Unavailable,
            Self::Writing(
                provenant_workflow::writing::WritingError::Invalid(_)
                | provenant_workflow::writing::WritingError::Rules
                | provenant_workflow::writing::WritingError::RulesLimit,
            ) => Outcome::Invalid,
            Self::Root(roots::RootError::Checkout | roots::RootError::State(_)) => Outcome::Invalid,
            _ => Outcome::Error,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseCommandOutput {
    findings: Vec<ReleaseCommandFinding>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseCommandFinding {
    rule: String,
    path: PathWire,
    line: usize,
    message: String,
}

enum Inspectable {
    Writing {
        paths: Vec<PathBuf>,
        rules: provenant_cli::Rules,
        page: provenant_cli::Page,
    },
    Config(provenant_cli::Page),
    Install {
        source: Option<PathBuf>,
        page: provenant_cli::Page,
    },
    Release {
        base: Option<String>,
        head: Option<String>,
        page: provenant_cli::Page,
    },
}
pub fn run(
    check: Check,
    globals: &Globals,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> u8 {
    let (name, check) = match check {
        Check::Providers(args) => return crate::check_providers::run(args, globals, workers),
        Check::Config(page) => ("check config", Inspectable::Config(page)),
        Check::Install { source, page } => ("check install", Inspectable::Install { source, page }),
        Check::Release { base, head, page } => {
            ("check release", Inspectable::Release { base, head, page })
        }
        Check::Writing { paths, rules, page } => {
            ("check writing", Inspectable::Writing { paths, rules, page })
        }
    };
    run_inspection(name, check, globals)
}

pub fn config(page: provenant_cli::Page, globals: &Globals) -> u8 {
    run_inspection("config check", Inspectable::Config(page), globals)
}

fn run_inspection(name: &str, check: Inspectable, globals: &Globals) -> u8 {
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
        Err(Error::Install(error)) => crate::install::install_error(name, &error, globals),
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
#[derive(serde::Serialize)]
#[serde(untagged)]
enum Inspection {
    Check(CheckPage),
    Writing {
        #[serde(flatten)]
        page: CheckPage<provenant_workflow::writing::FileFinding>,
        blockers: usize,
    },
}
impl Inspection {
    fn outcome(&self) -> Outcome {
        match self {
            Self::Check(p) => p.outcome(),
            Self::Writing { page, .. } => page.outcome(),
        }
    }
    fn human(&self) -> Result<String, serde_json::Error> {
        match self {
            Self::Check(p) => p.human(),
            Self::Writing { page: p, blockers } => {
                let mut text = String::new();
                for row in &p.findings {
                    let f = &row.finding;
                    text.push_str(&format!(
                        "{}:{}:{}: {:?}: {}: {}\n",
                        serde_json::to_string(&row.path)?,
                        f.line,
                        f.column,
                        f.severity,
                        f.rule,
                        f.message
                    ));
                }
                text.push_str(&format!("{} finding(s), {blockers} blocker(s)\n", p.count));
                if let Some(cursor) = &p.next_cursor {
                    text.push_str(&format!("next cursor: {cursor}\n"));
                }
                Ok(text)
            }
        }
    }
}
fn inspect(check: Inspectable, globals: &Globals) -> Result<Inspection, Error> {
    let workspace = if matches!(check, Inspectable::Release { .. }) {
        roots::Workspace::Checkout
    } else {
        roots::Workspace::Project
    };
    let roots::Resolved {
        assets,
        home,
        project,
        roots,
        ..
    } = roots::resolve(globals, workspace, roots::Product::Required)?;
    let selector = (
        env!("PROVENANT_SOURCE_DIGEST"),
        assets.as_os_str().as_encoded_bytes(),
        roots.state.as_path().as_os_str().as_encoded_bytes(),
        project.as_os_str().as_encoded_bytes(),
    );
    if let Inspectable::Writing { paths, rules, page } = check {
        use provenant_workflow::writing::{self, RuleSet};
        let set = match rules {
            provenant_cli::Rules::General => RuleSet::General,
            provenant_cli::Rules::Academic => RuleSet::Academic,
            provenant_cli::Rules::Legal => RuleSet::Legal,
        };
        let inspection = writing::inspect(&assets, &project, &paths, set)?;
        let evidence = inspection
            .evidence
            .iter()
            .map(|(p, b)| {
                (
                    p.to_string_lossy().into_owned(),
                    provenant_records::CanonicalV1::file_digest(b),
                )
            })
            .collect();
        let blockers = inspection
            .findings
            .iter()
            .filter(|f| f.finding.severity == writing::Severity::Blocker)
            .count();
        return Ok(Inspection::Writing {
            blockers,
            page: provenant_cli::check::findings_page(
                inspection.findings,
                evidence,
                ("writing", set.name(), selector),
                &page,
            )?,
        });
    }
    let page = match check {
        Inspectable::Writing { .. } => return Err(CheckError::Invalid("writing selector").into()),
        Inspectable::Config(page) => {
            let inspection =
                provenant_config::inspect_config(&roots, &provenant_cli::domains::SECTIONS)?;
            Ok(provenant_cli::check::config_page(
                inspection,
                crate::pool_effort_warnings(&roots),
                ("config", selector),
                &page,
            )?)
        }
        Inspectable::Install { source, page } => private_home(|private| {
            let smoke = crate::release_smoke::SystemSmoke::new(Some(private))?;
            let cancel = CancelToken::new();
            let release = source
                .as_deref()
                .map(|path| {
                    // Resolve aliased ancestors (macOS /tmp, /var) as install does; the
                    // release tree itself still refuses symlinks.
                    let path = provenant_core::PathResolver::resolve_existing_ancestor(
                        &provenant_os::fs::SystemPathResolver,
                        path,
                    )
                    .unwrap_or_else(|_| path.to_path_buf());
                    Release::read(&path, env!("PROVENANT_TARGET_TRIPLE"))
                })
                .transpose()?;
            let mut targets = Targets::new(roots.clone(), &home, &[]);
            // A configuration defect is `check config`'s finding. It must not
            // stop this check, which then has no registered accounts to compare.
            if let Ok(config) = provenant_cli::config_reads::read_for(&roots, "check install") {
                targets.accounts = provenant_cli::accounts::homes(&config.domains.accounts);
                targets.shared_settings =
                    crate::install::shared_settings(&config.domains.accounts, &home)
                        .ok()
                        .flatten();
            }
            let inspection = provenant_config::inspect_install(
                &targets,
                release.as_ref(),
                env!("PROVENANT_TARGET_TRIPLE"),
                &smoke,
                &cancel,
            )?;
            Ok(provenant_cli::check::install_page(
                inspection,
                &home,
                roots.state.as_path(),
                &targets.accounts,
                (
                    "install",
                    &source.as_ref().map(|p| p.as_os_str().as_encoded_bytes()),
                    selector,
                ),
                &page,
            )?)
        }),
        Inspectable::Release { base, head, page } => {
            git_page(&project, roots.state.as_path(), |git| {
                let range = match base.zip(head) {
                    Some((base, head)) => Some((
                        provenant_cli::check::resolve_revision(git, &base)?,
                        provenant_cli::check::resolve_revision(git, &head)?,
                    )),
                    None => None,
                };
                let selector = (
                    "release",
                    range.map(|(a, b)| (a.to_string(), b.to_string())),
                    selector,
                );
                let ports = PortTable::read_for(Some(&project), roots.state.as_path())?;
                match ports.release.backend {
                    PortBackend::Builtin => {
                        provenant_cli::check::release_page(git, range, selector, &page)
                    }
                    PortBackend::Command { command } => command_release_page(
                        &project,
                        roots.state.as_path(),
                        command,
                        ports.release.source,
                        range,
                        selector,
                        &page,
                    ),
                    PortBackend::None => Err(ReleasePortFailure::Disabled.into()),
                }
            })
        }
    }?;
    Ok(Inspection::Check(page))
}
fn command_release_page(
    project: &Path,
    state: &Path,
    command: provenant_config::PortCommand,
    source: PortSource,
    range: Option<(provenant_core::GitOid, provenant_core::GitOid)>,
    selector: impl serde::Serialize,
    page: &provenant_cli::Page,
) -> Result<CheckPage, Error> {
    let executor = SystemExecutor::new(1).map_err(|_| ReleasePortFailure::Executor)?;
    let mut arguments = Vec::new();
    if let Some((base, head)) = range {
        arguments.push(std::ffi::OsString::from("--base"));
        arguments.push(std::ffi::OsString::from(base.to_string()));
        arguments.push(std::ffi::OsString::from("--head"));
        arguments.push(std::ffi::OsString::from(head.to_string()));
    }
    let cancel = CancelToken::new();
    let mut execution = provenant_config::execute_port_command(
        &executor,
        &command,
        project,
        &arguments,
        &[],
        Duration::from_secs(60),
        &cancel,
    );
    let output = (|| {
        let outcome = execution
            .outcome
            .as_ref()
            .ok_or_else(|| ReleasePortFailure::Command(disposition_name(execution.disposition)))?;
        if execution.disposition != PortCommandDisposition::Succeeded {
            return Err(ReleasePortFailure::Command(disposition_name(
                execution.disposition,
            )));
        }
        if !execution.capture_complete() {
            return Err(ReleasePortFailure::Incomplete);
        }
        let output: ReleaseCommandOutput = serde_json::from_slice(&outcome.stdout.bytes)
            .map_err(|_| ReleasePortFailure::InvalidOutput)?;
        if output.findings.iter().any(|finding| finding.line == 0) {
            return Err(ReleasePortFailure::InvalidOutput);
        }
        Ok(output)
    })();
    if output.is_err() && execution.disposition == PortCommandDisposition::Succeeded {
        execution.disposition = PortCommandDisposition::Unknown;
    }
    let range_key = range.map_or_else(
        || "working-tree".to_owned(),
        |(base, head)| format!("{base}:{head}"),
    );
    let key = format!(
        "{}:{range_key}",
        CanonicalV1::file_digest(project.as_os_str().as_encoded_bytes())
    );
    provenant_config::record_port_command(state, "release", source, "check", &key, &execution)
        .map_err(|_| ReleasePortFailure::Evidence)?;
    let output: ReleaseCommandOutput = output?;
    let normalized =
        serde_json::to_vec(&output.findings).map_err(|_| ReleasePortFailure::InvalidOutput)?;
    let findings_digest = CanonicalV1::file_digest(&normalized);
    let findings = output
        .findings
        .into_iter()
        .map(|finding| Finding {
            rule: finding.rule,
            path: finding.path,
            line: finding.line,
            message: finding.message,
        })
        .collect();
    let evidence = vec![("release-command-findings".to_owned(), findings_digest)];
    Ok(provenant_cli::check::findings_page(
        findings, evidence, selector, page,
    )?)
}

fn disposition_name(disposition: PortCommandDisposition) -> &'static str {
    match disposition {
        PortCommandDisposition::Succeeded => "succeeded",
        PortCommandDisposition::Unknown => "unknown",
        PortCommandDisposition::Refused => "refused",
        PortCommandDisposition::TimedOut => "timed_out",
        PortCommandDisposition::Cancelled => "cancelled",
        PortCommandDisposition::Failed => "failed",
    }
}

fn private_home<T>(operation: impl FnOnce(&Path) -> Result<T, Error>) -> Result<T, Error> {
    let temp = fs::canonicalize(&std::env::temp_dir()).map_err(io::Error::from)?;
    let anchor = provenant_os::fs::Directory::open_anchor(&temp)?;
    let name = provenant_os::fs::temporary_name()?;
    anchor.mkdir(&name, 0o700)?;
    let result = operation(&temp.join(&name));
    let cleanup = anchor.rmdir(&name);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(value), Err(error)) => {
            let _ = writeln!(
                io::stderr().lock(),
                "provenant: warning: cannot clean private check directory: {error}"
            );
            Ok(value)
        }
        (Ok(value), Ok(())) => Ok(value),
    }
}
fn git_page(
    project: &Path,
    host_state: &Path,
    operation: impl FnOnce(&GitContext<'_>) -> Result<CheckPage, CheckError>,
) -> Result<CheckPage, Error> {
    private_home(|home| {
        let git = provenant_os::git::git_executable()
            .ok_or(CheckError::Evidence("Git executable unavailable"))?;
        let git = fs::canonicalize(&git)?;
        let runtime = provenant_confinement::UtilityRuntime::new(git, home, host_state)?;
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let repo = runtime.open_read_repo(
            &executor,
            project,
            provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
            &cancel,
        )?;
        Ok(operation(&GitContext {
            host_state,
            repo: &repo,
            runtime: &runtime,
            executor: &executor,
            cancel: &cancel,
        })?)
    })
}
