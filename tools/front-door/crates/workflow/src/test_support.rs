use serde_json::Value;
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use std::process::{Command, Stdio};

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
    let version = match Command::new(&interpreter).args(["-I", "--version"]).env_clear().output() {
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
    input.write_all(serde_json::to_string(cases).unwrap().as_bytes()).unwrap();
    use std::io::{Seek, SeekFrom};
    input.seek(SeekFrom::Start(0)).unwrap();
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = match Command::new(&interpreter)
        .arg("-I")
        .arg(root.join("tools/front-door/crates/workflow/tests/oracle.py"))
        .arg(&root)
        .arg(unit)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(stdout.try_clone().unwrap()))
        .stderr(Stdio::from(stderr.try_clone().unwrap()))
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable("interpreter is absent")
        }
        Err(error) => panic!("cannot start Python oracle: {error}"),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() { break status; }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            // A killed child is polled within a second bounded deadline as well.
            let reap_deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait().unwrap().is_none() && Instant::now() < reap_deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("Python oracle exceeded its 30 second bound");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    stdout.seek(SeekFrom::Start(0)).unwrap();
    stderr.seek(SeekFrom::Start(0)).unwrap();
    let mut output = String::new();
    let mut error = String::new();
    stdout.take(16 * 1024 * 1024).read_to_string(&mut output).unwrap();
    stderr.take(64 * 1024).read_to_string(&mut error).unwrap();
    assert!(status.success(), "Python oracle failed: {error}");
    Some(serde_json::from_str(&output).unwrap())
}

pub fn unhashable(kind: &str) -> Value {
    serde_json::json!({"error_type":"TypeError", "unhashable_type":kind})
}
