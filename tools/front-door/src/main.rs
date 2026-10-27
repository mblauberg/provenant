//! The installed binary: parse the command line and dispatch it.
use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(code) = provenant::disk_guard_early() {
        return ExitCode::from(code);
    }
    let parsed = provenant_cli::parse(std::env::args_os().skip(1));
    ExitCode::from(provenant::dispatch(parsed, &provenant::System))
}
