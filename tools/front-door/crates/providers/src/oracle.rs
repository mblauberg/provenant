//! Test-only Python oracle, with an explicit minimal subprocess environment.
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn unavailable(reason: &str) -> Option<Command> {
    eprintln!("skip: {reason}");
    assert!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() != Ok("1"),
        "required Python differential unavailable: {reason}"
    );
    None
}

fn command() -> Option<Command> {
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let make = || {
        let mut command = Command::new(&interpreter);
        command.env_clear();
        // Needed for executable lookup (including env-based interpreter wrappers).
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command.arg("-B");
        command
    };
    let output = match make()
        .args([
            "-c",
            "import json,sys; print(json.dumps(list(sys.version_info[:2])))",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable("Python interpreter absent")
        }
        Err(error) => panic!("could not probe Python oracle: {error}"),
    };
    assert!(
        output.status.success(),
        "Python version probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let version: Vec<u32> = serde_json::from_slice(&output.stdout).unwrap();
    if version.as_slice() < [3, 12].as_slice() {
        return unavailable("Python oracle requires >=3.12");
    }
    Some(make())
}

pub(crate) fn python<T: DeserializeOwned>(
    code: &str,
    input: &Value,
    api_key: Option<&str>,
) -> Option<T> {
    let mut command = command()?;
    if let Some(key) = api_key {
        command.env("ANTHROPIC_API_KEY", key);
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let mut child = command
        .args(["-c", code])
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
    Some(crate::decode_json(&output.stdout).unwrap())
}
