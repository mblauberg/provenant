//! Publish an oversized ordinary result under the fresh state root, atomically.
use provenant_supervisor::fs::{Directory, Publication, PublishMode};
use serde::Serialize;
use std::{
    ffi::OsStr,
    io,
    path::{Component, Path},
};

pub fn publish<T: Serialize>(
    result: &provenant_records::CommandResult<T>,
    globals: &provenant_cli::Globals,
) -> io::Result<provenant_records::AbsolutePath> {
    fn perform<T: Serialize>(
        result: &provenant_records::CommandResult<T>,
        globals: &provenant_cli::Globals,
    ) -> Result<provenant_records::AbsolutePath, Box<dyn std::error::Error>> {
        let assets = crate::roots::product_root()?;
        let project = crate::roots::selected_project(globals.project.as_deref(), false)?;
        let roots = crate::roots::roots(globals, &assets, &project)?;
        let path = roots.state.as_path().join("tmp/results");
        let mut dir = Directory::root(true)?;
        for component in path.components() {
            if let Component::Normal(name) = component {
                match dir.mkdir(name, 0o700) {
                    Ok(()) => dir.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
                dir = dir.open_verified_directory(name)?;
            }
        }
        let name = provenant_supervisor::fs::temporary_name()?;
        let bytes = serde_json::to_vec(result)?;
        match dir.atomic_write(&name, ".result-", &bytes, PublishMode::NoReplace)? {
            Publication::Durable => {}
            Publication::VisibleDurabilityUnconfirmed { sync_error } => {
                return Err(sync_error.into());
            }
        }
        Ok(provenant_records::AbsolutePath::new(
            &Path::new(&path).join(OsStr::new(&name)),
        )?)
    }
    perform(result, globals).map_err(|_| io::Error::other("cannot publish oversized result"))
}
