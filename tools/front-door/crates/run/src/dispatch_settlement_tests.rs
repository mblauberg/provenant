use super::*;

#[test]
fn holder_seals_redacted_results_and_digests_the_stored_bytes() -> TestResult {
    for (answer, expected) in [
        ("plain answer", "plain answer"),
        (
            "credential ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "credential [REDACTED]",
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
        assert!(
            artifact.redacted,
            "redacted means the filter was applied, including clean results"
        );
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
#[test]
fn settlement_scrubs_provider_session_history() -> TestResult {
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
        // Force a streaming filter refusal in each relocated store. Settlement
        // still succeeds and scrubs the other text in that store.
        std::fs::write(
            tree.join("overlong.log"),
            format!("https://{}\n", "a".repeat(128 * 1024)),
        )?;
    }
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        answer: Some(DONE_ANSWER.into()),
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
    for path in [
        &rollout,
        &session,
        &cursor.join("text.log"),
        &opencode.join("text.log"),
    ] {
        let text = std::fs::read_to_string(path)?;
        assert!(
            !text.contains(token),
            "session plaintext survived settlement in {path:?}"
        );
        assert!(text.contains("[REDACTED]"));
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
#[test]
fn a_lane_without_a_handoff_reports_a_stdout_path_denial_in_its_reason() -> TestResult {
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
    artifacts::seal(
        &dir,
        "stdout.log",
        b"tool: /fixture/missing-grant: Permission denied TOKEN_SENTINEL\n",
        CAPTURE_LIMIT,
    )?;
    artifacts::seal(&dir, "stderr.log", b"", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        answer: Some("The tool failed".into()),
        ..Fake::default()
    };
    store.finish(id, &dir, &observed, &fake, &call)?;
    let reason = terminal_reason(&store, id, &call)?;
    assert!(!reason.contains("/fixture/missing-grant"), "{reason}");
    assert!(!reason.contains("TOKEN_SENTINEL"), "{reason}");
    assert!(reason.contains("access grant"), "{reason}");
    // A read-only answer is its own proof, so the denial is a diagnostic only.
    assert_eq!(terminal(&store, id, &call)?, Outcome::Succeeded);
    Ok(())
}
#[test]
fn a_path_denial_leaves_a_finished_lanes_answer_and_success_intact() -> TestResult {
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
    artifacts::seal(&dir, "stderr.log", b"", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let answer = "cat: /fixture/missing-grant: Operation not permitted\nI used the brief instead.\n\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```\n";
    let fake = Fake {
        observed: Some(route()),
        answer: Some(answer.into()),
        ..Fake::default()
    };
    store.finish(id, &dir, &observed, &fake, &call)?;
    assert_eq!(terminal(&store, id, &call)?, Outcome::Succeeded);
    let result = artifacts::read_sealed(&dir, "result.md", RESULT_LIMIT)?.ok_or("result")?;
    assert_eq!(String::from_utf8(result)?, answer);
    let reason = terminal_reason(&store, id, &call)?;
    assert!(!reason.contains("/fixture/missing-grant"), "{reason}");
    assert!(!reason.contains("TOKEN_SENTINEL"), "{reason}");
    Ok(())
}
#[test]
fn denial_evidence_does_not_turn_an_empty_answer_into_success() -> TestResult {
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
    artifacts::seal(
        &dir,
        "stdout.log",
        b"tool: /fixture/missing-grant: Permission denied TOKEN_SENTINEL\n",
        CAPTURE_LIMIT,
    )?;
    artifacts::seal(&dir, "stderr.log", b"", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        answer: Some(String::new()),
        ..Fake::default()
    };
    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    let reason = terminal_reason(&store, id, &call)?;
    assert!(!reason.contains("/fixture/missing-grant"), "{reason}");
    assert!(!reason.contains("TOKEN_SENTINEL"), "{reason}");
    assert!(reason.contains("access grant"), "{reason}");
    assert_eq!(terminal(&store, id, &call)?, Outcome::Failed);
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
#[test]
fn unredactable_result_settles_failed_with_a_placeholder() -> TestResult {
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
        answer: Some(answer),
        ..Fake::default()
    };

    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    let AttemptState::Terminal {
        outcome, reason, ..
    } = &record.state
    else {
        return Err("not terminal".into());
    };
    assert_eq!(*outcome, Outcome::Failed);
    assert_eq!(reason.as_deref(), Some("redaction failed; result withheld"));
    assert_eq!(
        artifacts::read_sealed(&dir, "result.md", RESULT_LIMIT)?.as_deref(),
        Some(b"[result withheld: redaction failed]".as_slice())
    );
    assert!(record.result.ok_or("result")?.redacted);
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
#[test]
fn cancelled_lane_keeps_its_outcome_and_reason_when_session_scrub_fails() -> TestResult {
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
    // The operator cancelled before the provider spawned. An over-long `://`
    // run makes the session scrub refuse a transcript.
    artifacts::seal(
        &dir,
        "cancel.json",
        br#"{"reason":"explicit_cancellation"}"#,
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
    std::fs::write(sessions.join("overlong.jsonl"), overlong)?;
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
    assert_eq!(
        reason.as_deref(),
        Some("cancelled: explicit_cancellation; redaction failed; result withheld")
    );
    Ok(())
}
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
    artifacts::seal(&dir, "result.md", token.as_bytes(), RESULT_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        // The recomputed rendering cannot be filtered; the sealed orphan is
        // still preserved with an honest redaction flag rather than rewritten.
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
            outcome: Outcome::Failed,
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
fn a_reply_to_a_late_notification_keeps_the_turn_ending_handoff() -> TestResult {
    use provenant_records::handoff::TurnEnd;
    let first = format!("Done: parser.\n\n{STOPPED_EARLY}");
    let answer = format!("{first}\nThe wait was redundant; nothing changed.");
    let turn = |at, tool_activity| TurnEnd { at, tool_activity };
    // The declaration ended the first turn; the reply ran no tool.
    let record = settle_with_turns(
        &answer,
        Some(Outcome::Succeeded),
        Vec::new(),
        None,
        vec![turn(first.len(), true), turn(answer.len(), false)],
    )?;
    assert_eq!(record.partial_signals, [PartialSignal::DeclaredStop]);
    assert!(matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::ReviewRequired,
            ..
        }
    ));
    // Without boundaries, or after later tool work, the trailing text
    // displaces it as before.
    for turns in [
        Vec::new(),
        vec![turn(first.len(), true), turn(answer.len(), true)],
    ] {
        let record = settle_with_turns(&answer, Some(Outcome::Succeeded), Vec::new(), None, turns)?;
        assert!(record.partial_signals.is_empty());
    }
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
        format!("The contract says:\n{STOPPED_EARLY}\nI followed it and finished."),
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
        let AttemptState::Terminal { outcome, extra, .. } = &record.state else {
            return Err("not terminal".into());
        };
        let stop = extra
            .get("stop")
            .cloned()
            .map(serde_json::from_value::<provenant_records::Stop>)
            .transpose()?;
        if missing {
            assert_eq!(*outcome, Outcome::Succeeded);
            assert!(
                record
                    .partial_signals
                    .contains(&PartialSignal::IdentityUnobserved)
            );
            assert!(stop.is_none());
            assert!(!extra.contains_key("terminal_cause"));
        } else {
            assert_eq!(*outcome, Outcome::Failed);
            assert_eq!(
                extra.get("terminal_cause"),
                Some(&serde_json::to_value(TerminalCause::IdentityMismatch)?)
            );
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
        extra: Default::default(),
        route: "claude".into(),
        kind: SkipKind::AuthRequired,
        cause: None,
        detail: "account work: sign in again".into(),
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
            extra: Default::default()
        }
    );
    assert_eq!(running.started_at, Some(now.wall_now()?));

    Ok(())
}
#[test]
fn graph_readers_get_the_sealed_handoff_not_a_rereading_of_the_text() -> TestResult {
    use provenant_records::handoff::TurnEnd;
    let first = format!("Done: parser.\n\n{STOPPED_EARLY}");
    let answer = format!("{first}\nThe wait was redundant; nothing changed.");
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
        outcome: Some(Outcome::Succeeded),
        answer: Some(answer.clone()),
        turns: vec![
            TurnEnd {
                at: first.len(),
                tool_activity: true,
            },
            TurnEnd {
                at: answer.len(),
                tool_activity: false,
            },
        ],
        ..Fake::default()
    };
    let record = store.finish(id, &dir, &observed, &fake, &call)?;
    // The text ends with an acknowledgement, which the whole-answer rule
    // would call "no handoff"; the graph's capture carries the settled one.
    let capture = store.stream_result(id, &call, None)?;
    let sealed = store.handoff(&record)?.ok_or("sealed handoff")?;
    assert_eq!(capture.handoff, Ok(Some(sealed)));
    Ok(())
}
