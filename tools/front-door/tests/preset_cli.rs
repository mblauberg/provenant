#![allow(
    clippy::disallowed_methods,
    reason = "integration tests exercise the CLI process"
)]
use std::{path::PathBuf, process::Command};

#[test]
fn preset_wait_completes_a_retained_gate_graph_and_replays_it()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "seed",
        ],
    ] {
        let output = Command::new("git")
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("gate-only.toml"),
        "stages = [{ id = 'accept', kind = 'gate', condition = { type = 'budget' } }]\n",
    )?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "accept the bounded graph")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_provenant"))
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(temp.path().join("state"))
            .args(["run", "--preset", "gate-only", "--prompt-file"])
            .arg(&prompt)
            .args(["--wait", "1s"])
            .output()
    };
    let first = run()?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    assert_eq!(first["data"]["graph"]["status"], "complete");
    assert_eq!(
        first["data"]["graph"]["checkpoint"]["nodes"][0]["state"],
        "succeeded"
    );
    assert!(
        PathBuf::from(
            first["data"]["document"]
                .as_str()
                .ok_or("retained document")?
        )
        .exists()
    );
    let replay = run()?;
    assert!(replay.status.success());
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout)?;
    assert_eq!(
        replay["data"]["graph"]["checkpoint"],
        first["data"]["graph"]["checkpoint"]
    );
    Ok(())
}

#[test]
fn explain_pins_source_refs_before_hashing_without_creating_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("git")
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().into())
    };
    git(&["init", "-q"])?;
    let commit = || {
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "advance",
        ])
    };
    commit()?;
    let head = git(&["rev-parse", "HEAD"])?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "review this immutable subject")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = || -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(temp.path().join("state"))
            .args(["run", "--preset", "review-council", "--prompt-file"])
            .arg(&prompt)
            .arg("--explain")
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    };
    let first = run()?;
    assert_eq!(first["data"]["graph"]["nodes"][0]["source"]["commit"], head);
    commit()?;
    let second = run()?;
    assert_ne!(
        first["data"]["graph"]["graph_id"],
        second["data"]["graph"]["graph_id"]
    );
    assert_eq!(
        second["data"]["graph"]["nodes"][0]["source"]["commit"],
        git(&["rev-parse", "HEAD"])?
    );
    assert!(!temp.path().join("state").exists());
    Ok(())
}

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
fn explain_applies_writer_read_root_and_concurrency_bindings_without_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let presets = temp.path().join(".provenant/presets");
    let writer = temp.path().join("writer");
    let source = temp.path().join("source");
    std::fs::create_dir_all(&presets)?;
    std::fs::create_dir(&writer)?;
    std::fs::create_dir(&source)?;
    std::fs::write(
        presets.join("bound.toml"),
        "stages = [{ id = 'write', role = 'implementer' }]\n\
         [roles.implementer]\n\
         tier = 'senior'\n\
         access = 'worktree_write'\n\
         worktree = ''\n\
         write_scope = ['src']\n\
         prompt = '${prompt}'\n",
    )?;
    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "Implement the bounded slice")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(temp.path())
        .env("PROVENANT_ROOT", root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["run", "--preset", "bound", "--prompt-file"])
        .arg(&prompt)
        .arg("--worktree")
        .arg(&writer)
        .arg("--read-root")
        .arg(&source)
        .args(["--concurrency", "3", "--wait", "30m", "--explain"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let graph = &result["data"]["graph"];
    assert_eq!(graph["limits"]["concurrency"], 3);
    assert_eq!(
        graph["nodes"][0]["worktree"],
        writer.to_str().ok_or("writer path")?
    );
    assert_eq!(
        graph["nodes"][0]["read_roots"],
        serde_json::json!([std::fs::canonicalize(&source)?])
    );
    assert_eq!(graph["nodes"][0]["write_scope"], serde_json::json!(["src"]));
    assert!(!state.exists());
    Ok(())
}

#[test]
fn show_validates_and_list_reports_bad_files() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let presets = temp.path().join("provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("single.toml"),
        "[parameters.extra]\ntype = 'string'\n",
    )?;
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
        error.contains("parameters.extra") && error.contains("single.toml"),
        "{error}"
    );
    let listed = run(&["preset", "list"])?;
    assert!(listed.status.success());
    let output = String::from_utf8_lossy(&listed.stdout);
    assert!(
        output.contains("broken.toml") && output.contains("parameters.extra"),
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

#[test]
fn run_refuses_a_read_root_outside_the_project() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "Read elsewhere")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(temp.path())
        .env("PROVENANT_ROOT", root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(temp.path().join("state"))
        .args(["run", "--preset", "single", "--prompt-file"])
        .arg(&prompt)
        .arg("--read-root")
        .arg(outside.path())
        .arg("--explain")
        .output()?;
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("allowed_read_roots"), "{text}");
    Ok(())
}

#[test]
fn integration_sourced_graph_identity_follows_the_integration_head()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let git = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
        let output = Command::new("git")
            .current_dir(&project)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=f@example.invalid",
            ])
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    git(&["init", "-q"])?;
    git(&["commit", "--allow-empty", "-qm", "seed"])?;
    git(&["branch", "batch"])?;
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("integrated.toml"),
        "integration_branch = 'batch'\n\
         stages = [{ id = 'review', role = 'reviewer' }]\n\
         [roles.reviewer]\ntier = 'senior'\nsource = { type = 'integration' }\nprompt = '${prompt}'\n",
    )?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "review the batch")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = || -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(temp.path().join("state"))
            .args(["run", "--preset", "integrated", "--prompt-file"])
            .arg(&prompt)
            .arg("--explain")
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    };
    let first = run()?;
    assert_eq!(
        first["data"]["graph"]["graph_id"],
        run()?["data"]["graph"]["graph_id"]
    );
    git(&["checkout", "-q", "batch"])?;
    git(&["commit", "--allow-empty", "-qm", "lane delivered"])?;
    assert_ne!(
        first["data"]["graph"]["graph_id"],
        run()?["data"]["graph"]["graph_id"]
    );
    Ok(())
}

#[test]
fn params_file_supplies_lists_and_set_wins_over_it() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let prompt = temp.path().join("prompt");
    std::fs::write(&prompt, "brief")?;
    let params = temp.path().join("params.toml");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let root_text = root.display().to_string();
    std::fs::write(
        &params,
        format!(
            "batch_worktree = '{root_text}'\nlanes = [{{name='a',worktree='{root_text}',scope=['x'],spec='#1'}},{{name='b',worktree='{root_text}',scope=['y'],spec='#2'}}]\n"
        ),
    )?;
    let run = |extra: &[String]| -> Result<std::process::Output, std::io::Error> {
        Command::new(env!("CARGO_BIN_EXE_provenant"))
            .current_dir(&root)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path())
            .args(["--json", "--state-dir"])
            .arg(&state)
            .args(["run", "--preset", "big-batch", "--prompt-file"])
            .arg(&prompt)
            .arg("--params-file")
            .arg(&params)
            .arg("--explain")
            .args(extra)
            .output()
    };
    let output = run(&[])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("lane-a") && text.contains("lane-b"), "{text}");
    assert!(!text.contains("lane-c"));
    let output = run(&[
        "--set".to_owned(),
        format!("parameters.lanes=[{{name='c',worktree='{root_text}',scope=['z'],spec='#3'}}]"),
    ])?;
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("lane-c") && !text.contains("lane-a"),
        "{text}"
    );
    Ok(())
}

#[test]
fn preset_explain_describes_the_pattern_and_creates_no_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let state = temp.path().join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(&root)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["preset", "explain", "review-council"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["data"]["pattern"]["level"], "default");
    assert!(
        result["data"]["parameters"]
            .as_array()
            .is_some_and(|parameters| parameters.iter().any(|p| p["name"] == "risk_tier"))
    );
    assert!(!state.exists());
    Ok(())
}

fn scrub(text: &str, root: &std::path::Path) -> String {
    let text = text.replace(&root.display().to_string(), "<root>");
    let mut out = String::new();
    for line in text.lines() {
        let mut line = line.to_owned();
        while let Some(start) = line.find("sha256:") {
            let end = line[start..]
                .find(|c: char| !(c.is_ascii_hexdigit() || "sha256:".contains(c)))
                .map_or(line.len(), |n| start + n);
            line.replace_range(start..end, "<digest>");
        }
        let mut hex = 0;
        let mut start = None;
        for (index, byte) in line.bytes().enumerate() {
            if byte.is_ascii_hexdigit() {
                hex += 1;
                start.get_or_insert(index);
            } else {
                hex = 0;
                start = None;
            }
            if hex == 40 {
                let start = start.unwrap_or(0);
                line.replace_range(start..=index, "<commit>");
                break;
            }
        }
        if line.starts_with("Graph ") {
            line = "Graph <id>".into();
        }
        if line.starts_with("Limits:") || line.starts_with("Execution:") {
            continue;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn explain_text(preset: &str, extra: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&project)
                .args(args)
                .status()?
                .success()
        );
    }
    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "the change")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args(["run", "--preset", preset, "--prompt-file"])
        .arg(&prompt)
        .args(extra)
        .arg("--explain")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?
        .replace(&project.canonicalize()?.display().to_string(), "<project>");
    Ok(scrub(&text, &root))
}

fn golden(name: &str, actual: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, actual)?;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        actual, expected,
        "{name}: rerun with UPDATE_GOLDEN=1 to refresh"
    );
    Ok(())
}

#[test]
fn review_council_explain_shows_lenses_tiers_families_and_the_verifier()
-> Result<(), Box<dyn std::error::Error>> {
    let text = explain_text(
        "review-council",
        &[
            "--set",
            "parameters.worktree=\".\"",
            "--set",
            "parameters.risk_tier=\"high\"",
        ],
    )?;
    let verify = text
        .find("verify  role=review.verify")
        .ok_or("no verifier")?;
    let lens = text.find("lens-correctness-spec").ok_or("no lens")?;
    let synthesis = text
        .find("synthesis  role=review.synthesis")
        .ok_or("no synthesis")?;
    assert!(lens < verify && verify < synthesis, "{text}");
    assert!(text.contains("family=cross"), "{text}");
    assert!(text.contains("family=distinct_from_writer"), "{text}");
    golden("review-council.explain.txt", &text)?;
    Ok(())
}

#[test]
fn scout_swarm_and_generator_evaluator_explain_goldens() -> Result<(), Box<dyn std::error::Error>> {
    golden(
        "scout-swarm.explain.txt",
        &explain_text("scout-swarm", &[])?,
    )?;
    golden(
        "generator-evaluator.explain.txt",
        &explain_text(
            "generator-evaluator",
            &["--set", "parameters.worktree=\".\""],
        )?,
    )?;
    Ok(())
}

#[test]
fn preset_show_roles_lists_the_library_with_origins() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let repo = temp.path().join(".provenant");
    std::fs::create_dir_all(&repo)?;
    std::fs::write(
        repo.join("roles.toml"),
        "[roles.\"lens.house\"]\nextends = 'lens'\ncard = 'House rules.'\n",
    )?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .current_dir(temp.path())
        .env("PROVENANT_ROOT", root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(temp.path().join("state"))
        .args(["preset", "show", "--roles"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let roles = result["data"]["roles"].as_array().ok_or("no roles")?;
    let house = roles
        .iter()
        .find(|role| role["name"] == "lens.house")
        .ok_or("no house")?;
    assert!(
        house["origin"]
            .as_str()
            .ok_or("origin")?
            .ends_with("roles.toml")
    );
    assert_eq!(house["extends"], serde_json::json!(["lens"]));
    let scope = roles
        .iter()
        .find(|role| role["name"] == "plan.scope")
        .ok_or("no scope")?;
    assert_eq!(scope["origin"], "bundled");
    assert!(
        scope["references"][0]["digest"]
            .as_str()
            .ok_or("digest")?
            .starts_with("sha256:")
    );
    Ok(())
}
