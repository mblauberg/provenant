#![allow(
    clippy::disallowed_methods,
    reason = "integration tests exercise the CLI process"
)]
use std::{path::PathBuf, process::Command};

#[test]
fn run_announces_contributing_repo_file() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let presets = temp.path().join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    let file = presets.join("single.toml");
    std::fs::write(&file, "[parameters]\nroute = 'utility'\n")?;
    let prompt = temp.path().join("prompt");
    std::fs::write(&prompt, "brief")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // This directory is not a Git checkout, so the graph owner cannot submit.
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(temp.path())
        .env("PROVENANT_ROOT", root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(temp.path().join("state"))
        .args(["run", "--preset", "single", "--prompt-file"])
        .arg(prompt)
        .output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Repository preset:") && stderr.contains(&file.display().to_string()),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn explain_does_not_create_state() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let prompt = temp.path().join("prompt");
    std::fs::write(&prompt, "brief")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(&root)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["run", "--preset", "single", "--prompt-file"])
        .arg(&prompt)
        .arg("--explain")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["data"]["graph"]["nodes"][0]["id"], "task");
    assert!(!state.exists());
    std::fs::write(&prompt, vec![b'x'; 600_000])?;
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(&root)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["run", "--preset", "fan-out", "--prompt-file"])
        .arg(&prompt)
        .arg("--explain")
        .output()?;
    assert!(output.status.success());
    assert!(output.stdout.len() > 1_048_576);
    assert!(!state.exists());
    Ok(())
}

#[test]
fn show_validates_and_list_reports_bad_files() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let presets = temp.path().join("provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(presets.join("single.toml"), "[parameters]\nunknown = 1\n")?;
    std::fs::write(presets.join("broken.toml"), "[invalid")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = |args: &[&str]| -> std::io::Result<std::process::Output> {
        Command::new(env!("CARGO_BIN_EXE_provenant"))
            .current_dir(&root)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path())
            .arg("--json")
            .args(args)
            .output()
    };
    let shown = run(&["preset", "show", "single"])?;
    assert!(!shown.status.success());
    let error = String::from_utf8_lossy(&shown.stdout);
    assert!(
        error.contains("parameters.unknown") && error.contains("single.toml"),
        "{error}"
    );
    let listed = run(&["preset", "list"])?;
    assert!(listed.status.success());
    let output = String::from_utf8_lossy(&listed.stdout);
    assert!(
        output.contains("broken.toml") && output.contains("parameters.unknown"),
        "{output}"
    );
    assert!(run(&["preset", "show", "chain"])?.status.success());
    Ok(())
}

#[test]
fn oversized_expansions_fail_before_graph_state_for_run_and_explain()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let prompt = temp.path().join("prompt");
    std::fs::write(&prompt, vec![b'x'; 1_048_576])?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for explain in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_provenant"));
        command
            .current_dir(&root)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path())
            .args(["--json", "--state-dir"])
            .arg(&state)
            .args(["run", "--preset", "fan-out", "--prompt-file"])
            .arg(&prompt)
            .args(["--set", "parameters.width=9"]);
        if explain {
            command.arg("--explain");
        }
        let output = command.output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("8 MiB"));
        assert!(!state.exists());
    }
    Ok(())
}
