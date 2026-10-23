//! One interpreter gate for every records differential.
use std::process::Command;

fn unavailable(reason: impl std::fmt::Display) -> Option<Command> {
    assert!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() != Ok("1"),
        "required Python differential unavailable: {reason}"
    );
    eprintln!("skip: {reason}");
    None
}

pub(crate) fn python() -> Option<Command> {
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let probe = match Command::new(&interpreter)
        .args([
            "-c",
            "import sys;sys.exit(0 if sys.version_info >= (3,12) else 1)",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) => return unavailable(format_args!("{interpreter:?}: {error}")),
    };
    if !probe.status.success() {
        return unavailable(format_args!("{interpreter:?} requires Python >=3.12"));
    }
    Some(Command::new(interpreter))
}
