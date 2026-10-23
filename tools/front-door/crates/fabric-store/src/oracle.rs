//! One bounded, hermetic Node oracle gate for this crate.
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn unavailable(reason: impl std::fmt::Display, required: bool) -> Option<Value> {
    assert!(
        !required,
        "required Node differential unavailable: {reason}"
    );
    eprintln!("skip: {reason}");
    None
}
fn bounded(command: &mut Command, directory: &Path) -> std::io::Result<(bool, String, String)> {
    let stdout = directory.join("stdout");
    let stderr = directory.join("stderr");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(File::create(&stdout)?)
        .stderr(File::create(&stderr)?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let success = loop {
        if let Some(status) = child.try_wait()? {
            break status.success();
        }
        if Instant::now() >= deadline {
            child.kill()?;
            // kill has completed; still bound observation/reaping rather than wait().
            let reap = Instant::now() + Duration::from_secs(2);
            while child.try_wait()?.is_none() && Instant::now() < reap {
                std::thread::sleep(Duration::from_millis(5));
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Node oracle exceeded 30 seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let read = |path| -> std::io::Result<String> {
        let mut text = String::new();
        File::open(path)?
            .take(2_000_000)
            .read_to_string(&mut text)?;
        Ok(text)
    };
    Ok((success, read(stdout)?, read(stderr)?))
}
pub(super) fn node(scenario: &str) -> Option<Value> {
    let interpreter = std::env::var_os("PROVENANT_PORT_NODE").unwrap_or_else(|| "node".into());
    let required = std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1");
    node_with(scenario, &interpreter, required)
}
fn node_with(scenario: &str, interpreter: &std::ffi::OsStr, required: bool) -> Option<Value> {
    let directory = tempfile::tempdir().unwrap();
    let mut probe = Command::new(interpreter);
    probe
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .arg("--version");
    let version = match bounded(&mut probe, directory.path()) {
        Ok((true, text, _)) => text,
        Ok(_) => return unavailable("Node version probe failed", required),
        Err(e) => return unavailable(e, required),
    };
    if version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .is_none_or(|v| v < 24)
    {
        return unavailable("Node >=24 is required", required);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let source = root.join("runtime/fabric/src/store.ts");
    let loader = root.join("node_modules/tsx/dist/loader.mjs");
    if !loader.exists() {
        return unavailable(
            "oracle requires the checkout's installed tsx dependency",
            required,
        );
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle.mjs");
    std::fs::create_dir(directory.path().join("scratch")).unwrap();
    let mut command = Command::new(interpreter);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NODE_NO_WARNINGS", "1")
        .env("TMPDIR", directory.path().join("scratch"))
        .arg("--import")
        .arg(loader)
        .arg(fixture)
        .arg(source)
        .arg(directory.path().join("source.sqlite3"))
        .arg(scenario);
    let (ok, stdout, stderr) = match bounded(&mut command, directory.path()) {
        Ok(output) => output,
        Err(error) => return unavailable(error, required),
    };
    assert!(ok, "Node oracle failed: {stderr}");
    Some(
        serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("oracle JSON: {e}\n{stdout}\n{stderr}")),
    )
}

#[test]
fn missing_node_skips_optional_but_fails_required_mode() {
    let missing = Path::new("/no-such-fabric-oracle/node").as_os_str();
    assert!(node_with("leases", missing, false).is_none());
    assert!(std::panic::catch_unwind(|| node_with("leases", missing, true)).is_err());
}
