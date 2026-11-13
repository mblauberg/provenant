use super::*;

#[test]
fn lanes_show_only_live_capacity_cooldowns() {
    let account = provenant_quota::AccountScope {
        adapter: "codex".into(),
        account_key: "key".into(),
        label: "work".into(),
    };
    let mut record = provenant_quota::AccountRecord::empty(account.clone());
    record.capacity_cooldowns = vec![provenant_quota::CapacityCooldown {
        extra: Default::default(),
        account,
        model: "model".into(),
        observed_unix_seconds: 100,
        expires_unix_seconds: 700,
    }];
    let mut human = String::new();
    let shown = render_cooldowns(&[record.clone()], 400, &mut human);
    assert_eq!(shown[0]["remaining_seconds"], 300);
    assert!(human.contains("codex/model [work]: 300s remaining"));
    assert!(render_cooldowns(&[record.clone()], 700, &mut String::new()).is_empty());
    assert!(render_cooldowns(&[record], 99, &mut String::new()).is_empty());
}
#[test]
fn delivery_state_text_retains_known_details_without_extensions() {
    use provenant_records::run::{WriterDeliveryStage, WriterDeliveryState};
    for wire in [
        json!({"state":"not_attempted"}),
        json!({"state":"no_new_commits"}),
        json!({"state":"failed","stage":"push"}),
        json!({"state":"delivered","pull_request":null}),
        json!({"state":"delivered","pull_request":"https://example.invalid/pr/1"}),
        json!({"state":"delivered","pull_request":"pr\u{001b}\n1"}),
    ] {
        let mut wire = wire;
        wire["future_note"] = json!("future-secret");
        let state: WriterDeliveryState = serde_json::from_value(wire.clone()).unwrap();
        let text = delivery_state_text(&state);
        match state {
            WriterDeliveryState::NotAttempted { .. } | WriterDeliveryState::NoNewCommits { .. } => {
            }
            WriterDeliveryState::Failed { stage, .. } => {
                assert_eq!(stage, WriterDeliveryStage::Push);
            }
            WriterDeliveryState::Delivered { pull_request, .. } => {
                assert_eq!(pull_request.as_deref(), wire["pull_request"].as_str());
                if pull_request.is_some() {
                    assert!(text.contains("pr1") || text.contains("https://example.invalid/pr/1"));
                }
            }
        }
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(!text.contains("future-secret"));
    }
}
#[test]
fn render_lanes_removes_peer_terminal_controls() -> Result<(), Box<dyn std::error::Error>> {
    let reason = "\x1b[31mreason\x1b[0m";
    let account = "\x1b]0;account\x07";
    let detail = "TOKEN_SENTINEL /private/source\n\r\t\u{0008}\u{202e}detail";
    let remote = "or\x1bigin";
    let branch = "main\u{009b}line";
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "peer-lane-render",
        "attempt_number": 1,
        "title": "Review\nchanges",
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
        "applied_route": {"model":"fixture/\x1b[32mmodel","account_label":account},
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "write_grants": [{"source":"write_root","path":{"relative_to_root":{"encoding":"utf8","path":"work/shared"}},"scope":"subtree"}],
        "read_only_writable": ["locks"],
        "git_grants_digest": provenant_core::Digest::from_bytes([6; 32]),
        "queued_at": now,
        "priority": 10,
        "state": {"state":"terminal","outcome":"failed","exit_code":1,"finished_at":now,"reason":reason},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        },
        "skipped_routes": [{"route":"fixture/\x1b[33mfallback","kind":"unavailable","detail":detail}],
        "delivery": {"schema_version":1,"remote":remote,"branch":branch,"state":{"state":"not_attempted","future_note":"future-secret"}}
    }))?;
    let (data, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    let mut stale = lane.clone();
    stale.skipped_routes[0].cause =
        Some(provenant_records::RouteExclusionReason::StaleAvailability);
    let (stale_data, stale_human) = render_lanes_without_cooldowns(
        std::slice::from_ref(&stale),
        &[],
        None,
        Path::new("/state"),
    );
    let stale_skip = &stale_data["lanes"][0]["skipped_routes"][0];
    assert!(stale_skip["detail"].as_str().unwrap().contains("stale"));
    assert!(
        stale_skip["next"]
            .as_str()
            .unwrap()
            .contains("provenant quota tick")
    );
    assert!(stale_human.contains("stale"));
    assert!(!stale_skip.to_string().contains("TOKEN_SENTINEL"));
    assert!(!stale_human.contains("/private/source"));
    assert_eq!(
        human,
        format!(
            "{}/{}/{} Reviewchanges failed fixture/[32mmodel account=]0;account (the lane failed; inspect its result and local evidence)\n  skipped fixture/[33mfallback Unavailable: the selected route is unavailable; check route readiness with `provenant quota tick`\n  delivery origin:mainline NotAttempted head=unknown\n",
            RunId::from_bytes([2; 16]),
            TaskId::from_bytes([4; 16]),
            AttemptId::from_bytes([1; 16])
        )
    );
    assert!(!human.chars().any(|c| c.is_control() && c != '\n'));
    assert!(!human.contains('\u{202e}'));
    assert_eq!(data["lanes"][0]["title"], "Review\nchanges");
    assert_eq!(data["lanes"][0]["priority"], 10);
    assert!(data["lanes"][0].get("requested_priority").is_none());
    lane.requested_priority = Some(10);
    lane.priority = Some(15);
    let (different, _) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert_eq!(different["lanes"][0]["priority"], 15);
    assert_eq!(different["lanes"][0]["requested_priority"], 10);
    assert!(
        data["lanes"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("lane failed")
    );
    assert!(
        !data["lanes"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("reason")
    );
    assert_eq!(data["lanes"][0]["write_grants"][0]["source"], "write_root");
    assert_eq!(data["lanes"][0]["write_grants"][0]["scope"], "subtree");
    assert_eq!(
        data["lanes"][0]["write_grants"][0]["path"]["relative"]["path"],
        "work/shared"
    );
    assert_eq!(
        data["lanes"][0]["read_only_writable"],
        json!([{"kind":"relative", "relative":{"encoding":"utf8", "path":"locks"}}])
    );
    assert_eq!(
        data["lanes"][0]["git_grants_digest"],
        json!(provenant_core::Digest::from_bytes([6; 32]))
    );
    assert_eq!(data["lanes"][0]["account"], json!(account));
    let skipped = &data["lanes"][0]["skipped_routes"][0];
    assert_eq!(skipped["code"], "REFUSED");
    assert!(skipped["detail"].as_str().unwrap().contains("unavailable"));
    assert!(
        skipped["next"]
            .as_str()
            .unwrap()
            .contains("provenant quota tick")
    );
    let skipped_wire = skipped.to_string();
    for text in [human.as_str(), skipped_wire.as_str()] {
        assert!(!text.contains("TOKEN_SENTINEL"));
        assert!(!text.contains("/private/source"));
    }
    assert_eq!(data["lanes"][0]["delivery"]["remote"], json!(remote));
    assert_eq!(data["lanes"][0]["delivery"]["branch"], json!(branch));
    assert_eq!(
        data["lanes"][0]["delivery"]["state"]["future_note"],
        "future-secret"
    );
    assert!(!human.contains("future-secret"));
    lane.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::ModelUnavailable,
        exit_code: Some(1),
        finished_at: now,
        reason: Some("provider capacity: Selected model is at capacity".into()),
    };
    lane.session = Some("session".into());
    let (data, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert_eq!(data["lanes"][0]["state"], "model_unavailable");
    assert!(human.contains("the model is unavailable"));
    assert!(!human.contains("Selected model is at capacity"));
    assert!(human.contains(&format!(
        "continue: provenant retry {} --prompt-file PROMPT",
        lane.attempt_id
    )));
    lane.session = None;
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(!human.contains("provenant retry"));
    assert!(human.contains("redispatch with another model"));
    lane.session = Some("session".into());
    if let run::AttemptState::Terminal { reason, .. } = &mut lane.state {
        *reason = Some("provider model unavailable: no route".into());
    }
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(!human.contains("provenant retry"));
    assert!(human.contains("redispatch with a corrected model"));
    lane.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::ReviewRequired,
        exit_code: Some(0),
        finished_at: now,
        reason: None,
    };
    lane.head = Some(provenant_core::GitOid::parse(&"a".repeat(40))?);
    lane.protected_paths = vec![provenant_records::PathWire::from_relative(
        &provenant_core::RelativePath::parse(".github/unsafe\npath.yml")?,
    )?];
    lane.delivery = None;
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert_eq!(data["lanes"][0]["state"], "review_required");
    assert_eq!(data["lanes"][0]["head"], "a".repeat(40));
    assert_eq!(
        data["lanes"][0]["protected_paths"][0]["relative"]["path"],
        ".github/unsafe\npath.yml"
    );
    assert!(human.contains(&format!(
        "review head={} paths=.github/unsafepath.yml",
        "a".repeat(40)
    )));
    Ok(())
}
#[test]
fn render_lanes_shows_review_line_when_review_required_without_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "review-no-paths",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state":"terminal","outcome":"failed","exit_code":1,"finished_at":now,"reason":null},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        }
    }))?;
    lane.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::ReviewRequired,
        exit_code: Some(0),
        finished_at: now,
        reason: None,
    };
    lane.protected_paths = vec![];
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(human.contains("  review head=unknown paths=\n"));
    assert!(human.contains(&format!(
        "  next: provenant lane deliver --accept-review {}\n",
        lane.attempt_id
    )));
    Ok(())
}
#[test]
fn lanes_render_retry_lineage() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut failed = lane_fixture(
        1,
        4,
        run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::Failed,
            exit_code: Some(1),
            finished_at: now,
            reason: None,
        },
    )?;
    let mut retry = lane_fixture(
        2,
        4,
        run::AttemptState::Queued {
            extra: Default::default(),
        },
    )?;
    retry.retry_of = Some(failed.attempt_id);
    failed.superseded_by = Some(retry.attempt_id);
    let (data, human) = render_lanes(
        &[failed.clone(), retry.clone()],
        &[],
        None,
        Path::new("/state"),
        None,
    );
    assert_eq!(data["lanes"][0]["superseded_by"], json!(retry.attempt_id));
    assert_eq!(data["lanes"][1]["retry_of"], json!(failed.attempt_id));
    assert_eq!(
        human,
        format!(
            "{}/{}/{} failed → retrying ({}) unresolved ({})\n{}/{}/{} queued (retry of {}) unresolved\n",
            failed.run_id,
            failed.task_id,
            failed.attempt_id,
            retry.attempt_id,
            run::Outcome::Failed.diagnostic_reason(),
            retry.run_id,
            retry.task_id,
            retry.attempt_id,
            failed.attempt_id,
        )
    );
    Ok(())
}
#[test]
fn lanes_give_a_settled_lane_its_settlement_reason() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let failed = lane_fixture(
        1,
        4,
        run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::Failed,
            exit_code: Some(1),
            finished_at: now,
            reason: Some("provider failed".into()),
        },
    )?;
    let queued = lane_fixture(
        2,
        4,
        run::AttemptState::Queued {
            extra: Default::default(),
        },
    )?;
    let (data, _) =
        render_lanes_without_cooldowns(&[failed, queued], &[], None, Path::new("/state"));
    assert_eq!(
        data["lanes"][0]["settlement"]["reason"],
        run::Outcome::Failed.diagnostic_reason()
    );
    assert!(
        !data["lanes"][0]["settlement"]["reason"]
            .as_str()
            .unwrap()
            .contains("provider failed")
    );
    assert_eq!(data["lanes"][1]["settlement"], Value::Null);
    assert_eq!(data["lanes"][1]["reason"], Value::Null);
    Ok(())
}
#[test]
fn lanes_distinguish_exhausted_continuations_from_a_retry() -> Result<(), Box<dyn std::error::Error>>
{
    let now = WallTime::from_micros(100)?;
    let stopped = |reason: &str| run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::Failed,
        exit_code: Some(0),
        finished_at: now,
        reason: Some(reason.into()),
    };
    let mut pending = lane_fixture(1, 4, stopped("model reported unfinished work"))?;
    let exhausted = lane_fixture(
        2,
        5,
        stopped("model reported unfinished work; continuations exhausted"),
    )?;
    let next = AttemptId::from_bytes([3; 16]);
    pending.superseded_by = Some(next);
    let (data, human) = render_lanes(&[pending, exhausted], &[], None, Path::new("/state"), None);
    assert_eq!(data["lanes"][0]["superseded_by"], json!(next));
    assert_eq!(data["lanes"][1]["state"], "failed");
    assert!(data["lanes"][1]["superseded_by"].is_null());
    assert_eq!(
        data["lanes"][1]["reason"],
        run::Outcome::Failed.diagnostic_reason()
    );
    assert!(human.contains("retrying"));
    assert!(!human.contains("model reported unfinished work"));
    assert!(!human.contains("continuations exhausted"));
    Ok(())
}
#[test]
fn newest_settled_of_names_the_settled_attempt_of_one_task()
-> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let settled = || run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::Failed,
        exit_code: Some(1),
        finished_at: now,
        reason: None,
    };
    let running = || run::AttemptState::Running {
        extra: Default::default(),
    };
    // Newest first: a retry still running, then the attempt it replaced.
    let (retry, first) = (
        lane_fixture(2, 4, running())?,
        lane_fixture(1, 4, settled())?,
    );
    let (id, count, rule) =
        newest_settled_of(&[retry, first.clone()]).map_err(|fail| fail.message)?;
    assert_eq!((id, count, rule), (first.attempt_id, 2, "newest settled"));
    // Nothing has settled, so the newest attempt is the one named.
    let (newest, older) = (
        lane_fixture(3, 4, running())?,
        lane_fixture(2, 4, running())?,
    );
    let (id, _, rule) = newest_settled_of(&[newest.clone(), older]).map_err(|fail| fail.message)?;
    assert_eq!((id, rule), (newest.attempt_id, "newest"));
    // Attempts of two tasks are ambiguous, so RUN/TASK is asked for.
    let other = lane_fixture(4, 5, running())?;
    let error = newest_settled_of(&[newest, other])
        .err()
        .ok_or("two tasks were accepted")?;
    assert!(error.message.contains("2 tasks"), "{}", error.message);
    Ok(())
}
#[test]
fn render_lanes_points_at_the_provider_transcript() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "transcript-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": {"provider":"claude","model":"claude/claude-sonnet-5"},
        "session": "sess-1848",
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state": "terminal", "outcome": "rejected", "finished_at": now},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        }
    }))?;
    let root = tempfile::tempdir()?;
    let state = root.path().canonicalize()?;
    let attempt = artifact(&state, &lane, "");
    let transcript = attempt.join("provider-state/candidate-0/claude-config/projects/k");
    std::fs::create_dir_all(&transcript)?;
    std::fs::write(transcript.join("sess-1848.jsonl"), "{}\n")?;
    let (data, _) = render_lanes_without_cooldowns(&[lane.clone()], &[], None, &state);
    let found = &data["lanes"][0]["transcript"];
    assert_eq!(found["adapter"], "claude");
    assert_eq!(found["session"], "sess-1848");
    assert_eq!(
        found["path"],
        json!({"kind":"path", "root":"/", "relative":{"encoding":"utf8", "path":transcript.join("sess-1848.jsonl").strip_prefix("/")?.to_str().unwrap()}})
    );
    // Read-only Claude records its selected account's exact state-write path.
    // The lookup never follows links, so build the grant from the resolved temp root.
    let account = root
        .path()
        .canonicalize()?
        .join("accounts/claude/projects/project-key");
    std::fs::create_dir_all(&account)?;
    let path = account.join("sess-1848.jsonl");
    std::fs::write(&path, "{}\n")?;
    lane.write_grants = vec![run::WriteGrantRecord {
        extra: Default::default(),
        source: run::WriteSource::StateWrite,
        scope: run::PathScope::Exact,
        path: provenant_records::AbsolutePath::new(&path)?,
    }];
    let other = tempfile::tempdir()?;
    std::fs::create_dir_all(artifact(other.path(), &lane, ""))?;
    let (data, _) = render_lanes_without_cooldowns(&[lane], &[], None, other.path());
    assert_eq!(
        data["lanes"][0]["transcript"]["path"],
        json!({"kind":"path", "root":"/", "relative":{"encoding":"utf8", "path":path.strip_prefix("/")?.to_str().unwrap()}})
    );
    Ok(())
}
#[test]
fn render_lanes_omits_account_when_unresolved() -> Result<(), Box<dyn std::error::Error>> {
    let reason = "argv prompt exceeds 64 KiB";
    let now = WallTime::from_micros(100)?;
    let lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "prompt-refusal-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {
            "state": "terminal",
            "outcome": "rejected",
            "finished_at": now,
            "reason": reason
        },
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 65537,
            "complete": true,
            "redacted": false
        }
    }))?;
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert_eq!(
        human,
        format!(
            "{}/{}/{} rejected unresolved ({reason})\n",
            RunId::from_bytes([2; 16]),
            TaskId::from_bytes([4; 16]),
            AttemptId::from_bytes([1; 16]),
            reason = run::Outcome::Rejected.diagnostic_reason(),
        )
    );
    assert!(!human.contains("account="));
    assert!(data["lanes"][0].get("title").is_none());
    assert!(data["lanes"][0]["account"].is_null());
    assert_eq!(data["lanes"][0]["state"], "rejected");
    assert_eq!(
        data["lanes"][0]["reason"],
        json!(run::Outcome::Rejected.diagnostic_reason())
    );
    assert!(!human.contains(reason));
    assert!(data["lanes"][0]["applied_route"].is_null());
    Ok(())
}
#[test]
fn local_wait_observes_settlement_and_renders_pending_details()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let store = trace_store(&temp, &call)?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session { store, context };
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    session.store.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    let targets = [Target {
        selection: None,
        run: Some(prepared.run_id),
        task: None,
        attempt: Some(id),
    }];
    let picks = Picks {
        targets: &targets,
        owner: session.context.actor_id,
        label: None,
        since: None,
    };
    let mut fetch = candidates(&session, &picks, None);
    let first = fetch(Duration::from_secs(1)).map_err(|fail| fail.message)?;
    assert_eq!(first.len(), 1);
    let (data, _) = render_settled(&session, &first, None).map_err(|fail| fail.message)?;
    assert_eq!(
        data["lanes"][0]["waiting_for"],
        "launch: not yet handed to a guardian"
    );
    let mut reads = 0;
    let finished_at = clock.wall_now()?;
    let attempt_path = session
        .context
        .state_root
        .as_path()
        .join("attempts")
        .join(id.to_string());
    let directory = provenant_os::fs::Directory::open_anchor(&attempt_path)?;
    let mut custody = Some(directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?);
    let waited = watch::wait(
        |remaining| {
            reads += 1;
            if reads == 2 {
                session.store.withdraw(
                    id,
                    run::AttemptState::Queued {
                        extra: Default::default(),
                    },
                    run::Outcome::Cancelled,
                    "fixture settlement",
                    finished_at,
                    &call,
                )?;
            }
            fetch(remaining)
        },
        picks.scope().map_err(|fail| fail.message)?,
        Condition::All,
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .map_err(|fail| fail.message)?;
    assert!(waited.met);
    assert_eq!(waited.lanes.len(), 1);
    assert!(matches!(
        waited.lanes[0].state,
        run::AttemptState::Terminal {
            outcome: run::Outcome::Cancelled,
            ..
        }
    ));
    // The wait returns the sealed decision and reports retained custody
    // separately, instead of holding the terminal state back.
    assert_eq!(reads, 2, "a terminal decision ends the wait");
    assert_eq!(waited.lanes[0].custody_pending, Some(true));
    assert!(!session.store.custody_released(&waited.lanes[0], &call)?);
    drop(custody.take());
    assert!(session.store.custody_released(&waited.lanes[0], &call)?);
    Ok(())
}
#[test]
fn render_lanes_says_what_an_unstarted_lane_waits_for() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "wait-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 1,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state": "starting"},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 1,
            "complete": true,
            "redacted": false
        }
    }))?;
    let wait = run::LaneWait {
        extra: Default::default(),
        attempt_id: lane.attempt_id,
        lane_class: run::LaneClass::ReadOnly,
        waiting_for: "read-only startup spacing: 1.2 s left of 2.0 s".into(),
    };
    let (preparing, _) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert_eq!(preparing["lanes"][0]["waiting_for"], "launch preparation");
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[wait], None, Path::new("/state"));
    assert!(
        human.contains("starting unresolved (read-only startup spacing: 1.2 s left of 2.0 s)"),
        "{human}"
    );
    let row = &data["lanes"][0];
    assert_eq!(row["lane_class"], "read_only");
    assert_eq!(
        row["waiting_for"],
        "read-only startup spacing: 1.2 s left of 2.0 s"
    );
    assert_eq!(row["reason"], row["waiting_for"]);
    Ok(())
}
#[test]
fn render_lanes_surfaces_provider_silence_while_running() -> Result<(), Box<dyn std::error::Error>>
{
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "silent-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 1,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state":"running"},
        "usage": {},
        "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"},"digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
    }))?;
    lane.provider_silent_for_ms.0 = Some(1_800_000);
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert!(human.contains("provider silent for 1800s; provider still running"));
    assert_eq!(
        data["lanes"][0]["reason"],
        "provider silent for 1800s; provider still running"
    );
    assert_eq!(
        data["lanes"][0]["waiting_for"],
        "provider silent for 1800s; provider still running"
    );
    assert_eq!(data["lanes"][0]["state"], "running");
    Ok(())
}
#[test]
fn render_lanes_reports_uncommitted_without_echoing_dirty_path_prose()
-> Result<(), Box<dyn std::error::Error>> {
    let reason = "writer left uncommitted edits and no new commit: src/a.rs, b.txt";
    let now = WallTime::from_micros(100)?;
    let lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "uncommitted-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {
            "state": "terminal",
            "outcome": "uncommitted",
            "finished_at": now,
            "reason": reason
        },
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 1,
            "complete": true,
            "redacted": false
        }
    }))?;
    assert_eq!(settled(&lane.state), (true, false));
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert!(
        human.contains(&format!(
            " uncommitted unresolved ({})\n",
            run::Outcome::Uncommitted.diagnostic_reason()
        )),
        "{human}"
    );
    assert_eq!(data["lanes"][0]["state"], "uncommitted");
    assert_eq!(
        data["lanes"][0]["reason"],
        json!(run::Outcome::Uncommitted.diagnostic_reason())
    );
    assert!(!human.contains("src/a.rs"));
    assert!(!human.contains("b.txt"));
    assert!(!serde_json::to_string(&data)?.contains(reason));
    Ok(())
}
#[test]
fn lane_access_refuses_credential_and_enclosing_roots() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let credential = temp.path().join(".ssh").join("nested");
    std::fs::create_dir_all(&credential)?;
    let mut roots = vec![std::fs::canonicalize(&credential)?];
    // The home itself and its `.config` are enclosing roots; use the host's
    // real directories so the check sees a conventional home.
    let host_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_dir());
    if let Some(home) = &host_home {
        if home.join(".config").is_dir() {
            roots.push(home.join(".config"));
        }
        roots.push(home.clone());
    }
    assert!(roots.len() >= 2);
    let home = host_home
        .as_deref()
        .unwrap_or(Path::new("/fixture/host-home"));
    for root in roots {
        for (flag, read) in [("--read-root", true), ("--write-root", false)] {
            let access = crate::Access {
                attach: Vec::new(),
                mode: Some(Mode::ReadOnly),
                worktree: None,
                read_root: if read { vec![root.clone()] } else { vec![] },
                write_root: if read { vec![] } else { vec![root.clone()] },
                network: None,
                capability: vec![],
                no_capability: Vec::new(),
            };
            let fail = lane_access(&access, &temp.path().join("state"), home).unwrap_err();
            assert!(fail.message.contains(flag), "{}", fail.message);
            assert!(
                fail.message.contains(&root.display().to_string()),
                "{}",
                fail.message
            );
        }
    }
    Ok(())
}
