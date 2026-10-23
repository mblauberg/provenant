use serde_json::Value;
use std::io::Write;
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
    let version = match Command::new(&interpreter).arg("--version").output() {
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
    let mut child = match Command::new(&interpreter)
        .arg(root.join("tools/front-door/crates/workflow/tests/oracle.py"))
        .arg(&root)
        .arg(unit)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable("interpreter is absent")
        }
        Err(error) => panic!("cannot start Python oracle: {error}"),
    };
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(cases).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

pub fn unhashable(kind: &str) -> Value {
    serde_json::json!({"error_type":"TypeError", "unhashable_type":kind})
}
