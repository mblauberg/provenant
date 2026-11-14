use super::*;

#[test]
fn review_delivery_requires_acceptance_and_exact_head()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::Occupancy,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    use provenant_core::{CancelToken, Clock, RootInputs};
    use provenant_os::clock::SystemClock;
    let fixture = PrepareFixture::new()?;
    let mut execution = tests::context(&fixture.root)?;
    let mut inputs = RootInputs::new(fixture.root.join("home"), fixture.root.join("checkout"));
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    execution.project_id = provenant_confinement::worktrees::with_context(
        &roots,
        &fixture.root.join("home"),
        &fixture.writer.project,
        &clock,
        &cancel,
        |context| {
            let context = provenant_confinement::worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                attempt_id: None,
                operation_id: None,
                branch: "lane".into(),
                base: String::new(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: clock
                    .wall_now()
                    .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput)?,
                origin: provenant_confinement::worktrees::WorktreeOrigin::Adopted,
                label: None,
                reference: None,
            };
            let head = provenant_confinement::run_utility(
                context.executor,
                provenant_confinement::GitRead::RevParseHead,
                context.repo,
                context.utility,
                context.utility_budget,
                &cancel,
            )?;
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                base: String::from_utf8_lossy(&head.stdout.bytes).trim().into(),
                ..owner
            };
            provenant_confinement::worktrees::save_owner(
                &provenant_confinement::repo::verify_worktree(
                    context.repo,
                    &fixture.writer.worktree,
                )?,
                &owner,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    input.request.task.deliver = Some(fixture.request.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
        })?,
        RECORD_LIMIT,
    )?;
    let exit = tests::exit();
    tests::seal_observation(&store, &input, &exit)?;
    let lock = attempt.lock(
        std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
        provenant_os::fs::LockMode::Exclusive,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    drop(lock);
    let mut publication = tests::publication();
    publication.outcome = Outcome::ReviewRequired;
    publication.reason = Some("protected changes require review".into());
    store.publish_terminal(id, 1, &publication, &exit, &call)?;
    let deliver = |accept_review| {
        store.deliver(
            id,
            None,
            HostDeliveryContext::new(fixture.environment.clone()),
            accept_review,
            &call,
        )
    };
    let refusal = deliver(false).expect_err("review requires explicit acceptance");
    assert!(matches!(refusal, RunError::ReviewAcceptanceRequired));
    fixture.commit(&fixture.writer.worktree, "moved after review")?;
    assert!(matches!(
        deliver(true),
        Err(RunError::Validation(
            "worktree head differs from the settled receipt"
        ))
    ));
    fixture.lane_git(&["reset", "--hard", &head.to_string()])?;
    let receipt = deliver(true)?;
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered { .. }
    ));
    let acceptance = receipt.review_acceptance.as_ref().ok_or("acceptance")?;
    assert!(acceptance.protected_paths.is_empty());
    assert_eq!(acceptance.actor, execution.actor_id);
    // The push created the remote branch, so the shared ledger records it as
    // created at the bound endpoint, for this attempt.
    let integration = receipt.integration.as_ref().ok_or("integration")?;
    assert_eq!(integration.previous, None);
    assert_eq!(integration.integrated, head);
    let endpoint = provenant_records::custody::RefEndpoint::Remote {
        remote: integration.remote.clone(),
        push_url: integration.push_url.clone().ok_or("push URL")?,
    };
    let reference = provenant_confinement::repo::GitRefName::new(&integration.target)?;
    let ledger = provenant_confinement::worktrees::RefLedger::open(
        execution.state_root.as_path(),
        execution.project_id,
    )?;
    let owned = ledger
        .load(&endpoint, &reference)?
        .ok_or("pushed ref not recorded")?;
    assert_eq!(owned.origin, provenant_records::custody::RefOrigin::Created);
    assert_eq!(owned.attempt_id, Some(id));
    assert_eq!(owned.tip, head);
    assert_eq!(
        store.delivered(id, &call)?.map(|(_, receipt)| receipt),
        Some(receipt)
    );
    // Replaying the sealed receipt is not a recreation.
    deliver(true)?;
    assert_eq!(ledger.load(&endpoint, &reference)?, Some(owned));
    Ok(())
}

#[test]
fn settled_delivery_refuses_dirty_or_changed_head_and_replays_success()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::Occupancy,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    use provenant_core::{CancelToken, Clock, RootInputs};
    use provenant_os::clock::SystemClock;
    let fixture = PrepareFixture::new()?;
    let mut execution = tests::context(&fixture.root)?;
    let mut inputs = RootInputs::new(fixture.root.join("home"), fixture.root.join("checkout"));
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    execution.project_id = provenant_confinement::worktrees::with_context(
        &roots,
        &fixture.root.join("home"),
        &fixture.writer.project,
        &clock,
        &cancel,
        |context| {
            let context = provenant_confinement::worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                attempt_id: None,
                operation_id: None,
                branch: "lane".into(),
                base: String::new(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: clock
                    .wall_now()
                    .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput)?,
                origin: provenant_confinement::worktrees::WorktreeOrigin::Adopted,
                label: None,
                reference: None,
            };
            let head = provenant_confinement::run_utility(
                context.executor,
                provenant_confinement::GitRead::RevParseHead,
                context.repo,
                context.utility,
                context.utility_budget,
                &cancel,
            )?;
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                base: String::from_utf8_lossy(&head.stdout.bytes).trim().into(),
                ..owner
            };
            provenant_confinement::worktrees::save_owner(
                &provenant_confinement::repo::verify_worktree(
                    context.repo,
                    &fixture.writer.worktree,
                )?,
                &owner,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.task.writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempt_id;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
        })?,
        RECORD_LIMIT,
    )?;
    let exit = tests::exit();
    tests::seal_observation(&store, &input, &exit)?;
    let lock = attempt.lock(
        std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
        provenant_os::fs::LockMode::Exclusive,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    drop(lock);
    store.publish_terminal(id, 1, &tests::publication(), &exit, &call)?;
    let deliver = || {
        store.deliver(
            id,
            Some(fixture.request.clone()),
            HostDeliveryContext::new(fixture.environment.clone()),
            false,
            &call,
        )
    };
    std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
    assert!(deliver().is_err());
    std::fs::remove_file(fixture.writer.worktree.join("dirty"))?;
    fixture.commit(&fixture.writer.worktree, "moved after receipt")?;
    assert!(matches!(
        deliver(),
        Err(RunError::Validation(
            "worktree head differs from the settled receipt"
        ))
    ));
    fixture.lane_git(&["reset", "--hard", &head.to_string()])?;
    let key = provenant_core::hex(
        provenant_records::CanonicalV1::file_digest(&serde_json::to_vec(&(
            head,
            &fixture.request,
        ))?)
        .bytes(),
    );
    // Simulate a crash after the current completion seal but before the
    // mutable pickup pointer was published. Delivery should replay this seal.
    artifacts::seal(
        &attempt,
        &format!("delivery-complete-{key}.json"),
        &db::bytes(&WriterDeliveryReceipt {
            extra: Default::default(),
            schema_version: 1,
            remote: fixture.request.remote.clone(),
            branch: fixture.request.branch.clone(),
            head: Some(head),
            review_acceptance: None,
            state: WriterDeliveryState::Delivered {
                extra: Default::default(),
                pull_request: None,
            },
            integration: None,
        })?,
        RECORD_LIMIT,
    )?;
    let receipt = deliver()?;
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered { .. }
    ));
    assert_eq!(
        store.delivered(id, &call)?.map(|(_, delivered)| delivered),
        Some(receipt.clone())
    );
    assert_eq!(deliver()?, receipt);
    attempt.unlink(std::ffi::OsStr::new("delivery-pickup.json"))?;
    assert_eq!(deliver()?, receipt);
    assert_eq!(
        store.delivered(id, &call)?.map(|(_, delivered)| delivered),
        Some(receipt.clone())
    );
    store.record_worktree_settlement(
        id,
        &serde_json::json!({"schema_version":1,"state":"pruned"}),
        &call,
    )?;
    store.record_worktree_settlement(
        id,
        &serde_json::json!({"schema_version":1,"state":"retained","reason":"racing observer"}),
        &call,
    )?;
    assert_eq!(
        store.worktree_settlement(id, &call)?.ok_or("settlement")?["state"],
        "pruned"
    );
    // Sealed successful effects replay even when the checkout has since moved.
    fixture.commit(&fixture.writer.worktree, "moved after delivery")?;
    assert_eq!(deliver()?, receipt);
    fixture.git(&[
        "worktree",
        "remove",
        "--force",
        fixture.writer.worktree.to_str().ok_or("lane")?,
    ])?;
    assert_eq!(deliver()?, receipt);
    Ok(())
}

#[test]
fn terminal_settles_instruction_file_edits() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let fixture = PrepareFixture::new()?;
    assert!(!fixture.terminal(|| {
        for name in ["AGENTS.md", "CLAUDE.md"] {
            std::fs::write(
                fixture.writer.worktree.join(name),
                format!("writer {name}\n"),
            )?;
        }
        fixture.lane_git(&["add", "AGENTS.md", "CLAUDE.md"])?;
        fixture.commit(&fixture.writer.worktree, "Edit instructions")
    })?);
    for name in ["AGENTS.md", "CLAUDE.md"] {
        assert_eq!(
            std::fs::read_to_string(fixture.writer.worktree.join(name))?,
            format!("writer {name}\n")
        );
        fixture.lane_git(&["cat-file", "-e", &format!("HEAD:{name}")])?;
    }
    Ok(())
}

#[test]
fn terminal_leaves_uncommitted_instruction_edits_in_place()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    std::fs::write(fixture.writer.worktree.join("AGENTS.md"), "baseline\n")?;
    fixture.lane_git(&["add", "AGENTS.md"])?;
    fixture.commit(&fixture.writer.worktree, "Baseline")?;
    assert!(!fixture.terminal(|| {
        std::fs::write(
            fixture.writer.worktree.join("AGENTS.md"),
            "uncommitted edit\n",
        )?;
        Ok(())
    })?);
    assert_eq!(
        std::fs::read_to_string(fixture.writer.worktree.join("AGENTS.md"))?,
        "uncommitted edit\n"
    );
    Ok(())
}

#[test]
fn terminal_ignores_protected_changes_present_at_lane_start()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    std::fs::create_dir(fixture.writer.worktree.join(".github"))?;
    std::fs::write(
        fixture.writer.worktree.join(".github/ci.yml"),
        "PR policy\n",
    )?;
    fixture.lane_git(&["add", ".github"])?;
    fixture.commit(&fixture.writer.worktree, "Existing PR policy")?;
    assert!(!fixture.terminal(|| {
        std::fs::write(
            fixture.writer.worktree.join("source.txt"),
            "writer source\n",
        )?;
        fixture.lane_git(&["add", "source.txt"])?;
        fixture.commit(&fixture.writer.worktree, "Writer source")
    })?);
    Ok(())
}

#[test]
fn terminal_imports_a_stopped_merge_into_the_worktree()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    stopped_merge_is_restored(false)
}

#[test]
fn budget_stop_preserves_a_stopped_merge_without_committing_conflict_markers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    stopped_merge_is_restored(true)
}

#[test]
fn terminal_names_a_writer_left_off_the_lane_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let error = fixture
        .terminal(|| {
            fixture.lane_git(&["checkout", "--detach"])?;
            fixture.commit(&fixture.writer.worktree, "Detached work")
        })
        .expect_err("detached writer HEAD");
    assert!(matches!(
        error.downcast_ref::<RunError>(),
        Some(RunError::WriterHeadNotLaneBranch)
    ));
    Ok(())
}

#[test]
fn prepare_labels_unsupported_ssh_command() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture
        .environment
        .push(("GIT_SSH_COMMAND".into(), "ssh -v".into()));
    let reason = fixture
        .prepare()
        .expect_err("unsupported SSH command")
        .to_string();
    for part in ["Prepare", "environment", "GIT_SSH_COMMAND"] {
        assert!(reason.contains(part), "{reason}");
    }
    Ok(())
}

#[test]
fn delivery_diagnostics_redact_url_userinfo() {
    for (input, expected) in [
        ("https://u:pa/ss@h/repo", "https://[REDACTED]@h/repo"),
        ("https://u:pa?ss@h/repo", "https://[REDACTED]@h/repo"),
        ("https://u:a@b@h/repo", "https://[REDACTED]@h/repo"),
        ("u:pa/ss@h:repo", "[REDACTED]@h:repo"),
        ("u:pa?ss@h:repo", "[REDACTED]@h:repo"),
        ("u:a@b@h:repo", "[REDACTED]@h:repo"),
        ("https://truncated-secret/path", "https://[REDACTED]/path"),
    ] {
        assert_eq!(
            provenant_confinement::redact::delivery_diagnostics(input),
            expected
        );
        let error = super::delivery::prepare_error("test", input).to_string();
        assert!(error.ends_with(expected), "{error}");
    }
}

#[test]
fn prepare_refuses_remote_urls_with_embedded_credentials()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    for url in [
        "https://user:hunter2@example.invalid/repo.git",
        "https://hunter2@example.invalid/repo.git",
        "ssh://user:hunter2@example.invalid/repo.git",
        "https://example.invalid/repo.git?token=hunter2",
        "https://example.invalid/repo.git#hunter2",
    ] {
        fixture.git(&["remote", "set-url", "origin", url])?;
        let error = fixture.prepare().expect_err("credential-bearing URL");
        assert!(
            matches!(
                error,
                RunError::DeliveryPrepare {
                    step: "remote URL",
                    ..
                }
            ),
            "{url}"
        );
        assert!(!format!("{error:?}").contains("hunter2"), "{url}");
        assert!(!error.to_string().contains("hunter2"), "{url}");
    }
    Ok(())
}

#[test]
fn prepare_labels_environment_path_failures() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = PrepareFixture::new()?;
    for name in ["HOME", "GH_CONFIG_DIR"] {
        for (path, detail) in [
            (fixture.root.join("missing/config"), "not found"),
            (
                fixture.writer.worktree.clone(),
                "inside the writer worktree",
            ),
        ] {
            fixture.environment.retain(|(key, _)| key != name);
            fixture
                .environment
                .insert(0, (name.into(), path.into_os_string()));
            let reason = fixture
                .prepare()
                .expect_err("invalid host path")
                .to_string();
            for part in ["Prepare", "environment", name, detail] {
                assert!(reason.contains(part), "{reason}");
            }
        }
        fixture.environment.retain(|(key, _)| key != name);
    }
    Ok(())
}

#[test]
fn prepare_labels_unavailable_gh_executable() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = PrepareFixture::new()?;
    fixture.request.open_pr = true;
    fixture.environment.retain(|(key, _)| key != "PATH");
    let reason = fixture.prepare().expect_err("PATH is unset").to_string();
    for part in ["Prepare", "gh executable", "gh is unavailable", "PATH"] {
        assert!(reason.contains(part), "{reason}");
    }
    fixture.environment.push(("PATH".into(), "".into()));
    let reason = fixture
        .prepare()
        .expect_err("gh is unavailable")
        .to_string();
    for part in ["Prepare", "gh executable", "gh is unavailable", "PATH"] {
        assert!(reason.contains(part), "{reason}");
    }
    Ok(())
}

#[test]
fn prepare_labels_git_executable_inside_worktree()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let git = fixture.writer.worktree.join("git");
    fixture.executable(&git, "#!/bin/sh\nexit 0\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), git.into_os_string()));
    let reason = fixture
        .prepare()
        .expect_err("unsafe git executable")
        .to_string();
    for part in ["Prepare", "git executable", "inside the writer worktree"] {
        assert!(reason.contains(part), "{reason}");
    }
    Ok(())
}

#[test]
fn prepare_accepts_absolute_shell_helper_outside_path()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let helper = fixture.root.join("gh");
    fixture.executable(&helper, "#!/bin/sh\nexit 0\n")?;
    fixture.git(&[
        "config",
        "credential.helper",
        &format!("!{} auth git-credential", helper.display()),
    ])?;
    fixture.prepare()?;
    Ok(())
}

#[test]
fn prepare_failures_name_step_and_bound_git_stderr()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for (pattern, step) in [
        ("remote get-url", "remote URL"),
        ("symbolic-ref --quiet", "HEAD"),
        ("rev-parse --absolute-git-dir", "git dir"),
        ("config --get-regexp", "credential helper"),
        ("fetch --no-auto-maintenance", "fetch"),
    ] {
        let mut fixture = PrepareFixture::new()?;
        let git = fixture.root.join("git");
        let real_git = provenant_os::git::git_executable().ok_or("git executable")?;
        fixture.executable(&git, &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'{pattern}'*)\nprintf '%s' 'fixture git failure: private-token https://user:password@example.invalid/ https://secret-user@example.invalid/ scp-secret@example.invalid:repo https://malformed-secret:password/ {}' >&2\nexit 2;;\nesac\nexec '{}' \"$@\"\n", "é".repeat(5000), real_git.display()))?;
        fixture
            .environment
            .push(("PROVENANT_GIT".into(), git.into_os_string()));
        fixture
            .environment
            .push(("GH_TOKEN".into(), "private-token".into()));
        // The remote must advertise a branch, or there is nothing to fetch.
        fixture.git(&["push", "-q", "origin", "HEAD:refs/heads/seed"])?;
        let reason = fixture.prepare().expect_err("git must fail").to_string();
        assert!(reason.contains("Prepare"), "{reason}");
        assert!(reason.contains(step), "{reason}");
        assert!(reason.contains("fixture git failure"), "{reason}");
        assert!(!reason.contains("private-token"), "{reason}");
        assert!(!reason.contains("user:password"), "{reason}");
        assert!(!reason.contains("secret-user"), "{reason}");
        assert!(!reason.contains("scp-secret"), "{reason}");
        assert!(!reason.contains("malformed-secret"), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
        assert!(reason.len() < 1400, "diagnostic has {} bytes", reason.len());
    }
    Ok(())
}

#[test]
fn many_linked_worktrees_do_not_block_protection_checks()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    // The fixture already has one real linked worktree. Add Git's administrative
    // entries directly to exercise a long listing without 201 extra Git spawns.
    let admin = fixture.writer.project.join(".git/worktrees");
    let head = std::fs::read(admin.join("lane/HEAD"))?;
    for index in 0..201 {
        let path = fixture.root.join(format!("tree-{index}"));
        let entry = admin.join(format!("fixture-{index}"));
        std::fs::create_dir(&entry)?;
        std::fs::write(entry.join("HEAD"), &head)?;
        std::fs::write(entry.join("commondir"), b"../..\n")?;
        std::fs::write(
            entry.join("gitdir"),
            format!("{}\n", path.join(".git").display()),
        )?;
    }
    let project = &fixture.writer.project;
    let git = provenant_os::git::git_executable().ok_or("git executable")?;
    let argv = [
        git.into_os_string(),
        "worktree".into(),
        "list".into(),
        "--porcelain".into(),
    ];
    let mut options = ExecOptions::new(&argv, project, Duration::from_secs(5));
    options.env = Some(&fixture.environment);
    let listing = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(listing.termination, Termination::Exit(0));
    assert!(listing.stdout.bytes.len() > 16 * 1024);
    HostDeliveryContext::validate_integration_target(project, "feature/unprotected")?;
    HostDeliveryContext::check_delivery_branch(project, "feature/unprotected")?;
    Ok(())
}

#[test]
fn worktree_listing_over_the_capture_default_is_refused_at_every_stage()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let limit = provenant_os::exec::DEFAULT_CAPTURE_LIMIT;
    let (fixture, host) = oversize_host("worktree list", limit + 1)?;
    for stage in [WriterDeliveryStage::Prepare, WriterDeliveryStage::Integrate] {
        let error = host
            .command(
                &fixture.writer.project,
                "git",
                &["worktree", "list", "--porcelain"],
                stage,
            )
            .expect_err("an oversized listing must be refused");
        let RunError::DeliveryLimit {
            stage: actual_stage,
            tool,
            command,
            limit: actual_limit,
        } = &error
        else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(*actual_stage, stage);
        assert_eq!(*tool, "git");
        assert_eq!(command, "worktree list");
        assert_eq!(*actual_limit, limit);
        let projection = error.diagnostic(provenant_records::diagnostic::Destination::Local);
        assert_eq!(projection.code, "IO");
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("worktree list")
        );
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains(&limit.to_string())
        );
        assert!(matches!(
            projection.fact,
            Some(provenant_records::diagnostic::DiagnosticFact::DeliveryLimit(_))
        ));
    }
    Ok(())
}

#[test]
fn delivery_preflights_reuse_the_command_worktree_listing_with_many_worktrees()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let admin = fixture.writer.project.join(".git/worktrees");
    let head = std::fs::read(admin.join("lane/HEAD"))?;
    for index in 0..100 {
        let path = fixture.root.join(format!("preflight-tree-{index}"));
        let entry = admin.join(format!("preflight-{index}"));
        std::fs::create_dir(&entry)?;
        std::fs::write(entry.join("HEAD"), &head)?;
        std::fs::write(entry.join("commondir"), b"../..\n")?;
        std::fs::write(
            entry.join("gitdir"),
            format!("{}\n", path.join(".git").display()),
        )?;
    }

    let git = provenant_os::git::git_executable().ok_or("git executable")?;
    let wrapper = fixture.root.join("counting-git");
    let count = fixture.root.join("worktree-list-count");
    fixture.executable(
        &wrapper,
        &format!(
            "#!/bin/sh\ncase \" $* \" in *\" worktree list --porcelain \"*) printf x >> '{}';; esac\nexec '{}' \"$@\"\n",
            count.display(),
            git.display()
        ),
    )?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    let facts = provenant_confinement::worktrees::RepoFacts::default();
    let host = HostDeliveryContext::new(fixture.environment.clone()).with_repo_facts(&facts);

    // The CLI's owned-worktree and task preflights share these command facts.
    host.check_delivery_branch_with_facts(&fixture.writer.project, "unprotected")?;
    host.check_delivery_branch_with_facts(&fixture.writer.project, "unprotected")?;

    assert_eq!(
        std::fs::read(&count)?.len(),
        1,
        "worktree list should run once"
    );
    Ok(())
}

#[test]
fn non_listing_host_output_over_its_bound_names_the_command()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (fixture, host) = oversize_host("oversize-probe", 16 * 1024 + 1)?;
    let error = host
        .command(
            &fixture.writer.project,
            "git",
            &["symbolic-ref", "--quiet", "refs/heads/oversize-probe"],
            WriterDeliveryStage::Prepare,
        )
        .expect_err("oversized output must be refused");
    let message = error.to_string();
    assert!(message.contains("symbolic-ref"), "{message}");
    assert!(message.contains("16384"), "{message}");
    Ok(())
}

#[test]
fn host_forge_output_over_its_bound_names_gh_not_git()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.request.open_pr = true;
    let bin = fixture.root.join("gh-bin");
    std::fs::create_dir(&bin)?;
    let payload = fixture.root.join("gh-output");
    std::fs::write(&payload, vec![b'a'; 16 * 1024 + 1])?;
    fixture.executable(
        &bin.join("gh"),
        &format!("#!/bin/sh\ncat '{}'\n", payload.display()),
    )?;
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture.environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    // Preparing for a pull request needs a hosted remote URL.
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    fixture.executable(&wrapper, &format!("#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n", git.display()))?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    let host = fixture.prepare()?;
    let error = host
        .command(
            &fixture.writer.project,
            "gh",
            &["pr", "view", "1"],
            WriterDeliveryStage::PullRequest,
        )
        .expect_err("oversized forge output must be refused");
    let RunError::DeliveryLimit {
        tool: "gh", limit, ..
    } = &error
    else {
        panic!("unexpected error: {error:?}");
    };
    let message = error.to_string();
    assert!(message.contains("gh pr output"), "{message}");
    assert!(message.contains(&limit.to_string()), "{message}");
    Ok(())
}
