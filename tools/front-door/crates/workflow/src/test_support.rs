use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

pub fn oracle(unit: &str, cases: &Value) -> Option<Value> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let mut child = match Command::new("python3")
        .arg(root.join("tools/front-door/crates/workflow/tests/oracle.py"))
        .arg(&root).arg(unit)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping Python differential: python3 is absent");
            return None;
        }
        Err(error) => panic!("cannot start Python oracle: {error}"),
    };
    child.stdin.take().unwrap().write_all(serde_json::to_string(cases).unwrap().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "Python oracle failed: {}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}
