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
    let credential_url = format!(
        "file://user:secret@localhost{}",
        fixture.root.join("remote.git").display()
    );
    fixture.git(&["remote", "set-url", "origin", &credential_url])?;
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
    let host = HostDeliveryContext::new(fixture.environment.clone())
        .with_host_state(&fixture.root.join("state"))
        .prepare(&fixture.writer, &fixture.request)?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
            stop_work: None,
            warnings: Vec::new(),
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
        remote: integration
            .remote
            .remote_name()
            .ok_or("local integration has no remote")?
            .as_str()
            .to_owned(),
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
    for record in [
        serde_json::to_string(&receipt)?,
        serde_json::to_string(&owned)?,
    ] {
        assert!(!record.contains("user:secret@"));
    }
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
            stop_work: None,
            warnings: Vec::new(),
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
fn committed_tool_state_paths_warn_and_settle()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    crate::writer::TOOL_STATE_WARNINGS.with(|warnings| warnings.borrow_mut().clear());
    assert!(!fixture.terminal(|| {
        let path = fixture.writer.worktree.join(".local/state/gh/device-id");
        std::fs::create_dir_all(path.parent().ok_or("tool-state parent")?)?;
        std::fs::write(path, "device\n")?;
        fixture.lane_git(&["add", ".local"])?;
        fixture.commit(&fixture.writer.worktree, "Commit tool state")
    })?);
    let warnings = crate::writer::TOOL_STATE_WARNINGS.with(|warnings| warnings.borrow().clone());
    assert!(
        warnings
            .iter()
            .any(|line| line.contains("LANE_TOOL_STATE_COMMITTED")),
        "expected a tool-state policy warning, got {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|line| line.contains(".local/state/gh/device-id")),
        "warning must name the committed path, got {warnings:?}"
    );
    Ok(())
}

#[test]
fn ordinary_dot_paths_do_not_warn() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    crate::writer::TOOL_STATE_WARNINGS.with(|warnings| warnings.borrow_mut().clear());
    assert!(!fixture.terminal(|| {
        let github = fixture.writer.worktree.join(".github/x");
        std::fs::create_dir_all(github.parent().ok_or(".github parent")?)?;
        std::fs::write(github, "ci\n")?;
        let config = fixture.writer.worktree.join("src/.config/settings");
        std::fs::create_dir_all(config.parent().ok_or("src/.config parent")?)?;
        std::fs::write(config, "settings\n")?;
        fixture.lane_git(&["add", ".github", "src"])?;
        fixture.commit(&fixture.writer.worktree, "Ordinary dot paths")
    })?);
    let warnings = crate::writer::TOOL_STATE_WARNINGS.with(|warnings| warnings.borrow().clone());
    assert!(warnings.is_empty(), "unexpected warning: {warnings:?}");
    Ok(())
}

#[test]
fn terminal_retains_a_stopped_merge_in_private_git()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    stopped_merge_stays_private(false)
}

#[test]
fn budget_stop_preserves_a_stopped_merge_without_committing_conflict_markers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    stopped_merge_stays_private(true)
}

/// Prepare the lane's candidate Git view as a provider launch does, so a test
/// can leave an unfinished operation in it.
fn prepare_candidate_git(
    fixture: &PrepareFixture,
) -> std::result::Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime};
    use provenant_core::CancelToken;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let (attempt_path, temporary) = fixture.attempt_layout()?;
    let scratch = temporary.join("candidate-0");
    std::fs::create_dir_all(&scratch)?;
    let utility = UtilityRuntime::new(&git, fixture.root.join("home"), fixture.root.join("state"))?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let repo = utility.open_repo(
        &executor,
        &fixture.writer.project,
        UtilityBudget::bounded(Duration::from_secs(5))?,
        &cancel,
    )?;
    let worktree = provenant_confinement::verify_worktree(&repo, &fixture.writer.worktree)?;
    let base = crate::objects::resolve_ref(&git, worktree.repo(), "refs/heads/lane")?;
    crate::objects::prepare_bound(
        &git,
        worktree.repo(),
        &Directory::open_anchor(&attempt_path)?,
        &scratch,
        "refs/heads/lane",
        base,
    )?;
    Ok(scratch)
}

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn terminal_warns_and_publishes_a_cherry_pick_in_the_candidate_git()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    std::fs::write(fixture.writer.project.join("picked.txt"), "picked\n")?;
    fixture.git(&["add", "picked.txt"])?;
    fixture.commit(&fixture.writer.project, "picked")?;
    let picked = fixture.git_text(&["rev-parse", "HEAD"])?;
    crate::writer::GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow_mut().clear());
    assert!(!fixture.terminal(|| {
        let scratch = prepare_candidate_git(&fixture)?;
        // Spec: docs/specs/run-settlement.md#settlement -- git-cherry-pick(1)
        // does not set CHERRY_PICK_HEAD under --no-commit, so write the
        // resolved-but-uncommitted state the owner rule covers directly.
        std::fs::write(
            scratch.join("git-common").join("CHERRY_PICK_HEAD"),
            format!("{picked}\n"),
        )?;
        Ok(())
    })?);
    let candidate = fixture.attempt_layout()?.1.join("candidate-0/git-common");
    assert!(
        candidate.join("CHERRY_PICK_HEAD").is_file(),
        "the in-progress cherry-pick must be left in place"
    );
    let tip = fixture.git_text(&["rev-parse", "refs/heads/lane"])?;
    let warnings = crate::writer::GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow().clone());
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("cherry-pick")
                && warning.contains(&tip)
                && warning.contains("git-common")),
        "{warnings:?}"
    );
    Ok(())
}

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn terminal_warns_and_publishes_a_rebase_in_the_lane_git()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    crate::writer::GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow_mut().clear());
    let marker = fixture
        .writer
        .project
        .join(".git/worktrees/lane/rebase-merge");
    assert!(!fixture.terminal(|| {
        std::fs::create_dir_all(&marker)?;
        Ok(())
    })?);
    assert!(
        marker.is_dir(),
        "the in-progress rebase must be left in place"
    );
    let tip = fixture.git_text(&["rev-parse", "refs/heads/lane"])?;
    let warnings = crate::writer::GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow().clone());
    assert!(
        warnings.iter().any(|warning| warning.contains("rebase")
            && warning.contains(&tip)
            && warning.contains("worktrees/lane")),
        "{warnings:?}"
    );
    Ok(())
}

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn terminal_refuses_before_import_when_the_host_checkout_is_mid_operation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let marker = fixture.writer.project.join(".git/rebase-merge");
    std::fs::create_dir_all(&marker)?;
    // An operation on the lane branch is one settlement moves, so it must
    // refuse before import with the checkout's bytes unchanged.
    std::fs::write(marker.join("head-name"), "refs/heads/lane\n")?;
    let checkout = fixture.writer.worktree.join("AGENTS.md");
    let before = std::fs::read(&checkout).ok();
    let error = fixture
        .terminal(|| Ok(()))
        .expect_err("mid-operation host checkout");
    assert!(
        matches!(
            error.downcast_ref::<RunError>(),
            Some(RunError::Writer(
                provenant_confinement::worktrees::WorktreeError::GitOperationInProgress
            ))
        ),
        "{error:?}"
    );
    assert!(
        !fixture
            .attempt_layout()?
            .0
            .join("writer-terminal.json")
            .exists(),
        "the refusal must precede import and publication"
    );
    assert_eq!(std::fs::read(&checkout).ok(), before);
    assert!(
        marker.is_dir(),
        "the host checkout operation is left intact"
    );
    Ok(())
}

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn terminal_warns_and_proceeds_for_an_unrelated_host_checkout_rebase()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let marker = fixture.writer.project.join(".git/rebase-merge");
    std::fs::create_dir_all(&marker)?;
    std::fs::write(marker.join("head-name"), "refs/heads/side\n")?;
    crate::writer::GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow_mut().clear());
    let (settled, captured) = provenant_records::warning::capture(|| fixture.terminal(|| Ok(())));
    assert!(
        !settled?,
        "an owner rebase on an unrelated branch must not refuse a linked lane"
    );
    assert!(marker.is_dir(), "the host operation is left intact");
    assert!(
        captured.iter().any(|warning| {
            warning.code.as_ref() == "GIT_OPERATION_RETAINED"
                && warning.message.contains("refs/heads/side")
        }),
        "{captured:?}"
    );
    let attempt = provenant_os::fs::Directory::open_anchor(&fixture.attempt_layout()?.0)?;
    let raw = crate::artifacts::read_sealed(&attempt, "writer-terminal.json", crate::RECORD_LIMIT)?
        .ok_or("settled writer terminal record")?;
    let terminal: crate::writer::WriterTerminal = crate::db::decode(&raw)?;
    assert!(
        terminal
            .warnings
            .iter()
            .any(|warning| warning.contains("refs/heads/side")),
        "{:?}",
        terminal.warnings
    );
    Ok(())
}

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn terminal_refuses_when_the_host_checkout_rebases_the_delivery_destination()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let marker = fixture.writer.project.join(".git/rebase-merge");
    std::fs::create_dir_all(&marker)?;
    std::fs::write(marker.join("head-name"), "refs/heads/published/lane\n")?;
    let error = fixture
        .terminal(|| Ok(()))
        .expect_err("host rebases the delivery destination");
    assert!(
        matches!(
            error.downcast_ref::<RunError>(),
            Some(RunError::Writer(
                provenant_confinement::worktrees::WorktreeError::GitOperationInProgress
            ))
        ),
        "{error:?}"
    );
    assert!(marker.is_dir(), "the host operation is left intact");
    Ok(())
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
fn terminal_refuses_a_conflicted_merge_off_the_lane_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let merge_head = fixture
        .writer
        .project
        .join(".git/worktrees/lane/MERGE_HEAD");
    let error = fixture
        .terminal(|| {
            fixture.lane_git(&["checkout", "-qb", "other"])?;
            let head = fixture.git_text(&["rev-parse", "HEAD"])?;
            std::fs::write(&merge_head, format!("{head}\n"))?;
            Ok(())
        })
        .expect_err("an operation must not hide a writer off the lane branch");
    assert!(matches!(
        error.downcast_ref::<RunError>(),
        Some(RunError::WriterHeadNotLaneBranch)
    ));
    assert!(merge_head.is_file(), "the conflicted merge is left intact");
    Ok(())
}

#[test]
fn terminal_allows_a_detached_head_only_under_a_lane_rebase()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let marker = fixture
        .writer
        .project
        .join(".git/worktrees/lane/rebase-merge");
    crate::writer::GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow_mut().clear());
    let settled = fixture.terminal(|| {
        fixture.lane_git(&["checkout", "-q", "--detach"])?;
        std::fs::create_dir_all(&marker)?;
        std::fs::write(marker.join("head-name"), "refs/heads/lane\n")?;
        Ok(())
    })?;
    assert!(!settled);
    assert!(marker.is_dir(), "the lane rebase is left intact");
    let warnings = crate::writer::GIT_OPERATION_WARNINGS.with(|warnings| warnings.borrow().clone());
    assert!(
        warnings.iter().any(|warning| warning.contains("rebase")),
        "{warnings:?}"
    );
    Ok(())
}

#[test]
fn prepare_labels_unsupported_ssh_command() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture
        .environment
        .push(("GIT_SSH_COMMAND".into(), "ssh -v".into()));
    assert!(matches!(
        fixture.prepare(),
        Err(RunError::DeliveryPrepare {
            step: DeliveryPrepareStep::Environment,
            refusal: None,
            ..
        })
    ));
    Ok(())
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
                    step: DeliveryPrepareStep::RemoteUrl,
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
fn prepare_seals_the_builtin_forge_executable_only_when_selected()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.request.open_pr = true;
    fixture.environment.retain(|(key, _)| key != "PATH");
    assert!(fixture.prepare().is_err());
    Ok(())
}

#[test]
fn forge_none_skips_builtin_gh_even_when_it_is_available()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.request.open_pr = true;
    let marker = fixture.root.join("gh-called");
    let gh = fixture.root.join("gh");
    fixture.executable(
        &gh,
        &format!("#!/bin/sh\n: > '{}'\nexit 0\n", marker.display()),
    )?;
    // Opening a pull request needs a hosted push URL, whatever forge is chosen.
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    fixture.executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n",
            git.display()
        ),
    )?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture
        .environment
        .insert(0, ("PATH".into(), fixture.root.as_os_str().to_owned()));
    let config = fixture.writer.project.join(".provenant");
    std::fs::create_dir_all(&config)?;
    std::fs::write(
        config.join("ports.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": provenant_config::PORTS_SCHEMA_VERSION,
            "forge": {"source": "project", "backend": {"kind": "none"}}
        }))?,
    )?;

    let host = fixture.prepare()?;
    let result = host.command(
        &fixture.writer.project,
        "gh",
        &["pr", "list"],
        WriterDeliveryStage::PullRequest,
    );
    assert!(result.is_err());
    assert!(
        !marker.exists(),
        "none must not invoke the builtin forge adapter"
    );
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
    enum Expected {
        Prepare(DeliveryPrepareStep),
        TransportConfigRead,
    }
    for (pattern, expected) in [
        (
            "remote get-url",
            Expected::Prepare(DeliveryPrepareStep::RemoteUrl),
        ),
        (
            "symbolic-ref --quiet",
            Expected::Prepare(DeliveryPrepareStep::Head),
        ),
        (
            "rev-parse --absolute-git-dir",
            Expected::Prepare(DeliveryPrepareStep::GitDir),
        ),
        // Reading the helper settings is a named config read now: a failure is
        // a transport-config refusal naming the operation, not a prepare step.
        ("config --get-regexp", Expected::TransportConfigRead),
        (
            "fetch --no-auto-maintenance",
            Expected::Prepare(DeliveryPrepareStep::Fetch),
        ),
    ] {
        let mut fixture = PrepareFixture::new()?;
        let git = fixture.root.join("git");
        let real_git = provenant_os::git::git_executable().ok_or("git executable")?;
        // Git's final error is last, so the padding comes first: the kept tail
        // is the failure text and the dropped head is only padding.
        fixture.executable(&git, &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'{pattern}'*)\nprintf '%s' '{}fixture git failure: private-token https://user:password@example.invalid/ https://secret-user@example.invalid/ scp-secret@example.invalid:repo https://malformed-secret:password/ fixture tail marker' >&2\nexit 2;;\nesac\nexec '{}' \"$@\"\n", "é".repeat(5000), real_git.display()))?;
        fixture
            .environment
            .push(("PROVENANT_GIT".into(), git.into_os_string()));
        fixture
            .environment
            .push(("GH_TOKEN".into(), "private-token".into()));
        // The remote must advertise a branch, or there is nothing to fetch.
        fixture.git(&["push", "-q", "origin", "HEAD:refs/heads/seed"])?;
        let error = fixture.prepare().expect_err("git must fail");
        let prepare = matches!(expected, Expected::Prepare(_));
        let reason = match error {
            RunError::DeliveryPrepare {
                step: actual_step,
                reason,
                ..
            } => {
                let Expected::Prepare(step) = expected else {
                    panic!("unexpected prepare failure for {pattern}: {actual_step}");
                };
                assert_eq!(actual_step, step);
                reason
            }
            RunError::DeliveryTransport {
                stage: provenant_records::run::WriterDeliveryStage::Prepare,
                kind: TransportRefusal::Unreadable,
                reason,
            } => {
                assert!(
                    matches!(expected, Expected::TransportConfigRead),
                    "unexpected transport refusal for {pattern}: {reason}"
                );
                assert!(
                    reason.contains("read transport helper settings"),
                    "{reason}"
                );
                reason
            }
            other => panic!("unexpected failure class: {other:?}"),
        };
        if prepare {
            // Git's final error is what the tail keeps, and the owner chose raw
            // stderr in receipts, so the secret-shaped text is reported as is.
            assert!(reason.contains("fixture tail marker"), "{reason}");
            assert!(reason.contains("private-token"), "{reason}");
            assert!(
                reason.contains("https://user:password@example.invalid/"),
                "{reason}"
            );
            assert!(
                reason.contains(PREPARE_REASON_TRUNCATION_MARKER),
                "a long stderr is bounded with a marker: {reason}"
            );
            // The cut lands on a character boundary, never inside the padding.
            assert!(!reason.contains('\u{FFFD}'), "{reason}");
        }
        assert!(!reason.contains("[REDACTED]"), "{reason}");
        assert!(reason.len() <= PREPARE_REASON_LIMIT + PREPARE_REASON_TRUNCATION_MARKER.len());
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
    assert!(listing.stdout.bytes.len() > host_output_limit(HostOutputClass::Ordinary));
    HostDeliveryContext::validate_integration_target(
        project,
        "feature/unprotected",
        &crate::forge_for("origin", "main", &[]),
    )?;
    HostDeliveryContext::check_delivery_branch(
        project,
        "feature/unprotected",
        &crate::forge_for("origin", "main", &[]),
    )?;
    Ok(())
}

#[test]
fn worktree_listing_over_the_capture_default_is_refused_at_every_stage()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let limit = host_output_limit(HostOutputClass::FactCapture);
    let args = ["worktree", "list", "--porcelain"];
    let (fixture, host) = oversize_host("worktree list", limit + 1)?;
    for stage in [WriterDeliveryStage::Prepare, WriterDeliveryStage::Integrate] {
        let error = host
            .command(&fixture.writer.project, "git", &args, stage)
            .expect_err("an oversized listing must be refused");
        let RunError::DeliveryLimit {
            stage: actual_stage,
            tool,
            limit: actual_limit,
            ..
        } = &error
        else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(*actual_stage, stage);
        assert_eq!(*tool, "git");
        assert_eq!(*actual_limit, limit);
        let projection = error.diagnostic(provenant_records::diagnostic::Destination::Local);
        assert_eq!(projection.code, "IO");
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
    host.check_delivery_branch_with_facts(
        &fixture.writer.project,
        "unprotected",
        &crate::forge_for("origin", "main", &[]),
    )?;
    host.check_delivery_branch_with_facts(
        &fixture.writer.project,
        "unprotected",
        &crate::forge_for("origin", "main", &[]),
    )?;

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
    let limit = host_output_limit(HostOutputClass::Ordinary);
    let args = ["symbolic-ref", "--quiet", "refs/heads/oversize-probe"];
    let (fixture, host) = oversize_host("oversize-probe", limit + 1)?;
    let error = host
        .command(
            &fixture.writer.project,
            "git",
            &args,
            WriterDeliveryStage::Prepare,
        )
        .expect_err("oversized output must be refused");
    assert!(matches!(
        error,
        RunError::DeliveryLimit {
            stage: WriterDeliveryStage::Prepare,
            tool: "git",
            command,
            limit: actual_limit,
        } if command == git_subcommand(&args) && actual_limit == limit
    ));
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
    std::fs::write(
        &payload,
        vec![b'a'; host_output_limit(HostOutputClass::Ordinary) + 1],
    )?;
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
    assert!(matches!(
        error,
        RunError::DeliveryLimit {
            stage: WriterDeliveryStage::PullRequest,
            tool: "gh",
            command,
            limit,
        } if command == git_subcommand(&["pr", "view", "1"])
            && limit == host_output_limit(HostOutputClass::Ordinary)
    ));
    Ok(())
}
