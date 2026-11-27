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
    time::Duration,
};

/// Default bound for each Git command in a carry operation.
pub const CARRY_TIMEOUT: Duration = Duration::from_secs(30);

/// One untracked, non-ignored file carried in `All` mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedFile {
    /// Worktree-relative path as reported by `git ls-files`.
    pub path: String,
    pub bytes: Vec<u8>,
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
}

pub(crate) fn git(
    cwd: &Path,
    args: &[&str],
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
    options.cancel = Some(cancel);
    let outcome = executor.execute(options)?;
    Ok(GitOutput {
        ok: outcome.termination == Termination::Exit(0),
        stdout: String::from_utf8_lossy(&outcome.stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&outcome.stderr.bytes).into_owned(),
        timed_out: outcome.timed_out,
    })
}

fn porcelain_paths(output: &str, mode: CarryMode) -> Vec<String> {
    output
        .split('\0')
        .filter_map(|entry| {
            let path = entry.get(3..)?;
            if path.is_empty() {
                return None;
            }
            let untracked = entry.starts_with("??");
            if untracked && mode == CarryMode::Tracked {
                return None;
            }
            // A rename is reported as `old -> new`; the destination is the
            // path that carries the edit.
            Some(
                path.split_once(" -> ")
                    .map_or_else(|| path.to_owned(), |(_, new)| new.to_owned()),
            )
        })
        .collect()
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
    let status_args = if mode == CarryMode::All {
        vec!["status", "--porcelain=v1", "-z"]
    } else {
        vec!["status", "--porcelain=v1", "-z", "--untracked-files=no"]
    };
    let status = git(source, &status_args, env, executor, cancel, timeout)?;
    if status.timed_out {
        return Err(CarryError::TimedOut {
            operation: "status",
        });
    }
    if !status.ok {
        return Err(CarryError::Snapshot(status.stderr));
    }
    let paths = porcelain_paths(&status.stdout, mode);

    // `git stash create` writes objects only; it never touches the source
    // worktree or index. Untracked files are carried separately because
    // `git stash apply` has no flag to restore them.
    let created = git(
        source,
        &["stash", "create", "provenant carry"],
        env,
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

    let untracked = if mode == CarryMode::All {
        let listed = git(
            source,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            env,
            executor,
            cancel,
            timeout,
        )?;
        if listed.timed_out {
            return Err(CarryError::TimedOut {
                operation: "ls-files",
            });
        }
        if !listed.ok {
            return Err(CarryError::Snapshot(listed.stderr));
        }
        listed
            .stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| -> Result<CarriedFile, CarryError> {
                let bytes = std::fs::read(source.join(path))
                    .map_err(|error| CarryError::Snapshot(error.to_string()))?;
                Ok(CarriedFile {
                    path: path.to_owned(),
                    bytes,
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    Ok(CarrySnapshot {
        stash,
        paths,
        untracked,
        mode,
    })
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
        let destination = target.join(&file.path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| CarryError::Apply(error.to_string()))?;
        }
        std::fs::write(&destination, &file.bytes)
            .map_err(|error| CarryError::Apply(error.to_string()))?;
    }
    let staged = git(target, &["add", "-A"], env, executor, cancel, timeout)?;
    if staged.timed_out {
        return Err(CarryError::TimedOut { operation: "add" });
    }
    if !staged.ok {
        return Err(CarryError::Stage(staged.stderr));
    }
    let committed = git(
        target,
        &["commit", "-qm", "wip: carried from chair"],
        env,
        executor,
        cancel,
        timeout,
    )?;
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
}
