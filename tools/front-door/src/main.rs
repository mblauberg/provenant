//! The installed binary: parse the command line and dispatch it.
use std::process::ExitCode;

fn main() -> ExitCode {
    let parsed = provenant_cli::parse(std::env::args_os().skip(1));
    ExitCode::from(provenant::dispatch(parsed, &provenant::System))
}
