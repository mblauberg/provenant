mod common;
use common::{Sandbox, BINARY};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

fn invoke(spec: &str) -> std::process::Output {
    let mut child = Command::new(BINARY)
        .args(["lane", "run"])
        .env_remove("PROVENANT_FRONT_DOOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{spec}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn unknown_launch_major_refuses_without_starting() {
    let output = invoke(r#"{"schema":"provenant.lane.launch.v2"}"#);
    assert_eq!(
        output.status.code(),
        Some(64),
        "{}",
        common::stderr(&output)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn normal_exit_publishes_provider_status_and_redacts_environment() {
    let sandbox = Sandbox::new("lane-normal");
    let dir = sandbox.dir.display();
    let output = invoke(&format!(
        r#"{{"schema":"provenant.lane.launch.v1","argv":["/bin/sh","-c","printf 'hello\\n'; printf 'diagnostic\\n' >&2; exit 7"],"cwd":"{dir}","attempt_dir":"{dir}","run_token":"test","env":{{"PATH":"/usr/bin:/bin","SECRET":"private-value"}},"stdin":{{}},"capture":{{"events":"events.jsonl","stderr":"stderr.log","max_bytes":1024}},"silence_observation_ms":1000,"timeouts_ms":{{"startup":1000,"wall":2000,"settle":20,"child_grace":20,"root_flush":50}},"cancel_markers":[],"observer":{{"argv":[]}},"spare":{{"code_mode_host":true}}}}"#
    ));
    assert_eq!(output.status.code(), Some(0), "{}", common::stderr(&output));
    let record = fs::read_to_string(sandbox.dir.join("supervisor.exit.json")).unwrap();
    assert!(record.contains("\"exit\":7"), "{record}");
    assert!(record.contains("provenant.lane.exit.v1"));
    assert_eq!(
        fs::read_to_string(sandbox.dir.join("events.jsonl")).unwrap(),
        "hello\n"
    );
    let launch = fs::read_to_string(sandbox.dir.join("launch.json")).unwrap();
    assert!(!launch.contains("private-value"));
    assert!(launch.contains("SECRET"));
}
