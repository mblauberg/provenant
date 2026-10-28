//! The local overview is a compact, app-independent read of attention.
mod support;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn call(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let mut words = args.to_vec();
    words.push("--json");
    let output = rig.command(&words)?;
    assert_eq!(
        code(&output),
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

#[test]
fn overview_needs_you() -> Result {
    let empty = TestRig::new()?.without_state()?;
    let overview = call(&empty, &["console", "overview"])?;
    assert_eq!(overview["data"]["needs_you"], json!([]));
    assert_eq!(overview["data"]["counts"]["needs_you"], Value::Null);
    assert_eq!(overview["data"]["counts"]["running"], Value::Null);
    assert_eq!(overview["data"]["counts"]["attention"], Value::Null);
    assert_eq!(overview["data"]["coverage"]["inbox"], "unavailable");
    assert_eq!(overview["data"]["coverage"]["runs"], "unavailable");
    assert!(
        !empty.state.exists(),
        "overview created an absent state root"
    );

    let rig = TestRig::new()?;
    let draft = rig.root.join("request.json");
    std::fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "kind":"decision", "title":"Choose a repair", "body":"PRIVATE BODY MARKER",
            "blocking":true, "options":[], "recommended_option_id":null,
            "proceeding_with":null
        }))?,
    )?;
    let created = call(
        &rig,
        &[
            "ask",
            "--file",
            draft.to_str().ok_or("draft path")?,
            "--session",
            "chair-a",
        ],
    )?;
    let id = created["data"]["request_id"].as_str().ok_or("request ID")?;
    let overview = call(&rig, &["console", "overview"])?;
    let data = &overview["data"];
    let rows = data["needs_you"].as_array().ok_or("needs-you rows")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["title"], "Choose a repair");
    assert_eq!(rows[0]["project_label"], "project");
    assert!(rows[0]["project_id"].is_string());
    assert!(rows[0]["task_label"].is_null());
    assert_eq!(data["counts"]["needs_you"], 1);
    assert!(!overview.to_string().contains("PRIVATE BODY MARKER"));

    // Submit a queued lane through its owner, without starting a provider.
    use provenant_core::{ActorId, CancelToken, Clock, OperationId, ProjectId, TaskId, WaitBudget};
    use provenant_run::{
        DispatchRequest, DispatchTask, ExecutionContext, HarnessProfile, LaneAccess, Request,
        RequestedRoute, RunStore, Service,
    };
    let project = ProjectId::parse(rows[0]["project_id"].as_str().ok_or("project ID")?)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 5)?,
        cancel: &cancel,
    };
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let context = ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: project,
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let store = RunStore::open(&context, &request)?;
    Service::new(&store).dispatch(
        DispatchRequest {
            chair: None,
            operation_id: OperationId::parse("overview-lane")?,
            tasks: vec![DispatchTask {
                build_heavy: false,
                title: Some("Review changes".into()),
                task_id: TaskId::from_bytes([3; 16]),
                prompt: "PRIVATE PROMPT MARKER".into(),
                base: None,
                head: None,
                route: RequestedRoute::Named {
                    name: "senior".into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
                profile: HarnessProfile::default(),
                timeout_seconds: 60,
                delivery_id: None,
                deliver: None,
                worktree: None,
                access: LaneAccess::default(),
            }],
            concurrency: 1,
            lane_depth: 1,
            parent_attempt: None,
        },
        &rig.project,
        &request,
    )?;
    drop(store);
    let with_lane = call(&rig, &["console", "overview"])?;
    let lane = &with_lane["data"]["lanes"][0];
    assert_eq!(lane["task_label"], "Review changes");
    assert_eq!(lane["project_label"], "project");
    assert_eq!(with_lane["data"]["counts"]["running"], 1);
    assert!(!with_lane.to_string().contains("PRIVATE PROMPT MARKER"));

    // The inbox and run scans share one budget, including unprojected bodies.
    let fabric_db = rusqlite::Connection::open(rig.state.join("fabric/fabric.sqlite3"))?;
    let run_db = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    let run_len: u32 =
        run_db.query_row("SELECT length(record) FROM attempts", [], |row| row.get(0))?;
    let original: Vec<u8> =
        fabric_db.query_row("SELECT record FROM human_requests", [], |row| row.get(0))?;
    let target = 1_048_576 - run_len as usize + 1;
    let mut added_ids = Vec::new();
    let project_id = project.to_string();
    for index in 0..5 {
        let mut inflated: Value = serde_json::from_slice(&original)?;
        let row_target = target / 5 + usize::from(index < target % 5);
        let extra_id =
            provenant_core::HumanRequestId::from_bytes([index as u8 + 20; 16]).to_string();
        if index != 0 {
            inflated["id"] = json!(extra_id);
        }
        inflated["request"]["body"] = json!("");
        let overhead = serde_json::to_vec(&inflated)?.len();
        inflated["request"]["body"] = json!("x".repeat(row_target - overhead));
        let inflated = serde_json::to_vec(&inflated)?;
        assert_eq!(inflated.len(), row_target);
        if index == 0 {
            fabric_db.execute("UPDATE human_requests SET record=?1", [&inflated])?;
        } else {
            fabric_db.execute(
                "INSERT INTO human_requests VALUES(?1,?2,1,'chair-a','open',?3)",
                rusqlite::params![project_id, extra_id, inflated],
            )?;
            added_ids.push(extra_id);
        }
    }
    let bounded = call(&rig, &["console", "overview"])?;
    assert_eq!(bounded["data"]["coverage"]["inbox"], "complete");
    assert_eq!(bounded["data"]["coverage"]["runs"], "partial");
    assert_eq!(bounded["data"]["lanes"], json!([]));
    for added_id in added_ids {
        fabric_db.execute("DELETE FROM human_requests WHERE id=?1", [&added_id])?;
    }
    fabric_db.execute("UPDATE human_requests SET record=?1", [&original])?;

    let human = rig.command(&["console", "overview"])?;
    assert_eq!(code(&human), 0);
    let human = String::from_utf8(human.stdout.bytes)?;
    assert!(human.contains("Choose a repair"));
    assert!(!human.contains("PRIVATE BODY MARKER"));
    call(
        &rig,
        &[
            "inbox",
            "close",
            id,
            "--state",
            "withdrawn",
            "--reason",
            "No longer needed",
            "--expect-revision",
            "1",
        ],
    )?;
    let after = call(&rig, &["console", "overview"])?;
    assert_eq!(after["data"]["needs_you"], json!([]));
    assert_eq!(after["data"]["counts"]["needs_you"], 0);
    Ok(())
}
