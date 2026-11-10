//! Keep a bound worktree's files in step with its branch.
//!
//! A worktree-write lane edits the bound worktree's files directly while its
//! commits live in a private object store. Publication moves the branch, HEAD
//! and index together; until then only the files have moved. `converge` moves
//! the files between two commits, and only the files that provably hold the
//! lane's own content, so edits made by anyone else are never overwritten.
use super::{Execution, args, execute_bounded, invalid};
use crate::Result;
use provenant_confinement::repo::VerifiedRepo;
use provenant_core::GitOid;
use provenant_os::fs::Directory;
use std::{
    ffi::OsStr,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

const REGULAR: &[u8] = b"100644";
const EXECUTABLE: &[u8] = b"100755";
const ABSENT: &[u8] = b"000000";

/// Where Git reads the objects of the two commits from.
pub(crate) struct Stores<'a> {
    /// Git directory used to read the commits.
    pub(crate) git_dir: &'a Path,
    /// The lane's private object directory, when the commits are not yet
    /// in the host store.
    pub(crate) objects: Option<&'a Path>,
}

struct Change {
    old_mode: Vec<u8>,
    new_mode: Vec<u8>,
    old: String,
    new: String,
    path: Vec<u8>,
}

/// What `converge` did not touch.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Converged {
    pub(crate) moved: usize,
    /// Paths holding neither commit's content; left exactly as found.
    pub(crate) kept: Vec<String>,
}

fn changes(
    git: &Path,
    repo: &VerifiedRepo,
    stores: &Stores<'_>,
    from: GitOid,
    to: GitOid,
) -> Result<Vec<Change>> {
    let raw = execute_bounded(
        git,
        stores.git_dir,
        repo.root_path(),
        &args(&[
            "diff-tree",
            "-r",
            "--raw",
            "-z",
            "--no-renames",
            "--no-abbrev",
            &from.to_string(),
            &to.to_string(),
        ]),
        None,
        Execution {
            object_directory: stores.objects,
            alternate_directory: Some(&repo.common_git_dir().join("objects")),
            ..Execution::default()
        },
    )?;
    let mut fields = raw.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    let mut out = Vec::new();
    while let Some(meta) = fields.next() {
        let path = fields.next().ok_or_else(invalid)?;
        let meta = meta.strip_prefix(b":").ok_or_else(invalid)?;
        let parts: Vec<&[u8]> = meta.split(|byte| *byte == b' ').collect();
        let [old_mode, new_mode, old, new, _status] = parts.as_slice() else {
            return Err(invalid());
        };
        let text = |bytes: &[u8]| -> Result<String> {
            Ok(std::str::from_utf8(bytes)
                .map_err(|_| invalid())?
                .to_owned())
        };
        out.push(Change {
            old_mode: old_mode.to_vec(),
            new_mode: new_mode.to_vec(),
            old: text(old)?,
            new: text(new)?,
            path: path.to_vec(),
        });
    }
    Ok(out)
}

fn blob_hashes(
    git: &Path,
    repo: &VerifiedRepo,
    paths: &[&Change],
) -> Result<std::collections::HashMap<Vec<u8>, String>> {
    let mut input = Vec::new();
    let mut listed = Vec::new();
    for change in paths {
        // The stdin protocol is newline separated.
        if change.path.contains(&b'\n') {
            continue;
        }
        input.extend_from_slice(&change.path);
        input.push(b'\n');
        listed.push(change.path.clone());
    }
    if listed.is_empty() {
        return Ok(Default::default());
    }
    let out = execute_bounded(
        git,
        repo.private_git_dir(),
        repo.root_path(),
        &args(&["hash-object", "--no-filters", "--stdin-paths"]),
        None,
        Execution {
            input: Some(&input),
            ..Execution::default()
        },
    )?;
    let hashes: Vec<&[u8]> = out.split(|byte| *byte == b'\n').collect();
    if hashes.len() < listed.len() {
        return Err(invalid());
    }
    Ok(listed
        .into_iter()
        .zip(hashes)
        .map(|(path, hash)| (path, String::from_utf8_lossy(hash).into_owned()))
        .collect())
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Side {
    Absent,
    Matches,
    Other,
}

fn mode_of(mode: &[u8]) -> Option<bool> {
    match mode {
        REGULAR => Some(false),
        EXECUTABLE => Some(true),
        _ => None,
    }
}

/// Classify what the worktree holds at a path against one side of a change.
fn side(
    root: &Path,
    path: &[u8],
    mode: &[u8],
    oid: &str,
    hashes: &std::collections::HashMap<Vec<u8>, String>,
) -> Side {
    let full = root.join(OsStr::from_bytes(path));
    let metadata = match std::fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if mode == ABSENT {
                Side::Matches
            } else {
                Side::Absent
            };
        }
        Err(_) => return Side::Other,
    };
    // Only regular files are compared. Symlinks, submodules and directories
    // are left for the owner of the tree.
    let Some(executable) = mode_of(mode) else {
        return Side::Other;
    };
    if !metadata.file_type().is_file()
        || (metadata.permissions().mode() & 0o111 != 0) != executable
    {
        return Side::Other;
    }
    if hashes.get(path).is_some_and(|hash| hash == oid) {
        Side::Matches
    } else {
        Side::Other
    }
}

/// Move the files that a change from `from` to `to` touched, but only where
/// the worktree still holds `from`'s content. Files already at `to` are done;
/// any other content is kept and named in the result.
pub(crate) fn converge(
    git: &Path,
    repo: &VerifiedRepo,
    stores: &Stores<'_>,
    from: GitOid,
    to: GitOid,
) -> Result<Converged> {
    let all = changes(git, repo, stores, from, to)?;
    // A path that is a symlink, submodule or a directory on either side is
    // not ours to move.
    let regular: Vec<&Change> = all
        .iter()
        .filter(|change| {
            [&change.old_mode, &change.new_mode]
                .iter()
                .all(|mode| mode.as_slice() == ABSENT || mode_of(mode).is_some())
        })
        .collect();
    let mut result = Converged::default();
    for change in &all {
        if !regular.iter().any(|kept| std::ptr::eq(*kept, change)) {
            result
                .kept
                .push(String::from_utf8_lossy(&change.path).into_owned());
        }
    }
    let hashes = blob_hashes(git, repo, &regular)?;
    let root = repo.root_path();
    let mut write: Vec<u8> = Vec::new();
    let mut remove: Vec<&Change> = Vec::new();
    for change in &regular {
        let now_old = side(root, &change.path, &change.old_mode, &change.old, &hashes);
        let now_new = side(root, &change.path, &change.new_mode, &change.new, &hashes);
        if now_new == Side::Matches {
            continue;
        }
        if now_old == Side::Matches {
            if change.new_mode == ABSENT {
                remove.push(change);
            } else {
                write.extend_from_slice(&change.path);
                write.push(0);
            }
            result.moved += 1;
        } else {
            result
                .kept
                .push(String::from_utf8_lossy(&change.path).into_owned());
        }
    }
    for change in remove {
        let full = root.join(OsStr::from_bytes(&change.path));
        match std::fs::remove_file(&full) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !write.is_empty() {
        let index = scratch_index(repo);
        let outcome = (|| -> Result<()> {
            let shared = repo.common_git_dir().join("objects");
            let settings = || Execution {
                object_directory: stores.objects,
                alternate_directory: Some(&shared),
                index_file: Some(&index),
                ..Execution::default()
            };
            execute_bounded(
                git,
                stores.git_dir,
                root,
                &args(&["read-tree", &to.to_string()]),
                None,
                settings(),
            )?;
            execute_bounded(
                git,
                stores.git_dir,
                root,
                &args(&["checkout-index", "-f", "-z", "--stdin"]),
                None,
                Execution {
                    input: Some(&write),
                    ..settings()
                },
            )?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&index);
        outcome?;
    }
    Ok(result)
}

fn scratch_index(repo: &VerifiedRepo) -> PathBuf {
    repo.private_git_dir().join(".lane-converge-index")
}

/// Whether a prior failed settlement moved this attempt's files back.
pub(crate) fn reverted(attempt: &Directory) -> Result<bool> {
    Ok(
        crate::artifacts::read_sealed(attempt, "worktree-reverted.json", crate::RECORD_LIMIT)?
            .is_some(),
    )
}

pub(crate) fn seal_reverted(attempt: &Directory, tip: GitOid) -> Result<()> {
    crate::artifacts::seal(
        attempt,
        "worktree-reverted.json",
        &crate::db::bytes(&serde_json::json!({"schema_version":1,"tip":tip}))?,
        crate::RECORD_LIMIT,
    )
}
