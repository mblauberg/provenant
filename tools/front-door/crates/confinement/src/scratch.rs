//! Project-local, untracked operator scratch. Never edits a tracked ignore file.
use crate::repo::VerifiedRepo;
use crate::worktrees::WorktreeError;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use std::{ffi::OsStr, path::PathBuf};

/// Create (if needed) and exclude `.provenant/scratch`, the only project scratch
/// root. A legacy `.agent-run` is never probed, created, moved or deleted.
pub fn ensure(
    repo: &VerifiedRepo,
    clock: &dyn Clock,
    cancel: &CancelToken,
) -> Result<PathBuf, WorktreeError> {
    ensure_inner(repo, clock, cancel, || {})
}

fn ensure_inner(
    repo: &VerifiedRepo,
    clock: &dyn Clock,
    cancel: &CancelToken,
    before_lock: impl FnOnce(),
) -> Result<PathBuf, WorktreeError> {
    fn child(parent: &Directory, name: &str) -> Result<Directory, WorktreeError> {
        match parent.mkdir(OsStr::new(name), 0o700) {
            Ok(()) => parent.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        Ok(parent.open_verified_directory(OsStr::new(name))?)
    }
    repo.revalidate()?;
    let relative = ".provenant/scratch";
    let parent = child(repo.root_dir(), ".provenant")?;
    let scratch = child(&parent, "scratch")?;
    let info = child(repo.common_dir(), "info")?;
    before_lock();
    let _lock = info.lock(
        OsStr::new("provenant-exclude.lock"),
        LockMode::Exclusive,
        WaitBudget::cli(clock.mono_now(), 30).map_err(|_| WorktreeError::InvalidRequest)?,
        cancel,
        clock,
    )?;
    let mut bytes = match crate::stable_read::read_regular(&info, OsStr::new("exclude"), 1_048_576)
    {
        Ok((bytes, _)) => bytes,
        Err(crate::stable_read::ReadError::Filesystem(error)) if error.is_not_found() => Vec::new(),
        Err(_) => return Err(WorktreeError::InvalidOutput),
    };
    let pattern = format!("/{relative}/");
    if !bytes
        .split(|byte| *byte == b'\n')
        .any(|line| line == pattern.as_bytes())
    {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(format!("{pattern}\n").as_bytes());
        info.atomic_write(
            OsStr::new("exclude"),
            ".exclude-",
            &bytes,
            PublishMode::Replace,
        )?
        .require_durable()?;
    }
    child(&scratch, "graphs")?;
    repo.revalidate()?;
    Ok(repo.root_path().join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;
    use provenant_os::clock::SystemClock;

    #[test]
    fn concurrent_scratch_ensure_succeeds_for_both_callers()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        std::thread::scope(|scope| {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let repo = &fixture.repo;
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        ensure(repo, &SystemClock, &CancelToken::new())
                    })
                })
                .collect();
            barrier.wait();
            for worker in workers {
                assert_eq!(
                    worker.join().unwrap()?,
                    fixture.root.join(".provenant/scratch")
                );
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        })?;
        Ok(())
    }

    #[test]
    fn concurrent_scratch_waits_for_the_exclude_lock() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        ensure(&fixture.repo, &SystemClock, &CancelToken::new())?;
        let info = Directory::open_anchor(&fixture.repo.common_git_dir().join("info"))?;
        let held = info.lock(
            OsStr::new("provenant-exclude.lock"),
            LockMode::Exclusive,
            WaitBudget::nonblocking(SystemClock.mono_now()),
            &CancelToken::new(),
            &SystemClock,
        )?;
        std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let repo = &fixture.repo;
            let worker = scope.spawn(move || {
                ensure_inner(repo, &SystemClock, &CancelToken::new(), || {
                    sender.send(()).unwrap();
                })
            });
            receiver.recv().unwrap();
            assert!(
                !worker.is_finished(),
                "scratch contender must wait for exclusion"
            );
            assert!(
                !fixture.root.join(".provenant/scratch/locks").exists(),
                "scratch must not split its lock while the exclude lock is held"
            );
            drop(held);
            worker.join().unwrap()
        })?;
        Ok(())
    }

    #[test]
    fn scratch_is_local_excluded_idempotent_and_refuses_links()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let scratch = ensure(&fixture.repo, &SystemClock, &CancelToken::new())?;
        assert_eq!(scratch, fixture.root.join(".provenant/scratch"));
        assert!(scratch.join("graphs").is_dir());
        let exclude = std::fs::read(fixture.repo.common_git_dir().join("info/exclude"))?;
        ensure(&fixture.repo, &SystemClock, &CancelToken::new())?;
        assert_eq!(
            std::fs::read(fixture.repo.common_git_dir().join("info/exclude"))?,
            exclude
        );
        assert!(!fixture.root.join(".gitignore").exists());
        std::fs::create_dir(fixture.root.join(".agent-run"))?;
        assert_eq!(
            ensure(&fixture.repo, &SystemClock, &CancelToken::new())?,
            fixture.root.join(".provenant/scratch")
        );
        std::fs::remove_dir_all(&scratch)?;
        let elsewhere = fixture.root.join("elsewhere");
        std::fs::create_dir(&elsewhere)?;
        std::os::unix::fs::symlink(&elsewhere, &scratch)?;
        assert!(ensure(&fixture.repo, &SystemClock, &CancelToken::new()).is_err());
        Ok(())
    }

    #[test]
    fn scratch_ignores_legacy_root_and_never_touches_it() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::PermissionsExt;
        // Path, mode and file bytes for every entry under the legacy root.
        fn snapshot(root: &std::path::Path) -> std::io::Result<Vec<(PathBuf, u32, Vec<u8>)>> {
            let mut entries = Vec::new();
            let mut pending = vec![root.to_path_buf()];
            while let Some(path) = pending.pop() {
                let meta = std::fs::symlink_metadata(&path)?;
                let bytes = if meta.is_file() {
                    std::fs::read(&path)?
                } else {
                    Vec::new()
                };
                entries.push((path.clone(), meta.permissions().mode(), bytes));
                if meta.is_dir() {
                    for entry in std::fs::read_dir(&path)? {
                        pending.push(entry?.path());
                    }
                }
            }
            entries.sort();
            Ok(entries)
        }
        let fixture = Fixture::new()?;
        let legacy = fixture.root.join(".agent-run");
        std::fs::create_dir_all(legacy.join("graphs"))?;
        std::fs::write(legacy.join("graphs/old-plan.json"), b"{\"legacy\":true}")?;
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(
            legacy.join("graphs"),
            std::fs::Permissions::from_mode(0o755),
        )?;
        let before = snapshot(&legacy)?;
        let scratch = ensure(&fixture.repo, &SystemClock, &CancelToken::new())?;
        assert_eq!(scratch, fixture.root.join(".provenant/scratch"));
        assert_eq!(
            std::fs::metadata(scratch.join("graphs"))?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(snapshot(&legacy)?, before);
        Ok(())
    }
}
