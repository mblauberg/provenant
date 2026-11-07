use super::*;

#[test]
fn lane_access_canonicalises_attachment_files() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let image = temp.path().join("shot.png");
    std::fs::write(&image, b"image")?;
    let access = crate::Access {
        attach: vec![image.clone()],
        mode: None,
        worktree: None,
        read_root: vec![],
        write_root: vec![],
        network: None,
        capability: vec![],
        no_capability: vec![],
    };
    let granted = lane_access(
        &access,
        &temp.path().join("state"),
        Path::new("/fixture/home"),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(granted.attachments, vec![std::fs::canonicalize(image)?]);
    assert!(granted.read_roots.is_empty());
    Ok(())
}
#[test]
fn lane_access_refuses_a_root_enclosing_an_injected_nonconventional_home()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let team = temp.path().join("srv/team");
    let home = team.join("alice");
    let project = home.join("project");
    let config = home.join(".config");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&config)?;
    let team = std::fs::canonicalize(team)?;
    let home = std::fs::canonicalize(home)?;
    let project = std::fs::canonicalize(project)?;
    let config = std::fs::canonicalize(config)?;
    let state = temp.path().join("state");
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: vec![],
        no_capability: Vec::new(),
    };
    for root in [&team, &home, &config] {
        for (flag, read) in [("--read-root", true), ("--write-root", false)] {
            let fail = lane_access(
                &access(
                    if read { vec![root.clone()] } else { vec![] },
                    if read { vec![] } else { vec![root.clone()] },
                ),
                &state,
                &home,
            )
            .unwrap_err();
            assert!(fail.message.contains(flag), "{}", fail.message);
            assert!(
                fail.message.contains(&root.display().to_string()),
                "{}",
                fail.message
            );
        }
    }
    let granted = lane_access(
        &access(vec![project.clone()], vec![project.clone()]),
        &state,
        &home,
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(granted.read_roots, vec![project.clone()]);
    assert_eq!(granted.write_roots, vec![project]);
    Ok(())
}
#[test]
fn lane_access_allows_codex_standalone_read_roots() -> Result<(), Box<dyn std::error::Error>> {
    // The rule is lexical, so check it without the filesystem first.
    let shape = Path::new(
        "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin/bin",
    );
    assert!(
        !provenant_confinement::refuses_lane_root(shape, true, None),
        "reads pass entry"
    );
    assert!(
        provenant_confinement::refuses_lane_root(shape, false, None),
        "writes stay strict"
    );
    // Against a machine's real home: build a temp release and walk it
    // through `lane_access`. A lane can seal `~/.codex`; then the fixture
    // cannot exist and the filesystem part is skipped silently.
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(());
    };
    let releases = Path::new(&home).join(".codex/packages/standalone/releases");
    if std::fs::create_dir_all(&releases).is_err() {
        return Ok(());
    }
    let Ok(temp) = tempfile::tempdir_in(&releases) else {
        return Ok(());
    };
    let release = std::fs::canonicalize(temp.path())?;
    let state = tempfile::tempdir()?;
    let state = std::fs::canonicalize(state.path())?;
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: vec![],
        no_capability: Vec::new(),
    };
    let granted = lane_access(
        &access(vec![release.clone()], vec![]),
        &state.join("grants"),
        Path::new(&home),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(granted.read_roots, vec![release.clone()]);
    let fail = lane_access(
        &access(vec![], vec![release.clone()]),
        &state.join("writes"),
        Path::new(&home),
    )
    .unwrap_err();
    assert_eq!(
        fail.message,
        format!(
            "--write-root {} is or encloses a credential root",
            release.display()
        )
    );
    Ok(())
}
/// Entry agrees with compile: a read root in a Codex standalone release
/// passes the credential-root refusal, while a write root there stays
/// refused with the flag and the root named.
#[test]
fn entry_root_table_matches_confinement() {
    let standalone =
        "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin";
    let home = Path::new("/fixture/host-home");
    for (path, read_allowed, write_allowed) in [
        ("/Users/alice/.ssh", false, false),
        ("/Users/alice/.config", false, false),
        (standalone, true, false),
        ("/Users/alice/project", true, true),
    ] {
        for (flag, allowed) in [
            ("--read-root", read_allowed),
            ("--write-root", write_allowed),
        ] {
            let refused = refused_root(flag, Path::new(path), Path::new(path), home);
            assert_eq!(refused.is_none(), allowed, "{flag} {path}");
            if let Some(AccessError::CredentialRoot {
                flag: named,
                path: named_path,
            }) = refused
            {
                assert_eq!(named, flag);
                assert_eq!(named_path, Path::new(path));
            }
        }
    }
    // A host home outside /Users and /home is refused at entry, so entry
    // and compile agree without this test depending on the host's HOME.
    let home = Path::new("/srv/team/alice");
    for (path, read_allowed, write_allowed) in [
        ("/srv/team", false, false),
        ("/srv/team/alice", false, false),
        ("/srv/team/alice/.config", false, false),
        ("/srv/team/alice/project", true, true),
    ] {
        for (flag, allowed) in [
            ("--read-root", read_allowed),
            ("--write-root", write_allowed),
        ] {
            let refused = refused_root(flag, Path::new(path), Path::new(path), home);
            assert_eq!(refused.is_none(), allowed, "{flag} {path}");
        }
    }
}
#[test]
fn lane_access_maps_network_capability_and_roots() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let project = temp.path().join("project");
    provenant_os::fixture::private_state_dir(&state)?;
    std::fs::create_dir_all(&project)?;
    let access = |mode, network, capability: &[&str], write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(mode),
        worktree: None,
        read_root: Vec::new(),
        write_root,
        network,
        capability: capability.iter().map(|name| name.to_string()).collect(),
        no_capability: Vec::new(),
    };
    let network = |mode, switch, capability: &[&str]| {
        lane_access(
            &access(mode, switch, capability, Vec::new()),
            &state,
            Path::new("/fixture/host-home"),
        )
        .map(|access| access.network)
        .map_err(|fail| fail.message)
    };
    assert_eq!(network(Mode::WorktreeWrite, None, &[]), Ok(None));
    assert_eq!(network(Mode::ReadOnly, None, &["postgres"]), Ok(None));
    for mode in [Mode::ReadOnly, Mode::WorktreeWrite] {
        for (capability, expected) in [(Vec::new(), false), (vec!["postgres"], true)] {
            let declared = lane_access_checked(
                &access(mode, None, &capability, Vec::new()),
                &state,
                Path::new("/fixture/host-home"),
            )?;
            assert_eq!(declared.database_ipc, expected);
        }
    }
    assert_eq!(
        network(Mode::WorktreeWrite, Some(Switch::Off), &["postgres"]),
        Ok(Some(run::NetworkAccess::Local))
    );
    assert_eq!(
        network(Mode::ReadOnly, Some(Switch::Off), &["postgres"]),
        Ok(Some(run::NetworkAccess::Local))
    );
    assert_eq!(
        network(Mode::ReadOnly, Some(Switch::On), &[]),
        Ok(Some(run::NetworkAccess::Open))
    );
    let unknown = lane_access_checked(
        &access(Mode::ReadOnly, None, &["rust"], Vec::new()),
        &state,
        Path::new("/fixture/host-home"),
    )?;
    assert!(!unknown.database_ipc);
    let granted = lane_access(
        &access(Mode::WorktreeWrite, None, &[], vec![project.clone()]),
        &state,
        Path::new("/fixture/host-home"),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(
        granted.write_roots,
        [provenant_os::fs::canonicalize(&project)?]
    );
    for refused in [
        state.join("projects"),
        temp.path().to_path_buf(),
        temp.path().join("absent"),
    ] {
        std::fs::create_dir_all(state.join("projects"))?;
        assert!(
            lane_access(
                &access(Mode::WorktreeWrite, None, &[], vec![refused]),
                &state,
                Path::new("/fixture/host-home"),
            )
            .is_err()
        );
    }
    Ok(())
}
#[test]
fn lane_access_refuses_write_root_overlapping_the_worktree()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let worktree = temp.path().join("work").join("tree");
    let inside = worktree.join("src");
    provenant_os::fixture::private_state_dir(&state)?;
    std::fs::create_dir_all(&inside)?;
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::WorktreeWrite),
        worktree: Some(worktree.clone()),
        read_root,
        write_root,
        network: None,
        capability: Vec::new(),
        no_capability: Vec::new(),
    };
    for overlapping in [worktree.clone(), temp.path().join("work")] {
        let expected = provenant_os::fs::canonicalize(&overlapping)?;
        assert!(matches!(
            lane_access_checked(
                &access(Vec::new(), vec![overlapping]),
                &state,
                Path::new("/fixture/host-home"),
            ),
            Err(AccessError::WorktreeOverlap(path)) if path == expected
        ));
    }
    // A read root on the worktree and a write root beneath it stay valid.
    assert!(
        lane_access(
            &access(vec![worktree.clone()], vec![inside]),
            &state,
            Path::new("/fixture/host-home"),
        )
        .is_ok()
    );
    Ok(())
}
#[test]
fn lane_access_reads_installed_release_skills_only() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let releases = state.join("releases");
    let release = releases.join(provenant_records::SourceDigest::from_bytes([1; 32]).to_string());
    let skills = release.join("skills");
    std::fs::create_dir_all(&skills)?;
    std::fs::create_dir_all(state.join("projects"))?;
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: Vec::new(),
        no_capability: Vec::new(),
    };
    // Preset roles read bundled skills from the installed release.
    let granted = lane_access(
        &access(vec![skills.clone()], Vec::new()),
        &state,
        Path::new("/fixture/host-home"),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(
        granted.read_roots,
        [provenant_os::fs::canonicalize(&skills)?]
    );
    assert!(
        lane_access(
            &access(vec![release.clone()], Vec::new()),
            &state,
            Path::new("/fixture/host-home"),
        )
        .is_ok()
    );
    for invalid in ["not-a-digest", "sha256:abc", ".provenant-staging.fixture"] {
        let root = releases.join(invalid).join("skills");
        std::fs::create_dir_all(&root)?;
        assert!(matches!(
            lane_access_checked(
                &access(vec![root], Vec::new()),
                &state,
                Path::new("/fixture/host-home"),
            ),
            Err(AccessError::StateOverlap { .. })
        ));
    }
    for refused in [
        access(vec![releases.clone()], Vec::new()),
        access(vec![state.join("projects")], Vec::new()),
        access(Vec::new(), vec![release]),
        access(Vec::new(), vec![skills.clone()]),
    ] {
        assert!(matches!(
            lane_access_checked(&refused, &state, Path::new("/fixture/host-home"),),
            Err(AccessError::StateOverlap { .. })
        ));
    }
    Ok(())
}
#[test]
fn paged_output_never_splits_a_character() {
    let bytes = "a☀b".as_bytes();
    let mut chunk = run::OutputChunk {
        bytes: bytes[..3].to_vec(),
        len: bytes.len() as u64,
        offset: 0,
        next_offset: 3,
        eof: false,
        clipped: true,
        complete: true,
        encoding: "utf8-fragment",
        digest: provenant_records::CanonicalV1::file_digest(b""),
    };
    align_utf8_tail(&mut chunk);
    assert_eq!(chunk.bytes, b"a");
    assert_eq!(chunk.next_offset, 1);
    assert_eq!(chunk.encoding, "utf8");
}
#[test]
fn quota_reason_uses_safe_fact_without_provider_message() {
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: RunId::from_bytes([1; 16]),
            attempt: AttemptId::from_bytes([2; 16]),
            generation: 1,
        },
        termination: Some(provenant_os::exec::Termination::Exit(1)),
        never_spawned: false,
        launch_refusal: None,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        cancel_delivered_before_exit: None,
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: false,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: SettlementEvidence::NeverSpawned,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    };
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/a-free".into()),
        ..Default::default()
    };
    let registry = Registry {
        roots: None,
        home: None,
        assets: None,
        cwd: None,
        state_root: None,
        project_id: None,
        host: None,
        cancel: None,
        repo_facts: Default::default(),
    };
    for (message, _expected) in [
        (
            "Free allowance exhausted. See https://opencode.ai/go\\quota",
            "Free allowance exhausted. See https://opencode.ai/go\\quota",
        ),
        (
            "Free allowance exhausted TOKEN_SENTINEL /private/source \u{1b} ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "Free allowance exhausted for [REDACTED]",
        ),
    ] {
        let event = json!({"type":"provider.quota","response":{"body":{"error":{"type":"FreeUsageLimitError","message":message}}}});
        let decoded = registry
            .decode(
                &route,
                event.to_string().as_bytes(),
                &exit,
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
        assert_eq!(
            decoded.terminal_cause,
            Some(run::TerminalCause::FreePoolQuota)
        );
        let reason = decoded.reason.as_deref().unwrap();
        assert!(reason.contains("free tier exhausted"));
        assert!(reason.contains("wait"));
        assert!(!reason.contains(message));
        assert!(!reason.contains("ghp_"));
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/source"));
        assert!(!reason.contains('\u{1b}'));
    }
    let event = json!({"type":"provider.quota","message":"☀".repeat(300_000)});
    let decoded = registry
        .decode(
            &route,
            event.to_string().as_bytes(),
            &exit,
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
    let reason = decoded.reason.unwrap();
    assert!(
        reason.len() <= 8192,
        "reason must leave room in the terminal record"
    );
    assert!(!reason.contains('☀'));
    assert!(reason.contains("quota"));
    assert!(reason.contains("wait"));
}
#[test]
fn opencode_stderr_quota_settles_and_records_refusal() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use provenant_quota::Admission;
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    use std::os::unix::fs::PermissionsExt;
    if crate::fabric::in_lane() {
        // A lane suppresses the host quota writes this test observes.
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    std::fs::create_dir(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let provider_state = state.join("runs/run/attempts/attempt/provider-state");
    let argv = ["/bin/sh", "-c", "printf '%s\\n' 'AI.Error.QuotaExceeded: Go usage limit exceeded retryAfterMs=60000' >&2; exit 1"].map(std::ffi::OsString::from);
    let mut options = ExecOptions::new(&argv, temp.path(), Duration::from_secs(30));
    options.stdout = StreamPolicy::Capture { limit: 8192 };
    options.stderr = StreamPolicy::Capture { limit: 8192 };
    let output = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(output.termination, Termination::Exit(1));
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode-go/example".into()),
        account_key: Some("go".into()),
        account_label: Some("go".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry::default().decode_captures(
        &route,
        &output.stdout.bytes,
        &output.stderr.bytes,
        &exit,
        &provider_state,
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
    let records = crate::quota::read_cache_at(&state)?;
    assert_eq!(
        records.len(),
        1,
        "host refusal missing; lane environment suppresses host quota writes"
    );
    let refusal = &records[0].refusals[0];
    assert_eq!(refusal.pool.as_deref(), Some("opencode-go"));
    assert_eq!(
        refusal.resets_unix_seconds,
        Some(refusal.observed_unix_seconds + 60)
    );
    let resolved = ResolvedRoute {
        extra: Default::default(),
        adapter: "opencode".into(),
        model: "opencode-go/example".into(),
        effort: None,
        catalogue_revision: "fixture".into(),
        account_key: Some("go".into()),
        host: "host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: i64::MAX,
        },
        confidentiality: RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture".into(),
        },
        pool_selection: None,
    };
    let pools = vec!["opencode-go".into()];
    let check = |now_unix_seconds| {
        Admission {
            policies: &[],
            now_unix_seconds,
            records: &records,
            pools: &pools,
        }
        .check(&resolved)
    };
    assert!(check(refusal.observed_unix_seconds).is_err());
    assert!(check(refusal.observed_unix_seconds + 60).is_ok());
    Ok(())
}
#[test]
fn opencode_stderr_quota_text_does_not_overturn_a_successful_run()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    std::fs::create_dir(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let provider_state = state.join("runs/run/attempts/attempt/provider-state");
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(0));
    let decoded = Registry::default().decode_captures(
        &opencode_decode_route(),
        include_bytes!("../../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"),
        b"AI.Error.QuotaExceeded: Go usage limit exceeded retryAfterMs=60000\n",
        &exit,
        &provider_state,
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert!(
        crate::quota::read_cache_at(&state)
            .map(|records| records.iter().all(|record| record.refusals.is_empty()))
            .unwrap_or(true),
        "a successful run must not persist a quota refusal"
    );
    Ok(())
}
#[test]
fn opencode_unavailable_model_reaches_the_decoded_outcome() {
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode/glm-5.3-flas-free".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry::default()
        .decode(
            &route,
            include_bytes!("../../../../tests/fixtures/providers/opencode/model-unavailable.jsonl"),
            &exit,
            Path::new("/nonexistent"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    let reason = decoded.reason.as_deref().unwrap();
    assert!(reason.starts_with("provider model unavailable"), "{reason}");
    assert!(!reason.contains("opencode/misspelt-free"), "{reason}");
    assert!(reason.contains("nearest catalogued IDs:"));
    assert!(decoded.observed_route.is_none());
}
#[test]
fn unlisted_codex_generic_failure_does_not_suggest_catalogued_models() {
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-luna".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry::default()
        .decode(&route, b"", &exit, Path::new("/nonexistent"))
        .unwrap();
    assert_ne!(decoded.outcome, run::Outcome::Succeeded);
    assert!(
        !decoded
            .reason
            .unwrap_or_default()
            .contains("nearest catalogued IDs")
    );
}
#[test]
fn codex_capacity_reaches_the_decoded_outcome() {
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        ..Default::default()
    };
    let stream = format!(
        "{}\n{}\n",
        json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
        json!({"type":"turn.failed","error":{"message":"Selected model is at capacity. TOKEN_SENTINEL /private/source","codex_error_info":"server_overloaded"}})
    );
    let decoded = Registry::default()
        .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(
        decoded.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    assert_eq!(
        decoded.session.as_deref(),
        Some("01234567-89ab-cdef-0123-456789abcdef")
    );
}
#[test]
fn capacity_store_failure_does_not_change_settlement() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let account_dir = state.join("quota/key");
    std::fs::create_dir_all(&account_dir)?;
    for dir in [&state, &state.join("quota"), &account_dir] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let bad_record = account_dir.join("codex.json");
    std::fs::write(&bad_record, b"invalid quota record")?;
    std::fs::set_permissions(&bad_record, std::fs::Permissions::from_mode(0o600))?;
    let context = run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.path().join("home"),
            checkout: temp.path().join("checkout"),
            state_root: state.clone(),
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        run_id: RunId::from_bytes([3; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    // The malformed on-disk record makes the store update fail after the
    // settlement path creates the lock. The lock proves it attempted the write.
    assert!(!state.join("quota/quota.lock").exists());
    let decoded = Registry::default().decode_captures(
        &route,
        stream,
        &[],
        &exit,
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert!(state.join("quota/quota.lock").is_file());
    assert_eq!(std::fs::read(&bad_record)?, b"invalid quota record");
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(
        decoded.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    Ok(())
}
#[test]
fn opencode_decode_with_context_reads_home_data_when_xdg_is_unset()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(OPENCODE_DECODE_XDG_MARKER).is_none() {
        run_opencode_decode_child(
            "opencode_decode_with_context_reads_home_data_when_xdg_is_unset",
            None,
        )?;
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let context = opencode_decode_holder_context(temp.path())?;
    seed_opencode_session(
        &context.roots.home.join(".local/share/opencode"),
        "ses_x",
        "opencode",
        "glm-5.3-flash-free",
    )?;
    // The success fixture's stream reports no model; the session record
    // beside the holder home supplies it through `decode_captures`.
    let decoded = Registry::default().decode_captures(
        &opencode_decode_route(),
        include_bytes!("../../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"),
        &[],
        &clean_exit(),
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(decoded.session.as_deref(), Some("ses_x"));
    assert_eq!(
        decoded
            .observed_route
            .as_ref()
            .and_then(|r| r.model.as_deref()),
        Some("opencode/opencode/glm-5.3-flash-free")
    );
    Ok(())
}
#[test]
fn absolute_xdg_data_home_wins_over_home_for_decode_with_context()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(OPENCODE_DECODE_XDG_MARKER).is_none() {
        let xdg = tempfile::tempdir()?;
        seed_opencode_session(
            &xdg.path().join("opencode"),
            "ses_x",
            "opencode",
            "beam-free",
        )?;
        run_opencode_decode_child(
            "absolute_xdg_data_home_wins_over_home_for_decode_with_context",
            Some(xdg.path()),
        )?;
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let context = opencode_decode_holder_context(temp.path())?;
    // A differing home record proves `XDG_DATA_HOME` took precedence.
    seed_opencode_session(
        &context.roots.home.join(".local/share/opencode"),
        "ses_x",
        "opencode",
        "home-decoy",
    )?;
    let decoded = Registry::default().decode_captures(
        &opencode_decode_route(),
        include_bytes!("../../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"),
        &[],
        &clean_exit(),
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(
        decoded
            .observed_route
            .as_ref()
            .and_then(|r| r.model.as_deref()),
        Some("opencode/opencode/beam-free")
    );
    Ok(())
}
