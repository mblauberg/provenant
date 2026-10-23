//! Test-only Python oracle, with an explicit minimal subprocess environment.
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
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
        command.args(["-I", "-B"]);
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
        .args([
            "-c",
            &format!("import os,sys; sys.path.insert(0, os.getcwd()); {code}"),
        ])
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
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::yield_now(),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                panic!("Python oracle exceeded 60 second deadline");
            }
            Err(error) => panic!("could not observe Python oracle: {error}"),
        }
    }
    let status = child.wait().unwrap();
    let stdout = stdout_reader.join().unwrap().unwrap();
    let stderr = stderr_reader.join().unwrap().unwrap();
    assert!(
        status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    Some(crate::decode_json(&stdout).unwrap())
}
