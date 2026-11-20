//! Deterministic snapshots and negotiated transfer to the build store.
use super::process::Command;
use super::{BuildError, GitBinding, InfraKind, SnapshotSource};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Output,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRef {
    pub commit: String,
    pub ref_name: String,
    pub base: Option<String>,
}
fn error(s: impl ToString) -> BuildError {
    BuildError::Infra {
        kind: InfraKind::Git,
        detail: s.to_string(),
    }
}
fn io_error(error: std::io::Error) -> BuildError {
    let kind = if matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EIO)) {
        InfraKind::Disk
    } else {
        InfraKind::Git
    };
    BuildError::Infra {
        kind,
        detail: error.to_string(),
    }
}

fn clear_git(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
}
fn checked(cmd: &mut Command) -> Result<Output, BuildError> {
    let out = cmd.output().map_err(io_error)?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr);
        let kind = if detail.contains("No space left on device")
            || detail.contains("Input/output error")
        {
            InfraKind::Disk
        } else {
            InfraKind::Git
        };
        return Err(BuildError::Infra {
            kind,
            detail: detail.into_owned(),
        });
    }
    Ok(out)
}
fn value(out: Output) -> Result<String, BuildError> {
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(error)
}
fn bound(git: &GitBinding, worktree: &Path, index: &Path) -> Command {
    // The executable comes from the caller's frozen binding, never from a
    // hard-coded program name; the environment is scrubbed below.
    let program = &git.git;
    let mut c = Command::new(program);
    clear_git(&mut c);
    c.env("GIT_DIR", &git.git_dir)
        .env("GIT_COMMON_DIR", &git.common_dir)
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_INDEX_FILE", index)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(objects) = &git.object_dir {
        c.env("GIT_OBJECT_DIRECTORY", objects);
    }
    if let Some(config) = &git.config_global {
        c.env("GIT_CONFIG_GLOBAL", config);
    }
    c.current_dir(worktree);
    c
}
fn quote(value: &OsStr) -> Result<String, BuildError> {
    let value = value
        .to_str()
        .ok_or_else(|| error("non-UTF-8 Git upload-pack path"))?;
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}
struct TempIndex(PathBuf);
impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Capture a working tree without changing its index or refs.
pub fn working_tree_commit(
    worktree: &Path,
    git: &GitBinding,
    tmp: &Path,
) -> Result<String, BuildError> {
    fs::create_dir_all(tmp).map_err(io_error)?;
    let seq = format!(
        "{}-{}-{}",
        super::ledger::now_unix_ms(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let index = TempIndex(tmp.join(format!("index-{seq}")));
    if git.index_file.exists() {
        fs::copy(&git.index_file, &index.0).map_err(io_error)?;
    } else {
        checked(bound(git, worktree, &index.0).args(["read-tree", "HEAD"]))?;
    }
    checked(bound(git, worktree, &index.0).args(["add", "-A"]))?;
    let tree = value(checked(bound(git, worktree, &index.0).arg("write-tree"))?)?;
    let head = bound(git, worktree, &index.0)
        .args(["rev-parse", "-q", "--verify", "HEAD^{commit}"])
        .output()
        .map_err(io_error)?;
    let mut command = bound(git, worktree, &index.0);
    command.arg("commit-tree").arg(tree);
    if head.status.success() {
        command.arg("-p").arg(value(head)?);
    } else if head.status.code() != Some(1) {
        return Err(error(String::from_utf8_lossy(&head.stderr)));
    }
    command
        .args(["-m", "Provenant build snapshot"])
        .env("GIT_AUTHOR_NAME", "Provenant")
        .env("GIT_AUTHOR_EMAIL", "build@provenant.invalid")
        .env("GIT_AUTHOR_DATE", "@0 +0000")
        .env("GIT_COMMITTER_NAME", "Provenant")
        .env("GIT_COMMITTER_EMAIL", "build@provenant.invalid")
        .env("GIT_COMMITTER_DATE", "@0 +0000");
    value(checked(&mut command)?)
}

/// Pin a commit source without a verify base.
pub fn create(
    source: &SnapshotSource,
    binding: Option<&GitBinding>,
    store: &Path,
    owner: &str,
) -> Result<SnapshotRef, BuildError> {
    if !matches!(source, SnapshotSource::Commit { .. }) {
        return Err(BuildError::Unsupported(
            "working-tree snapshots need the slot temporary directory".into(),
        ));
    }
    let tmp = store.parent().ok_or_else(|| error("store has no parent"))?;
    create_in_slot(source, binding, store, tmp, owner, None)
}

/// Create a slot snapshot and pin it, optionally alongside a verify base.
pub fn create_in_slot(
    source: &SnapshotSource,
    binding: Option<&GitBinding>,
    store: &Path,
    tmp: &Path,
    owner: &str,
    base: Option<&str>,
) -> Result<SnapshotRef, BuildError> {
    if owner.is_empty()
        || !owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(error("invalid snapshot owner"));
    }
    fs::create_dir_all(tmp).map_err(io_error)?;
    let seq = format!(
        "{}-{}-{}",
        super::ledger::now_unix_ms(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let ref_name = format!("refs/snapshots/{owner}/{seq}");
    let base_ref = format!("refs/bases/{owner}/{seq}");
    let (commit, source_dir, git_bin, object_dir) = match source {
        SnapshotSource::WorkingTree { worktree, git } => (
            working_tree_commit(worktree, git, tmp)?,
            git.git_dir.clone(),
            git.git.clone(),
            git.object_dir.clone(),
        ),
        SnapshotSource::Commit {
            repo_common_dir,
            commit,
        } => (
            commit.clone(),
            binding.map_or_else(|| repo_common_dir.clone(), |git| git.git_dir.clone()),
            binding.map_or_else(
                || {
                    provenant_os::git::git_executable()
                        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"))
                },
                |git| git.git.clone(),
            ),
            binding.and_then(|git| git.object_dir.clone()),
        ),
    };
    let lock_path = store.with_extension("lock");
    let _lock = super::store::lock(&lock_path, false)?;
    let mut upload = String::from("env");
    if let Some(objects) = object_dir {
        upload.push_str(&format!(
            " GIT_OBJECT_DIRECTORY={}",
            quote(objects.as_os_str())?
        ));
        let alternate_file = objects.join("info/alternates");
        let alternates = match fs::read_to_string(alternate_file) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error(error)),
        };
        if let Some(alternates) = alternates {
            let paths: Vec<_> = alternates
                .lines()
                .map(|p| {
                    let p = Path::new(p);
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        objects.join(p)
                    }
                })
                .collect();
            let joined = std::env::join_paths(paths).map_err(error)?;
            upload.push_str(&format!(
                " GIT_ALTERNATE_OBJECT_DIRECTORIES={}",
                quote(&joined)?
            ));
        }
    }
    upload.push_str(&format!(
        " {} -c uploadpack.allowAnySHA1InWant=true upload-pack",
        quote(git_bin.as_os_str())?
    ));
    let mut fetch = Command::new(&git_bin);
    clear_git(&mut fetch);
    fetch
        .arg("--git-dir")
        .arg(store)
        .args(["fetch", "--no-tags", "--no-write-fetch-head"])
        .arg(format!("--upload-pack={upload}"))
        .arg(source_dir)
        .arg(format!("{commit}:{ref_name}"));
    if let Some(base) = base {
        fetch.arg(format!("{base}:{base_ref}"));
    }
    checked(&mut fetch)?;
    Ok(SnapshotRef {
        commit,
        ref_name,
        base: base.map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identical_tree_and_parent_deduplicate_without_writing_index() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let run = |args: &[&str]| {
            let out = provenant_os::fixture::git_command()
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.invalid"]);
        fs::write(repo.join("file"), "first").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "base"]);
        fs::write(repo.join("file"), "changed").unwrap();
        let index = repo.join(".git/index");
        let before = fs::read(&index).unwrap();
        let git_dir = repo.join(".git");
        let source = SnapshotSource::WorkingTree {
            worktree: repo.clone(),
            git: GitBinding {
                git: "git".into(),
                git_dir: git_dir.clone(),
                common_dir: git_dir,
                object_dir: None,
                index_file: index.clone(),
                config_global: None,
            },
        };
        let store = super::super::store::ensure(tmp.path(), "project").unwrap();
        let a = create_in_slot(
            &source,
            None,
            &store,
            &tmp.path().join("tmp"),
            "owner",
            None,
        )
        .unwrap();
        let b = create_in_slot(
            &source,
            None,
            &store,
            &tmp.path().join("tmp"),
            "owner",
            None,
        )
        .unwrap();
        assert_eq!(a.commit, b.commit);
        assert_eq!(fs::read(index).unwrap(), before);
    }
    #[test]
    fn commit_transfer_reads_private_lane_objects_without_publishing_them() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let run = |args: &[&str]| {
            let out = provenant_os::fixture::git_command()
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
        fs::write(repo.join("file"), "private").unwrap();
        run(&["add", "file"]);
        run(&["commit", "-qm", "base"]);
        let tree = run(&["rev-parse", "HEAD^{tree}"]);
        let objects = tmp.path().join("objects");
        fs::create_dir_all(objects.join("info")).unwrap();
        fs::write(
            objects.join("info/alternates"),
            format!("{}\n", repo.join(".git/objects").display()),
        )
        .unwrap();
        let out = provenant_os::fixture::git_command()
            .current_dir(&repo)
            .env("GIT_OBJECT_DIRECTORY", &objects)
            .args(["commit-tree", &tree, "-m", "private commit"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let commit = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        assert!(
            !provenant_os::fixture::git_command()
                .current_dir(&repo)
                .args(["cat-file", "-e", &commit])
                .status()
                .unwrap()
                .success()
        );
        let binding = GitBinding {
            git: "git".into(),
            git_dir: repo.join(".git"),
            common_dir: repo.join(".git"),
            object_dir: Some(objects),
            index_file: repo.join(".git/index"),
            config_global: None,
        };
        let source = SnapshotSource::Commit {
            repo_common_dir: binding.common_dir.clone(),
            commit: commit.clone(),
        };
        let store = super::super::store::ensure(tmp.path(), "private").unwrap();
        create(&source, Some(&binding), &store, "verify").unwrap();
        let out = provenant_os::fixture::git_command()
            .arg("--git-dir")
            .arg(store)
            .args(["show", &format!("{commit}:file")])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"private");
        assert!(
            !provenant_os::fixture::git_command()
                .current_dir(repo)
                .args(["cat-file", "-e", &commit])
                .status()
                .unwrap()
                .success()
        );
    }
}
