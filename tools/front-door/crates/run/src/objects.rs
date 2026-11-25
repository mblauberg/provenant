//! Import lane objects through Git's quarantined receive path.
use crate::{AdmissionError, LaneTipRefusal, Result, RunError};
use provenant_config::{HistoryEffect, LaneHistory, REBASE, Rebase, UNROOTED_TIP};
use provenant_confinement::{
    repo::VerifiedRepo,
    utility::{UtilityError, execute_with_git_lock_retry},
};
use provenant_core::{AttemptId, CancelToken, Clock, GitOid};
use provenant_os::{
    exec::{ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{Directory, FileKind, PublishMode},
};
use std::{
    ffi::{OsStr, OsString},
    io::Read,
    path::Path,
    time::Duration,
};

mod view;
pub(crate) mod worktree;
#[cfg(test)]
use view::object_binding;
pub(crate) use view::{
    IndexSync, NATIVE_INDEX_REMEDY, ObjectSource, bound_source, merge_objects, prepare_bound,
    prepare_read_only, private_tip, sync_native_index, update_private_tip,
};
#[cfg(test)]
pub(crate) use view::{bind, followed_native_index, prepare_refs, seed_native_index};

fn invalid() -> RunError {
    RunError::Validation("lane Git import failed")
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(Into::into).collect()
}

pub(crate) fn command(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
    _: Option<&Path>,
) -> Result<Vec<u8>> {
    execute(git, git_dir, cwd, arguments, None)
}

fn execute(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
    cancel: Option<&CancelToken>,
) -> Result<Vec<u8>> {
    execute_bounded(git, git_dir, cwd, arguments, cancel, Execution::default())
}

pub(crate) struct Execution<'a> {
    pub(crate) object_directory: Option<&'a Path>,
    pub(crate) index_file: Option<&'a Path>,
    pub(crate) alternate_directory: Option<&'a Path>,
    pub(crate) common_directory: Option<&'a Path>,
    pub(crate) timeout: Duration,
    pub(crate) discard: bool,
    pub(crate) input: Option<&'a [u8]>,
    pub(crate) clean_stderr: bool,
}

pub(crate) struct ImportScope<'a> {
    pub(crate) attempt: AttemptId,
    pub(crate) custody: Option<&'a provenant_supervisor::custody::Recovery>,
}

impl Default for Execution<'_> {
    fn default() -> Self {
        Self {
            object_directory: None,
            index_file: None,
            alternate_directory: None,
            common_directory: None,
            timeout: Duration::from_secs(120),
            discard: false,
            input: None,
            clean_stderr: false,
        }
    }
}

pub(crate) fn execute_bounded(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
    cancel: Option<&CancelToken>,
    settings: Execution<'_>,
) -> Result<Vec<u8>> {
    let retry_cancel = cancel.cloned().unwrap_or_default();
    let mut incomplete = None;
    let mut run_attempt = |timeout| {
        run_operation(
            git,
            git_dir,
            cwd,
            arguments,
            &retry_cancel,
            &settings,
            timeout,
        )
        .map_err(|failure| {
            let (error, output) = *failure;
            incomplete = output;
            error
        })
    };
    let buffered_transaction = settings.input.is_some()
        && arguments.len() >= 2
        && arguments[0] == OsStr::new("update-ref")
        && arguments[1] == OsStr::new("--stdin")
        && settings.input.is_some_and(is_atomic_ref_transaction);
    let retry_safe = settings.input.is_none() || buffered_transaction;
    let result = if retry_safe {
        execute_with_git_lock_retry(
            "lane Git import",
            settings.timeout,
            &retry_cancel,
            &mut run_attempt,
        )
    } else {
        run_attempt(settings.timeout)
    };
    let output = result.map_err(|error| match error {
        UtilityError::Cancelled => RunError::Validation("lane import cancelled"),
        UtilityError::TimedOut | UtilityError::GitLockTimedOut { .. } => {
            RunError::ObjectImportLimit("Git operation exceeded its byte-scaled time budget")
        }
        UtilityError::Incomplete => {
            let output = incomplete.take();
            if output.as_ref().is_some_and(|output| {
                String::from_utf8_lossy(&output.stderr.bytes)
                    .contains("pack exceeds maximum allowed size")
            }) {
                RunError::ObjectImportLimit("incoming pack exceeds the lane object byte limit")
            } else {
                operation_io(git_dir, output)
            }
        }
        _ => invalid(),
    })?;
    if output.termination != Termination::Exit(0)
        || output.drain_truncated
        || output.stdout.truncated
        || output.stdout.read_failed
        || output.stderr.read_failed
        || settings.clean_stderr
            && (output.stderr.truncated || !only_git_advice(&output.stderr.bytes))
    {
        if String::from_utf8_lossy(&output.stderr.bytes)
            .contains("pack exceeds maximum allowed size")
        {
            return Err(RunError::ObjectImportLimit(
                "incoming pack exceeds the lane object byte limit",
            ));
        }
        return Err(operation_io(git_dir, Some(output)));
    }
    let non_advice_stderr = strip_git_advice(&output.stderr.bytes);
    if !non_advice_stderr.is_empty() {
        use provenant_records::warning::{LANE_GIT_IMPORT_STDERR, Warning, warn};
        // Spec: docs/specs/cli.md#warnings-are-typed-values
        warn(Warning::new(
            LANE_GIT_IMPORT_STDERR,
            format!(
                "lane Git import: {}",
                String::from_utf8_lossy(&non_advice_stderr).trim_end()
            ),
        ));
    }
    Ok(output.stdout.bytes)
}

/// One bounded Git execution without applying a non-zero exit as an error, so
/// a caller that reads a plumbing command's exit status can see its output.
fn run_operation(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
    cancel: &CancelToken,
    settings: &Execution<'_>,
    timeout: Duration,
) -> std::result::Result<ExecOutcome, Box<(UtilityError, Option<ExecOutcome>)>> {
    let mut env: Vec<(OsString, OsString)> = [
        ("PATH", "/usr/bin:/bin"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_GRAFT_FILE", "/dev/null"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    if let Some(objects) = settings.object_directory {
        env.push((
            "GIT_OBJECT_DIRECTORY".into(),
            objects.as_os_str().to_owned(),
        ));
    }
    if let Some(index) = settings.index_file {
        env.push(("GIT_INDEX_FILE".into(), index.as_os_str().to_owned()));
    }
    env.push((
        "GIT_ALTERNATE_OBJECT_DIRECTORIES".into(),
        settings
            .alternate_directory
            .map(quote_alternate)
            .unwrap_or_default()
            .into(),
    ));
    if let Some(common_dir) = settings.common_directory {
        env.push(("GIT_COMMON_DIR".into(), common_dir.as_os_str().to_owned()));
    }
    // argv confines GIT_DIR to the sender; receive-pack opens the destination.
    let argv: Vec<OsString> = std::iter::once(git.into())
        .chain([OsString::from("--git-dir"), git_dir.into()])
        .chain(args(&[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "advice.graftFileDeprecated=false",
        ]))
        .chain(arguments.iter().cloned())
        .collect();
    let executor =
        SystemExecutor::new(1).map_err(|error| Box::new((UtilityError::from(error), None)))?;
    let mut options = ExecOptions::new(&argv, cwd, timeout);
    options.env = Some(&env);
    options.cancel = Some(cancel);
    options.process_group = true;
    options.stdout = if settings.discard {
        StreamPolicy::Discard
    } else {
        StreamPolicy::Capture {
            limit: 64 * 1024 * 1024,
        }
    };
    options.stdin = settings.input;
    options.stderr = StreamPolicy::Tail { limit: 4096 };
    let output = executor
        .execute(options)
        .map_err(|error| Box::new((UtilityError::from(error), None)))?;
    if output.cancelled {
        return Err(Box::new((UtilityError::Cancelled, Some(output))));
    }
    if output.timed_out {
        return Err(Box::new((UtilityError::TimedOut, Some(output))));
    }
    if output.drain_truncated
        || output.stdout.truncated
        || output.stdout.read_failed
        || output.stderr.read_failed
    {
        return Err(Box::new((UtilityError::Incomplete, Some(output))));
    }
    Ok(output)
}

/// Run a plumbing command whose non-zero exit carries information, such as
/// `merge-tree` reporting a conflict on exit 1.
fn raw_outcome(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
) -> Result<ExecOutcome> {
    let settings = Execution::default();
    let cancel = CancelToken::default();
    run_operation(
        git,
        git_dir,
        cwd,
        arguments,
        &cancel,
        &settings,
        settings.timeout,
    )
    .map_err(|failure| match *failure {
        (UtilityError::Incomplete, output) => operation_io(git_dir, output),
        _ => invalid(),
    })
}

fn operation_io(git_dir: &Path, output: Option<ExecOutcome>) -> RunError {
    RunError::OperationIo {
        operation: "import lane Git objects",
        path: Some(git_dir.into()),
        source: std::io::Error::other(
            output
                .map(|output| String::from_utf8_lossy(&output.stderr.bytes).into_owned())
                .unwrap_or_else(|| "Git capture was incomplete".to_owned()),
        ),
    }
}

fn is_atomic_ref_transaction(input: &[u8]) -> bool {
    let commands = input
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    commands.len() >= 4
        && commands.first().copied() == Some(&b"start"[..])
        && commands.get(commands.len() - 2).copied() == Some(&b"prepare"[..])
        && commands.last().copied() == Some(&b"commit"[..])
        && commands[1..commands.len() - 2]
            .iter()
            .all(|line| line.starts_with(b"delete refs/provenant/import-extra/"))
}

// `cat-file --batch` can exit zero for a corrupt object, reporting it as
// missing on stdout and an error on stderr. Its output is deliberately streamed
// to discard, so retain the stderr integrity check except for Git's known advice.
pub(crate) fn only_git_advice(stderr: &[u8]) -> bool {
    let Ok(stderr) = std::str::from_utf8(stderr) else {
        return false;
    };
    stderr.lines().all(|line| {
        let trimmed = line.trim();
        trimmed.is_empty() || trimmed.starts_with("hint:")
    })
}

pub(crate) fn strip_git_advice(stderr: &[u8]) -> Vec<u8> {
    let Ok(s) = std::str::from_utf8(stderr) else {
        return stderr.to_vec();
    };
    let mut kept = Vec::new();
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("hint:") {
            continue;
        }
        kept.push(line);
    }
    let filtered = kept.join("\n");
    if filtered.trim().is_empty() {
        Vec::new()
    } else {
        filtered.into_bytes()
    }
}

pub(crate) fn quote_alternate(path: &Path) -> String {
    let mut value = String::from("\"");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'"' | b'\\' => {
                value.push('\\');
                value.push(char::from(*byte));
            }
            0x20..=0x7e => value.push(char::from(*byte)),
            _ => value.push_str(&format!("\\{byte:03o}")),
        }
    }
    value.push('"');
    value
}

pub(crate) fn resolve_ref(git: &Path, repo: &VerifiedRepo, reference: &str) -> Result<GitOid> {
    let bytes = command(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["rev-parse", "--verify", "--end-of-options", reference]),
        None,
    )?;
    GitOid::parse(std::str::from_utf8(&bytes).map_err(|_| invalid())?.trim()).map_err(|_| invalid())
}

/// What the primary checkout holds: the branch HEAD names (none when
/// detached), the commit (none when unborn) and every local and remote-tracking
/// branch tip. The common Git directory's own HEAD is the primary checkout's,
/// whichever linked worktree asks.
pub(crate) struct PrimaryRefs {
    pub head_ref: Option<String>,
    pub head: Option<GitOid>,
    pub refs: std::collections::BTreeMap<String, GitOid>,
}

pub(crate) fn primary_refs(git: &Path, repo: &VerifiedRepo) -> Result<PrimaryRefs> {
    let read = |arguments: &[&str]| {
        command(
            git,
            repo.common_git_dir(),
            repo.root_path(),
            &args(arguments),
            None,
        )
    };
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).map(|text| text.trim().to_owned());
    let head_ref = read(&["symbolic-ref", "-q", "HEAD"])
        .ok()
        .and_then(|bytes| text(bytes).ok())
        .filter(|name| !name.is_empty());
    let head = read(&["rev-parse", "--verify", "--end-of-options", "HEAD"])
        .ok()
        .and_then(|bytes| text(bytes).ok())
        .and_then(|text| GitOid::parse(&text).ok());
    let listing = String::from_utf8(read(&[
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        "refs/heads/",
        "refs/remotes/",
    ])?)
    .map_err(|_| invalid())?;
    let mut refs = std::collections::BTreeMap::new();
    for line in listing.lines() {
        let (oid, name) = line.split_once(' ').ok_or_else(invalid)?;
        refs.insert(name.to_owned(), GitOid::parse(oid).map_err(|_| invalid())?);
    }
    Ok(PrimaryRefs {
        head_ref,
        head,
        refs,
    })
}

/// The commits at the heads of every linked worktree. The first listed
/// worktree is the primary checkout and is left out; the bound worktree and
/// other lanes' worktrees are in.
pub(crate) fn linked_worktree_heads(git: &Path, repo: &VerifiedRepo) -> Result<Vec<GitOid>> {
    let listing = String::from_utf8(command(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["worktree", "list", "--porcelain"]),
        None,
    )?)
    .map_err(|_| invalid())?;
    Ok(listing
        .split("\n\n")
        .skip(1)
        .flat_map(|entry| entry.lines())
        .filter_map(|line| line.strip_prefix("HEAD "))
        .filter_map(|oid| GitOid::parse(oid.trim()).ok())
        .collect())
}

/// Commits reachable from `tips` and from none of `excluded`, newest first and
/// at most `limit`, in one `git rev-list` call.
pub(crate) fn commits_outside(
    git: &Path,
    repo: &VerifiedRepo,
    tips: &[GitOid],
    excluded: &[GitOid],
    limit: usize,
) -> Result<Vec<GitOid>> {
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = String::new();
    for tip in tips {
        input.push_str(&format!("{tip}\n"));
    }
    for tip in excluded {
        input.push_str(&format!("^{tip}\n"));
    }
    let listing = execute_bounded(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["rev-list", &format!("--max-count={limit}"), "--stdin"]),
        None,
        Execution {
            input: Some(input.as_bytes()),
            ..Default::default()
        },
    )?;
    String::from_utf8(listing)
        .map_err(|_| invalid())?
        .lines()
        .map(|line| GitOid::parse(line.trim()).map_err(|_| invalid()))
        .collect()
}

/// List the refs below a Provenant-owned prefix. Ref names come from Git, so
/// they are already validated; the prefix check keeps the result in namespace.
fn list_refs(git: &Path, repo: &VerifiedRepo, prefix: &str) -> Result<Vec<String>> {
    let output = execute(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["for-each-ref", "--format=%(refname)", prefix]),
        None,
    )?;
    Ok(String::from_utf8_lossy(&output)
        .lines()
        .filter(|line| line.starts_with(prefix) && line.len() > prefix.len())
        .map(str::to_owned)
        .collect())
}

/// Delete refs in one atomic `update-ref --stdin` transaction, never one
/// process per ref: an attempt may pin thousands of objects. The `start`,
/// `prepare` and `commit` wrapper keeps the buffered transaction retry-safe on
/// a native ref-lock collision (`is_atomic_ref_transaction`).
pub(crate) fn delete_refs(git: &Path, repo: &VerifiedRepo, refs: &[String]) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let mut input = String::from("start\n");
    for reference in refs {
        input.push_str(&format!("delete {reference}\n"));
    }
    input.push_str("prepare\ncommit\n");
    execute_bounded(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["update-ref", "--stdin"]),
        None,
        Execution {
            input: Some(input.as_bytes()),
            ..Execution::default()
        },
    )?;
    Ok(())
}

/// Remove every import-extra pin of one attempt, whatever outcome settled it.
/// The OIDs are read from the ref store, so cleanup does not depend on the
/// success path having remembered the objects it imported.
pub(crate) fn clear_import_refs(git: &Path, repo: &VerifiedRepo, attempt: AttemptId) -> Result<()> {
    let prefix = format!("refs/provenant/import-extra/{attempt}/");
    delete_refs(git, repo, &list_refs(git, repo, &prefix)?)
}

/// Every attempt-scoped import-extra pin, for GC's settled/missing sweep.
pub(crate) fn all_import_extra_refs(git: &Path, repo: &VerifiedRepo) -> Result<Vec<String>> {
    list_refs(git, repo, "refs/provenant/import-extra/")
}

// Shared objects are host-owned. receive-pack checks newly introduced content
// and connectivity; delivery never hashes unrelated shared history.
pub(crate) fn require_commit(
    git: &Path,
    common: &Path,
    cwd: &Path,
    head: GitOid,
    base: Option<GitOid>,
    cancel: Option<&CancelToken>,
) -> Result<()> {
    execute(
        git,
        common,
        cwd,
        &args(&["cat-file", "-e", &format!("{head}^{{commit}}")]),
        cancel,
    )?;
    let mut arguments = args(&[
        "rev-list",
        "--objects",
        "--missing=error",
        "--no-object-names",
        &head.to_string(),
    ]);
    if let Some(base) = base {
        arguments.extend(args(&["--not", &base.to_string()]));
    }
    let objects = execute(git, common, cwd, &arguments, cancel)?;
    // rev-list checks links but does not inflate blob payloads. Read only this
    // delta so corrupt reachable shared blobs still fail before delivery.
    execute_bounded(
        git,
        common,
        cwd,
        &args(&["cat-file", "--batch", "--buffer"]),
        cancel,
        Execution {
            discard: true,
            input: Some(&objects),
            clean_stderr: true,
            ..Execution::default()
        },
    )
    .map(drop)
}

#[cfg(test)]
pub(crate) fn verify_graph(git: &Path, common: &Path, cwd: &Path, head: GitOid) -> Result<()> {
    require_commit(git, common, cwd, head, None, None)
}

pub(crate) fn is_ancestor(git: &Path, repo: &VerifiedRepo, ancestor: GitOid, head: GitOid) -> bool {
    execute(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&[
            "merge-base",
            "--is-ancestor",
            &ancestor.to_string(),
            &head.to_string(),
        ]),
        None,
    )
    .is_ok()
}

/// The known history a published lane tip descends from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ancestor {
    /// The pinned launch base: the expected case, nothing to report.
    LaunchBase,
    /// The tip has no known ancestor but is retained on the lane branch.
    Unrooted,
    /// A rebase or merge onto other history the host already knows: the
    /// origin default branch, the ref the lane launched against, or a
    /// remote-tracking branch. Publication proceeds and reports it.
    Known { reference: String, oid: GitOid },
}

impl Ancestor {
    pub(crate) fn warning(&self, branch: &str, base: GitOid, head: GitOid) -> Option<String> {
        match self {
            Self::LaunchBase => None,
            // The same claim publication warns about on stderr: the judged
            // head descended from no known root. The record must not launder it
            // into an ancestor inferred from a merge's host parent.
            Self::Unrooted => Some(format!(
                "lane tip {head} has no known ancestor; it does not descend from its launch base {base}; publishing {branch} anyway"
            )),
            Self::Known { reference, oid } => Some(format!(
                "lane tip {head} on {branch} no longer descends from its launch base {base}; \
                 published because it descends from {reference} ({oid})"
            )),
        }
    }
}

// Rebasing a lane onto the branch it feeds is a legitimate writer action, so
// any history the host already knows is acceptable. Order is fixed: the launch
// base, the origin default, the ref the lane launched against, then the first
// pushed remote-tracking branch the tip contains, then the host lane branch
// itself, which counts only when it is already in the tip. A merge that adds
// the branch as a parent must not root a history the judged tip does not have.
fn known_ancestor(
    git: &Path,
    repo: &VerifiedRepo,
    base: GitOid,
    launch_ref: Option<&str>,
    head: GitOid,
    branch: &str,
    branch_root: Option<GitOid>,
) -> Result<Option<Ancestor>> {
    let descends = |ancestor| is_ancestor(git, repo, ancestor, head);
    if descends(base) {
        return Ok(Some(Ancestor::LaunchBase));
    }
    for reference in ["refs/remotes/origin/HEAD"].into_iter().chain(launch_ref) {
        if let Ok(oid) = resolve_ref(git, repo, reference)
            && descends(oid)
        {
            return Ok(Some(Ancestor::Known {
                reference: reference.to_owned(),
                oid,
            }));
        }
    }
    let merged = execute(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&[
            "for-each-ref",
            "--merged",
            &head.to_string(),
            "--format=%(refname)",
            "refs/remotes/",
        ]),
        None,
    )?;
    for line in String::from_utf8_lossy(&merged).lines() {
        // origin/HEAD is a symbolic alias, already tried by name.
        if !line.ends_with("/HEAD")
            && let Ok(oid) = resolve_ref(git, repo, line)
        {
            return Ok(Some(Ancestor::Known {
                reference: line.to_owned(),
                oid,
            }));
        }
    }
    if let Some(root) = branch_root
        && descends(root)
    {
        return Ok(Some(Ancestor::Known {
            reference: branch.to_owned(),
            oid: root,
        }));
    }
    Ok(None)
}

/// Whether the judged lane head is the tip of the branch the writer pushed
/// upstream. A writer that rebased and force-pushed its own branch leaves the
/// remote-tracking ref at its new head while the host branch still points at the
/// displaced tip. That rewrite is the lane's own work, not a host commit gained
/// during the lane, so publication moves the host branch to the pushed head
/// instead of merging the displaced commits back into it.
fn pushed_upstream_tip(
    git: &Path,
    repo: &VerifiedRepo,
    head: GitOid,
    branch: &str,
) -> Result<bool> {
    let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let pattern = format!("refs/remotes/*/{short}");
    let refs = execute(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["for-each-ref", "--format=%(objectname)", &pattern]),
        None,
    )?;
    Ok(String::from_utf8_lossy(&refs)
        .lines()
        .any(|line| GitOid::parse(line.trim()).is_ok_and(|oid| oid == head)))
}

pub(crate) struct BranchPublication<'a> {
    pub branch: &'a str,
    pub base: GitOid,
    pub launch_ref: Option<&'a str>,
    pub head: GitOid,
    pub attempt: AttemptId,
    pub history: LaneHistory,
}

/// What a branch publication established. `previous` is the host tip the CAS
/// replaced: the tree the native index is expected to match before it advances.
/// `tip` is the commit that now ends the branch, which differs from the lane
/// head when publication merged a diverged history.
#[derive(Debug)]
pub(crate) struct Published {
    pub ancestor: Ancestor,
    pub previous: GitOid,
    pub tip: GitOid,
    /// Whether publication merged a diverged host history into the lane tip.
    pub merged: bool,
}

impl Published {
    /// Whether the branch now ends in this writer's own history: at the
    /// verified head, at a merge of that head with the host branch, or — on a
    /// resume after that merge — at the exact tip this attempt recorded before
    /// it moved the branch. A head the branch already contained publishes
    /// nothing, so the tip may be a commit someone else advanced; moving this
    /// worktree's files to it would discard what that head describes. Neither a
    /// commit's parents nor its identity can prove authorship: only the OID the
    /// attempt itself durably planned can.
    pub(crate) fn ends_at_head(&self, head: GitOid, recorded_tip: Option<GitOid>) -> bool {
        self.merged || self.tip == head || recorded_tip == Some(self.tip)
    }
}

/// What a worktree-free host merge of a diverged lane tip produced.
enum HostMerge {
    /// The merge commit that now ends the branch.
    Merged(GitOid),
    /// Real conflicts in these paths; the lane's private history is retained.
    Conflict(Vec<String>),
    /// The two tips share no commit, so Git refuses to merge them.
    Unrelated,
    /// The host repository configures a merge driver, which Git would execute
    /// under the lane's attributes.
    DriverConfigured,
}

/// Whether two commits share any history. `merge-base --all` exits 1 when they
/// do not, which is a fact about the two histories rather than a failure.
fn shares_history(git: &Path, repo: &VerifiedRepo, left: GitOid, right: GitOid) -> Result<bool> {
    let output = raw_outcome(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["merge-base", "--all", &left.to_string(), &right.to_string()]),
    )?;
    match output.termination {
        Termination::Exit(0) => Ok(true),
        Termination::Exit(1) => Ok(false),
        _ => Err(operation_io(repo.common_git_dir(), Some(output))),
    }
}

/// Whether the host repository configures any `merge.*.driver`. A driver runs
/// repository code selected by lane-controlled attributes, and delivery refuses
/// the same configuration before it merges (its "check configured merge
/// drivers" guard). Both read the shared effective scope, so an included or
/// worktree config cannot widen the merge past its guard.
fn configured_merge_driver(git: &Path, repo: &VerifiedRepo) -> Result<bool> {
    let output =
        provenant_os::git_config::merge_drivers(repo.common_git_dir().join("config"), |command| {
            raw_outcome(
                git,
                repo.common_git_dir(),
                repo.root_path(),
                command.as_ref(),
            )
        })
        .map_err(|error| RunError::ConfigCommandFailed {
            reason: provenant_core::bounded_line(&error.to_string(), 512),
        })?;
    match output.termination {
        // One `key value` line per configured driver.
        Termination::Exit(0) => Ok(!output.stdout.bytes.is_empty()),
        Termination::Exit(1) => Ok(false),
        _ => Err(operation_io(repo.common_git_dir(), Some(output))),
    }
}

/// Merge the host lane branch into a diverged lane tip without a worktree. The
/// merge runs only when no merge driver is configured and the two tips share
/// history; every outcome is classified so the caller can name the setting that
/// decides it. A conflict retains the lane's private history.
fn host_merge(git: &Path, repo: &VerifiedRepo, current: GitOid, head: GitOid) -> Result<HostMerge> {
    if configured_merge_driver(git, repo)? {
        return Ok(HostMerge::DriverConfigured);
    }
    if !shares_history(git, repo, current, head)? {
        return Ok(HostMerge::Unrelated);
    }
    let output = raw_outcome(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            &current.to_string(),
            &head.to_string(),
        ]),
    )?;
    match output.termination {
        Termination::Exit(0) => {
            let text = String::from_utf8_lossy(&output.stdout.bytes);
            let tree = text.lines().next().unwrap_or_default().trim();
            let commit = execute(
                git,
                repo.common_git_dir(),
                repo.root_path(),
                &args(&[
                    "-c",
                    "user.name=Provenant",
                    "-c",
                    "user.email=provenant@localhost",
                    "commit-tree",
                    tree,
                    "-p",
                    &current.to_string(),
                    "-p",
                    &head.to_string(),
                    "-m",
                    "provenant: merge diverged lane history",
                ]),
                None,
            )?;
            let merged =
                GitOid::parse(String::from_utf8_lossy(&commit).trim()).map_err(|_| invalid())?;
            Ok(HostMerge::Merged(merged))
        }
        // The first line is the tree of the conflicted merge; the rest name the
        // paths that conflict.
        Termination::Exit(1) => Ok(HostMerge::Conflict(
            String::from_utf8_lossy(&output.stdout.bytes)
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        )),
        // Any other exit is Git failing, not a fact about the two histories.
        _ => Err(operation_io(repo.common_git_dir(), Some(output))),
    }
}

/// `checkpoint` runs after the target commit exists but before the branch ref
/// moves. It durably records the planned OID in this attempt's publication
/// progress, the only witness on resume that a tip is this attempt's own merge
/// rather than a two-parent commit another writer made from the verified head.
pub(crate) fn publish_branch(
    git: &Path,
    repo: &VerifiedRepo,
    publication: BranchPublication<'_>,
    checkpoint: impl FnOnce(GitOid) -> Result<()>,
) -> Result<Published> {
    let BranchPublication {
        branch,
        base,
        launch_ref,
        head,
        attempt,
        history,
    } = publication;
    let current = resolve_ref(git, repo, branch)?;
    // A head the branch already contains publishes nothing, so it asks no
    // history question and cannot be refused for one. A branch still at its
    // launch base has not moved since launch, so a head behind it is the
    // lane's own rewind and publishes like any lane tip.
    let contains_head = current != base && is_ancestor(git, repo, head, current);
    // A branch rewound behind its launch base is judged before any history
    // question: a fast-forward would reinstate the history the host removed,
    // and a warning about the tip would only precede that refusal.
    if !contains_head && current != base && is_ancestor(git, repo, current, base) {
        return Err(RunError::PublicationRewound {
            branch_tip: current,
            base,
        });
    }
    let advances_current = !contains_head && is_ancestor(git, repo, current, head);
    // The unrooted setting judges the lane head, before any host merge exists:
    // a merge's parents must never supply ancestry the head does not have. The
    // host branch counts as a known root only when it is already in the tip.
    let lane_ancestor = known_ancestor(
        git,
        repo,
        base,
        launch_ref,
        head,
        branch,
        (contains_head || advances_current).then_some(current),
    )?;
    if lane_ancestor.is_none() && !contains_head {
        match history.unrooted_tip.value {
            HistoryEffect::Off => {}
            HistoryEffect::Warn => {
                let warning = Ancestor::Unrooted
                    .warning(branch, base, head)
                    .ok_or_else(invalid)?;
                use provenant_records::warning::{
                    LANE_HISTORY_PUBLICATION_ADJUSTED, Warning, warn,
                };
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(
                    Warning::new(
                        LANE_HISTORY_PUBLICATION_ADJUSTED,
                        format!(
                            "{warning}; {}; set {UNROOTED_TIP} to refuse to stop instead",
                            history.unrooted_tip.setting(UNROOTED_TIP),
                        ),
                    )
                    .subject(branch.to_owned()),
                );
            }
            HistoryEffect::Refuse => {
                return Err(RunError::LaneTipUnrooted(Box::new(LaneTipRefusal {
                    tip: head,
                    base,
                    launch_ref: launch_ref.map(str::to_owned),
                    setting: history.unrooted_tip.setting(UNROOTED_TIP),
                })));
            }
        }
    }
    let (target, merged, rewritten) = if contains_head {
        // The branch already contains the lane head, so publishing changes
        // nothing.
        (current, false, false)
    } else if current == base || advances_current {
        // Fast-forward from the launch base or from later lane commits.
        (head, false, false)
    } else if history.rebase.value == Rebase::Allow && pushed_upstream_tip(git, repo, head, branch)?
    {
        // The lane rewrote its branch, force-pushed it and left the upstream ref
        // at its new head. Moving the host branch there is not a merge: the
        // displaced tip is the lane's own superseded commits, and the CAS below
        // retains them for host recovery. A host commit made during the lane is
        // not the pushed tip, so it still takes the merge/refusal path.
        (head, false, true)
    } else {
        match history.rebase.value {
            Rebase::Refuse => {
                return Err(RunError::PublicationDiverged {
                    setting: history.rebase.setting(REBASE),
                });
            }
            Rebase::Allow => match host_merge(git, repo, current, head)? {
                HostMerge::Merged(target) => (target, true, false),
                HostMerge::Conflict(conflicting_files) => {
                    return Err(RunError::PublicationConflict {
                        conflicting_files,
                        setting: history.rebase.setting(REBASE),
                    });
                }
                HostMerge::Unrelated => {
                    return Err(RunError::PublicationUnrelated {
                        setting: history.rebase.setting(REBASE),
                    });
                }
                HostMerge::DriverConfigured => {
                    return Err(RunError::PublicationMergeDriver {
                        setting: history.rebase.setting(REBASE),
                    });
                }
            },
        }
    };
    if merged || rewritten {
        use provenant_records::warning::{LANE_HISTORY_PUBLICATION_ADJUSTED, Warning, warn};
        // Spec: docs/specs/cli.md#warnings-are-typed-values
        let message = if rewritten {
            format!(
                "lane history on {branch} was rewritten and force-pushed; published the pushed tip {head} over the displaced host tip {current}; {}; set {REBASE} to refuse to retain the lane's private history instead",
                history.rebase.setting(REBASE),
            )
        } else {
            format!(
                "lane history on {branch} diverged from the host branch; published a host merge of {head} and {current} as {target}; {}; set {REBASE} to refuse to retain the lane's private history instead",
                history.rebase.setting(REBASE),
            )
        };
        warn(Warning::new(LANE_HISTORY_PUBLICATION_ADJUSTED, message).subject(branch.to_owned()));
    }
    // The ancestor the publication established is the one judged on the lane
    // head before any host merge. A merge adds the host branch as a parent, so
    // recomputing from its tip would launder an unrooted head into a "descends
    // from the branch" record that disagrees with the warning already written
    // about that head.
    let ancestor = lane_ancestor.unwrap_or(Ancestor::Unrooted);
    // Record the planned OID before the ref moves. A resumed publication that
    // finds the branch already containing the head does not publish or merge
    // again; it accepts only the tip recorded here. When the branch already
    // contained the head there is nothing of this attempt's own to record, so
    // a foreign tip cannot become a planned tip on a later resume.
    if !contains_head {
        checkpoint(target)?;
    }
    // receive-pack has checked the graph. Git's CAS protects concurrent host
    // updates, including when a lane rebases or merges its private branch.
    {
        let message = match &ancestor {
            Ancestor::LaunchBase => "provenant: verified lane objects".to_owned(),
            Ancestor::Unrooted => "provenant: verified lane objects (no known ancestor)".to_owned(),
            Ancestor::Known { reference, oid } => {
                format!("provenant: verified lane objects (descends from {reference} {oid})")
            }
        };
        // Retain any history displaced by a rewrite in the same ref transaction
        // as the branch CAS. The content-addressed ref is stable on retry.
        let recovery = format!("refs/provenant/recovery/{current}");
        let preserve = target != current && !is_ancestor(git, repo, current, target);
        let transaction = if preserve {
            let retain = match resolve_ref(git, repo, &recovery) {
                // --no-deref replaces a symbolic ref itself with a direct pin.
                Ok(existing) if existing == current => {
                    format!("update {recovery} {current} {current}")
                }
                Ok(_) => return Err(invalid()),
                Err(_) => format!("create {recovery} {current}"),
            };
            format!("{retain}\nupdate {branch} {target} {current}\n")
        } else {
            format!("update {branch} {target} {current}\n")
        };
        execute_bounded(
            git,
            repo.common_git_dir(),
            repo.root_path(),
            &args(&["update-ref", "--no-deref", "-m", &message, "--stdin"]),
            None,
            Execution {
                input: Some(transaction.as_bytes()),
                ..Execution::default()
            },
        )?;
    }
    command(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["update-ref", "-d", &import_pin(attempt, head)]),
        None,
    )?;
    Ok(Published {
        ancestor,
        previous: current,
        tip: target,
        merged,
    })
}

pub(crate) fn import_pin(attempt: AttemptId, head: GitOid) -> String {
    format!("refs/provenant/import/{attempt}/{head}")
}

fn directory(parent: &Directory, name: &OsStr) -> Result<Directory> {
    match parent.mkdir(name, 0o700) {
        Ok(()) => parent.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(parent.open_verified_directory(name)?)
}

// Validate entry types before copying; sibling writers may still be active.
// Git only traverses the host-owned import copy, never this mutable tree.
fn object_bytes(directory: &Directory, depth: usize) -> Result<u64> {
    let mut entries = MAX_OBJECT_ENTRIES;
    object_bytes_with_entries(directory, depth, &mut entries)
}

const MAX_OBJECT_ENTRIES: u64 = 100_000;

fn object_entry(remaining: &mut u64) -> Result<()> {
    *remaining = remaining.checked_sub(1).ok_or(RunError::ObjectImportLimit(
        "lane object store exceeds its entry limit",
    ))?;
    Ok(())
}

fn object_bytes_with_entries(
    directory: &Directory,
    depth: usize,
    entries: &mut u64,
) -> Result<u64> {
    if depth > 128 {
        return Err(RunError::Validation(
            "lane object store nesting exceeds limit",
        ));
    }
    let mut bytes = 0_u64;
    for entry in directory.entries()? {
        let entry = entry?;
        object_entry(entries)?;
        let metadata = directory.metadata_at(&entry, false)?;
        let size = match metadata.kind {
            FileKind::Regular => metadata.size,
            FileKind::Directory => object_bytes_with_entries(
                &directory.open_verified_directory(&entry)?,
                depth + 1,
                entries,
            )?,
            _ => {
                return Err(RunError::Validation(
                    "lane object store contains a symlink or special file",
                ));
            }
        };
        bytes = bytes.checked_add(size).ok_or(RunError::ObjectImportLimit(
            "lane object byte count overflow",
        ))?;
    }
    Ok(bytes)
}

fn snapshot_source(
    source: &Directory,
    target: &Directory,
    remaining: &mut u64,
    entries: &mut u64,
    depth: usize,
) -> Result<()> {
    if depth > 128 {
        return Err(RunError::Validation(
            "lane object store nesting exceeds limit",
        ));
    }
    for name in source.entries()? {
        let name = name?;
        object_entry(entries)?;
        let before = source.metadata_at(&name, false)?;
        match before.kind {
            FileKind::Directory => snapshot_source(
                &source.open_verified_directory(&name)?,
                &directory(target, &name)?,
                remaining,
                entries,
                depth + 1,
            )?,
            FileKind::Regular => {
                if before.size > *remaining {
                    return Err(RunError::ObjectImportLimit(
                        "lane object snapshot grew beyond its byte budget",
                    ));
                }
                let mut input = source.open_read(&name)?;
                if !before.unchanged(&input.metadata()?) {
                    return Err(provenant_os::fs::FsError::RaceRefused { name }.into());
                }
                let mut output = target.create_new(&name)?;
                let input_path = source.current_path()?.join(&name);
                let copied = {
                    let mut bounded = (&mut input).take(before.size.saturating_add(1));
                    std::io::copy(&mut bounded, &mut output).map_err(|source| {
                        RunError::OperationIo {
                            operation: "snapshot lane Git objects",
                            path: Some(input_path),
                            source,
                        }
                    })?
                };
                if copied != before.size
                    || !before.unchanged(&input.metadata()?)
                    || !before.unchanged(&source.metadata_at(&name, false)?)
                {
                    return Err(provenant_os::fs::FsError::RaceRefused { name }.into());
                }
                *remaining -= copied;
            }
            _ => {
                return Err(RunError::Validation(
                    "lane object store contains a symlink or special file",
                ));
            }
        }
    }
    Ok(())
}

fn sanitise_source(source: &Directory, repo: &VerifiedRepo, is_snapshot: bool) -> Result<u64> {
    object_bytes(source, 0)?;
    if let Ok(info) = source.open_verified_directory(OsStr::new("info")) {
        match info.read_stable(OsStr::new("alternates"), 1024 * 1024) {
            Ok(bytes) => {
                let shared = repo.common_git_dir().join("objects");
                // Only the host's quoted alternate is accepted; foreign paths fail.
                if bytes != format!("{}\n", quote_alternate(&shared)).as_bytes()
                    && !bytes.is_empty()
                {
                    return Err(RunError::Validation("lane object alternates are untrusted"));
                }
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        // The exact validated host alternate is also used by live siblings.
        // It is host-maintained and must survive budget and import preparation.
        crate::artifacts::remove_entry_tree(&info, OsStr::new("http-alternates"))?;
        if is_snapshot {
            for name in ["commit-graph", "commit-graphs"] {
                crate::artifacts::remove_entry_tree(&info, OsStr::new(name))?;
            }
        }
    }
    if is_snapshot {
        for name in ["commit-graph", "multi-pack-index"] {
            crate::artifacts::remove_entry_tree(source, OsStr::new(name))?;
        }
        if let Ok(pack) = source.open_verified_directory(OsStr::new("pack")) {
            for entry in pack.entries()? {
                let entry = entry?;
                if entry.as_encoded_bytes().starts_with(b"multi-pack-index") {
                    crate::artifacts::remove_entry_tree(&pack, &entry)?;
                }
            }
        }
    }
    object_bytes(source, 0)
}

/// Apply the existing import checks before budget Git writes any lane objects.
pub(crate) fn prepare_budget_objects(
    source: &Directory,
    repo: &VerifiedRepo,
) -> Result<std::path::PathBuf> {
    let path = source_path(source, repo)?;
    sanitise_source(source, repo, false)?;
    Ok(path)
}

// Git needs a pathname for alternates. Reopen it without following a final
// symlink and compare the held identity before giving Git access. Custody must
// remain absent while Git uses it. Never sanitise the shared store as a source.
fn source_path(source: &Directory, repo: &VerifiedRepo) -> Result<std::path::PathBuf> {
    let path = source.current_path()?;
    if path.starts_with(repo.common_git_dir()) || repo.common_git_dir().starts_with(&path) {
        return Err(RunError::Validation(
            "lane objects overlap shared Git metadata",
        ));
    }
    let reopened = Directory::open_anchor(&path)?;
    if !source.metadata()?.same_identity(&reopened.metadata()?) {
        return Err(RunError::Validation(
            "lane object directory identity changed",
        ));
    }
    Ok(path)
}

fn import_limits(bytes: u64) -> Result<(u64, Duration)> {
    // Repacking can expand a delta and protocol overhead is non-zero, even
    // for an empty private store. Keep the cap tied to persisted lane bytes.
    let cap = bytes
        .checked_mul(4)
        .and_then(|n| n.checked_add(16 * 1024 * 1024))
        .ok_or(RunError::ObjectImportLimit(
            "lane object byte limit overflow",
        ))?;
    let seconds = 120_u64.saturating_add(cap.div_ceil(1024 * 1024));
    Ok((cap, Duration::from_secs(seconds)))
}

fn receiver_args(cap: u64) -> Vec<OsString> {
    args(&[
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "receive.fsckObjects=true",
        "-c",
        "transfer.fsckObjects=true",
        "-c",
        "receive.unpackLimit=1",
        "-c",
        "transfer.unpackLimit=1",
        "-c",
        &format!("receive.maxInputSize={cap}"),
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "receive-pack",
    ])
}

const STALE_IMPORT_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn stale_tree(directory: &Directory, cutoff: i128, depth: usize) -> Result<bool> {
    if depth > 128 || directory.metadata()?.modified_ns >= cutoff {
        return Ok(false);
    }
    for entry in directory.entries()? {
        let entry = entry?;
        let metadata = directory.metadata_at(&entry, false)?;
        if metadata.modified_ns >= cutoff {
            return Ok(false);
        }
        if metadata.kind == FileKind::Directory
            && !stale_tree(
                &directory.open_verified_directory(&entry)?,
                cutoff,
                depth + 1,
            )?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn sweep_stale(repo: &VerifiedRepo) -> Result<()> {
    let now = provenant_os::clock::SystemClock::new()
        .wall_now()
        .map_err(|_| RunError::Admission(AdmissionError::Clock))?
        .as_micros();
    let cutoff = i128::from(now) * 1000
        - i128::try_from(STALE_IMPORT_AGE.as_nanos()).map_err(|_| invalid())?;
    let objects = repo
        .common_dir()
        .open_verified_directory(OsStr::new("objects"))?;
    for (parent, prefix) in [
        (repo.common_dir(), b"tmp-provenant-import-".as_slice()),
        (repo.common_dir(), b".provenant-staging.".as_slice()),
        (&objects, b"incoming-".as_slice()),
    ] {
        for entry in parent.entries()? {
            let entry = entry?;
            if entry.as_encoded_bytes().starts_with(prefix)
                && parent.metadata_at(&entry, false)?.kind == FileKind::Directory
                && stale_tree(&parent.open_verified_directory(&entry)?, cutoff, 0)?
            {
                crate::artifacts::remove_entry_tree(parent, &entry)?;
            }
        }
    }
    Ok(())
}

/// Reuse the host-owned Git view for operations that must never load lane config.
pub(crate) fn with_clean_view<T>(
    git: &Path,
    repo: &VerifiedRepo,
    head: GitOid,
    cancel: Option<&CancelToken>,
    operation: impl FnOnce(&Directory, &Path) -> Result<T>,
) -> Result<T> {
    sweep_stale(repo)?;
    let name = OsString::from(format!(
        "tmp-provenant-import-{}",
        provenant_os::fs::temporary_name()?.to_string_lossy()
    ));
    repo.common_dir().mkdir(&name, 0o700)?;
    let view = repo.common_dir().open_verified_directory(&name)?;
    let path = repo.common_git_dir().join(&name);
    let result: Result<T> = (|| {
        execute(
            git,
            &path,
            repo.root_path(),
            &args(&[
                "init",
                "--bare",
                "--quiet",
                "--template=",
                &format!(
                    "--object-format={}",
                    if head.to_string().len() == 64 {
                        "sha256"
                    } else {
                        "sha1"
                    }
                ),
                path.to_str().ok_or_else(invalid)?,
            ]),
            cancel,
        )?;
        operation(&view, &path)
    })();
    let cleanup = crate::artifacts::remove_entry_tree(repo.common_dir(), &name);
    let value = result?;
    cleanup?;
    Ok(value)
}

/// Use a clean host-owned sender view, never mutable lane configuration.
/// receive-pack quarantines objects until integrity and connectivity pass.
#[cfg(test)]
pub(crate) fn settle(
    git: &Path,
    repo: &VerifiedRepo,
    source: Option<&Directory>,
    head: GitOid,
) -> Result<()> {
    settle_cancel(
        git,
        repo,
        source,
        AttemptId::from_bytes([0; 16]),
        head,
        None,
        None,
    )
}

#[cfg(test)]
pub(crate) fn settle_cancel(
    git: &Path,
    repo: &VerifiedRepo,
    source: Option<&Directory>,
    attempt: AttemptId,
    head: GitOid,
    cancel: Option<&CancelToken>,
    custody: Option<&provenant_supervisor::custody::Recovery>,
) -> Result<()> {
    settle_many(
        git,
        repo,
        source,
        head,
        &[],
        cancel,
        ImportScope { attempt, custody },
    )
}

pub(crate) fn settle_many(
    git: &Path,
    repo: &VerifiedRepo,
    source: Option<&Directory>,
    head: GitOid,
    extra: &[GitOid],
    cancel: Option<&CancelToken>,
    scope: ImportScope<'_>,
) -> Result<()> {
    let source = source.ok_or(RunError::Validation("writer has no private object directory; recover its commits on the host and start a fresh run"))?;
    repo.revalidate().map_err(|_| invalid())?;
    let source_path = source_path(source, repo)?;
    let bytes = object_bytes(source, 0)?;
    let (cap, timeout) = import_limits(bytes)?;
    with_clean_view(git, repo, head, cancel, |view, path| {
        let snapshot = directory(view, OsStr::new("lane-source"))?;
        let mut remaining = bytes;
        let mut entries = MAX_OBJECT_ENTRIES;
        snapshot_source(source, &snapshot, &mut remaining, &mut entries, 0)?;
        sanitise_source(&snapshot, repo, true)?;
        let snapshot_path = snapshot.current_path()?;
        let objects = view.open_verified_directory(OsStr::new("objects"))?;
        let info = objects.open_verified_directory(OsStr::new("info"))?;
        info.atomic_write(
            OsStr::new("alternates"),
            ".import-",
            format!(
                "{}\n{}\n",
                quote_alternate(&snapshot_path),
                quote_alternate(&repo.common_git_dir().join("objects"))
            )
            .as_bytes(),
            PublishMode::Replace,
        )?
        .require_durable()?;
        match repo
            .common_dir()
            .read_stable(OsStr::new("shallow"), 1024 * 1024)
        {
            Ok(bytes) => view
                .atomic_write(
                    OsStr::new("shallow"),
                    ".import-",
                    &bytes,
                    PublishMode::NoReplace,
                )?
                .require_durable()?,
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        let receiver = std::iter::once(git.as_os_str().to_owned())
            .chain(receiver_args(cap))
            .map(|arg| format!("'{}'", arg.to_string_lossy().replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        // Recheck the captured source identity and this attempt's custody before
        // importing the host-owned copy. Siblings may continue writing source.
        if scope.custody.is_some_and(|custody| {
            custody.observe() != provenant_supervisor::custody::RecoveryState::Absent
        }) {
            return Err(RunError::Validation(
                "writer object import requires absent custody",
            ));
        }
        let reopened = Directory::open_anchor(&source_path)?;
        if !source.metadata()?.same_identity(&reopened.metadata()?) {
            return Err(RunError::Validation(
                "lane object directory identity changed",
            ));
        }
        if scope
            .custody
            .is_some_and(|held| held.scope().attempt != scope.attempt)
        {
            return Err(RunError::Validation("writer import attempt mismatch"));
        }
        let attempt_scope = scope.attempt.to_string();
        let mut refspecs = vec![format!("{head}:{}", import_pin(scope.attempt, head))];
        refspecs.extend(
            extra
                .iter()
                .map(|oid| format!("{oid}:refs/provenant/import-extra/{attempt_scope}/{oid}")),
        );
        refspecs.sort();
        refspecs.dedup();
        for chunk in refspecs.chunks(128) {
            let mut push = args(&[
                "-c",
                "protocol.file.allow=always",
                "push",
                "--no-verify",
                "--porcelain",
            ]);
            push.push(format!("--receive-pack={receiver}").into());
            push.push(repo.common_git_dir().into());
            push.extend(chunk.iter().map(Into::into));
            execute_bounded(
                git,
                path,
                repo.root_path(),
                &push,
                cancel,
                Execution {
                    timeout,
                    ..Execution::default()
                },
            )?;
        }
        Ok(())
    })
}

#[cfg(test)]
#[path = "objects_tests.rs"]
mod tests;
