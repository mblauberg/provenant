//! Small, hermetic fixtures for the configurable Codex thread command protocol.
use crate::cloud::{CloudProvider, CloudResult, CloudSubmission, RemoteJob, RemoteState};
use crate::cloud_cli::{CloudCli, CodexCloud};
use provenant_config::{CodexCloudTransport, ProviderCloudSettings};
use serde_json::{Value, json};

#[derive(Default)]
struct FixtureCli {
    response: String,
    calls: Vec<(String, Vec<String>, Option<String>)>,
}

impl CloudCli for FixtureCli {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        self.calls.push((
            program.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            stdin.map(str::to_owned),
        ));
        Ok(self.response.clone())
    }
}

fn adapter(response: String) -> CodexCloud<FixtureCli> {
    CodexCloud::new(
        FixtureCli {
            response,
            ..Default::default()
        },
        ProviderCloudSettings {
            transport: CodexCloudTransport::Thread,
            command: None,
        },
    )
}

fn job() -> RemoteJob {
    RemoteJob {
        job_id: "job-1".into(),
        nonce: "nonce-1".into(),
        deadline_unix_seconds: u64::MAX,
        provider: "codex".into(),
        target: "codex-cloud".into(),
        environment: "environment-uuid~asenvcfg_1234".into(),
        repository: "example/repository".into(),
        branch: "main".into(),
        remote_id: "thread-1".into(),
        commit: "a".repeat(40),
        requested_model: "requested-model".into(),
        requested_effort: Some("high".into()),
        state: RemoteState::Running,
    }
}

fn report() -> Value {
    json!({
        "job_id": "job-1",
        "nonce": "nonce-1",
        "commit": "a".repeat(40),
        "exit_status": 0,
        "tests_run": ["targeted tests"],
        "tests_failed": [],
        "log_location": "https://example.invalid/log",
        "tracked_files_changed": false,
        "observed_model": "untrusted-self-report"
    })
}

fn status(turn_status: &str, items: Vec<Value>) -> Value {
    json!({
        "thread": {
            "id": "thread-1",
            "model": "observed-model",
            "status": {"active": {}}
        },
        "turns": {"data": {"values": [{
            "id": "turn-1",
            "status": turn_status,
            "itemsPage": {"data": {"values": items}}
        }]}}
    })
}

fn agent(text: String) -> Value {
    json!({"item": {"agentMessage": {"text": text}}})
}

fn finished(adapter: &mut CodexCloud<FixtureCli>, job: &RemoteJob) -> CloudResult {
    let RemoteState::Finished { result } = adapter.observe(job).unwrap() else {
        panic!("completed thread did not produce a result")
    };
    result
}

#[test]
fn cloud_thread_submit_uses_stdin_environment_and_read_only_with_json_identity() {
    let job = job();
    let mut adapter = adapter(format!(
        "Created cloud thread fallback-id\nSubmitting message message-1 ...\n{}\n",
        json!({
            "threadId": "thread-1",
            "environmentConfigId": job.environment,
            "clientUserMessageId": "message-1",
            "submission": {
                "turn": {"id": "turn-1", "status": "TURN_STATUS_IN_PROGRESS"},
                "admissionOutcome": {}
            }
        })
    ));
    assert_eq!(
        adapter
            .submit(CloudSubmission {
                job: &job,
                task: "run targeted tests"
            })
            .unwrap(),
        "thread-1"
    );
    let (program, args, stdin) = &adapter.cli.calls[0];
    assert_eq!(program, "codex-cloud-task");
    assert_eq!(args, &["submit", "--env", &job.environment, "--read-only"]);
    let prompt = stdin.as_deref().unwrap();
    for expected in ["run targeted tests", &job.commit, &job.job_id, &job.nonce] {
        assert!(prompt.contains(expected));
    }
    assert!(prompt.contains(".provenant-cloud-result.json"));
}

#[test]
fn cloud_thread_submit_custom_command_and_created_line_fallback() {
    let mut adapter = CodexCloud::new(
        FixtureCli {
            response: "Created cloud thread thread-2\nSubmitting message message-2 ...\n".into(),
            ..Default::default()
        },
        ProviderCloudSettings {
            transport: CodexCloudTransport::Thread,
            command: Some("custom-cloud".into()),
        },
    );
    assert_eq!(
        adapter
            .submit(CloudSubmission {
                job: &job(),
                task: "verify"
            })
            .unwrap(),
        "thread-2"
    );
    assert_eq!(adapter.cli.calls[0].0, "custom-cloud");
    adapter.cli.response =
        "Submitting message message-2 ...\n{\"clientUserMessageId\":\"message-2\"}".into();
    assert!(
        adapter
            .submit(CloudSubmission {
                job: &job(),
                task: "verify"
            })
            .is_err()
    );
}

#[test]
fn cloud_thread_turn_status_is_structured_and_current() {
    for state in ["TURN_STATUS_IN_PROGRESS", "TURN_STATUS_QUEUED", "queued"] {
        let mut adapter =
            adapter(status(state, vec![agent("completed failed".into())]).to_string());
        assert_eq!(adapter.observe(&job()).unwrap(), RemoteState::Running);
        assert_eq!(adapter.cli.calls[0].0, "codex-cloud-task");
        assert_eq!(adapter.cli.calls[0].1, ["status", "thread-1"]);
        assert_eq!(adapter.cli.calls[0].2, None);
    }
    let mut value = status("TURN_STATUS_COMPLETED", vec![agent(report().to_string())]);
    value["turns"]["data"]["values"]
        .as_array_mut()
        .unwrap()
        .insert(
            0,
            json!({"id":"old-turn","status":"TURN_STATUS_IN_PROGRESS"}),
        );
    let mut adapter = adapter(value.to_string());
    assert_eq!(finished(&mut adapter, &job()).exit_status, 0);
}

#[test]
fn cloud_thread_completed_uses_last_agent_message_and_observed_thread_model() {
    let mut stale = report();
    stale["nonce"] = json!("old-nonce");
    let value = status(
        "TURN_STATUS_COMPLETED",
        vec![
            agent(stale.to_string()),
            json!({"item":{"userMessage":{"text":"running queued failed"}}}),
            agent(format!("```json\n{}\n```", report())),
            json!({"item":{"userMessage":{"text":"later user message"}}}),
        ],
    );
    let mut adapter = adapter(value.to_string());
    let result = finished(&mut adapter, &job());
    assert_eq!(result.observed_model.as_deref(), Some("observed-model"));
    assert!(
        result
            .check("job-1", "nonce-1", &job().commit, "requested-model")
            .unwrap()
            .passed
    );
    assert_eq!(
        adapter.cli.calls.len(),
        1,
        "thread results must not request a cloud diff"
    );
    let mut value = value;
    value["thread"].as_object_mut().unwrap().remove("model");
    adapter.cli.response = value.to_string();
    assert_eq!(finished(&mut adapter, &job()).observed_model, None);
    value["turns"]["data"]["values"][0]["itemsPage"]["data"]["values"][2] =
        json!({"item": {"agentMessage": {"content": [{"text": {"text": report().to_string()}}]}}});
    adapter.cli.response = value.to_string();
    assert_eq!(finished(&mut adapter, &job()).nonce, "nonce-1");
    value["turns"]["data"]["values"][0]["itemsPage"]["data"]["values"]
        .as_array_mut()
        .unwrap()
        .push(agent("missing final result".into()));
    adapter.cli.response = value.to_string();
    assert!(
        adapter.observe(&job()).is_err(),
        "must not reuse an earlier result"
    );
}

#[test]
fn cloud_thread_result_checks_identity_and_preserves_reported_failures() {
    for (key, wrong) in [
        ("job_id", json!("another-job")),
        ("nonce", json!("another-nonce")),
        ("commit", json!("b".repeat(40))),
    ] {
        let mut result = report();
        result[key] = wrong;
        let mut adapter =
            adapter(status("TURN_STATUS_COMPLETED", vec![agent(result.to_string())]).to_string());
        assert!(
            adapter.observe(&job()).is_err(),
            "accepted mismatched {key}"
        );
    }
    for (key, wrong) in [
        ("tracked_files_changed", json!(true)),
        ("exit_status", json!(1)),
        ("tests_failed", json!(["failed test"])),
    ] {
        let mut result = report();
        result[key] = wrong;
        let mut adapter =
            adapter(status("TURN_STATUS_COMPLETED", vec![agent(result.to_string())]).to_string());
        let checked = finished(&mut adapter, &job())
            .check("job-1", "nonce-1", &job().commit, "requested-model")
            .unwrap();
        assert!(!checked.passed, "accepted reported failure in {key}");
    }
}

#[test]
fn cloud_thread_failed_and_cancelled_turns_are_terminal_with_detail() {
    for state in ["TURN_STATUS_FAILED", "TURN_STATUS_CANCELLED"] {
        let mut value = status(state, vec![agent("running earlier".into())]);
        value["turns"]["data"]["values"][0]["error"] = json!({"detail":"fixture failure detail"});
        let mut adapter = adapter(value.to_string());
        let observed = adapter.observe(&job()).unwrap();
        assert!(observed.is_terminal());
        let RemoteState::Failed { detail } = observed else {
            panic!("failed/cancelled turn did not report failure")
        };
        assert!(detail.contains("fixture failure detail"));
        assert!(detail.len() < 8192);
    }
}

#[test]
fn cloud_thread_unknown_shapes_fail_with_bounded_observation() {
    let mut wrong_thread = status("TURN_STATUS_COMPLETED", vec![agent(report().to_string())]);
    wrong_thread["thread"]["id"] = json!("other-thread");
    for value in [
        json!({"unknownShape": "running completed"}),
        wrong_thread,
        status("TURN_STATUS_NEW_UNKNOWN", vec![]),
        status("TURN_STATUS_COMPLETED", vec![]),
        status(
            "TURN_STATUS_COMPLETED",
            vec![json!({"item":{"userMessage":{"text":report().to_string()}}})],
        ),
        json!({"unknownShape": "界".repeat(10000)}),
    ] {
        let mut adapter = adapter(value.to_string());
        let error = adapter.observe(&job()).unwrap_err();
        assert!(!error.is_empty());
        assert!(error.contains("codex thread"));
        assert!(error.contains(&value.to_string().chars().take(12).collect::<String>()));
        assert!(
            error.len() < 8192,
            "unbounded diagnostic: {} bytes",
            error.len()
        );
    }
}

#[test]
fn cloud_thread_environment_list_cannot_recover_a_lost_submission() {
    let mut adapter = adapter(
        json!([{
            "id":"environment-uuid~asenvcfg_1234", "name":"nonce-1", "status":"ready"
        }])
        .to_string(),
    );
    assert_eq!(CloudProvider::recover(&mut adapter, &job()).unwrap(), None);
    assert!(
        adapter.cli.calls.is_empty(),
        "environment list is not a thread lookup"
    );
    assert!(!adapter.cancel(&job()).unwrap());
}
