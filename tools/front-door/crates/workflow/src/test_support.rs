use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn oracle(unit: &str, cases: &Value) -> Option<Value> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let required = std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1");
    let unavailable = |reason: &str| {
        if required {
            panic!("Python differential required but unavailable: {reason}");
        }
        eprintln!("skip: Python differential {reason}");
        None
    };
    let version = match bounded_output(
        Command::new(&interpreter)
            .args(["-I", "--version"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default()),
        Duration::from_secs(5),
    ) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let text = if text.is_empty() {
                String::from_utf8_lossy(&output.stderr)
            } else {
                text
            };
            text.split_whitespace()
                .last()
                .and_then(|v| {
                    v.split('.')
                        .take(2)
                        .map(str::parse::<u32>)
                        .collect::<Result<Vec<_>, _>>()
                        .ok()
                })
                .filter(|v| v.len() == 2)
                .map(|v| (v[0], v[1]))
        }
        Ok(output) => {
            return unavailable(&format!(
                "interpreter version check failed ({})",
                output.status
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable("interpreter is absent")
        }
        Err(error) => return unavailable(&format!("cannot inspect interpreter: {error}")),
    };
    if version.is_none_or(|(major, minor)| major < 3 || (major == 3 && minor < 12)) {
        return unavailable("requires Python 3.12 or newer");
    }
    let mut input = tempfile::tempfile().unwrap();
    input
        .write_all(serde_json::to_string(cases).unwrap().as_bytes())
        .unwrap();
    input.seek(SeekFrom::Start(0)).unwrap();
    let output = bounded_output(
        Command::new(&interpreter)
            .arg("-I")
            .arg(root.join("tools/front-door/crates/workflow/tests/oracle.py"))
            .arg(&root)
            .arg(unit)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::from(input)),
        Duration::from_secs(30),
    )
    .unwrap_or_else(|error| panic!("cannot run Python oracle: {error}"));
    let status = output.status;
    let error = String::from_utf8_lossy(&output.stderr);
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(status.success(), "Python oracle failed: {error}");
    Some(serde_json::from_str(&output).unwrap())
}

pub fn unhashable(kind: &str) -> Value {
    serde_json::json!({"error_type":"TypeError", "unhashable_type":kind})
}

fn bounded_output(
    command: &mut Command,
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let reap_deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait()?.is_none() && Instant::now() < reap_deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Python oracle exceeded its time bound",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if stdout.metadata()?.len() > 16 * 1024 * 1024 || stderr.metadata()?.len() > 64 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Python oracle output exceeds its byte bound",
        ));
    }
    stdout.seek(SeekFrom::Start(0))?;
    stderr.seek(SeekFrom::Start(0))?;
    let mut out = vec![];
    let mut err = vec![];
    stdout.take(16 * 1024 * 1024).read_to_end(&mut out)?;
    stderr.take(64 * 1024).read_to_end(&mut err)?;
    Ok(std::process::Output {
        status,
        stdout: out,
        stderr: err,
    })
}
