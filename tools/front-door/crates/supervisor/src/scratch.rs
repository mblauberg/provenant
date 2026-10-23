//! Only explicitly classified generated leaves can be removed at settlement.
//! The caller retains run/writer and attempt exclusion. Confinement must keep
//! this private generated directory outside the provider sandbox: mode 0700
//! alone does not exclude another process running as this user. All legitimate
//! mutators use these same leases. Unknown or externally writable trees must
//! never be admitted. POSIX has no portable conditional-unlink operation.
use crate::fs::{Directory, FileKind, FsError, Metadata};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;

#[derive(Debug)]
pub struct Scratch {
    directory: Directory,
    identity: Metadata,
    classified: BTreeMap<OsString, Metadata>,
}
impl Scratch {
    pub(crate) fn new(directory: Directory) -> Result<Self, FsError> {
        let identity = directory.metadata()?;
        if identity.mode & 0o077 != 0 || identity.uid != crate::sys::fs::effective_uid() {
            return Err(FsError::RaceRefused {
                name: "scratch".into(),
            });
        }
        Ok(Self {
            directory,
            identity,
            classified: BTreeMap::new(),
        })
    }
    /// Exclusive generated staging only. History and unknown entries are never
    /// admitted, and a symlink is never followed or removed as generated data.
    pub fn create(&mut self, name: &OsStr, bytes: &[u8]) -> Result<(), FsError> {
        let mut file = self.directory.create_new(name)?;
        file.write_all(bytes).map_err(|source| FsError::Io {
            operation: "write scratch",
            path: name.into(),
            source,
        })?;
        file.sync()?;
        let metadata = file.metadata()?;
        self.classified.insert(name.to_owned(), metadata);
        self.directory.sync()
    }
    pub(crate) fn settle(&mut self) -> Result<(), FsError> {
        let root = self.directory.metadata()?;
        if !root.same_identity(&self.identity) || root.mode & 0o077 != 0 {
            return Err(FsError::RaceRefused {
                name: "scratch".into(),
            });
        }
        for (name, expected) in &self.classified {
            let actual = match self.directory.metadata_at(name, false) {
                Ok(metadata) => metadata,
                Err(error) if error.is_not_found() => continue,
                Err(error) => return Err(error),
            };
            if actual.kind != FileKind::Regular
                || actual.links != 1
                || actual.device != root.device
                || !actual.same_identity(expected)
                || actual.size != expected.size
                || actual.modified_ns != expected.modified_ns
            {
                continue;
            }
            self.directory.unlink(name)?;
        }
        self.directory.sync()?;
        self.classified.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settlement_retains_unknown_changed_and_linked_content() {
        let root = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut scratch = Scratch::new(Directory::open_anchor(root.path()).unwrap()).unwrap();
        scratch.create(OsStr::new("profile"), b"generated").unwrap();
        scratch.create(OsStr::new("drift"), b"old").unwrap();
        scratch.create(OsStr::new("link"), b"old").unwrap();
        std::fs::write(root.path().join("history"), b"retain").unwrap();
        std::fs::write(root.path().join("drift"), b"different").unwrap();
        std::fs::remove_file(root.path().join("link")).unwrap();
        std::os::unix::fs::symlink(root.path().join("history"), root.path().join("link")).unwrap();
        scratch.settle().unwrap();
        assert!(!root.path().join("profile").exists());
        assert_eq!(
            std::fs::read(root.path().join("drift")).unwrap(),
            b"different"
        );
        assert!(root.path().join("link").is_symlink());
        assert_eq!(
            std::fs::read(root.path().join("history")).unwrap(),
            b"retain"
        );
    }
}
