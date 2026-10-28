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
