//! `wait` at the binary boundary: targets resolve before any wait, a settled
//! graph ends the wait with its verdict, and no work means nothing to wait for.
mod support;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::{Result, TestRig, code};

fn envelope(output: &provenant_os::exec::ExecOutcome) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

#[test]
#[allow(clippy::disallowed_methods)] // Asserts the real wait deadline is not consumed.
fn an_unknown_target_fails_before_waiting() -> Result {
    let rig = TestRig::new()?;
    let started = Instant::now();
    let out = rig.command(&["wait", "no-such-graph", "--timeout", "30s", "--json"])?;
    assert_eq!(code(&out), 2, "{out:?}");
    assert_eq!(envelope(&out)?["error"]["code"], "NOT_FOUND");
    assert!(started.elapsed() < Duration::from_secs(20));
    Ok(())
}

#[test]
fn a_settled_graph_ends_the_wait_and_no_work_is_nothing_to_wait_for() -> Result {
    let rig = TestRig::new()?;
    let idle = rig.command(&["wait", "--timeout", "30s", "--json"])?;
    assert_eq!(code(&idle), 0, "{idle:?}");
    assert_eq!(envelope(&idle)?["data"]["targets"], json!([]));
    let document = rig.root.join("graph.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&json!({
            "schema_version":1,"graph_id":"settles-now","nodes":[{
                "id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}
            }]
        }))?,
    )?;
    let run = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("document path")?,
        "--json",
    ])?;
    assert_eq!(code(&run), 0, "{run:?}");
    let out = rig.command(&["wait", "settles-now", "--timeout", "30s", "--json"])?;
    let value = envelope(&out)?;
    assert_eq!(code(&out), 0, "{value}");
    assert_eq!(value["data"]["timed_out"], false, "{value}");
    assert_eq!(value["data"]["targets"][0]["target"], "settles-now");
    assert_eq!(value["data"]["targets"][0]["settle"], "succeeded");
    Ok(())
}
