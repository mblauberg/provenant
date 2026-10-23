//! Native command edge for S3 worktree operations.
//!
//! The top-level parser flattens `GlobalOptions` and mounts `WorktreeArgs`
//! under `Command::Worktree`. This module owns only the frozen worktree leaf
//! grammar and composes confinement with supervisor's safe OS APIs.

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, error::ErrorKind};
use provenant_confinement::{
    utility::{UtilityBudget, UtilityRuntime},
    worktrees::{
        self, PruneScope, SystemWorkerObserver, WorktreeContext, WorktreeCreate, WorktreeRemove,
    },
};
use provenant_core::{CancelToken, Clock, GitOid, ProjectId, RootInputs, WaitBudget};
use provenant_supervisor::{
    census::{ProcessObservation, current_process},
    clock::SystemClock,
    exec::SystemExecutor,
    fs::{FileKind, FsError, LocalFs, SafeDir, SafeFs, canonicalize, effective_uid},
    pid::{CwdObservation, CwdObserver, SystemCwdObserver},
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    env,
    error::Error,
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const STATE_ENV: &str = "PROVENANT_STATE_DIR";
const HOME_ENV: &str = "HOME";
const UTILITY_HOME: &str = "utility-home";
const WORKTREES_DIR: &str = "worktrees";
const GIT_PATH: &str = "/usr/bin/git";
const WORKTREE_BUDGET: Duration = Duration::from_secs(60);
const MAX_RESULT_BYTES: usize = 1024 * 1024;

/// Flatten this beside the root subcommand enum. Clap's global flag propagation
/// accepts these options before or after any nested command.
#[derive(Args, Clone, Debug, Default)]
pub struct GlobalOptions {
    /// Select a verified project instead of the caller's current directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<PathBuf>,

    /// Select a fresh state root. Overrides PROVENANT_STATE_DIR.
    #[arg(long, global = true, value_name = "ABS")]
    pub state_dir: Option<PathBuf>,

    /// Render the versioned JSON result envelope.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub json: bool,

    /// Pure help for the current command, without a short alias.
    #[arg(long = "help", global = true, action = ArgAction::HelpLong)]
    _help: Option<bool>,
}

/// Mount under the top-level CLI as `Worktree(WorktreeArgs)`.
#[derive(Args, Clone, Debug)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    pub command: WorktreeCommand,
}

#[derive(Subcommand, Clone, Debug)]
pub enum WorktreeCommand {
    Create {
        #[arg(value_name = "BRANCH")]
        branch: String,
        #[arg(long, value_name = "REV")]
        base: Option<String>,
    },
    List,
    Remove(RemoveArgs),
}

#[derive(Args, Clone, Debug)]
pub struct RemoveArgs {
    /// A single managed worktree name. Conflicts with --merged.
    #[arg(value_name = "NAME", conflicts_with = "merged")]
    pub name: Option<String>,

    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "name")]
    pub merged: bool,

    #[arg(long, requires = "merged", value_name = "REF")]
    pub integration: Option<String>,

    /// Repeat once per branch; limited to 100 distinct branches.
    #[arg(long, action = ArgAction::Append, value_name = "NAME", conflicts_with = "since")]
    pub branch: Vec<String>,

    #[arg(
        long,
        requires = "merged",
        value_name = "OID",
        conflicts_with = "branch"
    )]
    pub since: Option<GitOid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeOperation {
    Create {
        branch: String,
        base: Option<String>,
    },
    List,
    Remove(WorktreeRemove),
}

#[derive(Debug, Clone)]
pub struct WorktreeRequest {
    pub globals: GlobalOptions,
    pub operation: WorktreeOperation,
}

/// Parse only for focused parser tests or a small top-level embedding. The
/// product parser should flatten GlobalOptions and mount WorktreeArgs in its
/// exhaustive command tree.
#[derive(Parser)]
#[command(
    name = "provenant",
    disable_version_flag = true,
    disable_help_flag = true
)]
struct WorktreeOnlyCli {
    #[command(flatten)]
    globals: GlobalOptions,
    #[command(subcommand)]
    command: WorktreeOnlyCommand,
}

#[derive(Subcommand)]
enum WorktreeOnlyCommand {
    Worktree(WorktreeArgs),
}

pub fn parse_worktree(
    args: impl IntoIterator<Item = OsString>,
) -> Result<WorktreeRequest, clap::Error> {
    let parsed = WorktreeOnlyCli::try_parse_from(args)?;
    let WorktreeOnlyCommand::Worktree(args) = parsed.command;
    request_from(parsed.globals, args)
        .map_err(|message| WorktreeOnlyCli::command().error(ErrorKind::ArgumentConflict, message))
}

pub fn request_from(globals: GlobalOptions, args: WorktreeArgs) -> Result<WorktreeRequest, String> {
    validate_globals(&globals)?;
    let operation = match args.command {
        WorktreeCommand::Create { branch, base } => {
            if branch.is_empty() {
                return Err("BRANCH must not be empty".into());
            }
            if base.as_ref().is_some_and(String::is_empty) {
                return Err("--base REV must not be empty".into());
            }
            WorktreeOperation::Create { branch, base }
        }
        WorktreeCommand::List => WorktreeOperation::List,
        WorktreeCommand::Remove(args) => {
            let operation = if args.merged {
                if args.name.is_some() {
                    return Err("NAME conflicts with --merged".into());
                }
                let integration = args
                    .integration
                    .ok_or_else(|| "--merged requires --integration REF".to_owned())?;
                if integration.is_empty() {
                    return Err("--integration REF must not be empty".into());
                }
                let scope =
                    match (args.branch.is_empty(), args.since) {
                        (false, None) => {
                            if args.branch.len() > 100 {
                                return Err("--branch may be repeated at most 100 times".into());
                            }
                            let mut distinct = BTreeSet::new();
                            if args.branch.iter().any(|branch| {
                                branch.is_empty() || !distinct.insert(branch.as_str())
                            }) {
                                return Err("--branch values must be nonempty and distinct".into());
                            }
                            PruneScope::Branches(args.branch)
                        }
                        (true, Some(since)) => PruneScope::Since(since),
                        _ => {
                            return Err(
                                "--merged requires exactly one of --branch NAME... or --since OID"
                                    .into(),
                            );
                        }
                    };
                WorktreeRemove::Merged { integration, scope }
            } else {
                if args.integration.is_some() || !args.branch.is_empty() || args.since.is_some() {
                    return Err("--integration, --branch and --since require --merged".into());
                }
                let name = args
                    .name
                    .ok_or_else(|| "remove requires NAME or --merged".to_owned())?;
                if name.is_empty() {
                    return Err("NAME must not be empty".into());
                }
                WorktreeRemove::Name(name)
            };
            WorktreeOperation::Remove(operation)
        }
    };
    Ok(WorktreeRequest { globals, operation })
}

fn validate_globals(globals: &GlobalOptions) -> Result<(), String> {
    if globals
        .project
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err("--project PATH must not be empty".into());
    }
    if let Some(path) = &globals.state_dir {
        if !path.is_absolute() {
            return Err("--state-dir ABS must be absolute".into());
        }
    }
    Ok(())
}

/// Run one parsed worktree request through the confinement owner.
pub fn execute(request: WorktreeRequest, cancel: &CancelToken) -> Result<Value, Box<dyn Error>> {
    let fs: Arc<dyn SafeFs> = Arc::new(LocalFs);
    let selected = selected_project_path(request.globals.project.as_deref())?;
    let project_path = canonicalize(&selected)?;

    let home = env::var_os(HOME_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unavailable"))?;
    let mut inputs = RootInputs::new(home, project_path.clone());
    inputs.state_dir = request.globals.state_dir.clone();
    inputs.environment_state_dir = env::var_os(STATE_ENV).map(PathBuf::from);
    let state = inputs.resolve(&LocalFs)?.state;
    let uid = effective_uid();
    let state_dir = ensure_private_absolute_dir(fs.as_ref(), state.as_path(), uid)?;
    let utility_home_path = state.as_path().join(UTILITY_HOME);
    let _utility_home = ensure_private_child(state_dir.as_ref(), OsStr::new(UTILITY_HOME), uid)?;
    let worktrees_dir = ensure_private_child(state_dir.as_ref(), OsStr::new(WORKTREES_DIR), uid)?;

    let executor = SystemExecutor::new(4)?;
    let utility =
        UtilityRuntime::git_only(Arc::clone(&fs), Path::new(GIT_PATH), &utility_home_path)?;
    let repo = utility.open_repo(&project_path)?;
    let project_id = project_id(&repo);
    let project_state = ensure_private_child(
        worktrees_dir.as_ref(),
        OsStr::new(&project_id.to_string()),
        uid,
    )?;
    let workers = SystemWorkerObserver;
    let clock = SystemClock::new();
    let utility_budget = UtilityBudget::bounded(WORKTREE_BUDGET)?;
    let lock_budget = WaitBudget::cli(clock.mono_now(), 60)?;
    let context = WorktreeContext {
        repo: &repo,
        project_state: project_state.as_ref(),
        fs: fs.as_ref(),
        executor: &executor,
        utility: &utility,
        github_repository: None,
        workers: &workers,
        clock: &clock,
        utility_budget,
        lock_budget,
    };
    let result = match request.operation {
        WorktreeOperation::Create { branch, base } => {
            let info = worktrees::create(&context, WorktreeCreate { branch, base }, cancel)?;
            json!({"kind": "created", "worktree": serde_json::to_value(info)?})
        }
        WorktreeOperation::List => {
            let list = worktrees::list(&context, cancel)?;
            let items = serde_json::to_value(list)?;
            json!({"kind": "listed", "worktrees": items})
        }
        WorktreeOperation::Remove(remove) => {
            let outcome = worktrees::remove(&context, remove, cancel)?;
            json!({
                "kind": "removed",
                "removed": serde_json::to_value(outcome.removed)?,
                "pruned_refs": outcome.pruned_refs,
                "retained": outcome.retained,
            })
        }
    };
    Ok(result)
}

fn selected_project_path(project: Option<&Path>) -> Result<PathBuf, Box<dyn Error>> {
    let current = match current_process() {
        ProcessObservation::Present(fact) => fact,
        ProcessObservation::Absent | ProcessObservation::Unknown(_) => {
            return Err(io::Error::other("cannot identify the current process").into());
        }
    };
    let cwd = match SystemCwdObserver.cwd(current.identity.pid)? {
        CwdObservation::Path(path) => path,
        CwdObservation::Gone | CwdObservation::PermissionDenied | CwdObservation::Unsupported => {
            return Err(io::Error::other("cannot observe the current directory").into());
        }
    };
    if !cwd.is_absolute() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "observed CWD is not absolute").into(),
        );
    }
    let Some(project) = project else {
        return Ok(cwd);
    };
    if project.is_absolute() {
        Ok(project.to_path_buf())
    } else {
        Ok(cwd.join(project))
    }
}

/// Bind an absolute path by descriptor-relative, no-follow component traversal.
/// New directories are mode 0700 and each parent is synced after mkdir. Only
/// the final directory must belong to the current user and have exact mode 0700.
fn ensure_private_absolute_dir(
    fs: &dyn SafeFs,
    path: &Path,
    uid: u32,
) -> Result<Box<dyn SafeDir>, Box<dyn Error>> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state root is not a normalized absolute path",
        )
        .into());
    }
    let mut current = fs.root(true)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current = open_or_create_directory(current.as_ref(), name, true)?;
    }
    require_private_owner(current.as_ref(), uid)?;
    Ok(current)
}

fn ensure_private_child(
    parent: &dyn SafeDir,
    name: &OsStr,
    uid: u32,
) -> Result<Box<dyn SafeDir>, Box<dyn Error>> {
    let child = open_or_create_directory(parent, name, true)?;
    require_private_owner(child.as_ref(), uid)?;
    Ok(child)
}

fn open_or_create_directory(
    parent: &dyn SafeDir,
    name: &OsStr,
    create: bool,
) -> Result<Box<dyn SafeDir>, Box<dyn Error>> {
    let before = match parent.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if create && error.is_not_found() => {
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            parent.metadata_at(name, false)?
        }
        Err(error) => return Err(error.into()),
    };
    if before.kind != FileKind::Directory {
        return Err(FsError::RaceRefused { name: name.into() }.into());
    }
    let child = parent.open_directory(name, false)?;
    let opened = child.metadata()?;
    let after = parent.metadata_at(name, false)?;
    if opened.kind != FileKind::Directory
        || !before.same_identity(&opened)
        || after.kind != FileKind::Directory
        || !opened.same_identity(&after)
    {
        return Err(FsError::RaceRefused { name: name.into() }.into());
    }
    Ok(child)
}

fn require_private_owner(directory: &dyn SafeDir, uid: u32) -> Result<(), Box<dyn Error>> {
    let metadata = directory.metadata()?;
    if metadata.kind != FileKind::Directory
        || metadata.uid != uid
        || metadata.mode & 0o777 != 0o700
        || metadata.mode & 0o7000 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "state directory must be owned by the current user with mode 0700",
        )
        .into());
    }
    Ok(())
}

/// Project IDs remain stable across linked worktrees because they bind to the
/// verified common Git directory. This fallback is needed until state exposes
/// a project registry identity API.
fn project_id(repo: &provenant_confinement::VerifiedRepo) -> ProjectId {
    let metadata = repo.common_identity();
    let mut hasher = Sha256::new();
    hasher.update(b"provenant.worktree-project.v1\0");
    hasher.update(metadata.device.to_be_bytes());
    hasher.update(metadata.inode.to_be_bytes());
    let digest = hasher.finalize();
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    ProjectId::from_bytes(id)
}

#[derive(Debug, Serialize)]
struct CommandResult<'a> {
    schema_version: u64,
    command: &'static str,
    status: &'static str,
    data: Option<&'a Value>,
    error: Option<CommandError<'a>>,
}

#[derive(Debug, Serialize)]
struct CommandError<'a> {
    code: &'static str,
    message: &'a str,
    cause: Option<&'a str>,
    repair: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct Failure {
    status: &'static str,
    exit: u8,
    code: &'static str,
    message: &'static str,
    repair: Option<&'static str>,
}

const INVALID_ARGUMENT: Failure = Failure {
    status: "invalid",
    exit: 2,
    code: "INVALID_ARGUMENT",
    message: "invalid worktree arguments",
    repair: None,
};

const SERIALIZATION_FAILURE: Failure = Failure {
    status: "error",
    exit: 3,
    code: "RESULT_ENCODING_FAILED",
    message: "worktree result could not be encoded",
    repair: None,
};

const RESULT_TOO_LARGE: Failure = Failure {
    status: "error",
    exit: 3,
    code: "RESULT_TOO_LARGE",
    message: "worktree result exceeds the 1 MiB output limit",
    repair: Some("Remove known stale managed worktrees, then retry the unpaged list."),
};

/// Exact pre-dispatch recognizer for the native worktree subtree. `args`
/// includes argv[0]; only the three frozen root globals may precede `worktree`.
pub fn is_worktree_invocation(args: &[OsString]) -> bool {
    let mut index = if args.is_empty() { 0 } else { 1 };
    while let Some(argument) = args.get(index) {
        if argument == OsStr::new("--json") {
            index += 1;
            continue;
        }
        if argument == OsStr::new("--project") || argument == OsStr::new("--state-dir") {
            index += 2;
            continue;
        }
        if argument.to_str().is_some_and(|value| {
            value.starts_with("--project=") || value.starts_with("--state-dir=")
        }) {
            index += 1;
            continue;
        }
        return argument == OsStr::new("worktree");
    }
    false
}

/// Native command entry point for the root binary's early worktree dispatch.
pub fn run(args: &[OsString]) -> u8 {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_with_io(args, &mut stdout, &mut stderr)
}

fn run_with_io(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    match parse_worktree(args.iter().cloned()) {
        Ok(request) => {
            let json_mode = request.globals.json;
            let cancel = CancelToken::new();
            match execute(request, &cancel) {
                Ok(data) => write_success(stdout, stderr, &data, json_mode),
                Err(error) => {
                    write_failure(stdout, stderr, classify_error(error.as_ref()), json_mode)
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            let bytes = error.to_string().into_bytes();
            if bytes.len() > MAX_RESULT_BYTES || stdout.write_all(&bytes).is_err() {
                3
            } else {
                0
            }
        }
        Err(_) => write_failure(stdout, stderr, INVALID_ARGUMENT, json_requested(args)),
    }
}

fn json_requested(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument.as_os_str() == OsStr::new("--json"))
}

fn classify_error(error: &(dyn Error + 'static)) -> Failure {
    use provenant_confinement::worktrees::WorktreeError;

    let Some(error) = error.downcast_ref::<WorktreeError>() else {
        return Failure {
            status: "error",
            exit: 3,
            code: "WORKTREE_ERROR",
            message: "worktree operation could not be completed",
            repair: None,
        };
    };
    match error {
        WorktreeError::InvalidRequest | WorktreeError::NotFound => Failure {
            status: "invalid",
            exit: 2,
            code: if matches!(error, WorktreeError::NotFound) {
                "NOT_FOUND"
            } else {
                "INVALID_ARGUMENT"
            },
            message: "worktree request is invalid or its target was not found",
            repair: None,
        },
        WorktreeError::Cancelled => Failure {
            status: "cancelled",
            exit: 130,
            code: "CANCELLED",
            message: "worktree operation was cancelled",
            repair: None,
        },
        WorktreeError::Dirty
        | WorktreeError::LiveWriter
        | WorktreeError::UnknownWriter
        | WorktreeError::CheckedOut
        | WorktreeError::Primary => Failure {
            status: "refused",
            exit: 1,
            code: "WORKTREE_REFUSED",
            message: "worktree operation was refused by custody or repository policy",
            repair: None,
        },
        _ => Failure {
            status: "error",
            exit: 3,
            code: "WORKTREE_ERROR",
            message: "worktree operation could not be completed",
            repair: None,
        },
    }
}

fn write_success(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    data: &Value,
    json_mode: bool,
) -> u8 {
    let bytes = if json_mode {
        encode_json(&CommandResult {
            schema_version: 1,
            command: "worktree",
            status: "ok",
            data: Some(data),
            error: None,
        })
    } else {
        serde_json::to_vec_pretty(data).map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
    };
    let Ok(bytes) = bytes else {
        return write_failure(stdout, stderr, SERIALIZATION_FAILURE, json_mode);
    };
    if bytes.len() > MAX_RESULT_BYTES {
        return write_failure(stdout, stderr, RESULT_TOO_LARGE, json_mode);
    }
    if stdout.write_all(&bytes).is_err() {
        let _ = writeln!(stderr, "provenant: ERROR: could not write worktree result");
        3
    } else {
        0
    }
}

fn write_failure(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    failure: Failure,
    json_mode: bool,
) -> u8 {
    if json_mode {
        let result = CommandResult {
            schema_version: 1,
            command: "worktree",
            status: failure.status,
            data: None,
            error: Some(CommandError {
                code: failure.code,
                message: failure.message,
                cause: None,
                repair: failure.repair,
            }),
        };
        match encode_json(&result) {
            Ok(bytes) if bytes.len() <= MAX_RESULT_BYTES && stdout.write_all(&bytes).is_ok() => {}
            _ => return 3,
        }
    } else if writeln!(
        stderr,
        "provenant: {}: {}",
        failure.status.to_ascii_uppercase(),
        failure.message
    )
    .is_err()
    {
        return 3;
    }
    failure.exit
}

fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn create_parses_global_options_on_either_side_of_subcommands() -> Result<(), Box<dyn Error>> {
        let before = parse_worktree(args(&[
            "provenant",
            "--json",
            "--project",
            "/tmp/project",
            "--state-dir",
            "/tmp/state",
            "worktree",
            "create",
            "feature/s3",
            "--base",
            "refs/heads/main",
        ]))?;
        assert!(before.globals.json);
        assert_eq!(
            before.globals.project.as_deref(),
            Some(Path::new("/tmp/project"))
        );
        assert_eq!(
            before.globals.state_dir.as_deref(),
            Some(Path::new("/tmp/state"))
        );
        assert_eq!(
            before.operation,
            WorktreeOperation::Create {
                branch: "feature/s3".into(),
                base: Some("refs/heads/main".into())
            }
        );

        let after = parse_worktree(args(&[
            "provenant",
            "worktree",
            "create",
            "feature/s3",
            "--project",
            "/tmp/project",
            "--state-dir",
            "/tmp/state",
            "--json",
        ]))?;
        assert!(after.globals.json);
        assert_eq!(
            after.operation,
            WorktreeOperation::Create {
                branch: "feature/s3".into(),
                base: None
            }
        );
        Ok(())
    }

    #[test]
    fn remove_requires_a_name_or_a_fully_scoped_merged_selection() {
        for argv in [
            args(&["provenant", "worktree", "remove"]),
            args(&["provenant", "worktree", "remove", "topic", "--merged"]),
            args(&["provenant", "worktree", "remove", "--merged"]),
            args(&[
                "provenant",
                "worktree",
                "remove",
                "--merged",
                "--integration",
                "main",
                "--branch",
                "one",
                "--since",
                &"a".repeat(40),
            ]),
            args(&[
                "provenant",
                "worktree",
                "remove",
                "--merged",
                "--integration",
                "main",
                "--since",
                &"A".repeat(40),
            ]),
            args(&[
                "provenant",
                "worktree",
                "remove",
                "--merged",
                "--integration",
                "main",
                "--branch",
                "one",
                "--branch",
                "one",
            ]),
        ] {
            assert!(parse_worktree(argv).is_err());
        }
    }

    #[test]
    fn merged_branch_scope_is_repeatable_bounded_and_disjoint_from_since()
    -> Result<(), Box<dyn Error>> {
        let request = parse_worktree(args(&[
            "provenant",
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "refs/heads/main",
            "--branch",
            "one",
            "--branch",
            "two",
        ]))?;
        assert_eq!(
            request.operation,
            WorktreeOperation::Remove(WorktreeRemove::Merged {
                integration: "refs/heads/main".into(),
                scope: PruneScope::Branches(vec!["one".into(), "two".into()])
            })
        );
        let since = parse_worktree(args(&[
            "provenant",
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--since",
            &"a".repeat(40),
        ]))?;
        assert!(matches!(
            since.operation,
            WorktreeOperation::Remove(WorktreeRemove::Merged {
                scope: PruneScope::Since(GitOid::Sha1(_)),
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn parser_rejects_relative_state_roots_and_conflicting_singletons() {
        assert!(
            parse_worktree(args(&[
                "provenant",
                "--state-dir",
                "relative",
                "worktree",
                "list",
            ]))
            .is_err()
        );
        assert!(
            parse_worktree(args(&[
                "provenant",
                "worktree",
                "list",
                "--project",
                "/one",
                "--project",
                "/two",
            ]))
            .is_err()
        );
        assert!(parse_worktree(args(&["provenant", "worktree", "list", "-h"])).is_err());
    }

    #[test]
    fn merged_branch_scope_stops_at_one_hundred() -> Result<(), Box<dyn Error>> {
        let mut at_limit = args(&[
            "provenant",
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
        ]);
        for index in 0..100 {
            at_limit.push("--branch".into());
            at_limit.push(format!("branch-{index}").into());
        }
        let parsed = parse_worktree(at_limit)?;
        assert!(matches!(
            parsed.operation,
            WorktreeOperation::Remove(WorktreeRemove::Merged {
                scope: PruneScope::Branches(ref branches),
                ..
            }) if branches.len() == 100
        ));

        let mut over_limit = args(&[
            "provenant",
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
        ]);
        for index in 0..101 {
            over_limit.push("--branch".into());
            over_limit.push(format!("branch-{index}").into());
        }
        assert!(parse_worktree(over_limit).is_err());
        Ok(())
    }

    #[test]
    fn early_router_recognizes_only_the_native_worktree_subtree() {
        assert!(is_worktree_invocation(&args(&[
            "provenant",
            "--project=/tmp/project",
            "--json",
            "worktree",
            "list",
        ])));
        assert!(is_worktree_invocation(&args(&[
            "provenant",
            "--state-dir",
            "/tmp/state",
            "worktree",
            "remove",
            "topic",
        ])));
        assert!(!is_worktree_invocation(&args(&[
            "provenant",
            "--project",
            "/tmp/project",
            "workflow",
            "list",
        ])));
        assert!(!is_worktree_invocation(&args(&[
            "provenant",
            "worktreex",
            "list",
        ])));
    }

    #[test]
    fn help_is_pure_and_invalid_json_uses_the_versioned_envelope() -> Result<(), Box<dyn Error>> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with_io(
            &args(&["provenant", "worktree", "remove", "--help"]),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert!(std::str::from_utf8(&stdout)?.contains("--merged"));

        stdout.clear();
        let code = run_with_io(
            &args(&["provenant", "--json", "worktree", "remove", "--merged"]),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
        let envelope: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["command"], "worktree");
        assert_eq!(envelope["status"], "invalid");
        assert_eq!(envelope["data"], Value::Null);
        assert_eq!(envelope["error"]["cause"], Value::Null);
        assert_eq!(envelope["error"]["repair"], Value::Null);
        Ok(())
    }

    #[test]
    fn result_limit_fails_without_truncating() -> Result<(), Box<dyn Error>> {
        let data = json!({"large": "x".repeat(MAX_RESULT_BYTES)});
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = write_success(&mut stdout, &mut stderr, &data, true);
        assert_eq!(code, 3);
        let envelope: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["error"]["code"], "RESULT_TOO_LARGE");
        assert!(stdout.len() < MAX_RESULT_BYTES);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[test]
    fn typed_owner_failures_map_to_the_frozen_exit_contract() {
        use provenant_confinement::worktrees::WorktreeError;

        let missing = classify_error(&WorktreeError::NotFound);
        assert_eq!((missing.status, missing.exit), ("invalid", 2));
        let dirty = classify_error(&WorktreeError::Dirty);
        assert_eq!((dirty.status, dirty.exit), ("refused", 1));
        let cancelled = classify_error(&WorktreeError::Cancelled);
        assert_eq!((cancelled.status, cancelled.exit), ("cancelled", 130));
        let incomplete = classify_error(&WorktreeError::InvalidOutput);
        assert_eq!((incomplete.status, incomplete.exit), ("error", 3));
    }
}
