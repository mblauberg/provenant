//! Git and `cargo metadata` boundaries used to build the two package graphs.

use crate::error::Error;
use determinator::Utf8Paths0;
use guppy::{MetadataCommand, graph::PackageGraph};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

// A named constant keeps the process-creation site explicit. The front-door
// selector is its own custody boundary for these two read-only subprocesses.
const GIT_PROGRAM: &str = "git";
const CARGO_PROGRAM: &str = "cargo";

/// Runs git in `workspace`, returning stdout on a zero exit.
#[allow(clippy::disallowed_methods)]
fn run_git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>, Error> {
    // `workspace` is authoritative: an ambient worktree binding must not
    // redirect these read-only commands to a different repository.
    let output = Command::new(GIT_PROGRAM)
        .current_dir(workspace)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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
pub(super) fn changed_paths(workspace: &Path, base: &str, head: &str) -> Result<Utf8Paths0, Error> {
    let stdout = run_git(workspace, &["diff", "--name-only", "-z", base, head, "--"])?;
    Utf8Paths0::from_bytes(stdout).map_err(|(_, error)| Error::NonUtf8Path(error.to_string()))
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

/// Builds an offline package graph for a checkout, using guppy's metadata wrapper.
pub(super) fn build_graph(directory: &Path, revision: &'static str) -> Result<PackageGraph, Error> {
    let manifest = directory.join("Cargo.toml");
    let mut command = MetadataCommand::new();
    command
        .cargo_path(CARGO_PROGRAM)
        .manifest_path(manifest)
        .current_dir(directory)
        .other_options(["--offline"]);
    command
        .build_graph()
        .map_err(|source| Error::Metadata { revision, source })
}
