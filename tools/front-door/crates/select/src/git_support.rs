//! Git and `cargo metadata` boundaries used to build the two package graphs.

use crate::{error::Error, metadata};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

// Named constants keep the process-creation sites explicit. The front-door
// selector is its own custody boundary for these read-only subprocesses.
const GIT_PROGRAM: &str = "git";
const CARGO_PROGRAM: &str = "cargo";

/// Builds a child command with the ambient worktree bindings removed, so a lane
/// binding cannot redirect these read-only commands to a different repository.
#[allow(clippy::disallowed_methods)]
fn command(program: &str, directory: &Path) -> Command {
    let mut command = Command::new(program);
    command.current_dir(directory);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
}

/// Runs git in `workspace`, returning stdout on a zero exit.
fn run_git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>, Error> {
    let output = command(GIT_PROGRAM, workspace)
        .args(args)
        .output()
        .map_err(|source| Error::GitSpawn {
            workspace: workspace.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::Git {
            workspace: workspace.to_path_buf(),
            args: args.join(" "),
            detail: if output.stderr.is_empty() {
                format!("exit status {}", output.status)
            } else {
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            },
        });
    }
    Ok(output.stdout)
}

/// The NUL-separated paths changed between `base` and `head`.
pub(super) fn changed_paths(
    workspace: &Path,
    base: &str,
    head: &str,
    object_dir: Option<&Path>,
) -> Result<Vec<PathBuf>, Error> {
    let args = ["diff", "--name-only", "-z", base, head, "--"];
    let stdout = if let Some(object_dir) = object_dir {
        let output = command(GIT_PROGRAM, workspace)
            .env("GIT_OBJECT_DIRECTORY", object_dir)
            .args(args)
            .output()
            .map_err(|source| Error::GitSpawn {
                workspace: workspace.to_path_buf(),
                source,
            })?;
        if !output.status.success() {
            return Err(Error::Git {
                workspace: workspace.to_path_buf(),
                args: args.join(" "),
                detail: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        output.stdout
    } else {
        run_git(workspace, &args)?
    };
    stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            std::str::from_utf8(part)
                .map(PathBuf::from)
                .map_err(|_| Error::NonUtf8Path(String::from_utf8_lossy(part).into_owned()))
        })
        .collect()
}

/// A detached, temporary checkout of the base revision.
///
/// Creating a worktree instead of altering the caller's checkout keeps the
/// selector read-only from the caller's point of view. Dropping it removes the
/// worktree registration before deleting the temporary directory.
pub(super) struct BaseCheckout {
    parent: PathBuf,
    workspace: PathBuf,
    path: PathBuf,
    registered: bool,
}

impl BaseCheckout {
    pub(super) fn new(workspace: &Path, base: &str, nonce: u64) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "provenant-select-{}-{nonce}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&parent).map_err(|source| Error::GitSpawn {
            workspace: workspace.to_path_buf(),
            source,
        })?;
        let path = parent.join("base");
        let path_text = path.to_string_lossy().into_owned();
        let result = run_git(
            workspace,
            &["worktree", "add", "--detach", &path_text, base],
        );
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&parent);
            return Err(error);
        }
        Ok(Self {
            parent,
            workspace: workspace.to_path_buf(),
            path,
            registered: true,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for BaseCheckout {
    fn drop(&mut self) {
        if self.registered {
            let path_text = self.path.to_string_lossy().into_owned();
            let _ = run_git(
                &self.workspace,
                &["worktree", "remove", "--force", &path_text],
            );
            self.registered = false;
        }
        let _ = fs::remove_dir_all(&self.parent);
    }
}

/// Reads an offline, locked `cargo metadata` document for a checkout and builds
/// its selection graph. No network access is permitted.
pub(super) fn load_metadata(
    directory: &Path,
    revision: &'static str,
) -> Result<metadata::Graph, Error> {
    let output = command(CARGO_PROGRAM, directory)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--locked",
            "--manifest-path",
        ])
        .arg(directory.join("Cargo.toml"))
        .output()
        .map_err(|source| Error::Metadata {
            revision,
            detail: format!("could not spawn cargo: {source}"),
        })?;
    if !output.status.success() {
        return Err(Error::Metadata {
            revision,
            detail: if output.stderr.is_empty() {
                format!("exit status {}", output.status)
            } else {
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            },
        });
    }
    metadata::parse(&output.stdout).map_err(|detail| Error::Metadata { revision, detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_paths_reads_a_private_snapshot_without_publishing_objects() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let run = |args: &[&str]| {
            let out = provenant_test_support::git_command()
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.invalid"]);
        fs::write(repo.join("file"), "before").unwrap();
        run(&["add", "file"]);
        run(&["commit", "-qm", "base"]);
        let base = run(&["rev-parse", "HEAD"]);
        fs::write(repo.join("file"), "after").unwrap();
        let objects = tmp.path().join("objects");
        fs::create_dir_all(objects.join("info")).unwrap();
        fs::write(
            objects.join("info/alternates"),
            format!("{}\n", repo.join(".git/objects").display()),
        )
        .unwrap();
        let invoke = |args: &[&str]| {
            let out = provenant_test_support::git_command()
                .current_dir(&repo)
                .env("GIT_OBJECT_DIRECTORY", &objects)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        invoke(&["add", "file"]);
        let tree = invoke(&["write-tree"]);
        let head = invoke(&["commit-tree", &tree, "-p", &base, "-m", "snapshot"]);
        assert_eq!(
            changed_paths(&repo, &base, &head, Some(&objects)).unwrap(),
            vec![PathBuf::from("file")]
        );
        assert!(
            !provenant_test_support::git_command()
                .current_dir(repo)
                .args(["cat-file", "-e", &head])
                .status()
                .unwrap()
                .success()
        );
    }
}
