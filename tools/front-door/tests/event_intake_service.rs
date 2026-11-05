#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Spawns the worker and a fake forge under test.
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn service_receives_signed_hints_and_polls_with_fake_gh() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let fake = dir.path().join("gh");
    fs::write(
        &fake,
        r##"#!/bin/sh
for arg in "$@"; do last="$arg"; done
case "$last" in
  *'pulls?state='*) body='[]' ;;
  */pulls/12) body='{"number":12,"state":"open","head":{"sha":"abc"}}' ;;
  *'check-runs?'*) body='{"check_runs":[]}' ;;
  *'status?'*) body='{"statuses":[]}' ;;
  *'comments?'*|*'reviews?'*) body='[]' ;;
  *) body='{}' ;;
esac
printf 'HTTP/1.1 200 OK\nETag: "fake"\nX-RateLimit-Remaining: 4999\n\n%s' "$body"
"##,
    )?;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755))?;
    let path = format!("{}:{}", dir.path().display(), std::env::var("PATH")?);
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    let state = dir.path().join("state");
    let mut command = Command::new(env!("CARGO_BIN_EXE_event-intake"));
    command
        .env("PATH", path)
        .env("PROVENANT_INTAKE_REPOSITORY", "o/r")
        .env("PROVENANT_INTAKE_DIR", &state)
        .env("PROVENANT_INTAKE_BIND", address.to_string())
        .env("PROVENANT_INTAKE_SECRET", "secret")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _worker = Worker(command.spawn()?);
    provenant_test_support::wait_for(
        "startup sweep",
        Duration::from_secs(20),
        || fs::read_to_string(state.join("state.json")).unwrap_or_default(),
        |s| s.contains("last_successful_sweep") && !s.contains("\"last_successful_sweep\":null"),
    )?;
    let body = r#"{"repository":{"full_name":"o/r"},"pull_request":{"number":12}}"#;
    let request = format!(
        "POST /webhook HTTP/1.1\r\nContent-Length: {}\r\nX-GitHub-Delivery: id\r\nX-GitHub-Event: pull_request\r\nX-Hub-Signature-256: sha256=db4061419529a8addfd7db7f148ff8677579ec4723d46ba890f9c7c80f8629ab\r\n\r\n{body}",
        body.len()
    );
    for _ in 0..2 {
        let mut stream = TcpStream::connect(address)?;
        stream.write_all(request.as_bytes())?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply)?;
        assert!(reply.starts_with("HTTP/1.1 202"), "{reply}");
    }
    provenant_test_support::wait_for(
        "marker reconciliation",
        Duration::from_secs(20),
        || fs::read_to_string(state.join("deliveries.json")).unwrap_or_default(),
        |s| s.contains("\"pending\":false") && s.contains("\"completed_at\":"),
    )?;
    let current: serde_json::Value = serde_json::from_slice(&fs::read(state.join("state.json"))?)?;
    assert_eq!(
        current["current"]["12"]["pull_request"]["head"]["sha"],
        "abc"
    );
    let markers: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("deliveries.json"))?)?;
    assert_eq!(markers["deliveries"].as_object().map(|m| m.len()), Some(1));
    Ok(())
}

#[test]
fn service_runs_without_receiver() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let fake = dir.path().join("gh");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf 'HTTP/1.1 200 OK\\nETag: fake\\n\\n[]'\n",
    )?;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755))?;
    let state = dir.path().join("state");
    let path = format!("{}:{}", dir.path().display(), std::env::var("PATH")?);
    let mut command = Command::new(env!("CARGO_BIN_EXE_event-intake"));
    command
        .env("PATH", &path)
        .env("PROVENANT_INTAKE_REPOSITORY", "o/r")
        .env("PROVENANT_INTAKE_DIR", &state)
        .env_remove("PROVENANT_INTAKE_BIND")
        .env_remove("PROVENANT_INTAKE_SECRET")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _worker = Worker(command.spawn()?);
    provenant_test_support::wait_for(
        "polling-only sweep",
        Duration::from_secs(20),
        || fs::read_to_string(state.join("state.json")).unwrap_or_default(),
        |s| s.contains("last_successful_sweep") && !s.contains("\"last_successful_sweep\":null"),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_event-intake"))
        .arg("status")
        .env("PATH", path)
        .env("PROVENANT_INTAKE_REPOSITORY", "o/r")
        .env("PROVENANT_INTAKE_DIR", &state)
        .env_remove("PROVENANT_INTAKE_BIND")
        .env_remove("PROVENANT_INTAKE_SECRET")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(status["requests"], 1);
    assert!(status["last_successful_sweep"].is_u64());
    assert!(status["oldest_pending_marker"].is_null());
    Ok(())
}
