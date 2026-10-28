//! Local catalogue reads and compatibility through the CLI.
mod support;
use serde_json::Value;
use std::path::{Path, PathBuf};
use support::{Result, TestRig, code};

fn call(rig: &TestRig, args: &[&str], expected: i64) -> Result<Value> {
    let mut words = args.to_vec();
    words.push("--json");
    let output = rig.command(&words)?;
    assert_eq!(
        code(&output),
        expected,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

fn snapshot(path: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut items = Vec::new();
    if path.exists() {
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                items.push((path.clone(), Vec::new()));
                items.extend(snapshot(&path)?);
            } else {
                items.push((path.clone(), std::fs::read(path)?));
            }
        }
    }
    items.sort();
    Ok(items)
}

#[test]
fn catalogue_read_only() -> Result {
    let empty = TestRig::new()?.without_state()?;
    assert_eq!(
        call(&empty, &["console", "projects"], 0)?["data"]["projects"],
        serde_json::json!([])
    );
    assert_eq!(
        call(&empty, &["console", "sessions"], 0)?["data"]["sessions"],
        serde_json::json!([])
    );
    assert!(!empty.state.exists());
    let empty_project = empty.project.to_str().ok_or("project path")?;
    call(
        &empty,
        &["--project", empty_project, "console", "projects"],
        3,
    )?;
    assert!(!empty.state.exists());

    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let commit = rig.execute(
        &git,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
        None,
    )?;
    assert_eq!(code(&commit), 0);
    let linked = rig.root.join("linked");
    let linked_text = linked.to_str().ok_or("linked path")?;
    let worktree = rig.execute(
        &git,
        &["worktree", "add", "-b", "linked", linked_text],
        None,
    )?;
    assert_eq!(code(&worktree), 0);
    let file = rig.root.join("checkpoint.md");
    std::fs::write(&file, "legacy checkpoint\n")?;
    let file_text = file.to_str().ok_or("file path")?;
    call(
        &rig,
        &["workflow", "session", "save", "one", "--file", file_text],
        0,
    )?;
    call(
        &rig,
        &[
            "--project",
            linked_text,
            "workflow",
            "session",
            "save",
            "two",
            "--file",
            file_text,
        ],
        0,
    )?;
    let projects = call(&rig, &["console", "projects"], 0)?;
    assert_eq!(
        projects["data"]["projects"]
            .as_array()
            .ok_or("projects")?
            .len(),
        1
    );
    let project = projects["data"]["projects"][0]["project_id"]
        .as_str()
        .ok_or("project id")?;
    let before = snapshot(&rig.state)?;
    let human = rig.command(&["console", "projects"])?;
    assert_eq!(code(&human), 0);
    let human = String::from_utf8(human.stdout.bytes)?;
    assert_eq!(
        human
            .lines()
            .filter(|line| line.starts_with("projects: "))
            .count(),
        1
    );
    assert!(human.contains("project_id="));
    assert!(!human.contains("\n  \"projects\""));
    let selected = call(&rig, &["--project", linked_text, "console", "sessions"], 0)?;
    assert_eq!(
        selected["data"]["sessions"]
            .as_array()
            .ok_or("sessions")?
            .len(),
        2
    );
    let first = call(&rig, &["console", "sessions", "--limit", "1"], 0)?;
    assert_eq!(first["data"]["sessions"][0]["summary"], Value::Null);
    assert_eq!(
        first["data"]["sessions"][0]["summary_status"],
        "unavailable"
    );
    let cursor = first["data"]["next_cursor"].as_str().ok_or("cursor")?;
    let second = call(
        &rig,
        &["console", "sessions", "--limit", "1", "--cursor", cursor],
        0,
    )?;
    assert_eq!(
        second["data"]["sessions"]
            .as_array()
            .ok_or("sessions")?
            .len(),
        1
    );
    assert_ne!(
        first["data"]["sessions"][0]["record"]["host_session_id"],
        second["data"]["sessions"][0]["record"]["host_session_id"]
    );
    call(
        &rig,
        &["console", "sessions", "--limit", "2", "--cursor", cursor],
        2,
    )?;
    assert_eq!(snapshot(&rig.state)?, before);
    std::fs::remove_file(
        rig.state
            .join("workflow/projects")
            .join(project)
            .join("sessions/two/discovery.json"),
    )?;
    let before = snapshot(&rig.state)?;
    let shown = call(&rig, &["workflow", "session", "show", "two"], 0)?;
    assert_eq!(shown["data"]["markdown"], "legacy checkpoint\n");
    let listed = call(&rig, &["console", "sessions"], 0)?;
    assert_eq!(
        listed["data"]["sessions"]
            .as_array()
            .ok_or("sessions")?
            .len(),
        1
    );
    call(
        &rig,
        &["console", "sessions", "--limit", "1", "--cursor", cursor],
        2,
    )?;
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}

#[test]
fn oversized_catalogue_never_publishes() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let rig = TestRig::new()?;
    let projects = rig.state.join("projects");
    std::fs::create_dir(&projects)?;
    std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o700))?;
    let common = format!("/{}", "a".repeat(180));
    let encoded = provenant_core::hex(common.as_bytes());
    for index in 0..6000 {
        let id = format!("{index:032x}");
        let dir = projects.join(&id);
        std::fs::create_dir(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let path = dir.join("project.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "project":id, "canonical_common_dir":encoded
            }))?,
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let before = snapshot(&rig.state)?;
    let human = rig.command(&["console", "projects"])?;
    assert_eq!(code(&human), 3);
    assert!(human.stdout.bytes.is_empty());
    assert!(String::from_utf8(human.stderr.bytes)?.contains("RESULT_TOO_LARGE"));
    let json = call(&rig, &["console", "projects"], 3)?;
    assert_eq!(json["error"]["code"], "RESULT_TOO_LARGE");
    assert_eq!(snapshot(&rig.state)?, before);
    Ok(())
}
