//! Host Git conformance with a linked worktree and a local bare remote.
use super::delivery::*;
use super::*;
use provenant_core::GitOid;
use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use std::{ffi::OsString, path::Path, time::Duration};

struct PrepareFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    environment: Vec<(OsString, OsString)>,
    writer: WriterRequest,
    request: WriterDeliveryRequest,
}

#[test]
fn integration_refuses_default_fallbacks_remote_head_and_primary_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "-m", "primary-live"])?;
    for target in [
        "main",
        "master",
        "primary-live",
        "origin/main",
        "origin/primary-live",
    ] {
        fixture.request =
            HostDeliveryContext::destination(DeliveryMode::Integrate(target.into()), "lane")?
                .ok_or("destination")?;
        assert!(
            matches!(
                fixture.prepare(),
                Err(RunError::DeliveryPrepare {
                    step: "integration target",
                    ..
                })
            ),
            "{target}"
        );
    }
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ])?;
    for target in ["trunk", "origin/trunk"] {
        fixture.request =
            HostDeliveryContext::destination(DeliveryMode::Integrate(target.into()), "lane")?
                .ok_or("destination")?;
        assert!(
            matches!(
                fixture.prepare(),
                Err(RunError::DeliveryPrepare {
                    step: "integration target",
                    ..
                })
            ),
            "{target}"
        );
    }
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/integration",
    ])?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    fixture.git(&[
        "symbolic-ref",
        "refs/heads/alias",
        "refs/heads/primary-live",
    ])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("alias".into()), "lane")?
            .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare(),
        Err(RunError::DeliveryPrepare {
            step: "integration target",
            ..
        })
    ));
    Ok(())
}

#[test]
fn graph_publication_never_adopts_a_foreign_local_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let publication = format!("provenant/graph/{head}");
    fixture.git(&["branch", &publication])?;
    assert!(
        host.graph_publication_branch(&fixture.writer.project, head)
            .is_err()
    );
    fixture.git(&["branch", "-d", &publication])?;
    fixture.git(&[
        "symbolic-ref",
        &format!("refs/heads/{publication}"),
        "refs/heads/missing",
    ])?;
    assert!(
        host.graph_publication_branch(&fixture.writer.project, head)
            .is_err()
    );
    fixture.git(&[
        "symbolic-ref",
        "--delete",
        &format!("refs/heads/{publication}"),
    ])?;
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    Ok(())
}

#[test]
fn delivery_records_dirty_and_checked_out_failures_after_start()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for dirty in [true, false] {
        let mut fixture = PrepareFixture::new()?;
        fixture.git(&["branch", "integration"])?;
        fixture.request = HostDeliveryContext::destination(
            DeliveryMode::Integrate("integration".into()),
            "lane",
        )?
        .ok_or("destination")?;
        let host = fixture.prepare()?;
        fixture.commit(&fixture.writer.worktree, "lane change")?;
        if dirty {
            std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
        } else {
            fixture.git(&[
                "worktree",
                "add",
                fixture.root.join("checked-out").to_str().ok_or("tree")?,
                "integration",
            ])?;
        }
        let path = fixture.root.join("attempt");
        std::fs::create_dir(&path)?;
        let attempt = provenant_os::fs::Directory::open_anchor(&path)?;
        let receipt = deliver_once(
            &host,
            &fixture.writer,
            &fixture.request,
            &attempt,
            "Run fixture\nAttempt fixture",
        )?;
        assert!(matches!(receipt.state, WriterDeliveryState::Failed { .. }));
        let raw =
            artifacts::read_sealed(&attempt, "delivery.json", RECORD_LIMIT)?.ok_or("delivery")?;
        assert_eq!(db::decode::<WriterDeliveryReceipt>(&raw)?, receipt);
        assert!(artifacts::read_sealed(&attempt, "delivery-started.json", RECORD_LIMIT)?.is_some());
    }
    Ok(())
}

#[test]
fn two_lanes_contend_for_integration_without_losing_either_head()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let first = fixture.prepare()?;
    let second_writer = WriterRequest {
        project: fixture.writer.project.clone(),
        worktree: fixture.root.join("second-lane"),
    };
    fixture.git(&[
        "worktree",
        "add",
        "-qb",
        "second-lane",
        second_writer.worktree.to_str().ok_or("tree")?,
    ])?;
    let second = HostDeliveryContext::new(fixture.environment.clone())
        .prepare(&second_writer, &fixture.request)?;
    fixture.commit(&fixture.writer.worktree, "first lane")?;
    fixture.commit(&second_writer.worktree, "second lane")?;
    let first_head = first.head(&fixture.writer)?;
    let second_head = second.head(&second_writer)?;
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            first.integrate(&fixture.writer, &fixture.request, first_head)
        });
        let two = scope.spawn(|| {
            barrier.wait();
            second.integrate(&second_writer, &fixture.request, second_head)
        });
        one.join()
            .expect("first lane thread")
            .expect("first integration");
        two.join()
            .expect("second lane thread")
            .expect("second integration");
    });
    for head in [first_head, second_head] {
        first.command(
            &fixture.writer.project,
            "git",
            &[
                "merge-base",
                "--is-ancestor",
                &head.to_string(),
                "integration",
            ],
            WriterDeliveryStage::Verify,
        )?;
    }
    Ok(())
}

#[test]
fn commit_pr_body_is_bounded_and_contains_commit_messages()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "Commit subject\n\nCommit body")?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert!(
        body.contains("Commit subject")
            && body.contains("Commit body")
            && body.contains("Attempt fixture")
    );
    let capped = cap_pr_body("\u{1f600}".repeat(30_000));
    assert!(capped.len() <= 60_000 && capped.ends_with("[Commit summary truncated.]\n"));
    fixture.commit(
        &fixture.writer.worktree,
        &format!("Large commit\n\n{}", "\u{1f600}".repeat(20_000)),
    )?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert!(body.len() <= 60_000 && body.contains("[Commit summary truncated.]"));
    Ok(())
}

#[test]
fn remote_integration_refreshes_the_leased_destination()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "remote lane")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    assert_eq!(
        host.command(
            &fixture.writer.worktree,
            "git",
            &["rev-parse", "refs/remotes/origin/integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(&fixture.writer, &fixture.request, head)?;
    Ok(())
}

#[test]
fn remote_integration_refuses_a_symbolic_destination()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/main"])?;
    fixture.git(&[
        "--git-dir",
        fixture.root.join("remote.git").to_str().ok_or("remote")?,
        "symbolic-ref",
        "refs/heads/integration",
        "refs/heads/main",
    ])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    let original = host.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    let remote = host.command(
        &fixture.writer.project,
        "git",
        &["ls-remote", "origin", "refs/heads/main"],
        WriterDeliveryStage::Verify,
    )?;
    assert_eq!(
        remote.split_whitespace().next(),
        Some(original.to_string().as_str())
    );
    Ok(())
}

#[test]
fn local_integration_fast_forwards_merges_and_replays()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane one")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(&fixture.writer, &fixture.request, head)?;
    // Divergent commits with a common tree still require a two-parent merge.
    fixture.commit(&fixture.writer.project, "integration side")?;
    fixture.git(&["branch", "-f", "integration", "HEAD"])?;
    fixture.commit(&fixture.writer.worktree, "lane two")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    let parents = host.command(
        &fixture.writer.project,
        "git",
        &["rev-list", "--parents", "-n", "1", "integration"],
        WriterDeliveryStage::Verify,
    )?;
    assert_eq!(parents.split_whitespace().count(), 3);
    assert!(parents.ends_with(&head.to_string()));
    Ok(())
}

#[test]
fn local_integration_conflict_dirty_and_checked_out_refuse_without_partial_state()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    std::fs::write(fixture.writer.project.join("conflict"), "integration")?;
    fixture.git(&["add", "conflict"])?;
    fixture.commit(&fixture.writer.project, "integration conflict")?;
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    let original = host.command(
        &fixture.writer.project,
        "git",
        &["rev-parse", "integration"],
        WriterDeliveryStage::Verify,
    )?;
    std::fs::write(fixture.writer.worktree.join("conflict"), "lane")?;
    fixture.lane_git(&["add", "conflict"])?;
    fixture.commit(&fixture.writer.worktree, "lane conflict")?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "integration"],
            WriterDeliveryStage::Verify
        )?,
        original
    );
    assert_eq!(host.head(&fixture.writer)?, head);
    assert!(!fixture.writer.project.join(".git/MERGE_HEAD").exists());
    std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
    assert!(
        host.integrate(&fixture.writer, &fixture.request, head)
            .is_err()
    );
    std::fs::remove_file(fixture.writer.worktree.join("dirty"))?;
    let primary = std::fs::read_to_string(fixture.writer.project.join(".git/HEAD"))?
        .trim()
        .trim_start_matches("ref: refs/heads/")
        .to_owned();
    let request = HostDeliveryContext::destination(DeliveryMode::Integrate(primary), "lane")?
        .ok_or("destination")?;
    assert!(matches!(
        HostDeliveryContext::new(fixture.environment.clone()).prepare(&fixture.writer, &request),
        Err(RunError::DeliveryPrepare {
            step: "integration target",
            ..
        })
    ));
    Ok(())
}

#[test]
fn concise_delivery_preserves_alias_contract_and_refuses_injection() -> Result<()> {
    assert!(HostDeliveryContext::destination(DeliveryMode::None, "")?.is_none());
    let push = HostDeliveryContext::destination(DeliveryMode::Push, "lane")?
        .ok_or(RunError::Validation("push"))?;
    assert_eq!(
        (&*push.remote, &*push.branch, push.open_pr),
        ("origin", "lane", false)
    );
    assert!(
        HostDeliveryContext::destination(DeliveryMode::Pr, "lane")?
            .is_some_and(|request| request.open_pr)
    );
    let remote = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or(RunError::Validation("integration"))?;
    assert_eq!(
        (&*remote.remote, &*remote.branch),
        ("origin", "integration")
    );
    for invalid in [
        "integrate:",
        "integrate:--all",
        "integrate:main..other",
        "integrate:refs/heads/../main",
        "pr:main",
    ] {
        assert!(invalid.parse::<DeliveryMode>().is_err(), "{invalid}");
    }
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
            provenant_confinement::worktrees::WorkerState,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
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
            provenant_confinement::worktrees::adopt(
                &context,
                &fixture.writer.worktree,
                &owner,
                &cancel,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    let call = Request {
        clock: &clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.tasks[0].writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
            protected_paths: vec![],
        })?,
        RECORD_LIMIT,
    )?;
    let exit = tests::exit();
    tests::seal_observation(&store, &input, &exit)?;
    let lock = attempt.lock(
        std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
        provenant_os::fs::LockMode::Exclusive,
        call.budget,
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
    artifacts::seal(
        &attempt,
        &format!("delivery-{key}.json"),
        &db::bytes(&WriterDeliveryReceipt {
            schema_version: 1,
            remote: fixture.request.remote.clone(),
            branch: fixture.request.branch.clone(),
            head: Some(head),
            state: WriterDeliveryState::Failed {
                stage: WriterDeliveryStage::Interrupted,
            },
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
impl PrepareFixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&home)?;
        let fixture = Self {
            _temp: temp,
            root: root.clone(),
            environment: vec![
                ("HOME".into(), home.into_os_string()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ],
            writer: WriterRequest {
                project,
                worktree: root.join("lane"),
            },
            request: WriterDeliveryRequest {
                mode: Default::default(),
                remote: "origin".into(),
                branch: "published/lane".into(),
                open_pr: false,
            },
        };
        fixture.git(&["init", "-q"])?;
        fixture.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Initial",
        ])?;
        let remote = root.join("remote.git");
        fixture.git(&["init", "--bare", "-q", remote.to_str().ok_or("remote")?])?;
        fixture.git(&["remote", "add", "origin", remote.to_str().ok_or("remote")?])?;
        fixture.git(&[
            "worktree",
            "add",
            "-qb",
            "lane",
            fixture.writer.worktree.to_str().ok_or("lane")?,
        ])?;
        Ok(fixture)
    }
    fn git(&self, args: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let git = provenant_os::git::git_executable().ok_or("git executable")?;
        let argv: Vec<OsString> = std::iter::once(git.into_os_string())
            .chain(args.iter().map(OsString::from))
            .collect();
        let mut options = ExecOptions::new(&argv, &self.writer.project, Duration::from_secs(5));
        options.env = Some(&self.environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(())
    }
    fn executable(&self, path: &Path, script: &str) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    fn prepare(&self) -> Result<HostDeliveryContext> {
        HostDeliveryContext::new(self.environment.clone()).prepare(&self.writer, &self.request)
    }
    fn lane_git(&self, args: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut command = vec!["-C", self.writer.worktree.to_str().ok_or("lane")?];
        command.extend_from_slice(args);
        self.git(&command)
    }
    fn commit(
        &self,
        path: &Path,
        message: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        self.git(&[
            "-C",
            path.to_str().ok_or("commit path")?,
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            message,
        ])
    }
    fn terminal(
        &self,
        write: impl FnOnce() -> std::result::Result<(), Box<dyn std::error::Error>>,
    ) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        self.terminal_via("origin", None, write)
    }
    fn terminal_via(
        &self,
        remote: &str,
        base_ref: Option<&str>,
        write: impl FnOnce() -> std::result::Result<(), Box<dyn std::error::Error>>,
    ) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime};
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::fs::Directory;
        use provenant_supervisor::custody::{RecoveryState, WriterBinding};
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;

        let state = self.root.join("state");
        let attempt_path = self.root.join("attempt");
        std::fs::create_dir(&state)?;
        std::fs::create_dir(&attempt_path)?;
        for path in [&state, &attempt_path, &self.root.join("home")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let attempt = Directory::open_anchor(&attempt_path)?;
        let utility = UtilityRuntime::new(
            provenant_os::git::git_executable().ok_or("git executable")?,
            self.root.join("home"),
        )?;
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
        let repo = utility.open_repo(&executor, &self.writer.project, budget, &cancel)?;
        let worktree = provenant_confinement::verify_worktree(&repo, &self.writer.worktree)?;
        let head = provenant_confinement::run_utility(
            &executor,
            GitRead::RevParseHead,
            worktree.repo(),
            &utility,
            budget,
            &cancel,
        )?;
        assert_eq!(head.termination, Termination::Exit(0));
        let base = GitOid::parse(std::str::from_utf8(&head.stdout.bytes)?.trim())?;
        let launch = writer::WriterLaunch {
            worktree,
            branch: provenant_confinement::repo::GitRefName::new("refs/heads/lane")?,
            base,
            custody: None,
        };
        let scope = provenant_supervisor::custody::Scope {
            generation: 1,
            ..tests::exit().scope
        };
        launch.snapshot(&state, &attempt, scope)?;
        let binding = WriterBinding::from_verified(
            launch.worktree.repo().root_dir(),
            launch.worktree.repo().private_dir(),
            launch.branch.as_str(),
            base,
        )?;
        let clock = provenant_os::clock::SystemClock::new();
        let recovery = provenant_supervisor::fixture::acquire_writer_with_binding(
            Directory::open_anchor(&state)?,
            scope,
            binding,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &cancel,
            &clock,
        )?
        .into_recovery()?;
        assert_eq!(recovery.observe(), RecoveryState::Absent);
        write()?;
        let review = launch.terminal(&state, &attempt, &recovery, remote, base_ref)?;
        let archive = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        let manifest: serde_json::Value = serde_json::from_slice(
            &archive.read_bounded(OsStr::new("manifest.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(manifest["review_required"], review);
        let terminal: writer::WriterTerminal = serde_json::from_slice(
            &attempt.read_bounded(OsStr::new("writer-terminal.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(terminal.protected_paths.is_empty(), !review);
        Ok(review)
    }
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
fn terminal_falls_back_when_delivery_target_cannot_be_resolved()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for target in ["missing_head", "dangling_head", "unrelated"] {
        for protected in [false, true] {
            let fixture = PrepareFixture::new()?;
            if target != "missing_head" {
                fixture.git(&[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ])?;
            }
            if target == "unrelated" {
                fixture.git(&["checkout", "--orphan", "unrelated"])?;
                fixture.commit(&fixture.writer.project, "Unrelated target")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
            }
            let review = fixture.terminal(|| {
                let path = if protected { "AGENTS.md" } else { "source.txt" };
                std::fs::write(fixture.writer.worktree.join(path), "writer change\n")?;
                fixture.lane_git(&["add", path])?;
                fixture.commit(&fixture.writer.worktree, "Writer change")
            })?;
            assert_eq!(review, protected, "{target}");
        }
    }
    Ok(())
}

fn merge_upstream_then_edit(
    fixture: &PrepareFixture,
    remote_ref: &str,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    fixture.lane_git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "merge",
        "--no-edit",
        remote_ref,
    ])?;
    std::fs::write(
        fixture.writer.worktree.join("source.txt"),
        "writer source\n",
    )?;
    fixture.lane_git(&["add", "source.txt"])?;
    fixture.commit(&fixture.writer.worktree, "Writer source")
}

fn commit_protected_on(
    fixture: &PrepareFixture,
    remote_ref: &str,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(fixture.writer.project.join(".github"))?;
    std::fs::write(
        fixture.writer.project.join(".github/ci.yml"),
        "upstream policy\n",
    )?;
    fixture.git(&["add", ".github"])?;
    fixture.commit(&fixture.writer.project, "Upstream policy")?;
    fixture.git(&["update-ref", remote_ref, "HEAD"])
}

#[test]
fn terminal_uses_the_base_branch_on_a_delivery_remote_without_head()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    commit_protected_on(&fixture, "refs/remotes/fork/main")?;
    // The delivery remote "fork" has no HEAD, but it carries the base branch.
    let review = fixture.terminal_via("fork", Some("main"), || {
        merge_upstream_then_edit(&fixture, "fork/main")
    })?;
    assert!(!review);
    Ok(())
}

#[test]
fn terminal_never_exempts_a_commit_reachable_only_from_another_remote()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    // origin is a fork with an unreviewed protected commit and a HEAD; the
    // delivery remote "upstream" has neither HEAD nor the base branch.
    commit_protected_on(&fixture, "refs/remotes/origin/main")?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    let review = fixture.terminal_via("upstream", Some("main"), || {
        merge_upstream_then_edit(&fixture, "origin/main")
    })?;
    assert!(review);
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
        .expect_err("detached writer HEAD")
        .to_string();
    assert!(
        error.contains("writer HEAD is not the lane branch"),
        "{error}"
    );
    Ok(())
}

#[test]
fn terminal_exempts_merged_upstream_protected_changes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for inherited in ["none", "addition", "deletion"] {
        for protected in [false, true] {
            let fixture = PrepareFixture::new()?;
            if inherited == "addition" {
                std::fs::create_dir(fixture.writer.worktree.join(".github"))?;
                std::fs::write(
                    fixture.writer.worktree.join(".github/pr.yml"),
                    "PR policy\n",
                )?;
                fixture.lane_git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.worktree, "Existing PR addition")?;
            } else if inherited == "deletion" {
                std::fs::create_dir(fixture.writer.project.join(".github"))?;
                std::fs::write(
                    fixture.writer.project.join(".github/old.yml"),
                    "old policy\n",
                )?;
                fixture.git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.project, "Shared policy")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
                fixture.lane_git(&["merge", "--ff-only", "origin/main"])?;
                fixture.lane_git(&["rm", ".github/old.yml"])?;
                fixture.commit(&fixture.writer.worktree, "Existing PR deletion")?;
            }
            let review = fixture.terminal(|| {
                std::fs::create_dir_all(fixture.writer.project.join(".github"))?;
                std::fs::write(
                    fixture.writer.project.join(".github/ci.yml"),
                    "main policy\n",
                )?;
                fixture.git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.project, "Upstream policy")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
                fixture.git(&[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ])?;
                std::fs::write(
                    fixture.writer.worktree.join("source.txt"),
                    "writer source\n",
                )?;
                fixture.lane_git(&["add", "source.txt"])?;
                fixture.commit(&fixture.writer.worktree, "Writer source")?;
                fixture.lane_git(&[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "merge",
                    "--no-edit",
                    "origin/main",
                ])?;
                if protected {
                    std::fs::write(
                        fixture.writer.worktree.join(".github/ci.yml"),
                        "writer policy\n",
                    )?;
                    fixture.lane_git(&["add", ".github"])?;
                    fixture.commit(&fixture.writer.worktree, "Writer policy")?;
                }
                Ok(())
            })?;
            assert_eq!(review, protected, "{inherited}");
        }
    }
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
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (environment): GIT_SSH_COMMAND is not supported; use host SSH config"
    );
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
            assert_eq!(
                reason,
                format!("host writer delivery failed at Prepare (environment): {name}: {detail}")
            );
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
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (gh executable): gh is unavailable: PATH is unset"
    );
    fixture.environment.push(("PATH".into(), "".into()));
    let reason = fixture
        .prepare()
        .expect_err("gh is unavailable")
        .to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (gh executable): gh is unavailable on PATH"
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
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (git executable): inside the writer worktree"
    );
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
        ("fetch --no-tags", "fetch"),
    ] {
        let mut fixture = PrepareFixture::new()?;
        let git = fixture.root.join("git");
        let real_git = provenant_os::git::git_executable().ok_or("git executable")?;
        fixture.executable(&git, &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'{pattern}'*)\nprintf '%s' 'fixture git failure: private-token https://user:password@example.invalid/ {}' >&2\nexit 2;;\nesac\nexec '{}' \"$@\"\n", "é".repeat(5000), real_git.display()))?;
        fixture
            .environment
            .push(("PROVENANT_GIT".into(), git.into_os_string()));
        fixture
            .environment
            .push(("GH_TOKEN".into(), "private-token".into()));
        let reason = fixture.prepare().expect_err("git must fail").to_string();
        assert!(reason.contains("Prepare"), "{reason}");
        assert!(reason.contains(step), "{reason}");
        assert!(reason.contains("fixture git failure"), "{reason}");
        assert!(!reason.contains("private-token"), "{reason}");
        assert!(!reason.contains("user:password"), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
        assert!(reason.len() < 1400, "diagnostic has {} bytes", reason.len());
    }
    Ok(())
}

#[test]
fn prepare_refuses_unavailable_and_unsafe_credential_helpers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let unavailable = fixture.root.join("missing/gh");
    let not_executable = fixture.root.join("not-executable");
    std::fs::write(&not_executable, "not executable")?;
    let lane_helper = fixture.writer.worktree.join("gh");
    fixture.executable(&lane_helper, "#!/bin/sh\nexit 0\n")?;
    let link = fixture.root.join("linked-gh");
    std::os::unix::fs::symlink(&lane_helper, &link)?;
    let opaque = fixture.root.join("gh;echo");
    fixture.executable(&opaque, "#!/bin/sh\nexit 0\n")?;
    for (helper, detail) in [
        (&unavailable, "gh: not found"),
        (&not_executable, "not-executable"),
        (&fixture.root, "unavailable"),
        (&lane_helper, "inside the writer worktree"),
        (&link, "inside the writer worktree"),
        (&opaque, "opaque shell credential helper"),
    ] {
        fixture.git(&[
            "config",
            "credential.helper",
            &format!("!{} auth git-credential", helper.display()),
        ])?;
        let reason = fixture.prepare().expect_err("unsafe helper").to_string();
        assert!(reason.contains("Prepare (credential helper)"), "{reason}");
        assert!(reason.contains(detail), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
    }
    Ok(())
}

#[test]
fn host_delivery_fetches_commits_pushes_and_records_upstream()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let project = root.join("project");
    let lane = root.join("lane");
    let remote = root.join("remote.git");
    for path in [&home, &project] {
        std::fs::create_dir(path)?;
    }
    let environment = vec![
        ("HOME".into(), home.into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ];
    let host = HostDeliveryContext::new(environment.clone());
    let executor = SystemExecutor::new(1)?;
    let git_executable = provenant_os::git::git_executable().ok_or("git executable")?;
    let git =
        |cwd: &Path, args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once(git_executable.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
                .collect();
            let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
            options.env = Some(&environment);
            let output = executor.execute(options)?;
            assert_eq!(
                output.termination,
                Termination::Exit(0),
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
            Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
        };
    git(&project, &["init", "-q"])?;
    git(&project, &["config", "user.name", "Fixture"])?;
    git(
        &project,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(&project, &["commit", "--allow-empty", "-qm", "Initial"])?;
    git(
        &project,
        &["init", "--bare", "-q", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &project,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    // A narrowed fetch refspec that omits the delivered branch, as in a
    // main-only clone, must not defeat delivery.
    git(
        &project,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/base:refs/remotes/origin/base",
        ],
    )?;
    git(&project, &["push", "-q", "origin", "HEAD:refs/heads/base"])?;
    let initial = git(&project, &["rev-parse", "HEAD"])?;
    git(&project, &["update-ref", "-d", "refs/remotes/origin/base"])?;
    git(
        &project,
        &[
            "worktree",
            "add",
            "-qb",
            "lane",
            lane.to_str().ok_or("lane")?,
        ],
    )?;
    let writer = WriterRequest {
        project,
        worktree: lane.clone(),
    };
    let request = WriterDeliveryRequest {
        mode: Default::default(),
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
    };
    let host = host.prepare(&writer, &request)?;
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/base"])?,
        initial
    );
    // The writer's committed head, rather than any provider's prose, is delivered.
    std::fs::write(lane.join("change"), "committed by the writer")?;
    git(&lane, &["add", "change"])?;
    git(&lane, &["commit", "-qm", "Writer change"])?;
    git(&lane, &["config", "push.followTags", "true"])?;
    git(
        &lane,
        &["tag", "-a", "local-only", "-m", "Do not deliver this tag"],
    )?;
    let hook = writer.project.join(".git/hooks/pre-push");
    std::fs::write(&hook, "#!/bin/sh\nexit 42\n")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))?;
    let head = GitOid::parse(&git(&lane, &["rev-parse", "HEAD"])?)?;
    let head_text = head.to_string();
    let fixture = root.join("store");
    std::fs::create_dir(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&tests::context(&fixture)?, &call)?;
    let mut input = tests::input();
    input.request.tasks[0].writer = Some(writer.clone());
    input.request.tasks[0].deliver = Some(request.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    // A lane that rewrites the stored destination after launch cannot steer
    // the host's push: the destination bound at prepare wins.
    let rewritten = WriterDeliveryRequest {
        branch: "main".into(),
        ..request.clone()
    };
    assert!(matches!(
        deliver_once(&host, &writer, &rewritten, &attempt, "fixture receipt"),
        Err(RunError::Validation(
            "delivery destination changed after launch"
        ))
    ));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/main"])?.is_empty());
    let receipt = deliver_once(&host, &writer, &request, &attempt, "fixture receipt")?;
    assert_eq!(receipt.head, Some(head));
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered { pull_request: None }
    ));
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/published/lane"])?,
        head_text.as_str()
    );
    assert_eq!(git(&lane, &["config", "branch.lane.remote"])?, "origin");
    assert_eq!(
        git(&lane, &["config", "branch.lane.merge"])?,
        "refs/heads/published/lane"
    );
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    assert!(git(&lane, &["ls-remote", "--tags", "origin"])?.is_empty());
    dispatch_tests::seal_exit(&store, &tests::exit())?;
    let record = store.publish_terminal(id, 1, &tests::publication(), &tests::exit(), &call)?;
    assert_eq!(record.delivery.as_ref(), Some(&receipt));
    assert_eq!(store.record(id, &call)?.head, Some(head));
    // A repeated terminal callback must not push a newer head or repeat PR creation.
    git(&lane, &["commit", "--allow-empty", "-qm", "Later"])?;
    assert_eq!(
        deliver_once(&host, &writer, &request, &attempt, "fixture receipt")?,
        receipt
    );
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    // A writer that made no commit reports `no_new_commits` and pushes nothing.
    let idle_lane = root.join("idle");
    git(
        &writer.project,
        &[
            "worktree",
            "add",
            "-qb",
            "idle",
            idle_lane.to_str().ok_or("idle")?,
        ],
    )?;
    let idle_writer = WriterRequest {
        project: writer.project.clone(),
        worktree: idle_lane.clone(),
    };
    let idle_request = WriterDeliveryRequest {
        branch: "published/idle".into(),
        ..request.clone()
    };
    let idle_host =
        HostDeliveryContext::new(environment.clone()).prepare(&idle_writer, &idle_request)?;
    let mut idle = tests::input();
    idle.run_id = provenant_core::RunId::from_bytes([17; 16]);
    idle.operation_id = provenant_core::OperationId::parse("idle-delivery")?;
    idle.attempts[0].attempt_id = provenant_core::AttemptId::from_bytes([18; 16]);
    idle.request.tasks[0].writer = Some(idle_writer.clone());
    idle.request.tasks[0].deliver = Some(idle_request.clone());
    store.create(&idle, &call)?;
    let idle_attempt = store.attempt_dir(idle.run_id, idle.attempts[0].attempt_id, false)?;
    let idle_receipt = deliver_once(
        &idle_host,
        &idle_writer,
        &idle_request,
        &idle_attempt,
        "fixture receipt",
    )?;
    assert_eq!(idle_receipt.state, WriterDeliveryState::NoNewCommits);
    assert_eq!(idle_receipt.head, Some(GitOid::parse(&initial)?));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/published/idle"])?.is_empty());
    // An interrupted delivery is recorded during terminal recovery, with no push.
    let mut interrupted = tests::input();
    interrupted.run_id = provenant_core::RunId::from_bytes([14; 16]);
    interrupted.operation_id = provenant_core::OperationId::parse("interrupted-delivery")?;
    interrupted.attempts[0].attempt_id = provenant_core::AttemptId::from_bytes([16; 16]);
    interrupted.request.tasks[0].writer = Some(writer);
    interrupted.request.tasks[0].deliver = Some(WriterDeliveryRequest {
        branch: "interrupted".into(),
        ..request
    });
    store.create(&interrupted, &call)?;
    let id = interrupted.attempts[0].attempt_id;
    let attempt = store.attempt_dir(interrupted.run_id, id, false)?;
    let pending = WriterDeliveryReceipt {
        branch: "interrupted".into(),
        state: WriterDeliveryState::Failed {
            stage: WriterDeliveryStage::Interrupted,
        },
        ..receipt
    };
    artifacts::seal(
        &attempt,
        "delivery-started.json",
        &db::bytes(&pending)?,
        RECORD_LIMIT,
    )?;
    let mut exit = tests::exit();
    exit.scope.run = interrupted.run_id;
    exit.scope.attempt = id;
    dispatch_tests::seal_exit(&store, &exit)?;
    artifacts::seal(
        &attempt,
        "result.md",
        tests::publication().result.as_bytes(),
        RESULT_LIMIT,
    )?;
    let record = store.recover_result_only(id, &tests::publication(), &exit, &call)?;
    assert_eq!(record.delivery, Some(pending));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/interrupted"])?.is_empty());
    Ok(())
}
