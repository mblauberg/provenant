//! A short symlink to an attempt's `tmp`, so a lane's `TMPDIR` fits macOS's
//! 104-byte `sun_path` limit. Only the `TMPDIR` string uses the alias: grants,
//! staging, settlement and retention keep the real path, and the sandbox
//! evaluates the resolved link target.

use provenant_os::fs::Directory;
use sha2::{Digest as _, Sha256};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const DOMAIN: &[u8] = b"provenant-short-temporary-root-v1\0";

/// The directory holding aliases, on the platform that needs them.
#[cfg(not(test))]
fn parent() -> Option<PathBuf> {
    cfg!(target_os = "macos").then(|| PathBuf::from("/private/tmp"))
}

// Unit tests create no alias unless a test opts in, so that fixture launches
// never leave links in the shared system directory.
#[cfg(test)]
thread_local! {
    static TEST_PARENT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Opt this thread's tests in to aliases under `parent`.
#[cfg(test)]
pub(crate) fn use_parent_in_tests(parent: &Path) {
    TEST_PARENT.with(|slot| *slot.borrow_mut() = Some(parent.to_owned()));
}

#[cfg(test)]
fn parent() -> Option<PathBuf> {
    TEST_PARENT.with(|parent| parent.borrow().clone())
}

fn alias_in(parent: &Path, attempt: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    let mut hash = Sha256::new();
    hash.update(DOMAIN);
    hash.update(attempt.as_os_str().as_bytes());
    let digest = hash.finalize();
    let mut name = String::from("pv-");
    for byte in &digest[..16] {
        name.push_str(&format!("{byte:02x}"));
    }
    parent.join(name)
}

/// Whether `alias` is our own link: a symlink owned by this user to `target`.
fn is_ours(alias: &Path, target: &Path) -> Option<bool> {
    match std::fs::symlink_metadata(alias) {
        Ok(metadata) => Some(
            metadata.file_type().is_symlink()
                && metadata.uid() == provenant_os::fs::effective_uid()
                && std::fs::read_link(alias).is_ok_and(|link| link == target),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(false),
    }
}

fn ensure_in(parent: &Path, attempt: &Path) -> Option<PathBuf> {
    let alias = alias_in(parent, attempt);
    let target = attempt.join("tmp");
    if let Some(ours) = is_ours(&alias, &target) {
        return ours.then_some(alias);
    }
    match std::os::unix::fs::symlink(&target, &alias) {
        Ok(()) => Some(alias),
        // Another process may have raced us to the same link.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            is_ours(&alias, &target).unwrap_or(false).then_some(alias)
        }
        Err(_) => None,
    }
}

fn remove_in(parent: &Path, attempt: &Path) {
    let alias = alias_in(parent, attempt);
    if is_ours(&alias, &attempt.join("tmp")) == Some(true) {
        // Unlinks the symlink itself; never follows it.
        let _ = std::fs::remove_file(alias);
    }
}

/// Create the short alias for this attempt's `tmp`, which must exist. Absent
/// off macOS, and whenever the name is taken by anything but our own link: the
/// launch then uses the long real path.
pub(crate) fn ensure(attempt: &Directory) -> Option<PathBuf> {
    ensure_in(&parent()?, &attempt.current_path().ok()?)
}

/// Unlink the alias if it is our link to this attempt's `tmp`. Never
/// recursive; a missing or foreign name is left alone.
pub(crate) fn remove(attempt: &Directory) {
    if let (Some(parent), Ok(path)) = (parent(), attempt.current_path()) {
        remove_in(&parent, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Check = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn alias_resolves_to_tmp_stays_short_and_is_unlinked_only_when_ours() -> Check {
        let temp = tempfile::tempdir()?;
        let attempt = std::fs::canonicalize(temp.path())?;
        std::fs::create_dir_all(attempt.join("tmp/candidate-0"))?;
        let parent = attempt.join("short");
        std::fs::create_dir(&parent)?;

        let alias = ensure_in(&parent, &attempt).ok_or("alias created")?;
        assert_eq!(std::fs::read_link(&alias)?, attempt.join("tmp"));
        assert_eq!(
            ensure_in(&parent, &attempt),
            Some(alias.clone()),
            "idempotent"
        );
        std::fs::write(alias.join("candidate-0/x"), b"y")?;
        assert!(attempt.join("tmp/candidate-0/x").is_file());

        remove_in(&parent, &attempt);
        assert!(std::fs::symlink_metadata(&alias).is_err());
        assert!(
            attempt.join("tmp/candidate-0/x").is_file(),
            "the target survives"
        );
        remove_in(&parent, &attempt);
        Ok(())
    }

    #[test]
    fn foreign_names_fall_back_without_failing_or_being_removed() -> Check {
        let temp = tempfile::tempdir()?;
        let attempt = std::fs::canonicalize(temp.path())?;
        std::fs::create_dir_all(attempt.join("tmp"))?;
        let parent = attempt.join("short");
        std::fs::create_dir(&parent)?;
        let alias = alias_in(&parent, &attempt);

        std::fs::create_dir(&alias)?;
        assert_eq!(ensure_in(&parent, &attempt), None, "directory");
        remove_in(&parent, &attempt);
        assert!(alias.is_dir());
        std::fs::remove_dir(&alias)?;

        std::os::unix::fs::symlink(attempt.join("elsewhere"), &alias)?;
        assert_eq!(ensure_in(&parent, &attempt), None, "link elsewhere");
        remove_in(&parent, &attempt);
        assert!(std::fs::symlink_metadata(&alias).is_ok());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_alias_keeps_socket_paths_within_sun_path_and_is_removed_with_tmp() -> Check {
        let temp = tempfile::tempdir()?;
        let attempts = std::fs::canonicalize(temp.path())?.join("attempt");
        std::fs::create_dir_all(attempts.join("tmp/candidate-0"))?;
        use std::os::unix::fs::PermissionsExt as _;
        for path in [
            &attempts,
            &attempts.join("tmp"),
            &attempts.join("tmp/candidate-0"),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let attempt = Directory::open_anchor(&attempts)?;
        use_parent_in_tests(Path::new("/private/tmp"));

        let alias = ensure(&attempt).ok_or("alias created")?;
        // Cleanup runs on every exit, so a failed assertion leaks nothing.
        struct Unlink<'a>(&'a Path);
        impl Drop for Unlink<'_> {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(self.0);
            }
        }
        let _unlink = Unlink(&alias);
        assert!(alias.starts_with("/private/tmp"));
        assert_eq!(std::fs::canonicalize(&alias)?, attempts.join("tmp"));
        let socket = alias.join("candidate-0/x.sock");
        assert!(socket.as_os_str().len() < 104, "{}", socket.display());

        crate::artifacts::remove_unreserved_terminal_temporary(&attempt)?;
        assert!(std::fs::symlink_metadata(&alias).is_err());
        assert!(!attempts.join("tmp").exists());
        Ok(())
    }
}
