use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// An exclusively created, owner-only directory removed when its owner exits.
pub(crate) struct ScratchDir(PathBuf);

impl ScratchDir {
    pub(crate) fn new_in(parent: &Path, prefix: &str) -> std::io::Result<Self> {
        for _ in 0..10 {
            let mut name = std::ffi::OsString::from(prefix);
            name.push(provenant_os::fs::temporary_name().map_err(std::io::Error::other)?);
            let path = parent.join(name);
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "scratch directory name collisions",
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scratch_is_private_unique_and_removed_on_drop() -> std::io::Result<()> {
        use super::ScratchDir;
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir()?;
        let first = ScratchDir::new_in(parent.path(), "scratch-")?;
        let second = ScratchDir::new_in(parent.path(), "scratch-")?;
        assert_ne!(first.path(), second.path());
        assert_eq!(
            std::fs::metadata(first.path())?.permissions().mode() & 0o777,
            0o700
        );
        let path = first.path().to_owned();
        std::fs::write(path.join("content"), b"data")?;
        drop(first);
        assert!(!path.exists());
        Ok(())
    }
}
