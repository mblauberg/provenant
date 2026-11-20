use crate::fail;
use provenant_cli::{Build, Globals};
use provenant_records::Outcome;

/// Build CLI handlers are introduced before their owning report and hygiene slices.
pub fn run(command: Build, globals: &Globals) -> u8 {
    let name = match command {
        Build::Status => "build status",
        Build::Report { .. } => "build report",
        Build::Reset { .. } => "build reset",
        Build::Maintain => "build maintain",
    };
    fail(
        name,
        Outcome::Unavailable,
        "BUILD_NOT_IMPLEMENTED",
        "build command implementation is not available yet",
        globals,
    )
}
