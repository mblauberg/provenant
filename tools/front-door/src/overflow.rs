//! Publish an oversized ordinary result through the shared lane-aware owner.
use serde::Serialize;
use std::io;

pub fn publish<T: Serialize>(
    result: &provenant_records::CommandResult<T>,
    globals: &provenant_cli::Globals,
) -> io::Result<provenant_records::AbsolutePath> {
    let perform = || -> Result<_, Box<dyn std::error::Error>> {
        let assets = crate::roots::product_root()?;
        let project = crate::roots::selected_project(globals.project.as_deref(), false)?;
        let roots = crate::roots::roots(globals, &assets, &project)?;
        Ok(provenant_cli::fabric::publish(result, &roots)?)
    };
    perform().map_err(|_| io::Error::other("cannot publish oversized result"))
}
