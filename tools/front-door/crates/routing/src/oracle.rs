//! Test-only v4 Python oracle; production never invokes Python.
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn unavailable(reason: &str) -> Option<Command> {
    eprintln!("skip: {reason}");
    assert!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() != Ok("1"),
        "required Python oracle unavailable: {reason}"
    );
    None
}

// File-backed pipes avoid deadlock on large fixtures; every child has a deadline.
fn run(command: &mut Command, input: &[u8]) -> std::io::Result<Output> {
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input)?;
    use std::io::{Seek, SeekFrom};
    stdin.seek(SeekFrom::Start(0))?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            // Reap through the same bounded observation loop after cancellation.
            let reap = Instant::now() + Duration::from_secs(5);
            while child.try_wait()?.is_none() && Instant::now() < reap {
                std::thread::sleep(Duration::from_millis(5));
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Python oracle deadline exceeded",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    fn read(file: &mut File) -> std::io::Result<Vec<u8>> {
        file.seek(SeekFrom::Start(0))?;
        let mut data = Vec::new();
        file.take(8 * 1024 * 1024).read_to_end(&mut data)?;
        Ok(data)
    }
    Ok(Output {
        status,
        stdout: read(&mut stdout)?,
        stderr: read(&mut stderr)?,
    })
}

pub(crate) fn python<T: DeserializeOwned>(code: &str, input: &Value) -> Option<T> {
    let program = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let command = || {
        let mut command = Command::new(&program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("AGENT_FABRIC_STATE_ROOT", home.path().join("state"))
            .env("LC_ALL", "C.UTF-8")
            .current_dir(root);
        command
    };
    let version = run(
        command().args([
            "-I",
            "-c",
            "import sys; sys.exit(0 if sys.version_info >= (3,12) else 42)",
        ]),
        b"",
    );
    match version {
        Err(error) => {
            unavailable(&format!("Python oracle {program:?} unavailable: {error}"))?;
        }
        Ok(output) if output.status.code() == Some(42) => {
            unavailable(&format!("Python oracle {program:?} is older than 3.12"))?;
        }
        Ok(output) => assert!(
            output.status.success(),
            "Python oracle version check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
    let output = run(
        command().args(["-I", "-c", code]),
        &serde_json::to_vec(input).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}
