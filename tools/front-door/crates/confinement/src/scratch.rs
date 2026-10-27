//! Project-local, untracked operator scratch. Never edits a tracked ignore file.
use crate::repo::VerifiedRepo;
use crate::worktrees::WorktreeError;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use std::{ffi::OsStr, path::PathBuf};

pub fn ensure(
    repo: &VerifiedRepo,
    clock: &dyn Clock,
    cancel: &CancelToken,
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
    let legacy = match repo.root_dir().metadata_at(OsStr::new(".agent-run"), false) {
        Ok(_) => true,
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.into()),
    };
    let (relative, scratch) = if legacy {
        (".agent-run", child(repo.root_dir(), ".agent-run")?)
    } else {
        let parent = child(repo.root_dir(), ".provenant")?;
        (".provenant/scratch", child(&parent, "scratch")?)
    };
    let info = child(repo.common_dir(), "info")?;
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
        .require_durable()
        .map_err(|_| WorktreeError::RegistrationUnconfirmed)?;
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
    #[allow(clippy::disallowed_methods, reason = "real OS lock contention fixture")]
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
                sender.send(()).unwrap();
                ensure(repo, &SystemClock, &CancelToken::new())
            });
            receiver.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(
                !worker.is_finished(),
                "scratch contender must wait for exclusion"
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
            fixture.root.join(".agent-run")
        );
        std::fs::remove_dir_all(fixture.root.join(".agent-run"))?;
        std::os::unix::fs::symlink(&scratch, fixture.root.join(".agent-run"))?;
        assert!(ensure(&fixture.repo, &SystemClock, &CancelToken::new()).is_err());
        Ok(())
    }
}
