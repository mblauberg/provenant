//! `land` against a fake `gh` and real Git remotes, worktrees and branches.
use super::worktree_tests::support::{Result, TestRig, code};
use super::worktree_tests::{command, git, json, repository};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

const NUMBER: &str = "1668";

/// Capture through dispatch, then retain through real land and gc.
pub(crate) fn pr_contribution_roundtrip() -> Result {
    use provenant_core::{CancelToken, Clock, OperationId, TaskId};
    use provenant_run::{DispatchRequest, DispatchTask, Service};
    let landing = landing()?;
    let (_, context) = super::worktree_tests::writer_context(&landing.rig)?;
    let store = super::worktree_tests::open_writer_store(&context)?;
    let unbound = landing.rig.root.join("unbound");
    fs::create_dir(&unbound)?;
    git_in(&landing.rig, &unbound, &["init", "-q"])?;
    let output = command(
        &landing.rig,
        &[
            "--project",
            unbound.to_str().ok_or("path")?,
            "pr",
            "contributions",
            NUMBER,
            "--json",
        ],
    )?;
    assert_eq!(code(&output), 0);
    assert_eq!(
        json(&output)?["data"]["coverage"]["local_attempts"],
        "unbound"
    );
    let retry_unbound = command(
        &landing.rig,
        &[
            "--project",
            unbound.to_str().ok_or("path")?,
            "pr",
            "contributions",
            NUMBER,
            "--bind",
            "--json",
        ],
    )?;
    assert_ne!(code(&retry_unbound), 0);
    let failure = json(&retry_unbound)?;
    assert!(
        failure["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("project is unbound")),
        "{failure}"
    );
    fs::write(
        landing.rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.fabric.peers.unreachable]\nhost='{}'\ndestination='unreachable.invalid'\n",
            provenant_core::HostId::from_bytes([99; 16]),
        ),
    )?;
    let wall = provenant_os::clock::SystemClock::new().wall_now()?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(wall.as_micros() - 31 * 86_400_000_000)?,
        provenant_core::MonoTime::from_micros(0),
    );
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let mut ids = Vec::new();
    for index in 0..4_u8 {
        let submit = || -> Result<DispatchRequest> {
            Ok(DispatchRequest {
                chair: None,
                operation_id: OperationId::parse(&format!("contribution-{index}"))?,
                tasks: vec![DispatchTask {
                    context: None,
                    build_heavy: false,
                    title: None,
                    profile: Default::default(),
                    task_id: TaskId::from_bytes([index + 1; 16]),
                    prompt: "local work".into(),
                    base: None,
                    head: None,
                    route: provenant_run::RequestedRoute::Named {
                        extra: Default::default(),
                        name: "senior".into(),
                        effort: None,
                    },
                    account: None,
                    fallback: vec![],
                    timeout_seconds: 60,
                    delivery_id: None,
                    deliver: None,
                    worktree: (index < 3).then(|| landing.worktree.clone()),
                    access: Default::default(),
                }],
                concurrency: 1,
                lane_depth: 1,
                parent_attempt: None,
            })
        };
        let run = Service::new(&store).dispatch(submit()?, &landing.worktree, &call)?;
        let attempt = store.pending_attempts(run.run_id, &call)?[0].attempt_id;
        ids.push(attempt.to_string());
        if index == 0 {
            // An attribution conflict after create cannot turn accepted work
            // into a failed dispatch. The host retains a diagnostic.
            let db = rusqlite::Connection::open(
                store.writer_registry().join("pr-contributions.sqlite3"),
            )?;
            db.execute(
                "UPDATE branch_attempts SET role='read_only' WHERE attempt=?1",
                [attempt.to_string()],
            )?;
            let replay = Service::new(&store).dispatch(submit()?, &landing.worktree, &call)?;
            assert_eq!(replay.run_id, run.run_id);
            let diagnostic = fs::read_to_string(context.state_root.as_path().join(format!(
                "runs/{}/pr-contribution-diagnostic.txt",
                run.run_id
            )))?;
            assert!(diagnostic.contains("PR contribution capture failed"));
            db.execute(
                "UPDATE branch_attempts SET role='worker' WHERE attempt=?1",
                [attempt.to_string()],
            )?;
        }
        assert_eq!(
            Service::new(&store)
                .dispatch(submit()?, &landing.worktree, &call)?
                .run_id,
            run.run_id
        );
        if index < 2 {
            contribution_terminal(
                &store,
                &context,
                attempt,
                &landing.head,
                if index == 0 {
                    provenant_run::Outcome::Succeeded
                } else {
                    provenant_run::Outcome::Failed
                },
                &call,
            )?;
            continue;
        }
        store.withdraw(
            attempt,
            provenant_run::AttemptState::Queued {
                extra: Default::default(),
            },
            provenant_run::Outcome::Cancelled,
            "fixture ended",
            clock.wall_now()?,
            &call,
        )?;
        if index == 2 {
            let resume = provenant_run::Resume {
                prior: attempt,
                expected_generation: 0,
                operation_id: OperationId::parse("contribution-repair")?,
                run_id: provenant_core::RunId::from_bytes([91; 16]),
                attempt_id: provenant_core::AttemptId::from_bytes([92; 16]),
                prompt: "repair".into(),
                created_at: clock.wall_now()?,
            };
            let repaired = store.resume_fresh(&resume, "repair fixture", &call)?;
            assert_eq!(
                store.resume_fresh(&resume, "repair fixture", &call)?.run_id,
                repaired.run_id
            );
            let id = store.pending_attempts(repaired.run_id, &call)?[0].attempt_id;
            ids.push(id.to_string());
            store.cancel(id, "repair cancelled", &call)?;
        }
    }
    let read = || -> Result<serde_json::Value> {
        let output = command(&landing.rig, &["pr", "contributions", NUMBER, "--json"])?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stdout.bytes)
        );
        Ok(json(&output)?["data"].clone())
    };
    assert!(read()?["attempts"].as_array().ok_or("attempts")?.is_empty());
    assert_eq!(read()?["coverage"]["local_attempts"], "unbound");
    git(&landing.rig, &["push", "-q", "origin", "feat"])?;
    let template = landing.rig.project.join(".github/pull_request_template.md");
    fs::write(
        &template,
        include_str!("../../../.github/pull_request_template.md"),
    )?;
    let summary = landing.rig.root.join("summary.md");
    fs::write(
        &summary,
        "# Capture contributions\n\nRetain local attempts.\n",
    )?;
    let open_args = [
        "pr",
        "open",
        "--from-attempt",
        &ids[0],
        "--branch",
        "feat",
        "--base",
        "main",
        "--summary-file",
        summary.to_str().ok_or("path")?,
        "--json",
    ];
    let mut dry = open_args.to_vec();
    dry.push("--dry-run");
    let preview = command(&landing.rig, &dry)?;
    assert_eq!(
        code(&preview),
        0,
        "{}",
        String::from_utf8_lossy(&preview.stdout.bytes)
    );
    assert!(read()?["attempts"].as_array().ok_or("attempts")?.is_empty());
    // PR creation succeeds even when its optional binding cannot be stored.
    let contributions = store.writer_registry().join("pr-contributions.sqlite3");
    let db = rusqlite::Connection::open(&contributions)?;
    db.execute_batch("CREATE TRIGGER reject_binding BEFORE INSERT ON branch_prs BEGIN SELECT RAISE(ABORT, 'fixture binding failure'); END;")?;
    let opened = command(&landing.rig, &open_args)?;
    assert_eq!(
        code(&opened),
        0,
        "{}",
        String::from_utf8_lossy(&opened.stdout.bytes)
    );
    let warning = String::from_utf8(opened.stderr.bytes)?;
    assert!(warning.contains("warning:"));
    assert!(warning.contains("provenant pr contributions 1668 --bind"));
    let closed_view = fs::read(landing.rig.project.join("view.json"))?;
    let mut open_view: serde_json::Value = serde_json::from_slice(&closed_view)?;
    open_view["state"] = serde_json::json!("OPEN");
    fs::write(
        landing.rig.project.join("view.json"),
        serde_json::to_vec(&open_view)?,
    )?;
    fs::write(landing.rig.project.join("checks-fail"), "")?;
    let unbound_land = command(&landing.rig, &["land", NUMBER, "--json"])?;
    assert_ne!(code(&unbound_land), 0);
    assert_eq!(json(&unbound_land)?["error"]["code"], "CI_UNAVAILABLE");
    assert!(
        String::from_utf8_lossy(&unbound_land.stderr.bytes)
            .contains("provenant pr contributions 1668 --bind")
    );
    fs::write(landing.rig.project.join("view.json"), closed_view)?;
    fs::remove_file(landing.rig.project.join("checks-fail"))?;
    db.execute_batch("DROP TRIGGER reject_binding;")?;
    let retry = || -> Result {
        let output = command(
            &landing.rig,
            &["pr", "contributions", NUMBER, "--bind", "--json"],
        )?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(())
    };
    let merged_view = fs::read(landing.rig.project.join("view.json"))?;
    let mut open_view: serde_json::Value = serde_json::from_slice(&merged_view)?;
    open_view["state"] = serde_json::json!("OPEN");
    fs::write(
        landing.rig.project.join("view.json"),
        serde_json::to_vec(&open_view)?,
    )?;
    retry()?;
    retry()?;
    let bound = read()?;
    assert_eq!(bound["attempts"].as_array().ok_or("attempts")?.len(), 5);
    fs::remove_file(&template)?;
    fs::write(landing.rig.project.join("view.json"), merged_view)?;
    db.execute_batch("CREATE TRIGGER reject_binding BEFORE INSERT ON branch_prs BEGIN SELECT RAISE(ABORT, 'fixture binding failure'); END;")?;
    db.execute_batch("CREATE TRIGGER reject_binding_update BEFORE UPDATE OF head ON branch_prs BEGIN SELECT RAISE(ABORT, 'fixture head refresh failure'); END;")?;
    // A failed bind/head refresh cannot prevent recording an observed closure,
    // even when CI is unavailable after the merge.
    fs::write(landing.rig.project.join("checks-fail"), "")?;
    let retained = landing.worktree.join("cleanup-pending.txt");
    fs::write(&retained, "uncommitted work must survive\n")?;
    let before = fs::read_to_string(landing.rig.project.join("gh-calls"))?;
    let refused = command(&landing.rig, &["land", NUMBER, "--json"])?;
    assert_eq!(code(&refused), 0);
    assert_eq!(json(&refused)?["data"]["merged"], true);
    assert_eq!(json(&refused)?["data"]["cleanup_pending"], true);
    assert!(retained.exists());
    let after = fs::read_to_string(landing.rig.project.join("gh-calls"))?;
    assert!(!after[before.len()..].contains("pr merge"));
    assert!(!after[before.len()..].contains("/commits/"));
    assert_eq!(
        db.query_row(
            "SELECT min(closed) FROM branch_prs WHERE number=1668",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        1
    );
    fs::remove_file(retained)?;
    fs::remove_file(landing.rig.project.join("checks-fail"))?;
    let output = command(&landing.rig, &["land", NUMBER, "--json"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("no open contribution binding"));
    let landed = json(&output)?;
    db.execute_batch("DROP TRIGGER reject_binding; DROP TRIGGER reject_binding_update;")?;
    assert_eq!(status(&landed, "worktree")?, "done", "{landed}");
    assert!(!landing.worktree.exists());
    let before = read()?;
    let rows = before["attempts"].as_array().ok_or("attempts")?;
    assert_eq!(rows.len(), 5, "{before}");
    assert_eq!(before, bound);
    for id in &ids {
        let row = rows
            .iter()
            .find(|row| row["attempt"] == id.as_str())
            .ok_or("missing attempt")?;
        assert!(row["weight_millionths"].is_null());
        assert!(row["run"].is_string());
        assert!(row["queued_at"].is_string());
        assert!(row["finished_at"].is_string());
        assert!(row["outcome"].is_string());
        assert_eq!(row["pr_count"], 1);
        assert_eq!(row["ambiguous"], false);
        if row["attempt"] == ids[0] || row["attempt"] == ids[1] {
            assert_eq!(row["model"], "codex/fixture");
            assert_eq!(row["account"], "fixture");
        } else {
            assert!(row["model"].is_null());
            assert!(row["account"].is_null());
        }
        assert_eq!(row["head"], landing.head);
    }
    assert_eq!(
        rows.iter().filter(|row| row["role"] == "read_only").count(),
        1
    );
    assert_eq!(rows.iter().filter(|row| row["role"] == "repair").count(), 1);
    assert_eq!(before["coverage"]["peer_attempts"], "unknown");
    assert_eq!(
        before["coverage"]["known_peer_hosts"],
        serde_json::json!(["unreachable"])
    );
    let text = command(&landing.rig, &["pr", "contributions", NUMBER, "--full"])?;
    assert_eq!(code(&text), 0);
    let text = String::from_utf8(text.stdout.bytes)?;
    assert!(text.contains("role=repair model=unknown account=unknown weight=unknown"));
    assert!(text.contains("peer_attempts: unknown (known hosts: unreachable)"));
    let first = store.record(ids[0].parse()?, &call)?;
    let log = context.state_root.as_path().join(format!(
        "runs/{}/attempts/{}/stdout.log",
        first.run_id, first.attempt_id
    ));
    fs::write(&log, vec![b'x'; 128 * 1024])?;
    let gc = command(&landing.rig, &["gc", "--apply", "--json"])?;
    assert_eq!(
        code(&gc),
        0,
        "{}",
        String::from_utf8_lossy(&gc.stdout.bytes)
    );
    assert!(!log.exists(), "expired capture was collected");
    assert_eq!(
        fs::metadata(log.with_file_name("stdout.log.tail"))?.len(),
        64 * 1024
    );
    assert_eq!(read()?, before);
    land(&landing)?;
    assert_eq!(read()?, before);
    // Recreate the branch after land. A new dispatch and new PR must share
    // no attempts with the retained, merged PR, even after bind replay.
    let recreated = command(&landing.rig, &["worktree", "create", "feat", "--json"])?;
    assert_eq!(code(&recreated), 0);
    let dispatch_reused = |operation: &str, task: u8| -> Result<provenant_core::AttemptId> {
        let new_run = Service::new(&store).dispatch(
            DispatchRequest {
                chair: None,
                operation_id: OperationId::parse(operation)?,
                tasks: vec![DispatchTask {
                    context: None,
                    build_heavy: false,
                    title: None,
                    profile: Default::default(),
                    task_id: TaskId::from_bytes([task; 16]),
                    prompt: "new work".into(),
                    base: None,
                    head: None,
                    route: provenant_run::RequestedRoute::Named {
                        extra: Default::default(),
                        name: "senior".into(),
                        effort: None,
                    },
                    account: None,
                    fallback: vec![],
                    timeout_seconds: 60,
                    delivery_id: None,
                    deliver: None,
                    worktree: Some(landing.worktree.clone()),
                    access: Default::default(),
                }],
                concurrency: 1,
                lane_depth: 1,
                parent_attempt: None,
            },
            &landing.worktree,
            &call,
        )?;
        let new_attempt = store.pending_attempts(new_run.run_id, &call)?[0].attempt_id;
        store.cancel(new_attempt, "fixture ended", &call)?;
        Ok(new_attempt)
    };
    let new_attempt = dispatch_reused("reused-branch", 98)?;
    assert_eq!(read()?, before);
    let links = provenant_run::Contributions::open(store.writer_registry(), &call)?;
    let new_pr = provenant_run::PullRequest {
        repository: "owner/repo".into(),
        number: 1669,
    };
    links.bind(
        context.project_id,
        "feat",
        &new_pr,
        provenant_core::GitOid::parse(&landing.head)?,
        &call,
    )?;
    let new_rows = links.for_pr(&new_pr, &call)?;
    assert_eq!(new_rows.len(), 1);
    assert_eq!(new_rows[0].attempt, new_attempt);
    retry()?;
    assert_eq!(read()?, before);
    let second_new_pr = provenant_run::PullRequest {
        number: 1670,
        ..new_pr.clone()
    };
    links.bind(
        context.project_id,
        "feat",
        &second_new_pr,
        provenant_core::GitOid::parse(&landing.head)?,
        &call,
    )?;
    let output = command(&landing.rig, &["pr", "contributions", "1669", "--json"])?;
    assert_eq!(code(&output), 0);
    let data = json(&output)?["data"].clone();
    assert_eq!(data["coverage"]["local_attempts"], "ambiguous");
    assert_eq!(data["coverage"]["ambiguous"], true);
    assert_eq!(data["attempts"][0]["pr_count"], 2);
    // External closures are swept even when binding a closed target. Each
    // candidate is checked once despite bindings spanning generations.
    let previous = links.for_pr(&new_pr, &call)?;
    fs::write(landing.rig.project.join("state-1669"), "MERGED")?;
    fs::write(landing.rig.project.join("state-1670"), "CLOSED")?;
    fs::write(landing.rig.project.join("state-1671"), "CLOSED")?;
    let calls_before = fs::read_to_string(landing.rig.project.join("gh-calls"))?;
    let bind_cli = |number: &str| -> Result<provenant_os::exec::ExecOutcome> {
        let before = fs::read_to_string(landing.rig.project.join("gh-calls"))?;
        let output = command(
            &landing.rig,
            &["pr", "contributions", number, "--bind", "--json"],
        )?;
        let after = fs::read_to_string(landing.rig.project.join("gh-calls"))?;
        assert_eq!(
            after[before.len()..]
                .lines()
                .filter(|line| *line == "repo view --json nameWithOwner")
                .count(),
            1
        );
        Ok(output)
    };
    let swept = bind_cli("1671")?;
    assert_eq!(
        code(&swept),
        0,
        "{}",
        String::from_utf8_lossy(&swept.stderr.bytes)
    );
    assert!(
        links
            .open_bindings(context.project_id, "feat", &call)?
            .is_empty()
    );
    let calls_after = fs::read_to_string(landing.rig.project.join("gh-calls"))?;
    for number in [1669, 1670] {
        let expected = format!("pr view {number} --repo owner/repo --json state,mergedAt,closedAt");
        assert_eq!(
            calls_after[calls_before.len()..]
                .lines()
                .filter(|line| *line == expected)
                .count(),
            1
        );
    }
    assert_eq!(links.for_pr(&new_pr, &call)?, previous);
    let bound_new = bind_cli("1672")?;
    assert_eq!(code(&bound_new), 0);
    let calls_before = fs::read(landing.rig.project.join("gh-calls"))?;
    let reused = dispatch_reused("externally-merged-branch", 99)?;
    assert_eq!(
        fs::read(landing.rig.project.join("gh-calls"))?,
        calls_before,
        "capture stays offline"
    );
    let latest = provenant_run::PullRequest {
        number: 1672,
        ..new_pr.clone()
    };
    let rows = links.for_pr(&latest, &call)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempt, reused);
    assert_eq!(rows[0].pr_count, 1);
    assert_eq!(links.for_pr(&new_pr, &call)?, previous);
    assert_eq!(read()?, before);
    links.allocate(
        reused,
        &[(latest.clone(), provenant_run::WEIGHT_ONE)],
        &call,
    )?;
    let skipped = bind_cli("1673")?;
    assert_eq!(code(&skipped), 0);
    let data = json(&skipped)?["data"].clone();
    assert_eq!(data["coverage"]["local_attempts"], "skipped");
    assert_eq!(data["coverage"]["skipped_sealed"], 1);
    assert_eq!(data["attempts"].as_array().ok_or("attempts")?.len(), 0);
    assert_eq!(
        links.for_pr(&latest, &call)?[0].weight_millionths,
        Some(provenant_run::WEIGHT_ONE)
    );

    // pr open uses the same sweep before binding its newly created PR.
    fs::write(landing.rig.project.join("state-1672"), "MERGED")?;
    fs::write(landing.rig.project.join("state-1673"), "CLOSED")?;
    fs::write(landing.rig.project.join("create-number"), "1674")?;
    fs::write(
        &template,
        include_str!("../../../.github/pull_request_template.md"),
    )?;
    let opened = command(&landing.rig, &open_args)?;
    assert_eq!(
        code(&opened),
        0,
        "{}",
        String::from_utf8_lossy(&opened.stderr.bytes)
    );
    let active = links.open_bindings(context.project_id, "feat", &call)?;
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].number, 1674);
    let after_open = dispatch_reused("swept-pr-open", 100)?;
    let rows = links.for_pr(&active[0], &call)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempt, after_open);
    assert_eq!(rows[0].pr_count, 1);

    // A failed gh sweep warns and retains the previous sharing behaviour.
    let fallback = bind_cli("1675")?;
    assert_eq!(code(&fallback), 0);
    assert!(String::from_utf8_lossy(&fallback.stderr.bytes).contains("stale binding sweep failed"));
    assert_eq!(
        links
            .open_bindings(context.project_id, "feat", &call)?
            .len(),
        2
    );
    assert_eq!(links.for_pr(&active[0], &call)?[0].pr_count, 2);

    // Successful land must warn too when its bind failed and close has no row.
    let unbound_landing = self::landing()?;
    let (_, context) = super::worktree_tests::writer_context(&unbound_landing.rig)?;
    let store = super::worktree_tests::open_writer_store(&context)?;
    let _links = provenant_run::Contributions::open(store.writer_registry(), &call)?;
    let db = rusqlite::Connection::open(store.writer_registry().join("pr-contributions.sqlite3"))?;
    db.execute_batch("CREATE TRIGGER reject_binding BEFORE INSERT ON branch_prs BEGIN SELECT RAISE(ABORT, 'fixture binding failure'); END;")?;
    let landed = command(&unbound_landing.rig, &["land", NUMBER, "--json"])?;
    assert_eq!(
        code(&landed),
        0,
        "{}",
        String::from_utf8_lossy(&landed.stdout.bytes)
    );
    assert!(String::from_utf8_lossy(&landed.stderr.bytes).contains("no open contribution binding"));
    let calls = fs::read_to_string(unbound_landing.rig.project.join("gh-calls"))?;
    assert_eq!(
        calls
            .lines()
            .filter(|line| *line == "repo view --json nameWithOwner")
            .count(),
        1
    );
    Ok(())
}

/// Provider exit seam shared with the landing fixtures; publication is real.
fn contribution_terminal(
    store: &provenant_run::RunStore,
    context: &provenant_run::ExecutionContext,
    id: provenant_core::AttemptId,
    head: &str,
    outcome: provenant_run::Outcome,
    call: &provenant_run::Request<'_>,
) -> Result {
    use provenant_supervisor::custody::{
        ExitObservation, PublicationState, Scope, SettlementEvidence,
    };
    use std::os::unix::fs::OpenOptionsExt;
    let record = store.record(id, call)?;
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: record.run_id,
            attempt: id,
            generation: record.generation,
        },
        termination: Some(provenant_os::exec::Termination::Exit(
            if outcome == provenant_run::Outcome::Succeeded {
                0
            } else {
                1
            },
        )),
        never_spawned: false,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: true,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
        stdout: Default::default(),
        stderr: Default::default(),
    };
    let mut evidence = serde_json::to_value(&exit)?;
    evidence
        .as_object_mut()
        .ok_or("exit fixture")?
        .remove("publication");
    evidence["schema_version"] = 1.into();
    let directory = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}", record.run_id));
    for (name, bytes) in [
        ("process-exit.json", serde_json::to_vec(&evidence)?),
        (
            "writer-terminal.json",
            serde_json::to_vec(
                &serde_json::json!({"schema_version":1,"head":head,"protected_paths":[]}),
            )?,
        ),
        ("custody.lock", vec![]),
    ] {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(name))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    let route = provenant_run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/fixture".into()),
        account_label: Some("fixture".into()),
        ..Default::default()
    };
    store.publish_terminal(
        id,
        record.revision,
        &provenant_run::TerminalPublication {
            outcome,
            started_at: Some(record.queued_at),
            finished_at: call.clock.wall_now()?,
            provider_ms: Some(1),
            reason: None,
            result: "fixture output",
            complete: true,
            redacted: false,
            resolved_route: Some(route.clone()),
            applied_route: Some(route.clone()),
            observed_route: Some(route),
            usage: Default::default(),
            session: None,
            partial_signals: Vec::new(),
        },
        &exit,
        call,
    )?;
    Ok(())
}

/// The fake `gh` the driver re-enters as: a passing, unchanged pull request.
pub(crate) fn fake_gh(args: &[String]) -> Result<u8> {
    let call = args.join(" ");
    // Real gh rejects JSON fields it does not have; so does this fake.
    if let Some(at) = args.iter().position(|arg| arg == "--json") {
        let known: &[&str] = match (
            args.first().map(String::as_str),
            args.get(1).map(String::as_str),
        ) {
            (Some("pr"), Some("view" | "list")) => &[
                "baseRefName",
                "baseRefOid",
                "closedAt",
                "headRefName",
                "headRefOid",
                "headRepository",
                "headRepositoryOwner",
                "isCrossRepository",
                "isDraft",
                "mergeStateStatus",
                "mergeable",
                "mergedAt",
                "number",
                "reviewDecision",
                "state",
                "statusCheckRollup",
                "title",
                "url",
            ],
            (Some("pr"), Some("checks")) => &[
                "bucket",
                "completedAt",
                "description",
                "event",
                "link",
                "name",
                "startedAt",
                "state",
                "workflow",
            ],
            (Some("repo"), Some("view")) => {
                &["defaultBranchRef", "name", "nameWithOwner", "owner", "url"]
            }
            _ => &[],
        };
        for field in args
            .get(at + 1)
            .map(String::as_str)
            .unwrap_or("")
            .split(',')
        {
            if !known.is_empty() && !known.contains(&field) {
                let _ = writeln!(io::stderr(), "Unknown JSON field: {field:?}");
                return Ok(1);
            }
        }
    }
    let dir = std::env::var_os("FIXTURE_GH_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("gh-calls"))?,
        "{call}"
    )?;
    match call.as_str() {
        value if value.starts_with("pr create ") => {
            std::io::copy(&mut io::stdin(), &mut io::sink())?;
            let number = fs::read_to_string(dir.join("create-number")).unwrap_or("1668".into());
            writeln!(io::stdout(), "https://github.com/owner/repo/pull/{number}")?;
        }
        "repo view --json nameWithOwner" => {
            io::stdout().write_all(br#"{"nameWithOwner":"owner/repo"}"#)?;
        }
        "pr view 1668 --json headRefOid" => {
            io::stdout().write_all(&fs::read(dir.join("head.json"))?)?
        }
        "pr view 1668 --json state,baseRefName,headRefName,isCrossRepository" => {
            io::stdout().write_all(&fs::read(dir.join("view.json"))?)?;
        }
        value if value.starts_with("pr view ") && value.ends_with(" --json state") => {
            let number = args.get(2).ok_or("PR number")?;
            let state = fs::read_to_string(dir.join(format!("state-{number}")))?;
            io::stdout().write_all(&serde_json::to_vec(&serde_json::json!({"state":state}))?)?;
        }
        // The stale-binding sweep also reads the end time; the fixture has none.
        value
            if value.starts_with("pr view ")
                && value.ends_with(" --json state,mergedAt,closedAt") =>
        {
            let number = args.get(2).ok_or("PR number")?;
            let state = fs::read_to_string(dir.join(format!("state-{number}")))?;
            io::stdout().write_all(&serde_json::to_vec(
                &serde_json::json!({"state":state,"mergedAt":null,"closedAt":null}),
            )?)?;
        }
        value if value.starts_with("pr view ") && value.ends_with(" --json headRefOid") => {
            io::stdout().write_all(&fs::read(dir.join("head.json"))?)?;
        }
        value
            if value.starts_with("pr view ")
                && value.ends_with(" --json state,baseRefName,headRefName,isCrossRepository") =>
        {
            let number = args.get(2).ok_or("PR number")?;
            let state =
                fs::read_to_string(dir.join(format!("state-{number}"))).unwrap_or("OPEN".into());
            io::stdout().write_all(&serde_json::to_vec(&serde_json::json!({"state":state,"baseRefName":"main","headRefName":"feat","isCrossRepository":false}))?)?;
        }
        "pr checks 1668 --watch" => {
            fs::write(dir.join("watched"), "")?;
        }
        "pr view 1668 --json baseRefName,url" => {
            io::stdout().write_all(
                br#"{"baseRefName":"main","url":"https://github.com/owner/repo/pull/1668"}"#,
            )?;
        }
        "api repos/owner/repo/branches/main" => {
            io::stdout().write_all(br#"{"protected":false}"#)?;
        }
        "api --paginate --slurp repos/owner/repo/rules/branches/main?per_page=100" => {
            io::stdout().write_all(b"[[]]")?;
        }
        value
            if value.starts_with("api --paginate --slurp repos/owner/repo/commits/")
                && value.ends_with("/statuses?per_page=100") =>
        {
            io::stdout().write_all(b"[[]]")?;
        }
        value
            if value.starts_with("api --paginate --slurp repos/owner/repo/commits/")
                && value.ends_with("/check-runs?per_page=100&filter=latest") =>
        {
            if dir.join("checks-fail").exists() {
                return Ok(1);
            }
            let head: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join("head.json"))?)?;
            io::stdout().write_all(&serde_json::to_vec(&serde_json::json!([{"check_runs":[{
                "name":"build","head_sha":head["headRefOid"],"status":"completed","conclusion":"success","started_at":"2020-01-01T00:00:00Z"
            }]}]))?)?;
        }
        "pr checks 1668 --json bucket" | "pr checks 1668 --json name,bucket" => {
            if dir.join("checks-fail").exists() {
                return Ok(8);
            }
            io::stdout().write_all(br#"[{"bucket":"pass"}]"#)?;
        }
        value if value.starts_with("pr merge 1668 --merge --match-head-commit ") => {
            fs::write(dir.join("merge-args"), args.join("\n") + "\n")?;
            if dir.join("merge-completes").exists() {
                let mut view: serde_json::Value =
                    serde_json::from_slice(&fs::read(dir.join("view.json"))?)?;
                view["state"] = "MERGED".into();
                fs::write(dir.join("view.json"), serde_json::to_vec(&view)?)?;
            }
        }
        _ => return Err(format!("unexpected gh arguments: {call}").into()),
    }
    Ok(0)
}

struct Landing {
    rig: TestRig,
    worktree: PathBuf,
    /// The branch head the pull request merged.
    head: String,
    /// `main` before the merge, which the local checkout still has.
    stale: String,
}

fn git_in(rig: &TestRig, dir: &Path, args: &[&str]) -> Result<String> {
    let dir = dir.to_str().ok_or("path")?;
    let mut argv = vec!["-C", dir];
    argv.extend_from_slice(args);
    git(rig, &argv)
}

/// A project whose `feat` worktree was merged into `origin/main` by a merge
/// commit, with the local `main` still before it.
fn landing() -> Result<Landing> {
    landing_with_git_dir(false)
}

fn landing_with_git_dir(separate: bool) -> Result<Landing> {
    let mut rig = repository()?;
    if separate {
        git(
            &rig,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                rig.root.join("separate.git").to_str().ok_or("path")?,
            ],
        )?;
    }
    let admin = rig.project.join(git(&rig, &["rev-parse", "--git-dir"])?);
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    symlink(std::env::current_exe()?, bin.join("gh"))?;
    let (_, path) = rig
        .env
        .iter_mut()
        .find(|(key, _)| key == "PATH")
        .ok_or("PATH")?;
    *path = format!("{}:/usr/bin:/bin", bin.display()).into();
    fs::write(
        admin.join("info/exclude"),
        "head.json\nview.json\ngh-calls\nwatched\nmerge-args\nchecks-fail\nmerge-completes\n",
    )?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n**/target/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore build output"])?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    let origin = rig.root.join("origin.git");
    git(
        &rig,
        &["init", "-q", "--bare", origin.to_str().ok_or("path")?],
    )?;
    git(
        &rig,
        &["remote", "add", "origin", origin.to_str().ok_or("path")?],
    )?;
    git(&rig, &["push", "-q", "origin", "main"])?;
    let created = command(&rig, &["worktree", "create", "feat", "--json"])?;
    assert_eq!(
        code(&created),
        0,
        "{}",
        String::from_utf8_lossy(&created.stdout.bytes)
    );
    let worktree = rig.project.join(".worktrees/feat");
    fs::write(worktree.join("feature.txt"), "feature\n")?;
    git_in(&rig, &worktree, &["add", "feature.txt"])?;
    git_in(&rig, &worktree, &["commit", "-qm", "feature"])?;
    let head = git_in(&rig, &worktree, &["rev-parse", "HEAD"])?;
    git(
        &rig,
        &["merge", "-q", "--no-ff", "-m", "merge feat", "feat"],
    )?;
    git(&rig, &["push", "-q", "origin", "main"])?;
    git(&rig, &["reset", "-q", "--hard", &stale])?;
    fs::write(
        rig.project.join("head.json"),
        format!(r#"{{"headRefOid":"{head}"}}"#),
    )?;
    fs::write(
        rig.project.join("view.json"),
        r#"{"state":"MERGED","baseRefName":"main","headRefName":"feat","isCrossRepository":false}"#,
    )?;
    Ok(Landing {
        rig,
        worktree,
        head,
        stale,
    })
}

fn land(landing: &Landing) -> Result<serde_json::Value> {
    let output = command(&landing.rig, &["land", NUMBER, "--json"])?;
    let value = json(&output)?;
    assert!(
        matches!(code(&output), 0 | 5) || value["data"]["merged"] == true,
        "{value}"
    );
    assert_eq!(value["data"]["head"], landing.head);
    Ok(value)
}

fn status<'a>(value: &'a serde_json::Value, step: &str) -> Result<&'a str> {
    value["data"]["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|entry| entry["step"] == step))
        .and_then(|entry| entry["status"].as_str())
        .ok_or_else(|| format!("no {step} step in {value}").into())
}

fn branch_exists(rig: &TestRig, branch: &str) -> Result<bool> {
    Ok(!git(rig, &["branch", "--list", branch])?.is_empty())
}

pub(crate) fn pr_land_merges_then_fetches_fast_forwards_and_prunes() -> Result {
    let landing = landing()?;
    let rig = &landing.rig;
    let mut view: serde_json::Value =
        serde_json::from_slice(&fs::read(rig.project.join("view.json"))?)?;
    view["state"] = "OPEN".into();
    fs::write(rig.project.join("view.json"), serde_json::to_vec(&view)?)?;
    fs::write(rig.project.join("merge-completes"), "")?;
    // Build output the worktree owns, which would otherwise block removal.
    fs::create_dir_all(landing.worktree.join("crate/target/debug"))?;
    fs::write(
        landing.worktree.join("crate/target/debug/artifact"),
        "built",
    )?;
    let output = command(rig, &["land", NUMBER, "--head", &landing.head, "--json"])?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(value["data"]["merged"], true);
    for step in ["fetch", "fast_forward", "worktree", "branch"] {
        assert_eq!(status(&value, step)?, "done", "{step}: {value}");
    }
    // The merge went through the existing repository workflow at this head.
    let merge = fs::read_to_string(rig.project.join("merge-args"))?;
    assert!(
        merge.contains(&format!("--match-head-commit\n{}", landing.head)),
        "{merge}"
    );
    assert!(!rig.project.join("watched").exists());
    assert_eq!(
        git(rig, &["rev-parse", "main"])?,
        git(rig, &["rev-parse", "origin/main"])?
    );
    assert_ne!(git(rig, &["rev-parse", "main"])?, landing.stale);
    assert!(!landing.worktree.exists());
    assert!(!branch_exists(rig, "feat")?);
    // Landing again finds nothing left to prune, and the text form names each step.
    let text = command(rig, &["land", NUMBER, "--full"])?;
    assert_eq!(code(&text), 0);
    let text = String::from_utf8(text.stdout.bytes)?;
    assert!(
        text.starts_with(&format!("merged #{NUMBER} {}\n", landing.head)),
        "{text}"
    );
    assert!(text.contains("  worktree skipped"), "{text}");
    assert!(text.contains("  branch skipped"), "{text}");
    Ok(())
}

pub(crate) fn pr_land_prunes_the_selected_merged_worktree() -> Result {
    let mut landing = landing()?;
    // Git retains registrations of unrelated checkouts that disappeared.
    let missing = landing.rig.project.join(".worktrees/disappeared");
    git(
        &landing.rig,
        &[
            "worktree",
            "add",
            "--detach",
            missing.to_str().ok_or("path")?,
        ],
    )?;
    fs::remove_dir_all(&missing)?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let path = landing.worktree.to_str().ok_or("path")?;
    let output = command(&landing.rig, &["--project", path, "land", NUMBER, "--json"])?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(status(&value, "worktree")?, "done", "{value}");
    assert_eq!(status(&value, "branch")?, "done", "{value}");
    assert!(!landing.worktree.exists());
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_refuses_incomplete_worker_census() -> Result {
    for observation in ["initial", "late"] {
        let mut landing = landing()?;
        landing.rig.env.push((
            "FIXTURE_GH_DIR".into(),
            landing.rig.project.clone().into_os_string(),
        ));
        landing
            .rig
            .env
            .push(("FIXTURE_UNKNOWN_WORKERS".into(), observation.into()));
        let output = command(
            &landing.rig,
            &[
                "--project",
                landing.worktree.to_str().ok_or("path")?,
                "land",
                NUMBER,
                "--json",
            ],
        )?;
        let value = json(&output)?;
        assert_eq!(code(&output), 0, "{value}");
        assert_eq!(value["data"]["cleanup_pending"], true, "{value}");
        assert_eq!(status(&value, "worktree")?, "refused", "{value}");
        let text = value.to_string();
        assert!(
            text.contains("incomplete") && text.contains("retry"),
            "{value}"
        );
        assert!(landing.worktree.is_dir());
        assert!(branch_exists(&landing.rig, "feat")?);
    }
    Ok(())
}

// The shell remains parked in the lane while its child runs land. A complete
// census seam makes the positive cases independent of host-wide visibility;
// native cwd observations exercise refusal for root/subdirectory contenders.
// The confinement census test proves ancestor exclusion at the process seam.
#[allow(clippy::disallowed_methods)] // Real child cwd exercises the native worker observer.
pub(crate) fn pr_land_from_lane_ignores_only_its_callers() -> Result {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for (from_subdirectory, worker_subdirectory) in [
        (false, None),
        (true, None),
        (false, Some(false)),
        (false, Some(true)),
    ] {
        let another_worker = worker_subdirectory.is_some();
        let mut landing = landing()?;
        let main = landing.rig.project.clone();
        landing
            .rig
            .env
            .push(("FIXTURE_GH_DIR".into(), main.clone().into_os_string()));
        if another_worker {
            landing
                .rig
                .env
                .push(("FIXTURE_SYSTEM_WORKERS".into(), "1".into()));
        }
        let nested = landing.worktree.join("nested");
        assert!(nested.is_dir());
        let cwd_file = landing.rig.root.join("cwd-after-land");
        landing.rig.env.push((
            "FIXTURE_LAND_CWD_FILE".into(),
            cwd_file.clone().into_os_string(),
        ));
        let worker = if another_worker {
            Some(Child(
                std::process::Command::new("/bin/sh")
                    .args(["-c", "read line"])
                    .current_dir(if worker_subdirectory == Some(true) {
                        &nested
                    } else {
                        &landing.worktree
                    })
                    .stdin(std::process::Stdio::piped())
                    .spawn()?,
            ))
        } else {
            None
        };
        landing.rig.project = if from_subdirectory {
            nested
        } else {
            landing.worktree.clone()
        };
        let executable = std::env::current_exe()?;
        let state = landing.rig.state.to_str().ok_or("state path")?;
        let output = landing.rig.execute(
            Path::new("/bin/sh"),
            &[
                "-c",
                "\"$@\" & wait $!",
                "land-shell",
                executable.to_str().ok_or("path")?,
                "--state-dir",
                state,
                "land",
                NUMBER,
                "--json",
            ],
            None,
        )?;
        let value = json(&output)?;
        assert_eq!(
            Path::new(&fs::read_to_string(&cwd_file)?),
            main,
            "land must render from the surviving checkout"
        );
        let notice = "your shell's directory was removed";
        assert_eq!(
            value.to_string().matches(notice).count(),
            usize::from(!another_worker),
            "{value}"
        );
        assert_eq!(code(&output), 0, "{value}");
        assert_eq!(value["data"]["cleanup_pending"], another_worker, "{value}");
        assert_eq!(
            status(&value, "worktree")?,
            if another_worker { "refused" } else { "done" },
            "{value}"
        );
        assert_eq!(landing.worktree.exists(), another_worker, "{value}");
        if another_worker {
            assert!(
                value.to_string().contains("worktree writer is live"),
                "{value}"
            );
        }
        drop(worker);
        landing.rig.project = main;
        assert_eq!(branch_exists(&landing.rig, "feat")?, another_worker);
    }
    Ok(())
}

pub(crate) fn pr_land_canonicalises_quoted_worktree_paths() -> Result {
    let mut landing = landing()?;
    let alias = landing.rig.root.join("alias\twith quotes\"");
    symlink(&landing.rig.project, &alias)?;
    fs::write(
        landing.rig.project.join(".git/worktrees/feat/gitdir"),
        format!("{}/.worktrees/feat/.git\n", alias.display()),
    )?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let selected = alias.join(".worktrees/feat");
    let output = command(
        &landing.rig,
        &[
            "--project",
            selected.to_str().ok_or("path")?,
            "land",
            NUMBER,
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(status(&value, "worktree")?, "done", "{value}");
    assert!(!landing.worktree.exists());
    Ok(())
}

pub(crate) fn pr_land_recovers_only_abandoned_prepared_writer_leases() -> Result {
    use provenant_core::Clock;
    use provenant_supervisor::custody::{Scope, WriterBinding};
    for case in [
        "abandoned",
        "held",
        "malformed",
        "wrong_binding",
        "dirty",
        "symlink_output",
        "protected_output",
        "allowed_output",
    ] {
        let landing = landing()?;
        let (_, context) = super::worktree_tests::writer_context(&landing.rig)?;
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        let request = provenant_run::resolve_writer(
            context.state_root.as_path(),
            &landing.rig.project,
            &landing.worktree,
        )?;
        let writer = provenant_run::prepare_writer(
            &request,
            context.state_root.as_path(),
            context.project_id,
        )?;
        let binding = WriterBinding::from_verified(
            writer.worktree.repo().root_dir(),
            writer.worktree.repo().private_dir(),
            if case == "wrong_binding" {
                "refs/heads/other"
            } else {
                writer.branch.as_str()
            },
            provenant_core::GitOid::parse(&landing.head)?,
        )?;
        let state = landing
            .rig
            .state
            .join("worktrees")
            .join(context.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &landing.worktree,
            ));
        let lease = provenant_supervisor::fixture::acquire_writer_with_binding(
            provenant_os::fs::Directory::open_anchor(&state)?,
            Scope {
                run: provenant_core::RunId::from_bytes([81; 16]),
                attempt: provenant_core::AttemptId::from_bytes([82; 16]),
                generation: 1,
            },
            binding,
            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?;
        let lease = if case == "held" {
            Some(lease)
        } else {
            drop(lease);
            None
        };
        if case == "malformed" {
            fs::write(state.join("reservation.json"), b"malformed reservation")?;
        }
        let reservation = fs::read(state.join("reservation.json"))?;
        match case {
            "dirty" => fs::write(landing.worktree.join("notes.txt"), "uncommitted")?,
            "symlink_output" => {
                fs::create_dir(landing.rig.root.join("external"))?;
                fs::write(landing.rig.root.join("external/artifact"), "preserve")?;
                symlink(
                    landing.rig.root.join("external"),
                    landing.worktree.join("target"),
                )?;
            }
            "protected_output" => {
                fs::create_dir(landing.worktree.join("target"))?;
                fs::write(landing.worktree.join("target/.git"), "protected repository")?;
            }
            "allowed_output" => {
                fs::create_dir(landing.worktree.join("target"))?;
                fs::write(landing.worktree.join("target/artifact"), "regenerable")?;
            }
            _ => {}
        }
        let value = land(&landing)?;
        let removed = matches!(case, "abandoned" | "allowed_output");
        assert_eq!(
            status(&value, "worktree")?,
            if removed { "done" } else { "refused" },
            "{case}: {value}"
        );
        assert_eq!(landing.worktree.exists(), !removed);
        assert_eq!(branch_exists(&landing.rig, "feat")?, !removed);
        if removed {
            assert!(!state.join("reservation.json").exists());
            assert!(state.join("settlement.json").exists());
        } else {
            assert_eq!(fs::read(state.join("reservation.json"))?, reservation);
            if case == "dirty" {
                assert_eq!(
                    fs::read_to_string(landing.worktree.join("notes.txt"))?,
                    "uncommitted"
                );
            }
            if case == "protected_output" {
                assert_eq!(
                    fs::read_to_string(landing.worktree.join("target/.git"))?,
                    "protected repository"
                );
            }
            if case == "symlink_output" {
                assert!(landing.worktree.join("target").is_symlink());
                assert_eq!(
                    fs::read_to_string(landing.rig.root.join("external/artifact"))?,
                    "preserve"
                );
            }
        }
        drop(lease);
    }
    Ok(())
}

/// Terminal records may live in a child store while custody remains in the
/// host registry. Exercise land through the production recursive snapshot.
pub(crate) fn pr_land_recovers_terminal_writer_leases_in_parent_and_child_stores() -> Result {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::fs::Directory;
    use provenant_run::{AttemptState, RouteRecord, RunStore, TerminalPublication};
    use provenant_supervisor::custody::{
        ExitObservation, PublicationState, Scope, SettlementEvidence, WriterBinding,
    };
    use std::os::unix::fs::OpenOptionsExt;
    for child in [false, true] {
        let landing = landing()?;
        let rig = &landing.rig;
        let (roots, mut context) = super::worktree_tests::writer_context(rig)?;
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 10)?,
            cancel: &cancel,
        };
        // Keep a normal host store beside the nested store, as the snapshot
        // sees it during a real dispatch from a lane.
        drop(RunStore::open(&context, &call)?);
        if child {
            context.state_root = crate::roots::child(
                &rig.home,
                &rig.project,
                rig.state.join("children").join("retained-writer"),
            )?
            .state;
        }
        let store = RunStore::open(&context, &call)?.with_writer_registry(&rig.state);
        let request = provenant_run::resolve_writer(&rig.state, &rig.project, &landing.worktree)?;
        let input =
            super::worktree_tests::writer_run(request.clone(), "land-terminal", 91, "done")?;
        store.create(&input, &call)?;
        let id = input.attempts[0].attempt_id;
        let mut running = store.record(id, &call)?;
        running.revision += 1;
        running.generation = 1;
        running.state = AttemptState::Running {
            extra: Default::default(),
        };
        running.started_at = Some(clock.wall_now()?);
        running.provider_launched = Some(true);
        // The provider seam advances admission without launching a backend;
        // terminal publication below still uses Run's canonical transaction.
        rusqlite::Connection::open(context.state_root.as_path().join("run.sqlite3"))?.execute(
            "UPDATE attempts SET revision=?1,record=?2 WHERE id=?3",
            rusqlite::params![
                i64::try_from(running.revision)?,
                provenant_records::CanonicalV1::bytes(&running)?.as_bytes(),
                id.to_string()
            ],
        )?;
        let scope = Scope {
            run: input.run_id,
            attempt: id,
            generation: running.generation,
        };
        let attempt = context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}", input.run_id));
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope,
            termination: Some(provenant_os::exec::Termination::Exit(0)),
            never_spawned: false,
            stop_reason: None,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: true,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
            stdout: Default::default(),
            stderr: Default::default(),
        };
        let mut evidence = serde_json::to_value(&exit)?;
        evidence
            .as_object_mut()
            .ok_or("exit fixture")?
            .remove("publication");
        evidence["schema_version"] = 1.into();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(attempt.join("process-exit.json"))?;
        file.write_all(&serde_json::to_vec(&evidence)?)?;
        file.sync_all()?;
        drop(file);
        let route = RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/test-model".into()),
            ..Default::default()
        };
        store.publish_terminal(
            id,
            running.revision,
            &TerminalPublication {
                outcome: provenant_run::Outcome::Succeeded,
                started_at: running.started_at,
                finished_at: clock.wall_now()?,
                provider_ms: Some(1),
                reason: None,
                result: "landed writer",
                complete: true,
                redacted: false,
                resolved_route: Some(route.clone()),
                applied_route: Some(route.clone()),
                observed_route: Some(route),
                usage: Default::default(),
                session: None,
                partial_signals: Vec::new(),
            },
            &exit,
            &call,
        )?;
        let attempt_directory = Directory::open_anchor(&attempt)?;
        attempt_directory.mkdir(std::ffi::OsStr::new("quarantine"), 0o700)?;
        let archive =
            attempt_directory.open_verified_directory(std::ffi::OsStr::new("quarantine"))?;
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "scope": scope, "kind": "quarantine_manifest",
        }))?;
        archive
            .atomic_write(
                std::ffi::OsStr::new("manifest.json"),
                ".manifest-",
                &manifest,
                provenant_os::fs::PublishMode::NoReplace,
            )?
            .require_durable()?;
        let writer = provenant_run::prepare_writer(&request, &rig.state, context.project_id)?;
        let state = rig
            .state
            .join("worktrees")
            .join(context.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &landing.worktree,
            ));
        let binding = WriterBinding::from_verified(
            writer.worktree.repo().root_dir(),
            writer.worktree.repo().private_dir(),
            writer.branch.as_str(),
            provenant_core::GitOid::parse(&landing.head)?,
        )?;
        drop(provenant_supervisor::fixture::acquire_writer_with_binding(
            Directory::open_anchor(&state)?,
            scope,
            binding,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?);
        let reservation = state.join("reservation.json");
        let mut fence: serde_json::Value = serde_json::from_slice(&fs::read(&reservation)?)?;
        let metadata = Directory::open_anchor(&attempt)?.metadata()?;
        fence["launch_state"] = "never_spawned".into();
        fence["unresolved_cleanup"] = false.into();
        fence["census_incomplete"] = false.into();
        fence["attempt_device"] = metadata.device.into();
        fence["attempt_inode"] = metadata.inode.into();
        fs::write(&reservation, serde_json::to_vec(&fence)?)?;
        assert!(
            crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?
                .get(&landing.worktree)
                == Some(&true)
        );
        let value = land(&landing)?;
        assert_eq!(
            status(&value, "worktree")?,
            "done",
            "child={child}: {value}"
        );
        assert_eq!(status(&value, "branch")?, "done", "child={child}: {value}");
        assert!(!landing.worktree.exists());
        assert!(!branch_exists(rig, "feat")?);
        assert!(!reservation.exists());
        assert!(state.join("settlement.json").is_file());
        assert!(attempt.join("release.json").is_file());
        assert_eq!(
            fs::read(attempt.join("quarantine/manifest.json"))?,
            manifest
        );
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
    }
    Ok(())
}

pub(crate) fn pr_land_primary_with_separate_git_directory() -> Result {
    for from_lane in [false, true] {
        let mut landing = landing_with_git_dir(true)?;
        let main = landing.rig.project.clone();
        landing
            .rig
            .env
            .push(("FIXTURE_GH_DIR".into(), main.clone().into_os_string()));
        if from_lane {
            landing.rig.project = landing.worktree.clone();
        }
        let value = land(&landing)?;
        assert_eq!(status(&value, "worktree")?, "done", "{value}");
        assert_eq!(status(&value, "branch")?, "done", "{value}");
        assert!(!landing.worktree.exists());
        landing.rig.project = main;
        assert!(!branch_exists(&landing.rig, "feat")?);
    }
    Ok(())
}

pub(crate) fn pr_land_out_of_tree_with_separate_git_directory() -> Result {
    for configured in [true, false] {
        let mut landing = landing_with_git_dir(true)?;
        let main = landing.rig.project.clone();
        let lane = landing.rig.root.join("outside-feat");
        git(
            &landing.rig,
            &[
                "worktree",
                "move",
                landing.worktree.to_str().ok_or("path")?,
                lane.to_str().ok_or("path")?,
            ],
        )?;
        let adopted = command(
            &landing.rig,
            &["worktree", "adopt", lane.to_str().ok_or("path")?, "--json"],
        )?;
        assert_eq!(
            code(&adopted),
            0,
            "{}",
            String::from_utf8_lossy(&adopted.stdout.bytes)
        );
        if configured {
            git(
                &landing.rig,
                &["config", "core.worktree", main.to_str().ok_or("path")?],
            )?;
        }
        landing
            .rig
            .env
            .push(("FIXTURE_GH_DIR".into(), main.clone().into_os_string()));
        let output = command(
            &landing.rig,
            &[
                "--project",
                lane.to_str().ok_or("path")?,
                "land",
                NUMBER,
                "--json",
            ],
        )?;
        let value = json(&output)?;
        assert_eq!(value["data"]["merged"], true, "{value}");
        if configured {
            assert_eq!(code(&output), 0, "{value}");
            assert_eq!(status(&value, "worktree")?, "done", "{value}");
            assert_eq!(status(&value, "branch")?, "done", "{value}");
            assert!(!lane.exists());
            assert!(!branch_exists(&landing.rig, "feat")?);
        } else {
            assert_eq!(code(&output), 0, "{value}");
            assert_eq!(value["data"]["cleanup_pending"], true);
            assert!(value.to_string().contains("worktree inventory"), "{value}");
            assert!(lane.exists());
            assert!(branch_exists(&landing.rig, "feat")?);
        }
    }
    Ok(())
}

pub(crate) fn pr_land_refuses_each_unsafe_step_and_reports_the_rest() -> Result {
    // A checkout of the base with a tracked modification is not fast-forwarded.
    let landing = landing()?;
    let ignore = landing.rig.project.join(".gitignore");
    let mut contents = fs::read_to_string(&ignore)?;
    contents.push_str("mine\n");
    fs::write(&ignore, &contents)?;
    let output = command(
        &landing.rig,
        &["land", NUMBER, "--head", &landing.head, "--json"],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(value["data"]["cleanup_pending"], true, "{value}");
    assert_eq!(value["data"]["merged"], true);
    assert_eq!(status(&value, "fast_forward")?, "refused");
    assert_eq!(status(&value, "worktree")?, "done");
    assert_eq!(
        git(&landing.rig, &["rev-parse", "main"])?,
        landing.stale,
        "a dirty checkout keeps its base"
    );
    assert_eq!(fs::read_to_string(&ignore)?, contents);

    // Untracked files in the base checkout, such as Provenant's own scratch,
    // do not stop the fast-forward and are left in place.
    let landing = self::landing()?;
    fs::create_dir_all(landing.rig.project.join(".provenant/scratch"))?;
    fs::write(
        landing.rig.project.join(".provenant/scratch/note"),
        "mine\n",
    )?;
    fs::write(landing.rig.project.join("stray.txt"), "mine\n")?;
    let output = command(
        &landing.rig,
        &["land", NUMBER, "--head", &landing.head, "--json"],
    )?;
    let value = json(&output)?;
    assert_eq!(status(&value, "fast_forward")?, "done", "{value}");
    assert_ne!(
        git(&landing.rig, &["rev-parse", "main"])?,
        landing.stale,
        "{value}"
    );
    assert!(landing.rig.project.join("stray.txt").exists());
    assert!(landing.rig.project.join(".provenant/scratch/note").exists());

    // A worktree with uncommitted work is kept, and so is its branch.
    let mut landing = self::landing()?;
    fs::write(landing.worktree.join("notes.txt"), "unsaved\n")?;
    fs::create_dir_all(landing.worktree.join("target"))?;
    fs::write(landing.worktree.join("target/artifact"), "built")?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let output = command(
        &landing.rig,
        &[
            "--project",
            landing.worktree.to_str().ok_or("path")?,
            "land",
            NUMBER,
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(value["data"]["cleanup_pending"], true, "{value}");
    assert_eq!(status(&value, "worktree")?, "refused", "{value}");
    assert_eq!(status(&value, "branch")?, "refused", "{value}");
    assert!(landing.worktree.join("notes.txt").exists());
    assert!(
        landing.worktree.join("target/artifact").exists(),
        "a dirty worktree keeps its build output"
    );
    assert!(branch_exists(&landing.rig, "feat")?);

    // Commits the pull request never merged keep the branch.
    let mut landing = self::landing()?;
    fs::write(landing.worktree.join("later.txt"), "later\n")?;
    git_in(&landing.rig, &landing.worktree, &["add", "later.txt"])?;
    git_in(&landing.rig, &landing.worktree, &["commit", "-qm", "later"])?;
    landing.rig.env.push((
        "FIXTURE_GH_DIR".into(),
        landing.rig.project.clone().into_os_string(),
    ));
    let output = command(
        &landing.rig,
        &[
            "--project",
            landing.worktree.to_str().ok_or("path")?,
            "land",
            NUMBER,
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(value["data"]["cleanup_pending"], true, "{value}");
    assert_eq!(status(&value, "worktree")?, "refused", "{value}");
    assert_eq!(status(&value, "branch")?, "refused", "{value}");
    assert!(
        landing.worktree.join("later.txt").exists(),
        "the checkout holding unmerged commits survives"
    );
    assert!(branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_leaves_a_local_namesake_of_a_fork_branch_alone() -> Result {
    // The merged pull request came from a fork's `feat`, so the local `feat`
    // branch and its clean, fully merged worktree are somebody else's.
    let landing = landing()?;
    fs::write(
        landing.rig.project.join("view.json"),
        r#"{"state":"MERGED","baseRefName":"main","headRefName":"feat","isCrossRepository":true}"#,
    )?;
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], true, "{value}");
    assert_eq!(status(&value, "fast_forward")?, "done", "{value}");
    assert_eq!(status(&value, "worktree")?, "skipped", "{value}");
    assert_eq!(status(&value, "branch")?, "skipped", "{value}");
    assert!(
        value.to_string().contains("another repository"),
        "the report says why: {value}"
    );
    assert!(landing.worktree.exists());
    assert!(branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_leaves_a_reused_branch_name_alone() -> Result {
    // Someone recreated `feat` at the base after the pull request's `feat` was
    // merged. The base contains that tip, but it is not the merged head, so the
    // branch and its clean checkout are not the pull request's to remove.
    let landing = landing()?;
    git_in(
        &landing.rig,
        &landing.worktree,
        &["reset", "-q", "--hard", "origin/main"],
    )?;
    let reused = git(&landing.rig, &["rev-parse", "feat"])?;
    assert_ne!(reused, landing.head);
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], true, "{value}");
    assert_eq!(status(&value, "fast_forward")?, "done", "{value}");
    assert_eq!(status(&value, "worktree")?, "skipped", "{value}");
    assert_eq!(status(&value, "branch")?, "skipped", "{value}");
    assert!(
        value
            .to_string()
            .contains("not the merged pull request head"),
        "the report says why: {value}"
    );
    assert!(landing.worktree.exists());
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, reused);
    Ok(())
}

pub(crate) fn pr_land_deletes_an_unregistered_branch_only_at_the_merged_head() -> Result {
    // No worktree is registered for `feat`. After the cleanup check passes,
    // another process moves `feat` to the base, which also contains it. The
    // deletion is pinned to the merged head, so it leaves that branch alone.
    let landing = landing()?;
    unregister(&landing)?;
    let moved = git(&landing.rig, &["rev-parse", "origin/main"])?;
    let (status, detail) = race(&landing, &["branch", "-f", "feat", "origin/main"])?;
    assert_eq!(status, "refused", "{detail}");
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, moved);

    // Undisturbed, the same branch at the merged head is deleted.
    let landing = self::landing()?;
    unregister(&landing)?;
    let (status, detail) = race(&landing, &[])?;
    assert_eq!(status, "done", "{detail}");
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_keeps_a_branch_held_by_a_paused_rebase() -> Result {
    // The rebase leaves its worktree on a detached HEAD, so only the rebase
    // state records that `feat` is still in use.
    let landing = landing()?;
    let held = hold(&landing, "rebase")?;
    assert_kept(&landing, race(&landing, &[])?, "rebased")?;

    // Once the rebase is aborted nothing holds the branch, so it is deleted.
    git_in(&landing.rig, &held, &["rebase", "--abort"])?;
    git_in(&landing.rig, &held, &["checkout", "-q", "--detach"])?;
    assert_deleted(&landing)
}

pub(crate) fn pr_land_keeps_a_branch_held_by_a_bisect() -> Result {
    let landing = landing()?;
    let held = hold(&landing, "bisect")?;
    assert_kept(&landing, race(&landing, &[])?, "bisected")?;

    git_in(&landing.rig, &held, &["bisect", "reset"])?;
    git_in(&landing.rig, &held, &["checkout", "-q", "--detach"])?;
    assert_deleted(&landing)
}

pub(crate) fn pr_land_keeps_a_branch_held_in_a_checkout_that_went_away() -> Result {
    // The paused rebase lives in the worktree's administrative directory, so
    // losing the checkout directory does not release the branch.
    let landing = landing()?;
    let held = hold(&landing, "rebase")?;
    fs::rename(&held, landing.rig.root.join("moved-away"))?;
    assert_kept(&landing, race(&landing, &[])?, "rebased")
}

pub(crate) fn pr_land_refuses_when_a_worktree_entry_cannot_be_read() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let landing = landing()?;
    hold(&landing, "rebase")?;
    let entry = landing.rig.project.join(".git/worktrees/held");
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o000))?;
    let outcome = race(&landing, &[]);
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o755))?;
    assert_kept(&landing, outcome?, "cannot read")
}

pub(crate) fn pr_land_refuses_when_worktree_state_is_malformed() -> Result {
    // A truncated rebase record or an unparseable HEAD must refuse, not read
    // as a worktree that holds nothing.
    let landing = landing()?;
    hold(&landing, "rebase")?;
    let entry = landing.rig.project.join(".git/worktrees/held");
    let head_name = ["rebase-merge", "rebase-apply"]
        .iter()
        .map(|dir| entry.join(dir).join("head-name"))
        .find(|path| path.is_file())
        .ok_or("no rebase head-name")?;
    let original = fs::read(&head_name)?;
    for malformed in ["", "refs/\n"] {
        fs::write(&head_name, malformed)?;
        assert_kept(&landing, race(&landing, &[])?, "cannot parse")?;
    }
    fs::write(&head_name, original)?;

    let head = entry.join("HEAD");
    let original = fs::read(&head)?;
    fs::write(&head, "abc\n")?;
    let outcome = race(&landing, &[]);
    fs::write(&head, original)?;
    assert_kept(&landing, outcome?, "cannot parse HEAD")?;

    let bisect = self::landing()?;
    hold(&bisect, "bisect")?;
    let start = bisect.rig.project.join(".git/BISECT_START");
    let start = if start.is_file() {
        start
    } else {
        bisect.rig.project.join(".git/worktrees/held/BISECT_START")
    };
    fs::write(&start, ":\n")?;
    assert_kept(&bisect, race(&bisect, &[])?, "cannot parse BISECT_START")
}

/// The `branch` step refused, naming `reason`, and `feat` is still at the
/// merged head.
fn assert_kept(landing: &Landing, (status, detail): (String, String), reason: &str) -> Result {
    assert_eq!(status, "refused", "{detail}");
    assert!(detail.contains(reason), "{detail}");
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, landing.head);
    Ok(())
}

/// With nothing holding `feat`, the cleanup deletes it.
fn assert_deleted(landing: &Landing) -> Result {
    let (status, detail) = race(landing, &[])?;
    assert_eq!(status, "done", "{detail}");
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

/// A second worktree that has `feat` mid-`operation` on a detached HEAD, with
/// the registered `feat` worktree dropped.
fn hold(landing: &Landing, operation: &str) -> Result<PathBuf> {
    unregister(landing)?;
    let held = landing.rig.root.join("held");
    git(
        &landing.rig,
        &[
            "worktree",
            "add",
            "-q",
            held.to_str().ok_or("path")?,
            "feat",
        ],
    )?;
    if operation == "rebase" {
        git_in(
            &landing.rig,
            &held,
            &[
                "-c",
                "sequence.editor=sed -i.bak s/^pick/edit/",
                "rebase",
                "-q",
                "-i",
                "HEAD~1",
            ],
        )?;
    } else {
        let first = git_in(
            &landing.rig,
            &held,
            &["rev-list", "--max-parents=0", "HEAD"],
        )?;
        git_in(&landing.rig, &held, &["bisect", "start", "HEAD", &first])?;
    }
    let listing = git(&landing.rig, &["worktree", "list", "--porcelain"])?;
    assert!(
        !listing.contains("branch refs/heads/feat"),
        "the worktree must be detached: {listing}"
    );
    Ok(held)
}

/// The `branch` step when `between` runs after the cleanup check.
fn race(landing: &Landing, between: &[&str]) -> Result<(String, String)> {
    let mut argv = vec!["--land-race", landing.head.as_str()];
    argv.extend_from_slice(between);
    let argv: Vec<_> = argv.into_iter().map(std::ffi::OsString::from).collect();
    let output = landing
        .rig
        .execute_os(&std::env::current_exe()?, &argv, None)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let text = String::from_utf8(output.stdout.bytes)?;
    let (status, detail) = text.split_once('\n').ok_or("race output")?;
    Ok((status.to_owned(), detail.trim().to_owned()))
}

/// Drop the `feat` worktree and keep its branch.
fn unregister(landing: &Landing) -> Result {
    let path = landing.worktree.to_str().ok_or("path")?;
    git(&landing.rig, &["worktree", "remove", "--force", path])?;
    Ok(())
}

/// The message of a refused `pr open`, which is a usage error.
fn open_refusal(rig: &TestRig, args: &[&str]) -> Result<String> {
    let mut argv = vec!["pr", "open"];
    argv.extend_from_slice(args);
    argv.push("--json");
    let output = command(rig, &argv)?;
    assert_eq!(
        code(&output),
        2,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let value = json(&output)?;
    Ok(value["error"]["message"]
        .as_str()
        .ok_or("error message")?
        .to_owned())
}

pub(crate) fn pr_open_refuses_what_the_receipts_cannot_support() -> Result {
    let rig = repository()?;
    let summary = rig.root.join("summary.md");
    let summary_arg = summary.to_str().ok_or("path")?;
    fs::write(
        &summary,
        "# feat(cli): add the thing\n\nIt adds the thing.\n",
    )?;
    let unknown = "ab".repeat(16);
    let args = [
        "--from-attempt",
        &unknown,
        "--summary-file",
        summary_arg,
        "--dry-run",
    ];
    // No template, then an empty summary, then an attempt nobody ran.
    assert!(open_refusal(&rig, &args)?.contains("pull request template"));
    fs::create_dir_all(rig.project.join(".github"))?;
    fs::write(
        rig.project.join(".github/pull_request_template.md"),
        include_str!("../../../.github/pull_request_template.md"),
    )?;
    fs::write(&summary, "\n")?;
    assert!(open_refusal(&rig, &args)?.contains("summary file is empty"));
    fs::write(
        &summary,
        "# feat(cli): add the thing\n\nIt adds the thing.\n",
    )?;
    assert!(!open_refusal(&rig, &args)?.is_empty());

    // A lane that never ran cannot back a pull request.
    let dispatched = rig.execute_os(
        &std::env::current_exe()?,
        &[
            "--state-dir".into(),
            rig.state.clone().into_os_string(),
            "dispatch".into(),
            "--model".into(),
            "codex/gpt-6-luna".into(),
            "--operation-id".into(),
            "pr-open-fixture".into(),
            "--prompt-stdin".into(),
            "--json".into(),
        ],
        Some(b"never launched"),
    )?;
    let attempt = json(&dispatched)?["data"]["lanes"][0]["attempt_id"]
        .as_str()
        .ok_or("attempt")?
        .to_owned();
    let message = open_refusal(
        &rig,
        &[
            "--from-attempt",
            &attempt,
            "--summary-file",
            summary_arg,
            "--dry-run",
        ],
    )?;
    assert!(
        message.contains("only a succeeded or review_required attempt"),
        "{message}"
    );
    // A malformed attempt ID is a usage error before anything is read.
    let invalid = command(
        &rig,
        &[
            "pr",
            "open",
            "--from-attempt",
            "nope",
            "--summary-file",
            summary_arg,
        ],
    )?;
    assert_eq!(code(&invalid), 2);
    Ok(())
}

pub(crate) fn pr_land_changes_nothing_when_the_merge_does_not_happen() -> Result {
    for flag in ["checks-fail", "moved"] {
        let landing = landing()?;
        let rig = &landing.rig;
        let mut view: serde_json::Value =
            serde_json::from_slice(&fs::read(rig.project.join("view.json"))?)?;
        view["state"] = "OPEN".into();
        fs::write(rig.project.join("view.json"), serde_json::to_vec(&view)?)?;
        if flag == "moved" {
            fs::write(
                rig.project.join("head.json"),
                r#"{"headRefOid":"2222222222222222222222222222222222222222"}"#,
            )?;
        } else {
            fs::write(rig.project.join(flag), "")?;
        }
        let output = command(rig, &["land", NUMBER, "--head", &landing.head, "--json"])?;
        assert_ne!(code(&output), 0, "{flag}");
        let value = json(&output)?;
        assert_eq!(value["data"]["merged"], false, "{value}");
        assert!(!rig.project.join("merge-args").exists(), "{flag}");
        assert!(landing.worktree.exists(), "{flag}");
        assert!(branch_exists(rig, "feat")?, "{flag}");
        assert_eq!(git(rig, &["rev-parse", "main"])?, landing.stale, "{flag}");
    }
    // The current head is the default, and a pending merge cleans nothing.
    let landing = landing()?;
    fs::write(
        landing.rig.project.join("view.json"),
        r#"{"state":"OPEN","baseRefName":"main","headRefName":"feat","isCrossRepository":false}"#,
    )?;
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], false);
    assert!(landing.worktree.exists());
    Ok(())
}
