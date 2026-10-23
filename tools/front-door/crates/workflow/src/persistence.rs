//! Shared descriptor-relative workflow directory traversal.
use provenant_core::{ProjectId, StateRoot};
use provenant_supervisor::fs::{Directory, FsError};
use std::ffi::OsStr;

pub(crate) fn directory(
    root: &StateRoot,
    project: ProjectId,
    collection: &str,
    key: &str,
    create: bool,
) -> Result<Directory, FsError> {
    let mut dir = Directory::open_anchor(root.as_path())?;
    for name in [
        "workflow",
        "projects",
        &project.to_string(),
        collection,
        key,
    ] {
        let name = OsStr::new(name);
        if create {
            match dir.mkdir(name, 0o700) {
                Ok(()) => dir.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error),
            }
        }
        dir = dir.open_verified_directory(name)?;
    }
    Ok(dir)
}
