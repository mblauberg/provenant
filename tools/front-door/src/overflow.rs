//! Publish an oversized ordinary result through the shared lane-aware owner.
use serde::Serialize;
use std::io;

pub fn publish<T: Serialize>(
    result: &provenant_records::CommandResult<T>,
    globals: &provenant_cli::Globals,
) -> io::Result<provenant_records::AbsolutePath> {
    let perform = || -> Result<_, Box<dyn std::error::Error>> {
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Required,
        )?;
        Ok(provenant_cli::fabric::publish(result, &resolved.roots)?)
    };
    perform().map_err(|_| io::Error::other("cannot publish oversized result"))
}
