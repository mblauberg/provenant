mod support;
#[path = "support/terminal_run.rs"]
mod terminal_run;
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
    let output = rig.execute(&git, &["commit", "--allow-empty", "-qm", "Initial"], None)?;
    assert_eq!(code(&output), 0, "{output:?}");
    Ok(rig)
}

#[test]
fn offline_pipeline_preserves_unattributed_and_unmeasured_evidence() -> Result {
    let rig = rig()?;
    terminal_run::seed(
        &rig,
        "pipeline-fixture",
        &[
            json!({"graph_id":"retained-graph","node_id":"review","pattern":"council","role":"correctness"}),
        ],
    )?;
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
    terminal_run::seed(
        &rig,
        "unreadable-graph-fixture",
        &[json!({"graph_id":"invalid/graph","node_id":"review"})],
    )?;
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
    terminal_run::seed(
        &rig,
        "unattributed-fixture",
        &[json!({"node_id":"review"}), json!({"node_id":"review"})],
    )?;
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

#[test]
fn stats_reports_route_access_effort_outcomes_and_nonrandom_comparison() -> Result {
    let rig = rig()?;
    terminal_run::seed(&rig, "route-outcomes-fixture", &[Value::Null])?;
    let output = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let rows = report["data"]["route_outcomes"]
        .as_array()
        .ok_or("outcome rows")?;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["access"], "read-only");
    assert_eq!(row["effort"], "unknown");
    assert_eq!(row["samples"], 1);
    assert_eq!(row["successes"], 0);
    assert_eq!(row["success_rate"], 0.0);
    assert!(row.get("cost_per_success_microdollars").is_none());
    assert!(row["time_per_success_ms"].is_null());
    assert_eq!(row["comparison"], "observational_not_hash_assigned");
    let output = rig.command(&["telemetry", "stats"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let human = String::from_utf8(output.stdout.bytes)?;
    assert!(human.contains("access=read-only effort=unknown n=1"));
    assert!(!human.contains("cost"));
    assert!(human.contains("time/success_ms=unknown"));
    Ok(())
}

#[test]
fn backfill_copies_each_attempt_once_and_outlives_the_run_store() -> Result {
    let rig = rig()?;
    terminal_run::seed(&rig, "backfill-fixture", &[Value::Null, Value::Null])?;
    let output = rig.command(&["telemetry", "stats", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let totals = &report["data"]["totals"];
    let backfill = |rig: &TestRig| -> Result<Value> {
        let output = rig.command(&["telemetry", "backfill", "--json"])?;
        assert_eq!(code(&output), 0, "{output:?}");
        Ok(serde_json::from_slice::<Value>(&output.stdout.bytes)?["data"].clone())
    };
    let first = backfill(&rig)?;
    assert_eq!(first["added"], 2, "{first}");
    assert_eq!(first["added"], totals["attempts"]);
    let ledger = provenant_telemetry::ledger::read(&rig.state)?;
    assert!(ledger.unrecognised.is_empty(), "{:?}", ledger.unrecognised);
    assert!(totals.get("cost").is_none(), "stats no longer reports cost");
    let cost = rig.command(&["telemetry", "cost", "--json"])?;
    assert_eq!(code(&cost), 0, "{cost:?}");
    let cost: Value = serde_json::from_slice(&cost.stdout.bytes)?;
    assert_eq!(cost["data"]["totals"]["attempts"], 2, "{cost}");
    assert!(
        ledger
            .rows
            .iter()
            .all(|row| row.labels.run.is_some() && row.labels.task.is_some())
    );
    let second = backfill(&rig)?;
    assert_eq!(
        (&second["added"], &second["present"]),
        (&json!(0), &json!(2))
    );
    // Deleting the run store leaves the ledger whole.
    for name in ["run.sqlite3", "run.sqlite3-wal", "run.sqlite3-shm"] {
        match std::fs::remove_file(rig.state.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    let after = backfill(&rig)?;
    assert_eq!(
        (&after["attempts"], &after["ledger_rows"]),
        (&json!(0), &json!(2))
    );
    assert_eq!(
        provenant_telemetry::ledger::read(&rig.state)?.rows,
        ledger.rows
    );
    Ok(())
}
