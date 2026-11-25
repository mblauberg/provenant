/// Every launch-input refusal in the graph, peer, owner and unified paths names
/// its field. A bare `InvalidInput` has no field to repair, so the paths that
/// reject graph input must build their refusals with `FacadeError::field` or a
/// typed detail, never with an empty one.
// Spec: docs/specs/run-launch.md#writer-binding
#[test]
fn graph_input_refusals_name_their_field() {
    let bare = ["InvalidInput { ", "detail: None }"].concat();
    for (path, source) in [
        (
            "src/fabric.rs",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/fabric.rs")),
        ),
        (
            "src/fabric/graph_execution.rs",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/fabric/graph_execution.rs"
            )),
        ),
        (
            "src/fabric/graph_inputs.rs",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/fabric/graph_inputs.rs"
            )),
        ),
        (
            "src/fabric/owner.rs",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/fabric/owner.rs")),
        ),
        (
            "src/fabric/peer.rs",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/fabric/peer.rs")),
        ),
        (
            "src/fabric/unified.rs",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/fabric/unified.rs"
            )),
        ),
    ] {
        let unnamed: Vec<usize> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(&bare))
            .map(|(index, _)| index + 1)
            .collect();
        assert!(
            unnamed.is_empty(),
            "{path} has an unnamed refusal at lines {unnamed:?}"
        );
    }
}

#[cfg(test)]
#[test]
fn peer_remote_base_pins_a_commit_reachable_from_the_remote()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{self, Occupancy, WorkerObserver};
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
    let remote = temp.path().join("remote.git");
    for path in [&home, &state, &project, &remote] {
        std::fs::create_dir_all(path)?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    provenant_os::fixture::run_git(&remote, &["init", "--bare", "-q", "--initial-branch=main"])?;
    provenant_os::fixture::run_git(&project, &["init", "-q", "--initial-branch=main"])?;
    provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
    provenant_os::fixture::run_git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().ok_or("remote path")?,
        ],
    )?;
    provenant_os::fixture::run_git(&project, &["push", "origin", "HEAD:refs/heads/main"])?;
    let initial = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["rev-parse", "HEAD"],
    )?)?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            let pinned = remote_base(
                context,
                "HEAD",
                std::time::Duration::from_secs(30),
                true,
                &cancel,
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert_eq!(pinned.to_string(), initial.trim());
            Ok(())
        },
    )?;
    let absent = temp.path().join("absent.git");
    provenant_os::fixture::run_git(
        &project,
        &[
            "remote",
            "add",
            "a-unavailable",
            absent.to_str().ok_or("absent path")?,
        ],
    )?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert_eq!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                )
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?
                .to_string(),
                initial.trim()
            );
            Ok(())
        },
    )?;
    provenant_os::fixture::run_git(&project, &["remote", "remove", "a-unavailable"])?;
    // Fetch leaves a deleted remote-tracking ref behind; it must not make the
    // old tip appear to remain advertised by the remote.
    provenant_os::fixture::run_git(&remote, &["update-ref", "-d", "refs/heads/main"])?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    true,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            Ok(())
        },
    )?;
    provenant_os::fixture::run_git(&remote, &["update-ref", "refs/heads/main", initial.trim()])?;
    let limited = worktrees::RepoFacts::with_limits(provenant_os::exec::DEFAULT_CAPTURE_LIMIT, 1)?;
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            let context = &worktrees::WorktreeContext {
                facts: &limited,
                ..*context
            };
            let error = remote_base(
                context,
                "HEAD",
                std::time::Duration::from_secs(30),
                false,
                &cancel,
            )
            .expect_err("bounded fact output must be reported");
            assert!(matches!(
                &error,
                FacadeError::Io { operation, .. }
                    if operation == provenant_fabric_runtime::coordination::facade::REPOSITORY_FACT_LIMIT_OPERATION
            ));
            let wire = serde_json::to_value(&error)
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert_eq!(wire["operation"], "repository fact output limit exceeded");
            let cancelled = CancelToken::new();
            cancelled.cancel();
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancelled
                ),
                Err(FacadeError::Cancelled)
            ));
            Ok(())
        },
    )?;
    // A replacement object must not make a local-only root look reachable
    // from the real remote-tracking commit.
    let tree = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["rev-parse", "HEAD^{tree}"],
    )?)?;
    let local = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["commit-tree", tree.trim(), "-m", "local only"],
    )?)?;
    let replacement = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            local.trim(),
            "-m",
            "poisoned ancestry",
        ],
    )?)?;
    provenant_os::fixture::run_git(&project, &["update-ref", "HEAD", local.trim()])?;
    provenant_os::fixture::run_git(&project, &["replace", initial.trim(), replacement.trim()])?;
    let contains = format!("--contains={}", local.trim());
    let poisoned = provenant_os::fixture::run_git(
        &project,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &contains,
            "refs/remotes",
        ],
    )?;
    assert!(
        !poisoned.is_empty(),
        "fixture must poison ordinary reachability"
    );
    worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            provenant_os::fixture::run_git(&project, &["replace", "-d", initial.trim()])
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let grafts = project.join(".git/info/grafts");
            std::fs::write(&grafts, format!("{} {}\n", initial.trim(), local.trim()))
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let poisoned = provenant_os::fixture::run_git(
                &project,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    &contains,
                    "refs/remotes",
                ],
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert!(
                !poisoned.is_empty(),
                "fixture must poison ordinary reachability"
            );
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    false,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            std::fs::remove_file(grafts).map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            std::fs::rename(&project, temp.path().join("moved-project"))
                .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            std::fs::create_dir(&project).map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            assert!(matches!(
                remote_base(
                    context,
                    "HEAD",
                    std::time::Duration::from_secs(30),
                    true,
                    &cancel
                ),
                Err(FacadeError::BaseUnreachable)
            ));
            Ok(())
        },
    )?;
    Ok(())
}

// Spec: docs/specs/fabric.md#publication-boundary
#[cfg(test)]
#[test]
fn peer_base_resolution_fetches_a_commit_missing_from_a_stale_clone()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{Occupancy, WorkerObserver};
    use provenant_records::Outcome;
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
    let rig = crate::worktree_tests::repository()?;
    provenant_os::fixture::private_state_dir(&rig.state)?;
    let temp = tempfile::tempdir()?;
    let origin = temp.path().join("origin.git");
    let upstream = temp.path().join("upstream");
    std::fs::create_dir_all(&origin)?;
    // Name the branch in the bare repo itself: a clone checks out the branch
    // Git advertises as HEAD, and a clone on an unborn default branch would
    // root its commit and then be rejected as non-fast-forward.
    provenant_os::fixture::run_git(&origin, &["init", "--bare", "-q", "--initial-branch=main"])?;
    // Seed the peer clone's origin at the commit the peer already has.
    provenant_os::fixture::run_git(
        &rig.project,
        &[
            "remote",
            "add",
            "origin",
            origin.to_str().ok_or("origin path")?,
        ],
    )?;
    provenant_os::fixture::run_git(&rig.project, &["push", "origin", "HEAD:refs/heads/main"])?;
    let base = String::from_utf8(provenant_os::fixture::run_git(
        &rig.project,
        &["rev-parse", "HEAD"],
    )?)?;
    let base = base.trim().to_owned();
    // Move the origin forward from a separate clone of main; the peer clone is
    // behind, and the advance is a descendant, so no push rewrites the origin.
    provenant_os::fixture::run_git(
        temp.path(),
        &[
            "clone",
            "-q",
            "--branch",
            "main",
            origin.to_str().ok_or("origin path")?,
            upstream.to_str().ok_or("upstream path")?,
        ],
    )?;
    provenant_os::fixture::run_git(&upstream, &["config", "user.name", "Fixture"])?;
    provenant_os::fixture::run_git(
        &upstream,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    provenant_os::fixture::run_git(&upstream, &["commit", "--allow-empty", "-qm", "ahead"])?;
    let merge_base = String::from_utf8(provenant_os::fixture::run_git(
        &upstream,
        &["merge-base", &base, "HEAD"],
    )?)?;
    assert_eq!(
        merge_base.trim(),
        base,
        "the advanced origin must descend from the peer's base"
    );
    provenant_os::fixture::run_git(&upstream, &["push", "origin", "HEAD:refs/heads/main"])?;
    let ahead = String::from_utf8(provenant_os::fixture::run_git(
        &upstream,
        &["rev-parse", "HEAD"],
    )?)?;
    let ahead = ahead.trim().to_owned();
    assert_ne!(ahead, base);
    assert!(
        provenant_os::fixture::run_git(&rig.project, &["cat-file", "-e", &ahead]).is_err(),
        "the peer clone must start without the pinned commit"
    );

    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let factory = Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &Free,
    };
    let facts = provenant_confinement::worktrees::RepoFacts::default();
    let cancel = CancelToken::new();

    // Resolution fetches the missing commit from origin, then pins it.
    let resolved = factory.resolve_base(&factory.cwd, &ahead, &facts, &cancel)?;
    assert_eq!(resolved.to_string(), ahead);
    assert!(
        provenant_os::fixture::run_git(&rig.project, &["cat-file", "-e", &ahead]).is_ok(),
        "the fetch must bring the pinned commit into the peer clone"
    );

    // A commit still missing after the fetch is a host-state refusal naming the
    // clone and the fetch outcome, never an invalid-input error.
    let absent = "f".repeat(40);
    let error = factory
        .resolve_base(&factory.cwd, &absent, &facts, &cancel)
        .expect_err("an absent commit must be refused");
    match &error {
        FacadeError::BaseFetchFailed { clone, fetch } => {
            assert_eq!(clone, &rig.project.to_string_lossy().into_owned());
            assert!(fetch.contains("origin"), "fetch outcome: {fetch}");
        }
        other => panic!("expected a host-state base failure, got {other:?}"),
    }
    let diagnostic = error.diagnostic();
    let text = format!(
        "{} {}",
        diagnostic.detail.as_deref().unwrap_or_default(),
        diagnostic.next.as_deref().unwrap_or_default()
    );
    assert!(text.contains("peer clone"), "diagnostic: {text}");
    assert!(text.contains("origin"), "diagnostic: {text}");
    assert_ne!(error.code(), "invalid_input");
    assert_eq!(
        provenant_cli::fabric::facade_outcome(&error),
        (Outcome::Refused, provenant_run::ErrorKind::BaseUnreachable)
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#publication-boundary
#[cfg(test)]
#[test]
fn cancelled_base_fetch_keeps_the_cancellation() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{Occupancy, WorkerObserver};
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };
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
    let rig = crate::worktree_tests::repository()?;
    provenant_os::fixture::private_state_dir(&rig.state)?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let factory = Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock::new(),
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        workers: &Free,
    };
    let cancel = CancelToken::new();
    let calls = AtomicUsize::new(0);
    let resolve = || -> Result<provenant_core::GitOid, FacadeError> {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            // Cancellation arrives after the base was found unresolvable and
            // while the origin fetch is in flight. The retry observes it.
            cancel.cancel();
            Err(FacadeError::invalid(InvalidInputDetail::UnresolvableBase(
                "deadbeef".into(),
            )))
        } else {
            assert!(
                cancel.is_cancelled(),
                "the retry must observe the cancellation raised during the fetch"
            );
            Err(FacadeError::Cancelled)
        }
    };
    let error = factory
        .resolve_base_with(&rig.project, resolve, &cancel)
        .expect_err("a cancellation during the fetch must be preserved");
    assert!(
        matches!(error, FacadeError::Cancelled),
        "cancellation must not be recast as a fetch refusal: {error:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // The same holds for any other retry failure: only a still-unresolvable
    // base becomes a host-state fetch refusal.
    let cancel = CancelToken::new();
    cancel.cancel();
    let calls = AtomicUsize::new(0);
    let resolve = || -> Result<provenant_core::GitOid, FacadeError> {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(FacadeError::invalid(InvalidInputDetail::UnresolvableBase(
                "deadbeef".into(),
            )))
        } else {
            Err(FacadeError::Timeout)
        }
    };
    let error = factory
        .resolve_base_with(&rig.project, resolve, &cancel)
        .expect_err("other retry failures must be preserved");
    assert!(
        matches!(error, FacadeError::Timeout),
        "an unrelated retry failure must pass through: {error:?}"
    );
    Ok(())
}

#[cfg(test)]
#[test]
fn graph_document_decoder_checks_header_before_typed_graph() {
    for version in [1, 2] {
        let source = format!(r#"{{"schema_version":{version},"graph_id":"test","nodes":[]}}"#);
        let document = decode_graph_document(source.as_bytes()).unwrap();
        assert_eq!(document.schema_version, version);
    }
    assert!(matches!(
        decode_graph_document(br#"{"schema_version":3,"nodes":"future shape"}"#),
        Err(FacadeError::UpgradeRequired { version: 3, .. })
    ));
    assert!(decode_graph_document(br#"{"schema_version":1,"schema_version":2}"#).is_err());
    assert!(decode_graph_document(br#"{"schema_version":1,"nodes":"invalid"}"#).is_err());
    assert!(decode_graph_document(br#"{"graph_id":"test","nodes":[]}"#).is_err());
}

#[cfg(test)]
#[test]
fn scratch_prepare_lock_timeout_keeps_a_fixed_cause() {
    use provenant_confinement::worktrees::WorktreeError;
    let error = scratch_prepare_error(WorktreeError::Filesystem(
        provenant_os::fs::FsError::LockTimeout {
            path: "info/provenant-exclude.lock".into(),
            timeout: std::time::Duration::ZERO,
        },
    ));
    assert!(error.to_string().contains("prepare project scratch"));
    assert!(error.to_string().contains(".provenant/scratch"));
    assert_eq!(error.code(), "io");
    assert!(error.cause().is_some());
    let projection = error.diagnostic();
    assert_eq!(projection.detail.as_deref(), error.cause());
    assert!(matches!(error, FacadeError::Io { path: Some(path), .. }
        if path == ".provenant/scratch"));
}

#[test]
fn prompt_limit_refusal_keeps_safe_field_and_existing_code() {
    let error = run_error(provenant_run::RunError::PromptTooLarge { limit: 1_048_576 });
    let projection = error.diagnostic();
    assert_eq!(projection.code, "invalid_input");
    assert!(matches!(error, FacadeError::InvalidInput {
        detail: Some(provenant_fabric_runtime::coordination::facade::InvalidInputDetail::FieldTooLarge { ref field, limit: 1_048_576 })
    } if field == "prompt"));
    let detail = projection.detail.unwrap();
    assert_eq!(
        detail,
        "invalid input field `prompt`: exceeds 1048576 bytes"
    );
    assert_eq!(
        projection.next.as_deref(),
        Some("check the named input field and retry")
    );
}

#[test]
fn peer_output_binding_is_consumed_before_the_cli_projection() {
    let origin = provenant_records::WorkKey::new(
        provenant_core::ProjectId::from_bytes([7; 16]),
        "origin",
        "worker",
        1,
    )
    .unwrap();
    let output = serde_json::from_value(serde_json::json!({
        "kind":"chunk", "target":{"attempt":"11111111111111111111111111111111"},
        "part":"result", "state":"succeeded", "reason":null, "offset":0,
        "next_offset":2, "eof":true, "digest":provenant_core::Digest::from_bytes([2; 32]),
        "bytes":{"encoding":"utf8","data":"ok"}, "length":2,
        "planner_fallback":"bounded planner failure",
        "peer_origin_work_key":origin,
        "peer_graph":{"host":provenant_core::HostId::from_bytes([3;16]),"graph_id":"peer-graph"},
        "peer_origin_context":{"graph_id":"origin","node_id":"worker","attempt":1},
        "peer_graph_collected":true,
        "peer_subtree":{"status":"complete","outcome":"succeeded","next_node":null,
            "checkpoint":{"schema_version":1,"graph_id":"peer-graph","plan_digest":"digest","cancelled":false,"nodes":[]}}
    }))
    .unwrap();
    let checked = peer::checked_output_origin(output, &origin).unwrap();
    let OutputResult::Chunk {
        extra,
        peer,
        length,
        planner_fallback,
        ..
    } = checked
    else {
        panic!("chunk expected")
    };
    assert!(extra.is_empty());
    assert_eq!(length, Some(2));
    assert_eq!(planner_fallback.as_deref(), Some("bounded planner failure"));
    assert_eq!(peer, Default::default());
    assert!(
        extra
            .keys()
            .all(|key| !key.starts_with("peer_") && !key.starts_with("graph_"))
    );
}

#[test]
fn peer_output_length_round_trips_and_rejects_malformed_values() {
    let mut value = serde_json::json!({
        "kind":"chunk", "target":{"attempt":"11111111111111111111111111111111"},
        "part":"result", "state":"succeeded", "reason":null, "offset":0,
        "next_offset":0, "eof":true,
        "digest":provenant_core::Digest::from_bytes([2; 32]),
        "bytes":{"encoding":"utf8","data":""}, "length":17,
        "planner_fallback":"planner failed", "future_output_field":{"kept":true}
    });
    let chunk: OutputResult = serde_json::from_value(value.clone()).unwrap();
    let encoded = serde_json::to_value(chunk).unwrap();
    assert_eq!(encoded["length"], 17);
    assert_eq!(encoded["planner_fallback"], "planner failed");
    assert_eq!(
        encoded["future_output_field"],
        serde_json::json!({"kept":true})
    );

    value["length"] = serde_json::json!("unknown");
    assert!(serde_json::from_value::<OutputResult>(value).is_err());
}

#[test]
fn dirty_writer_refusal_names_counted_paths_and_keeps_a_safe_action_on_the_wire() {
    // The typed path summary survives the wire and reaches the operator; a
    // terminal control in a filename is escaped, not emitted.
    let paths = "first.rs, src/second.rs and 17 more";
    let error = run_error(provenant_run::RunError::Writer(
        provenant_confinement::worktrees::WorktreeError::Dirty(format!("{paths}\u{1b}")),
    ));
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    let diagnostic = decoded.diagnostic();
    let detail = diagnostic.detail.as_deref().unwrap();
    assert!(detail.contains("uncommitted"), "{detail}");
    assert!(detail.contains(paths), "{detail}");
    assert!(!detail.contains('\u{1b}'), "{detail}");
    assert!(diagnostic.next.as_deref().unwrap().contains("commit"));

    // The free-form reason remains untrusted: it is suppressed even when the
    // typed fact names the paths.
    let error = FacadeError::DispatchRefused {
        fact: Some(
            provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DirtyWriter {
                paths: paths.into(),
            },
        ),
        disk: None,
        reason: "TOKEN_SENTINEL".into(),
    };
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    let detail = decoded.diagnostic().detail.unwrap();
    assert!(detail.contains(paths), "{detail}");
    assert!(!detail.contains("TOKEN_SENTINEL"), "{detail}");

    let error = run_error(provenant_run::RunError::Writer(
        provenant_confinement::worktrees::WorktreeError::CheckedOut,
    ));
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    let diagnostic = decoded.diagnostic();
    assert!(
        diagnostic
            .detail
            .as_deref()
            .unwrap()
            .contains("checked out")
    );
    let next = diagnostic.next.as_deref().unwrap();
    assert!(next.contains("provenant worktree list"));
    assert!(next.contains("--worktree"));
}

#[test]
fn memory_pressure_refusal_keeps_source_and_resource_action() {
    use provenant_records::diagnostic::{Layer, PressureLevel, PressureThreshold};
    let error = run_error(provenant_run::RunError::Admission(
        provenant_run::AdmissionError::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: Layer::Host,
        },
    ));
    let projection = error.diagnostic();
    assert_eq!(projection.code, "MEMORY_PRESSURE");
    assert!(projection.detail.unwrap().contains("host configuration"));
    assert!(projection.next.unwrap().contains("memory"));

    let fail = provenant_cli::run::Fail::from(provenant_run::RunError::Admission(
        provenant_run::AdmissionError::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: Layer::Host,
        },
    ));
    let error = dispatch_refused(fail);
    let wire = serde_json::to_vec(&error).unwrap();
    let decoded: FacadeError = serde_json::from_slice(&wire).unwrap();
    let projection = decoded.diagnostic();
    assert_eq!(projection.code, "dispatch_refused");
    assert!(projection.detail.unwrap().contains("host configuration"));
    assert!(projection.next.unwrap().contains("memory"));
}

#[test]
fn lane_cap_refusal_retains_the_setting_through_both_adapters() {
    use provenant_records::diagnostic::{LaneLimit, Layer};
    let failure = || {
        provenant_run::RunError::Admission(provenant_run::AdmissionError::LaneCap {
            active: 3,
            limit: 3,
            kind: LaneLimit::ProjectWriters,
            origin: Layer::Host,
        })
    };
    for error in [run_error(failure()), dispatch_refused(failure().into())] {
        let decoded: FacadeError =
            serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
        let projection = decoded.diagnostic();
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("max_project_writer_lanes")
        );
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("host configuration")
        );
        assert!(
            projection
                .next
                .as_deref()
                .unwrap()
                .contains("max_project_writer_lanes")
        );
    }
}

#[test]
fn pacing_dispatch_refusal_keeps_the_route_and_monthly_window_note() {
    use provenant_records::diagnostic::{
        Diagnostic, DiagnosticFact, PacingReserveSetting, PacingSettingSource, ResourceFact,
    };
    let fact = ResourceFact::PacingReserve {
        percent: 10,
        setting: PacingReserveSetting::Account,
        origin: PacingSettingSource::Configuration,
        hold: Box::new(provenant_records::diagnostic::PacingHold {
            account: "codex/opaque-account".into(),
            window: "five_hour".into(),
            setting_path: "domains.routing.pacing.accounts.codex/opaque-account.reserve.five_hour"
                .into(),
        }),
    };
    let projection = fact
        .project(Diagnostic::new("REFUSED").category("refused"))
        .fact(DiagnosticFact::Resource(fact.clone()));
    let failure = provenant_cli::run::Fail {
        status: provenant_records::Outcome::Refused,
        kind: provenant_run::ErrorKind::Refused,
        message: "opencode/gpt-go account OpenCode Go; window=thirty_day; domains.routing.pacing.monthly (source: host configuration)".into(),
        detail: None,
        projection: Some(Box::new(projection)),
    };

    let error = dispatch_refused(failure);
    let encoded = serde_json::to_string(&error).expect("serialized refusal");
    assert!(encoded.contains("window=thirty_day"));
    assert!(encoded.contains("domains.routing.pacing.monthly"));
    let diagnostic = error.diagnostic();
    let detail = diagnostic.detail.expect("refusal detail");
    assert!(
        detail.contains("selected account is held by a 10% pacing reserve"),
        "{detail}"
    );
    assert!(detail.contains("opencode/gpt-go"), "{detail}");
    assert!(detail.contains("OpenCode Go"), "{detail}");
    assert!(detail.contains("window=thirty_day"), "{detail}");
    assert!(
        detail.contains("domains.routing.pacing.monthly"),
        "{detail}"
    );
    assert!(detail.contains("source: host configuration"), "{detail}");
}

#[test]
fn process_limit_retains_io_code_and_safe_resource_facts() {
    let failure = || {
        provenant_run::RunError::Admission(provenant_run::AdmissionError::Processes {
            used: 950,
            limit: 1000,
        })
    };
    for (error, code) in [
        (run_error(failure()), "io"),
        (dispatch_refused(failure().into()), "dispatch_refused"),
    ] {
        let decoded: FacadeError =
            serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
        let projection = decoded.diagnostic();
        assert_eq!(projection.code, code);
        let detail = projection.detail.unwrap();
        assert!(detail.contains("RLIMIT_NPROC"));
        assert!(detail.contains("950"));
        assert!(detail.contains("1000"));
        assert!(detail.contains("100"));
        assert!(projection.next.unwrap().contains("process"));
    }
}

#[test]
fn tick_wakes_graph_without_deadline_but_respects_future_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(serde_json::json!({
            "schema_version": 2, "graph_id": "waiter", "plan_digest": "plan", "nodes": [
                {"id":"branch", "worktree":null, "state":"running", "attempts":[{
                    "key":{"graph_id":"waiter", "node_id":"branch", "route_index":0, "attempt": 1},
                    "route":{"adapter":"fixture", "model":"fixture/model"},
                    "state":"running", "launched":true, "launch_actor":"actor"
                }]}
            ]
        }))?;
    let now = provenant_core::WallTime::from_micros(1_000_000)?;
    assert!(graph_wake_due(&checkpoint, now));
    checkpoint.nodes[0].state = provenant_fabric_runtime::graph::NodeState::Pending;
    checkpoint.wake_at = Some(provenant_core::WallTime::from_micros(2_000_000)?);
    assert!(!graph_wake_due(&checkpoint, now));
    assert!(graph_wake_due(&checkpoint, checkpoint.wake_at.unwrap()));
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes these helpers without libtest.
pub(crate) mod peer_tests {
    use super::*;
    #[allow(unused_imports)]
    // The worktree driver includes this module without libtest functions.
    use provenant_confinement::worktrees;
    use provenant_core::AttemptId;
    use provenant_run::{AttemptState, Outcome, RunStore};

    fn graph_test_context(
        key: &provenant_fabric_runtime::graph::AttemptKey,
    ) -> provenant_records::run::WorkContext {
        // Graph advance stamps the work key's one-based launch order.
        provenant_records::run::WorkContext {
            graph_id: Some(key.graph_id.clone()),
            node_id: Some(key.node_id.clone()),
            attempt: Some(key.attempt),
            ..Default::default()
        }
    }

    /// `--exact` matching nothing (a renamed test) exits 0; that must fail.
    pub(crate) fn assert_one_test_ran(name: &str, stdout: &[u8]) {
        let stdout = String::from_utf8_lossy(stdout);
        assert!(
            stdout.lines().any(|line| line == "running 1 test"),
            "no test matched `{name}`:\n{stdout}"
        );
    }

    #[test]
    fn exact_host_dispatch_rejects_zero_matches() {
        assert_one_test_ran("case", b"running 1 test\n");
        assert!(
            std::panic::catch_unwind(|| assert_one_test_ran("case", b"running 0 tests\n")).is_err()
        );
    }

    const HOST_TEST_CEILING: std::time::Duration = std::time::Duration::from_secs(120);

    // The host test forwards its child's stderr as the harness's own report.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn host_test(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        if !provenant_cli::fabric::in_lane() {
            return Ok(false);
        }
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            name.into(),
            "--nocapture".into(),
        ];
        let removed = [
            provenant_core::RUN_STATE_DIRECTORY_ENV,
            provenant_core::consumer_env::LANE_DEPTH,
            provenant_core::consumer_env::PARENT_ATTEMPT_ID,
        ];
        let fixture_home = tempfile::tempdir()?;
        let isolate_home = name.ends_with("settlement_defers_adopted_writer_cleanup");
        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !removed.iter().any(|name| key == name) && !(isolate_home && key == "HOME")
            })
            .collect();
        if isolate_home {
            environment.push(("HOME".into(), fixture_home.path().as_os_str().to_owned()));
        }
        let cwd = std::env::current_dir()?;
        // A hang ceiling, not a latency budget: the fixtures provision real
        // repositories, so their duration scales with host load. A per-test
        // table of longer ceilings only moved the flake to the next test.
        let mut options = ExecOptions::new(&argv, &cwd, HOST_TEST_CEILING);
        options.env = Some(&environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        // Keep native-backend skip evidence visible through host isolation.
        use std::io::Write;
        std::io::stderr().lock().write_all(&output.stderr.bytes)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert_one_test_ran(name, &output.stdout.bytes);
        Ok(true)
    }

    struct FreeWorkers;
    impl WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }

    pub(crate) struct InboxSeedOwner<'a> {
        actor: String,
        pub(crate) at: provenant_core::WallTime,
        inbox: &'a provenant_fabric_runtime::inbox::InboxService<'a>,
        cancel: &'a provenant_core::CancelToken,
    }

    impl<'a> InboxSeedOwner<'a> {
        pub(crate) fn new(
            actor: String,
            at: provenant_core::WallTime,
            inbox: &'a provenant_fabric_runtime::inbox::InboxService<'a>,
            cancel: &'a provenant_core::CancelToken,
        ) -> Self {
            Self {
                actor,
                at,
                inbox,
                cancel,
            }
        }
    }

    impl provenant_fabric_runtime::graph::AttemptOwner for InboxSeedOwner<'_> {
        type Error = FacadeError;

        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }

        fn actor(&self) -> String {
            self.actor.clone()
        }

        fn wall_now(&self) -> Option<provenant_core::WallTime> {
            Some(self.at)
        }

        fn file_ask(
            &mut self,
            filing: provenant_fabric_runtime::graph::AskFiling<'_>,
        ) -> Result<provenant_fabric_runtime::graph::AskFiled, Self::Error> {
            crate::fabric::owner::graph_ask_file(self.inbox, &filing, self.cancel)
        }

        fn observe_or_start(
            &mut self,
            _: &provenant_fabric_runtime::graph::AttemptKey,
            _: provenant_fabric_runtime::graph::NodeInput<'_>,
            _: &provenant_fabric_runtime::graph::GraphRoute,
            _: provenant_fabric_runtime::graph::AccessMode,
            _: Option<&str>,
            _: provenant_fabric_runtime::graph::Replay<'_>,
        ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
            Ok(provenant_fabric_runtime::graph::AttemptObservation::Pending { attempt_ref: None })
        }
    }

    mod loop_repair_tests {
        include!("loop_repair_tests.rs");
    }

    mod tick_tests {
        include!("tick_tests.rs");
    }

    mod peer_graph_tests {
        include!("peer_graph_tests.rs");
    }

    pub(crate) struct SettlementFixture {
        pub(crate) rig: crate::worktree_tests::support::TestRig,
        pub(crate) roots: provenant_core::Roots,
        pub(crate) store: RunStore,
        pub(crate) settled: provenant_run::HolderContext,
        pub(crate) worktree: PathBuf,
    }

    fn publish_success(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        publish_outcome(rig, store, record, head, Outcome::Succeeded, call)
    }

    fn publish_outcome(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        outcome: Outcome,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        publish_routed(rig, store, record, head, outcome, "fixture/model", call)
    }

    fn publish_routed(
        rig: &crate::worktree_tests::support::TestRig,
        store: &RunStore,
        record: &provenant_run::AttemptRecord,
        head: provenant_core::GitOid,
        outcome: Outcome,
        model: &str,
        call: &provenant_run::Request<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let attempt = rig
            .state
            .join("attempts")
            .join(record.attempt_id.to_string());
        let directory = provenant_os::fs::Directory::open_anchor(&attempt)?;
        use provenant_records::custody::Scope;
        use provenant_supervisor::custody::{
            ExitObservation, PublicationState, SettlementEvidence,
        };
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope: Scope {
                attempt: record.attempt_id,
                generation: record.generation,
            },
            termination: Some(provenant_os::exec::Termination::Exit(0)),
            never_spawned: false,
            launch_refusal: None,
            stop_reason: None,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            cancel_delivered_before_exit: None,
            provider_identity_conflict: false,
            provider_identity_evidence_incomplete: false,
            provider_identity_eof_fragment: false,
            omitted_spared: 0,
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: false,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
            stdout: Default::default(),
            stderr: Default::default(),
        };
        let mut exit_wire = serde_json::to_value(&exit)?;
        let object = exit_wire.as_object_mut().ok_or("exit object")?;
        object.remove("publication");
        object.insert("schema_version".into(), 1.into());
        for (name, wire) in [
            ("process-exit.json", exit_wire),
            (
                "writer-terminal.json",
                serde_json::json!({"schema_version":1,"head":head,"protected_paths":[]}),
            ),
        ] {
            directory
                .atomic_write(
                    std::ffi::OsStr::new(name),
                    ".fixture-",
                    &serde_json::to_vec(&wire)?,
                    provenant_os::fs::PublishMode::NoReplace,
                )?
                .require_durable()?;
        }
        drop(directory.lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?);
        let route = provenant_run::RouteRecord {
            provider: model.split_once('/').map(|(adapter, _)| adapter.into()),
            model: Some(model.into()),
            ..Default::default()
        };
        store.publish_terminal(
            record.attempt_id,
            record.revision,
            &provenant_run::TerminalPublication {
                handoff: Ok(None),
                terminal_cause: None,
                terminal_note: None,
                outcome,
                finished_at: call.clock.wall_now()?,
                started_at: None,
                provider_ms: None,
                reason: None,
                result: "OK",
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

    impl SettlementFixture {
        pub(crate) fn resumed() -> Result<Self, Box<dyn std::error::Error>> {
            Self::resumed_as("settlement-resume")
        }

        /// A settled lane whose head is one commit past its base.
        pub(crate) fn resumed_with_lane_commit() -> Result<Self, Box<dyn std::error::Error>> {
            Self::build("settlement-resume", None, "lane", true, None)
        }

        pub(crate) fn resumed_with_integrated_lane_commit()
        -> Result<Self, Box<dyn std::error::Error>> {
            let delivery = provenant_run::HostDeliveryContext::destination(
                provenant_run::DeliveryMode::Integrate("origin/trunk".into()),
                "resumed-lane",
                &provenant_run::forge_for("origin", "main", &[]),
            )?
            .ok_or("integration delivery")?;
            Self::build("settlement-resume", None, "lane", true, Some(delivery))
        }

        fn resumed_as(operation: &str) -> Result<Self, Box<dyn std::error::Error>> {
            Self::settled_fixture(operation, None)
        }

        fn settled_fixture(
            operation: &str,
            graph_id: Option<&str>,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            Self::build(operation, graph_id, "lane", false, None)
        }

        /// A settled attempt admitted under `graph`'s `node`, attempt 1: the
        /// work key a graph binding this attempt to that node must carry.
        pub(crate) fn for_node(
            graph: &str,
            node: &str,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            Self::build(&format!("settled-{graph}"), Some(graph), node, false, None)
        }

        /// Dispatch another attempt in this store under `graph`'s `node`,
        /// attempt 1, from the fixture's lane head. The caller publishes its
        /// terminal state.
        pub(crate) fn dispatch_keyed(
            &self,
            operation: &str,
            graph: &str,
            node: &str,
            call: &provenant_run::Request<'_>,
        ) -> Result<
            (provenant_run::AttemptRecord, provenant_core::GitOid),
            Box<dyn std::error::Error>,
        > {
            let head = self
                .store
                .record(self.settled.attempt_id, call)?
                .head
                .ok_or("head")?;
            let run = provenant_run::Service::new(&self.store).dispatch(
                provenant_run::DispatchRequest {
                    chair: None,
                    operation_id: operation.parse()?,
                    lane_depth: 1,
                    parent_attempt: None,
                    task: provenant_run::DispatchTask {
                        source_head: None,
                        context: Some(provenant_records::run::WorkContext {
                            graph_id: Some(graph.into()),
                            node_id: Some(node.into()),
                            attempt: Some(1),
                            ..Default::default()
                        }),
                        build_heavy: false,
                        title: None,
                        prompt: "reply OK".into(),
                        base: Some(head.to_string()),
                        fixture: None,
                        pinned_base: None,
                        head: None,
                        profile: Default::default(),
                        route: provenant_run::RequestedRoute::Model {
                            required_family: None,
                            extra: Default::default(),
                            provider: "fixture".into(),
                            model: "fixture/model".into(),
                            effort: None,
                        },
                        account: None,
                        timeout_seconds: 30,
                        delivery_id: None,
                        deliver: None,
                        worktree: Some(self.worktree.clone()),
                        access: Default::default(),
                    },
                },
                &self.rig.project,
                call,
            )?;
            Ok((self.store.record(run.attempt_id, call)?, head))
        }

        fn build(
            operation: &str,
            graph_id: Option<&str>,
            node: &str,
            lane_commit: bool,
            delivery: Option<provenant_run::WriterDeliveryRequest>,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            use provenant_confinement::worktrees::{
                self, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
            };
            let rig = crate::worktree_tests::support::TestRig::new()?;
            let git = provenant_os::git::git_executable().ok_or("git")?;
            for args in [
                vec!["config", "user.name", "Fixture"],
                vec!["config", "user.email", "fixture@example.invalid"],
                vec!["commit", "--allow-empty", "-qm", "base"],
            ] {
                let output = rig.execute(&git, &args, None)?;
                assert_eq!(crate::worktree_tests::support::code(&output), 0);
            }
            let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
            // A nested-lane fixture may only look up an existing binding.
            if std::env::var_os("PROVENANT_PANEL_ROUTING_FIXTURE").is_some() {
                provenant_config::bind_project(
                    &roots,
                    provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
                )?;
            }
            let identity = Context::open(roots.clone(), &rig.home, &rig.project)?.identity;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = provenant_run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let context = provenant_run::ExecutionContext {
                state_root: roots.state.clone(),
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                read_prompts: true,
            };
            let store =
                RunStore::open(&context, &call)?.with_writer_registry(roots.state.as_path());
            let info = worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    worktrees::create(
                        &worktrees::WorktreeContext {
                            workers: &FreeWorkers,
                            ..*context
                        },
                        WorktreeCreate {
                            branch: "resumed-lane".into(),
                            base: Some("HEAD".into()),
                        },
                        &cancel,
                    )
                },
            )?;
            let run = provenant_run::Service::new(&store).dispatch(
                provenant_run::DispatchRequest {
                    chair: None,
                    operation_id: if graph_id.is_some() {
                        operation
                    } else {
                        "settlement-original"
                    }
                    .parse()?,
                    lane_depth: 1,
                    parent_attempt: None,
                    task: provenant_run::DispatchTask {
                        source_head: None,
                        context: Some(provenant_records::run::WorkContext {
                            graph_id: Some(graph_id.unwrap_or("lane-resumed").to_owned()),
                            node_id: Some(node.into()),
                            attempt: Some(1),
                            ..Default::default()
                        }),
                        build_heavy: false,
                        title: None,
                        prompt: "reply OK".into(),
                        base: Some(info.head.to_string()),
                        fixture: None,
                        pinned_base: None,
                        head: None,
                        profile: Default::default(),
                        route: provenant_run::RequestedRoute::Model {
                            required_family: None,
                            extra: Default::default(),
                            provider: "fixture".into(),
                            model: "fixture/model".into(),
                            effort: None,
                        },
                        account: None,
                        timeout_seconds: 30,
                        delivery_id: None,
                        deliver: delivery,
                        worktree: Some(info.path.clone()),
                        access: Default::default(),
                    },
                },
                &rig.project,
                &call,
            )?;
            let prior = run.attempt_id;
            let prior_operation = store.attempt_operation(prior, &call)?.operation_id;
            worktrees::with_context(
                &roots,
                &rig.home,
                &rig.project,
                &clock,
                &cancel,
                |context| {
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    worktrees::save_owner(
                        &tree,
                        &WorktreeOwner {
                            attempt_id: Some(prior),
                            operation_id: Some(prior_operation.clone()),
                            branch: "resumed-lane".into(),
                            base: info.head.to_string(),
                            host: identity.host,
                            created_at: clock
                                .wall_now()
                                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                            origin: WorktreeOrigin::Lane,
                            label: None,
                            reference: None,
                        },
                    )?;
                    Ok(())
                },
            )?;
            let head = if lane_commit {
                let lane = info.path.to_str().ok_or("lane path")?;
                std::fs::write(info.path.join("lane-work.txt"), "lane work")?;
                for args in [
                    vec!["-C", lane, "add", "lane-work.txt"],
                    vec!["-C", lane, "commit", "-qm", "lane work"],
                ] {
                    let output = rig.execute(&git, &args, None)?;
                    assert_eq!(crate::worktree_tests::support::code(&output), 0);
                }
                let output = rig.execute(&git, &["-C", lane, "rev-parse", "HEAD"], None)?;
                String::from_utf8(output.stdout.bytes)?
                    .trim()
                    .parse::<provenant_core::GitOid>()?
            } else {
                info.head
            };
            if graph_id.is_some() {
                let record = store.record(prior, &call)?;
                publish_success(&rig, &store, &record, head, &call)?;
                let settled = provenant_run::HolderContext {
                    schema_version: 1,
                    roots: provenant_run::RootSeed {
                        home: rig.home.clone(),
                        checkout: rig.project.clone(),
                        state_root: rig.state.clone(),
                    },
                    project_id: identity.project,
                    actor_id: identity.inbox.actor,
                    attempt_id: record.attempt_id,
                    host_delivery: None,
                    launch: None,
                    sealed: None,
                };
                return Ok(Self {
                    rig,
                    roots,
                    store,
                    settled,
                    worktree: info.path,
                });
            }
            store.withdraw(
                prior,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
            let resume = provenant_run::Resume {
                extend: Vec::new(),
                prior,
                expected_generation: 0,
                operation_id: operation.parse()?,

                attempt_id: AttemptId::from_bytes([31; 16]),
                prompt: "continue".into(),
                created_at: clock.wall_now()?,
            };
            store.resume_fresh(&resume, "fixture", &call)?;
            let record = store.record(resume.attempt_id, &call)?;
            publish_success(&rig, &store, &record, head, &call)?;
            let settled = provenant_run::HolderContext {
                schema_version: 1,
                roots: provenant_run::RootSeed {
                    home: rig.home.clone(),
                    checkout: rig.project.clone(),
                    state_root: rig.state.clone(),
                },
                project_id: identity.project,
                actor_id: identity.inbox.actor,
                attempt_id: record.attempt_id,
                host_delivery: None,
                launch: None,
                sealed: None,
            };
            Ok(Self {
                rig,
                roots,
                store,
                settled,
                worktree: info.path,
            })
        }
    }

    fn integrate_graph(
        fixture: &SettlementFixture,
        id: &str,
        target: &str,
    ) -> Result<
        (
            Result<provenant_fabric_runtime::graph::GraphRun, FacadeError>,
            bool,
        ),
        Box<dyn std::error::Error>,
    > {
        use provenant_fabric_runtime::graph::FileJournal;
        let cancel = CancelToken::new();
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            workers: &FreeWorkers,
        };
        let document = fixture.rig.project.join(format!("{id}-graph.json"));
        let route = serde_json::json!({"adapter":"fixture","model":"fixture/model"});
        std::fs::write(
            &document,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "graph_id":id,
                "nodes":[
                    {"id":"one","prompt":"reply OK","access":"worktree_write",
                        "worktree":fixture.worktree,"delivery":format!("integrate:{target}"),
                        "route":route},
                    // An unrelated writer on the same worktree that never integrates.
                    {"id":"two","prompt":"reply OK","access":"worktree_write",
                        "worktree":fixture.worktree,"depends_on":["one"],
                        "route":route}
                ]
            }))?,
        )?;
        let result = factory.graph(
            GraphInput::Run {
                extra: Default::default(),
                document,
            },
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
        );
        let admitted =
            FileJournal::read_checkpoint(&fixture.roots.state, fixture.settled.project_id, id)?
                .is_some();
        Ok((result, admitted))
    }

    pub(crate) fn writer_graph_output_preserves_lens_diff_base()
    -> Result<(), Box<dyn std::error::Error>> {
        if host_test("writer_graph_output_preserves_lens_diff_base")? {
            return Ok(());
        }
        use provenant_fabric_runtime::graph::{self, AttemptOwner};
        struct Seed(String);
        impl AttemptOwner for Seed {
            type Error = std::convert::Infallible;
            fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
                provenant_config::PortTable::builtin().worktree_layout
            }
            fn actor(&self) -> String {
                "fixture".into()
            }
            fn observe_or_start(
                &mut self,
                _: &graph::AttemptKey,
                _: graph::NodeInput<'_>,
                _: &graph::GraphRoute,
                _: graph::AccessMode,
                _: Option<&str>,
                _: graph::Replay<'_>,
            ) -> Result<graph::AttemptObservation, Self::Error> {
                Ok(graph::AttemptObservation::Pending {
                    attempt_ref: Some(self.0.clone()),
                })
            }
        }
        let fixture = SettlementFixture::resumed()?;
        let factory = Factory {
            context: Context::open(
                fixture.roots.clone(),
                &fixture.rig.home,
                &fixture.rig.project,
            )?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: fixture.rig.home.clone(),
            cwd: fixture.rig.project.clone(),
            assets: fixture.rig.project.clone(),
            host_launch: None,
            workers: &FreeWorkers,
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
        let call = provenant_run::Request {
            clock: &factory.clock,
            cancel: &cancel,
        };
        let git = provenant_os::git::git_executable().ok_or("git")?;
        let node_source = provenant_run::resolve_git_base(&git, &fixture.rig.project, "HEAD")?
            .parse::<provenant_core::GitOid>()?;

        // 1. Dispatch writer attempt 1 on fixture.worktree with base = node_source.
        let dispatch = provenant_run::Service::new(&fixture.store).dispatch(
            provenant_run::DispatchRequest {
                chair: None,
                operation_id: "writer-output-op".parse()?,
                lane_depth: 1,
                parent_attempt: None,
                task: provenant_run::DispatchTask {
                    source_head: None,
                    context: Some(provenant_records::run::WorkContext {
                        // The work key the graph's writer node binds below.
                        graph_id: Some("writer-output-lens-graph".into()),
                        node_id: Some("writer".into()),
                        attempt: Some(1),
                        ..Default::default()
                    }),
                    build_heavy: false,
                    title: Some("writer".into()),
                    prompt: "implement slice".into(),
                    base: Some(node_source.to_string()),
                    fixture: None,
                    pinned_base: None,
                    head: Some(node_source.to_string()),
                    profile: Default::default(),
                    route: provenant_run::RequestedRoute::Model {
                        required_family: None,
                        extra: Default::default(),
                        provider: "claude".into(),
                        model: "claude/test-model".into(),
                        effort: None,
                    },
                    account: None,
                    timeout_seconds: 30,
                    delivery_id: None,
                    deliver: None,
                    worktree: Some(fixture.worktree.clone()),
                    access: Default::default(),
                },
            },
            &fixture.rig.project,
            &call,
        )?;
        let attempt_1 = dispatch.attempt_id;
        let attempt_1_record = fixture.store.record(attempt_1, &call)?;
        // This graph adopts the fixture's idle lane. Bind its owner to the
        // writer attempt rather than the unrelated resume fixture lineage.
        let writer = provenant_run::prepare_writer(
            &provenant_run::WriterRequest {
                extra: Default::default(),
                custody_project: Some(fixture.settled.project_id),
                project: fixture.rig.project.clone(),
                worktree: fixture.worktree.clone(),
            },
            fixture.roots.state.as_path(),
            fixture.settled.project_id,
        )?;
        let mut lane_owner = worktrees::load_owner(&writer.worktree)?.ok_or("lane owner")?;
        lane_owner.attempt_id = Some(attempt_1);
        lane_owner.operation_id = Some(
            fixture
                .store
                .attempt_operation(attempt_1, &call)?
                .operation_id,
        );
        worktrees::save_owner(&writer.worktree, &lane_owner)?;

        let worktree_str = fixture.worktree.to_str().ok_or("worktree path")?;

        // The writer commits a subject distinct from its graph source.
        std::fs::write(
            fixture.worktree.join("writer-edited"),
            "writer fixture completed",
        )?;
        std::fs::write(fixture.worktree.join("first.txt"), b"first edit\n")?;
        let output = fixture.rig.execute(
            &git,
            &["-C", worktree_str, "add", "first.txt", "writer-edited"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let output = fixture.rig.execute(
            &git,
            &["-C", worktree_str, "commit", "-m", "first commit"],
            None,
        )?;
        assert_eq!(crate::worktree_tests::support::code(&output), 0);
        let head_1 = provenant_run::resolve_git_base(&git, &fixture.worktree, "HEAD")?
            .parse::<provenant_core::GitOid>()?;

        publish_outcome(
            &fixture.rig,
            &fixture.store,
            &attempt_1_record,
            head_1,
            Outcome::Succeeded,
            &call,
        )?;

        // 2. Set up graph with writer node and dependent lens node.
        let graph_id = "writer-output-lens-graph";
        let mut document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": graph_id,
            "nodes": [
                {
                    "id": "writer",
                    "prompt": "write",
                    "access": "worktree_write",
                    "worktree": fixture.worktree,
                    "source": { "type": "commit", "commit": node_source.to_string() },
                    "route": { "adapter": "fixture", "model": "fixture/model" }
                },
                {
                    "id": "lens",
                    "prompt": "review",
                    "access": "read_only",
                    "depends_on": ["writer"],
                    "source": { "type": "node", "node": "writer" },
                    "route": { "adapter": "fixture", "model": "fixture/model" }
                }
            ]
        }))?;

        // This fixture creates a current graph; legacy pending executions must
        // still be drained before the host admits a new dependent attempt.
        document.template_library = graph::TemplateLibrary::bundled().snapshot();

        let mut journal = graph::FileJournal::open(
            &fixture.roots.state,
            fixture.settled.project_id,
            graph_id,
            budget,
            &cancel,
            &factory.clock,
        )?;
        journal.bind(graph::GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: fixture.rig.project.clone(),
            document: document.clone(),
        })?;

        // Seed attempt_1 for the writer node in the graph checkpoint.
        let mut seed = Seed(format!("{}/{}", factory.context.identity.host, attempt_1));
        graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;

        // Now advance with BoundGraphOwner.
        let mut owner = BoundGraphOwner {
            writer_launch_budget: None,
            admission_timeout: None,
            launch_warnings: Vec::new(),
            repo_facts: Default::default(),
            factory: &factory,
            integration_base: GraphBaseBinding::new(None, Some(node_source)),
            integration_branch: None,
            network: None,
            capability_overrides: Default::default(),
            exclude: Vec::new(),
            budget,
            cancel: &cancel,
            prices: None,
            tick: false,
        };
        let run = graph::advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;

        // 3. Verification:
        // The graph observes the writer output directly.
        let writer_progress = &run.checkpoint.nodes[0];
        assert_eq!(writer_progress.state, graph::NodeState::Succeeded);
        assert_eq!(
            writer_progress
                .outputs
                .as_ref()
                .and_then(|o| o.commit.as_ref()),
            Some(&head_1.to_string())
        );

        // - Lens node was submitted, and its diff base stays node_source (not head_1)
        let lens_progress = &run.checkpoint.nodes[1];
        assert_ne!(
            lens_progress.state,
            graph::NodeState::Blocked,
            "{lens_progress:?}"
        );
        let lens_attempt_ref = lens_progress
            .attempts
            .first()
            .and_then(|a| a.attempt_ref.as_ref())
            .ok_or("lens attempt_ref")?;
        let (_, lens_attempt_id) = lens_attempt_ref.split_once('/').ok_or("attempt split")?;
        let lens_record = fixture.store.record(lens_attempt_id.parse()?, &call)?;
        let lens_request = fixture.store.request(lens_record.attempt_id, &call)?;
        let lens_task = lens_request.task;

        let node_source_str = node_source.to_string();
        let head_1_str = head_1.to_string();
        assert_eq!(
            lens_task.base.as_deref(),
            Some(node_source_str.as_str()),
            "lens diff base must stay the node source"
        );
        assert_ne!(
            lens_task.base.as_deref(),
            Some(head_1_str.as_str()),
            "lens diff base must not advance to the writer output"
        );
        assert_eq!(
            lens_task.head.as_deref(),
            Some(head_1_str.as_str()),
            "lens subject commit must be the writer output"
        );

        Ok(())
    }
    mod peer_graph_lifecycle {
        include!("peer_graph_lifecycle.rs");
    }
    mod peer_graph_settlement {
        include!("peer_graph_settlement.rs");
    }
    mod peer_graph_routing {
        include!("peer_graph_routing.rs");
    }
    mod peer_graph_submission {
        include!("peer_graph_submission.rs");
    }
    mod peer_writer_delivery {
        include!("peer_writer_delivery.rs");
    }
}

#[test]
fn fallback_cause_uses_only_unanimous_typed_facts() {
    use provenant_records::{
        RouteExclusionReason as Cause,
        run::{SkipKind, SkippedRoute},
    };
    let route = |cause| SkippedRoute {
        skipped_accounts: Vec::new(),
        extra: Default::default(),
        route: "/private/token".into(),
        kind: SkipKind::Unavailable,
        plan_error: None,
        guard_boundary: None,
        guard_resource: None,
        confinement_guard: None,
        continuation_guard: None,
        cause,
        offer_mismatch: None,
        detail: "prompt token \u{1b} /private/secret".into(),
    };
    let first = route(Some(Cause::StaleAvailability));
    let first: SkippedRoute = serde_json::from_slice(&serde_json::to_vec(&first).unwrap()).unwrap();
    assert_eq!(
        unanimous_preparation_cause(std::slice::from_ref(&first)),
        Some(Cause::StaleAvailability)
    );
    assert_eq!(
        unanimous_preparation_cause(&[first.clone(), route(None)]),
        None
    );
    assert_eq!(
        unanimous_preparation_cause(&[first, route(Some(Cause::AuthRequired))]),
        None
    );
    assert_eq!(unanimous_preparation_cause(&[]), None);
}

#[test]
fn untyped_dispatch_refusal_retains_projection_or_message_locally() {
    use provenant_records::diagnostic::Diagnostic;
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    for projection in [
        None,
        Some(Box::new(Diagnostic::new("fixture").detail(format!(
            "setup script failed: missing dependency {secret}"
        )))),
    ] {
        let fail = provenant_cli::run::Fail {
            status: provenant_records::Outcome::Refused,
            kind: provenant_run::ErrorKind::Refused,
            message: format!("setup script failed: missing dependency {secret}"),
            detail: None,
            projection,
        };
        let error = dispatch_refused(fail);
        let wire = serde_json::to_string(&error).unwrap();
        let decoded: FacadeError = serde_json::from_str(&wire).unwrap();
        let diagnostic = decoded.diagnostic();
        assert!(
            diagnostic
                .detail
                .unwrap()
                .contains("setup script failed: missing dependency")
        );
        assert!(wire.contains(secret));
        assert!(!diagnostic.next.unwrap().contains("receipt"));
    }
}

#[test]
fn confinement_refusal_keeps_actual_cause_through_cli_and_dispatch() {
    use provenant_confinement::permissions::PermissionError;
    let fail = provenant_cli::run::Fail::from(provenant_run::RunError::Confinement(
        PermissionError::AnchorMismatchAt {
            path: "/fixture/worktree".into(),
            stage: "setup anchor changed ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        },
    ));
    assert!(fail.message.contains("setup anchor changed"));
    assert!(
        fail.message
            .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
    );
    let error = dispatch_refused(fail);
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    assert!(
        decoded
            .diagnostic()
            .detail
            .unwrap()
            .contains("setup anchor changed")
    );
}

#[test]
fn managed_writer_path_mismatch_names_the_layout_and_repair() {
    let error = run_error(provenant_run::RunError::Writer(
        provenant_confinement::worktrees::WorktreeError::ManagedPathMismatch {
            requested: PathBuf::from("/tmp/project/.worktrees/linked/.worktrees/fix-parser"),
            expected: PathBuf::from("/tmp/project/.worktrees/fix-parser"),
        },
    ));
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.code, "dispatch_refused");
    assert!(diagnostic.detail.as_deref().is_some_and(|detail| {
        detail.contains("does not match") && detail.contains("configured project")
    }));
    let detail = diagnostic.detail.as_deref().unwrap();
    assert!(detail.contains("--branch"));
    assert!(detail.contains("--worktree"));
}

#[cfg(test)]
#[test]
fn failed_worktree_add_retains_safe_operation_and_lock_facts_through_relay()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{
        self, GitMutationOperation, Occupancy, WorkerObserver, WorktreeCreate,
    };
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
    let project = temp.path().join("project");
    let state = temp.path().join("state");
    for path in [&home, &project, &state] {
        std::fs::create_dir_all(path)?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    provenant_os::fixture::run_git(&project, &["init", "-q"])?;
    provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
    // A real Git ref lock gives deterministic mutation failure without a timeout.
    std::fs::write(
        project.join(".git/refs/heads/refused-lane.lock"),
        b"credential-sentinel",
    )?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let error = worktrees::with_context_using_workers(
        &roots,
        &home,
        &project,
        &clock,
        &cancel,
        &Free,
        |context| {
            worktrees::create(
                context,
                WorktreeCreate {
                    branch: "refused-lane".into(),
                    base: Some("HEAD".into()),
                },
                &cancel,
            )
        },
    )
    .expect_err("Git ref lock must refuse worktree addition");
    assert_eq!(
        error.mutation_failure(),
        Some((GitMutationOperation::WorktreeAdd, true))
    );
    let facade = graph_writer_initialisation_error("HEAD", error);
    let relayed = interface::relay_error(ServiceError::Facade(facade));
    assert!(matches!(
        relayed,
        FacadeError::GitMutationFailed {
            operation: GitMutationOperation::WorktreeAdd,
            lock_failure: true,
        }
    ));
    let wire = serde_json::to_string(&relayed)?;
    assert!(!wire.contains(&project.to_string_lossy().to_string()));
    assert!(!wire.contains("credential-sentinel"));
    let restored: FacadeError = serde_json::from_str(&wire)?;
    let diagnostic = restored.diagnostic();
    assert_eq!(restored.code(), "io");
    assert!(diagnostic.detail.as_deref().is_some_and(
        |detail| detail.contains("git worktree add") && detail.contains("lock failure: true")
    ));
    let untyped = interface::relay_error(ServiceError::Cli {
        code: "io".into(),
        message: "credential-sentinel".into(),
        data: serde_json::Value::Null,
    });
    let untyped_wire = serde_json::to_string(&untyped)?;
    assert!(untyped_wire.contains("credential-sentinel"));
    assert!(
        untyped
            .diagnostic()
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("credential-sentinel"))
    );
    assert!(
        !diagnostic
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("graph input")
    );
    Ok(())
}
