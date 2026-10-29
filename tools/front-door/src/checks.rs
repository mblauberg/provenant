//! Binary edge for checks: capture roots once, then render the owner's page.
use crate::{emit, fail, roots};
use provenant_cli::{
    Check, Globals,
    check::{CheckError, CheckPage, GitContext},
};
use provenant_config::{Release, Targets};
use provenant_core::CancelToken;
use provenant_os::{exec::SystemExecutor, fs};
use provenant_records::{CommandResult, Outcome};
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
}
impl Error {
    fn outcome(&self) -> Outcome {
        match self {
            Self::Check(error) => error.outcome(),
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
        base: Option<provenant_core::GitOid>,
        head: Option<provenant_core::GitOid>,
        page: provenant_cli::Page,
    },
}
pub fn run(check: Check, globals: &Globals) -> u8 {
    let (name, check) = match check {
        Check::Config(page) => ("check config", Inspectable::Config(page)),
        Check::Install { source, page } => ("check install", Inspectable::Install { source, page }),
        Check::Release { base, head, page } => {
            ("check release", Inspectable::Release { base, head, page })
        }
        Check::Writing { paths, rules, page } => {
            ("check writing", Inspectable::Writing { paths, rules, page })
        }
    };
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
            targets.accounts =
                provenant_config::read_with_domains(&roots, &provenant_cli::domains::SECTIONS)
                    .map(|config| provenant_cli::accounts::homes(&config.domains.accounts))
                    .unwrap_or_default();
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
    }?;
    Ok(Inspection::Check(page))
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
    operation: impl FnOnce(&GitContext<'_>) -> Result<CheckPage, CheckError>,
) -> Result<CheckPage, Error> {
    private_home(|home| {
        let git = provenant_os::git::git_executable()
            .ok_or(CheckError::Evidence("Git executable unavailable"))?;
        let git = fs::canonicalize(&git)?;
        let runtime = provenant_confinement::UtilityRuntime::new(git, home)?;
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
