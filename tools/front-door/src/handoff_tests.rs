use super::*;

fn test_forge(remote: &str) -> provenant_config::ForgeContext {
    provenant_run::forge_for(remote, "trunk", &[])
}

fn remote(name: &str) -> provenant_config::RemoteName {
    provenant_config::RemoteName::new(name).expect("fixture remote")
}

#[test]
fn git_refuses_stdout_past_its_bound() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("big"), vec![b'a'; 128 * 1024]).unwrap();
    let error = git(
        temp.path(),
        &["diff", "--no-index", "--no-color", "/dev/null", "big"],
        GIT_TIMEOUT,
    )
    .expect_err("truncated output must not read as complete")
    .message;
    assert!(
        error.contains("output exceeded the 65536-byte limit"),
        "{error}"
    );
    assert!(error.contains("diff"), "{error}");
    // Output within the bound is still returned whole.
    let (ok, text) = git(temp.path(), &["--version"], GIT_TIMEOUT).unwrap();
    assert!(ok);
    assert!(text.starts_with("git version"), "{text}");
}

#[test]
fn failed_recreation_fetch_never_authorises_stale_tracking_refs() {
    let mut fetched = false;
    let mut attempts = 0;
    for _ in 0..2 {
        let result = fetch_recreation_base(&mut fetched, |fetch| {
            assert!(fetch, "a failed fetch must be retried");
            attempts += 1;
            Err::<(), _>("fetch failed")
        });
        assert!(result.is_err());
    }
    assert_eq!(attempts, 2);
    fetch_recreation_base(&mut fetched, |fetch| {
        assert!(fetch);
        Ok::<_, &str>(())
    })
    .expect("fetch succeeds");
    fetch_recreation_base(&mut fetched, |fetch| {
        assert!(!fetch, "a verified fetch may be reused");
        Ok::<_, &str>(())
    })
    .expect("verified tracking refs may be reused");
}

#[test]
fn wip_refuses_held_or_uncertain_writer_custody() {
    let mut tree = Tree {
        path: "/repo/lane".into(),
        branch: Some("lane".into()),
        head: "a".repeat(40),
        primary: false,
        dirty: true,
        unknown: false,
        live_writer: false,
        writer_unknown: false,
        writer_evidence: None,
        writer_advisory: None,
        upstream: None,
        pushed: false,
    };
    for state in [
        Occupancy::Held,
        Occupancy::Unknown {
            scope: tree.path.clone(),
            evidence: "fixture".into(),
            source: worktrees::OccupancyUnknownSource::Custody,
        },
    ] {
        (tree.live_writer, tree.writer_unknown) = handoff_writer_state(&state);
        assert!(unsafe_wip(&tree), "{state:?} must not permit Git writes");
    }
    for advisory in [
        Occupancy::Available,
        Occupancy::Observed { process: None },
        Occupancy::Unknown {
            scope: tree.path.clone(),
            evidence: "census incomplete".into(),
            source: worktrees::OccupancyUnknownSource::Census,
        },
    ] {
        (tree.live_writer, tree.writer_unknown) = handoff_writer_state(&advisory);
        assert!(!unsafe_wip(&tree), "{advisory:?} is advisory");
    }
    let mut output = Vec::new();
    write_writer_advisory(&mut output, "/repo/lane: census incomplete").expect("advisory written");
    let output = String::from_utf8(output).expect("UTF-8 advisory");
    assert!(output.contains("/repo/lane: census incomplete"));
    tree.writer_unknown = true;
    tree.writer_evidence = Some("/repo: process census incomplete".into());
    let refusal = wip_unsafe_description(&tree);
    assert!(refusal.contains("/repo: process census incomplete"));
    tree.dirty = false;
    tree.upstream = Some("b".repeat(40));
    tree.pushed = true;
    assert!(
        tree.problems().is_empty(),
        "observation alone must not block handoff"
    );
}

#[test]
fn wip_holds_the_writer_lease_during_git_mutation() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{Occupancy, WorkerObserver};
    use std::collections::BTreeMap;
    struct Free;
    impl WorkerObserver for Free {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Available))
                .collect()
        }
    }
    struct Bystander;
    impl WorkerObserver for Bystander {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Observed { process: None }))
                .collect()
        }
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let project = temp.path().join("project");
    let git_dir = temp.path().join("project-metadata");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::create_dir_all(&project)?;
    provenant_os::fixture::run_git(
        &project,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            git_dir.to_str().ok_or("non-UTF-8 fixture path")?,
        ],
    )?;
    std::fs::write(project.join("tracked.txt"), "base\n")?;
    provenant_os::fixture::run_git(&project, &["add", "tracked.txt"])?;
    provenant_os::fixture::run_git(&project, &["commit", "-qm", "base"])?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let info = crate::worktrees::with_context(
        &roots,
        &home,
        &project,
        GIT_TIMEOUT,
        &Free,
        |context, cancel| {
            Ok(worktrees::create(
                context,
                WorktreeCreate {
                    branch: "wip-custody".into(),
                    base: None,
                },
                cancel,
            )?)
        },
    )?;
    let mut tree = Tree {
        path: info.path.clone(),
        branch: Some("wip-custody".into()),
        head: info.head.to_string(),
        primary: false,
        dirty: true,
        unknown: false,
        live_writer: false,
        writer_unknown: false,
        writer_evidence: None,
        writer_advisory: None,
        upstream: None,
        pushed: false,
    };
    let facts = worktrees::RepoFacts::default();
    provenant_os::fixture::run_git(
        &project,
        &["update-ref", "refs/remotes/origin/wip-custody", &tree.head],
    )?;
    std::fs::write(project.join("tracked.txt"), "dirty primary\n")?;
    let inspected = crate::worktrees::with_context(
        &roots,
        &home,
        &project,
        GIT_TIMEOUT,
        &Free,
        |context, cancel| inspect_context(context, &facts, cancel, Some(&remote("origin"))),
    )?;
    let primary_path = std::fs::canonicalize(&project)?;
    let listed_paths: Vec<_> = inspected
        .iter()
        .map(|row| row.path.display().to_string())
        .collect();
    let primary = inspected
        .iter()
        .find(|row| row.path == primary_path)
        .ok_or_else(|| format!("missing primary checkout; listed paths: {listed_paths:?}"))?;
    assert!(
        primary.primary,
        "the external metadata dir cannot hide primary identity"
    );
    assert!(primary.dirty);
    assert!(
        unsafe_wip(primary),
        "handoff WIP never treats the primary as a branch"
    );
    let row = inspected
        .iter()
        .find(|row| row.path == tree.path)
        .ok_or("missing lane")?;
    assert!(!row.primary, "a linked writer is not the primary checkout");
    assert!(!unsafe_wip(row));
    assert_eq!(row.upstream.as_deref(), Some(tree.head.as_str()));
    assert!(row.pushed);
    let observed = crate::worktrees::with_context(
        &roots,
        &home,
        &project,
        GIT_TIMEOUT,
        &Bystander,
        |context, cancel| inspect_context(context, &facts, cancel, Some(&remote("origin"))),
    )?;
    let observed = observed
        .iter()
        .find(|row| row.path == tree.path)
        .ok_or("missing lane")?;
    assert!(!unsafe_wip(observed));
    assert!(observed.writer_advisory.as_deref().is_some_and(|warning| {
        warning.contains(&tree.path.to_string_lossy().into_owned())
            && warning.contains("process was observed")
    }));
    let limited = worktrees::RepoFacts::with_limits(1, provenant_os::git::REPO_FACT_SCALAR_LIMIT)?;
    let overflow = crate::worktrees::with_context(
        &roots,
        &home,
        &project,
        GIT_TIMEOUT,
        &Free,
        |context, cancel| inspect_context(context, &limited, cancel, Some(&remote("origin"))),
    );
    assert!(matches!(
        overflow,
        Err(crate::worktrees::Error::Worktree(
            worktrees::WorktreeError::Utility(
                provenant_confinement::utility::UtilityError::Truncated { stream: "stdout" }
            )
        ))
    ));
    let held = crate::worktrees::with_context(
        &roots,
        &home,
        &project,
        GIT_TIMEOUT,
        &Free,
        |context, _| {
            worktrees::cache_writer_guard(context.project_state, &tree.path)
                .map_err(|_| worktrees::WorktreeError::UnknownWriter.into())
        },
    )?;
    let called = std::cell::Cell::new(false);
    let refused = with_wip_custody(&roots, &home, &project, &Free, &facts, &tree, || {
        called.set(true);
        Ok(())
    })
    .expect_err("held writer must refuse WIP");
    assert_eq!(refused.code(), "WIP_REFUSED");
    assert!(!called.get());
    drop(held);
    with_wip_custody(&roots, &home, &project, &Bystander, &facts, &tree, || {
        called.set(true);
        Ok(())
    })
    .map_err(|error| error.message)?;
    assert!(
        called.replace(false),
        "a bystander must not block guarded WIP"
    );
    provenant_os::fixture::run_git(&tree.path, &["commit", "--allow-empty", "-qm", "moved"])?;
    let refreshed = crate::worktrees::with_context(
        &roots,
        &home,
        &project,
        GIT_TIMEOUT,
        &Free,
        |context, cancel| inspect_context(context, &facts, cancel, Some(&remote("origin"))),
    )?;
    let row = refreshed
        .iter()
        .find(|row| row.path == tree.path)
        .ok_or("missing lane")?;
    assert!(!row.pushed, "inspection must refresh after HEAD moves");
    let refused = with_wip_custody(&roots, &home, &project, &Free, &facts, &tree, || {
        called.set(true);
        Ok(())
    })
    .expect_err("moved HEAD must refuse WIP");
    assert_eq!(refused.code(), "WIP_REFUSED");
    assert!(!called.get());
    tree.head = String::from_utf8(provenant_os::fixture::run_git(
        &tree.path,
        &["rev-parse", "HEAD"],
    )?)?
    .trim()
    .to_owned();
    with_wip_custody(&roots, &home, &project, &Free, &facts, &tree, || {
        let held = crate::worktrees::with_context(
            &roots,
            &home,
            &project,
            GIT_TIMEOUT,
            &Free,
            |context, _| {
                Ok(worktrees::cache_writer_probe(context.project_state, &tree.path).is_err())
            },
        )?;
        assert!(held, "the writer lock must cover the mutation");
        provenant_os::fixture::run_git(
            &tree.path,
            &["commit", "--allow-empty", "-qm", "guarded mutation"],
        )
        .map_err(|error| Refusal::refused("WIP_FAILED", error.to_string()))?;
        called.set(true);
        Ok(())
    })
    .map_err(|error| error.message)?;
    assert!(called.get());
    let current = String::from_utf8(provenant_os::fixture::run_git(
        &tree.path,
        &["rev-parse", "HEAD"],
    )?)?;
    let cached = crate::worktrees::with_context(
        &roots,
        &home,
        &project,
        GIT_TIMEOUT,
        &Free,
        |context, cancel| {
            let context = &worktrees::WorktreeContext {
                facts: &facts,
                ..*context
            };
            Ok(worktrees::list_cached(context, cancel)?)
        },
    )?;
    let current_row = cached
        .iter()
        .find(|row| row.path == tree.path)
        .ok_or("missing lane")?;
    assert_eq!(current_row.head.to_string(), current.trim());
    assert_ne!(current_row.head.to_string(), tree.head);
    Ok(())
}

#[test]
fn account_directory_returns_only_a_registered_match() {
    use provenant_providers::identity::RegisteredAccount;
    let accounts = vec![
        RegisteredAccount::new("claude", "work", "/Users/me/.claude-work".into()).unwrap(),
        RegisteredAccount::new("claude", "home", "/Users/me/.claude-home".into()).unwrap(),
    ];
    assert_eq!(
        account_directory(&accounts, "work").unwrap(),
        PathBuf::from("/Users/me/.claude-work")
    );
    assert_eq!(account_directory(&accounts, "other"), None);
}

#[test]
fn reused_worktrees_name_a_differing_head_or_dirty_tree() {
    let base = provenant_core::GitOid::parse(&"a".repeat(40)).unwrap();
    let mut info = worktrees::WorktreeInfo {
        warnings: Vec::new(),
        path: "/repo/.worktrees/x".into(),
        branch: Some("feat/x".into()),
        head: base,
        dirty: WorktreeCondition::Clean,
        occupancy: Occupancy::Available,
        owner: Err("worktree registration was not observed".into()),
        primary: false,
        lane: None,
        attempt: None,
        base: None,
        reused: false,
    };
    assert_eq!(worktree_warning(&info, &base, &remote("origin")), None);
    info.head = provenant_core::GitOid::parse(&"b".repeat(40)).unwrap();
    info.dirty = WorktreeCondition::Dirty;
    let warning = worktree_warning(&info, &base, &remote("origin")).unwrap();
    assert!(
        warning.contains("local branch differs from origin"),
        "{warning}"
    );
    assert!(warning.contains("uncommitted changes"), "{warning}");
}

#[test]
fn a_delivery_that_did_not_launch_fails_unless_no_launch_was_given() {
    let receipt = HandoffReceipt::Delivered {
        extra: Default::default(),
        prompt_path: "/state/prompt.md".into(),
        project: "/repo".into(),
        worktrees: Vec::new(),
        attachments: Vec::new(),
        script_path: Some("/state/launch.command".into()),
        launched: false,
        launch_error: Some("no Terminal".into()),
    };
    let mut data = json!({"receipt": serde_json::to_value(&receipt).unwrap()});
    let failure = launch_failure("workshop", &receipt, false, &mut data).unwrap();
    assert_eq!(failure.code, "LAUNCH_FAILED");
    assert!(matches!(failure.status, Outcome::Error));
    let (kept, _) = failure.data.unwrap();
    assert_eq!(kept["receipt"]["outcome"], "delivered");
    let mut data = json!({});
    assert!(launch_failure("workshop", &receipt, true, &mut data).is_none());
    let mut launched = receipt.clone();
    if let HandoffReceipt::Delivered { launched: l, .. } = &mut launched {
        *l = true;
    }
    assert!(launch_failure("workshop", &launched, false, &mut data).is_none());
}

#[test]
fn attachments_are_capped_and_named_plainly() {
    let temp = tempfile::tempdir().unwrap();
    let small = temp.path().join("notes.md");
    fs::write(&small, b"hello").unwrap();
    let read = read_attachments(std::slice::from_ref(&small)).unwrap();
    assert_eq!(read[0].name, "notes.md");
    assert_eq!(read[0].base64, encode_base64(b"hello"));
    assert!(read_attachments(&[small.clone(), small.clone()]).is_err());
    let big = temp.path().join("big.bin");
    fs::write(&big, vec![0; ATTACHMENT_LIMIT]).unwrap();
    assert!(read_attachments(std::slice::from_ref(&big)).is_ok());
    let err = read_attachments(&[big, small]).err().unwrap();
    assert!(err.message.contains("512 KiB"), "{}", err.message);
    assert!(read_attachments(&[temp.path().to_path_buf()]).is_err());
}

#[test]
fn a_handoff_checkpoint_keeps_the_configured_number_of_revisions() {
    use provenant_core::{CancelToken, Clock, PathResolver, ProjectId, RootInputs, WaitBudget};
    struct Plain;
    impl PathResolver for Plain {
        fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&Plain).unwrap();
    fs::create_dir_all(roots.state.as_path()).unwrap();
    fs::write(
        roots.state.as_path().join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[retention]\nsession_revisions=2\n",
    )
    .unwrap();
    let store = checkpoint_store(
        &roots,
        ProjectId::from_bytes([3; 16]),
        HostSessionId::parse("handoff-session").unwrap(),
    )
    .with_chair(
        provenant_core::HostId::from_bytes([4; 16]),
        provenant_core::ActorId::from_bytes([5; 16]),
    );
    let (clock, cancel) = (SystemClock::new(), CancelToken::new());
    for text in ["one\n", "two\n", "three\n", "four\n"] {
        let budget = WaitBudget::cli(clock.mono_now(), 30).unwrap();
        store
            .save(text.as_bytes(), budget, &cancel, &clock)
            .unwrap();
    }
    let kept: Vec<u64> = store
        .revisions()
        .unwrap()
        .iter()
        .map(|revision| revision.revision)
        .collect();
    assert_eq!(kept, [3, 4], "[retention] session_revisions applies");
    let repair = checkpoint_repair(
        &Globals {
            state_dir: Some(PathBuf::from("/custom state")),
            project: Some(PathBuf::from("/selected project")),
            ..Default::default()
        },
        Path::new("/selected project"),
        &HostSessionId::parse("handoff-session").unwrap(),
        Some(provenant_records::Harness::Codex),
    );
    assert!(checkpoint_identity(&store, &repair).is_ok());
    let other = checkpoint_store(
        &roots,
        ProjectId::from_bytes([3; 16]),
        HostSessionId::parse("handoff-session").unwrap(),
    )
    .with_chair(
        provenant_core::HostId::from_bytes([4; 16]),
        provenant_core::ActorId::from_bytes([6; 16]),
    );
    let refusal = checkpoint_identity(&other, &repair).err().unwrap();
    assert_eq!(refusal.code(), "IDENTITY");
    assert!(refusal.message.contains("provenant --state-dir '/custom state' --project '/selected project' workflow session save handoff-session --client codex --rebind --file <checkpoint.md>"));
}

#[test]
fn checkpoint_repair_without_a_client_requires_a_harness() {
    let repair = checkpoint_repair(
        &Globals::default(),
        Path::new("/project"),
        &HostSessionId::parse("chair").unwrap(),
        None,
    );
    assert!(repair.contains("workflow session save chair --client HARNESS --rebind"));
}

#[test]
fn explicit_session_ids_are_validated() {
    // Only explicit IDs: tests never read the live session's environment.
    assert_eq!(
        session_id(Some("explicit-session"))
            .ok()
            .flatten()
            .map(String::from),
        Some("explicit-session".into())
    );
    assert!(session_id(Some("Not Valid")).is_err());
}

/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
#[test]
fn wip_push_and_recreation_follow_the_upstream_context() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let bare = temp.path().join("upstream.git");
    std::fs::create_dir_all(&project)?;
    provenant_os::fixture::run_git(
        temp.path(),
        &["init", "--bare", bare.to_str().ok_or("remote")?],
    )?;
    for args in [
        vec!["init", "-q", "-b", "trunk"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["remote", "add", "upstream", bare.to_str().ok_or("remote")?],
        vec!["push", "upstream", "HEAD:refs/heads/trunk"],
        vec!["checkout", "-qb", "lane"],
        vec!["commit", "--allow-empty", "-qm", "lane"],
    ] {
        provenant_os::fixture::run_git(&project, &args)?;
    }
    provenant_os::fixture::run_git(&bare, &["symbolic-ref", "HEAD", "refs/heads/trunk"])?;
    let head = provenant_os::fixture::run_git(&project, &["rev-parse", "HEAD"])?;
    let tree = Tree {
        path: project.clone(),
        branch: Some("lane".into()),
        head: String::from_utf8_lossy(&head).trim().to_owned(),
        primary: false,
        dirty: false,
        unknown: false,
        live_writer: false,
        writer_unknown: false,
        writer_evidence: None,
        writer_advisory: None,
        upstream: None,
        pushed: false,
    };
    let forge = test_forge("upstream");
    assert!(forge.is_protected("trunk"));
    wip_git(&tree, &remote(&forge.remote.value)).map_err(|refusal| format!("{refusal:?}"))?;
    let pushed = provenant_os::fixture::run_git(&bare, &["rev-parse", "refs/heads/lane"])?;
    assert_eq!(pushed.trim_ascii(), head.trim_ascii());
    assert!(!forge.is_protected("lane"));
    Ok(())
}

/// The handoff inspection and recreation call sites use the declared remote.
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
#[test]
fn inspect_and_recreate_follow_the_configured_upstream_remote()
-> Result<(), Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;

    struct Free;
    impl WorkerObserver for Free {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, Occupancy::Available))
                .collect()
        }
    }

    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let project = temp.path().join("project");
    let bare = temp.path().join("upstream.git");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::create_dir_all(&project)?;
    provenant_os::fixture::run_git(
        temp.path(),
        &["init", "--bare", bare.to_str().ok_or("remote path")?],
    )?;
    provenant_os::fixture::run_git(&project, &["init", "-q", "-b", "trunk"])?;
    std::fs::write(project.join("tracked.txt"), "base\n")?;
    provenant_os::fixture::run_git(&project, &["add", "tracked.txt"])?;
    provenant_os::fixture::run_git(&project, &["commit", "-qm", "base"])?;
    provenant_os::fixture::run_git(
        &project,
        &[
            "remote",
            "add",
            "upstream",
            bare.to_str().ok_or("remote path")?,
        ],
    )?;
    for args in [
        vec!["checkout", "-qb", "lane"],
        vec!["commit", "--allow-empty", "-qm", "lane"],
        vec!["push", "-q", "upstream", "HEAD:refs/heads/lane"],
        vec!["checkout", "-q", "trunk"],
        vec!["checkout", "-qb", "recreated"],
        vec!["commit", "--allow-empty", "-qm", "recreated"],
        vec!["push", "-q", "upstream", "HEAD:refs/heads/recreated"],
        vec!["checkout", "-q", "trunk"],
        vec!["branch", "-D", "recreated"],
        vec!["fetch", "-q", "upstream"],
        // Inspection enumerates checkouts, not all local branches.
        vec!["checkout", "-q", "lane"],
    ] {
        provenant_os::fixture::run_git(&project, &args)?;
    }
    std::fs::create_dir_all(project.join(".provenant"))?;
    std::fs::write(
        project.join(".provenant/ports.json"),
        r#"{"git":{"remote":"upstream"}}"#,
    )?;

    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let context = Context::open(roots.clone(), &home, &project)?;
    let source = Source {
        roots: roots.clone(),
        home: home.clone(),
        assets: temp.path().join("assets"),
        cwd: project.clone(),
        context,
    };
    let declared_remote =
        resolve_remote(roots.state.as_path(), &project, &source.context.port_table)
            .map_err(|refusal| refusal.message)?;
    assert_eq!(declared_remote.as_ref(), Some(&remote("upstream")));
    let inspected = inspect(
        &source,
        &Free,
        &worktrees::RepoFacts::default(),
        declared_remote.as_ref(),
    )
    .map_err(|refusal| refusal.message)?;
    let lane = inspected
        .iter()
        .find(|tree| tree.branch.as_deref() == Some("lane"))
        .expect("lane is listed by handoff inspection");
    assert!(lane.pushed, "inspection must resolve upstream/lane");
    assert!(lane.upstream.is_some());

    let clock = SystemClock::new();
    let target = Target {
        context: &source.context,
        home: &home,
        cwd: &project,
        workers: &Free,
        source: HostId::from_bytes([1; 16]),
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
    };
    let made = recreate(&target, &["recreated".into()], declared_remote);
    assert_eq!(made.len(), 1);
    assert!(
        made[0].error.is_none(),
        "recreation from upstream must succeed"
    );
    assert!(made[0].created);
    assert!(
        made[0]
            .path
            .as_deref()
            .is_some_and(|path| Path::new(path).exists())
    );
    Ok(())
}
