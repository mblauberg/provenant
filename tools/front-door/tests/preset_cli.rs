#![allow(
    clippy::disallowed_methods,
    reason = "integration tests exercise the CLI process"
)]
use std::{path::PathBuf, process::Command};

#[test]
fn preset_explain_warns_about_every_omitted_lens() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prompt = temp.path().join("prompt");
    std::fs::write(&prompt, "Review the bounded change")?;
    let output = fixture_cli()
        .current_dir(&root)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args([
            "--json",
            "run",
            "--preset",
            "review-council",
            "--prompt-file",
        ])
        .arg(&prompt)
        .args(["--explain", "--set", "parameters.risk_tier='routine'"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let warnings = result["data"]["graph"]["provenance"]["warnings"]
        .as_array()
        .ok_or("warnings missing")?;
    assert!(!warnings.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    for warning in warnings {
        assert!(
            stderr.contains(warning.as_str().ok_or("warning text")?),
            "{stderr}"
        );
    }
    Ok(())
}

#[test]
fn bound_worktree_default_head_subject_resolves_in_that_worktree()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let invoking = temp.path().join("invoking");
    std::fs::create_dir(&invoking)?;
    let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command()
            .current_dir(&invoking)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    git(&["init", "-q"])?;
    git(&["commit", "--allow-empty", "-qm", "writer head A"])?;
    let worktree_head = git(&["rev-parse", "HEAD"])?;
    git(&["branch", "writer"])?;
    git(&["commit", "--allow-empty", "-qm", "invoking head B"])?;
    let invoking_head = git(&["rev-parse", "HEAD"])?;
    let worktree = invoking.join(".worktrees/writer");
    std::fs::create_dir_all(worktree.parent().ok_or("worktree parent")?)?;
    git(&[
        "worktree",
        "add",
        worktree.to_str().ok_or("worktree path")?,
        "writer",
    ])?;
    assert_ne!(invoking_head, worktree_head);

    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "Implement the approved bounded change")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = fixture_cli()
        .current_dir(&invoking)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args([
            "--json",
            "run",
            "--preset",
            "generator-evaluator",
            "--prompt-file",
        ])
        .arg(&prompt)
        .args(["--explain", "--set"])
        .arg(format!("parameters.worktree='{}'", worktree.display()))
        .output()?;
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let graph = &result["data"]["graph"];
    assert_eq!(graph["nodes"][0]["source"]["commit"], worktree_head);
    assert_eq!(
        graph["provenance"]["resolved_subject"]["commit"],
        worktree_head
    );
    assert_eq!(
        graph["provenance"]["resolved_subject"]["worktree"],
        worktree.display().to_string()
    );

    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("differs from bound worktree HEAD"),
        "unexpected warning: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let explicit = fixture_cli()
        .current_dir(&invoking)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args([
            "--json",
            "run",
            "--preset",
            "generator-evaluator",
            "--prompt-file",
        ])
        .arg(&prompt)
        .args(["--explain", "--set"])
        .arg(format!("parameters.worktree='{}'", worktree.display()))
        .args(["--set", "parameters.subject.commit='HEAD'"])
        .output()?;
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let stderr = String::from_utf8_lossy(&explicit.stderr);
    assert!(
        stderr.contains("differs from bound worktree HEAD"),
        "stderr: {stderr}"
    );
    let explicit: serde_json::Value = serde_json::from_slice(&explicit.stdout)?;
    assert_eq!(
        explicit["data"]["graph"]["nodes"][0]["source"]["commit"],
        invoking_head
    );
    assert!(
        explicit["data"]["graph"]["provenance"]
            .get("resolved_subject")
            .is_none()
    );
    Ok(())
}

#[test]
fn review_council_bound_worktree_resolves_subject_and_warns_on_differing_head()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let invoking = temp.path().join("invoking");
    std::fs::create_dir(&invoking)?;
    let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command()
            .current_dir(&invoking)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    git(&["init", "-q"])?;
    git(&["commit", "--allow-empty", "-qm", "subject commit A"])?;
    let worktree_head = git(&["rev-parse", "HEAD"])?;
    git(&["branch", "subject-branch"])?;
    git(&["commit", "--allow-empty", "-qm", "invoking head B"])?;
    let invoking_head = git(&["rev-parse", "HEAD"])?;
    let worktree = invoking.join(".worktrees/review");
    std::fs::create_dir_all(worktree.parent().ok_or("worktree parent")?)?;
    git(&[
        "worktree",
        "add",
        worktree.to_str().ok_or("worktree path")?,
        "subject-branch",
    ])?;
    assert_ne!(invoking_head, worktree_head);

    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "Review the approved bounded change")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

    // 1. With bound worktree, default HEAD resolves to the bound worktree's HEAD.
    let output = fixture_cli()
        .current_dir(&invoking)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args([
            "--json",
            "run",
            "--preset",
            "review-council",
            "--prompt-file",
        ])
        .arg(&prompt)
        .arg("--read-root")
        .arg(&invoking)
        .args(["--explain", "--set"])
        .arg(format!("parameters.worktree='{}'", worktree.display()))
        .output()?;
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let graph = &result["data"]["graph"];
    // The subject lane (node 0) source equals the bound worktree HEAD (the subject commit).
    assert_eq!(graph["nodes"][0]["source"]["commit"], worktree_head);
    assert_eq!(
        graph["provenance"]["resolved_subject"]["commit"],
        worktree_head
    );
    assert_eq!(
        graph["provenance"]["resolved_subject"]["worktree"],
        worktree.display().to_string()
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("differs from bound worktree HEAD"),
        "unexpected warning on matching head: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // 2. When explicit source differs from bound worktree HEAD, --explain warns.
    let explicit = fixture_cli()
        .current_dir(&invoking)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args([
            "--json",
            "run",
            "--preset",
            "review-council",
            "--prompt-file",
        ])
        .arg(&prompt)
        .arg("--read-root")
        .arg(&invoking)
        .args(["--explain", "--set"])
        .arg(format!("parameters.worktree='{}'", worktree.display()))
        .args(["--set", "parameters.subject.commit='HEAD'"])
        .output()?;
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let explicit_json: serde_json::Value = serde_json::from_slice(&explicit.stdout)?;
    assert_eq!(
        explicit_json["data"]["graph"]["nodes"][0]["source"]["commit"],
        invoking_head
    );
    let stderr = String::from_utf8_lossy(&explicit.stderr);
    assert!(
        stderr.contains("differs from bound worktree HEAD"),
        "expected warning in stderr: {stderr}"
    );
    let warnings = explicit_json["data"]["graph"]["provenance"]["warnings"]
        .as_array()
        .ok_or("warnings array")?;
    assert!(
        warnings.iter().any(|w| {
            w.as_str().is_some_and(|text| {
                text.contains("differs from bound worktree HEAD")
                    && text.contains(&invoking_head)
                    && text.contains(&worktree_head)
            })
        }),
        "expected warning in provenance: {warnings:?}"
    );

    // 3. Human --explain output also includes the warning.
    let human = fixture_cli()
        .current_dir(&invoking)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args(["run", "--preset", "review-council", "--prompt-file"])
        .arg(&prompt)
        .arg("--read-root")
        .arg(&invoking)
        .args(["--explain", "--set"])
        .arg(format!("parameters.worktree='{}'", worktree.display()))
        .args(["--set", "parameters.subject.commit='HEAD'"])
        .output()?;
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(
        human_stdout.contains("Warning: source commit")
            && human_stdout.contains("differs from bound worktree HEAD"),
        "human output missing warning: {human_stdout}"
    );

    Ok(())
}

/// A council with a pinned base carries a document-level review base, which the
/// runtime accepts only under schema 2. `--explain` never admitted the graph,
/// so the mismatch surfaced only on submit, as `preset_graph: invalid input`.
#[test]
fn review_council_with_a_base_is_admitted_on_submit() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    git(&["init", "-q"])?;
    git(&["commit", "--allow-empty", "-qm", "base"])?;
    let base = git(&["rev-parse", "HEAD"])?;
    git(&["commit", "--allow-empty", "-qm", "subject"])?;
    let head = git(&["rev-parse", "HEAD"])?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "review the subject")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(temp.path().join("state"))
        .args(["run", "--preset", "review-council", "--prompt-file"])
        .arg(&prompt)
        .args([
            "--set",
            &format!("parameters.subject.commit=\"{head}\""),
            "--set",
            &format!("parameters.base=\"{base}\""),
            "--set",
            "parameters.review_shape=\"quick\"",
            "--plan",
            "off",
            "--wait",
            "1s",
        ])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_ne!(result["error"]["code"], "preset_graph", "{stdout}");
    assert!(
        !stdout.contains("invalid input"),
        "the submit was refused as invalid: {stdout}"
    );
    assert!(
        result["data"]["graph"]["checkpoint"]["graph_id"].is_string(),
        "the graph was not admitted: {stdout}"
    );
    Ok(())
}

fn fixture_cli() -> Command {
    scrubbed(env!("CARGO_BIN_EXE_provenant"))
}

fn scrubbed(program: &str) -> Command {
    let mut command = Command::new(program);
    provenant_os::fixture::scrub_git_env(&mut command);
    // Drop inherited PROVENANT_* settings: a lane's markers would make the
    // fixture CLI act as a lane member.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PROVENANT_") {
            command.env_remove(key);
        }
    }
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

#[test]
fn review_explain_snapshots_substantial_default_panel() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let brief = temp.path().join("brief");
    std::fs::write(&brief, "Review this change")?;
    let output = fixture_cli()
        .current_dir(&root)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args(["review", "HEAD", "--brief-file"])
        .arg(&brief)
        .args([
            "--risk",
            "substantial",
            "--range",
            "HEAD..HEAD",
            "--explain",
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    let request = provenant_routing::ReviewRequest {
        risk_tier: Some("substantial".into()),
        ..Default::default()
    };
    let panel =
        provenant_routing::ReviewConfig::default().resolve_panel(&request, None, &[], false)?;
    assert!(
        text.starts_with(&format!("review {} ({};", panel.shape, panel.source)),
        "{text}"
    );
    for slot in panel.slots {
        let pool = slot.tier.as_str().ok_or("pool")?;
        assert!(
            text.lines()
                .any(|line| line.contains(&format!("lens={}", slot.lens))
                    && line.contains(&format!("pool={pool}"))),
            "{text}"
        );
    }
    Ok(())
}

#[test]
fn review_explain_snapshots_integration_panel() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let brief = temp.path().join("brief");
    std::fs::write(&brief, "Review this integration head")?;
    let output = fixture_cli()
        .current_dir(&root)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args(["review", "HEAD", "--brief-file"])
        .arg(&brief)
        .args([
            "--kind",
            "integration",
            "--risk",
            "routine",
            "--risk-tag",
            "security",
            "--explain",
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    let request = provenant_routing::ReviewRequest {
        kind: Some("integration".into()),
        risk_tier: Some("routine".into()),
        risk_tags: vec!["security".into()],
        ..Default::default()
    };
    let panel =
        provenant_routing::ReviewConfig::default().resolve_panel(&request, None, &[], false)?;
    assert!(
        text.starts_with(&format!("review {} ({};", panel.shape, panel.source)),
        "{text}"
    );
    assert!(text.contains("risk tags: security"), "{text}");
    for slot in panel.slots {
        let pool = slot.tier.as_str().ok_or("pool")?;
        assert!(
            text.lines()
                .any(|line| line.contains(&format!("lens={}", slot.lens))
                    && line.contains(&format!("pool={pool}"))),
            "{text}"
        );
    }
    Ok(())
}

#[test]
fn preset_wait_completes_a_retained_gate_graph_and_replays_it()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "seed"],
    ] {
        let output = provenant_os::fixture::git_command()
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
        fixture_cli()
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
    let graph_id = first["data"]["graph"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or("graph id")?;
    let status_command = format!(
        "provenant --state-dir {} fabric graph status {graph_id}",
        temp.path().join("state").display()
    );
    assert_eq!(first["data"]["graph_id"], graph_id);
    assert_eq!(first["data"]["status_command"], status_command);
    assert_eq!(
        first["data"]["graph"]["checkpoint"]["nodes"][0]["state"],
        "succeeded"
    );
    let document = PathBuf::from(
        first["data"]["document"]
            .as_str()
            .ok_or("retained document")?,
    );
    assert!(document.exists());
    let document_hash =
        provenant_records::CanonicalV1::file_digest(&std::fs::read(&document)?).to_string();
    let graph =
        |verb: &str, selector: &str| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            let output = fixture_cli()
                .current_dir(&project)
                .env("PROVENANT_ROOT", &root)
                .env("XDG_CONFIG_HOME", temp.path().join("config"))
                .args(["--json", "--state-dir"])
                .arg(temp.path().join("state"))
                .args(["fabric", "graph", verb])
                .args(if verb == "resume" {
                    vec!["--document", selector]
                } else {
                    vec![selector]
                })
                .output()?;
            assert!(
                output.status.success(),
                "{verb} {selector}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            Ok(serde_json::from_slice(&output.stdout)?)
        };
    // The printed command runs verbatim, quoting included, in a shell.
    let printed = scrubbed("sh")
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("FIXTURE_BIN", env!("CARGO_BIN_EXE_provenant"))
        .arg("-c")
        .arg(format!(
            "provenant() {{ \"$FIXTURE_BIN\" --json \"$@\"; }}; {status_command}"
        ))
        .output()?;
    assert!(
        printed.status.success(),
        "{}",
        String::from_utf8_lossy(&printed.stdout)
    );
    let printed: serde_json::Value = serde_json::from_slice(&printed.stdout)?;
    assert_eq!(printed["data"]["checkpoint"]["graph_id"], graph_id);
    // Every selector form resolves identically for status and resume, and a
    // finished graph with no pending nodes still resumes by id.
    let digest = document_hash.strip_prefix("sha256:").ok_or("digest")?;
    let prefix = &digest[..12];
    for selector in [
        graph_id,
        document.to_str().ok_or("path")?,
        &document_hash,
        prefix,
    ] {
        let status = graph("status", selector)?;
        assert_eq!(
            status["data"]["checkpoint"]["graph_id"], graph_id,
            "{selector}"
        );
        let resumed = graph("resume", selector)?;
        assert_eq!(
            resumed["data"]["checkpoint"]["graph_id"], graph_id,
            "{selector}"
        );
        assert_eq!(
            resumed["data"]["checkpoint"]["nodes"],
            status["data"]["checkpoint"]["nodes"]
        );
    }
    // Selector precedence: an exact graph ID (even one ending `.json`) wins
    // over path forms; an existing bare file name is a path; a hex ID that is
    // also a hash prefix of a document declaring another graph is ambiguous.
    let relabelled = |graph_id: &str| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&document)?)?;
        value["graph_id"] = graph_id.into();
        Ok(serde_json::to_vec(&value)?)
    };
    let scratch = project.join(".provenant/scratch/graphs");
    let other = relabelled("other-graph")?;
    std::fs::write(scratch.join("other"), &other)?;
    std::fs::write(scratch.join("bare"), relabelled("dotted.json")?)?;
    let other_hash = provenant_records::CanonicalV1::file_digest(&other).to_string();
    let hex_id = &other_hash[7..15];
    for retained in ["dotted.json", hex_id] {
        let file = project.join(format!("{retained}.doc"));
        std::fs::write(&file, relabelled(retained)?)?;
        let run_graph = fixture_cli()
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(temp.path().join("state"))
            .args(["fabric", "graph", "run", "--document"])
            .arg(&file)
            .output()?;
        assert!(
            run_graph.status.success(),
            "{}",
            String::from_utf8_lossy(&run_graph.stdout)
        );
    }
    assert_eq!(
        graph("status", "dotted.json")?["data"]["checkpoint"]["graph_id"],
        "dotted.json"
    );
    assert_eq!(
        graph("status", "bare")?["data"]["checkpoint"]["graph_id"],
        "dotted.json"
    );
    let ambiguous = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(temp.path().join("state"))
        .args(["fabric", "graph", "status", hex_id])
        .output()?;
    assert!(!ambiguous.status.success());
    let ambiguous: serde_json::Value = serde_json::from_slice(&ambiguous.stdout)?;
    assert_eq!(
        ambiguous["error"]["code"], "dispatch_refused",
        "{ambiguous}"
    );
    assert!(
        ambiguous["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("accepted forms")),
        "{ambiguous}"
    );
    std::fs::remove_file(scratch.join("bare"))?;
    std::fs::remove_file(scratch.join("other"))?;
    let unknown = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(temp.path().join("state"))
        .args([
            "fabric",
            "graph",
            "status",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .output()?;
    assert!(!unknown.status.success());
    let unknown: serde_json::Value = serde_json::from_slice(&unknown.stdout)?;
    assert_eq!(unknown["error"]["code"], "not_found", "{unknown}");
    assert!(
        unknown["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("accepted forms")),
        "{unknown}"
    );
    assert_eq!(
        unknown["error"]["repair"], "run `provenant help fabric graph status`",
        "{unknown}"
    );
    let human = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--state-dir"])
        .arg(temp.path().join("state"))
        .args(["run", "--preset", "gate-only", "--prompt-file"])
        .arg(&prompt)
        .args(["--wait", "1s"])
        .output()?;
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout)?;
    assert!(human.contains(&format!("Graph ID: {graph_id}")), "{human}");
    assert!(
        human.contains(&format!("Status: {status_command}")),
        "{human}"
    );
    let replay = run()?;
    assert!(replay.status.success());
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout)?;
    assert_eq!(
        replay["data"]["graph"]["checkpoint"],
        first["data"]["graph"]["checkpoint"]
    );
    // A symlinked graphs directory is never scanned for a hash.
    let graphs = project.join(".provenant/scratch/graphs");
    let moved = project.join(".provenant/scratch/graphs-moved");
    std::fs::rename(&graphs, &moved)?;
    std::os::unix::fs::symlink(&moved, &graphs)?;
    let linked = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(temp.path().join("state"))
        .args(["fabric", "graph", "status", &document_hash[7..19]])
        .output()?;
    assert!(!linked.status.success());
    assert!(String::from_utf8(linked.stdout)?.contains("accepted forms"));
    Ok(())
}

#[test]
fn fresh_gives_a_rerun_a_new_graph_id_and_a_plain_rerun_the_same_one()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "look at it twice")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let graph_id = |extra: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = fixture_cli()
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(temp.path().join("state"))
            .args(["run", "--preset", "fan-out", "--prompt-file"])
            .arg(&prompt)
            .arg("--explain")
            .args(extra)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(value["data"]["graph"]["graph_id"]
            .as_str()
            .ok_or("graph id missing")?
            .to_owned())
    };
    let plain = graph_id(&[])?;
    assert_eq!(plain, graph_id(&[])?);
    let fresh = graph_id(&["--fresh"])?;
    assert_ne!(plain, fresh);
    assert_ne!(fresh, graph_id(&["--fresh"])?);
    // The flag is a preset option, like --set.
    let output = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .args(["run", "--fresh", "--prompt-file"])
        .arg(&prompt)
        .output()?;
    assert!(!output.status.success());
    Ok(())
}

#[test]
fn graph_retention_collects_operator_named_scratch_source() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    std::fs::create_dir(&project)?;
    std::fs::create_dir(&home)?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "seed"],
    ] {
        let output = provenant_os::fixture::git_command()
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
    std::fs::write(&prompt, "complete the graph")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let preset = fixture_cli()
        .current_dir(&project)
        .env("HOME", &home)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["run", "--preset", "gate-only", "--prompt-file"])
        .arg(&prompt)
        .args(["--wait", "1s"])
        .output()?;
    assert!(
        preset.status.success(),
        "{} {}",
        String::from_utf8_lossy(&preset.stdout),
        String::from_utf8_lossy(&preset.stderr)
    );
    let preset: serde_json::Value = serde_json::from_slice(&preset.stdout)?;
    let source = PathBuf::from(
        preset["data"]["document"]
            .as_str()
            .ok_or("preset document")?,
    );
    let mut document: serde_json::Value = serde_json::from_slice(&std::fs::read(&source)?)?;
    document["graph_id"] = "operator-scratch".into();
    let operator = source
        .parent()
        .ok_or("scratch directory")?
        .join("operator.json");
    std::fs::write(&operator, serde_json::to_vec(&document)?)?;
    document["graph_id"] = "unrelated".into();
    let unrelated = source
        .parent()
        .ok_or("scratch directory")?
        .join("unrelated.json");
    std::fs::write(&unrelated, serde_json::to_vec(&document)?)?;
    let graph = fixture_cli()
        .current_dir(&project)
        .env("HOME", &home)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["fabric", "graph", "run", "--document"])
        .arg(&operator)
        .output()?;
    assert!(
        graph.status.success(),
        "{} {}",
        String::from_utf8_lossy(&graph.stdout),
        String::from_utf8_lossy(&graph.stderr)
    );
    let graph: serde_json::Value = serde_json::from_slice(&graph.stdout)?;
    assert_eq!(graph["data"]["status"], "complete");
    let graph_dir = std::fs::read_dir(state.join("fabric/graphs"))?
        .next()
        .ok_or("graph directory")??
        .path();
    let checkpoint = graph_dir.join("graph-operator-scratch.json");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&checkpoint)?
        .set_times(std::fs::FileTimes::new().set_modified(old))?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[gc]\nschedule=false\ngraph_checkpoint_days=1\n",
    )?;
    let gc = fixture_cli()
        .current_dir(&home)
        .env("HOME", &home)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(&state)
        .args(["internal", "tick-gc"])
        .output()?;
    assert!(
        gc.status.success(),
        "{} {}",
        String::from_utf8_lossy(&gc.stdout),
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(!checkpoint.exists());
    assert!(!operator.exists());
    assert!(unrelated.exists());
    assert!(source.exists());
    Ok(())
}

#[test]
fn tick_reports_a_missing_graph_worktree_once_without_failing()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    std::fs::create_dir(&project)?;
    std::fs::create_dir(&home)?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "seed"],
    ] {
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let graph = serde_json::json!({
        "schema_version":1,"graph_id":"missing-worktree","decision_session":"chair-session",
        "nodes":[{"id":"choose","kind":"decide","access":"read_only","decide":{
            "question":"Ship?","options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
            "default":"hold","timeout_seconds":3600
        }}]
    });
    let document = project.join("graph.json");
    std::fs::write(&document, serde_json::to_vec(&graph)?)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = fixture_cli()
        .current_dir(&project)
        .env("HOME", &home)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["fabric", "graph", "run", "--document"])
        .arg(&document)
        .output()?;
    assert!(
        run.status.success(),
        "{} {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("gate-only.toml"),
        "stages = [{ id = 'accept', kind = 'gate', condition = { type = 'budget' } }]\n",
    )?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "complete the graph")?;
    let terminal = fixture_cli()
        .current_dir(&project)
        .env("HOME", &home)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["run", "--preset", "gate-only", "--prompt-file"])
        .arg(&prompt)
        .args(["--wait", "1s"])
        .output()?;
    assert!(
        terminal.status.success(),
        "{} {}",
        String::from_utf8_lossy(&terminal.stdout),
        String::from_utf8_lossy(&terminal.stderr)
    );
    let terminal: serde_json::Value = serde_json::from_slice(&terminal.stdout)?;
    assert_eq!(terminal["data"]["graph"]["status"], "complete");
    let terminal_id = terminal["data"]["graph"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or("terminal graph id")?;
    let graph_dir = std::fs::read_dir(state.join("fabric/graphs"))?
        .next()
        .ok_or("project graph directory")??
        .path();
    let terminal_checkpoint = graph_dir.join(format!("graph-{terminal_id}.json"));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&terminal_checkpoint)?
        .set_times(std::fs::FileTimes::new().set_modified(old))?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[gc]\nschedule=false\n",
    )?;
    std::fs::remove_dir_all(&project)?;
    let tick = || {
        fixture_cli()
            .current_dir(&home)
            .env("HOME", &home)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(&state)
            .arg("tick")
            .output()
    };
    let first = tick()?;
    assert!(
        first.status.success(),
        "{} {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    let warnings = first["data"]["graphs"]["warnings"]
        .as_array()
        .ok_or("warnings")?;
    assert_eq!(warnings.len(), 1);
    let warning = warnings[0].as_str().ok_or("warning")?;
    assert!(warning.contains("missing-worktree"));
    assert!(!warning.contains("upgrade"));
    let second = tick()?;
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout)?;
    assert!(
        second["data"]["graphs"]["warnings"]
            .as_array()
            .ok_or("warnings")?
            .is_empty()
    );
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[gc]\nschedule=true\ngraph_checkpoint_days=1\n",
    )?;
    let gc = fixture_cli()
        .current_dir(&home)
        .env("HOME", &home)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(&state)
        .args(["internal", "tick-gc"])
        .output()?;
    assert!(
        gc.status.success(),
        "{} {}",
        String::from_utf8_lossy(&gc.stdout),
        String::from_utf8_lossy(&gc.stderr)
    );
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("tick/gc-result.json"))?)?;
    assert_eq!(result["state"], "complete");
    assert!(!terminal_checkpoint.exists());
    assert!(graph_dir.join("graph-missing-worktree.json").exists());
    Ok(())
}

#[test]
fn fresh_rerun_after_cancel_starts_a_new_graph() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "seed"],
    ] {
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // A gate-only preset runs no provider, so the graph exists without a launch.
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("gate-only.toml"),
        "stages = [{ id = 'accept', kind = 'gate', condition = { type = 'budget' } }]\n",
    )?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "accept the bounded graph")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let raw = |args: &[&str]| -> Result<(bool, serde_json::Value), Box<dyn std::error::Error>> {
        let output = fixture_cli()
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(temp.path().join("state"))
            .args(args)
            .output()?;
        Ok((
            output.status.success(),
            serde_json::from_slice(&output.stdout)?,
        ))
    };
    let cli = |args: &[&str]| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let (success, value) = raw(args)?;
        assert!(success, "{args:?}: {value}");
        Ok(value)
    };
    let prompt = prompt.to_str().ok_or("prompt path")?;
    let run = [
        "run",
        "--preset",
        "gate-only",
        "--prompt-file",
        prompt,
        "--wait",
        "1s",
    ];
    let first = cli(&run)?;
    let graph_id = first["data"]["graph_id"]
        .as_str()
        .ok_or("graph id")?
        .to_owned();
    let cancelled = cli(&["fabric", "graph", "cancel", &graph_id])?;
    assert_eq!(
        cancelled["data"]["checkpoint"]["cancelled"], true,
        "{cancelled}"
    );
    // A plain rerun reattaches to the cancelled graph; --fresh starts another.
    let (_, rerun) = raw(&run)?;
    assert_eq!(rerun["data"]["graph_id"], graph_id.as_str(), "{rerun}");
    assert_eq!(rerun["status"], "cancelled", "{rerun}");
    let fresh = cli(&[&run[..], &["--fresh"]].concat())?;
    let fresh_id = fresh["data"]["graph_id"].as_str().ok_or("fresh graph id")?;
    assert_ne!(fresh_id, graph_id);
    assert_eq!(fresh["data"]["graph"]["status"], "complete", "{fresh}");
    Ok(())
}

#[test]
fn explain_pins_source_refs_before_hashing_without_creating_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command()
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
    let commit = || git(&["commit", "--allow-empty", "-qm", "advance"]);
    commit()?;
    let head = git(&["rev-parse", "HEAD"])?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "review this immutable subject")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = || -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let output = fixture_cli()
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
    // The CLI reports the project path canonically, and macOS links `/var`
    // away, so compare against the canonical spelling of the fixture file.
    let file = std::fs::canonicalize(&file)?;
    let prompt = temp.path().join("prompt");
    std::fs::write(&prompt, "brief")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // This directory is not a Git checkout, so the graph owner cannot submit.
    let output = fixture_cli()
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
    let output = fixture_cli()
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
    let output = fixture_cli()
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
fn explain_admission_matches_submission() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    std::fs::create_dir(&state)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    }
    let prompt = temp.path().join("prompt");
    std::fs::write(&prompt, "brief")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("limits.toml"),
        r#"
stages = [
  { id = "a", tier = "senior" },
  { id = "b", tier = "senior", depends_on = ["a"] },
  { id = "c", tier = "senior", depends_on = ["a"] },
  { id = "d", tier = "senior", depends_on = ["b"] },
]
"#,
    )?;
    let init = provenant_os::fixture::git_command()
        .current_dir(&project)
        .args(["init", "-q"])
        .output()?;
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let run = |explain: bool| -> Result<std::process::Output, Box<dyn std::error::Error>> {
        let mut command = fixture_cli();
        command
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(&state)
            .args(["run", "--preset", "limits", "--prompt-file"])
            .arg(&prompt);
        if explain {
            command.arg("--explain");
        }
        Ok(command.output()?)
    };
    for key in [
        "nodes",
        "edges",
        "routes_per_node",
        "node_prompt_bytes",
        "graph_prompt_bytes",
        "fan_out_width",
        "expansion_depth",
    ] {
        std::fs::write(
            state.join("config.toml"),
            format!("schema_version = 1\n[domains.fabric.graph.limits]\n{key} = 1\n"),
        )?;
        let explained = run(true)?;
        assert!(
            explained.status.success(),
            "{key}: {}",
            String::from_utf8_lossy(&explained.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&explained.stdout)?;
        assert_eq!(
            value["data"]["admission"]["status"], "refused",
            "{key}: {value}"
        );
        let reason = value["data"]["admission"]["reason"]
            .as_str()
            .ok_or("missing admission reason")?;
        assert!(reason.contains(&format!("limits.{key}")), "{key}: {reason}");
        assert!(value["data"].get("execution").is_none());
        assert!(value["data"].get("validation_error").is_none());
        let submitted = run(false)?;
        assert!(!submitted.status.success(), "{key}");
        let submitted_value: serde_json::Value = serde_json::from_slice(&submitted.stdout)?;
        let submitted_reason = submitted_value["error"]["message"]
            .as_str()
            .ok_or("missing submission refusal reason")?;
        assert_eq!(reason, submitted_reason, "{key}: {submitted_value}");
    }
    fn has_graph_journal(path: &std::path::Path) -> std::io::Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() && has_graph_journal(&entry.path())? {
                return Ok(true);
            }
            if file_type.is_file() && entry.file_name().to_string_lossy().starts_with("graph-") {
                return Ok(true);
            }
        }
        Ok(false)
    }
    assert!(!has_graph_journal(&state)?);
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
    let output = fixture_cli()
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
        fixture_cli()
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
        let mut command = fixture_cli();
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
fn preset_explain_warns_once_per_brief_path_outside_the_bound_read_roots()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let elsewhere = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    std::fs::write(source.join("inside.md"), "in-root")?;
    let outside = std::fs::canonicalize(elsewhere.path())?.join("design-quality-lens.md");
    std::fs::write(&outside, "rubric")?;
    let missing = elsewhere.path().join("missing.md");
    let prompt = temp.path().join("brief");
    std::fs::write(
        &prompt,
        format!(
            "Apply `{}` and {} again; also {} and {}.",
            outside.display(),
            outside.display(),
            source.join("inside.md").display(),
            missing.display()
        ),
    )?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = |json: bool| -> Result<std::process::Output, Box<dyn std::error::Error>> {
        let mut command = fixture_cli();
        command
            .current_dir(temp.path())
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"));
        if json {
            command.arg("--json");
        }
        command
            .arg("--state-dir")
            .arg(temp.path().join("state"))
            .args(["run", "--preset", "single", "--prompt-file"])
            .arg(&prompt)
            .arg("--read-root")
            .arg(&source)
            .arg("--explain");
        Ok(command.output()?)
    };
    let output = run(true)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let warnings = result["data"]["warnings"]
        .as_array()
        .ok_or("warnings must travel as data")?;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let text = warnings[0].as_str().ok_or("warning text")?;
    assert!(text.contains(&outside.display().to_string()), "{text}");
    assert!(text.contains("--read-root"), "{text}");
    let human = String::from_utf8(run(false)?.stdout)?;
    assert_eq!(
        human.matches("warning: brief references").count(),
        1,
        "{human}"
    );
    assert!(!temp.path().join("state").exists());

    // A brief that cites only in-root and missing paths prints no warning.
    std::fs::write(
        &prompt,
        format!(
            "See {} and {}.",
            source.join("inside.md").display(),
            missing.display()
        ),
    )?;
    let quiet: serde_json::Value = serde_json::from_slice(&run(true)?.stdout)?;
    assert!(quiet["data"].get("warnings").is_none(), "{quiet}");
    Ok(())
}

#[test]
fn run_refuses_a_read_root_outside_the_project() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "Read elsewhere")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = fixture_cli()
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
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
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
        let output = fixture_cli()
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
        fixture_cli()
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
    let output = fixture_cli()
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
        &["commit", "-q", "--allow-empty", "-m", "base"],
    ] {
        assert!(
            provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .status()?
                .success()
        );
    }
    // A small change on top of the base, so a review stage can measure it.
    std::fs::write(project.join("change.txt"), "one line\n")?;
    for args in [
        &["add", "change.txt"][..],
        &["commit", "-q", "-m", "change"],
    ] {
        assert!(
            provenant_os::fixture::git_command()
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
    let checkout = project.canonicalize()?.display().to_string();
    let output = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args(["run", "--preset", preset, "--prompt-file"])
        .arg(&prompt)
        // `{project}` names the throwaway checkout, a registered worktree.
        .args(extra.iter().map(|arg| arg.replace("{project}", &checkout)))
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
    golden_in("assets/presets/fixtures", name, actual)
}

// Prompt lengths include the checkout path before the displayed path is scrubbed.
fn scrub_prompt_bytes(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.starts_with("Inspection: ")
                && let Some(start) = line.find("\"prompt_bytes\":")
            {
                let value_start = start + "\"prompt_bytes\":".len();
                let value_end = value_start
                    + line[value_start..]
                        .bytes()
                        .take_while(u8::is_ascii_digit)
                        .count();
                let mut line = line.to_owned();
                line.replace_range(value_start..value_end, "<bytes>");
                line
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn golden_in(dir: &str, name: &str, actual: &str) -> Result<(), Box<dyn std::error::Error>> {
    let actual = format!("{}\n", actual.trim_end());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(dir)
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().ok_or("golden path has no parent")?)?;
        std::fs::write(&path, &actual)?;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        scrub_prompt_bytes(&actual),
        scrub_prompt_bytes(&expected),
        "{name}: rerun with UPDATE_GOLDEN=1 to refresh"
    );
    Ok(())
}

#[test]
fn preset_list_and_show_human_output_match_goldens() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let run = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let out = fixture_cli()
            .current_dir(&root)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path())
            .args(args)
            .output()?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(scrub(&String::from_utf8(out.stdout)?, &root))
    };
    let list = run(&["preset", "list"])?;
    for entry in
        std::fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/presets"))?
    {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or("preset name")?;
            assert!(
                list.lines().any(|line| {
                    let mut columns = line.split_whitespace();
                    columns.next() == Some(name) && columns.next() == Some("bundled")
                }),
                "missing bundled {name}: {list}"
            );
        }
    }
    golden_in(
        "tests/golden",
        "preset-show-scout-swarm.txt",
        &run(&["preset", "show", "scout-swarm"])?,
    )?;
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
            "parameters.risk_tier=\"crucial\"",
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
    assert!(text.contains("family=cross_preferred"), "{text}");
    assert!(text.contains("family=distinct_from_writer"), "{text}");
    golden("review-council.explain.txt", &text)?;
    Ok(())
}

#[test]
fn scout_swarm_and_generator_evaluator_explain_goldens() -> Result<(), Box<dyn std::error::Error>> {
    for (name, args, nodes, edges, max_attempts) in [
        ("scout-swarm", &["--plan", "off"][..], 7, 6, 12),
        (
            "generator-evaluator",
            &["--set", "parameters.worktree=\".\""][..],
            3,
            3,
            3,
        ),
    ] {
        let text = explain_text(name, args)?;
        let field = |prefix: &str| text.lines().find_map(|line| line.strip_prefix(prefix));
        let admission: serde_json::Value =
            serde_json::from_str(field("Admission: ").ok_or("missing admission")?)?;
        let inspection: serde_json::Value =
            serde_json::from_str(field("Inspection: ").ok_or("missing inspection")?)?;
        assert_eq!(admission["status"], "admitted", "{text}");
        assert!(
            text.contains(&format!("Preset selection: {name} (override)")),
            "{text}"
        );
        assert_eq!(inspection["nodes"], nodes, "{text}");
        assert_eq!(inspection["edges"], edges, "{text}");
        assert_eq!(inspection["max_attempts"], max_attempts, "{text}");
    }
    Ok(())
}

#[test]
fn remaining_bundled_patterns_have_explain_goldens() -> Result<(), Box<dyn std::error::Error>> {
    let worktree = ["--plan", "off", "--set", "parameters.worktree=\".\""];
    let batch = ["--plan", "off", "--set", "parameters.batch_worktree=\".\""];
    // The review stage measures the last commit against its parent.
    let deliver = [
        "--set",
        "parameters.worktree=\".\"",
        "--set",
        "parameters.base=\"HEAD~1\"",
    ];
    for (name, extra) in [
        ("single", &[][..]),
        ("chain", &[]),
        ("fan-out", &["--plan", "off"][..]),
        ("fan-out-synthesise", &["--plan", "off"]),
        ("big-batch", &batch[..]),
        ("scope-then-run", &worktree[..]),
        ("deliver", &deliver[..]),
    ] {
        let explained = explain_text(name, extra)?;
        if name == "single" {
            let inspection = explained
                .lines()
                .find_map(|line| line.strip_prefix("Inspection: "))
                .ok_or("missing inspection")?;
            let inspection: serde_json::Value = serde_json::from_str(inspection)?;
            assert_eq!(inspection["prompt_bytes"], 10);
        }
        if name == "big-batch" {
            assert!(explained.contains("plan: off -> off"), "{explained}");
            assert!(!explained.contains("Planner route"), "{explained}");
            assert!(explained.contains("writer worktree"), "{explained}");
            continue;
        }
        golden(&format!("{name}.explain.txt"), &explained)?;
    }
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
    let output = fixture_cli()
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

/// A route-less ready prompt selects a preset by rules. An explicit network
/// restriction reaches every graph lane it submits, and single-lane flags the
/// graph cannot apply are refused before selection.
#[test]
fn automatic_preset_selection_submits_network_off_and_refuses_lane_only_flags()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "seed"],
    ] {
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(output.status.success());
    }
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        project.join(".provenant/preset-rules.toml"),
        "[select]\ndefault = { preset = 'probe' }\n",
    )?;
    std::fs::write(
        presets.join("probe.toml"),
        "stages = [{ id = 'task', tier = { adapter = 'fixture', model = 'fixture/model' } }]\n",
    )?;
    let prompt = project.join("ready.md");
    std::fs::write(
        &prompt,
        "Acceptance criteria are explicit. Surface: the run command. Risk tier: low. \
         Verification uses cargo test. Design decision is settled.\n",
    )?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let state = temp.path().join("state");
    let run = |extra: &[&str]| {
        fixture_cli()
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(&state)
            .args(["run", "--prompt-file"])
            .arg(&prompt)
            .args(extra)
            .output()
    };
    // Flags that choose a model, route or manifest bypass automatic selection;
    // all other independently usable incompatible flags reach this boundary.
    for args in [
        vec!["--effort", "high"],
        vec!["--account", "fixture"],
        vec!["--fallback", "codex/model"],
        vec!["--mode", "read-only"],
        vec!["--worktree-branch", "topic"],
        vec!["--host", "peer"],
        vec!["--deliver", "none"],
        vec!["--write-root", "/tmp"],
        vec!["--capability", "postgres"],
        vec!["--knob", "test=value"],
        vec!["--delivery-id", "11111111111111111111111111111111"],
        vec!["--head", "1111111111111111111111111111111111111111"],
        vec!["--base", "main"],
        vec!["--title", "title"],
        vec!["--build-heavy"],
        vec!["--read-heavy"],
        vec!["--confidential"],
        vec!["--provider-timeout", "3600"],
        vec!["--operation-id", "chosen"],
        vec!["--branch", "topic", "--host", "peer", "--base", "main"],
    ] {
        let automatic = run(&args)?;
        let result: serde_json::Value = serde_json::from_slice(&automatic.stdout)?;
        assert_eq!(result["error"]["code"], "preset", "{args:?}: {result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .is_some_and(|s| s.contains(args[0])),
            "{args:?}: {result}"
        );
        let mut explicit_args = vec!["--preset", "probe"];
        explicit_args.extend(&args);
        let explicit = run(&explicit_args)?;
        assert!(!explicit.status.success(), "{args:?}");
        let result: serde_json::Value = serde_json::from_slice(&explicit.stdout)?;
        assert_eq!(result["status"], "invalid", "{args:?}: {result}");
    }
    let stdin = fixture_cli()
        .current_dir(&project)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args(["run", "--prompt-stdin"])
        .output()?;
    let result: serde_json::Value = serde_json::from_slice(&stdin.stdout)?;
    assert_eq!(result["error"]["code"], "preset", "{result}");
    assert!(
        result["error"]["message"]
            .as_str()
            .is_some_and(|s| s.contains("--prompt-stdin")),
        "{result}"
    );
    let refused = run(&["--account", "fixture", "--effort", "high"])?;
    assert!(!refused.status.success());
    let refused: serde_json::Value = serde_json::from_slice(&refused.stdout)?;
    assert_eq!(refused["status"], "invalid", "{refused}");
    assert_eq!(refused["error"]["code"], "preset", "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("--effort, --account")),
        "{refused}"
    );
    assert!(
        refused["error"]["repair"]
            .as_str()
            .is_some_and(|repair| repair.contains("--model or --route")),
        "{refused}"
    );
    assert!(!state.join("runs").exists());

    let output = run(&["--network", "off", "--wait", "5s"])?;
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let document = result["data"]["document"]
        .as_str()
        .ok_or_else(|| format!("retained document: {result}"))?;
    let document: serde_json::Value = serde_json::from_slice(&std::fs::read(document)?)?;
    assert_eq!(document["network"], "off");
    let mut submitted = Vec::new();
    for run in std::fs::read_dir(state.join("runs"))? {
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run?.path().join("request.json"))?)?;
        for task in request["tasks"].as_array().ok_or("tasks")? {
            submitted.push(task["access"]["network"].clone());
        }
    }
    // `--network off` keeps only the provider's own endpoints.
    assert_eq!(submitted, [serde_json::json!("provider")], "{result}");
    Ok(())
}

/// `run --exclude` on an automatically selected graph is refused when a
/// selector is unknown, and otherwise binds the list to the graph so every
/// node's task carries it as the graph's exclusion knob.
#[test]
fn graph_exclusions_are_validated_and_inherited_by_every_node()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "seed"],
    ] {
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(output.status.success());
    }
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        project.join(".provenant/preset-rules.toml"),
        "[select]\ndefault = { preset = 'probe' }\n",
    )?;
    std::fs::write(
        presets.join("probe.toml"),
        "stages = [{ id = 'task', tier = { adapter = 'fixture', model = 'fixture/model' } }]\n",
    )?;
    let prompt = project.join("ready.md");
    std::fs::write(
        &prompt,
        "Acceptance criteria are explicit. Surface: the run command. Risk tier: low. \
         Verification uses cargo test. Design decision is settled.\n",
    )?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let state = temp.path().join("state");
    let run = |extra: &[&str]| {
        fixture_cli()
            .current_dir(&project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(&state)
            .args(["run", "--prompt-file"])
            .arg(&prompt)
            .args(extra)
            .output()
    };
    let refused = run(&["--exclude", "nosuchprovider"])?;
    assert!(!refused.status.success());
    let refused: serde_json::Value = serde_json::from_slice(&refused.stdout)?;
    assert_eq!(refused["status"], "invalid", "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("adapter/id")),
        "{refused}"
    );
    assert!(!state.join("runs").exists());

    let output = run(&["--exclude", "claude", "--wait", "5s"])?;
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let document = result["data"]["document"]
        .as_str()
        .ok_or_else(|| format!("retained document: {result}"))?;
    let document: serde_json::Value = serde_json::from_slice(&std::fs::read(document)?)?;
    assert_eq!(document["exclude"], serde_json::json!(["claude"]));
    let mut carried = Vec::new();
    for run in std::fs::read_dir(state.join("runs"))? {
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run?.path().join("request.json"))?)?;
        for task in request["tasks"].as_array().ok_or("tasks")? {
            carried.push(task["profile"]["knobs"]["routing.graph_exclude"].clone());
        }
    }
    assert_eq!(carried, [serde_json::json!(["claude"])], "{result}");
    Ok(())
}

/// A project whose automatic selection picks `gate-only`, with `config` as the
/// state's `domains.fabric` settings. Gate presets finish without a provider.
struct ExperimentProject {
    temp: tempfile::TempDir,
    project: PathBuf,
    state: PathBuf,
    prompt: PathBuf,
}

impl ExperimentProject {
    fn new(config: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        std::fs::create_dir(&project)?;
        for args in [
            vec!["init", "-q"],
            vec!["commit", "--allow-empty", "-qm", "seed"],
        ] {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(output.status.success());
        }
        let presets = project.join(".provenant/presets");
        std::fs::create_dir_all(&presets)?;
        std::fs::write(
            project.join(".provenant/preset-rules.toml"),
            "[select]\ndefault = { preset = 'gate-only' }\n",
        )?;
        std::fs::write(
            presets.join("gate-only.toml"),
            "stages = [{ id = 'accept', kind = 'gate', condition = { type = 'budget' } }]\n",
        )?;
        std::fs::write(
            presets.join("gate-alt.toml"),
            "stages = [{ id = 'review', kind = 'gate', condition = { type = 'budget' } }]\n",
        )?;
        let state = temp.path().join("state");
        std::fs::create_dir(&state)?;
        std::fs::write(
            state.join("config.toml"),
            format!("schema_version = 1\n{config}"),
        )?;
        // Underspecified work takes the readiness route, so the brief states its criteria.
        let prompt = project.join("ready.md");
        std::fs::write(
            &prompt,
            "Acceptance criteria are explicit. Surface: the run command. Risk tier: low. \
             Verification uses cargo test. Design decision is settled.\n",
        )?;
        Ok(Self {
            temp,
            project,
            state,
            prompt,
        })
    }

    fn run(&self, extra: &[&str]) -> std::io::Result<std::process::Output> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        fixture_cli()
            .current_dir(&self.project)
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(&self.state)
            .args(["run", "--prompt-file"])
            .arg(&self.prompt)
            .args(["--wait", "1s"])
            .args(extra)
            .output()
    }

    /// The graph the run would submit, plus its stderr. `--explain` stops after
    /// placement and binding, so these tests need no provider or graph state.
    fn document(
        &self,
        extra: &[&str],
    ) -> Result<(serde_json::Value, String), Box<dyn std::error::Error>> {
        let mut args = vec!["--explain"];
        args.extend_from_slice(extra);
        let output = self.run(&args)?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "{}\n{stderr}",
            String::from_utf8_lossy(&output.stdout)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok((result["data"]["graph"].clone(), stderr))
    }
}

const WORK_ITEM: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn task_id_assigns_the_candidate_arm_and_labels_the_run() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = ExperimentProject::new(
        "[domains.fabric.pipelines.experiment.trial]\ncontrol = 'gate-only'\narms = { candidate = 'gate-alt' }\nassign = 'epsilon'\nepsilon = 1.0\n",
    )?;
    let (document, stderr) =
        fixture.document(&["--task-id", WORK_ITEM, "--risk-tier", "routine"])?;
    assert!(!stderr.contains("experiment"), "{stderr}");
    assert_eq!(document["context"]["experiment_id"], "trial", "{document}");
    assert_eq!(
        document["context"]["experiment_arm"], "candidate",
        "{document}"
    );
    assert_eq!(
        document["context"]["pattern"], "experiment:trial:candidate",
        "{document}"
    );
    assert_eq!(document["context"]["work_item"], WORK_ITEM, "{document}");
    Ok(())
}

#[test]
fn shadow_executes_the_control_graph_and_records_the_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let shadowed = ExperimentProject::new(
        "[domains.fabric.pipelines.experiment.trial]\ncontrol = 'gate-only'\narms = { candidate = 'gate-alt' }\nassign = 'epsilon'\nepsilon = 1.0\nshadow = true\n",
    )?;
    let plain = ExperimentProject::new("")?;
    let (shadow, _) = shadowed.document(&["--task-id", WORK_ITEM, "--risk-tier", "routine"])?;
    let (control, _) = plain.document(&["--task-id", WORK_ITEM, "--risk-tier", "routine"])?;
    assert_eq!(shadow["context"]["experiment_arm"], "control", "{shadow}");
    assert_eq!(shadow["context"]["shadow_of"], "candidate", "{shadow}");
    assert!(
        control["context"].get("experiment_arm").is_none(),
        "{control}"
    );
    // Shadow labels differ, but the executed graph is the control graph.
    assert_eq!(shadow["nodes"], control["nodes"], "{shadow}");
    Ok(())
}

#[test]
fn invalid_entry_warns_by_name_and_the_valid_one_still_applies()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = ExperimentProject::new(
        "[domains.fabric.pipelines.experiment.broken]\ncontrol = 'gate-only'\n\n[domains.fabric.pipelines.experiment.trial]\ncontrol = 'gate-only'\narms = { candidate = 'gate-alt' }\nassign = 'epsilon'\nepsilon = 1.0\n",
    )?;
    let (document, stderr) =
        fixture.document(&["--task-id", WORK_ITEM, "--risk-tier", "routine"])?;
    assert!(stderr.contains("broken"), "{stderr}");
    assert_eq!(document["context"]["experiment_id"], "trial", "{document}");
    Ok(())
}

#[test]
fn unknown_arm_preset_and_unlisted_tier_are_named_in_the_run()
-> Result<(), Box<dyn std::error::Error>> {
    let unknown = ExperimentProject::new(
        "[domains.fabric.pipelines.experiment.trial]\ncontrol = 'gate-only'\narms = { candidate = 'no-such-preset' }\nassign = 'epsilon'\nepsilon = 1.0\n",
    )?;
    let (document, stderr) =
        unknown.document(&["--task-id", WORK_ITEM, "--risk-tier", "routine"])?;
    assert!(stderr.contains("no-such-preset"), "{stderr}");
    assert!(
        document["context"].get("experiment_arm").is_none(),
        "{document}"
    );

    let tiered = ExperimentProject::new(
        "[domains.fabric.pipelines.experiment.trial]\ncontrol = 'gate-only'\narms = { candidate = 'gate-alt' }\nassign = 'epsilon'\nepsilon = 1.0\ntiers = ['crucial']\n",
    )?;
    let (document, _) = tiered.document(&["--task-id", WORK_ITEM, "--risk-tier", "routine"])?;
    assert!(
        document["context"].get("experiment_arm").is_none(),
        "{document}"
    );
    assert!(
        document["provenance"]["experiments"]
            .as_array()
            .is_some_and(|skips| skips
                .iter()
                .any(|skip| skip.as_str().is_some_and(|s| s.contains("trial")))),
        "{document}"
    );
    Ok(())
}

#[test]
fn explicit_preset_and_no_task_id_never_read_experiments() -> Result<(), Box<dyn std::error::Error>>
{
    // The table is invalid, so any read of it would produce a warning.
    let fixture = ExperimentProject::new(
        "[domains.fabric.pipelines.experiment.broken]\ncontrol = 'gate-only'\n",
    )?;
    let (document, stderr) = fixture.document(&[])?;
    assert!(!stderr.contains("broken"), "{stderr}");
    assert!(
        document["context"].get("experiment_arm").is_none(),
        "{document}"
    );
    let output = fixture.run(&["--preset", "gate-only", "--task-id", WORK_ITEM])?;
    assert!(!output.status.success());
    Ok(())
}

#[test]
fn research_presets_have_plan_on_explain_goldens() -> Result<(), Box<dyn std::error::Error>> {
    let worktree = ["--plan", "on", "--set", "parameters.worktree=\".\""];
    for (name, extra) in [
        ("scout-swarm", &["--plan", "on"][..]),
        ("fan-out", &["--plan", "on"]),
        ("fan-out-synthesise", &["--plan", "on"]),
        ("scope-then-run", &worktree[..]),
    ] {
        let text = explain_text(name, extra)?;
        assert!(
            text.contains("plan: on -> on (planning requested)"),
            "{text}"
        );
        assert!(text.contains("Planner route: principal"), "{text}");
        golden(&format!("{name}.plan-on.explain.txt"), &text)?;
    }
    Ok(())
}

#[test]
fn plan_flag_decides_and_explains_the_planner_front_end() -> Result<(), Box<dyn std::error::Error>>
{
    let batch = [
        "--set",
        "parameters.batch_worktree=\"{project}\"",
        "--set",
        "parameters.worktrees=[\"{project}\"]",
    ];
    let with = |mode: &str| -> Vec<String> {
        let mut args: Vec<String> = vec!["--plan".into(), mode.into()];
        args.extend(batch.iter().map(|s| (*s).to_owned()));
        args
    };
    let run = |mode: &str| -> Result<String, Box<dyn std::error::Error>> {
        let owned = with(mode);
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        explain_text("big-batch", &args)
    };
    let on = run("on")?;
    assert!(on.contains("plan: on -> on (planning requested)"), "{on}");
    assert!(on.contains("Planner route: principal"), "{on}");
    assert!(
        on.contains("deadline_seconds: \"${budget_seconds}\""),
        "{on}"
    );
    golden("big-batch.plan-on.explain.txt", &on)?;
    let off = run("off")?;
    assert!(
        off.contains("plan: off -> off (planning turned off)"),
        "{off}"
    );
    assert!(!off.contains("Planner route"), "{off}");
    // Default `auto` reads the brief: a short one with no signals stays static.
    let auto = run("auto")?;
    assert!(auto.contains("plan: auto -> "), "{auto}");
    let plain = explain_text("big-batch", &batch[..])?;
    assert!(plain.contains("plan: auto -> "), "{plain}");
    Ok(())
}

#[test]
fn default_config_expands_big_batch() -> Result<(), Box<dyn std::error::Error>> {
    let text = explain_text(
        "big-batch",
        &[
            "--plan",
            "off",
            "--set",
            "parameters.batch_worktree=\"{project}\"",
            "--set",
            "parameters.worktrees=[\"{project}\"]",
            "--set",
            "parameters.lanes=[{name=\"one\",worktree=\"{project}\",scope=[\".\"],spec=\"\"}]",
        ],
    )?;
    assert!(text.contains("big-batch"), "{text}");
    assert!(!text.contains("Requires configuration"), "{text}");
    assert!(!text.contains("Execution: null"), "{text}");
    Ok(())
}

#[test]
fn preset_run_followed_by_provenant_run_on_same_bound_worktree_succeeds()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let state = temp.path().join("state");
    std::fs::create_dir(&project)?;
    let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    git(&["init", "-q"])?;
    std::fs::write(
        project.join(".gitignore"),
        ".worktrees/\n.provenant/\ntarget/\n",
    )?;
    git(&["add", ".gitignore"])?;
    git(&["commit", "-qm", "initial"])?;
    let worktree = project.join(".worktrees/bound-lane");

    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("gate-only.toml"),
        "stages = [{ id = 'accept', kind = 'gate', condition = { type = 'budget' } }]\n[parameters]\nworktree = ''\n",
    )?;
    let prompt = project.join("brief");
    std::fs::write(&prompt, "accept the bounded graph")?;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n",
    )?;
    let run_cli = |args: &[&str]| -> std::io::Result<std::process::Output> {
        fixture_cli()
            .current_dir(&project)
            .env("HOME", &home)
            .env("CODEX_HOME", home.join(".codex"))
            .env("PATH", "/usr/bin:/bin")
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(&state)
            .args(args)
            .output()
    };

    // Create a lane-owned worktree and let its initial attempt settle. This
    // also makes the fixture robust on hosts with an incomplete process census.
    let initial = run_cli(&[
        "run",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "bound-lane",
        "--deliver",
        "none",
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--wait",
        "10s",
    ])?;
    let initial_json: serde_json::Value = serde_json::from_slice(&initial.stdout)?;
    let run_id = initial_json["data"]["run_id"]
        .as_str()
        .ok_or_else(|| format!("initial writer not admitted: {initial_json}"))?;
    let settled = run_cli(&["lanes", "--id", run_id, "--all", "--wait", "10s"])?;
    assert!(
        settled.status.success(),
        "initial writer did not settle: {}",
        String::from_utf8_lossy(&settled.stdout)
    );

    // First: run a preset bound to the worktree.
    let preset_run = run_cli(&[
        "run",
        "--preset",
        "gate-only",
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--set",
        &format!("parameters.worktree='{}'", worktree.display()),
        "--wait",
        "1s",
    ])?;
    assert!(
        preset_run.status.success(),
        "preset run failed: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&preset_run.stdout),
        String::from_utf8_lossy(&preset_run.stderr)
    );

    // The bound worktree has ignored files written into scratch,
    // and potentially target/ build files.
    std::fs::create_dir_all(worktree.join("target/debug"))?;
    std::fs::write(worktree.join("target/debug/output.bin"), "bin")?;
    std::fs::create_dir_all(worktree.join(".provenant/scratch/graphs"))?;
    std::fs::write(worktree.join(".provenant/scratch/graphs/graph.json"), "{}")?;

    let status = provenant_os::fixture::git_command()
        .current_dir(&worktree)
        .args(["status", "--porcelain"])
        .output()?;
    assert!(status.status.success());
    assert!(
        status.stdout.is_empty(),
        "worktree has non-ignored changes: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    // Claim the existing branch as a writer after the preset, exercising ownership
    // admission with ignored files present rather than just a gate-only graph.
    let writer = run_cli(&[
        "dispatch",
        "--model",
        "codex/gpt-6-luna",
        "--effort",
        "low",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "bound-lane",
        "--deliver",
        "none",
        "--prompt-file",
        prompt.to_str().unwrap(),
    ])?;
    assert!(
        writer.status.success(),
        "writer admission failed: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&writer.stdout),
        String::from_utf8_lossy(&writer.stderr)
    );
    let writer_json: serde_json::Value = serde_json::from_slice(&writer.stdout)?;
    assert!(writer_json["data"]["run_id"].is_string(), "{writer_json}");
    assert_eq!(
        writer_json["data"]["worktree"]["owned"], true,
        "{writer_json}"
    );
    assert_eq!(
        writer_json["data"]["worktree"]["branch"], "bound-lane",
        "{writer_json}"
    );

    // Second: provenant run on the same bound worktree succeeds despite ignored files.
    let run_again = run_cli(&[
        "run",
        "--preset",
        "gate-only",
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--set",
        &format!("parameters.worktree='{}'", worktree.display()),
        "--wait",
        "1s",
    ])?;
    assert!(
        run_again.status.success(),
        "second run failed: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run_again.stdout),
        String::from_utf8_lossy(&run_again.stderr)
    );

    // Also verify running from within the bound worktree directory succeeds.
    let worktree_presets = worktree.join(".provenant/presets");
    std::fs::create_dir_all(&worktree_presets)?;
    std::fs::copy(
        presets.join("gate-only.toml"),
        worktree_presets.join("gate-only.toml"),
    )?;
    let run_in_worktree = fixture_cli()
        .current_dir(&worktree)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .args(["--json", "--state-dir"])
        .arg(&state)
        .args([
            "run",
            "--preset",
            "gate-only",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--set",
            &format!("parameters.worktree='{}'", worktree.display()),
            "--wait",
            "1s",
        ])
        .output()?;
    assert!(
        run_in_worktree.status.success(),
        "run inside bound worktree failed: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run_in_worktree.stdout),
        String::from_utf8_lossy(&run_in_worktree.stderr)
    );

    Ok(())
}

#[test]
fn preset_refuses_readable_state_directories_with_chmod_repair()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let state = temp.path().join("state with 'quotes'");
    let home = temp.path().join("home");
    std::fs::create_dir(&project)?;
    std::fs::create_dir(&state)?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n",
    )?;
    std::fs::create_dir(&home)?;
    let git = provenant_os::fixture::git_command()
        .current_dir(&project)
        .args(["init", "-q"])
        .output()?;
    assert!(git.status.success(), "{git:?}");
    let commit = provenant_os::fixture::git_command()
        .current_dir(&project)
        .args(["commit", "--allow-empty", "-qm", "initial"])
        .output()?;
    assert!(commit.status.success(), "{commit:?}");
    let presets = project.join(".provenant/presets");
    std::fs::create_dir_all(&presets)?;
    std::fs::write(
        presets.join("gate-only.toml"),
        "stages = [{ id = 'accept', kind = 'gate', condition = { type = 'budget' } }]\n",
    )?;
    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "accept the bounded graph")?;
    let run = || -> std::io::Result<std::process::Output> {
        fixture_cli()
            .current_dir(&project)
            .env("HOME", &home)
            .env(
                "PROVENANT_ROOT",
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
            )
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(["--json", "--state-dir"])
            .arg(&state)
            .args(["run", "--preset", "gate-only", "--prompt-file"])
            .arg(&prompt)
            .args(["--wait", "10s"])
            .output()
    };
    for path in [&state, &state.join("tmp")] {
        std::fs::create_dir_all(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        let output = run()?;
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert!(!output.status.success(), "{value}");
        let message = value["error"]["message"].as_str().ok_or("message")?;
        assert!(
            message.contains(&std::fs::canonicalize(path)?.display().to_string()),
            "{value}"
        );
        assert!(
            message.contains("0755") && message.contains("other users can read"),
            "{value}"
        );
        let repair = value["error"]["repair"].as_str().ok_or("chmod repair")?;
        assert!(repair.contains("chmod 700"), "{value}");
        // Execute the suggested command to cover paths with spaces and quotes.
        let repaired = Command::new("/bin/sh")
            .arg("-c")
            .arg(
                repair
                    .strip_prefix("run ")
                    .ok_or("repair prefix")?
                    .strip_suffix(" and retry")
                    .ok_or("repair suffix")?,
            )
            .status()?;
        assert!(repaired.success());
    }
    let output = run()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(())
}

#[test]
fn preset_file_spec_resolves_from_checkout_subdirectory() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let subdir = project.join("nested");
    std::fs::create_dir_all(&subdir)?;
    assert!(
        provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(["init", "-q"])
            .status()?
            .success()
    );
    assert!(
        provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(["commit", "--allow-empty", "-qm", "initial",])
            .status()?
            .success()
    );
    std::fs::write(project.join("spec.md"), "root acceptance criteria")?;
    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "review")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = fixture_cli()
        .current_dir(&subdir)
        .env("PROVENANT_ROOT", &root)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .args([
            "--json",
            "run",
            "--preset",
            "review-council",
            "--prompt-file",
        ])
        .arg(&prompt)
        .args(["--explain", "--set", "parameters.spec='spec.md'"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let output = String::from_utf8(output.stdout)?;
    assert!(output.contains("root acceptance criteria"), "{output}");
    Ok(())
}

#[test]
fn preset_missing_spec_warns_in_explain_and_launch() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    std::fs::create_dir(temp.path().join("home"))?;
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "initial"],
    ] {
        assert!(
            provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .status()?
                .success()
        );
    }
    let writer = temp.path().join("writer");
    let batch = temp.path().join("batch");
    for (branch, worktree) in [("writer", &writer), ("batch", &batch)] {
        assert!(
            provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(["worktree", "add", "-q", "-b", branch])
                .arg(worktree)
                .status()?
                .success()
        );
    }
    std::fs::create_dir_all(writer.join(".agent-run"))?;
    std::fs::write(writer.join(".agent-run/missing.md"), "lane-only criteria")?;
    let prompt = temp.path().join("brief");
    std::fs::write(&prompt, "review")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (preset, explain) in [
        ("review-council", true),
        ("review-council", false),
        ("big-batch", true),
        ("big-batch", false),
    ] {
        let mut command = fixture_cli();
        command
            .current_dir(&project)
            .env("HOME", temp.path().join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("PROVENANT_ROOT", &root)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .arg("--state-dir")
            .arg(temp.path().join("state"))
            .args(["run", "--preset", preset, "--prompt-file"])
            .arg(&prompt)
            .args(["--set", "parameters.spec='.agent-run/missing.md'"]);
        if preset == "big-batch" {
            command.args([
                "--plan",
                "on",
                "--set",
                &format!("parameters.batch_worktree='{}'", batch.display()),
                "--set",
                &format!("parameters.worktrees=['{}']", writer.display()),
            ]);
        }
        if explain {
            command.arg("--explain");
        } else {
            command.args(["--wait", "1s"]);
        }
        let output = command.output()?;
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains("spec file '.agent-run/missing.md' could not be read"),
            "{preset}, explain={explain}: {stderr} / {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            stderr.contains("pass an issue-number spec instead"),
            "{stderr}"
        );
    }
    Ok(())
}
