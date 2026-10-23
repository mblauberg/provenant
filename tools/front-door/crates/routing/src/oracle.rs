//! Test-only v4 Python oracle; production never invokes Python.
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn unavailable(reason: &str) -> Option<std::process::Command> {
    eprintln!("skip: {reason}");
    assert!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() != Ok("1"),
        "required Python oracle unavailable: {reason}"
    );
    None
}

fn interpreter() -> Option<Command> {
    let program = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let version = Command::new(&program)
        .args([
            "-I",
            "-c",
            "import sys; sys.exit(0 if sys.version_info >= (3,12) else 42)",
        ])
        .output();
    match version {
        Err(error) => {
            return unavailable(&format!("Python oracle {program:?} unavailable: {error}"))
        }
        Ok(output) if output.status.code() == Some(42) => {
            return unavailable(&format!("Python oracle {program:?} is older than 3.12"))
        }
        Ok(output) => assert!(
            output.status.success(),
            "Python oracle version check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
    Some(Command::new(program))
}

pub(crate) fn python<T: DeserializeOwned>(code: &str, input: &Value) -> Option<T> {
    let mut command = interpreter()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let mut child = command
        .args(["-I", "-c", code])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(input).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}
