//! Real dispatch composition with isolated, synthetic provider processes.
mod support;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::ExitCode,
};
use support::{Result, TestRig, code};

fn fixture() -> Result {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|v| v == "--version") {
        writeln!(io::stdout(), "codex-cli 0.159.0")?;
        return Ok(());
    }
    if args.get(1).is_some_and(|v| v == "auth") {
        writeln!(
            io::stdout(),
            "{}",
            json!({"loggedIn":true,"email":"fixture@example.test"})
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|v| v == "-p") {
        let dir = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").ok_or("CLAUDE_CONFIG_DIR")?);
        assert_eq!(
            dir.file_name().and_then(|s| s.to_str()),
            Some(".claude-pro")
        );
        if args.iter().any(|v| v == "provenant-refresh-probe") {
            let expiry =
                u64::try_from(jiff::Timestamp::now().as_millisecond())?.saturating_add(3_600_000);
            fs::write(
                dir.join(".credentials.json"),
                serde_json::to_vec(
                    &json!({"claudeAiOauth":{"accessToken":"fixture-refreshed-token","expiresAt":expiry}}),
                )?,
            )?;
            fs::write(dir.join("refreshed"), b"yes")?;
            return Ok(());
        }
        assert_eq!(
            std::env::var("CLAUDE_CODE_OAUTH_TOKEN")?,
            "fixture-refreshed-token"
        );
        for frame in [
            json!({"type":"system","subtype":"init","session_id":"fixture-session"}),
            json!({"type":"assistant","session_id":"fixture-session","parent_tool_use_id":null,"message":{"model":"claude-sonnet-5","role":"assistant","content":[{"type":"text","text":"OK"}],"usage":{"input_tokens":1,"output_tokens":1}}}),
            json!({"type":"result","session_id":"fixture-session","subtype":"success","is_error":false}),
        ] {
            writeln!(io::stdout(), "{frame}")?;
        }
        return Ok(());
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt)?;
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("CODEX_HOME")?);
    let auth: Value = serde_json::from_slice(&fs::read(home.join("auth.json"))?)?;
    assert_eq!(auth["tokens"]["account_id"], "fixture-one");
    let session = "01234567-89ab-cdef-0123-456789abcdef";
    let sessions = home.join("sessions/2026/10/24");
    fs::create_dir_all(&sessions)?;
    let recorded_session = if prompt.contains("mismatch") {
        "other-thread"
    } else {
        session
    };
    if !prompt.contains("missing") {
        fs::write(
            sessions.join(format!("rollout-2026-10-24T00-00-00-{session}.jsonl")),
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":recorded_session}}),
                json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
                json!({"type":"response_item","payload":{"content":"must not be retained"}})
            ),
        )?;
    }
    for frame in [
        json!({"type":"thread.started","thread_id":session}),
        json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
    ] {
        writeln!(io::stdout(), "{frame}")?;
    }
    Ok(())
}
fn codex_dispatch_observes_rollout() -> Result {
    for (index, prompt) in ["reply OK", "missing", "mismatch"].iter().enumerate() {
        let mut rig = TestRig::new()?;
        // The production read-only profile anchors the host Rust runtime roots.
        for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
            fs::create_dir_all(rig.home.join(path))?;
        }
        fs::write(rig.home.join(".rustup/settings.toml"), "")?;
        let bin = rig.root.join("provider/bin");
        fs::create_dir_all(&bin)?;
        fs::copy(std::env::current_exe()?, bin.join("codex"))?;
        let account = rig.root.join("codex");
        fs::write(
            account.join("auth.json"),
            br#"{"tokens":{"account_id":"fixture-one"}}"#,
        )?;
        rig.env.retain(|(k, _)| k != "PATH");
        rig.env.push((
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ));
        let prompt_file = rig.root.join("prompt.txt");
        fs::write(&prompt_file, prompt)?;
        let op = format!("{:032x}", index + 1);
        let out = rig.command(&[
            "dispatch",
            if index == 0 { "--route" } else { "--model" },
            if index == 0 {
                "utility"
            } else {
                "codex/gpt-6-luna"
            },
            "--effort",
            "low",
            "--operation-id",
            &op,
            "--prompt-file",
            prompt_file.to_str().ok_or("prompt")?,
            "--wait",
            "5",
            "--json",
        ])?;
        assert_eq!(
            code(&out),
            0,
            "{} {}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        let lanes = rig.command(&["lanes", "--all", "--json"])?;
        let value: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let rows = value["data"]["lanes"]
            .as_array()
            .ok_or_else(|| format!("lanes: {value}"))?;
        let lane = rows.first().ok_or_else(|| format!("lane: {value}"))?;
        if !cfg!(target_os = "macos") {
            assert_eq!(lane["state"], "rejected", "{lane}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("UnsupportedPlatform")),
                "{lane}"
            );
            return rig.assert_legacy_unchanged();
        }
        if index == 0 {
            assert_eq!(lane["state"], "succeeded", "{lane}");
            assert_eq!(lane["observed_route"]["model"], "codex/gpt-6-luna");
            assert_eq!(lane["observed_route"]["source"], "codex_rollout");
            assert!(lane["applied_route"]["account_label"].as_str().is_some());
        } else {
            assert_eq!(lane["state"], "failed", "{lane}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("provider identity was not observed: ")),
                "{lane}"
            );
        }
        rig.assert_legacy_unchanged()?;
    }
    Ok(())
}
fn claude_dispatch_refreshes_registered_account() -> Result {
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    fs::copy(std::env::current_exe()?, bin.join("claude"))?;
    let account = rig.home.join(".claude-pro");
    fs::create_dir(&account)?;
    fs::write(
        account.join(".credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"fixture-expired-token","expiresAt":1}}"#,
    )?;
    rig.env.retain(|(k, _)| k != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let registered = rig.command(&[
        "accounts",
        "register",
        "pro",
        "--adapter",
        "claude",
        "--directory",
        account.to_str().ok_or("account path")?,
        "--json",
    ])?;
    assert_eq!(
        code(&registered),
        0,
        "{}",
        String::from_utf8_lossy(&registered.stdout.bytes)
    );
    let listed = rig.command(&["accounts", "list", "--json"])?;
    let listed: Value = serde_json::from_slice(&listed.stdout.bytes)?;
    assert_eq!(
        listed["data"]["entries"]["claude-pro"]["label"],
        "pro",
        "{} config={}",
        listed,
        fs::read_to_string(rig.state.join("config.toml"))?
    );
    let manifest = rig.root.join("manifest.json");
    fs::write(
        &manifest,
        serde_json::to_vec(
            &json!({"schema_version":1,"tasks":[{"prompt":"reply OK","model":"claude/claude-sonnet-5","effort":"low","account":"pro"}]}),
        )?,
    )?;
    let out = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest")?,
        "--operation-id",
        "fixture-claude",
        "--wait",
        "5",
        "--json",
    ])?;
    assert_eq!(
        code(&out),
        0,
        "{} {}",
        String::from_utf8_lossy(&out.stdout.bytes),
        String::from_utf8_lossy(&out.stderr.bytes)
    );
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lane = &lanes["data"]["lanes"][0];
    if !cfg!(target_os = "macos") {
        assert_eq!(lane["state"], "rejected", "{lane}");
        assert!(
            lane["reason"]
                .as_str()
                .is_some_and(|r| r.contains("UnsupportedPlatform")),
            "{lane}"
        );
        assert!(account.join("refreshed").exists());
        return rig.assert_legacy_unchanged();
    }
    assert_eq!(lane["state"], "succeeded", "{lane}");
    assert_eq!(lane["account"], "pro");
    assert_eq!(lane["observed_route"]["model"], "claude/claude-sonnet-5");
    assert!(account.join("refreshed").exists());
    let attempt = rig
        .state
        .join("runs")
        .join(lane["run_id"].as_str().ok_or("run")?)
        .join("attempts")
        .join(lane["attempt_id"].as_str().ok_or("attempt")?);
    let authority: Value =
        serde_json::from_slice(&fs::read(attempt.join("credential-authority.json"))?)?;
    assert_eq!(authority["actor"], lane["run_id"]);
    assert_eq!(
        authority["source"]["path"],
        account.join(".credentials.json").to_str().ok_or("source")?
    );
    assert_eq!(authority["purpose"], "lane dispatch");
    for name in [
        "launch.json",
        "attempt.json",
        "stdout.log",
        "stderr.log",
        "credential-authority.json",
    ] {
        if let Ok(bytes) = fs::read(attempt.join(name)) {
            assert!(!String::from_utf8_lossy(&bytes).contains("fixture-refreshed-token"));
        }
    }
    rig.assert_legacy_unchanged()
}

fn opencode_free_pool_cools_second_model() -> Result {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, ProjectId, RootInputs, RunId,
        TaskId, WaitBudget,
    };
    use provenant_records::{
        CanonicalV1, ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
        run::{CanonicalRequest, HarnessProfile, RequestedRoute, RouteRecord, TaskRequest},
    };
    use provenant_run::{ExecutionContext, NewAttempt, PreparedRun, Request, RunStore};
    use provenant_supervisor::{
        clock::SystemClock,
        custody::{CaptureTotals, ExitObservation, PublicationState, Scope, SettlementEvidence},
        exec::Termination,
        fs::{Directory, LocalFs},
    };

    let rig = TestRig::new()?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&LocalFs)?;
    let project_id = ProjectId::from_bytes([21; 16]);
    let actor_id = ActorId::from_bytes([22; 16]);
    let execution = ExecutionContext {
        state_root: roots.state.clone(),
        project_id,
        actor_id,
        read_prompts: true,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 10)?,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let run_id = RunId::from_bytes([23; 16]);
    let attempt_id = AttemptId::from_bytes([24; 16]);
    let task_id = TaskId::from_bytes([25; 16]);
    let applied = RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode/deepseek-v4.1-flash-free".into()),
        effort: None,
        account_label: Some("open-code-free".into()),
        account_key: Some("keyless-free".into()),
        source: Some("fixture_route".into()),
        pool_selection: None,
    };
    let prompt = "reply OK";
    let prepared = PreparedRun {
        run_id,
        operation_id: OperationId::parse("opencode-cooldown")?,
        request: CanonicalRequest {
            schema_version: 1,
            tasks: vec![TaskRequest {
                profile: HarnessProfile::default(),
                task_id,
                prompt_digest: CanonicalV1::file_digest(prompt.as_bytes()),
                route: RequestedRoute::Model {
                    provider: "opencode".into(),
                    model: "opencode/opencode/deepseek-v4.1-flash-free".into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
                authority_digest: Digest::from_bytes([26; 32]),
                writer: None,
                provider_timeout_seconds: 60,
                delivery_id: None,
            }],
            concurrency: 1,
            parent: None,
            lane_depth: None,
            parent_attempt: None,
        },
        attempts: vec![NewAttempt {
            task_id,
            attempt_id,
            prompt: prompt.into(),
        }],
        created_at: clock.wall_now()?,
    };
    store.create(&prepared, &call)?;
    let started_at = clock.wall_now()?;
    let mut running = store.record(attempt_id, &call)?;
    running.revision = 2;
    running.resolved_route = Some(applied.clone());
    running.applied_route = Some(applied.clone());
    running.started_at = Some(started_at);
    running.provider_launched = Some(true);
    running.state = provenant_run::AttemptState::Running;
    let running_bytes = CanonicalV1::bytes(&running)?.as_bytes().to_vec();
    let database = rusqlite::Connection::open(roots.state.as_path().join("run.sqlite3"))?;
    database.execute(
        "UPDATE attempts SET revision=?1,record=?2 WHERE id=?3",
        rusqlite::params![2_i64, running_bytes, attempt_id.to_string()],
    )?;

    let attempt_dir = roots
        .state
        .as_path()
        .join("runs")
        .join(run_id.to_string())
        .join("attempts")
        .join(attempt_id.to_string());
    let provider_state = attempt_dir.join("provider-state");
    fs::create_dir_all(&provider_state)?;
    let evidence = if cfg!(target_os = "macos") {
        SettlementEvidence::DarwinCensusAndCustodyLease
    } else {
        SettlementEvidence::LinuxKernelChildren
    };
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: run_id,
            attempt: attempt_id,
            generation: 0,
        },
        termination: Some(Termination::Exit(1)),
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
        settlement_evidence: evidence,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    };
    let mut sealed_exit = serde_json::to_value(&exit)?;
    let fields = sealed_exit.as_object_mut().ok_or("exit fixture")?;
    fields.remove("publication");
    fields.insert("schema_version".into(), 1.into());
    let exit_bytes = serde_json::to_vec(&sealed_exit)?;
    let mut exit_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("process-exit.json"))?;
    exit_file.write_all(&exit_bytes)?;
    exit_file.sync_all()?;

    // Fixture provider output enters Registry.decode through RunStore::finish.
    let stdout = br#"{"type":"error","error":{"type":"provider.quota","message":"free tier exhausted","response":{"body":{"error":{"type":"FreeUsageLimitError","message":"fixture free allowance exhausted"}}}}}"#;
    let mut stdout_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("stdout.log"))?;
    stdout_file.write_all(stdout)?;
    stdout_file.sync_all()?;
    let attempt = Directory::open_anchor(&attempt_dir)?;
    let terminal = store.finish(
        attempt_id,
        &attempt,
        &exit,
        &provenant_cli::run::Registry::default(),
        &call,
    )?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::QuotaLimited,
            ..
        }
    ));
    let account_path = roots
        .state
        .as_path()
        .join("quota/keyless-free/opencode.json");
    let quota_record = provenant_quota::AccountRecord::decode(&fs::read(account_path)?)?;
    assert_eq!(quota_record.refusals.len(), 1);
    assert_eq!(
        quota_record.refusals[0].pool.as_deref(),
        Some("opencode-free")
    );
    let now = quota_record.refusals[0].observed_unix_seconds;

    let second = ResolvedRoute {
        adapter: "opencode".into(),
        model: "opencode/glm-5.3-flash-free".into(),
        effort: None,
        catalogue_revision: "fixture-catalogue".into(),
        account_key: Some("keyless-free".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            availability_ref: "fixture-preflight".into(),
            expires_unix_seconds: now + 300,
        },
        confidentiality: RouteConfidentiality {
            evidence_ref: "fixture-access".into(),
        },
        pool_selection: None,
    };
    let pools = vec!["opencode-free".to_owned()];
    assert!(matches!(
        provenant_quota::Admission {
            now_unix_seconds: now,
            records: std::slice::from_ref(&quota_record),
            refusals: &[],
            pools: &pools,
        }
        .check(&second),
        Err(provenant_quota::QuotaError::Refused(
            "free tier exhausted; use a paid route or wait for its allowance reset"
        ))
    ));
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args
        .first()
        .is_some_and(|s| s == "exec" || s == "--version" || s == "auth" || s == "-p")
    {
        fixture()
    } else {
        provenant_supervisor::fixture::run_libtest(
            &args,
            &[
                "codex_dispatch_observes_rollout",
                "claude_dispatch_refreshes_registered_account",
                "opencode_free_pool_cools_second_model",
            ],
            |name| {
                let result = match name {
                    "codex_dispatch_observes_rollout" => codex_dispatch_observes_rollout(),
                    "claude_dispatch_refreshes_registered_account" => {
                        claude_dispatch_refreshes_registered_account()
                    }
                    _ => opencode_free_pool_cools_second_model(),
                };
                result.map_err(|e| io::Error::other(e.to_string()))
            },
        )
        .map_err(Into::into)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "dispatch fixture: {error}");
            ExitCode::FAILURE
        }
    }
}
