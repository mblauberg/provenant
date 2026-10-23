//! Installation-specific path preparation around the supervisor's safe API.
use crate::{io, pending, Result};
use provenant_supervisor::fs::{Directory, PublishMode};
use std::{fs, path::Path};

fn parent(path: &Path) -> Result<Directory> {
    let directory = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let resolved = crate::resolve(directory)?;
    Directory::open_anchor(&resolved).map_err(|e| io(directory, e.into()))
}

pub(crate) fn atomic_write(path: &Path, prefix: &str, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(directory).map_err(|e| io(directory, e))?;
    let name = path.file_name().ok_or_else(|| crate::invalid("installation destination has no name"))?;
    parent(path)?.atomic_write(name, prefix, bytes, PublishMode::Replace).map_err(|e| io(path, e.into()))
}

pub(crate) fn replace_link(destination: &Path, source: &Path) -> Result<()> {
    let name = destination.file_name().ok_or_else(|| crate::invalid("installation destination has no name"))?;
    parent(destination)?.replace_link(name, source).map_err(|e| io(destination, e.into()))
}
