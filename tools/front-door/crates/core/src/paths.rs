//! Native path values. No filesystem or environment access occurs here.
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("path must be nonempty and relative with only normal components")]
pub struct RelativePathError;
/// A relative native path with no `.` or `..` components. Filesystem
/// containment and no-follow checks remain the supervisor's responsibility.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath(PathBuf);
impl RelativePath {
    pub fn parse(path: impl AsRef<Path>) -> Result<Self, RelativePathError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || has_dot_or_nul(path)
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(RelativePathError);
        }
        Ok(Self(path.into()))
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}
impl AsRef<Path> for RelativePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

// Path::components() normalises interior `.` before callers can reject it.
pub(crate) fn has_dot_or_nul(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes.contains(&0)
        || bytes
            .split(|b| *b == b'/')
            .any(|part| part == b"." || part == b"..")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_paths_reject_normalization_and_parent_traversal() {
        assert_eq!(
            RelativePath::parse("a/b").unwrap().as_path(),
            Path::new("a/b")
        );
        for bad in ["", ".", "./a", "a/./b", "a/../b", "../a", "/a", "a\0b"] {
            assert!(RelativePath::parse(bad).is_err(), "{bad:?}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let native = std::ffi::OsStr::from_bytes(b"a/\xff");
            assert_eq!(
                RelativePath::parse(native).unwrap().as_path().as_os_str(),
                native
            );
        }
    }
}
