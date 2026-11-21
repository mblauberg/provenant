//! Build from a pinned remote-tracking commit in a disposable host checkout.
use provenant_config::{CargoBuildProfile, LaneBuildSettings};
use provenant_core::{Clock, GitOid};
use provenant_os::exec::{ExecError, ExecOptions, Executor, SystemExecutor, Termination};
use provenant_run::target_seed::{self, Manifest};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const PUBLICATION_WAIT_SECONDS: u64 = 10;
/// Bound on the output tail a failing `build.prepare` command carries into its
/// typed error, its refresh record and the seed result.
const OUTPUT_TAIL_BYTES: usize = 2 * 1024;

/// The single stderr seam for seed-build operator lines. Warnings and the
/// `build.prepare` trace are facts for the operator, never refusals by themselves.
#[allow(clippy::print_stderr)]
fn seed_build_stderr(line: impl std::fmt::Display) {
    eprintln!("{line}");
}

/// A failing `build.prepare` command. It travels as the error of `refresh` or
/// `build`, so a caller tells it from a generic failure by type. The public
/// fields let the tick record the command and its bounded output tail. The
/// tail is masked on the capture stream before the window bounds it, so every
/// later store and display sees only masked text; the manual
/// `worktree seed-build` path shows it as a cause, never as the refusal message.
#[derive(Debug)]
pub struct PrepareFailed {
    pub index: usize,
    pub argv: Vec<String>,
    pub exit: Option<i32>,
    pub tail: String,
}

impl std::fmt::Display for PrepareFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "build.prepare[{}] command failed: {}",
            self.index,
            self.argv.join(" ")
        )?;
        match self.exit {
            Some(code) => write!(formatter, " (exit {code})"),
            None => write!(formatter, " (no exit code)"),
        }
    }
}

impl Error for PrepareFailed {}

/// The `build.prepare` failure an error carries, if any, anywhere in its source
/// chain, like [`skip_of`] but for a failed command rather than a skip.
pub fn prepare_failed_of<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a PrepareFailed> {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if let Some(failed) = error.downcast_ref::<PrepareFailed>() {
            return Some(failed);
        }
        cause = error.source();
    }
    None
}

/// What a failed seed command leaves behind: its exit status, when the child
/// exited normally, and the last [`OUTPUT_TAIL_BYTES`] of its combined output.
#[derive(Debug)]
struct CommandFailure {
    exit: Option<i32>,
    tail: String,
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("seed command failed")
    }
}

impl Error for CommandFailure {}

/// The last 2 KiB of the combined stdout and stderr, lossily decoded and bounded.
fn output_tail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = Vec::with_capacity(stdout.len() + stderr.len());
    combined.extend_from_slice(stdout);
    combined.extend_from_slice(stderr);
    let start = combined.len().saturating_sub(OUTPUT_TAIL_BYTES);
    String::from_utf8_lossy(&combined[start..]).into_owned()
}

/// Report the inherited compiler-selection keys a seed build removes from its
/// child environment. A seed build is not a lane: wrappers and rustup overrides
/// come from whoever launched it, so the build clears them rather than refusing.
/// Lane use of a mismatched seed is still guarded by the eligibility check in
/// `target_seed.rs`. At most one line is printed per process, whatever the
/// number of projects a tick pass refreshes.
#[allow(clippy::print_stderr)] // The warning is the operator-facing output of the refresh.
fn ensure_host_seed_environment_eligible() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let environment = std::env::vars_os().collect::<Vec<_>>();
    let cleared = target_seed::seed_build_cleared_keys(&environment);
    if cleared.is_empty() {
        return;
    }
    WARNED.call_once(|| {
        eprintln!(
            "warning: seed-build cleared inherited compiler-selection environment: {}",
            cleared.join(", ")
        );
    });
}

fn cargo_config_ineligibility(reason: &str) -> io::Error {
    io::Error::other(format!(
        "shared Cargo target seed is unavailable because Cargo configuration cannot be represented ({reason})"
    ))
}

fn check_host_cargo_config_file(path: &Path) -> Result<()> {
    use std::io::Read;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(cargo_config_ineligibility("cargo_config_unreadable").into()),
    };
    if !metadata.is_file() {
        return Err(cargo_config_ineligibility("cargo_config_unreadable").into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(65537).read_to_end(&mut bytes))
        .map_err(|_| cargo_config_ineligibility("cargo_config_unreadable"))?;
    target_seed::cargo_config_seed_eligibility(&bytes)
        .map_err(|reason| cargo_config_ineligibility(reason).into())
}

fn check_host_cargo_configuration(state: &Path) -> Result<()> {
    let environment = std::env::vars_os().collect::<Vec<_>>();
    let home = environment
        .iter()
        .find(|(key, _)| key == "HOME")
        .map(|(_, value)| PathBuf::from(value));
    let explicit_cargo_home = environment
        .iter()
        .find(|(key, _)| key == "CARGO_HOME")
        .is_some_and(|(_, value)| !value.is_empty());
    if home.is_none() && !explicit_cargo_home {
        return Err(cargo_config_ineligibility("cargo_home_unresolved").into());
    }
    let home = home.unwrap_or_default();
    let cargo_home = provenant_confinement::cargo::CargoHome::resolve(&home, |name| {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    })
    .map_err(|_| cargo_config_ineligibility("cargo_home_unresolved"))?;
    let mut directories = std::collections::BTreeSet::new();
    directories.insert(cargo_home.path);
    for ancestor in target_seed::store_path(state).ancestors() {
        directories.insert(ancestor.join(".cargo"));
    }
    for directory in directories {
        for name in provenant_confinement::cargo::CONFIG_FILES {
            check_host_cargo_config_file(&directory.join(name))?;
        }
    }
    Ok(())
}

/// Why a project has no seed until its owner repairs it. The pass skips it and
/// records the reason; it is not a failure of the pass and never stops another
/// project's seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// No layer declares `verify.base` and the remote HEAD names none.
    VerifyBaseUnset,
    /// The declared or inferred verify base is not a ref this repository has.
    VerifyBaseUnresolved,
    /// The verify base has no Cargo manifest at the selected workspace.
    WorkspaceAbsentAtBase,
    /// A reason a newer writer recorded. It decodes so one unknown skip cannot
    /// make the whole result unreadable.
    #[serde(other)]
    Unrecognised,
}

impl SkipReason {
    pub fn cause(self) -> &'static str {
        match self {
            Self::VerifyBaseUnset => "verify.base is unset and cannot be inferred",
            Self::VerifyBaseUnresolved => "its verify base does not resolve to a commit",
            Self::WorkspaceAbsentAtBase => "its verify base has no Cargo manifest at the workspace",
            Self::Unrecognised => "of a reason this build does not recognise",
        }
    }

    pub fn repair(self) -> &'static str {
        match self {
            Self::VerifyBaseUnset => {
                "Set verify.base in .provenant/ports.json or the host ports.json, or fetch the remote so its HEAD resolves, then run `provenant worktree seed-build` in that repository."
            }
            Self::VerifyBaseUnresolved => {
                "Fetch the verify base or set verify.base to a remote-tracking ref the repository has, then run `provenant worktree seed-build` in that repository."
            }
            Self::WorkspaceAbsentAtBase => {
                "Set verify.workspace to a directory that holds a Cargo.toml at the verify base, or land the workspace on the base, then run `provenant worktree seed-build` in that repository."
            }
            Self::Unrecognised => {
                "Run `provenant worktree seed-build` in that repository to see the current cause."
            }
        }
    }
}

/// A repairable condition that makes one project unseedable. It travels as the
/// error of `refresh`, so a caller tells it from a failure by type, not text.
#[derive(Debug)]
pub struct SeedSkip {
    pub reason: SkipReason,
    detail: String,
}

impl SeedSkip {
    pub(crate) fn new(reason: SkipReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    fn unset() -> Self {
        Self::new(
            SkipReason::VerifyBaseUnset,
            provenant_config::verify_base_unset_error().to_string(),
        )
    }
}

impl std::fmt::Display for SeedSkip {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for SeedSkip {}

/// The skip an error carries, if any, anywhere in its source chain.
#[allow(dead_code)] // The harness-free worktree driver includes this module without the tick.
pub fn skip_of<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a SeedSkip> {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if let Some(skip) = error.downcast_ref::<SeedSkip>() {
            return Some(skip);
        }
        cause = error.source();
    }
    None
}

fn seed_error(stage: &str, error: &(dyn Error + 'static)) -> io::Error {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if let Some(ExecError::Io { operation, source }) = error.downcast_ref::<ExecError>() {
            return io::Error::other(format!("{stage}: utility {operation} failed: {source}"));
        }
        cause = error.source();
    }
    io::Error::other(format!("{stage}: {error}"))
}

#[cfg(test)]
#[test]
fn seed_spawn_failures_keep_the_os_cause() {
    let error = ExecError::Io {
        operation: "spawn",
        source: io::Error::from_raw_os_error(2),
    };
    let message = seed_error("opening repository", &error).to_string();
    assert!(message.contains("opening repository: utility spawn failed"));
    assert!(message.contains("os error 2"));
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = make_directories_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn make_directories_writable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
        )?;
        for entry in fs::read_dir(path)? {
            make_directories_writable(&entry?.path())?;
        }
    }
    Ok(())
}

/// The environment a seed-build child process sees. Inherited compiler-selection
/// keys are dropped, as are `GIT_*` keys and keys Provenant sets through
/// `overrides`; overrides are then applied last so the safe values win.
fn seed_build_child_environment(
    inherited: &[(OsString, OsString)],
    overrides: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let cleared = target_seed::seed_build_cleared_keys(inherited);
    let mut env: Vec<_> = inherited
        .iter()
        .filter(|(key, _)| {
            !key.to_string_lossy().starts_with("GIT_")
                && !overrides.iter().any(|(name, _)| name == key)
                && !cleared.iter().any(|name| OsStr::new(name) == key)
        })
        .cloned()
        .collect();
    env.extend_from_slice(overrides);
    env
}

/// The manifest flags that describe inherited build settings. A cleared key no
/// longer applies to the child process, so it must not be recorded as if it did.
fn seed_build_flags(environment: &[(OsString, OsString)]) -> Vec<String> {
    let cleared = target_seed::seed_build_cleared_keys(environment);
    [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
    ]
    .into_iter()
    .filter(|key| !cleared.iter().any(|name| name == key))
    .filter_map(|key| {
        let value = environment
            .iter()
            .find(|(name, _)| name.as_os_str() == OsStr::new(key))
            .and_then(|(_, value)| value.to_str())?;
        Some(format!("{key}={value}"))
    })
    .collect()
}

fn run(cwd: &Path, argv: &[OsString], overrides: &[(OsString, OsString)]) -> Result<Vec<u8>> {
    let inherited = std::env::vars_os().collect::<Vec<_>>();
    let env = seed_build_child_environment(&inherited, overrides);
    let mut options = ExecOptions::new(argv, cwd, Duration::from_secs(4 * 60 * 60));
    options.env = Some(&env);
    options.process_group = true;
    let executor = SystemExecutor::new(1)?;
    let result = executor
        .execute(options)
        .map_err(|error| seed_error("running seed command", &error))?;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.stdout.truncated
        || result.stdout.read_failed
    {
        return Err(io::Error::other(format!(
            "command failed: {argv:?}: {}",
            String::from_utf8_lossy(&result.stderr.bytes)
        ))
        .into());
    }
    Ok(result.stdout.bytes)
}

/// Withheld text when local redaction cannot filter captured output. Never pass
/// the raw stream on in that case: withhold the whole tail instead.
const WITHHELD_TAIL: &str = "[REDACTED]";

/// Like [`run`], but keeps the combined output tail when a command fails. Both
/// streams use a tail policy: a long prepare log cannot grow the error without
/// bound, and the retained bytes are enough to explain a failed generation step.
/// Local redaction masks each stream at capture, on the executor's
/// `CaptureFilter`, ahead of the Tail window: a secret that straddles the
/// window start is masked before the cut can leave part of it behind. A tail
/// stream's `truncated` flag is expected, so unlike `run` it is not a failure on
/// its own.
fn run_with_tail(
    cwd: &Path,
    argv: &[OsString],
    overrides: &[(OsString, OsString)],
) -> std::result::Result<Vec<u8>, CommandFailure> {
    let inherited = std::env::vars_os().collect::<Vec<_>>();
    let env = seed_build_child_environment(&inherited, overrides);
    let mut options = ExecOptions::new(argv, cwd, Duration::from_secs(4 * 60 * 60));
    options.env = Some(&env);
    options.process_group = true;
    options.stdout = provenant_os::exec::StreamPolicy::Tail {
        limit: OUTPUT_TAIL_BYTES,
    };
    options.stderr = provenant_os::exec::StreamPolicy::Tail {
        limit: OUTPUT_TAIL_BYTES,
    };
    let failed = |tail: String, exit: Option<i32>| CommandFailure { exit, tail };
    let mut stdout_filter = provenant_redact::Redactor::for_local_disclosure()
        .map_err(|_| failed(WITHHELD_TAIL.to_owned(), None))?;
    let mut stderr_filter = provenant_redact::Redactor::for_local_disclosure()
        .map_err(|_| failed(WITHHELD_TAIL.to_owned(), None))?;
    options.stdout_filter = Some(&mut stdout_filter);
    options.stderr_filter = Some(&mut stderr_filter);
    let executor = SystemExecutor::new(1)
        .map_err(|error| failed(seed_error("running seed command", &error).to_string(), None))?;
    let result = executor
        .execute(options)
        .map_err(|error| failed(seed_error("running seed command", &error).to_string(), None))?;
    let filter_failed = result.stdout.filter_failed || result.stderr.filter_failed;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.stdout.read_failed
        || result.stderr.read_failed
        || filter_failed
    {
        let exit = match result.termination {
            Termination::Exit(code) => i32::try_from(code).ok(),
            Termination::Signal(_) => None,
        };
        let tail = if filter_failed {
            WITHHELD_TAIL.to_owned()
        } else {
            output_tail(&result.stdout.bytes, &result.stderr.bytes)
        };
        return Err(failed(tail, exit));
    }
    Ok(result.stdout.bytes)
}

fn git(cwd: &Path, args: &[OsString]) -> Result<Vec<u8>> {
    let executable =
        provenant_os::git::git_executable().ok_or_else(|| io::Error::other("git unavailable"))?;
    let mut argv: Vec<OsString> = vec![
        executable.into_os_string(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
    ];
    argv.extend_from_slice(args);
    run(
        cwd,
        &argv,
        &[
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ],
    )
}

// A clone advertises local branches and tags, not remote-tracking refs.
// Fetch the pinned object explicitly before checkout.
fn checkout_commit(project: &Path, checkout: &Path, commit: &str) -> Result<()> {
    git(
        project,
        &[
            "clone".into(),
            "--no-local".into(),
            "--no-hardlinks".into(),
            "--no-checkout".into(),
            "--".into(),
            project.as_os_str().into(),
            checkout.as_os_str().into(),
        ],
    )?;
    git(
        checkout,
        &[
            "fetch".into(),
            "--no-tags".into(),
            "--".into(),
            project.as_os_str().into(),
            commit.into(),
        ],
    )?;
    git(
        checkout,
        &["checkout".into(), "--detach".into(), commit.into()],
    )?;
    Ok(())
}

/// The pinned remote-tracking commit every seed build starts from: the
/// project's declared verify base (`verify.base` in the port table), inferred
/// from the effective remote's HEAD when no layer declares it. A repository
/// with another remote or default branch needs no compiled `origin/main`.
struct VerifyBase {
    commit: String,
    project_key: String,
    workspace: PathBuf,
    toolchain_files: Vec<(PathBuf, Vec<u8>)>,
}

fn read_verify_base_file(
    repo: &provenant_confinement::ReadOnlyRepo,
    executor: &dyn Executor,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
    tree: GitOid,
    path: &Path,
) -> Result<Option<Vec<u8>>> {
    use provenant_confinement::utility::{GitObjectKind, run_read_utility};
    let Some(oid) = verify_base_file_blob(repo, executor, utility, budget, cancel, tree, path)?
    else {
        return Ok(None);
    };
    let blob = run_read_utility(
        executor,
        provenant_confinement::GitRead::RawObject {
            kind: GitObjectKind::Blob,
            oid,
        },
        repo,
        utility,
        budget,
        cancel,
    )
    .map_err(|error| seed_error("reading verify-base file", &error))?;
    if blob.termination != Termination::Exit(0)
        || blob.timed_out
        || blob.stdout.truncated
        || blob.stdout.read_failed
    {
        return Err(io::Error::other("reading verify-base file failed").into());
    }
    if blob.stdout.bytes.len() > 64 * 1024 {
        return Err(io::Error::other("verify-base file exceeds its size limit").into());
    }
    Ok(Some(blob.stdout.bytes))
}

fn verify_base_file_blob(
    repo: &provenant_confinement::ReadOnlyRepo,
    executor: &dyn Executor,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
    tree: GitOid,
    path: &Path,
) -> Result<Option<GitOid>> {
    use provenant_confinement::{repo::RepoPath, utility::run_read_utility};
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::other("verify-base path is not UTF-8"))?;
    let pathspec = RepoPath::new(path)?;
    let listing = run_read_utility(
        executor,
        provenant_confinement::GitRead::TreeEntries {
            oid: tree,
            prefix: pathspec,
        },
        repo,
        utility,
        budget,
        cancel,
    )
    .map_err(|error| seed_error("reading verify-base paths", &error))?;
    if listing.termination != Termination::Exit(0)
        || listing.timed_out
        || listing.stdout.truncated
        || listing.stdout.read_failed
    {
        return Err(io::Error::other("reading verify-base paths failed").into());
    }
    let mut entries = listing
        .stdout
        .bytes
        .split(|byte| *byte == 0)
        .filter(|line| !line.is_empty());
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    if entries.next().is_some() {
        return Err(io::Error::other("verify-base path is ambiguous").into());
    }
    let Some(separator) = entry.iter().position(|byte| *byte == b'\t') else {
        return Err(io::Error::other("verify-base entry is invalid").into());
    };
    let metadata = std::str::from_utf8(&entry[..separator])?;
    if &entry[separator + 1..] != path.as_bytes() {
        return Err(io::Error::other("verify-base path did not match").into());
    }
    let mut fields = metadata.split_ascii_whitespace();
    let mode = fields.next();
    let kind = fields.next();
    let oid = fields.next();
    if fields.next().is_some() || !matches!(mode, Some("100644" | "100755")) || kind != Some("blob")
    {
        return Err(io::Error::other("verify-base path is not a regular file").into());
    }
    Ok(Some(GitOid::parse(oid.ok_or_else(|| {
        io::Error::other("verify-base blob is missing")
    })?)?))
}

/// Resolve an unset `verify.base` from the effective remote's HEAD symref. The
/// read-only repository is the same one used for an explicit base, so the
/// inferred value goes through the identical validation and object reads.
fn infer_verify_base(
    remote: &str,
    repo: &provenant_confinement::ReadOnlyRepo,
    executor: &dyn Executor,
    utility: &provenant_confinement::UtilityRuntime,
    budget: provenant_confinement::UtilityBudget,
    cancel: &provenant_core::CancelToken,
) -> Result<String> {
    use provenant_confinement::{
        GitRead, repo::GitRefName, repo::GitRevision, utility::run_read_utility,
    };
    if remote.is_empty() {
        return Err(SeedSkip::unset().into());
    }
    let head = GitRefName::remote_tracking(format!("{remote}/HEAD"))?;
    let revision = GitRevision::new(head.as_str())?;
    let outcome = run_read_utility(
        executor,
        GitRead::SymbolicRevision { revision },
        repo,
        utility,
        budget,
        cancel,
    )
    .map_err(|error| seed_error("reading remote HEAD", &error))?;
    if outcome.termination != Termination::Exit(0)
        || outcome.timed_out
        || outcome.stdout.truncated
        || outcome.stdout.read_failed
    {
        return Err(SeedSkip::unset().into());
    }
    let target = String::from_utf8(outcome.stdout.bytes)
        .map_err(|error| seed_error("reading remote HEAD", &error))?;
    provenant_config::infer_verify_base(remote, target.trim())
        .map(|(base, _)| base)
        .ok_or_else(|| SeedSkip::unset().into())
}

fn verify_base(host_state: &Path, project: &Path, workspace: &Path) -> Result<VerifyBase> {
    use provenant_confinement::{
        repo::GitRefName,
        utility::{UtilityBudget, UtilityRuntime},
        worktrees::RepoFacts,
    };
    check_host_cargo_configuration(host_state)?;
    let executable =
        provenant_os::git::git_executable().ok_or_else(|| io::Error::other("git unavailable"))?;
    let utility = UtilityRuntime::read_only(executable, host_state)?;
    let executor = SystemExecutor::new(1)?;
    let budget = UtilityBudget::bounded(Duration::from_secs(4 * 60 * 60))?;
    let cancel = provenant_core::CancelToken::new();
    let repo = utility
        .open_read_repo(&executor, project, budget, &cancel)
        .map_err(|error| seed_error("opening repository", &error))?;
    let project_key = target_seed::project_key(repo.common_git_dir());
    let facts = RepoFacts::default();
    let verify = provenant_config::PortTable::read_for(Some(project), host_state)?.verify;
    let base = if verify.base.is_empty() {
        infer_verify_base(&verify.remote, &repo, &executor, &utility, budget, &cancel)?
    } else {
        verify.base
    };
    let reference = GitRefName::remote_tracking(&base)?;
    let commit = facts
        .resolve_read_commit(&repo, &executor, &utility, budget, reference, &cancel)
        .map_err(|error| -> Box<dyn Error> {
            match error {
                provenant_confinement::worktrees::WorktreeError::NotFound
                | provenant_confinement::worktrees::WorktreeError::BaseNotFound { .. } => {
                    SeedSkip::new(
                        SkipReason::VerifyBaseUnresolved,
                        format!(
                            "verify base {base} does not resolve to a commit in this repository"
                        ),
                    )
                    .into()
                }
                error => seed_error("resolving verify base", &error).into(),
            }
        })?;
    let tree = facts
        .resolve_read_tree(&repo, &executor, &utility, budget, commit, &cancel)
        .map_err(|error| seed_error("reading verify-base tree", &error))?;
    let cargo_manifest = workspace.join("Cargo.toml");
    if verify_base_file_blob(
        &repo,
        &executor,
        &utility,
        budget,
        &cancel,
        tree,
        &cargo_manifest,
    )?
    .is_none()
    {
        return Err(SeedSkip::new(
            SkipReason::WorkspaceAbsentAtBase,
            "verify base has no Cargo manifest at the selected workspace",
        )
        .into());
    }
    let mut toolchain_files = Vec::new();
    let mut directory = workspace.to_path_buf();
    let mut found_nearest_toolchain_pin = false;
    loop {
        for name in provenant_confinement::cargo::CONFIG_FILES {
            let path = directory.join(".cargo").join(name);
            if let Some(bytes) =
                read_verify_base_file(&repo, &executor, &utility, budget, &cancel, tree, &path)?
            {
                target_seed::cargo_config_seed_eligibility(&bytes)
                    .map_err(cargo_config_ineligibility)?;
            }
        }
        if !found_nearest_toolchain_pin {
            for name in ["rust-toolchain", "rust-toolchain.toml"] {
                let path = directory.join(name);
                if let Some(bytes) =
                    read_verify_base_file(&repo, &executor, &utility, budget, &cancel, tree, &path)?
                {
                    if bytes.len() > 16 * 1024 {
                        return Err(
                            io::Error::other("verify-base toolchain pin is too large").into()
                        );
                    }
                    found_nearest_toolchain_pin = true;
                    toolchain_files.push((path, bytes));
                }
            }
        }
        if !directory.pop() {
            break;
        }
    }
    Ok(VerifyBase {
        commit: commit.to_string(),
        project_key,
        workspace: workspace.to_path_buf(),
        toolchain_files,
    })
}

/// What a refresh did.
#[derive(Debug, PartialEq, Eq)]
pub enum Refresh {
    /// This project has no selected Cargo workspace; nothing to seed.
    NotCargo,
    /// A complete generation matches the verify-base dependency identity.
    Current,
    /// Another host seed build holds the build lock; nothing was started.
    Busy,
    /// The requested commit did not produce the requested identity.
    IdentityMismatch,
    /// A new generation was published.
    Built(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GenerationIdentity {
    project_key: String,
    commit: String,
    rustc_vv: String,
    host_triple: String,
    cargo_profile: CargoBuildProfile,
    hash: String,
}

fn workspace_relative(state: &Path, project: &Path) -> Result<Option<PathBuf>> {
    let Some(workspace) = target_seed::cargo_build_directory(project, state)? else {
        return Ok(None);
    };
    let project = project.canonicalize()?;
    let relative = workspace
        .strip_prefix(project)
        .map_err(|_| io::Error::other("Cargo workspace is outside the project"))?;
    Ok(Some(relative.to_path_buf()))
}

enum IdentityRefresh {
    NotCargo,
    Busy,
    Ready {
        identity: GenerationIdentity,
        workspace: PathBuf,
    },
}

fn is_lock_timeout(error: &(dyn Error + 'static)) -> bool {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if error
            .downcast_ref::<provenant_os::fs::FsError>()
            .is_some_and(|error| matches!(error, provenant_os::fs::FsError::LockTimeout { .. }))
        {
            return true;
        }
        cause = error.source();
    }
    false
}

/// Protect temporary seed-store staging from GC without holding the store lock
/// across repository or compiler work. All callers take Store before Build,
/// and Build acquisition is nonblocking while Store is held.
fn lock_for_identity_probe(state: &Path) -> Result<Option<provenant_os::fs::LockGuard>> {
    let store_lock = match target_seed::lock_store(state) {
        Ok(lock) => lock,
        Err(error) if is_lock_timeout(error.as_ref()) => return Ok(None),
        Err(error) => return Err(error),
    };
    let build_lock = match target_seed::lock_build(state) {
        Ok(lock) => lock,
        Err(error) if is_lock_timeout(error.as_ref()) => return Ok(None),
        Err(error) => return Err(error),
    };
    drop(store_lock);
    Ok(Some(build_lock))
}

fn refresh_identity(state: &Path, project: &Path) -> Result<IdentityRefresh> {
    refresh_identity_with_probe(state, project, |workspace| {
        target_seed::rustc_metadata(workspace)
            .map_err(|error| seed_error("probing verify-base compiler", &error).into())
    })
}

fn refresh_identity_with_probe(
    state: &Path,
    project: &Path,
    probe: impl FnOnce(&Path) -> Result<(String, String)>,
) -> Result<IdentityRefresh> {
    // The Build lock protects our temporary staging tree from GC. Release
    // Store before doing slow work so a producer can publish its finished seed.
    let Some(_build_lock) = lock_for_identity_probe(state)? else {
        return Ok(IdentityRefresh::Busy);
    };
    let Some(workspace) = workspace_relative(state, project)? else {
        return Ok(IdentityRefresh::NotCargo);
    };
    let base = verify_base(state, project, &workspace)?;
    let (rustc_vv, host_triple) = rustc_metadata_for_base(state, &base, probe)?;
    let cargo_profile = cargo_build_profile(state)?;
    let source = target_seed::GitSeedSource {
        checkout: project,
        commit: Some(&base.commit),
    };
    let hash =
        target_seed::seed_identity(&source, &workspace, &rustc_vv, &host_triple, &cargo_profile)?
            .hash;
    Ok(IdentityRefresh::Ready {
        identity: GenerationIdentity {
            project_key: base.project_key,
            commit: base.commit,
            rustc_vv,
            host_triple,
            cargo_profile,
            hash,
        },
        workspace,
    })
}

fn rustc_metadata_for_base(
    state: &Path,
    base: &VerifyBase,
    probe: impl FnOnce(&Path) -> Result<(String, String)>,
) -> Result<(String, String)> {
    let store = target_seed::store_path(state);
    let random = provenant_os::fs::temporary_name()?;
    let staging = Staging(store.join(format!(".staging-verify-base-{}", random.to_string_lossy())));
    fs::create_dir(&staging.0)?;
    let checkout = staging.0.join("checkout");
    fs::create_dir_all(&checkout)?;
    for (path, bytes) in &base.toolchain_files {
        let destination = checkout.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, bytes)?;
    }
    let workspace = checkout.join(&base.workspace);
    fs::create_dir_all(&workspace)?;
    probe(&workspace)
}

fn cargo_build_profile(state: &Path) -> Result<CargoBuildProfile> {
    let settings = LaneBuildSettings::read(state)?;
    let environment = std::env::vars_os().collect::<Vec<_>>();
    settings
        .cargo_build_profile(&environment)
        .ok_or_else(|| io::Error::other("Cargo profile environment is not UTF-8").into())
}

/// Record a refresh fact for part A's lane-start and doctor readers. A failed
/// write warns and is otherwise ignored: health is advisory and never changes
/// whether a build succeeded.
fn record_refresh_outcome(state: &Path, project_key: &str, outcome: &str, reason: Option<&str>) {
    let at = match provenant_os::clock::SystemClock.wall_now() {
        Ok(at) => at,
        Err(error) => {
            seed_build_stderr(format!(
                "warning: could not record seed refresh outcome: clock unavailable: {error}"
            ));
            return;
        }
    };
    if let Err(error) =
        provenant_run::target_seed_health::record_refresh(state, project_key, outcome, reason, at)
    {
        seed_build_stderr(format!(
            "warning: could not record seed refresh outcome for {outcome}: {error}"
        ));
    }
}

/// A short, bounded cause for a recorded refresh failure. A prepare failure's
/// output tail is already masked on the capture stream, so it is safe to store
/// and print as-is; skips name their typed reason and everything else keeps the
/// error text, which `record_refresh` caps at 2 KiB.
fn refresh_failure_reason(error: &(dyn Error + 'static)) -> String {
    if let Some(prepare) = prepare_failed_of(error) {
        format!("{}: {}", prepare.argv.join(" "), prepare.tail)
    } else if let Some(skip) = skip_of(error) {
        format!("{}: {}", skip.reason.cause(), skip)
    } else {
        error.to_string()
    }
}

/// Rebuild this repository's seed when its verify base has no complete
/// generation, at most one build at a time. Call after a fetch that may have
/// advanced the base; it does nothing when the seed is current or a build
/// is running, and never builds from a lane checkout (the build uses a scratch
/// clone of the pinned base commit).
// The detached seed worker (`internal tick-seed`) calls this once per project
// after the daily collection's fetch. The harness-free worktree driver
// includes this module without that caller.
#[allow(dead_code)]
pub fn refresh(state: &Path, project: &Path) -> Result<Refresh> {
    refuse_in_lane()?;
    ensure_host_seed_environment_eligible();
    let (identity, workspace) = match refresh_identity(state, project)? {
        IdentityRefresh::NotCargo => return Ok(Refresh::NotCargo),
        IdentityRefresh::Busy => return Ok(Refresh::Busy),
        IdentityRefresh::Ready {
            identity,
            workspace,
        } => (identity, workspace),
    };
    let project_key = identity.project_key.clone();
    let outcome = refresh_with(state, &identity, |stage| {
        build_into(
            state,
            stage,
            project,
            &workspace,
            &identity.commit,
            &identity.project_key,
            &identity.cargo_profile,
        )
    });
    match &outcome {
        Ok(Refresh::Built(_)) => record_refresh_outcome(state, &project_key, "built", None),
        Ok(Refresh::Current) => record_refresh_outcome(state, &project_key, "current", None),
        Ok(Refresh::Busy) => record_refresh_outcome(state, &project_key, "busy", None),
        Ok(Refresh::IdentityMismatch) => {
            record_refresh_outcome(state, &project_key, "identity_mismatch", None)
        }
        Ok(Refresh::NotCargo) => {}
        Err(error) => {
            let (recorded, reason) = if prepare_failed_of(error.as_ref()).is_some() {
                ("prepare_failed", refresh_failure_reason(error.as_ref()))
            } else if skip_of(error.as_ref()).is_some() {
                ("skipped", refresh_failure_reason(error.as_ref()))
            } else {
                ("failed", refresh_failure_reason(error.as_ref()))
            };
            record_refresh_outcome(state, &project_key, recorded, Some(&reason));
        }
    }
    outcome
}

/// Refresh exactly the commit named by a lane request, checking both its tree
/// and the prepared checkout before allowing publication.
pub fn refresh_requested(
    state: &Path,
    project: &Path,
    request: &target_seed::SeedRequest,
) -> Result<Refresh> {
    refuse_in_lane()?;
    ensure_host_seed_environment_eligible();
    let Some(_lock) = lock_for_identity_probe(state)? else {
        return Ok(Refresh::Busy);
    };
    let Some(workspace) = workspace_relative(state, project)? else {
        return Ok(Refresh::NotCargo);
    };
    let stage = Staging(target_seed::store_path(state).join(format!(
        ".staging-request-{}",
        provenant_os::fs::temporary_name()?.to_string_lossy()
    )));
    fs::create_dir(&stage.0)?;
    let checkout = stage.0.join("checkout");
    checkout_commit(project, &checkout, &request.commit)?;
    let (rustc_vv, host_triple) = target_seed::rustc_metadata(&checkout.join(&workspace))?;
    let cargo_profile = cargo_build_profile(state)?;
    let source = target_seed::GitSeedSource {
        checkout: project,
        commit: Some(&request.commit),
    };
    let hash =
        target_seed::seed_identity(&source, &workspace, &rustc_vv, &host_triple, &cargo_profile)?
            .hash;
    if hash != request.identity {
        record_refresh_outcome(state, &request.project_key, "identity_mismatch", None);
        return Ok(Refresh::IdentityMismatch);
    }
    let identity = GenerationIdentity {
        project_key: request.project_key.clone(),
        commit: request.commit.clone(),
        rustc_vv,
        host_triple,
        cargo_profile,
        hash,
    };
    drop(stage);
    drop(_lock);
    let result = refresh_with(state, &identity, |stage| {
        let manifest = build_into(
            state,
            stage,
            project,
            &workspace,
            &identity.commit,
            &identity.project_key,
            &identity.cargo_profile,
        )?;
        if manifest.identity.as_deref() != Some(identity.hash.as_str()) {
            return Err(IdentityMismatch.into());
        }
        Ok(manifest)
    });
    match result {
        Err(error) if error.downcast_ref::<IdentityMismatch>().is_some() => {
            record_refresh_outcome(state, &request.project_key, "identity_mismatch", None);
            Ok(Refresh::IdentityMismatch)
        }
        other => {
            match &other {
                Ok(Refresh::Built(_)) => {
                    record_refresh_outcome(state, &request.project_key, "built", None)
                }
                Ok(Refresh::Current) => {
                    record_refresh_outcome(state, &request.project_key, "current", None)
                }
                Ok(Refresh::Busy) => {
                    record_refresh_outcome(state, &request.project_key, "busy", None)
                }
                Err(error) => record_refresh_outcome(
                    state,
                    &request.project_key,
                    "failed",
                    Some(&refresh_failure_reason(error.as_ref())),
                ),
                _ => {}
            }
            other
        }
    }
}

#[derive(Debug)]
struct IdentityMismatch;
impl std::fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("requested seed identity changed")
    }
}
impl Error for IdentityMismatch {}

/// Raised under the build lock when another refresher published this commit
/// while we waited for it.
#[derive(Debug)]
struct AlreadyCurrent;
impl std::fmt::Display for AlreadyCurrent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("seed already current")
    }
}
impl Error for AlreadyCurrent {}

fn refresh_with(
    state: &Path,
    identity: &GenerationIdentity,
    build: impl FnOnce(&Path) -> Result<Manifest>,
) -> Result<Refresh> {
    refresh_racing(state, identity, || {}, build)
}

/// `between` runs after the unlocked staleness check and before the build
/// lock: the window another refresher can publish in.
fn refresh_racing(
    state: &Path,
    identity: &GenerationIdentity,
    between: impl FnOnce(),
    build: impl FnOnce(&Path) -> Result<Manifest>,
) -> Result<Refresh> {
    refresh_racing_with_publication_wait(state, identity, between, PUBLICATION_WAIT_SECONDS, build)
}

fn refresh_racing_with_publication_wait(
    state: &Path,
    identity: &GenerationIdentity,
    between: impl FnOnce(),
    publication_wait_seconds: u64,
    build: impl FnOnce(&Path) -> Result<Manifest>,
) -> Result<Refresh> {
    if has_generation(state, identity) {
        return Ok(Refresh::Current);
    }
    // Another refresher may publish this commit between the check above and
    // our taking the build lock, so look again once we hold it.
    between();
    let result = publish_with_publication_wait(state, publication_wait_seconds, |stage| {
        if has_generation(state, identity) {
            return Err(AlreadyCurrent.into());
        }
        build(stage)
    });
    match result {
        Ok(id) => Ok(Refresh::Built(id)),
        Err(error) if error.is::<AlreadyCurrent>() => Ok(Refresh::Current),
        Err(error) if is_lock_timeout(error.as_ref()) => Ok(Refresh::Busy),
        Err(error) => Err(error),
    }
}

/// Check every complete generation because a newer generation may belong to
/// another repository in the shared seed store. The identity includes the
/// compiler and effective Cargo profile.
fn has_generation(state: &Path, identity: &GenerationIdentity) -> bool {
    let generations = target_seed::store_path(state).join("generations");
    let Ok(entries) = fs::read_dir(generations) else {
        return false;
    };
    entries.filter_map(std::result::Result::ok).any(|entry| {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || entry.file_name().to_string_lossy().starts_with('.')
            || !fs::read(path.join("complete")).is_ok_and(|data| data == b"complete\n")
            || !entry
                .metadata()
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o222 == 0)
        {
            return false;
        }
        fs::read(path.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
            .is_some_and(|manifest| {
                // A full generation holds the checkout's own units and can go
                // stale against a lane's sources; it is never current.
                manifest.contents == "deps"
                    && manifest.project_key == identity.project_key
                    && manifest.identity.as_deref() == Some(&identity.hash)
            })
    })
}

fn refuse_in_lane() -> Result<()> {
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() {
        return Err(io::Error::other("target seeds can only be built on the host").into());
    }
    Ok(())
}

pub fn build(state: &Path, project: &Path) -> Result<String> {
    refuse_in_lane()?;
    ensure_host_seed_environment_eligible();
    let workspace = workspace_relative(state, project)?
        .ok_or_else(|| io::Error::other("project has no selected Cargo workspace"))?;
    let base = verify_base(state, project, &workspace)?;
    let cargo_profile = cargo_build_profile(state)?;
    let project_key = base.project_key.clone();
    let outcome = publish(state, |stage| {
        build_into(
            state,
            stage,
            project,
            &workspace,
            &base.commit,
            &base.project_key,
            &cargo_profile,
        )
    });
    match &outcome {
        Ok(_) => record_refresh_outcome(state, &project_key, "built", None),
        Err(error) => {
            let (recorded, reason) = if prepare_failed_of(error.as_ref()).is_some() {
                ("prepare_failed", refresh_failure_reason(error.as_ref()))
            } else if skip_of(error.as_ref()).is_some() {
                ("skipped", refresh_failure_reason(error.as_ref()))
            } else {
                ("failed", refresh_failure_reason(error.as_ref()))
            };
            record_refresh_outcome(state, &project_key, recorded, Some(&reason));
        }
    }
    outcome
}

/// Run every `build.prepare` argv in order from the checkout root, before any
/// Cargo command touches the tree. The port table is read from the live project
/// and the host state, not from the checkout: the repository declares the
/// commands, and the checkout supplies only the working directory for them. An
/// unreadable or invalid table warns and runs nothing; a failing command fails
/// that build with a typed error carrying its bounded output tail.
fn run_prepares(
    state: &Path,
    project: &Path,
    checkout_root: &Path,
    overrides: &[(OsString, OsString)],
) -> Result<()> {
    let table = match provenant_config::PortTable::read_for(Some(project), state) {
        Ok(table) => table,
        Err(error) => {
            seed_build_stderr(format!(
                "warning: seed-build build.prepare could not be read ({error}); no prepare command ran"
            ));
            return Ok(());
        }
    };
    let layer = provenant_config::port_source_layer(table.build.source);
    for (index, argv) in table.build.prepare.iter().enumerate() {
        seed_build_stderr(format!(
            "seed-build: build.prepare[{index}] (source {layer}): {}",
            argv.join(" ")
        ));
        let argv = argv.iter().map(OsString::from).collect::<Vec<_>>();
        run_with_tail(checkout_root, &argv, overrides).map_err(|failure| PrepareFailed {
            index,
            argv: argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            exit: failure.exit,
            tail: failure.tail,
        })?;
    }
    Ok(())
}

fn build_into(
    state: &Path,
    stage: &Path,
    project: &Path,
    workspace_relative: &Path,
    commit: &str,
    project_key: &str,
    cargo_profile: &CargoBuildProfile,
) -> Result<Manifest> {
    let checkout = stage.join("checkout");
    checkout_commit(project, &checkout, commit)?;
    let checkout_root = checkout.canonicalize()?;
    let target = stage.join("target");
    fs::create_dir(&target)?;
    let environment = std::env::vars_os().collect::<Vec<_>>();
    let cargo = target_seed::cargo_tool(&environment, "cargo")?;
    let mut flags = vec![
        "cargo build --locked --all-targets".to_owned(),
        "cargo test --locked --no-run".to_owned(),
        "cargo check --workspace --all-targets --locked".to_owned(),
    ];
    flags.extend(seed_build_flags(&environment));
    let mut overrides = cargo_profile.environment().to_vec();
    let mut paths = vec![
        cargo
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    overrides.push((
        "PATH".into(),
        std::env::join_paths(paths)
            .map_err(|error| io::Error::other(format!("cargo PATH unresolved: {error}")))?,
    ));
    overrides.extend([
        ("CARGO_TARGET_DIR".into(), target.as_os_str().into()),
        ("CARGO_BUILD_BUILD_DIR".into(), target.as_os_str().into()),
        // Provenant's safe value; an inherited non-zero value is cleared.
        ("RUSTUP_AUTO_INSTALL".into(), "0".into()),
    ]);
    run_prepares(state, project, &checkout_root, &overrides)?;
    let worktree = checkout.join(workspace_relative).canonicalize()?;
    if !worktree.starts_with(&checkout_root) || !worktree.join("Cargo.toml").metadata()?.is_file() {
        return Err(io::Error::other(
            "verify base has no Cargo manifest at the selected workspace",
        )
        .into());
    }
    let (rustc_vv, host_triple) = target_seed::rustc_metadata(&worktree)?;
    // Capture the checkout after preparation, so generated changes cannot be
    // published under the unchanged commit-tree identity.
    let source = target_seed::GitSeedSource {
        checkout: &checkout_root,
        commit: None,
    };
    let identity = target_seed::seed_identity(
        &source,
        workspace_relative,
        &rustc_vv,
        &host_triple,
        cargo_profile,
    )?;
    fs::write(
        stage.join("identity.json"),
        serde_json::to_vec_pretty(&identity)?,
    )?;
    make_read_only(&stage.join("identity.json"))?;
    for args in [
        &["build", "--locked", "--all-targets"][..],
        &["test", "--locked", "--no-run"][..],
        &["check", "--workspace", "--all-targets", "--locked"][..],
    ] {
        let argv: Vec<OsString> = std::iter::once(cargo.as_os_str().to_owned())
            .chain(args.iter().map(OsString::from))
            .collect();
        run(&worktree, &argv, &overrides)?;
    }
    // The checkout's own units are never reusable by a lane: their sources move
    // with the lane, and a stale artifact would be trusted by mtime. Clean every
    // path package, leaving only dependency units in the seed.
    clean_path_packages(&cargo, &worktree, &overrides)?;
    // Record the size once, while building the host seed is already walking the
    // tree; a lane clone must not walk a multi-gigabyte seed before starting.
    let allocated_bytes = target_seed::allocated_tree_bytes(&target);
    fs::remove_dir_all(checkout)?;
    Ok(Manifest {
        project_key: project_key.into(),
        rustc_vv,
        host_triple,
        commit: commit.to_owned(),
        identity: Some(identity.hash),
        profile: "dev".into(),
        cargo_profile: cargo_profile.clone(),
        features: vec!["default".into()],
        flags,
        allocated_bytes,
        contents: "deps".into(),
    })
}

/// Remove the units of every path package from a built seed target. `cargo
/// metadata` lists workspace members and path dependencies alike with a null
/// `source`; registry and other non-path dependencies are left untouched. The
/// enumeration and the clean use the same child environment and target overrides
/// as the build, through the process helper that applies `seed_build_child_environment`.
fn clean_path_packages(
    cargo: &Path,
    worktree: &Path,
    overrides: &[(OsString, OsString)],
) -> Result<()> {
    let metadata = [
        OsString::from("metadata"),
        "--format-version".into(),
        "1".into(),
        "--locked".into(),
    ];
    let argv: Vec<OsString> = std::iter::once(cargo.as_os_str().to_owned())
        .chain(metadata)
        .collect();
    let output = run(worktree, &argv, overrides)?;
    let metadata: serde_json::Value = serde_json::from_slice(&output)
        .map_err(|error| io::Error::other(format!("cargo metadata: {error}")))?;
    let packages = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| io::Error::other("cargo metadata has no package list"))?;
    let mut specs = Vec::new();
    for package in packages {
        if package.get("source").is_none_or(serde_json::Value::is_null)
            && let Some(id) = package.get("id").and_then(serde_json::Value::as_str)
        {
            specs.push(id.to_owned());
        }
    }
    for spec in specs {
        let argv: Vec<OsString> = vec![
            cargo.as_os_str().to_owned(),
            "clean".into(),
            "-p".into(),
            spec.into(),
        ];
        run(worktree, &argv, overrides)?;
    }
    Ok(())
}

fn publish(state: &Path, build: impl FnOnce(&Path) -> Result<Manifest>) -> Result<String> {
    publish_with_publication_wait(state, PUBLICATION_WAIT_SECONDS, build)
}

fn publish_with_publication_wait(
    state: &Path,
    publication_wait_seconds: u64,
    build: impl FnOnce(&Path) -> Result<Manifest>,
) -> Result<String> {
    let store_lock = target_seed::lock_store(state)?;
    let _build_lock = target_seed::lock_build(state)?;
    let store = target_seed::store_path(state);
    let random = provenant_os::fs::temporary_name()?;
    let stage = Staging(store.join(format!(".staging-{}", random.to_string_lossy())));
    fs::create_dir(&stage.0)?;
    drop(store_lock);
    let manifest = build(&stage.0)?;
    // Cargo tags a target directory only when it creates it; tag it here so a
    // generation and every seed cloned from it carry the tag.
    provenant_confinement::cache::tag_cache_directory(&provenant_os::fs::Directory::open_anchor(
        &stage.0.join("target"),
    )?)?;
    fs::write(
        stage.0.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    make_read_only(&stage.0.join("target"))
        .map_err(|error| io::Error::new(error.kind(), format!("freeze generation: {error}")))?;
    make_read_only(&stage.0.join("manifest.json"))?;
    sync_tree(&stage.0)?;
    let id = format!(
        "{:020}-{}",
        provenant_os::clock::SystemClock.wall_now()?.as_micros(),
        random.to_string_lossy()
    );
    let _lock = target_seed::lock_store_for_publication(state, publication_wait_seconds)?;
    fs::rename(&stage.0, store.join("generations").join(&id))
        .map_err(|error| io::Error::new(error.kind(), format!("publish generation: {error}")))?;
    let generation = store.join("generations").join(&id);
    let marker = generation.join("complete");
    fs::write(&marker, b"complete\n")?;
    make_read_only(&marker)?;
    provenant_os::fs::durable_sync_file(&fs::File::open(&marker)?)?;
    let mode = fs::metadata(&generation)?.permissions().mode();
    fs::set_permissions(&generation, fs::Permissions::from_mode(mode & !0o222))?;
    sync_directory(&generation)?;
    sync_directory(&store.join("generations"))?;
    sync_directory(&store)?;
    Ok(id)
}

// Never follow symlinks, which may point outside the staged tree.
fn sync_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            sync_tree(&entry?.path())?;
        }
        return sync_directory(path);
    }
    if !metadata.is_file() {
        return Err(io::Error::other("unsupported entry in target seed"));
    }
    provenant_os::fs::durable_sync_file(&fs::File::open(path)?)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn make_read_only(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            make_read_only(&entry?.path())?;
        }
    }
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(metadata.permissions().mode() & !0o222),
    )
}

#[cfg(test)]
mod checkout_tests {
    #[allow(unused_imports)]
    // This module is also included by a harness-free integration target.
    use super::*;

    #[test]
    fn verify_base_infers_a_non_main_default_branch_from_remote_head() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        let project = temp.path().join("project");
        fs::create_dir(&remote).unwrap();
        git(&remote, &["init".into(), "--initial-branch=master".into()]).unwrap();
        fs::write(
            remote.join("Cargo.toml"),
            "[package]\nname='seed-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            remote.join("rust-toolchain.toml"),
            include_str!("../rust-toolchain.toml"),
        )
        .unwrap();
        git(
            &remote,
            &[
                "add".into(),
                "Cargo.toml".into(),
                "rust-toolchain.toml".into(),
            ],
        )
        .unwrap();
        provenant_os::fixture::run_git(&remote, &["commit", "-m", "base"]).unwrap();
        git(
            temp.path(),
            &[
                "clone".into(),
                "--".into(),
                remote.as_os_str().into(),
                project.as_os_str().into(),
            ],
        )
        .unwrap();
        let expected = String::from_utf8(
            git(&project, &["rev-parse".into(), "origin/master".into()]).unwrap(),
        )
        .unwrap();
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let workspace = workspace_relative(&state, &project).unwrap().unwrap();
        let base = verify_base(&state, &project, &workspace).unwrap();
        assert_eq!(base.commit, expected.trim());
    }

    #[test]
    fn checkout_transfers_remote_tracking_commit_ahead_of_local_branch() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        let project = temp.path().join("project");
        let checkout = temp.path().join("checkout");
        fs::create_dir(&remote).unwrap();
        git(&remote, &["init".into(), "--initial-branch=main".into()]).unwrap();
        fs::write(
            remote.join("Cargo.toml"),
            "[package]\nname='seed-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            remote.join("rust-toolchain.toml"),
            include_str!("../rust-toolchain.toml"),
        )
        .unwrap();
        fs::write(remote.join("file"), "first").unwrap();
        git(
            &remote,
            &[
                "add".into(),
                "Cargo.toml".into(),
                "rust-toolchain.toml".into(),
                "file".into(),
            ],
        )
        .unwrap();
        let commit = |message: &str| {
            provenant_os::fixture::run_git(&remote, &["commit", "-am", message]).unwrap();
        };
        commit("first");
        git(
            temp.path(),
            &[
                "clone".into(),
                "--".into(),
                remote.as_os_str().into(),
                project.as_os_str().into(),
            ],
        )
        .unwrap();
        fs::write(remote.join("file"), "second").unwrap();
        commit("second");
        git(&project, &["fetch".into(), "origin".into()]).unwrap();
        let pinned =
            String::from_utf8(git(&project, &["rev-parse".into(), "origin/main".into()]).unwrap())
                .unwrap();
        let pinned = pinned.trim();
        let local = String::from_utf8(git(&project, &["rev-parse".into(), "main".into()]).unwrap())
            .unwrap();
        assert_ne!(pinned, local.trim());
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let workspace = workspace_relative(&state, &project).unwrap().unwrap();
        let primary = verify_base(&state, &project, &workspace).unwrap();
        assert_eq!(primary.commit, pinned);
        assert_eq!(
            primary.toolchain_files,
            vec![(
                PathBuf::from("rust-toolchain.toml"),
                include_str!("../rust-toolchain.toml").as_bytes().to_vec()
            )]
        );
        let linked = temp.path().join("linked");
        git(
            &project,
            &[
                "worktree".into(),
                "add".into(),
                "--detach".into(),
                linked.as_os_str().into(),
                pinned.into(),
            ],
        )
        .unwrap();
        let linked = verify_base(&state, &linked, &workspace).unwrap();
        assert_eq!(linked.commit, pinned);
        assert_eq!(linked.project_key, primary.project_key);
        checkout_commit(&project, &checkout, pinned).unwrap();
        assert_eq!(fs::read_to_string(checkout.join("file")).unwrap(), "second");
        let actual =
            String::from_utf8(git(&checkout, &["rev-parse".into(), "HEAD".into()]).unwrap())
                .unwrap();
        assert_eq!(actual.trim(), pinned);
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn verify_base_rejects_unrepresented_cargo_compiler_selection() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        let project = temp.path().join("project");
        fs::create_dir_all(remote.join(".cargo")).unwrap();
        git(&remote, &["init".into(), "--initial-branch=main".into()]).unwrap();
        fs::write(
            remote.join("Cargo.toml"),
            "[package]\nname='seed-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            remote.join(".cargo/config.toml"),
            "[build]\nrustc = '/private/compiler-path'\n",
        )
        .unwrap();
        git(
            &remote,
            &[
                "add".into(),
                "Cargo.toml".into(),
                ".cargo/config.toml".into(),
            ],
        )
        .unwrap();
        provenant_os::fixture::run_git(&remote, &["commit", "-m", "seed base"]).unwrap();
        git(
            temp.path(),
            &[
                "clone".into(),
                "--".into(),
                remote.as_os_str().into(),
                project.as_os_str().into(),
            ],
        )
        .unwrap();

        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let workspace = PathBuf::new();
        let error = verify_base(&state, &project, &workspace)
            .err()
            .expect("pinned Cargo config selects a compiler");
        let message = error.to_string();
        assert!(
            message.contains("cargo_config_selects_compiler"),
            "{message}"
        );
        assert!(!message.contains("/private/compiler-path"), "{message}");
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn currentness_uses_the_verify_base_toolchain_pin_not_the_live_checkout_pin() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        let project = temp.path().join("project");
        fs::create_dir(&remote).unwrap();
        git(&remote, &["init".into(), "--initial-branch=main".into()]).unwrap();
        fs::write(
            remote.join("Cargo.toml"),
            "[package]\nname='seed-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(remote.join("Cargo.lock"), "version = 3\n").unwrap();
        fs::write(
            remote.join("rust-toolchain.toml"),
            include_str!("../rust-toolchain.toml"),
        )
        .unwrap();
        git(
            &remote,
            &[
                "add".into(),
                "Cargo.toml".into(),
                "Cargo.lock".into(),
                "rust-toolchain.toml".into(),
            ],
        )
        .unwrap();
        provenant_os::fixture::run_git(&remote, &["commit", "-m", "verify base"]).unwrap();
        git(
            temp.path(),
            &[
                "clone".into(),
                "--".into(),
                remote.as_os_str().into(),
                project.as_os_str().into(),
            ],
        )
        .unwrap();
        fs::write(
            project.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = 'unavailable-seed-fixture-toolchain'\n",
        )
        .unwrap();
        git(&project, &["add".into(), "rust-toolchain.toml".into()]).unwrap();
        provenant_os::fixture::run_git(&project, &["commit", "-m", "live toolchain change"])
            .unwrap();

        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let result = refresh_identity_with_probe(&state, &project, |probe| {
            let pin = fs::read_to_string(probe.join("rust-toolchain.toml"))?;
            assert_eq!(pin, include_str!("../rust-toolchain.toml"));
            Ok((format!("version selected by {pin}"), "fixture-host".into()))
        })
        .unwrap();
        let IdentityRefresh::Ready {
            identity,
            workspace,
        } = result
        else {
            panic!("verify-base Cargo workspace should be refreshable")
        };
        let live_pin = fs::read_to_string(project.join("rust-toolchain.toml")).unwrap();
        assert!(live_pin.contains("unavailable-seed-fixture-toolchain"));
        assert_ne!(live_pin, include_str!("../rust-toolchain.toml"));
        assert_eq!(workspace, PathBuf::new());
        let base_commit =
            String::from_utf8(git(&remote, &["rev-parse".into(), "main".into()]).unwrap()).unwrap();
        assert_eq!(identity.commit, base_commit.trim());
        publish(&state, |stage| {
            fs::create_dir(stage.join("target"))?;
            Ok(Manifest {
                project_key: identity.project_key.clone(),
                rustc_vv: identity.rustc_vv.clone(),
                host_triple: identity.host_triple.clone(),
                commit: identity.commit.clone(),
                identity: Some(identity.hash.clone()),
                profile: "dev".into(),
                cargo_profile: identity.cargo_profile.clone(),
                features: vec![],
                flags: vec![],
                allocated_bytes: None,
                contents: "deps".into(),
            })
        })
        .unwrap();
        assert_eq!(
            refresh_with(&state, &identity, |_| {
                panic!("the verify-base seed is current despite the live checkout pin")
            })
            .unwrap(),
            Refresh::Current
        );
    }

    #[test]
    fn store_lock_contention_is_busy_before_workspace_or_compiler_probe() {
        let state = tempfile::tempdir().unwrap();
        let project = state.path().join("not-a-repository");
        let _lock = target_seed::lock_store(state.path()).unwrap();
        let mut probed = false;
        let result = refresh_identity_with_probe(state.path(), &project, |_| {
            probed = true;
            Ok(("unexpected rustc invocation".into(), "host".into()))
        })
        .unwrap();
        assert!(matches!(result, IdentityRefresh::Busy));
        assert!(!probed, "compiler probe must not run while store is locked");
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    // This module is also included by a harness-free integration target.
    use super::*;
    #[test]
    fn failure_never_publishes() {
        let state = tempfile::tempdir().unwrap();
        assert!(
            publish(state.path(), |_| Err(
                io::Error::other("build failed").into()
            ))
            .is_err()
        );
        assert_eq!(
            fs::read_dir(target_seed::store_path(state.path()).join("generations"))
                .unwrap()
                .count(),
            0
        );
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn seed_build_clears_inherited_compiler_selection_env() {
        let profile = LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let inherited: Vec<(OsString, OsString)> = [
            ("RUSTC_WRAPPER", "/secret/wrap"),
            ("RUSTUP_TOOLCHAIN", "nightly"),
            ("RUSTFLAGS", "-C secretflag"),
            ("RUSTUP_AUTO_INSTALL", "1"),
            ("GIT_CONFIG_GLOBAL", "/secret/gitconfig"),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .chain(
            [
                OsString::from("CARGO_INCREMENTAL"),
                OsString::from("CARGO_PROFILE_DEV_DEBUG"),
            ]
            .into_iter()
            .map(|key| (key, OsString::from("profile-value"))),
        )
        .collect();
        let mut overrides = profile.environment().to_vec();
        overrides.extend([
            ("CARGO_TARGET_DIR".into(), OsString::from("/seed/target")),
            ("RUSTUP_AUTO_INSTALL".into(), OsString::from("0")),
        ]);

        let child = seed_build_child_environment(&inherited, &overrides);
        for key in [
            "RUSTC_WRAPPER",
            "RUSTUP_TOOLCHAIN",
            "RUSTFLAGS",
            "GIT_CONFIG_GLOBAL",
        ] {
            assert!(
                !child.iter().any(|(name, _)| name == key),
                "{key} must not reach the child"
            );
        }
        assert_eq!(
            child
                .iter()
                .filter(|(name, _)| name == "RUSTUP_AUTO_INSTALL")
                .count(),
            1
        );
        // The inherited auto-install value is replaced by the safe one, and
        // profile/target values Provenant sets survive exactly once.
        assert_eq!(
            child
                .iter()
                .filter(|(name, value)| name == "RUSTUP_AUTO_INSTALL" && value == "0")
                .count(),
            1
        );
        for key in [
            "CARGO_INCREMENTAL",
            "CARGO_PROFILE_DEV_DEBUG",
            "CARGO_TARGET_DIR",
        ] {
            assert_eq!(
                child.iter().filter(|(name, _)| name == key).count(),
                1,
                "{key} is set by Provenant and must be kept"
            );
        }
        // The cleared environment leaves no unsupported override, so the build
        // proceeds: the seed is no longer refused for the launcher's shell.
        assert_eq!(
            target_seed::unsupported_seed_build_environment(&child),
            None
        );

        // Flags record only settings that still apply to the child.
        let flags = seed_build_flags(&inherited);
        assert!(
            !flags.iter().any(|flag| flag.contains("RUSTFLAGS")),
            "a cleared key must not be recorded: {flags:?}"
        );
        assert_eq!(flags, Vec::<String>::new());
    }

    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn identity(project_key: &str, commit: &str) -> GenerationIdentity {
        GenerationIdentity {
            project_key: project_key.into(),
            commit: commit.into(),
            hash: "identity".into(),
            rustc_vv: "version".into(),
            host_triple: "host".into(),
            cargo_profile: LaneBuildSettings::default()
                .cargo_build_profile(&[])
                .expect("default Cargo profile is UTF-8"),
        }
    }
    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn manifest(identity: &GenerationIdentity) -> Manifest {
        Manifest {
            project_key: identity.project_key.clone(),
            rustc_vv: identity.rustc_vv.clone(),
            host_triple: identity.host_triple.clone(),
            commit: identity.commit.clone(),
            identity: Some(identity.hash.clone()),
            profile: "dev".into(),
            cargo_profile: identity.cargo_profile.clone(),
            features: vec![],
            flags: vec![],
            allocated_bytes: None,
            contents: "deps".into(),
        }
    }
    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn built(stage: &Path, identity: &GenerationIdentity) -> Result<Manifest> {
        fs::create_dir(stage.join("target"))?;
        Ok(manifest(identity))
    }
    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn freeze(state: &Path) {
        let generations = target_seed::store_path(state).join("generations");
        for entry in fs::read_dir(generations).unwrap() {
            make_directories_writable(&entry.unwrap().path()).unwrap();
        }
    }

    #[allow(dead_code)] // This source module is also included by a test target that does not run seed-builder tests.
    fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let target = destination.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else if entry.file_type()?.is_file() {
                fs::copy(entry.path(), target)?;
            } else {
                return Err(io::Error::other("unexpected entry in Cargo target fixture"));
            }
        }
        Ok(())
    }

    #[allow(dead_code)] // This source module is also included by a test target that does not run seed-builder tests.
    fn run_cargo_output(
        cwd: &Path,
        argv: &[OsString],
        overrides: &[(OsString, OsString)],
    ) -> Result<String> {
        let inherited = std::env::vars_os().collect::<Vec<_>>();
        let env = seed_build_child_environment(&inherited, overrides);
        let mut options = ExecOptions::new(argv, cwd, Duration::from_secs(4 * 60 * 60));
        options.env = Some(&env);
        options.process_group = true;
        let result = SystemExecutor::new(1)?.execute(options)?;
        if result.termination != Termination::Exit(0)
            || result.timed_out
            || result.stdout.truncated
            || result.stdout.read_failed
            || result.stderr.truncated
            || result.stderr.read_failed
        {
            return Err(io::Error::other("Cargo fixture command failed").into());
        }
        let mut output = result.stdout.bytes;
        output.extend(result.stderr.bytes);
        String::from_utf8(output).map_err(Into::into)
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn matching_seed_profile_reuses_a_dependency_as_fresh() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let dependency = temp.path().join("dependency");
        fs::create_dir_all(app.join("src")).unwrap();
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = 'seeded-dependency'\nversion = '0.1.0'\nedition = '2021'\n",
        )
        .unwrap();
        fs::write(
            dependency.join("src/lib.rs"),
            "pub fn value() -> u8 { 42 }\n",
        )
        .unwrap();
        fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = 'seed-probe-app'\nversion = '0.1.0'\nedition = '2021'\n\n[dependencies]\nseeded-dependency = { path = '../dependency' }\n",
        )
        .unwrap();
        fs::write(
            app.join("src/main.rs"),
            "fn main() { println!(\"{}\", seeded_dependency::value()); }\n",
        )
        .unwrap();

        let profile = LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let mut profile_env = profile.environment().to_vec();
        profile_env.push(("CARGO_NET_OFFLINE".into(), "true".into()));
        let generate_lock = [
            OsString::from("cargo"),
            "generate-lockfile".into(),
            "--offline".into(),
        ];
        run(&app, &generate_lock, &profile_env).unwrap();

        let seed_target = temp.path().join("seed-target");
        let mut seed_env = profile_env.clone();
        seed_env.extend([
            ("CARGO_TARGET_DIR".into(), seed_target.as_os_str().into()),
            (
                "CARGO_BUILD_BUILD_DIR".into(),
                seed_target.as_os_str().into(),
            ),
        ]);
        let build_seed = [
            OsString::from("cargo"),
            "build".into(),
            "--locked".into(),
            "--all-targets".into(),
            "--offline".into(),
        ];
        run(&app, &build_seed, &seed_env).unwrap();

        let writer_target = temp.path().join("writer-target");
        copy_tree(&seed_target, &writer_target).unwrap();
        let mut writer_env = profile_env;
        writer_env.extend([
            ("CARGO_TARGET_DIR".into(), writer_target.as_os_str().into()),
            (
                "CARGO_BUILD_BUILD_DIR".into(),
                writer_target.as_os_str().into(),
            ),
        ]);
        let build_writer = [
            OsString::from("cargo"),
            "build".into(),
            "--locked".into(),
            "--verbose".into(),
            "--offline".into(),
        ];
        let output = run_cargo_output(&app, &build_writer, &writer_env).unwrap();
        assert!(
            output
                .lines()
                .any(|line| line.contains("Fresh seeded-dependency")),
            "expected the writer target to reuse the seeded dependency, got:\n{output}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::disallowed_methods)] // Exercises real Cargo freshness.
    fn clippy_reuses_seeded_check_units() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let write = |path: &str, text: &str| {
            let path = app.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "rust-toolchain.toml",
            include_str!("../rust-toolchain.toml"),
        );
        write(
            "Cargo.toml",
            "[package]\nname='app'\nversion='0.1.0'\nedition='2021'\n[dependencies]\nvendored='1'\n",
        );
        write(
            "src/lib.rs",
            "pub fn value() -> u32 { vendored::value() }\n",
        );
        write(
            ".cargo/config.toml",
            "[source.crates-io]\nreplace-with='vendor'\n[source.vendor]\ndirectory='vendor'\n",
        );
        write(
            "vendor/vendored/Cargo.toml",
            "[package]\nname='vendored'\nversion='1.0.0'\nedition='2021'\n",
        );
        write(
            "vendor/vendored/src/lib.rs",
            "pub fn value() -> u32 { 1 }\n",
        );
        write(
            "vendor/vendored/.cargo-checksum.json",
            &format!("{{\"files\":{{}},\"package\":\"{}\"}}", "0".repeat(64)),
        );
        let seed = temp.path().join("seed");
        let run_cargo = |target: &Path, args: &[&str]| {
            let output = std::process::Command::new(
                std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
            )
            .args(args)
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_BUILD_BUILD_DIR", target)
            .env("CARGO_INCREMENTAL", "0")
            .output()
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        run_cargo(&seed, &["generate-lockfile", "--offline"]);
        run_cargo(
            &seed,
            &[
                "check",
                "--workspace",
                "--all-targets",
                "--locked",
                "--offline",
            ],
        );
        run_cargo(&seed, &["clean", "-p", "app", "--offline"]);
        let lint = temp.path().join("target-lint");
        copy_tree(&seed, &lint).unwrap();
        let output = run_cargo(
            &lint,
            &[
                "check",
                "--workspace",
                "--all-targets",
                "--locked",
                "--offline",
                "--message-format=json",
            ],
        );
        let messages: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let dependency: Vec<_> = messages
            .iter()
            .filter(|message| {
                message["reason"] == "compiler-artifact" && message["target"]["name"] == "vendored"
            })
            .collect();
        assert!(
            !dependency.is_empty(),
            "missing dependency artifacts: {messages:?}"
        );
        assert!(
            dependency.iter().all(|message| message["fresh"] == true),
            "{dependency:?}"
        );
    }

    /// A deps-only seed holds no units of the checkout, so a lane must rebuild
    /// its own members even when their sources are older than the seed's
    /// artifacts, while dependency units stay fresh. The fixture uses real
    /// Cargo with a build script, a member-to-member dependency and a non-member
    /// path package, so every local unit kind is covered.
    /// Spec: docs/specs/run.md#cargo-target-seed
    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::disallowed_methods)] // Runs the pinned Cargo and reads the real file clock.
    fn older_sources_rebuild_after_deps_only_seed() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let write = |path: &str, text: &str| {
            let path = workspace.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "rust-toolchain.toml",
            include_str!("../rust-toolchain.toml"),
        );
        write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"member\", \"inner\"]\nexclude = [\"dep-path\"]\nresolver = \"2\"\n",
        );
        write(
            "member/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ninner = { path = \"../inner\" }\ndep-path = { path = \"../dep-path\" }\nvendored = \"1\"\n",
        );
        write(
            "member/src/lib.rs",
            "pub fn value() -> u32 { inner::value() + dep_path::value() + vendored::value() }\n",
        );
        write(
            "inner/Cargo.toml",
            "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n\n[dependencies]\nvendored = \"1\"\n",
        );
        write(
            "inner/build.rs",
            "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }\n",
        );
        write(
            "inner/src/lib.rs",
            "pub fn value() -> u32 { vendored::value() }\n",
        );
        write(
            "dep-path/Cargo.toml",
            "[package]\nname = \"dep-path\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nvendored = \"1\"\n",
        );
        write(
            "dep-path/src/lib.rs",
            "pub fn value() -> u32 { vendored::value() }\n",
        );
        // A directory source stands in for a registry offline: its units are
        // dependency units and must stay fresh in the deps-only seed.
        write(
            ".cargo/config.toml",
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"vendor\"\n",
        );
        write(
            "vendor/vendored/Cargo.toml",
            "[package]\nname = \"vendored\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        write(
            "vendor/vendored/src/lib.rs",
            "pub fn value() -> u32 { 1 }\n",
        );
        write(
            "vendor/vendored/.cargo-checksum.json",
            &format!("{{\"files\":{{}},\"package\":\"{}\"}}", "0".repeat(64)),
        );

        let profile = LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let mut base_env = profile.environment().to_vec();
        base_env.push(("CARGO_NET_OFFLINE".into(), "true".into()));
        let generate_lock = [
            OsString::from("cargo"),
            "generate-lockfile".into(),
            "--offline".into(),
        ];
        run(&workspace, &generate_lock, &base_env).unwrap();

        let seed_target = temp.path().join("seed-target");
        let mut seed_env = base_env.clone();
        seed_env.extend([
            ("CARGO_TARGET_DIR".into(), seed_target.as_os_str().into()),
            (
                "CARGO_BUILD_BUILD_DIR".into(),
                seed_target.as_os_str().into(),
            ),
        ]);
        let build_seed = [
            OsString::from("cargo"),
            "build".into(),
            "--locked".into(),
            "--all-targets".into(),
            "--offline".into(),
        ];
        run(&workspace, &build_seed, &seed_env).unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();
        let cargo = target_seed::cargo_tool(&environment, "cargo").unwrap();
        clean_path_packages(&cargo, &workspace, &seed_env).unwrap();

        // Clone the seed as a lane target, then backdate the checkout's own
        // sources so mtime alone would call their units fresh.
        let lane_target = temp.path().join("lane-target");
        copy_tree(&seed_target, &lane_target).unwrap();
        let older = fs::metadata(workspace.join("member/src/lib.rs"))
            .unwrap()
            .modified()
            .unwrap()
            - Duration::from_secs(3600);
        for path in [
            "member/Cargo.toml",
            "member/src/lib.rs",
            "inner/Cargo.toml",
            "inner/build.rs",
            "inner/src/lib.rs",
            "dep-path/Cargo.toml",
            "dep-path/src/lib.rs",
        ] {
            fs::File::open(workspace.join(path))
                .unwrap()
                .set_modified(older)
                .unwrap();
        }

        let mut lane_env = base_env;
        lane_env.extend([
            ("CARGO_TARGET_DIR".into(), lane_target.as_os_str().into()),
            (
                "CARGO_BUILD_BUILD_DIR".into(),
                lane_target.as_os_str().into(),
            ),
        ]);
        let build_lane = [
            OsString::from("cargo"),
            "build".into(),
            "--locked".into(),
            "--offline".into(),
            "--message-format=json".into(),
        ];
        let output = run_cargo_output(&workspace, &build_lane, &lane_env).unwrap();
        let mut freshness = std::collections::BTreeMap::new();
        for line in output.lines() {
            let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if message["reason"] == "compiler-artifact" {
                let name = message["target"]["name"].as_str().unwrap().to_owned();
                freshness.insert(name, message["fresh"].as_bool().unwrap());
            }
        }
        for name in ["member", "inner", "dep_path", "build-script-build"] {
            assert_eq!(freshness.get(name), Some(&false), "{name}: {freshness:?}");
        }
        assert_eq!(freshness.get("vendored"), Some(&true), "{freshness:?}");
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn refresh_checks_all_generations_for_this_repository() {
        let state = tempfile::tempdir().unwrap();
        let builds = std::cell::Cell::new(0);
        let run = |head: &str| {
            let identity = identity("repo-a", head);
            refresh_with(state.path(), &identity, |stage| {
                builds.set(builds.get() + 1);
                built(stage, &identity)
            })
            .unwrap()
        };
        assert!(matches!(run("aaa"), Refresh::Built(_)));
        assert_eq!(run("aaa"), Refresh::Current);
        assert_eq!(builds.get(), 1, "a current seed is not rebuilt");
        // An incomplete generation cannot establish currentness.
        let generations = target_seed::store_path(state.path()).join("generations");
        fs::create_dir(generations.join("99999999999999999999-partial")).unwrap();
        assert!(has_generation(state.path(), &identity("repo-a", "aaa")));
        assert_eq!(run("bbb"), Refresh::Current);
        assert_eq!(
            builds.get(),
            1,
            "a different commit with matching inputs is current"
        );
        let mut changed = identity("repo-a", "bbb");
        changed.hash = "different".into();
        assert!(!has_generation(state.path(), &changed));
        freeze(state.path());
    }

    /// A full generation predates the deps-only contract. Even when every other
    /// identity field matches, it is not current: refresh must build a
    /// replacement that holds dependency units only.
    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn current_full_generation_triggers_replacement() {
        let state = tempfile::tempdir().unwrap();
        let identity = identity("repo", "aaa");
        publish(state.path(), |stage| {
            fs::create_dir(stage.join("target"))?;
            let mut full = manifest(&identity);
            full.contents.clear();
            Ok(full)
        })
        .unwrap();
        assert!(!has_generation(state.path(), &identity));

        let builds = std::cell::Cell::new(0);
        assert!(matches!(
            refresh_with(state.path(), &identity, |stage| {
                builds.set(builds.get() + 1);
                built(stage, &identity)
            })
            .unwrap(),
            Refresh::Built(_)
        ));
        assert_eq!(builds.get(), 1);
        assert!(has_generation(state.path(), &identity));
        assert_eq!(
            refresh_with(state.path(), &identity, |_| {
                panic!("the replacement deps generation is current")
            })
            .unwrap(),
            Refresh::Current
        );
        freeze(state.path());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn refresh_rebuilds_the_same_commit_when_profile_or_toolchain_changes() {
        let state = tempfile::tempdir().unwrap();
        let builds = std::cell::Cell::new(0);
        let run = |identity: &GenerationIdentity| {
            refresh_with(state.path(), identity, |stage| {
                builds.set(builds.get() + 1);
                built(stage, identity)
            })
            .unwrap()
        };
        let baseline = identity("repo", "same-commit");
        assert!(matches!(run(&baseline), Refresh::Built(_)));
        assert_eq!(run(&baseline), Refresh::Current);

        let mut profile_changed = baseline.clone();
        profile_changed.cargo_profile.debug = "full".into();
        profile_changed.hash = "profile".into();
        assert!(matches!(run(&profile_changed), Refresh::Built(_)));
        assert_eq!(run(&profile_changed), Refresh::Current);

        let mut compiler_changed = profile_changed.clone();
        compiler_changed.rustc_vv = "new version".into();
        compiler_changed.hash = "compiler".into();
        assert!(matches!(run(&compiler_changed), Refresh::Built(_)));
        assert_eq!(run(&compiler_changed), Refresh::Current);

        let mut triple_changed = compiler_changed.clone();
        triple_changed.host_triple = "new host".into();
        triple_changed.hash = "triple".into();
        assert!(matches!(run(&triple_changed), Refresh::Built(_)));
        assert_eq!(builds.get(), 4);
        freeze(state.path());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn a_newer_generation_from_another_repository_does_not_rebuild_this_repository() {
        let state = tempfile::tempdir().unwrap();
        let common_a = state.path().join("repo-a/.git");
        let common_b = state.path().join("repo-b/.git");
        fs::create_dir_all(&common_a).unwrap();
        fs::create_dir_all(&common_b).unwrap();
        let key_a = target_seed::project_key(&common_a.canonicalize().unwrap());
        let key_b = target_seed::project_key(&common_b.canonicalize().unwrap());
        assert_ne!(key_a, key_b);
        let alias_a = state.path().join("repo-a-alias");
        std::os::unix::fs::symlink(&common_a, &alias_a).unwrap();
        assert_eq!(
            target_seed::project_key(&alias_a.canonicalize().unwrap()),
            key_a
        );
        for (project_key, commit) in [(&key_a, "aaa"), (&key_b, "bbb")] {
            let identity = identity(project_key, commit);
            assert!(matches!(
                refresh_with(state.path(), &identity, |stage| built(stage, &identity)).unwrap(),
                Refresh::Built(_)
            ));
        }
        let identity_a = identity(&key_a, "aaa");
        assert_eq!(
            refresh_with(state.path(), &identity_a, |_| {
                panic!("repo A already has a complete matching generation")
            })
            .unwrap(),
            Refresh::Current
        );
        let identity_b = identity(&key_b, "bbb");
        assert_eq!(
            refresh_with(state.path(), &identity_b, |_| {
                panic!("repo B already has a complete matching generation")
            })
            .unwrap(),
            Refresh::Current
        );
        freeze(state.path());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    #[allow(clippy::disallowed_methods)] // A real deadline bounds the fork-race retry.
    fn refresh_retries_failed_commits_and_serialises_builds() {
        let state = tempfile::tempdir().unwrap();
        {
            // Another host build holds the build lock: no second build starts.
            target_seed::lock_store(state.path()).map(drop).unwrap();
            let _running = target_seed::lock_build(state.path()).unwrap();
            let identity = identity("repo", "aaa");
            let outcome = refresh_with(state.path(), &identity, |_| panic!("must not build"));
            assert_eq!(outcome.unwrap(), Refresh::Busy);
        }
        let identity = identity("repo", "aaa");
        // A sibling test thread can fork while the lock above is released and
        // briefly hold a copy of its descriptor, so retry for at most 2 s.
        let retry_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let failed = loop {
            let attempt = refresh_with(state.path(), &identity, |_| {
                Err(io::Error::other("build failed").into())
            });
            if !matches!(attempt, Ok(Refresh::Busy)) || std::time::Instant::now() >= retry_deadline
            {
                break attempt;
            }
            std::thread::yield_now();
        };
        assert!(failed.is_err(), "{failed:?}");
        assert!(!has_generation(state.path(), &identity));
        // The next scheduled refresh retries the same commit after an
        // infrastructure failure has been repaired.
        assert!(matches!(
            refresh_with(state.path(), &identity, |stage| built(stage, &identity)).unwrap(),
            Refresh::Built(_)
        ));
        freeze(state.path());
    }

    #[test]
    fn active_seed_producer_wins_over_refresh_probe_and_publishes() {
        let state = tempfile::tempdir().unwrap();
        let state_path = state.path().to_path_buf();
        let identity = identity("repo", "aaa");
        let producer_identity = identity.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            publish(&state_path, |stage| {
                fs::create_dir(stage.join("target"))?;
                started_tx.send(()).map_err(io::Error::other)?;
                release_rx
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(io::Error::other)?;
                Ok(manifest(&producer_identity))
            })
            .map_err(|error| error.to_string())
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("producer should hold the build lock in its stage");

        let invalid_project = state.path().join("not-a-repository");
        let mut probed = false;
        let outcome = refresh_identity_with_probe(state.path(), &invalid_project, |_| {
            probed = true;
            Ok(("unexpected probe".into(), "host".into()))
        })
        .unwrap();
        assert!(matches!(outcome, IdentityRefresh::Busy));
        assert!(
            !probed,
            "a refresh must not probe under the producer's build lock"
        );

        release_tx.send(()).unwrap();
        let generation_id = producer
            .join()
            .expect("producer thread should not panic")
            .expect("finished producer should publish its completed target");
        let generation = target_seed::store_path(state.path())
            .join("generations")
            .join(generation_id);
        assert_eq!(
            fs::read(generation.join("complete")).unwrap(),
            b"complete\n"
        );
        make_directories_writable(&generation).unwrap();
    }

    #[test]
    fn publication_waits_for_a_reader_store_lock_to_release() {
        let state = tempfile::tempdir().unwrap();
        let state_path = state.path().to_path_buf();
        let reader = target_seed::lock_store(state.path()).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let publisher = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result =
                target_seed::lock_store_for_publication(&state_path, PUBLICATION_WAIT_SECONDS)
                    .map(drop)
                    .map_err(|error| error.to_string());
            finished_tx.send(result).unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("publisher should attempt the bounded store acquisition");
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(reader);
        finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("publisher should continue after the reader releases Store")
            .expect("bounded publication lock should succeed");
        publisher.join().unwrap();
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn publication_lock_timeout_is_a_busy_refresh_and_cleans_completed_stage() {
        let state = tempfile::tempdir().unwrap();
        let state_path = state.path().to_path_buf();
        let (start_lock_tx, start_lock_rx) = std::sync::mpsc::channel();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let collector = std::thread::spawn(move || {
            start_lock_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the build callback starts the publication contention");
            let _lock = target_seed::lock_store(&state_path).unwrap();
            locked_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("test releases the store lock after publication returns");
        });

        let identity = identity("repo", "aaa");
        let build_entered = std::cell::Cell::new(false);
        let outcome = refresh_racing_with_publication_wait(
            state.path(),
            &identity,
            || {},
            1,
            |stage| {
                fs::create_dir(stage.join("target"))?;
                fs::write(stage.join("target/artifact"), b"built")?;
                assert!(stage.join("target/artifact").is_file());
                build_entered.set(true);
                start_lock_tx.send(()).map_err(io::Error::other)?;
                locked_rx
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(io::Error::other)?;
                Ok(manifest(&identity))
            },
        );
        release_tx.send(()).unwrap();
        collector.join().unwrap();
        assert!(
            build_entered.get(),
            "publication contention follows build entry"
        );
        assert_eq!(outcome.unwrap(), Refresh::Busy);

        let store = target_seed::store_path(state.path());
        assert_eq!(
            fs::read_dir(store.join("generations")).unwrap().count(),
            0,
            "a contended publication is retried rather than partially published"
        );
        assert!(
            fs::read_dir(store)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".staging-"))
        );
    }

    #[test]
    fn refresh_rechecks_staleness_after_taking_the_build_lock() {
        let state = tempfile::tempdir().unwrap();
        let identity = identity("repo", "aaa");
        let outcome = refresh_racing(
            state.path(),
            &identity,
            || {
                publish(state.path(), |stage| built(stage, &identity)).unwrap();
            },
            |_| panic!("the racing publisher already built this commit"),
        )
        .unwrap();
        assert_eq!(outcome, Refresh::Current);
        freeze(state.path());
    }

    #[test]
    fn success_publishes_read_only_generation() {
        let state = tempfile::tempdir().unwrap();
        let id = publish(state.path(), |stage| {
            fs::create_dir(stage.join("target"))?;
            fs::write(stage.join("target/artifact"), b"built")?;
            Ok(Manifest {
                project_key: "repo".into(),
                rustc_vv: "version".into(),
                host_triple: "host".into(),
                commit: "commit".into(),
                identity: None,
                profile: "dev".into(),
                cargo_profile: LaneBuildSettings::default()
                    .cargo_build_profile(&[])
                    .expect("default Cargo profile is UTF-8"),
                features: vec![],
                flags: vec![],
                allocated_bytes: None,
                contents: "deps".into(),
            })
        })
        .unwrap();
        let generation = target_seed::store_path(state.path())
            .join("generations")
            .join(id);
        for path in [
            generation.clone(),
            generation.join("manifest.json"),
            generation.join("target/artifact"),
        ] {
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o222, 0);
        }
        assert_eq!(
            fs::read(generation.join("target/CACHEDIR.TAG")).unwrap(),
            provenant_confinement::cache::CACHE_DIRECTORY_TAG_CONTENT
        );
        make_directories_writable(&generation).unwrap();
    }

    /// A Cargo project whose build succeeds only once a prepare command has
    /// written a gitignored `src/generated.rs` into the checkout root. The
    /// prepare script records its working directory so the test can see which
    /// directory it ran in after the disposable checkout is removed.
    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn prepared_project(temp: &Path) -> (PathBuf, String, PathBuf) {
        let project = temp.join("project");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='prepare-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::write(
            project.join("src/main.rs"),
            "mod generated;\nfn main() { generated::value(); }\n",
        )
        .unwrap();
        fs::write(project.join(".gitignore"), "src/generated.rs\n").unwrap();
        run(
            &project,
            &[
                OsString::from("cargo"),
                "generate-lockfile".into(),
                "--offline".into(),
            ],
            &[],
        )
        .unwrap();
        git(&project, &["init".into(), "--initial-branch=main".into()]).unwrap();
        git(
            &project,
            &[
                "add".into(),
                "Cargo.toml".into(),
                "Cargo.lock".into(),
                "src/main.rs".into(),
                ".gitignore".into(),
            ],
        )
        .unwrap();
        provenant_os::fixture::run_git(&project, &["commit", "-m", "base"]).unwrap();
        let commit =
            String::from_utf8(git(&project, &["rev-parse".into(), "HEAD".into()]).unwrap())
                .unwrap()
                .trim()
                .to_owned();
        let marker = temp.join("prepare-cwd");
        let script = temp.join("generate.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'pub fn value() {{}}\\n' > src/generated.rs\nprintf '%s' \"$PWD\" > {}\nprintf '%s\\n' \"$CARGO_TARGET_DIR\" \"$CARGO_BUILD_BUILD_DIR\" \"$RUSTUP_AUTO_INSTALL\" \"$CARGO_INCREMENTAL\" \"$CARGO_PROFILE_DEV_DEBUG\" \"$PATH\" > {}.env\n",
                marker.display(), marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (project, commit, marker)
    }

    #[allow(dead_code)] // The harness-free target includes this module without its tests.
    fn write_prepare(project: &Path, commands: Vec<Vec<String>>) {
        fs::create_dir_all(project.join(".provenant")).unwrap();
        fs::write(
            project.join(".provenant/ports.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "build": { "prepare": commands },
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn request_builds_its_commit() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state).unwrap();
        let (project, commit, _) = prepared_project(temp.path());
        write_prepare(
            &project,
            vec![vec![
                temp.path()
                    .join("generate.sh")
                    .to_string_lossy()
                    .into_owned(),
            ]],
        );
        fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
        provenant_os::fixture::run_git(&project, &["commit", "-am", "later"]).unwrap();
        let (rustc, triple) = target_seed::rustc_metadata(&project).unwrap();
        let profile = cargo_build_profile(&state).unwrap();
        let source = target_seed::GitSeedSource {
            checkout: &project,
            commit: Some(&commit),
        };
        let hash = target_seed::seed_identity(&source, Path::new(""), &rustc, &triple, &profile)
            .unwrap()
            .hash;
        let request = target_seed::SeedRequest {
            schema: 1,
            project_key: "abcd".into(),
            commit: commit.clone(),
            identity: hash,
            requested_at: provenant_core::WallTime::from_micros(1).unwrap(),
            attempt: 0,
        };
        let mut mismatched = request.clone();
        mismatched.identity = "different".into();
        assert_eq!(
            refresh_requested(&state, &project, &mismatched).unwrap(),
            Refresh::IdentityMismatch
        );
        assert!(
            fs::read_dir(target_seed::store_path(&state).join("generations"))
                .unwrap()
                .next()
                .is_none()
        );
        let Refresh::Built(id) = refresh_requested(&state, &project, &request).unwrap() else {
            panic!("requested commit must be built")
        };
        let bytes = fs::read(
            target_seed::store_path(&state)
                .join("generations")
                .join(id)
                .join("manifest.json"),
        )
        .unwrap();
        let manifest: Manifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(manifest.commit, commit);
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn prepare_runs_before_seed_build_in_checkout_root() {
        let temp = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (project, commit, marker) = prepared_project(temp.path());
        write_prepare(
            &project,
            vec![vec![
                temp.path()
                    .join("generate.sh")
                    .to_string_lossy()
                    .into_owned(),
            ]],
        );
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let identity = identity("prepare-key", &commit);
        let manifest = build_into(
            state.path(),
            &stage,
            &project,
            Path::new("."),
            &commit,
            "prepare-key",
            &identity.cargo_profile,
        )
        .unwrap();
        let observed = fs::read_to_string(marker.with_extension("env")).unwrap();
        let lines: Vec<_> = observed.lines().collect();
        assert_eq!(lines[0], stage.join("target").to_string_lossy());
        assert_eq!(lines[1], lines[0]);
        assert_eq!(lines[2], "0");
        for (index, (_, value)) in identity.cargo_profile.environment().iter().enumerate() {
            assert_eq!(lines[index + 3], value.to_string_lossy());
        }
        let cargo =
            target_seed::cargo_tool(&std::env::vars_os().collect::<Vec<_>>(), "cargo").unwrap();
        assert_eq!(
            std::env::split_paths(lines[5]).next().unwrap(),
            cargo.parent().unwrap()
        );
        assert!(
            manifest
                .flags
                .iter()
                .any(|flag| flag == "cargo check --workspace --all-targets --locked")
        );
        assert_eq!(
            fs::read_to_string(&marker).unwrap(),
            stage
                .canonicalize()
                .unwrap()
                .join("checkout")
                .to_string_lossy(),
            "prepare ran in the checkout root, not the workspace subdirectory"
        );
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn failing_prepare_fails_the_build_and_records_prepare_failed_with_bounded_tail() {
        let temp = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (project, commit, _) = prepared_project(temp.path());
        let script = temp.path().join("fail.sh");
        fs::write(
            &script,
            "#!/bin/sh\nhead -c 8192 /dev/zero | tr '\\0' 'x' >&2\nexit 7\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        write_prepare(&project, vec![vec![script.to_string_lossy().into_owned()]]);
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let identity = identity("prepare-key", &commit);
        let error = build_into(
            state.path(),
            &stage,
            &project,
            Path::new("."),
            &commit,
            "prepare-key",
            &identity.cargo_profile,
        )
        .unwrap_err();
        let prepare = prepare_failed_of(error.as_ref()).expect("typed prepare failure");
        assert_eq!(prepare.index, 0);
        assert_eq!(prepare.exit, Some(7));
        assert!(prepare.tail.len() <= OUTPUT_TAIL_BYTES);
        assert!(prepare.tail.contains("xxx"));
        // The target directory exists before prepare (prepare shares the build
        // environment that points at it), but cargo must not have written to it.
        assert!(
            fs::read_dir(stage.join("target")).is_ok_and(|mut entries| entries.next().is_none()),
            "cargo never runs after a failed prepare command"
        );
    }

    /// Spec: docs/specs/run.md#cargo-target-seed, docs/specs/cli.md#results
    #[test]
    fn failing_prepare_cause_is_masked_in_json_under_full_redaction() {
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Full);
        let secret = "seed-build-credential-3391";
        let _scope = provenant_redact::publication_scope([secret.as_bytes().to_vec()]).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (project, commit, _) = prepared_project(temp.path());
        let script = temp.path().join("fail.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"npm ERR! registry rejected {secret} (Cannot find package 'zod')\" >&2\necho 'install the missing dependency' >&2\nexit 7\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        write_prepare(&project, vec![vec![script.to_string_lossy().into_owned()]]);
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let identity = identity("prepare-key", &commit);
        let error = build_into(
            state.path(),
            &stage,
            &project,
            Path::new("."),
            &commit,
            "prepare-key",
            &identity.cargo_profile,
        )
        .unwrap_err();

        // The recorded refresh reason carries the tail only after masking.
        let reason = refresh_failure_reason(error.as_ref());
        assert!(reason.contains("install the missing dependency"), "{reason}");
        assert!(!reason.contains(secret), "{reason}");
        assert!(reason.contains("[REDACTED]"), "{reason}");

        // A manual `seed-build --json` drives the real `fail_with_cause`
        // emission path and renders the same masked tail as error.cause, after
        // the message and before the one typed next.
        let failure = crate::worktrees::seed_build_failure(error.as_ref());
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        crate::fail_with_cause_with_writers(
            "worktree seed-build",
            failure.outcome(),
            "WORKTREE",
            &failure.local_message(),
            failure.cause(),
            failure.repair(),
            &globals,
            &mut stdout,
            &mut stderr,
        );
        let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        let cause = value["error"]["cause"].as_str().expect("error.cause");
        assert!(cause.contains("Cannot find package 'zod'"), "{cause}");
        assert!(cause.contains("install the missing dependency"), "{cause}");
        assert!(!cause.contains(secret), "{cause}");
        assert!(cause.contains("[REDACTED]"), "{cause}");
        let message = value["error"]["message"].as_str().expect("error.message");
        assert!(message.contains("build.prepare[0]"), "{message}");
        assert!(!message.contains(secret), "{message}");
    }

    /// Spec: docs/specs/run.md#cargo-target-seed, docs/specs/cli.md#results
    #[test]
    fn failing_prepare_secret_straddling_the_window_start_is_masked_before_the_cut() {
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Full);
        let secret = "seed-build-credential-4472";
        let _scope = provenant_redact::publication_scope([secret.as_bytes().to_vec()]).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (project, commit, _) = prepared_project(temp.path());
        // The secret starts inside the raw 2 KiB window and extends past its
        // start: cutting first would leave a long, still-recognisable suffix.
        // Masking on the stream before the Tail window must hide the whole value.
        let prefix = "A".repeat(100);
        let suffix = format!("{}\nFATAL-MARKER\n", "B".repeat(2020));
        let prefix_path = temp.path().join("prefix.bin");
        let suffix_path = temp.path().join("suffix.bin");
        fs::write(&prefix_path, &prefix).unwrap();
        fs::write(&suffix_path, &suffix).unwrap();
        let script = temp.path().join("fail.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat '{}' >&2\nprintf '%s' '{}' >&2\ncat '{}' >&2\nexit 7\n",
                prefix_path.display(),
                secret,
                suffix_path.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        write_prepare(&project, vec![vec![script.to_string_lossy().into_owned()]]);
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let identity = identity("prepare-key", &commit);
        let error = build_into(
            state.path(),
            &stage,
            &project,
            Path::new("."),
            &commit,
            "prepare-key",
            &identity.cargo_profile,
        )
        .unwrap_err();

        let prepare = prepare_failed_of(error.as_ref()).expect("prepare failure");
        let tail = &prepare.tail;
        assert!(tail.len() <= OUTPUT_TAIL_BYTES, "{tail}");
        // The failure site at the end of the log survives the window.
        assert!(tail.contains("FATAL-MARKER"), "{tail}");
        // No slice long enough to be a known exact value leaks from the cut.
        for start in 0..=secret.len() - provenant_redact::MIN_EXACT_BYTES {
            assert!(
                !tail.contains(&secret[start..start + provenant_redact::MIN_EXACT_BYTES]),
                "secret suffix leaked: {tail}"
            );
        }
        assert!(tail.contains("[REDACTED]"), "{tail}");
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn empty_prepare_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (project, _, marker) = prepared_project(temp.path());
        // Commit the generated module so this project builds with no prepare
        // step; the marker script stays on disk but is not referenced.
        fs::write(project.join("src/generated.rs"), "pub fn value() {}\n").unwrap();
        git(
            &project,
            &["add".into(), "-f".into(), "src/generated.rs".into()],
        )
        .unwrap();
        provenant_os::fixture::run_git(&project, &["commit", "-m", "generated"]).unwrap();
        let commit =
            String::from_utf8(git(&project, &["rev-parse".into(), "HEAD".into()]).unwrap())
                .unwrap()
                .trim()
                .to_owned();
        write_prepare(&project, Vec::new());
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let identity = identity("prepare-key", &commit);
        build_into(
            state.path(),
            &stage,
            &project,
            Path::new("."),
            &commit,
            "prepare-key",
            &identity.cargo_profile,
        )
        .unwrap();
        assert!(!marker.exists(), "an empty prepare runs no command");
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn refresh_records_outcome_for_health() {
        let state = tempfile::tempdir().unwrap();
        let key = "project-key";
        record_refresh_outcome(state.path(), key, "built", None);
        assert_eq!(
            provenant_run::target_seed_health::read_refresh(state.path(), key)
                .unwrap()
                .outcome,
            "built"
        );
        record_refresh_outcome(state.path(), key, "failed", Some("cargo failed"));
        let failed = provenant_run::target_seed_health::read_refresh(state.path(), key).unwrap();
        assert_eq!(failed.outcome, "failed");
        assert_eq!(failed.reason.as_deref(), Some("cargo failed"));
        record_refresh_outcome(
            state.path(),
            key,
            "prepare_failed",
            Some("node gen.mjs: output tail"),
        );
        let prepare = provenant_run::target_seed_health::read_refresh(state.path(), key).unwrap();
        assert_eq!(prepare.outcome, "prepare_failed");
        assert_eq!(prepare.reason.as_deref(), Some("node gen.mjs: output tail"));
    }
}
