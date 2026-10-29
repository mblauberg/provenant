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
    Ok(rig)
}

fn assert_group(rig: &TestRig, by: &str, value: &str, graph: Option<&str>) -> Result {
    let output = rig.command(&["telemetry", "stats", "--by", by, "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"]["totals"]["attempts"], 1, "{report}");
    let groups = &report["data"]["context_groups"];
    assert_eq!(groups.as_array().ok_or("groups")?.len(), 1);
    assert_eq!(groups[0]["value"], value, "{report}");
    assert_eq!(groups[0]["totals"]["attempts"], 1);
    if let Some(graph) = graph {
        assert_eq!(report["data"]["graph_quality"][0]["graph_id"], graph);
        assert_eq!(report["data"]["graph_quality"][0]["checkpoint"], "read");
    }
    Ok(())
}

#[test]
fn manifest_submission_retains_optional_work_context() -> Result {
    let rig = rig()?;
    let manifest = rig.project.join("tasks.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "tasks": [{
                "prompt": "Review the bounded slice",
                "context": {"graph_id":"manifest-graph", "node_id":"review", "pattern":"council"},
                "route": {"kind":"model", "provider":"fixture", "model":"fixture/model"}
            }]
        }))?,
    )?;
    // An unavailable fixture route settles locally and supplies an attempt fact.
    let output = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest path")?,
        "--operation-id",
        "manifest-context",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(value["data"]["run_id"].is_string(), "{value}");
    assert_group(&rig, "node", "review", None)?;
    assert_group(&rig, "pattern", "council", None)?;
    Ok(())
}

#[test]
fn submitted_graph_node_is_grouped_by_node_and_pattern() -> Result {
    let rig = rig()?;
    let document = rig.project.join("graph.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&json!({
            "schema_version": 1, "graph_id":"attributed-graph",
            "context": {"pattern":"graph-default", "preset":"example"},
            "nodes": [{
                "id":"inspect", "prompt":"Review the bounded slice", "access":"read_only",
                "context": {"pattern":"council"},
                "route":{"adapter":"fixture", "model":"fixture/model"}
            }]
        }))?,
    )?;
    let output = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        document.to_str().ok_or("graph path")?,
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        value["data"]["checkpoint"]["graph_id"], "attributed-graph",
        "{value}"
    );
    assert_group(&rig, "node", "inspect", Some("attributed-graph"))?;
    assert_group(&rig, "pattern", "council", Some("attributed-graph"))?;
    assert_group(&rig, "preset", "example", Some("attributed-graph"))?;
    Ok(())
}
