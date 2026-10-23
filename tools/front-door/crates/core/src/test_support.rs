use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn unavailable(message: &str) -> Option<Value> {
    eprintln!("skip: {message}");
    assert_ne!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref(),
        Ok("1"),
        "required oracle unavailable: {message}"
    );
    None
}

// One bounded oracle runner for this crate. No inherited provider/config env.
pub(crate) fn python(module: &str, body: &str, paths: &[&Path]) -> Option<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let prefix = r#"
import importlib.util, json, os, pathlib, sys
if sys.version_info < (3,12):
    print('Python 3.12 or newer required', file=sys.stderr)
    sys.exit(77)
root = pathlib.Path(sys.argv[1])
if sys.argv[2]:
    spec = importlib.util.spec_from_file_location('oracle', root / sys.argv[2])
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
"#;
    let mut command = Command::new(interpreter);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", "/nonexistent-port-fixture")
        .env("TZ", "UTC")
        .args(["-I", "-c", &format!("{prefix}\n{body}")])
        .arg(root)
        .arg(module)
        .args(paths)
        .stdin(Stdio::null());

    run(command, "Python")
}

fn run(mut command: Command, name: &str) -> Option<Value> {
    let stdout = tempfile::tempfile().unwrap();
    let stderr = tempfile::tempfile().unwrap();
    command
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return unavailable(&format!("{name} interpreter absent"))
        }
        Err(e) => panic!("cannot launch {name} oracle: {e}"),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            // Reaping remains bounded, including a pathological child.
            let cleanup = Instant::now() + Duration::from_secs(2);
            while child.try_wait().unwrap().is_none() && Instant::now() < cleanup {
                std::thread::park_timeout(Duration::from_millis(5));
            }
            panic!("{name} oracle exceeded 30 seconds");
        }
        std::thread::park_timeout(Duration::from_millis(5));
    };
    use std::io::{Read, Seek, SeekFrom};
    let read = |mut file: std::fs::File| {
        file.seek(SeekFrom::Start(0)).unwrap();
        assert!(
            file.metadata().unwrap().len() <= 8 * 1024 * 1024,
            "oracle output too large"
        );
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    };
    let error = read(stderr);
    if status.code() == Some(77) {
        return unavailable(&String::from_utf8_lossy(&error));
    }
    assert!(
        status.success(),
        "{name} oracle failed: {}",
        String::from_utf8_lossy(&error)
    );
    Some(serde_json::from_slice(&read(stdout)).unwrap())
}

pub(crate) fn python_values(body: &str, input: &Value) -> Option<Value> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.json");
    std::fs::write(&path, serde_json::to_vec(input).unwrap()).unwrap();
    python(
        "",
        &format!("values=json.loads(pathlib.Path(sys.argv[3]).read_text())\n{body}"),
        &[&path],
    )
}

pub(crate) fn node_values(body: &str, input: &Value) -> Option<Value> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.json");
    std::fs::write(&path, serde_json::to_vec(input).unwrap()).unwrap();
    let interpreter = std::env::var_os("PROVENANT_PORT_NODE").unwrap_or_else(|| "node".into());
    let prefix = r#"
if (Number(process.versions.node.split('.')[0]) < 24) {
    process.stderr.write('Node 24 or newer required'); process.exit(77);
}
const values = JSON.parse(require('node:fs').readFileSync(process.argv[1], 'utf8'));
"#;
    let mut command = Command::new(interpreter);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", "/nonexistent-port-fixture")
        .env("TZ", "UTC")
        .args(["--input-type=commonjs", "-e", &format!("{prefix}\n{body}")])
        .arg(path)
        .stdin(Stdio::null());
    run(command, "Node")
}
