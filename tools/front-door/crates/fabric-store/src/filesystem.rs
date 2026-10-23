//! Store-owned policy composed over the supervisor's safe filesystem handles.
use crate::*;
use provenant_supervisor::fs::{SafeDir, SafeFs};
use std::{
    ffi::OsStr,
    path::{Component, PathBuf},
};

/// Resolve this trusted caller path as Node does, following directory links.
/// Component operations retain directory handles; only explicit .. uses a new
/// trusted anchor, since the supervisor deliberately refuses .. as a component.
pub(crate) fn directory(
    fs: &dyn SafeFs,
    path: &Path,
    create: bool,
) -> std::io::Result<Box<dyn SafeDir>> {
    let mut dir = fs.root(path.is_absolute())?;
    let mut resolved = PathBuf::from(if path.is_absolute() { "/" } else { "." });
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                resolved.push("..");
                dir = fs.open_anchor(&resolved)?;
            }
            Component::Normal(name) => {
                dir = match dir.open_directory(name, true) {
                    Ok(child) => child,
                    Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                        match dir.mkdir(name, 0o700) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                            Err(e) => return Err(e),
                        }
                        dir.open_directory(name, true)?
                    }
                    Err(error) => return Err(error),
                };
                resolved.push(name);
            }
            Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsupported path prefix",
                ))
            }
        }
    }
    Ok(dir)
}
pub(crate) fn parent_and_name(path: &Path) -> (&Path, &OsStr) {
    (
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
        path.file_name().unwrap_or(path.as_os_str()),
    )
}

#[derive(Debug, thiserror::Error)]
#[error("{}", self.message())]
pub struct FilesystemError {
    pub operation: &'static str,
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}
impl FilesystemError {
    pub(crate) fn new(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self {
            operation,
            path: path.into(),
            source,
        }
    }
    fn message(&self) -> String {
        use provenant_supervisor::sys::errno::*;
        let (code, detail) = match self.source.raw_os_error() {
            Some(ENOENT) => ("ENOENT", "no such file or directory"),
            Some(ENOTDIR) => ("ENOTDIR", "not a directory"),
            Some(EACCES) => ("EACCES", "permission denied"),
            Some(EPERM) => ("EPERM", "operation not permitted"),
            Some(EISDIR) => ("EISDIR", "illegal operation on a directory"),
            Some(ELOOP) => ("ELOOP", "too many symbolic links encountered"),
            Some(EEXIST) => ("EEXIST", "file already exists"),
            _ => return self.source.to_string(),
        };
        format!(
            "{code}: {detail}, {} '{}'",
            self.operation,
            self.path.display()
        )
    }
}
