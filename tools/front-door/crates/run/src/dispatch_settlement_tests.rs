use super::*;

#[test]
fn budget_completion_never_overrides_unsettled_provider_evidence() -> TestResult {
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    use provenant_os::exec::Termination;
    for (stop, clean) in [
        (None, false),
        (
            Some(provenant_supervisor::machine::StopReason::TokenBudget),
            false,
        ),
        (
            Some(provenant_supervisor::machine::StopReason::TokenBudget),
            true,
        ),
        (
            Some(provenant_supervisor::machine::StopReason::WallDeadline),
            false,
        ),
    ] {
        for (termination, eof_fragment) in [
            (Termination::Exit(0), false),
            (Termination::Exit(1), false),
            (Termination::Signal(9), false),
            (Termination::Exit(0), true),
        ] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let run = input();
            let id = run.attempt_id;
            store.create(&run, &call)?;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let dir = store.attempt_dir(id, false)?;
            artifacts::seal(&dir, "stdout.log", b"damaged capture", CAPTURE_LIMIT)?;
            if clean {
                artifacts::seal(
                    &dir,
                    provenant_records::AttemptFile::BudgetCleanPoint.name(),
                    b"{\"schema_version\":1,\"clean\":true}",
                    RECORD_LIMIT,
                )?;
            }
            let mut observed = exit();
            observed.scope = RunStore::scope(&store.record(id, &call)?);
            observed.stop_reason = stop;
            observed.termination = Some(termination);
            observed.provider_identity_eof_fragment = eof_fragment;
            seal_exit(&store, &observed)?;
            let fake = Fake {
            observed: Some(route()),
            outcome: Some(Outcome::Unsettled),
            terminal_cause: Some(CauseKind::ProviderCaptureDefect { line: 3 }),
            reason: Some("provider capture evidence defect at stdout.log line 3; replay incomplete".into()),
            complete: Some(false),
            // Even a done declaration cannot repair missing provider evidence.
            answer: Some("```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"finished\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```".into()),
            ..Fake::default()
        };
            let record = store.finish(id, &dir, &observed, &fake, &call)?;
            assert!(
                matches!(
                    record.state,
                    AttemptState::Terminal {
                        outcome: Outcome::Unsettled,
                        terminal_cause: Some(CauseKind::ProviderCaptureDefect { line: 3 }),
                        ..
                    }
                ),
                "stop={stop:?}, clean={clean}, termination={termination:?}, eof={eof_fragment}: {:?}",
                record.state
            );
            assert!(!record.result.ok_or("result")?.complete);
        }
    }
    Ok(())
}

// Spec: docs/specs/run-settlement.md#retention-and-disclosure
#[test]
fn holder_seals_raw_results_and_digests_the_stored_bytes() -> TestResult {
    for (answer, expected) in [
        ("plain answer", "plain answer"),
        (
            "credential ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "credential ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        ),
        (
            "unregistered sk-abcdefghijklmnopqrstuvwxyz password=\"printed-value\"",
            "unregistered sk-abcdefghijklmnopqrstuvwxyz password=\"printed-value\"",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let dir = store.attempt_dir(id, false)?;
        artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
        let mut observed = exit();
        observed.scope = RunStore::scope(&store.record(id, &call)?);
        seal_exit(&store, &observed)?;
        let fake = Fake {
            observed: Some(route()),
            answer: Some(answer.into()),
            ..Fake::default()
        };
        let record = store.finish(id, &dir, &observed, &fake, &call)?;
        let artifact = record.result.ok_or("result")?;
        assert_eq!(
            artifacts::read_sealed(&dir, "result.md", RESULT_LIMIT)?.as_deref(),
            Some(expected.as_bytes())
        );
        assert!(!artifact.redacted);
        assert_eq!(
            artifact.digest,
            CanonicalV1::file_digest(expected.as_bytes())
        );
        assert_eq!(artifact.bytes, expected.len() as u64);
        let capture = store.stream_result(id, &call, None)?;
        assert_eq!(capture.digest, artifact.digest.to_string());
        assert_eq!(capture.tail, expected);
    }
    Ok(())
}
// Spec: docs/specs/run-settlement.md#retention-and-disclosure
#[test]
fn settlement_preserves_provider_session_history() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let _scope = provenant_redact::publication_scope([token.as_bytes().to_vec()])?;
    let attempt = store
        .context
        .state_root
        .as_path()
        .join(format!("attempts/{id}"));
    let codex = attempt.join("provider-state/candidate-0/sessions/2026/10/24");
    std::fs::create_dir_all(&codex)?;
    let rollout = codex.join("rollout-2026-10-24T00-00-00-thread.jsonl");
    std::fs::write(
        &rollout,
        format!("{{\"type\":\"response_item\",\"payload\":{{\"content\":\"printed {token}\"}}}}\n"),
    )?;
    let claude = attempt.join("provider-state/candidate-0/claude-config/projects/key");
    std::fs::create_dir_all(&claude)?;
    let session = claude.join("session.jsonl");
    std::fs::write(&session, format!("{{\"message\":\"{token}\"}}\n"))?;
    let cursor = attempt.join("provider-state/candidate-0/.cursor/chats/project/session");
    let opencode = attempt.join("provider-state/candidate-0/data/opencode/log");
    for tree in [&cursor, &opencode] {
        std::fs::create_dir_all(tree)?;
        std::fs::write(tree.join("text.log"), format!("{token}\n"))?;
        // Long session text remains private continuation state as well.
        std::fs::write(
            tree.join("overlong.log"),
            format!("https://{}\n", "a".repeat(128 * 1024)),
        )?;
    }
    let binary = claude.join("session.db");
    std::fs::write(&binary, b"\0private\xff")?;
    let paths = [
        &rollout,
        &session,
        &cursor.join("text.log"),
        &opencode.join("text.log"),
        &binary,
    ];
    let before = paths
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        answer: Some(DONE_ANSWER.into()),
        ..Fake::default()
    };

    store.finish(id, &dir, &observed, &fake, &call)?;
    let record = store.record(id, &call)?;
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    for (path, expected) in paths.iter().zip(before) {
        assert_eq!(
            std::fs::read(path)?,
            expected,
            "session changed at {path:?}"
        );
    }
    Ok(())
}
#[test]
fn empty_success_is_rejected_at_settlement() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        answer: Some(String::new()),
        ..Fake::default()
    };
    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::Failed,
            ..
        }
    ));
    Ok(())
}
#[test]
fn failed_lane_reason_reports_path_denial_without_captured_text() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"", CAPTURE_LIMIT)?;
    artifacts::seal(
        &dir,
        "stderr.log",
        b"tool: /fixture/missing-grant: Permission denied TOKEN_SENTINEL\n",
        CAPTURE_LIMIT,
    )?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        outcome: Some(Outcome::Failed),
        failure_note: Some("TOKEN_SENTINEL /private/source".into()),
        ..Fake::default()
    };
    store.finish(id, &dir, &observed, &fake, &call)?;
    let reason = terminal_reason(&store, id, &call)?;
    assert!(!reason.contains("/private/source"), "{reason}");
    assert!(!reason.contains("/fixture/missing-grant"), "{reason}");
    assert!(!reason.contains("TOKEN_SENTINEL"), "{reason}");
    assert!(reason.contains("access grant"), "{reason}");
    Ok(())
}
#[derive(Clone, Copy)]
enum DenialStream {
    None,
    Stdout,
}

#[derive(Clone, Copy)]
enum DenialAnswer {
    Plain,
    CompletedHandoff,
    Empty,
}

#[test]
fn path_denial_keeps_the_answer_and_settlement_outcome() -> TestResult {
    for (stream, answer, expected) in [
        (
            DenialStream::Stdout,
            DenialAnswer::Plain,
            Outcome::Succeeded,
        ),
        (
            DenialStream::None,
            DenialAnswer::CompletedHandoff,
            Outcome::Succeeded,
        ),
        (DenialStream::Stdout, DenialAnswer::Empty, Outcome::Failed),
    ] {
        path_denial_case(stream, answer, expected)?;
    }
    Ok(())
}

fn path_denial_case(
    stream: DenialStream,
    answer_case: DenialAnswer,
    expected: Outcome,
) -> TestResult {
    const DENIAL: &[u8] = b"tool: /fixture/missing-grant: Permission denied TOKEN_SENTINEL\n";
    const COMPLETED: &str = "cat: /fixture/missing-grant: Operation not permitted\nI used the brief instead.\n\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```\n";
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    let captured = match stream {
        DenialStream::None => b"".as_slice(),
        DenialStream::Stdout => DENIAL,
    };
    artifacts::seal(&dir, "stdout.log", captured, CAPTURE_LIMIT)?;
    artifacts::seal(&dir, "stderr.log", b"", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let answer = match answer_case {
        DenialAnswer::Plain => "The tool failed",
        DenialAnswer::CompletedHandoff => COMPLETED,
        DenialAnswer::Empty => "",
    };
    let fake = Fake {
        observed: Some(route()),
        answer: Some(answer.into()),
        ..Fake::default()
    };
    let _record = store.finish(id, &dir, &observed, &fake, &call)?;
    assert_eq!(terminal(&store, id, &call)?, expected);
    let reason = terminal_reason(&store, id, &call)?;
    assert!(!reason.contains("/fixture/missing-grant"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    if matches!(stream, DenialStream::Stdout) {
        assert!(reason.contains("access grant"));
    }
    if matches!(answer_case, DenialAnswer::CompletedHandoff) {
        let result = artifacts::read_sealed(&dir, "result.md", RESULT_LIMIT)?.ok_or("result")?;
        assert_eq!(result, answer.as_bytes());
    }
    Ok(())
}
#[test]
fn quota_limited_empty_result_stays_quota_limited() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"", CAPTURE_LIMIT)?;
    // The quota evidence is only in the sealed stderr: the settlement must
    // read it and hand it to the decoder for the outcome to be QuotaLimited.
    artifacts::seal(&dir, "stderr.log", b"quota exhausted", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        quota_stderr: Some(b"quota exhausted"),
        answer: Some(String::new()),
        ..Fake::default()
    };
    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::QuotaLimited,
            ..
        }
    ));
    Ok(())
}
// Spec: docs/specs/run-settlement.md#retention-and-disclosure
#[test]
fn long_raw_result_settles_successfully() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let answer = format!("http://{}", "a".repeat(provenant_os::exec::FRAME_LIMIT + 1));
    let fake = Fake {
        observed: Some(route()),
        answer: Some(answer.clone()),
        ..Fake::default()
    };

    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    let AttemptState::Terminal {
        outcome, reason, ..
    } = &record.state
    else {
        return Err("not terminal".into());
    };
    assert_eq!(*outcome, Outcome::Succeeded);
    assert_eq!(reason.as_deref(), None);
    assert_eq!(
        artifacts::read_sealed(&dir, "result.md", RESULT_LIMIT)?.as_deref(),
        Some(answer.as_bytes())
    );
    assert!(!record.result.as_ref().ok_or("result")?.redacted);
    // A repeat settlement returns the same terminal record instead of looping.
    let repeat = store.finish(id, &dir, &observed, &fake, &call)?;
    assert_eq!(repeat.revision, record.revision);
    Ok(())
}
#[test]
fn a_busy_publication_lock_leaves_the_sealed_decision_to_replay() -> TestResult {
    provenant_sqlite::with_busy_timeout(std::time::Duration::from_millis(50), || -> TestResult {
        use provenant_os::fs::{FsError, LockMode};
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let dir = store.attempt_dir(id, false)?;
        artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
        let mut observed = exit();
        observed.scope = RunStore::scope(&store.record(id, &call)?);
        seal_exit(&store, &observed)?;
        let fake = Fake {
            observed: Some(route()),
            ..Fake::default()
        };
        let held = store.attempt_dir(run.attempt_id, false)?.try_lock(
            std::ffi::OsStr::new("publication.lock"),
            LockMode::Exclusive,
        )?;
        let error = store
            .finish(id, &dir, &observed, &fake, &call)
            .expect_err("a held publication lock cannot commit");
        assert!(
            matches!(error, RunError::Filesystem(FsError::LockTimeout { .. })),
            "{error:?}"
        );
        let decision = artifacts::read_sealed(&dir, "terminal-pending.json", RECORD_LIMIT)?
            .ok_or("decision")?;
        drop(held);
        let record = store.finish(id, &dir, &observed, &fake, &call)?;
        assert!(matches!(record.state, AttemptState::Terminal { .. }));
        assert_eq!(
            artifacts::read_sealed(&dir, "terminal-pending.json", RECORD_LIMIT)?
                .ok_or("decision")?,
            decision,
            "replay commits the sealed bytes"
        );
        assert_eq!(fake.decoded.get(), 1, "replay never decodes again");
        Ok(())
    })
}
// Spec: docs/specs/run-settlement.md#retention-and-disclosure
#[test]
fn cancelled_lane_preserves_outcome_and_session_bytes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
    // The operator cancelled before the provider spawned. Retain the session unchanged.
    // The scoped stop-intent reader (b290fda595) accepts only a complete
    // record; a bare reason fragment no longer names a cancellation.
    artifacts::seal(
        &dir,
        "cancel.json",
        &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "scope": RunStore::scope(&store.record(id, &call)?),
            "requester": store.context.actor_id,
            "request_id": "session-cancel",
            "reason": "explicit_cancellation",
        }))?,
        RECORD_LIMIT,
    )?;
    let attempt = store
        .context
        .state_root
        .as_path()
        .join(format!("attempts/{id}"));
    let sessions = attempt.join("provider-state/candidate-0/sessions");
    std::fs::create_dir_all(&sessions)?;
    let overlong = format!(
        "https://{}\n",
        "a".repeat(provenant_os::exec::FRAME_LIMIT + 1)
    );
    std::fs::write(sessions.join("overlong.jsonl"), &overlong)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    observed.never_spawned = true;
    observed.termination = None;
    observed.terminal_observed = false;
    observed.warnings = vec!["stopped by the operator".into()];
    observed.stop_reason = Some(provenant_supervisor::machine::StopReason::ExplicitCancellation);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        ..Fake::default()
    };

    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    let AttemptState::Terminal {
        outcome, reason, ..
    } = &record.state
    else {
        return Err("not terminal".into());
    };
    assert_eq!(*outcome, Outcome::Cancelled);
    assert_eq!(reason.as_deref(), Some("cancelled: explicit_cancellation"));
    assert_eq!(
        std::fs::read_to_string(sessions.join("overlong.jsonl"))?,
        overlong
    );
    Ok(())
}
// Spec: docs/specs/run-settlement.md#retention-and-disclosure
#[test]
fn orphan_result_with_plaintext_settles_once_and_preserves_its_bytes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let _scope = provenant_redact::publication_scope([token.as_bytes().to_vec()])?;
    artifacts::seal(&dir, "result.md", token.as_bytes(), RESULT_LIMIT)?;
    // An independent session traversal failure must not rewrite an orphan.
    let deep = store
        .context
        .state_root
        .as_path()
        .join(format!("attempts/{id}/provider-state/candidate-0/sessions"))
        .join("d/".repeat(32));
    std::fs::create_dir_all(&deep)?;
    std::fs::write(deep.join("deep.jsonl"), "session text")?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        // The sealed orphan wins over a different recomputed rendering.
        answer: Some(format!(
            "http://{}",
            "a".repeat(provenant_os::exec::FRAME_LIMIT + 1)
        )),
        ..Fake::default()
    };

    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    let result = record.result.as_ref().ok_or("result")?;
    assert!(!result.redacted);
    assert_eq!(
        artifacts::read_sealed(&dir, "result.md", RESULT_LIMIT)?.as_deref(),
        Some(token.as_bytes())
    );
    // Orphan recovery reaches a terminal state once; a repeat is a no-op.
    let repeat = store.finish(id, &dir, &observed, &fake, &call)?;
    assert_eq!(repeat.revision, record.revision);
    Ok(())
}
#[test]
fn a_declared_stop_settles_review_required() -> TestResult {
    let record = settle_with(
        &format!("Done: parser.\n\n{STOPPED_EARLY}\n"),
        Some(Outcome::Succeeded),
        Vec::new(),
    )?;
    let AttemptState::Terminal {
        outcome, reason, ..
    } = &record.state
    else {
        return Err("not terminal".into());
    };
    assert_eq!(*outcome, Outcome::ReviewRequired);
    assert_eq!(record.partial_signals, [PartialSignal::DeclaredStop]);
    let reason = reason.as_deref().ok_or("no reason")?;
    assert!(reason.contains("renderer and tests remain"), "{reason}");
    let blocked = STOPPED_EARLY.replace("stopped_early", "blocked");
    let record = settle_with(&blocked, Some(Outcome::Succeeded), Vec::new())?;
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::ReviewRequired,
            ..
        }
    ));
    assert_eq!(record.partial_signals, [PartialSignal::DeclaredStop]);
    Ok(())
}
#[test]
fn trailing_prose_does_not_change_a_declared_stop() -> TestResult {
    // The declaration channel settles the lane; trailing prose and a later
    // tool call in the answer can never supersede it.
    let answer =
        format!("Done: parser.\n\n{STOPPED_EARLY}\nThe wait was redundant; nothing changed.");
    let record = settle_with(&answer, Some(Outcome::Succeeded), Vec::new())?;
    assert_eq!(record.partial_signals, [PartialSignal::DeclaredStop]);
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::ReviewRequired,
            ..
        }
    ));
    Ok(())
}
#[test]
fn a_read_only_answer_without_handoff_settles_succeeded() -> TestResult {
    // `input()` is a read-only task, so its complete answer is its proof.
    for answer in [
        "answer".to_owned(),
        "VERDICT: PASS".to_owned(),
        "VERDICT: FAIL".to_owned(),
        "VERDICT: INCONCLUSIVE".to_owned(),
        "All of the brief is done.".to_owned(),
        "Work remains in the renderer, which I did not finish.".to_owned(),
        // A block quoted mid-answer is not the lane's closing statement.
        format!("The contract says:\n````text\n{STOPPED_EARLY}\n````\nI followed it and finished."),
    ] {
        let record = settle_with(&answer, Some(Outcome::Succeeded), Vec::new())?;
        assert!(record.partial_signals.is_empty(), "{answer}");
        let AttemptState::Terminal {
            outcome: Outcome::Succeeded,
            reason: None,
            ..
        } = &record.state
        else {
            return Err(format!("read-only answer must succeed: {:?}", record.state).into());
        };
        assert_ne!(
            record.settlement_reason,
            Some(provenant_records::run::SettlementReason::HandoffMissing)
        );
    }
    Ok(())
}
#[test]
fn provider_stops_are_kept_and_failures_carry_no_flag() -> TestResult {
    let record = settle_with(
        "answer",
        Some(Outcome::Succeeded),
        vec![PartialSignal::ProviderStop],
    )?;
    assert_eq!(record.partial_signals, [PartialSignal::ProviderStop]);
    let record = settle_with(
        &format!("{STOPPED_EARLY}\n"),
        Some(Outcome::Failed),
        vec![PartialSignal::ProviderStop],
    )?;
    assert_eq!(
        record.partial_signals,
        [PartialSignal::DeclaredStop, PartialSignal::ProviderStop]
    );
    Ok(())
}
#[test]
fn handoff_missing_and_done_outcome_contradictions() -> TestResult {
    let done = STOPPED_EARLY.replace("stopped_early", "done");
    let success = settle_with(&done, Some(Outcome::Succeeded), Vec::new())?;
    assert!(success.partial_signals.is_empty());
    for outcome in [Outcome::Cancelled, Outcome::ReviewRequired] {
        let record = settle_with(&done, Some(outcome), Vec::new())?;
        let AttemptState::Terminal {
            outcome: actual,
            reason,
            ..
        } = record.state
        else {
            return Err("not terminal".into());
        };
        assert_eq!(actual, outcome);
        assert!(reason.unwrap_or_default().contains("handoff declared done"));
    }
    Ok(())
}
// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn incomplete_identity_evidence_preserves_settlement_failures() -> TestResult {
    use provenant_records::policy::Strength;
    for strength in [Strength::Refuse, Strength::Warn, Strength::Off] {
        for route_mismatch in [true, false] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let context = context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let mut run = input();
            run.request
                .task
                .profile
                .policy_overrides
                .insert("PROVIDER_IDENTITY_EVIDENCE_INCOMPLETE".into(), strength);
            let id = run.attempt_id;
            store.create(&run, &call)?;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let dir = store.attempt_dir(id, false)?;
            artifacts::seal(&dir, "stdout.log", b"answer", CAPTURE_LIMIT)?;
            let mut observed = exit();
            observed.scope = RunStore::scope(&store.record(id, &call)?);
            observed.provider_identity_evidence_incomplete = true;
            seal_exit(&store, &observed)?;
            let fake = Fake {
                observed: Some(if route_mismatch {
                    RouteRecord {
                        model: Some("other/model".into()),
                        ..route()
                    }
                } else {
                    route()
                }),
                outcome: (!route_mismatch).then_some(Outcome::Failed),
                answer: Some("answer".into()),
                ..Fake::default()
            };
            let record = store.finish(id, &dir, &observed, &fake, &call)?;
            let AttemptState::Terminal {
                outcome,
                terminal_cause,
                ..
            } = record.state
            else {
                return Err("not terminal".into());
            };
            assert_eq!(outcome, Outcome::Failed);
            assert_eq!(
                terminal_cause,
                route_mismatch.then_some(CauseKind::IdentityMismatch)
            );
        }
    }
    Ok(())
}

#[test]
fn identity_observations_warn_when_missing_and_hold_when_contradictory() -> TestResult {
    for observed_route in [
        None,
        Some(RouteRecord {
            model: Some("other/model".into()),
            ..route()
        }),
        Some(RouteRecord {
            account_key: Some("other-account".into()),
            ..route()
        }),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        let requested = RouteRecord {
            account_key: Some("requested-account".into()),
            ..route()
        };
        store.mark_starting(
            &store.record(id, &call)?,
            &requested,
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let dir = store.attempt_dir(id, false)?;
        artifacts::seal(&dir, "stdout.log", b"answer", CAPTURE_LIMIT)?;
        let mut observed = exit();
        observed.scope = RunStore::scope(&store.record(id, &call)?);
        seal_exit(&store, &observed)?;
        let missing = observed_route.is_none();
        let fake = Fake {
            observed: observed_route.clone(),
            answer: Some("answer".into()),
            ..Fake::default()
        };
        let record = store.finish(id, &dir, &observed, &fake, &call)?;
        assert_eq!(
            record
                .applied_route
                .as_ref()
                .and_then(|r| r.account_key.as_deref()),
            Some("requested-account")
        );
        assert_eq!(record.observed_route, observed_route);
        let AttemptState::Terminal {
            outcome,
            terminal_cause,
            stop,
            ..
        } = &record.state
        else {
            return Err("not terminal".into());
        };
        let stop = stop.clone();
        if missing {
            assert_eq!(*outcome, Outcome::Succeeded);
            assert!(
                record
                    .partial_signals
                    .contains(&PartialSignal::IdentityUnobserved)
            );
            assert!(stop.is_none());
            assert_eq!(*terminal_cause, None);
        } else {
            assert_eq!(*outcome, Outcome::Failed);
            assert_eq!(*terminal_cause, Some(CauseKind::IdentityMismatch));
            assert_eq!(
                stop.as_ref().map(|stop| stop.cause()),
                Some(provenant_records::StopCause::IdentityMismatch)
            );
            assert!(provenant_records::StopCause::IdentityMismatch.rule().pinned);
        }
        // Exercise both persisted validation and idempotent settlement replay.
        assert_eq!(store.record(id, &call)?, record);
        assert_eq!(store.finish(id, &dir, &observed, &fake, &call)?, record);
    }
    Ok(())
}

#[test]
fn runtime_deadline_reason_survives_a_cancellation_record() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"half an answer", CAPTURE_LIMIT)?;
    artifacts::seal(
        &dir,
        "cancel.json",
        br#"{"reason":"wall_deadline"}"#,
        RECORD_LIMIT,
    )?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    observed.termination = Some(Termination::Signal(9));
    observed.stop_reason = Some(provenant_supervisor::machine::StopReason::WallDeadline);
    observed.cancel_delivered_before_exit = Some(true);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        outcome: Some(Outcome::Failed),
        ..Fake::default()
    };
    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    let AttemptState::Terminal {
        outcome, reason, ..
    } = record.state
    else {
        return Err("not terminal".into());
    };
    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(
        reason.as_deref(),
        Some("runtime deadline exceeded: provider stopped after 3600s")
    );
    Ok(())
}
#[test]
fn skipped_fallback_candidates_are_recorded_on_the_attempt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    let skipped = vec![SkippedRoute {
        skipped_accounts: Vec::new(),
        extra: Default::default(),
        route: "claude".into(),
        kind: SkipKind::AuthRequired,
        cause: None,
        detail: "account work: sign in again".into(),
        ..Default::default()
    }];
    store.record_skipped_routes(id, &skipped, &call)?;
    let started = store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    assert_eq!(started.skipped_routes, skipped);
    assert_eq!(store.record(id, &call)?.skipped_routes, skipped);
    Ok(())
}
#[test]
fn admission_holds_spacing_floors_and_deadline() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    let now = at_micros(100);

    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        read_only_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let initial = input();
    store.create(&initial, &call)?;
    let first = initial.attempt_id;
    store.mark_starting(
        &store.record(first, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store
        .claim(first, &now, &call)?
        .ok_or("first not admitted")?;
    assert_eq!(
        running.state,
        AttemptState::Running {
            extra: Default::default(),
            child_wait: None,
        }
    );
    assert_eq!(running.started_at, Some(now.wall_now()?));

    Ok(())
}
#[test]
fn graph_readers_get_the_sealed_handoff_not_a_rereading_of_the_text() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(id, false)?;
    artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let declared: serde_json::Value =
        serde_json::from_str(STOPPED_EARLY.lines().nth(1).ok_or("declaration line")?)?;
    store.declare(
        id,
        &provenant_records::declaration::DeclarationInput {
            request_id: "graph-reader".into(),
            payload: declared,
        },
        &provenant_records::declaration::DeclarationLimit::shipped(),
    )?;
    let answer =
        format!("Done: parser.\n\n{STOPPED_EARLY}\nThe wait was redundant; nothing changed.");
    let fake = Fake {
        observed: Some(route()),
        outcome: Some(Outcome::Succeeded),
        answer: Some(answer),
        ..Fake::default()
    };
    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    // The graph's capture reads the settled snapshot, never the prose again.
    let capture = store.stream_result(id, &call, None)?;
    let sealed = store.handoff(&record)??.ok_or("no sealed declaration")?;
    assert_eq!(capture.handoff, Ok(Some(sealed)));
    Ok(())
}

#[test]
fn abandon_writer_creation_floor_requires_proven_removed_lane_worktree() -> TestResult {
    use crate::{RunStore, Service, WriterRequest};
    use provenant_confinement::worktrees::{WorktreeOrigin, WorktreeOwner};

    fn setup_registration(
        common: &std::path::Path,
        worktree: &std::path::Path,
        attempt_id: Option<AttemptId>,
        origin: WorktreeOrigin,
    ) -> TestResult {
        let reg = common.join("worktrees/lane");
        std::fs::create_dir_all(&reg)?;
        let owner = WorktreeOwner {
            attempt_id,
            operation_id: Some("op-floor".parse()?),
            branch: "fix/3914".into(),
            base: "1".repeat(40),
            host: provenant_core::HostId::from_bytes([3; 16]),
            created_at: WallTime::from_micros(1_780_000_010_000_000)?,
            origin,
            label: None,
            reference: None,
        };
        std::fs::write(reg.join("gitdir"), format!("{}/.git\n", worktree.display()))?;
        std::fs::write(
            reg.join("provenant-owner.json"),
            serde_json::to_vec(&owner)?,
        )?;
        Ok(())
    }

    let queued_at = WallTime::from_micros(1_780_000_005_000_000)?;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let ctx = context(temp.path())?;
    let checkout = std::fs::canonicalize(temp.path())?.join("checkout");
    let common = checkout.join(".git");
    let worktree = checkout.join(".worktrees/lane");
    let store = RunStore::open(&ctx, &call)?;
    let service = Service::new(&store);

    let mut fresh = input();
    fresh.created_at = queued_at;
    fresh.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        custody_project: Some(ctx.project_id),
        project: checkout.clone(),
        worktree: worktree.clone(),
    });
    store.create(&fresh, &call)?;
    let id = fresh.attempt_id;

    // Missing Git registration fails closed.
    assert_eq!(
        service.abandon_writer_creation_floor(id, Some(&common), &call)?,
        None
    );

    // Valid removed Lane registration for this attempt yields queued_at (both
    // via explicit common_dir and repository_marker fallback).
    setup_registration(&common, &worktree, Some(id), WorktreeOrigin::Lane)?;
    assert_eq!(
        service.abandon_writer_creation_floor(id, Some(&common), &call)?,
        Some(queued_at)
    );
    assert_eq!(
        service.abandon_writer_creation_floor(id, None, &call)?,
        Some(queued_at)
    );

    // An extant worktree keeps using its own filesystem birth time.
    std::fs::create_dir_all(&worktree)?;
    assert_eq!(
        service.abandon_writer_creation_floor(id, Some(&common), &call)?,
        None
    );
    std::fs::remove_dir(&worktree)?;

    // Adopted origin or another attempt_id fails closed.
    setup_registration(&common, &worktree, Some(id), WorktreeOrigin::Adopted)?;
    assert_eq!(
        service.abandon_writer_creation_floor(id, Some(&common), &call)?,
        None
    );
    let other_id = AttemptId::from_bytes([9; 16]);
    setup_registration(&common, &worktree, Some(other_id), WorktreeOrigin::Lane)?;
    assert_eq!(
        service.abandon_writer_creation_floor(id, Some(&common), &call)?,
        None
    );

    // Retry attempts reuse the predecessor's worktree and must fail closed.
    let mut retry = input();
    retry.attempt_id = AttemptId::from_bytes([7; 16]);
    retry.operation_id = "run-retry".parse()?;
    retry.created_at = WallTime::from_micros(1_780_000_020_000_000)?;
    retry.request.parent = Some(id);
    retry.request.task.writer = fresh.request.task.writer.clone();
    store.create(&retry, &call)?;
    setup_registration(
        &common,
        &worktree,
        Some(retry.attempt_id),
        WorktreeOrigin::Lane,
    )?;
    assert_eq!(
        service.abandon_writer_creation_floor(retry.attempt_id, Some(&common), &call)?,
        None
    );

    // Graph continuations reuse a predecessor's worktree and must fail closed.
    let mut cont = input();
    cont.attempt_id = AttemptId::from_bytes([8; 16]);
    cont.operation_id = "run-cont".parse()?;
    cont.created_at = WallTime::from_micros(1_780_000_030_000_000)?;
    cont.request.task.writer = fresh.request.task.writer;
    tests::rekey(&mut cont);
    cont.request
        .task
        .context
        .get_or_insert_with(Default::default)
        .writer_predecessor = Some(id);
    store.create(&cont, &call)?;
    setup_registration(
        &common,
        &worktree,
        Some(cont.attempt_id),
        WorktreeOrigin::Lane,
    )?;
    assert_eq!(
        service.abandon_writer_creation_floor(cont.attempt_id, Some(&common), &call)?,
        None
    );

    // A registration older than the queue sets the floor: a reused checkout
    // can predate queued_at, and the floor must not move past its creation.
    let mut late = input();
    late.attempt_id = AttemptId::from_bytes([5; 16]);
    late.operation_id = "run-late".parse()?;
    late.created_at = WallTime::from_micros(4_000_000_000_000_000)?;
    late.request.task.writer = cont.request.task.writer.clone();
    tests::rekey(&mut late);
    store.create(&late, &call)?;
    setup_registration(
        &common,
        &worktree,
        Some(late.attempt_id),
        WorktreeOrigin::Lane,
    )?;
    let registered =
        provenant_confinement::worktrees::worktree_registration_for_path(&common, &worktree)?
            .and_then(|(_, birth)| birth);
    let floor = service.abandon_writer_creation_floor(late.attempt_id, Some(&common), &call)?;
    assert_eq!(floor, registered, "the floor is the registration's birth");
    if let Some(floor) = floor {
        assert!(floor < late.created_at);
    }
    Ok(())
}
