//! Carry uncommitted chair edits into a fresh writer worktree.
//!
//! Spec: docs/specs/agent-native.md#4-a-lane-as-a-native-sub-agent
//! Issue: #3453
//!
//! A snapshot is taken at launch with `git stash create`, which writes objects
//! only: it never changes the source worktree or its index. `All` mode adds
//! untracked, non-ignored files. Applying the snapshot in the new worktree
//! creates the writer's first commit, `wip: carried from chair`, so the parent
//! can see exactly which paths moved into the child.

use provenant_core::{CancelToken, GitOid};
use provenant_os::exec::{ExecOptions, Executor, Termination};
use provenant_records::lane_contract::CarryMode;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// Default bound for each Git command in a carry operation.
pub const CARRY_TIMEOUT: Duration = Duration::from_secs(30);

static INDEX_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct SnapshotIndex(PathBuf);

impl Drop for SnapshotIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One untracked, non-ignored file carried in `All` mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedFile {
    /// Worktree-relative path as reported by `git ls-files`.
    pub path: String,
    pub content: CarriedContent,
}

/// Preserve Git's distinction between regular files and symbolic links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CarriedContent {
    File { bytes: Vec<u8>, executable: bool },
    Symlink(PathBuf),
}

/// A bounded snapshot of the chair's uncommitted edits. `stash` is the object
/// id returned by `git stash create`; `paths` is the display list for the
/// launch footer; `untracked` holds the files carried in `All` mode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CarrySnapshot {
    pub stash: Option<GitOid>,
    pub paths: Vec<String>,
    pub untracked: Vec<CarriedFile>,
    pub mode: CarryMode,
}

impl CarrySnapshot {
    pub fn is_empty(&self) -> bool {
        self.stash.is_none() && self.untracked.is_empty()
    }
}

fn capture_untracked(source: &Path, path: &str) -> Result<CarriedFile, CarryError> {
    use provenant_os::fs::{Directory, FileKind};
    use std::path::Component;
    let mut directory =
        Directory::open_anchor(source).map_err(|error| CarryError::Snapshot(error.to_string()))?;
    let mut components = Path::new(path).components().peekable();
    let name = loop {
        let Some(Component::Normal(name)) = components.next() else {
            return Err(CarryError::Snapshot(format!(
                "invalid untracked path: {path}"
            )));
        };
        if components.peek().is_none() {
            break name;
        }
        directory = directory
            .open_directory(name)
            .map_err(|error| CarryError::Snapshot(error.to_string()))?;
    };
    let before = directory
        .metadata_at(name, false)
        .map_err(|error| CarryError::Snapshot(error.to_string()))?;
    let content = match before.kind {
        FileKind::Regular => {
            let bytes = directory
                .read_stable(name, u64::MAX - 1)
                .map_err(|error| CarryError::Snapshot(error.to_string()))?;
            let after = directory
                .metadata_at(name, false)
                .map_err(|error| CarryError::Snapshot(error.to_string()))?;
            if !before.unchanged(&after) {
                return Err(CarryError::Snapshot(format!(
                    "untracked file changed: {path}"
                )));
            }
            CarriedContent::File {
                bytes,
                executable: before.mode & 0o100 != 0,
            }
        }
        FileKind::Symlink => {
            let link = directory
                .read_link(name)
                .map_err(|error| CarryError::Snapshot(error.to_string()))?;
            let after = directory
                .metadata_at(name, false)
                .map_err(|error| CarryError::Snapshot(error.to_string()))?;
            if !before.unchanged(&after) {
                return Err(CarryError::Snapshot(format!(
                    "untracked symlink changed: {path}"
                )));
            }
            CarriedContent::Symlink(link)
        }
        _ => {
            return Err(CarryError::Snapshot(format!(
                "untracked path is not a file or symlink: {path}"
            )));
        }
    };
    Ok(CarriedFile {
        path: path.to_owned(),
        content,
    })
}

fn restore_untracked(target: &Path, file: &CarriedFile) -> Result<(), CarryError> {
    use provenant_os::fs::Directory;
    use std::{io::Write, path::Component};
    let mut directory =
        Directory::open_anchor(target).map_err(|error| CarryError::Apply(error.to_string()))?;
    let mut components = Path::new(&file.path).components().peekable();
    let name = loop {
        let Some(Component::Normal(name)) = components.next() else {
            return Err(CarryError::Apply(format!(
                "invalid carried path: {}",
                file.path
            )));
        };
        if components.peek().is_none() {
            break name;
        }
        match directory.mkdir(name, 0o755) {
            Ok(()) => {}
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(CarryError::Apply(error.to_string())),
        }
        directory = directory
            .open_directory(name)
            .map_err(|error| CarryError::Apply(error.to_string()))?;
    };
    match &file.content {
        CarriedContent::File { bytes, executable } => {
            let mut output = directory
                .create_new(name)
                .map_err(|error| CarryError::Apply(error.to_string()))?;
            output
                .write_all(bytes)
                .map_err(|error| CarryError::Apply(error.to_string()))?;
            if *executable {
                output
                    .make_owner_executable()
                    .map_err(|error| CarryError::Apply(error.to_string()))?;
            }
        }
        CarriedContent::Symlink(link) => directory
            .symlink(link, name)
            .map_err(|error| CarryError::Apply(error.to_string()))?,
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CarryError {
    #[error("git executable unavailable")]
    GitUnavailable,
    #[error("git stash create failed: {0}")]
    Snapshot(String),
    #[error("git stash apply failed: {0}")]
    Apply(String),
    #[error("git add failed: {0}")]
    Stage(String),
    #[error("git commit failed: {0}")]
    Commit(String),
    #[error("invalid git object id: {0}")]
    InvalidOid(String),
    #[error("git command {operation} timed out")]
    TimedOut { operation: &'static str },
    #[error("git command failed: {0}")]
    Exec(#[from] provenant_os::exec::ExecError),
}

/// One bounded Git invocation. The caller owns the executor, environment and
/// cancellation; no process-wide state is touched.
pub(crate) struct GitOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub incomplete: bool,
}

pub(crate) fn git(
    cwd: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<GitOutput, CarryError> {
    git_with_stdin(cwd, args, None, env, executor, cancel, timeout)
}

pub(crate) fn git_with_stdin(
    cwd: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<GitOutput, CarryError> {
    let program = provenant_os::git::git_executable().ok_or(CarryError::GitUnavailable)?;
    let argv: Vec<OsString> = std::iter::once(program.into_os_string())
        .chain(args.iter().map(OsString::from))
        .collect();
    let mut options = ExecOptions::new(&argv, cwd, timeout);
    options.env = Some(env);
    options.stdin = stdin;
    options.cancel = Some(cancel);
    let outcome = executor.execute(options)?;
    let incomplete = outcome.stdout.truncated
        || outcome.stdout.read_failed
        || outcome.drain_truncated
        || std::str::from_utf8(&outcome.stdout.bytes).is_err();
    Ok(GitOutput {
        ok: outcome.termination == Termination::Exit(0),
        stdout: String::from_utf8_lossy(&outcome.stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&outcome.stderr.bytes).into_owned(),
        timed_out: outcome.timed_out,
        incomplete,
    })
}

/// Snapshot uncommitted edits in `source` without touching its worktree or
/// index. `Off` returns an empty snapshot. `Tracked` snapshots staged and
/// unstaged tracked changes; `All` also snapshots untracked, non-ignored files.
pub fn snapshot(
    source: &Path,
    mode: CarryMode,
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<CarrySnapshot, CarryError> {
    if mode == CarryMode::Off {
        return Ok(CarrySnapshot {
            stash: None,
            paths: Vec::new(),
            untracked: Vec::new(),
            mode,
        });
    }
    // Stash's internal update-index must not write the chair's index,
    // including on a failed or interrupted snapshot.
    let mut readonly_env = env.to_vec();
    readonly_env.push((OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0")));
    let status = git(
        source,
        &["status", "--porcelain=v2", "-z", "--ignore-submodules=none"],
        &readonly_env,
        executor,
        cancel,
        timeout,
    )?;
    if status.timed_out {
        return Err(CarryError::TimedOut {
            operation: "status",
        });
    }
    if !status.ok || status.incomplete {
        return Err(CarryError::Snapshot(status.stderr));
    }
    if status
        .stdout
        .split('\0')
        .filter(|line| line.starts_with("1 ") || line.starts_with("2 "))
        .filter_map(|line| line.split(' ').nth(2))
        .any(|sub| {
            sub.starts_with('S') && sub.as_bytes().get(2..4).is_some_and(|bits| bits != b"..")
        })
    {
        return Err(CarryError::Snapshot(
            "uncommitted submodule contents cannot be carried".into(),
        ));
    }

    let index = git(
        source,
        &["rev-parse", "--git-path", "index"],
        &readonly_env,
        executor,
        cancel,
        timeout,
    )?;
    if index.timed_out {
        return Err(CarryError::TimedOut {
            operation: "rev-parse",
        });
    }
    if !index.ok || index.incomplete {
        return Err(CarryError::Snapshot(index.stderr));
    }
    let source_index = source.join(index.stdout.trim_end());
    let index_path = std::env::temp_dir().join(format!(
        "provenant-carry-{}-{}.index",
        std::process::id(),
        INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let private = SnapshotIndex(index_path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut destination = options
        .open(&private.0)
        .map_err(|error| CarryError::Snapshot(error.to_string()))?;
    let mut original = std::fs::File::open(source_index)
        .map_err(|error| CarryError::Snapshot(error.to_string()))?;
    std::io::copy(&mut original, &mut destination)
        .map_err(|error| CarryError::Snapshot(error.to_string()))?;
    drop(destination);
    let mut index_env = readonly_env.clone();
    index_env.push((
        OsString::from("GIT_INDEX_FILE"),
        private.0.as_os_str().to_owned(),
    ));
    // Untracked files are carried separately because stash create excludes them.
    let created = git(
        source,
        &["stash", "create", "carry lane edits"],
        &index_env,
        executor,
        cancel,
        timeout,
    )?;
    if created.timed_out {
        return Err(CarryError::TimedOut { operation: "stash" });
    }
    if !created.ok {
        return Err(CarryError::Snapshot(created.stderr));
    }
    let oid = created.stdout.trim();
    let stash = if oid.is_empty() {
        None
    } else {
        Some(GitOid::parse(oid).map_err(|_| CarryError::InvalidOid(oid.to_owned()))?)
    };

    // Derive the footer from the saved object, not an earlier status read:
    // the chair may have edited another path while Git made the snapshot.
    // Disabling rename detection includes both the removed and added paths.
    let mut paths = if let Some(stash) = stash {
        let parent = format!("{stash}^1");
        let saved = stash.to_string();
        let diff = git(
            source,
            &["diff", "--no-renames", "--name-only", "-z", &parent, &saved],
            &readonly_env,
            executor,
            cancel,
            timeout,
        )?;
        if diff.timed_out {
            return Err(CarryError::TimedOut { operation: "diff" });
        }
        if !diff.ok || diff.incomplete {
            return Err(CarryError::Snapshot(diff.stderr));
        }
        diff.stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };

    let untracked = if mode == CarryMode::All {
        let listed = git(
            source,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            &readonly_env,
            executor,
            cancel,
            timeout,
        )?;
        if listed.timed_out {
            return Err(CarryError::TimedOut {
                operation: "ls-files",
            });
        }
        if !listed.ok || listed.incomplete {
            return Err(CarryError::Snapshot(listed.stderr));
        }
        listed
            .stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| capture_untracked(source, path))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    paths.extend(untracked.iter().map(|file| file.path.clone()));
    paths.sort();
    paths.dedup();
    Ok(CarrySnapshot {
        stash,
        paths,
        untracked,
        mode,
    })
}

/// Read the checkout HEAD for a carry eligibility or recovery check.
pub fn head(
    worktree: &Path,
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<GitOid, CarryError> {
    let result = git(
        worktree,
        &["rev-parse", "--verify", "HEAD"],
        env,
        executor,
        cancel,
        timeout,
    )?;
    if result.timed_out {
        return Err(CarryError::TimedOut {
            operation: "rev-parse",
        });
    }
    if !result.ok || result.incomplete {
        return Err(CarryError::Snapshot(result.stderr));
    }
    let value = result.stdout.trim();
    GitOid::parse(value).map_err(|_| CarryError::InvalidOid(value.into()))
}

/// Identify the exact HEAD from which Git constructed the tracked snapshot.
pub fn snapshot_base(
    source: &Path,
    snapshot: &CarrySnapshot,
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<Option<GitOid>, CarryError> {
    let Some(stash) = snapshot.stash else {
        return Ok(None);
    };
    let parent = format!("{stash}^1");
    let result = git(
        source,
        &["rev-parse", "--verify", &parent],
        env,
        executor,
        cancel,
        timeout,
    )?;
    if result.timed_out {
        return Err(CarryError::TimedOut {
            operation: "rev-parse",
        });
    }
    if !result.ok || result.incomplete {
        return Err(CarryError::Snapshot(result.stderr));
    }
    let value = result.stdout.trim();
    Ok(Some(
        GitOid::parse(value).map_err(|_| CarryError::InvalidOid(value.into()))?,
    ))
}

/// Read the first parent of a carry commit for the post-commit fence.
pub fn commit_parent(
    worktree: &Path,
    commit: GitOid,
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<GitOid, CarryError> {
    let revision = format!("{commit}^1");
    let result = git(
        worktree,
        &["rev-parse", "--verify", &revision],
        env,
        executor,
        cancel,
        timeout,
    )?;
    if result.timed_out {
        return Err(CarryError::TimedOut {
            operation: "rev-parse",
        });
    }
    if !result.ok || result.incomplete {
        return Err(CarryError::Commit(result.stderr));
    }
    let value = result.stdout.trim();
    GitOid::parse(value).map_err(|_| CarryError::InvalidOid(value.into()))
}

/// Refuse tracked or non-ignored uncommitted changes in a target checkout.
pub fn ensure_clean(
    target: &Path,
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<(), CarryError> {
    let status = git(
        target,
        &["status", "--porcelain=v1", "-z"],
        env,
        executor,
        cancel,
        timeout,
    )?;
    if status.timed_out {
        return Err(CarryError::TimedOut {
            operation: "status",
        });
    }
    if !status.ok || status.incomplete || !status.stdout.is_empty() {
        return Err(CarryError::Apply("target worktree is not clean".into()));
    }
    Ok(())
}

fn check_collisions(
    target: &Path,
    paths: &[String],
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<(), CarryError> {
    use std::{collections::BTreeSet, path::Component};
    let specs = paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect::<Vec<_>>();
    let mut args = vec!["ls-files", "--cached", "-z", "--"];
    args.extend(specs.iter().map(String::as_str));
    let tracked = git(target, &args, env, executor, cancel, timeout)?;
    if tracked.timed_out {
        return Err(CarryError::TimedOut {
            operation: "ls-files",
        });
    }
    if !tracked.ok || tracked.incomplete {
        return Err(CarryError::Apply(tracked.stderr));
    }
    let tracked = tracked
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect::<BTreeSet<_>>();
    for path in paths {
        let mut cursor = target.to_owned();
        let mut components = Path::new(path).components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(CarryError::Apply(format!("invalid carried path: {path}")));
            };
            cursor.push(name);
            match std::fs::symlink_metadata(&cursor) {
                Ok(metadata)
                    if components.peek().is_some()
                        && (metadata.file_type().is_symlink() || !metadata.is_dir()) =>
                {
                    return Err(CarryError::Apply(format!(
                        "carried path has a non-directory parent: {path}"
                    )));
                }
                Ok(_) if components.peek().is_none() && !tracked.contains(path.as_str()) => {
                    return Err(CarryError::Apply(format!(
                        "carried path already exists in target: {path}"
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(CarryError::Apply(error.to_string())),
            }
        }
    }
    Ok(())
}

/// Apply a snapshot in a clean `target` worktree and commit it as the first
/// writer commit. Returns the new commit id, or `None` when nothing was
/// carried.
pub fn apply(
    target: &Path,
    snapshot: &CarrySnapshot,
    env: &[(OsString, OsString)],
    executor: &dyn Executor,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<Option<GitOid>, CarryError> {
    if snapshot.is_empty() {
        return Ok(None);
    }
    if snapshot.paths.is_empty() {
        return Err(CarryError::Apply(
            "snapshot has no committable paths".into(),
        ));
    }
    ensure_clean(target, env, executor, cancel, timeout)?;
    check_collisions(target, &snapshot.paths, env, executor, cancel, timeout)?;
    if let Some(stash) = &snapshot.stash {
        let oid = stash.to_string();
        let applied = git(
            target,
            &["stash", "apply", &oid],
            env,
            executor,
            cancel,
            timeout,
        )?;
        if applied.timed_out {
            return Err(CarryError::TimedOut {
                operation: "stash apply",
            });
        }
        if !applied.ok {
            return Err(CarryError::Apply(applied.stderr));
        }
    }
    for file in &snapshot.untracked {
        restore_untracked(target, file)?;
    }
    let pathspecs = snapshot
        .paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect::<Vec<_>>();
    let mut add = vec!["add", "-A", "--"];
    add.extend(pathspecs.iter().map(String::as_str));
    let staged = git(target, &add, env, executor, cancel, timeout)?;
    if staged.timed_out {
        return Err(CarryError::TimedOut { operation: "add" });
    }
    if !staged.ok {
        return Err(CarryError::Stage(staged.stderr));
    }
    let mut commit = vec!["commit", "-qm", "wip: carried from chair", "--only", "--"];
    commit.extend(pathspecs.iter().map(String::as_str));
    let committed = git(target, &commit, env, executor, cancel, timeout)?;
    if committed.timed_out {
        return Err(CarryError::TimedOut {
            operation: "commit",
        });
    }
    if !committed.ok {
        return Err(CarryError::Commit(committed.stderr));
    }
    let head = git(
        target,
        &["rev-parse", "HEAD"],
        env,
        executor,
        cancel,
        timeout,
    )?;
    let oid = head.stdout.trim();
    Ok(Some(
        GitOid::parse(oid).map_err(|_| CarryError::InvalidOid(oid.to_owned()))?,
    ))
}

/// Path of a carried snapshot's private index, kept beside the worktree so a
/// crash cannot leave a stale file in the chair's tree.
pub(crate) fn private_index_path(git_dir: &Path, tag: &str) -> PathBuf {
    git_dir.join(format!("provenant-carry-{tag}.index"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::SystemExecutor;
    use std::fs;

    fn test_env() -> Vec<(OsString, OsString)> {
        let mut env = provenant_os::fixture::git_env();
        if let Some(git) = provenant_os::git::resolve_git_executable(None) {
            env.push((OsString::from("PROVENANT_GIT"), git.into_os_string()));
        }
        env
    }

    fn git_at(cwd: &Path, args: &[&str]) -> String {
        let env = test_env();
        let executor = SystemExecutor::new(4).expect("executor");
        let cancel = CancelToken::new();
        let out = git(cwd, args, &env, &executor, &cancel, CARRY_TIMEOUT).expect("git");
        assert!(out.ok, "git {} failed: {}", args.join(" "), out.stderr);
        out.stdout.trim().to_owned()
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        source: PathBuf,
        target: PathBuf,
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let temp = tempfile::tempdir()?;
            let root = temp.path();
            let source = root.join("source");
            fs::create_dir(&source)?;
            git_at(&source, &["init", "-q"]);
            fs::write(source.join("tracked.txt"), "base\n")?;
            git_at(&source, &["add", "tracked.txt"]);
            git_at(&source, &["commit", "-qm", "initial"]);
            let target = root.join("target");
            git_at(
                &source,
                &[
                    "worktree",
                    "add",
                    "-qb",
                    "writer",
                    target.to_str().ok_or("path")?,
                ],
            );
            Ok(Self {
                _temp: temp,
                source,
                target,
            })
        }

        fn edit(&self) -> Result<(), Box<dyn std::error::Error>> {
            fs::write(self.source.join("tracked.txt"), "edited\n")?;
            fs::write(self.source.join("untracked.txt"), "new\n")?;
            Ok(())
        }
    }

    #[test]
    fn carry_never_mutates_chair_index() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        fixture.edit()?;
        let index = fixture.source.join(".git/index");
        let before_index = fs::read(&index)?;
        let before_status = git_at(&fixture.source, &["status", "--porcelain=v1"]);
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let _ = snapshot(
            &fixture.source,
            CarryMode::All,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        assert_eq!(fs::read(&index)?, before_index);
        assert_eq!(
            git_at(&fixture.source, &["status", "--porcelain=v1"]),
            before_status
        );
        Ok(())
    }

    #[test]
    fn off_mode_is_a_pure_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        fixture.edit()?;
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let snapshot = snapshot(
            &fixture.source,
            CarryMode::Off,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        assert!(snapshot.is_empty());
        assert_eq!(
            git_at(
                &fixture.target,
                &["status", "--porcelain=v1", "--untracked-files=no"]
            ),
            ""
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn all_carries_symlinks_and_executable_files_without_changing_source()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new()?;
        fixture.edit()?;
        fs::create_dir(fixture.source.join("new"))?;
        fs::write(fixture.source.join("new/script"), "#!/bin/sh\n")?;
        fs::set_permissions(
            fixture.source.join("new/script"),
            fs::Permissions::from_mode(0o755),
        )?;
        symlink("../tracked.txt", fixture.source.join("new/link"))?;
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let before = fs::read(fixture.source.join(".git/index"))?;
        let captured = snapshot(
            &fixture.source,
            CarryMode::All,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        assert!(captured.paths.contains(&"new/link".to_string()));
        assert!(captured.paths.contains(&"new/script".to_string()));
        apply(
            &fixture.target,
            &captured,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        assert_eq!(
            fs::read_link(fixture.target.join("new/link"))?,
            PathBuf::from("../tracked.txt")
        );
        assert_ne!(
            fs::metadata(fixture.target.join("new/script"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_eq!(
            git_at(&fixture.target, &["log", "-1", "--format=%s"]),
            "wip: carried from chair"
        );
        assert_eq!(fs::read(fixture.source.join(".git/index"))?, before);
        assert_eq!(fs::read(fixture.source.join("tracked.txt"))?, b"edited\n");
        Ok(())
    }

    #[test]
    fn carry_refuses_dirty_target_before_applying() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        fixture.edit()?;
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let captured = snapshot(
            &fixture.source,
            CarryMode::Tracked,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        fs::write(fixture.target.join("tracked.txt"), "other writer\n")?;
        assert!(
            apply(
                &fixture.target,
                &captured,
                &env,
                &executor,
                &cancel,
                CARRY_TIMEOUT
            )
            .is_err()
        );
        assert_eq!(
            fs::read(fixture.target.join("tracked.txt"))?,
            b"other writer\n"
        );
        assert_eq!(fs::read(fixture.source.join("tracked.txt"))?, b"edited\n");
        Ok(())
    }

    #[test]
    fn carry_does_not_overwrite_ignored_target_file() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.source.join("new.txt"), "from source\n")?;
        fs::write(fixture.target.join("new.txt"), "target private data\n")?;
        let exclude = fixture.target.join(git_at(
            &fixture.target,
            &["rev-parse", "--git-path", "info/exclude"],
        ));
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let captured = snapshot(
            &fixture.source,
            CarryMode::All,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        // Linked worktrees share the exclude file; ignore only after snapshot.
        fs::write(exclude, "new.txt\n")?;
        assert!(
            apply(
                &fixture.target,
                &captured,
                &env,
                &executor,
                &cancel,
                CARRY_TIMEOUT
            )
            .is_err()
        );
        assert_eq!(
            fs::read(fixture.target.join("new.txt"))?,
            b"target private data\n"
        );
        assert_eq!(fs::read(fixture.source.join("new.txt"))?, b"from source\n");
        Ok(())
    }

    #[test]
    fn saved_rename_reports_both_paths() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        git_at(&fixture.source, &["mv", "tracked.txt", "renamed.txt"]);
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let captured = snapshot(
            &fixture.source,
            CarryMode::Tracked,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        assert_eq!(captured.paths, vec!["renamed.txt", "tracked.txt"]);
        Ok(())
    }

    #[test]
    fn tracked_add_does_not_overwrite_ignored_target_file() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = Fixture::new()?;
        fs::write(fixture.source.join("new.txt"), "from source\n")?;
        git_at(&fixture.source, &["add", "new.txt"]);
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let captured = snapshot(
            &fixture.source,
            CarryMode::Tracked,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        fs::write(fixture.target.join("new.txt"), "target private data\n")?;
        let exclude = fixture.target.join(git_at(
            &fixture.target,
            &["rev-parse", "--git-path", "info/exclude"],
        ));
        fs::write(exclude, "new.txt\n")?;
        assert!(
            apply(
                &fixture.target,
                &captured,
                &env,
                &executor,
                &cancel,
                CARRY_TIMEOUT
            )
            .is_err()
        );
        assert_eq!(
            fs::read(fixture.target.join("new.txt"))?,
            b"target private data\n"
        );
        assert_eq!(fs::read(fixture.source.join("new.txt"))?, b"from source\n");
        Ok(())
    }

    #[test]
    fn staged_and_unstaged_edits_leave_the_original_index_untouched()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        fs::write(fixture.source.join("tracked.txt"), "staged\n")?;
        git_at(&fixture.source, &["add", "tracked.txt"]);
        fs::write(fixture.source.join("tracked.txt"), "working copy\n")?;
        let before = fs::read(fixture.source.join(".git/index"))?;
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        let captured = snapshot(
            &fixture.source,
            CarryMode::Tracked,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        apply(
            &fixture.target,
            &captured,
            &env,
            &executor,
            &cancel,
            CARRY_TIMEOUT,
        )?;
        assert_eq!(fs::read(fixture.source.join(".git/index"))?, before);
        assert_eq!(git_at(&fixture.source, &["show", ":tracked.txt"]), "staged");
        assert_eq!(
            fs::read(fixture.source.join("tracked.txt"))?,
            b"working copy\n"
        );
        assert_eq!(
            git_at(&fixture.target, &["show", "HEAD:tracked.txt"]),
            "working copy"
        );
        Ok(())
    }

    #[test]
    fn invalid_snapshot_cannot_write_outside_the_target() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = Fixture::new()?;
        let malicious = CarrySnapshot {
            stash: None,
            paths: vec!["../source/tracked.txt".into()],
            untracked: vec![CarriedFile {
                path: "../source/tracked.txt".into(),
                content: CarriedContent::File {
                    bytes: b"overwritten".to_vec(),
                    executable: false,
                },
            }],
            mode: CarryMode::All,
        };
        let env = test_env();
        let executor = SystemExecutor::new(4)?;
        let cancel = CancelToken::new();
        assert!(
            apply(
                &fixture.target,
                &malicious,
                &env,
                &executor,
                &cancel,
                CARRY_TIMEOUT
            )
            .is_err()
        );
        assert_eq!(fs::read(fixture.source.join("tracked.txt"))?, b"base\n");
        Ok(())
    }
}
