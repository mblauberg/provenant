mod support;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn rig() -> Result<TestRig> {
    let rig = TestRig::new()?;
    std::fs::create_dir_all(rig.project.join("config"))?;
    std::fs::write(
        rig.project.join("config/telemetry-prices.toml"),
        include_str!("../../../config/telemetry-prices.toml"),
    )?;
    // TestRig's executor supplies a clean environment, including for Git.
    let git = provenant_os::git::git_executable().ok_or("Git")?;
    let output = rig.execute(
        &git,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Initial",
        ],
        None,
    )?;
    assert_eq!(code(&output), 0, "{output:?}");
    Ok(rig)
}

#[test]
fn offline_pipeline_preserves_unattributed_and_unmeasured_evidence() -> Result {
    let rig = rig()?;
    let manifest = rig.project.join("tasks.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({"schema_version":1,"tasks":[{
            "prompt":"Review a slice", "context":{"graph_id":"retained-graph","node_id":"review","pattern":"council","role":"correctness"},
            "route":{"kind":"model","provider":"codex","model":"gpt-6-luna"}
        }]}))?,
    )?;
    let output = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("path")?,
        "--operation-id",
        "pipeline-fixture",
        "--json",
    ])?;
    let dispatched: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(dispatched["data"]["run_id"].is_string(), "{dispatched}");
    let output = rig.command(&[
        "telemetry",
        "stats",
        "--outcomes",
        "--main-ref",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let pipeline = &report["data"]["pipeline"];
    assert_eq!(pipeline["outcomes"]["ci"], "not_measured", "{report}");
    assert_eq!(pipeline["outcomes"]["unattributed_rows"], 1);
    assert_eq!(
        pipeline["outcomes"]["rows"][0]["evidence"]["pattern"],
        "council"
    );
    assert!(pipeline["outcomes"]["rows"][0]["signals"]["ci_failures"].is_null());
    assert_eq!(pipeline["ranking"]["min_samples"], 20);
    assert_eq!(pipeline["ranking"]["arms"][0]["risk_tier"], "unknown");
    assert_eq!(pipeline["ranking"]["arms"][0]["status"], "insufficient");
    assert_eq!(pipeline["ranking"]["suggestions"], json!([]));
    let ci = rig.project.join("ci.json");
    std::fs::write(&ci, "[]")?;
    let output = rig.command(&[
        "telemetry",
        "stats",
        "--suggest",
        "--main-ref",
        "HEAD",
        "--ci-file",
        ci.to_str().ok_or("path")?,
        "--min-samples",
        "3",
        "--outcome-days",
        "7",
        "--expiry-days",
        "2",
        "--json",
    ])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"]["pipeline"]["outcomes"]["ci"], "measured");
    assert_eq!(report["data"]["pipeline"]["outcomes"]["window_days"], 7);
    assert_eq!(report["data"]["pipeline"]["ranking"]["min_samples"], 3);
    assert_eq!(report["data"]["pipeline"]["ranking"]["expiry_days"], 2);
    assert!(!rig.state.join("overrides.json").exists());
    Ok(())
}

#[test]
fn unreadable_graph_checkpoint_does_not_abort_pipeline() -> Result {
    let rig = rig()?;
    let manifest = rig.project.join("unreadable-graph.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({"schema_version":1,"tasks":[{
            "prompt":"Review a slice", "context":{"graph_id":"invalid/graph","node_id":"review"},
            "route":{"kind":"model","provider":"codex","model":"gpt-6-luna"}
        }]}))?,
    )?;
    let output = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("path")?,
        "--operation-id",
        "unreadable-graph-fixture",
        "--json",
    ])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let output = rig.command(&[
        "telemetry",
        "stats",
        "--outcomes",
        "--main-ref",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(report["data"]["pipeline"]["outcomes"]["rows"][0]["evidence"]["confirmed"].is_null());
    assert_eq!(
        report["data"]["pipeline"]["outcomes"]["rows"][0]["stratum"],
        "unmeasured"
    );
    Ok(())
}

#[test]
fn repeated_node_labels_without_a_graph_preserve_each_unattributed_row() -> Result {
    let rig = rig()?;
    let manifest = rig.project.join("unattributed.json");
    let task = json!({"prompt":"Review a slice", "context":{"node_id":"review"},
        "route":{"kind":"model","provider":"codex","model":"gpt-6-luna"}});
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({"schema_version":1,"tasks":[task,task]}))?,
    )?;
    let output = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("path")?,
        "--operation-id",
        "unattributed-fixture",
        "--json",
    ])?;
    let dispatched: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(dispatched["data"]["run_id"].is_string(), "{dispatched}");
    let output = rig.command(&[
        "telemetry",
        "stats",
        "--outcomes",
        "--main-ref",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        report["data"]["pipeline"]["outcomes"]["unattributed_rows"], 2,
        "{report}"
    );
    Ok(())
}

#[test]
fn pipeline_refuses_malformed_ci_and_invalid_configuration() -> Result {
    let rig = rig()?;
    let ci = rig.project.join("bad-ci.json");
    std::fs::write(&ci, "[{\"conclusion\":\"failure\"}]")?;
    let output = rig.command(&[
        "telemetry",
        "stats",
        "--outcomes",
        "--main-ref",
        "HEAD",
        "--ci-file",
        ci.to_str().ok_or("path")?,
        "--json",
    ])?;
    assert_ne!(code(&output), 0);
    for args in [
        vec![
            "telemetry",
            "stats",
            "--suggest",
            "--min-samples",
            "0",
            "--json",
        ],
        vec![
            "telemetry",
            "stats",
            "--outcomes",
            "--outcome-days",
            "0",
            "--json",
        ],
        vec!["telemetry", "stats", "--outcomes", "--usage", "--json"],
    ] {
        assert_ne!(code(&rig.command(&args)?), 0);
    }
    Ok(())
}
