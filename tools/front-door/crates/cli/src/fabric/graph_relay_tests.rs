//! Unit tests for [`super::graph_relay`], moved verbatim from the parent file.

use super::*;

#[cfg(test)]
mod wait_tests {
    use super::*;

    #[test]
    fn lane_stop_is_confined_to_its_own_subtree() {
        for target in ["parent", "parent--child", "graph/parent--child--grandchild"] {
            assert!(stop_target("graph", "parent", target).is_ok());
        }
        for target in [
            "graph",
            "other",
            "parental",
            "other/parent--child",
            "parent--sibling",
        ] {
            assert!(stop_target("graph", "parent--child", target).is_err());
        }
    }

    #[test]
    fn pending_reclaims_retain_the_completed_child_wait_result() {
        let mut reclaims = 0;
        let mut waits = 0;
        let result = with_waiting(
            |waiting| {
                if waiting {
                    return Ok(());
                }
                wait_for_admission(|| {
                    reclaims += 1;
                    Ok(json!({"reclaiming": reclaims < 3}))
                })
            },
            || {
                waits += 1;
                Ok(json!({"children": [{"node": "parent--child", "result": "saved work"}]}))
            },
        )
        .unwrap();
        assert_eq!(reclaims, 3);
        assert_eq!(waits, 1);
        assert_eq!(result["children"][0]["result"], "saved work");
    }

    #[test]
    fn returning_from_a_child_wait_clears_waiting_on_success_and_error() {
        for succeeds in [true, false] {
            let mut frames = Vec::new();
            let result = with_waiting(
                |waiting| {
                    frames.push(waiting);
                    Ok(())
                },
                || {
                    if succeeds {
                        Ok(42)
                    } else {
                        Err(refused("cancelled", "child wait cancelled"))
                    }
                },
            );
            assert_eq!(frames, [true, false]);
            assert_eq!(result.is_ok(), succeeds);
            match result {
                Ok(value) => assert_eq!(value, 42),
                Err(ServiceError::Cli { code, .. }) => assert_eq!(code, "cancelled"),
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_fabric_runtime::graph::template::Compiler;

    #[test]
    fn relay_templates_apply_user_then_repository_layers() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let repo = root.path().join("repo");
        std::fs::create_dir(&user).unwrap();
        std::fs::create_dir(&repo).unwrap();
        std::fs::write(
            user.join("custom.toml"),
            "brief = 'User brief'\ntier = 'junior'\naccess = 'read_only'\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("custom.toml"),
            "brief = 'Repository brief'\ntier = 'senior'\naccess = 'read_only'\n",
        )
        .unwrap();
        let library = layered_templates(&user, &root.path().join("missing")).unwrap();
        let node = Compiler::new(&library)
            .compile(&json!({"id":"child", "use":"custom"}))
            .unwrap();
        assert_eq!(node.prompt, "User brief");
        let library = layered_templates(&user, &repo).unwrap();
        let node = Compiler::new(&library)
            .compile(&json!({"id":"child", "use":"custom"}))
            .unwrap();
        assert_eq!(node.prompt, "Repository brief");
        assert!(library.resolve("task").is_ok());
    }

    // Spec: docs/specs/fabric.md#one-model-spelling-per-route
    #[test]
    fn exact_model_tier_lowers_to_its_full_catalogue_id() {
        let catalogue = provenant_routing::Catalogue::bundled().unwrap();
        let id = "opencode/opencode/deepseek-v4.1-flash-free";
        let (key, route, warnings) = tier_route(
            "review",
            &json!(format!("{id}@low")),
            None,
            true,
            Some(&catalogue),
        )
        .unwrap();
        assert_eq!(key, "route");
        assert_eq!(route["adapter"], "opencode");
        assert_eq!(route["model"], id);
        assert_eq!(route["effort"], "low");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    // Spec: docs/specs/fabric.md#one-model-spelling-per-route
    #[test]
    fn relative_model_tier_tables_and_lists_lower_to_full_ids() {
        let catalogue = provenant_routing::Catalogue::bundled().unwrap();
        let table = json!({"adapter":"opencode", "model":"opencode/hy4-flash-free"});
        let (key, route, warnings) =
            tier_route("review", &table, None, true, Some(&catalogue)).unwrap();
        assert_eq!(key, "route");
        assert_eq!(route["model"], "opencode/opencode/hy4-flash-free");
        assert!(warnings.is_empty(), "{warnings:?}");

        // A bare model on a non-namespaced adapter still becomes a parseable
        // full id, so review-family resolution no longer fails on a bare name.
        let bare = json!({"adapter":"anthropic", "model":"claude-sonnet-5-5"});
        let (_, route, warnings) =
            tier_route("review", &bare, None, true, Some(&catalogue)).unwrap();
        assert_eq!(route["model"], "anthropic/claude-sonnet-5-5");
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].code,
            provenant_records::warning::ROUTE_MODEL_NOT_CATALOGUED.as_str()
        );
        assert!(provenant_routing::ModelId::parse(route["model"].as_str().unwrap()).is_ok());

        let (key, routes, warnings) =
            tier_route("review", &json!([table]), None, true, Some(&catalogue)).unwrap();
        assert_eq!(key, "routes");
        assert_eq!(
            routes[0]["model"], "opencode/opencode/hy4-flash-free",
            "route list members canonicalise too"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}

#[cfg(test)]
mod child_wait_tests {
    use super::*;

    #[test]
    fn child_wait_polls_until_terminal_and_clears_slot_on_cancellation() {
        for cancelled in [false, true] {
            let cancel = CancelToken::new();
            let mut frames = Vec::new();
            let mut polls = 0;
            let result = with_waiting(
                |waiting| {
                    frames.push(waiting);
                    Ok(())
                },
                || {
                    poll_children(
                        &cancel,
                        PollTiming {
                            within: std::time::Duration::from_secs(2),
                            interval: std::time::Duration::from_millis(1),
                        },
                        &provenant_os::clock::SystemClock::new(),
                        provenant_os::clock::SystemClock.mono_now(),
                        ChildWaitMode::All,
                        &mut |_| {},
                        |_| {
                            polls += 1;
                            if cancelled {
                                cancel.cancel();
                            }
                            Ok(json!({"children":[{"node":"a--b", "settled":polls == 2}]}))
                        },
                    )
                },
            );
            assert_eq!(frames, [true, false]);
            assert_eq!(result.is_ok(), !cancelled);
            assert_eq!(polls, if cancelled { 1 } else { 2 });
        }
    }

    #[test]
    fn child_wait_stops_on_the_mode_ending_condition() {
        let child = |node: &str, state: &str| json!({"node":node,"state":state,"settled":state != "running"});
        let ok = child("a", "succeeded");
        let failed = child("b", "failed");
        let running = child("b", "running");
        assert!(!ChildWaitMode::All.done(&[ok.clone(), running.clone()]));
        assert!(ChildWaitMode::Any.done(&[ok.clone(), running.clone()]));
        assert!(!ChildWaitMode::FailFast.done(&[ok.clone(), running]));
        assert!(ChildWaitMode::FailFast.done(&[ok.clone(), failed.clone()]));
        assert!(ChildWaitMode::All.done(&[ok, failed]));

        for (mode, expected_polls) in [
            (ChildWaitMode::Any, 1),
            (ChildWaitMode::FailFast, 2),
            (ChildWaitMode::All, 2),
        ] {
            let mut polls = 0;
            let result = poll_children(
                &CancelToken::new(),
                PollTiming {
                    within: std::time::Duration::from_secs(2),
                    interval: std::time::Duration::from_millis(1),
                },
                &provenant_os::clock::SystemClock::new(),
                provenant_os::clock::SystemClock.mono_now(),
                mode,
                &mut |_| {},
                |_| {
                    polls += 1;
                    let state = if polls == 1 { "running" } else { "failed" };
                    Ok(json!({"children":[child("a", "succeeded"), child("b", state)]}))
                },
            );
            assert!(result.is_ok(), "mode={mode:?}");
            assert_eq!(polls, expected_polls, "mode={mode:?}");
        }
    }

    #[test]
    fn uncertain_yield_and_timeout_still_clear_the_slot() {
        let mut frames = Vec::new();
        let result: Result<(), _> = with_waiting(
            |waiting| {
                frames.push(waiting);
                if waiting {
                    Err(refused("io", "lost reply"))
                } else {
                    Ok(())
                }
            },
            || panic!("must not wait after uncertain yield"),
        );
        assert!(result.is_err());
        assert_eq!(frames, [true, false]);
        // Time spent yielding the slot consumes the same settlement deadline.
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0).unwrap(),
            provenant_core::MonoTime::from_micros(0),
        );
        let started = clock.mono_now();
        frames.clear();
        let result = with_waiting(
            |waiting| {
                frames.push(waiting);
                if waiting {
                    clock
                        .wait_until(provenant_core::MonoTime::from_micros(2_000_000))
                        .unwrap();
                }
                Ok(())
            },
            || {
                poll_children(
                    &CancelToken::new(),
                    PollTiming {
                        within: std::time::Duration::from_secs(1),
                        interval: std::time::Duration::from_millis(1),
                    },
                    &clock,
                    started,
                    ChildWaitMode::All,
                    &mut |_| {},
                    |_| panic!("must not poll after yield consumed the deadline"),
                )
            },
        );
        assert!(matches!(result, Err(ServiceError::Cli { code, .. }) if code == "timeout"));
        assert_eq!(frames, [true, false]);
    }
}

#[cfg(test)]
mod provisioning_tests {
    use super::*;

    fn fixture_git_text(
        project: &std::path::Path,
        args: &[&str],
    ) -> Result<String, Box<dyn std::error::Error>> {
        Ok(
            String::from_utf8(provenant_os::fixture::run_git(project, args)?)?
                .trim()
                .to_owned(),
        )
    }

    fn authority_fixture()
    -> Result<(tempfile::TempDir, Context, std::path::PathBuf, String), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        fixture_git_text(&project, &["init", "-q"])?;
        fixture_git_text(&project, &["commit", "--allow-empty", "-qm", "base"])?;
        let source = fixture_git_text(&project, &["rev-parse", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let context = Context::open(
            inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
            &home,
            &project,
        )?;
        Ok((root, context, project, source))
    }

    // Spec: docs/specs/run-launch.md#admission-and-custody
    #[test]
    fn nested_provisioning_reuses_only_its_creators_exact_checkout()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_root, context, project, source) = authority_fixture()?;
        let git = |args: &[&str]| fixture_git_text(&project, args);
        let clock = provenant_os::clock::SystemClock;
        let cancel = CancelToken::new();
        provision_writer(
            &context,
            ".worktrees/unrelated",
            "unrelated",
            &source,
            WaitBudget::cli(clock.mono_now(), 5)?,
            &cancel,
        )?;
        let owner = provisioning_owner_digest("graph-one", "child")?;
        let other_owner = provisioning_owner_digest("graph-two", "child")?;
        let owned_path = ".worktrees/reserved";
        let provision = |path, branch, owner: &provenant_core::Digest| {
            provision_writer_checked(
                &context,
                path,
                branch,
                &source,
                Some(owner),
                WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                &cancel,
            )
        };
        provision(owned_path, "reserved", &owner)?;
        provision(owned_path, "reserved", &owner)?;
        assert!(provision(owned_path, "reserved", &other_owner).is_err());
        assert!(provision(".worktrees/unrelated", "unrelated", &owner).is_err());
        let owned = project.join(owned_path).canonicalize()?;
        git(&["worktree", "remove", owned.to_str().ok_or("path")?])?;
        git(&["worktree", "add", owned.to_str().ok_or("path")?, "reserved"])?;
        assert!(
            provision(owned_path, "reserved", &owner).is_err(),
            "a recycled checkout has no creator receipt"
        );
        assert!(owned.is_dir(), "refusal preserves the unrelated checkout");
        Ok(())
    }

    // Spec: docs/specs/run-launch.md#admission-and-custody
    #[test]
    fn deferred_nested_creation_retains_its_owner_before_source_resolution()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::GraphCheckpoint;
        let path = ".worktrees/deferred";
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"deferred","plan_digest":"fixture",
            "nodes":[{"id":"parent--later","state":"pending","worktree":path,"attempts":[]},
                {"id":"dependency","state":"running","worktree":null,"attempts":[]}],
            "expansions":[{"source":"parent","nodes":[{
                "id":"parent--later","access":"worktree_write","worktree":path,"provision_worktree":true,
                "source":{"type":"node","node":"dependency"},"prompt":"later"
            }]}]
        }))?;
        bind_admitted_provisioning_owners(&mut checkpoint)?;
        let owner = checkpoint.nodes[0]
            .provisioned_owner
            .ok_or("deferred writer owner")?;
        assert!(
            checkpoint.pending_writer_provisioning.is_empty(),
            "the source is not available yet"
        );
        checkpoint = serde_json::from_slice(&serde_json::to_vec(&checkpoint)?)?;
        relay::fence_provisioning(&mut checkpoint, "parent--later", path, &"a".repeat(40));
        assert_eq!(
            checkpoint.pending_writer_provisioning["parent--later"].owner,
            Some(owner)
        );
        relay::clear_provisioning(&mut checkpoint, "parent--later");
        assert_eq!(checkpoint.nodes[0].provisioned_owner, Some(owner));
        // A later admission cannot turn this target into another lane's
        // creation simply because the original effect fence was cleared.
        let mut foreign = checkpoint.expansions[0].clone();
        foreign.nodes[0].id = "other--writer".into();
        checkpoint.expansions.push(foreign);
        let mut foreign_progress = checkpoint.nodes[0].clone();
        foreign_progress.id = "other--writer".into();
        foreign_progress.provisioned_owner = None;
        checkpoint.nodes.push(foreign_progress);
        assert_eq!(
            bind_admitted_provisioning_owners(&mut checkpoint)
                .unwrap_err()
                .code(),
            "writer_parent_required"
        );
        assert_eq!(checkpoint.nodes[0].provisioned_owner, Some(owner));
        Ok(())
    }

    // Spec: docs/specs/run-launch.md#admission-and-custody
    #[test]
    fn nested_worktree_authority_cannot_cross_owners_or_checkout_incarnations()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::GraphCheckpoint;
        let (_root, context, project, source) = authority_fixture()?;
        let git = |args: &[&str]| fixture_git_text(&project, args);
        let clock = provenant_os::clock::SystemClock;
        let cancel = CancelToken::new();
        for name in ["parent", "unrelated"] {
            provision_writer(
                &context,
                &format!(".worktrees/{name}"),
                name,
                &source,
                WaitBudget::cli(clock.mono_now(), 5)?,
                &cancel,
            )?;
        }
        let forged = serve_with_read_and_parent(
            &context,
            "unused",
            json!({
                "op":"add","id":"child","prompt":"write","idempotency_key":"forged",
                "launch":{"node":{"id":"child","access":"worktree_write","worktree":".worktrees/unrelated","provision_worktree":true}}
            }),
            RelayCall {
                budget: WaitBudget::cli(clock.mono_now(), 5)?,
                cancel: &cancel,
                clock: &clock,
            },
            None,
            &|_| unreachable!("a forged provisioning flag must not perform an operation"),
        );
        assert_eq!(forged.unwrap_err().code(), "writer_parent_required");
        let parent = project.join(".worktrees/parent").canonicalize()?;
        let sealed = crate::fabric::checkout_subject(&context.roots, &parent)?;
        let checkpoint = |path: &std::path::Path| -> Result<GraphCheckpoint, serde_json::Error> {
            serde_json::from_value(json!({
                "schema_version":2,"graph_id":"sharing","plan_digest":"fixture",
                "nodes":[{"id":"parent--child","state":"pending","worktree":path,"attempts":[]}],
                "expansions":[{"source":"parent","nodes":[{
                    "id":"parent--child","access":"worktree_write","worktree":path,"prompt":"write"
                }]}]
            }))
        };
        let mut shared = checkpoint(&parent)?;
        bind_shared_writers(&context, &mut shared, Some(&sealed))?;
        let binding = shared.nodes[0]
            .writer_binding
            .as_ref()
            .ok_or("shared binding")?;
        assert_eq!(binding.worktree, parent.to_str().ok_or("path")?);
        assert!(binding.source_directory_identity.is_some());
        assert!(!shared.nodes[0].writer_binding_pending);
        // An unrelated caller cannot inherit the owner's authority by naming
        // its path, even when that checkout is temporarily unoccupied.
        assert_eq!(
            bind_shared_writers(&context, &mut checkpoint(&parent)?, None)
                .unwrap_err()
                .code(),
            "writer_parent_required"
        );
        assert_eq!(
            bind_shared_writers(
                &context,
                &mut checkpoint(&project.join(".worktrees/unrelated"))?,
                Some(&sealed)
            )
            .unwrap_err()
            .code(),
            "writer_parent_required"
        );
        // A recycled path is a different authority, including before any child
        // obtains a holder lease. The pinned directory binding also persists.
        let old_identity = binding.source_directory_identity.clone();
        git(&["worktree", "remove", parent.to_str().ok_or("path")?])?;
        git(&["worktree", "add", parent.to_str().ok_or("path")?, "parent"])?;
        let recreated = crate::fabric::checkout_subject(&context.roots, &parent)?;
        assert_ne!(recreated, sealed);
        assert_eq!(
            bind_shared_writers(&context, &mut checkpoint(&parent)?, Some(&sealed))
                .unwrap_err()
                .code(),
            "writer_parent_required"
        );
        assert_eq!(
            shared.nodes[0]
                .writer_binding
                .as_ref()
                .and_then(|binding| binding.source_directory_identity.clone()),
            old_identity
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn registry_lock_refusal_forwards_the_recorded_holder() {
        use provenant_fabric_runtime::coordination::facade::FacadeError;
        let holder = provenant_core::AttemptId::from_bytes([31; 16]);
        let error = provisioning_refusal(
            provenant_confinement::worktrees::WorktreeError::RegistryLockTimeout {
                holder: Some(holder),
            },
        );
        assert!(
            matches!(
                &error,
                provenant_fabric_runtime::relay::ServiceError::Facade(
                    FacadeError::WriterLockBusy {
                        holder: Some(recorded)
                    }
                ) if *recorded == holder
            ),
            "{error:?}"
        );
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    #[allow(clippy::disallowed_methods)] // Real-time boundary: hold the guard past the 1 s bound.
    fn concurrent_launch_waits_out_writer_lock_contention_past_the_reader_bound()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, GraphExecution, NodeState,
        };
        use provenant_os::fs::{Directory, LockMode};
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "base"])?;
        let source = git(&["rev-parse", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let mut context = Context::open(
            inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
            &home,
            &project,
        )?;
        // The reader's launch allowance is the old, shorter bound. A launch
        // that contends for a writer must wait on the provisioning allowance.
        context.config.graph.launch_wait_seconds = 1;
        context.config.graph.writer_provision_seconds = 6;
        let clock = provenant_os::clock::SystemClock;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        let reader = WaitBudget::cli(clock.mono_now(), context.config.graph.launch_wait_seconds)?;
        let writer_budget = writer_launch_budget(&context, &clock, reader);
        assert!(
            writer_budget.remaining(clock.mono_now()).as_micros() > 5_000_000,
            "the writer budget must outlast the reader launch bound"
        );
        let path = ".worktrees/waiting-writer";
        provision_writer(&context, path, "waiting-writer", &source, budget, &cancel)?;
        let slot = context
            .roots
            .state
            .as_path()
            .join("worktrees")
            .join(context.identity.project.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                // Slots are keyed by the canonical checkout path.
                &project.canonicalize()?.join(path),
            ));
        let slot = Directory::open_anchor(&slot)?;
        let held =
            slot.try_lock_existing(std::ffi::OsStr::new("writer.lock"), LockMode::Exclusive)?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"provision-contention","nodes":[{
                "id":"writer","access":"worktree_write","worktree":path,"prompt":"Write","route":{"alias":"junior"}
            }]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"provision-contention","plan_digest":"fixture","nodes":[{
                "id":"writer","state":"pending","worktree":path,"attempts":[]
            }]
        }))?;
        relay::fence_provisioning(&mut checkpoint, "writer", path, &source);
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "provision-contention",
            budget,
            &cancel,
            &clock,
        )?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: project.clone(),
            document,
        })?;
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
        journal.save(&checkpoint)?;
        let error = recover_provisioning(
            &context,
            &mut journal,
            &mut checkpoint,
            &["writer".into()],
            writer_budget,
            &cancel,
        )?
        .ok_or("contention diagnostic")?;
        // Contending for a held writer wait does not block the launch: the
        // node keeps running with a wait sized by the provisioning allowance,
        // not by the reader's shorter launch allowance.
        assert_eq!(error.code(), "writer_busy");
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        assert!(checkpoint.nodes[0].attempts.is_empty());
        let retry = checkpoint.nodes[0]
            .launch_retry
            .as_ref()
            .ok_or("contention wait")?;
        let deadline = retry.deadline.ok_or("contention deadline")?;
        assert!(retry.wake_at.is_some(), "contention schedules a wake");
        let remaining = deadline
            .as_micros()
            .saturating_sub(clock.wall_now()?.as_micros());
        assert!(
            remaining > 2_000_000,
            "waits past the reader launch bound: {remaining}us of {deadline}"
        );
        assert!(
            remaining <= 7_000_000,
            "bounded by the provisioning allowance: {remaining}us of {deadline}"
        );
        assert_eq!(
            relay::pending_provisioning(&checkpoint),
            vec![("writer".into(), path.into(), source.clone())]
        );
        assert_eq!(
            journal.load("provision-contention")?,
            Some(checkpoint.clone())
        );
        // Hold the writer guard past the reader bound, then release it. The
        // waiting launch must still be running and provision on its next wake.
        std::thread::sleep(std::time::Duration::from_millis(1_200));
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        drop(held);
        if let Some(retry) = checkpoint.nodes[0].launch_retry.as_mut() {
            retry.wake_at = None;
        }
        assert!(
            recover_provisioning(
                &context,
                &mut journal,
                &mut checkpoint,
                &[],
                writer_budget,
                &cancel
            )?
            .is_none()
        );
        assert!(relay::pending_provisioning(&checkpoint).is_empty());
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        Ok(())
    }

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn worktree_exists_refusal_blocks_the_node_with_its_reason()
    -> Result<(), Box<dyn std::error::Error>> {
        // A directory at the managed path with no Git registration makes
        // create refuse before any mutation, with a typed WorktreeExists
        // reason that must reach the node alongside the refusal.
        refused_provisioning_blocks_the_node(
            ".worktrees/unregistered-writer",
            true,
            Some("the path is not a registered worktree"),
        )
    }

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn writer_path_outside_the_layout_blocks_the_node() -> Result<(), Box<dyn std::error::Error>> {
        // A path that maps to no managed branch is refused before any Git
        // mutation, so the node blocks and its fence clears.
        refused_provisioning_blocks_the_node("elsewhere/writer", false, None)
    }

    fn refused_provisioning_blocks_the_node(
        path: &str,
        occupy: bool,
        cause: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, GraphExecution, NodeState,
        };
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(output.status.success());
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "base"])?;
        let source = git(&["rev-parse", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let context = Context::open(
            inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
            &home,
            &project,
        )?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        if occupy {
            std::fs::create_dir_all(project.join(path))?;
        }
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"g-exists","nodes":[{
                "id":"writer","access":"worktree_write","worktree":path,"prompt":"Write","route":{"alias":"junior"}
            }]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-exists","plan_digest":"fixture","nodes":[{
                "id":"writer","state":"pending","worktree":path,"attempts":[]
            }]
        }))?;
        relay::fence_provisioning(&mut checkpoint, "writer", path, &source);
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-exists",
            budget,
            &cancel,
            &clock,
        )?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: project.clone(),
            document,
        })?;
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
        journal.save(&checkpoint)?;
        let error = recover_provisioning(
            &context,
            &mut journal,
            &mut checkpoint,
            &["writer".into()],
            budget,
            &cancel,
        )?
        .ok_or("existing worktree refusal")?;
        let message = error.to_string();
        if let Some(cause) = cause {
            assert!(message.contains(cause), "{message}");
        }
        assert_eq!(checkpoint.nodes[0].state, NodeState::Blocked);
        assert_eq!(
            checkpoint.nodes[0].advance_error_code.as_deref(),
            Some("launch_refused")
        );
        let blocked = checkpoint.nodes[0]
            .advance_error
            .as_deref()
            .ok_or("blocked node reason")?;
        assert!(blocked.contains(cause.unwrap_or(error.code())), "{blocked}");
        assert!(relay::pending_provisioning(&checkpoint).is_empty());
        Ok(())
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn skipped_writer_fence_is_cleared_without_provisioning_a_worktree()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, GraphExecution, NodeState,
        };
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(output.status.success());
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "base"])?;
        let source = git(&["rev-parse", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let context = Context::open(
            inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
            &home,
            &project,
        )?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let path = ".worktrees/skipped-writer";
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"g-skipped","nodes":[{
                "id":"writer","access":"worktree_write","worktree":path,"prompt":"Write","route":{"alias":"junior"}
            }]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-skipped","plan_digest":"fixture",
            "nodes":[{"id":"writer","state":"skipped","worktree":path,"attempts":[]}],
            "expansions":[{"source":"parent","nodes":[{
                "id":"writer","access":"worktree_write","worktree":path,
                "provision_worktree":true,"prompt":"Write","route":{"alias":"junior"},
                "when":[{"name":"flag","expected":"true","actual":"false"}]
            }]}]
        }))?;
        bind_admitted_provisioning_owners(&mut checkpoint)?;
        assert!(checkpoint.nodes[0].provisioned_owner.is_none());
        relay::fence_provisioning(&mut checkpoint, "writer", path, &source);
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-skipped",
            budget,
            &cancel,
            &clock,
        )?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: project.clone(),
            document,
        })?;
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
        journal.save(&checkpoint)?;
        assert!(
            recover_provisioning(
                &context,
                &mut journal,
                &mut checkpoint,
                &["writer".into()],
                budget,
                &cancel,
            )?
            .is_none(),
            "a skipped writer must not refuse provisioning"
        );
        assert!(relay::pending_provisioning(&checkpoint).is_empty());
        assert_eq!(checkpoint.nodes[0].state, NodeState::Skipped);
        assert!(
            !project.join(path).exists(),
            "a skipped writer creates no checkout"
        );
        Ok(())
    }

    #[test]
    fn relay_refusal_cause_reaches_the_lane_without_raw_source_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let socket = root.path().join("relay/socket");
        let _host = provenant_fabric_runtime::relay::HostRelay::start_with_graph(
            socket.clone(),
            Default::default(),
            |_, _, _| Err(ServiceError::FabricOwnerUnavailable),
            |_, _, _| Err(refused("writer_parent_required", "PROMPT_SENTINEL")),
        )?;
        let error = provenant_fabric_runtime::relay::graph(
            &socket,
            json!({"op":"children"}),
            &CancelToken::new(),
            std::time::Duration::from_secs(5),
        )
        .expect_err("writer parent refusal");
        assert_eq!(error.code(), "writer_parent_required");
        let wire = error.structured_error();
        assert!(
            wire["message"]
                .as_str()
                .is_some_and(|message| message.contains("registered parent worktree"))
        );
        assert!(!wire.to_string().contains("PROMPT_SENTINEL"));
        Ok(())
    }

    #[test]
    fn writer_child_worktree_is_created_at_source_and_dirty_reuse_is_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let canonical_root = root.path().canonicalize()?;
        let home = canonical_root.join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        for args in [
            vec!["init", "-q"],
            vec!["commit", "--allow-empty", "-qm", "base"],
        ] {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = provenant_os::fixture::git_command()
            .current_dir(&project)
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        let source = String::from_utf8(output.stdout)?.trim().to_owned();
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        let state = root.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        inputs.state_dir = Some(state);
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(project.join(".git"))?,
        )?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(provenant_os::clock::SystemClock.mono_now(), 55)?;
        // Explicit top-level writer branches use the same managed path rules
        // as worktree creation, preserving slashes in the Git branch itself.
        provision_writer(
            &context,
            ".worktrees/topic-fix",
            "topic/fix",
            &source,
            budget,
            &cancel,
        )?;
        let branch = provenant_os::fixture::git_command()
            .current_dir(project.join(".worktrees/topic-fix"))
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()?;
        assert!(branch.status.success());
        assert_eq!(String::from_utf8(branch.stdout)?.trim(), "topic/fix");
        assert!(
            provision_writer(
                &context,
                ".worktrees/topic-fix",
                "other",
                &source,
                budget,
                &cancel
            )
            .is_err()
        );
        let path = ".worktrees/graph-parent--child";
        let clock = provenant_os::clock::SystemClock;
        provision_child_with_budget(
            &context,
            path,
            &source,
            None,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &cancel,
        )?;
        let output = provenant_os::fixture::git_command()
            .current_dir(project.join(path))
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?.trim(), source);
        struct Census(provenant_confinement::worktrees::Occupancy);
        impl provenant_confinement::worktrees::WorkerObserver for Census {
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
                    .map(|root| (root, self.0.clone()))
                    .collect()
            }
        }
        let census = Census(provenant_confinement::worktrees::Occupancy::Available);
        let provision = || {
            provenant_confinement::worktrees::with_context_using_workers(
                &context.roots,
                &context.home,
                &context.cwd,
                &provenant_os::clock::SystemClock::new(),
                &cancel,
                &census,
                |ctx| {
                    provision_in_context(
                        ctx,
                        path,
                        path.trim_start_matches(".worktrees/"),
                        &source,
                        None,
                        &cancel,
                    )
                },
            )
        };
        provision()?;
        for occupancy in [
            provenant_confinement::worktrees::Occupancy::Observed { process: None },
            provenant_confinement::worktrees::Occupancy::census_unknown(
                project.join(path),
                "fixture incomplete census",
            ),
        ] {
            provenant_confinement::worktrees::with_context_using_workers(
                &context.roots,
                &context.home,
                &context.cwd,
                &provenant_os::clock::SystemClock::new(),
                &cancel,
                &Census(occupancy),
                |ctx| {
                    provision_in_context(
                        ctx,
                        path,
                        path.trim_start_matches(".worktrees/"),
                        &source,
                        None,
                        &cancel,
                    )?;
                    // The fixture's TMPDIR may be an alias. Lock the same
                    // canonical checkout identity that production verifies.
                    let canonical_worktree = provenant_os::fs::canonicalize(&project.join(path))?;
                    let _held = provenant_confinement::worktrees::cache_writer_guard(
                        ctx.project_state,
                        &canonical_worktree,
                    )
                    .map_err(provenant_confinement::worktrees::custody_refusal)?;
                    let held = provision_in_context(
                        ctx,
                        path,
                        path.trim_start_matches(".worktrees/"),
                        &source,
                        None,
                        &cancel,
                    );
                    assert!(
                        matches!(
                            held,
                            Err(provenant_confinement::worktrees::WorktreeError::WriterBusy { .. })
                        ),
                        "held custody must refuse reuse: {held:?}"
                    );
                    Ok(())
                },
            )?;
        }
        std::fs::write(project.join(path).join("parent-work"), "preserve")?;
        assert!(provision().is_err());
        assert_eq!(
            std::fs::read_to_string(project.join(path).join("parent-work"))?,
            "preserve"
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn readonly_parent_add_and_edit_provision_writers_at_the_pinned_graph_source()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, GraphExecution, NodeState,
        };
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "base"])?;
        let source = git(&["rev-parse", "HEAD"])?;
        let branch = git(&["symbolic-ref", "--short", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-writers","integration_branch":branch,"template_library":{},
            "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
        }))?;
        let reference = format!("{}/caller", context.identity.host);
        let bases = provenant_fabric_runtime::graph::GraphBaseBinding::new(
            None,
            Some(provenant_core::GitOid::parse(&source)?),
        )
        .ok_or("integration base")?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-writers","plan_digest":"fixture",
            "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[{
                "launched":true,"key":{"graph_id":"g-writers","node_id":"parent","route_index":0, "attempt": 1},
                "route":{"alias":"senior"},"state":"running","attempt_ref":reference,"outcome":null,"launch_actor":"fixture"
            }]}]
        }))?;
        checkpoint.bases = Some(bases);
        {
            let mut journal = FileJournal::open(
                &context.roots.state,
                context.identity.project,
                "g-writers",
                budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                peer_origin_context: None,
                peer_submission_digest: None,
                requester_path: None,
                extra: Default::default(),
                bases: Some(bases),
                directory: project.clone(),
                document,
            })?;
            journal.prepare_fixture_checkpoint(&mut checkpoint)?;
            journal.save(&checkpoint)?;
        }
        git(&["commit", "--allow-empty", "-qm", "move host head"])?;
        // Remote-only names must not override the children's captured source.
        let remote = root.path().join("origin.git");
        let remote = remote.to_str().ok_or("remote path")?;
        git(&["clone", "--bare", ".", remote])?;
        for child in ["writer", "edited"] {
            git(&[
                "--git-dir",
                remote,
                "branch",
                &format!("g-writers-parent--{child}"),
                "HEAD",
            ])?;
        }
        git(&["remote", "add", "origin", remote])?;
        git(&["checkout", "--detach", "-q"])?;
        let request = json!({"op":"add","id":"writer","use":"implement","prompt":"Write","writer":true,"write_scope":["src"],"idempotency_key":"write"});
        let nested = |arguments| {
            serve_with_read_and_parent(
                &context,
                "caller",
                arguments,
                RelayCall {
                    budget,
                    cancel: &cancel,
                    clock: &clock,
                },
                None,
                &|_| unreachable!("no reads"),
            )
        };
        let receipt = nested(request.clone())?;
        let path = receipt["worktree"].as_str().ok_or("missing worktree")?;
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        let replay = nested(request)?;
        assert_eq!(replay["node"], receipt["node"]);
        assert_eq!(replay["replayed"], true);
        let edit = json!({"op":"edit","id":"g-writers","patch":{"schema_version":1,"idempotency_key":"edit-writer","nodes":[{
            "id":"edited","use":"implement","prompt":"Edit","write_scope":["other"]
        }]}});
        let _ = nested(edit)?;
        assert_eq!(
            git(&[
                "-C",
                ".worktrees/g-writers-parent--edited",
                "rev-parse",
                "HEAD"
            ])?,
            source
        );
        // A writer guardian can both provision an independent child and
        // share its own checkout without declaring an integration branch.
        let owned = project.join(path).canonicalize()?;
        let sealed = crate::fabric::checkout_subject(&context.roots, &owned)?;
        let foreign = project
            .join(".worktrees/g-writers-parent--edited")
            .canonicalize()?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-owner","nodes":[{
                "id":"owning","access":"worktree_write","worktree":owned,
                "write_scope":["src"],"prompt":"own","route":{"alias":"senior"}
            },{"id":"foreign","access":"worktree_write","worktree":foreign,"prompt":"other owner","route":{"alias":"senior"}}]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"g-owner","plan_digest":"fixture",
            "nodes":[{"id":"owning","state":"running","worktree":owned,"attempts":[{
                "launched":true,"key":{"graph_id":"g-owner","node_id":"owning","route_index":0,"attempt":1},
                "route":{"alias":"senior"},"state":"running","attempt_ref":format!("{}/owner-caller",context.identity.host),
                "outcome":null,"launch_actor":"fixture"
            }]},{"id":"foreign","state":"pending","worktree":foreign,"source_commit":source,
                "writer_binding":{"worktree":foreign,"head":source},"attempts":[]}]
        }))?;
        {
            let mut journal = FileJournal::open(
                &context.roots.state,
                context.identity.project,
                "g-owner",
                budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                peer_origin_context: None,
                peer_submission_digest: None,
                requester_path: None,
                extra: Default::default(),
                bases: None,
                directory: project.clone(),
                document,
            })?;
            journal.prepare_fixture_checkpoint(&mut checkpoint)?;
            journal.save(&checkpoint)?;
        }
        let nested_owner = |arguments| {
            serve_with_read_and_parent(
                &context,
                "owner-caller",
                arguments,
                RelayCall {
                    budget,
                    cancel: &cancel,
                    clock: &clock,
                },
                Some(&sealed),
                &|_| unreachable!("no reads"),
            )
        };
        let independent = nested_owner(
            json!({"op":"add","id":"fresh","prompt":"work","writer":true,"idempotency_key":"fresh"}),
        )?;
        assert_ne!(independent["worktree"].as_str(), owned.to_str());
        assert_eq!(
            git(&[
                "-C",
                independent["worktree"].as_str().ok_or("path")?,
                "rev-parse",
                "HEAD"
            ])?,
            source
        );
        let sharing = json!({"op":"add","id":"shared","prompt":"share","idempotency_key":"shared",
            "launch":{"node":{"id":"shared","access":"worktree_write","worktree":owned,"write_scope":["src/lib"],"prompt":"share","route":{"alias":"senior"}}}});
        let templates = project.join(provenant_config::repository_files::TEMPLATES);
        std::fs::create_dir_all(&templates)?;
        std::fs::write(
            templates.join("redirect.toml"),
            format!(
                "brief = 'unrelated checkout'\ntier = 'senior'\naccess = 'worktree_write'\n[node]\nworktree = {}\nprovision_worktree = true\n",
                serde_json::to_string(foreign.to_str().ok_or("path")?)?,
            ),
        )?;
        let redirected = nested_owner(json!({"op":"add","id":"redirected","use":"redirect",
            "prompt":"try template authority","idempotency_key":"redirected"}));
        assert_eq!(
            redirected.unwrap_err().code(),
            "writer_parent_required",
            "a template cannot mint provisioning authority over a bound sibling"
        );
        let admitted = nested_owner(sharing.clone())?;
        assert!(
            !admitted["warnings"]
                .as_array()
                .ok_or("owner warning")?
                .is_empty()
        );
        let replay = nested_owner(sharing)?;
        assert_eq!(replay["warnings"], admitted["warnings"]);
        assert_eq!(replay["replayed"], true);
        let saved = FileJournal::read_checkpoint(
            &context.roots.state,
            context.identity.project,
            "g-owner",
        )?
        .ok_or("checkpoint")?;
        assert!(
            saved
                .nodes
                .iter()
                .find(|row| row.id == "owning--fresh")
                .and_then(|row| row.provisioned_owner)
                .is_some()
        );
        assert!(saved.pending_writer_provisioning.is_empty());
        assert_eq!(
            saved
                .nodes
                .iter()
                .find(|row| row.id == "owning--shared")
                .and_then(|row| row.writer_binding.as_ref())
                .and_then(|binding| binding.shared_worktree.as_ref()),
            Some(&sealed)
        );

        // A conditional writer added by a nested lane never runs, so it is
        // skipped in the checkpoint and takes no shared binding. Its declared
        // worktree is not the parent's, which used to refuse the whole add.
        let skipped_path = ".worktrees/g-owner-owning--skipped";
        nested_owner(json!({
            "op":"add","id":"skipped","prompt":"never","idempotency_key":"skipped",
            "launch":{"node":{
                "id":"skipped","access":"worktree_write","worktree":skipped_path,
                "write_scope":["src/lib"],"prompt":"never","route":{"alias":"senior"},
                "when":[{"name":"flag","expected":"true","actual":"false"}]
            }}
        }))?;
        let saved = FileJournal::read_checkpoint(
            &context.roots.state,
            context.identity.project,
            "g-owner",
        )?
        .ok_or("checkpoint")?;
        let row = saved
            .nodes
            .iter()
            .find(|row| row.id == "owning--skipped")
            .ok_or("skipped writer row")?;
        assert_eq!(row.state, NodeState::Skipped, "{row:?}");
        assert!(row.writer_binding.is_none(), "{row:?}");
        assert!(
            !project.join(skipped_path).exists(),
            "a skipped writer creates no checkout"
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    // Spec: docs/specs/routing.md#pinned-review-selection
    #[test]
    fn lost_child_receipt_replays_while_active_and_after_graph_settlement()
    -> Result<(), Box<dyn std::error::Error>> {
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
        use provenant_fabric_runtime::graph::{
            AttemptState, GraphCheckpoint, GraphExecution, NodeState,
        };
        for (terminal, concurrent) in [(NodeState::Succeeded, false), (NodeState::Skipped, true)] {
            let root = tempfile::tempdir()?;
            let home = root.path().join("home");
            let project = home.join("project");
            std::fs::create_dir_all(&project)?;
            let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
                let output = provenant_os::fixture::git_command()
                    .current_dir(&project)
                    .args(args)
                    .output()?;
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                Ok(String::from_utf8(output.stdout)?.trim().to_owned())
            };
            git(&["init", "-q"])?;
            git(&["commit", "--allow-empty", "-qm", "base"])?;
            let base = git(&["rev-parse", "HEAD"])?;
            git(&["commit", "--allow-empty", "-qm", "review subject"])?;
            let subject = git(&["rev-parse", "HEAD"])?;
            let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
            inputs.state_dir = Some(root.path().join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let context = Context::open(roots, &home, &project)?;
            let cancel = CancelToken::new();
            let clock = provenant_os::clock::SystemClock::new();
            let budget = WaitBudget::cli(clock.mono_now(), 5)?;
            let reference = format!("{}/caller", context.identity.host);
            let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
                "schema_version":2,"graph_id":"g-lost-receipt","plan_digest":"fixture",
                "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"g-lost-receipt","node_id":"parent","route_index":0, "attempt": 1},
                    "route":{"alias":"senior"},"state":"running","attempt_ref":reference,
                    "outcome":null,"launch_actor":"fixture"
                }]}]
            }))?;
            let open = || {
                FileJournal::open(
                    &context.roots.state,
                    context.identity.project,
                    "g-lost-receipt",
                    budget,
                    &cancel,
                    &clock,
                )
            };
            {
                let mut journal = open()?;
                journal.bind(GraphExecution {
                    peer_origin_context: None,
                    peer_submission_digest: None,

                    requester_path: None,
                    extra: Default::default(),
                    bases: None,
                    directory: project.clone(),
                    document: serde_json::from_value(json!({
                        "schema_version":1,"graph_id":"g-lost-receipt","template_library":{},
                        "nodes":[{"id":"parent","access":"read_only","prompt":"Go","route":{"alias":"senior"}}]
                    }))?,
                })?;
                journal.prepare_fixture_checkpoint(&mut checkpoint)?;
                journal.save(&checkpoint)?;
            }
            let request = json!({
                "op":"add","id":"child","use":"task","prompt":"ghp_abcdefghijklmnopqrstuvwxyz0123456789", "idempotency_key":"lost-reply",
                "launch":{"node":{"use":"task","prompt":format!("Review {base}...{subject}"),
                    "source":{"type":"commit","commit":subject}}}
            });
            let input = if concurrent {
                json!({"template":"review","subject":"HEAD","review_base":base})
            } else {
                json!({"template":"review","pr":7})
            };
            let identity = relay::RunRequestIdentity::for_input(&input)?;
            let run = json!({"op":"run","request_id":identity.id,"input":input});
            let prepare = |input: Value| -> Result<Value, ServiceError> {
                assert_eq!(input["op"], "prepare_child");
                if concurrent {
                    assert_eq!(input["input"]["subject"], "HEAD");
                    assert_eq!(input["input"]["review_base"], base);
                } else {
                    assert_eq!(input["input"]["pr"], 7);
                }
                // Host lookup/preparation must not hold the graph journal lock.
                drop(open().map_err(journal_error)?);
                let mut prepared = request.clone();
                prepared.as_object_mut().unwrap().remove("op");
                Ok(prepared)
            };
            let racing_prepare = |input| {
                // Another caller admits the same raw request while this caller
                // is preparing outside the journal lock. Its pins win.
                serve_with_read(
                    &context,
                    "caller",
                    run.clone(),
                    budget,
                    &cancel,
                    &clock,
                    &prepare,
                )?;
                let mut moved = prepare(input)?;
                moved["launch"]["node"]["source"]["commit"] = json!(base);
                Ok(moved)
            };
            let receipt = serve_with_read(
                &context,
                "caller",
                run.clone(),
                budget,
                &cancel,
                &clock,
                &|input| {
                    if concurrent {
                        racing_prepare(input)
                    } else {
                        prepare(input)
                    }
                },
            )?;
            // A durable admission warning must survive Run replay, host restart
            // and parent settlement just as it does for Add replay.
            let mut receipt = receipt;
            let warning = "fixture advisory path overlap";
            {
                let mut journal = open()?;
                let mut checkpoint = journal.load("g-lost-receipt")?.ok_or("checkpoint")?;
                let expansion = checkpoint
                    .expansions
                    .iter_mut()
                    .find(|expansion| {
                        expansion.patch.as_ref().is_some_and(|patch| {
                            Some(patch.revision) == receipt["revision"].as_u64()
                        })
                    })
                    .ok_or("admission expansion")?;
                expansion.floor_warnings.push(warning.into());
                journal.save(&checkpoint)?;
            }
            receipt["warnings"] = json!([warning]);
            git(&["commit", "--allow-empty", "-qm", "moved review ref"])?;
            let moved_subject = git(&["rev-parse", "HEAD"])?;
            assert_ne!(moved_subject, subject);
            let before_live_replay = open()?.load("g-lost-receipt")?;
            let admitted = before_live_replay
                .as_ref()
                .ok_or("checkpoint")?
                .expansions
                .iter()
                .flat_map(|expansion| &expansion.nodes)
                .find(|node| node.id == receipt["node"].as_str().unwrap())
                .ok_or("child")?;
            assert_eq!(
                serde_json::to_value(&admitted.source)?,
                request["launch"]["node"]["source"]
            );
            assert_eq!(
                admitted.prompt,
                request["launch"]["node"]["prompt"].as_str().unwrap()
            );
            let mut changed_wait = run.clone();
            changed_wait["input"]["wait_seconds"] = json!(5);
            let live_replay = serve_with_read(
                &context,
                "caller",
                changed_wait,
                budget,
                &cancel,
                &clock,
                &|input| {
                    let mut moved = prepare(input)?;
                    moved["launch"]["node"]["source"]["commit"] = json!(moved_subject);
                    panic!(
                        "a matching receipt must bypass lookup even when the review ref moved: {moved}"
                    );
                },
            )?;
            let mut expected_replay = receipt.clone();
            expected_replay["replayed"] = json!(true);
            assert_eq!(live_replay, expected_replay);
            // Reopen the checkpoint and context, as after a host restart, with
            // forge lookup unavailable. Both pins and the receipt must survive.
            let reloaded_context = Context::open(context.roots.clone(), &home, &project)?;
            let unavailable = |_| Err(refused("forge_unavailable", "fixture lookup unavailable"));
            let reloaded_replay = serve_with_read(
                &reloaded_context,
                "caller",
                run.clone(),
                budget,
                &cancel,
                &clock,
                &unavailable,
            )?;
            assert_eq!(reloaded_replay, expected_replay);
            assert_eq!(open()?.load("g-lost-receipt")?, before_live_replay);
            let mut fresh_run = run.clone();
            fresh_run["input"]["idempotency_key"] = json!("fresh-live-run");
            fresh_run["request_id"] =
                json!(relay::RunRequestIdentity::for_input(&fresh_run["input"])?.id);
            assert!(matches!(
                serve_with_read(&context, "caller", fresh_run.clone(), budget, &cancel, &clock, &unavailable),
                Err(error) if error.code() == "forge_unavailable"
            ));
            let fresh_receipt = serve_with_read(
                &context,
                "caller",
                fresh_run,
                budget,
                &cancel,
                &clock,
                &|input| {
                    let mut prepared = prepare(input)?;
                    prepared["id"] = json!("fresh-child");
                    prepared["idempotency_key"] = json!("fresh-live-run");
                    prepared["launch"]["node"]["source"]["commit"] = json!(moved_subject);
                    Ok(prepared)
                },
            )?;
            assert_eq!(fresh_receipt["replayed"], false);
            let fresh_checkpoint = open()?.load("g-lost-receipt")?.ok_or("checkpoint")?;
            let fresh_child = fresh_checkpoint
                .expansions
                .iter()
                .flat_map(|expansion| &expansion.nodes)
                .find(|node| node.id == fresh_receipt["node"].as_str().unwrap())
                .ok_or("fresh child")?;
            assert_eq!(
                serde_json::to_value(&fresh_child.source)?,
                json!({"type":"commit","commit":moved_subject})
            );
            let settled = {
                let mut journal = open()?;
                let mut checkpoint = journal.load("g-lost-receipt")?.ok_or("checkpoint")?;
                for node in &mut checkpoint.nodes {
                    node.state = terminal;
                    for attempt in &mut node.attempts {
                        attempt.state = AttemptState::Terminal;
                    }
                }
                checkpoint.cancelled = terminal == NodeState::Skipped;
                journal.save(&checkpoint)?;
                checkpoint
            };
            assert!(
                FileJournal::pending_graph_ids(&context.roots.state, context.identity.project)?
                    .is_empty()
            );
            let replay = serve(&context, "caller", request.clone(), budget, &cancel, &clock)?;
            assert_eq!(replay["node"], receipt["node"]);
            assert_eq!(replay["revision"], receipt["revision"]);
            {
                let warnings = replay["warnings"]
                    .as_array()
                    .ok_or("visible replay warnings")?;
                assert!(
                    warnings
                        .iter()
                        .any(|w| w.to_string().contains("prompt looks like a credential"))
                );
                assert!(warnings.iter().all(|w| {
                    !w.to_string()
                        .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
                }));
            }
            assert_eq!(replay["replayed"], true);
            let unavailable = |_| panic!("settled run must not prepare or fetch");
            let prepared_replay = serve_with_read(
                &context,
                "caller",
                run.clone(),
                budget,
                &cancel,
                &clock,
                &unavailable,
            )?;
            assert_eq!(prepared_replay["node"], replay["node"]);
            assert_eq!(prepared_replay["revision"], replay["revision"]);
            let mut changed_run = run.clone();
            changed_run["input"]["pr"] = json!(8);
            assert!(
                matches!(serve_with_read(&context, "caller", changed_run, budget, &cancel, &clock, &unavailable), Err(error) if error.code() == "invalid_input")
            );
            let mut fresh_run = run;
            fresh_run["input"]["idempotency_key"] = json!("fresh-run");
            fresh_run["request_id"] =
                json!(relay::RunRequestIdentity::for_input(&fresh_run["input"])?.id);
            assert!(
                matches!(serve_with_read(&context, "caller", fresh_run, budget, &cancel, &clock, &unavailable), Err(error) if error.code() == "graph_relay_unavailable")
            );
            assert_eq!(open()?.load("g-lost-receipt")?, Some(settled));
            let mut changed = request.clone();
            changed["prompt"] = json!("Different");
            assert!(
                matches!(serve(&context, "caller", changed, budget, &cancel, &clock),
                Err(ServiceError::Cli { code, .. }) if code == "idempotency_conflict")
            );
            let mut fresh = request;
            fresh["idempotency_key"] = json!("new-request");
            assert!(
                matches!(serve(&context, "caller", fresh, budget, &cancel, &clock),
                Err(error) if error.code() == "graph_relay_unavailable")
            );
        }
        Ok(())
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn lane_edit_refuses_skip_and_replace_outside_its_subtree()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphCheckpoint, GraphDocument, GraphExecution};
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let context = Context::open(
            inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
            &home,
            &project,
        )?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"edit-scope",
            "nodes":[
                {"id":"writer","access":"read_only","prompt":"Write","route":{"alias":"senior"}},
                {"id":"review","access":"read_only","prompt":"Review","route":{"alias":"senior"}}
            ]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2,"graph_id":"edit-scope","plan_digest":"fixture",
            "nodes":[
                {"id":"writer","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"edit-scope","node_id":"writer","route_index":0, "attempt": 1},
                    "route":{"alias":"senior"},"state":"running",
                    "attempt_ref":format!("{}/caller",context.identity.host),"outcome":null,"launch_actor":"fixture"
                }]},
                {"id":"review","state":"pending","worktree":null,"attempts":[]}
            ]
        }))?;
        {
            let mut journal = FileJournal::open(
                &context.roots.state,
                context.identity.project,
                "edit-scope",
                budget,
                &cancel,
                &clock,
            )?;
            journal.bind(GraphExecution {
                peer_origin_context: None,
                peer_submission_digest: None,
                requester_path: None,
                extra: Default::default(),
                bases: None,
                directory: project.clone(),
                document,
            })?;
            journal.prepare_fixture_checkpoint(&mut checkpoint)?;
            journal.save(&checkpoint)?;
        }
        for edit in [
            json!({"skip":["review"]}),
            json!({"replace":[{"id":"review","access":"read_only","prompt":"Bypass review","route":{"alias":"senior"}}]}),
        ] {
            let mut patch = edit;
            patch["schema_version"] = json!(1);
            let result = serve(
                &context,
                "caller",
                json!({"op":"edit","id":"edit-scope","patch":patch}),
                budget,
                &cancel,
                &clock,
            );
            assert_eq!(
                result
                    .expect_err("edits outside the subtree must be refused")
                    .code(),
                "edit_scope"
            );
            let saved = FileJournal::read_checkpoint(
                &context.roots.state,
                context.identity.project,
                "edit-scope",
            )?
            .ok_or("missing checkpoint")?;
            assert_eq!(saved, checkpoint);
        }
        Ok(())
    }

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn crash_after_the_fence_is_recovered_by_the_next_serve()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, GraphExecution, NodeState,
        };
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(output.status.success());
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "base"])?;
        let source = git(&["rev-parse", "HEAD"])?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 30)?;
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let path = ".worktrees/g-crash-a--w";
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1, "graph_id":"g-crash",
            "nodes":[
                {"id":"a","access":"read_only","prompt":"Go","route":{"alias":"senior"}},
                {"id":"a--w","access":"worktree_write","worktree":path,"prompt":"Write","route":{"alias":"junior"}}
            ]
        }))?;
        journal.bind(GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: project.clone(),
            document: document.clone(),
        })?;
        // The crash: the fence was checkpointed, then the process died before
        // any Git effect.
        let reference = format!("{}/caller", context.identity.host);
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":2, "graph_id":"g-crash", "plan_digest":"fixture", "nodes":[
                {"id":"a","state":"running","worktree":null,"attempts":[{
                    "launched":true,"key":{"graph_id":"g-crash","node_id":"a","route_index":0, "attempt": 1},
                    "route":{"alias":"senior"},"state":"running","attempt_ref":reference,
                    "outcome":null,"launch_actor":"fixture"
                }]},
                {"id":"a--w","state":"pending","worktree":path,"attempts":[]}
            ]
        }))?;
        relay::fence_provisioning(&mut checkpoint, "a--w", path, &source);
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
        journal.save(&checkpoint)?;
        assert!(!project.join(path).exists());
        // Process death releases the journal lock. Recover using a fresh host.
        drop(journal);
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let mut reloaded = journal.load("g-crash")?.expect("checkpoint");
        assert!(
            recover_provisioning(&context, &mut journal, &mut reloaded, &[], budget, &cancel)?
                .is_none()
        );
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        assert!(relay::pending_provisioning(&reloaded).is_empty());
        let saved = journal.load("g-crash")?.expect("checkpoint");
        assert!(relay::pending_provisioning(&saved).is_empty());

        // Git finished, but the host crashed before saving the cleared fence.
        // Reusing the same path must not allocate another tree or branch.
        relay::fence_provisioning(&mut reloaded, "a--w", path, &source);
        journal.save(&reloaded)?;
        drop(journal);
        let before = git(&["worktree", "list", "--porcelain"])?;
        let mut journal = FileJournal::open(
            &context.roots.state,
            context.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let mut reloaded = journal.load("g-crash")?.expect("checkpoint");
        assert!(
            recover_provisioning(&context, &mut journal, &mut reloaded, &[], budget, &cancel)?
                .is_none()
        );
        assert_eq!(git(&["worktree", "list", "--porcelain"])?, before);
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);
        let saved = journal.load("g-crash")?.expect("checkpoint");
        assert!(relay::pending_provisioning(&saved).is_empty());

        // A crash after Git succeeds leaves uncertain effects even when the
        // original launch deadline has expired. A later pre-mutation refusal
        // cannot settle that fence, and must not block unrelated relay reads.
        reloaded.nodes[1]
            .launch_retry
            .as_mut()
            .ok_or("launch deadline")?
            .deadline = Some(provenant_core::WallTime::from_micros(0)?);
        relay::fence_provisioning(&mut reloaded, "a--w", path, &source);
        journal.save(&reloaded)?;
        drop(journal);
        let restarted = Context::open(context.roots.clone(), &home, &project)?;
        let slot = restarted
            .roots
            .state
            .as_path()
            .join("worktrees")
            .join(restarted.identity.project.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &project.canonicalize()?.join(path),
            ));
        let slot = provenant_os::fs::Directory::open_anchor(&slot)?;
        let held = slot.try_lock_existing(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let short = WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(100_000),
        );
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            short,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert_eq!(
            relay::pending_provisioning(&saved),
            vec![("a--w".into(), path.into(), source.clone())]
        );
        assert_eq!(saved.nodes[1].state, NodeState::Pending);
        assert!(saved.nodes[1].advance_owner_code.is_none());
        drop(held);

        // A mismatched source is another refusal before mutation; it cannot
        // reconcile the original checkout either.
        git(&["-C", path, "commit", "--allow-empty", "-qm", "later"])?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            budget,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert_eq!(
            relay::pending_provisioning(&saved),
            vec![("a--w".into(), path.into(), source.clone())]
        );
        git(&["-C", path, "reset", "--hard", &source])?;

        // With identity restored, a fresh bounded recovery budget verifies
        // the exact checkout despite the expired launch.
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            budget,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert!(relay::pending_provisioning(&saved).is_empty());
        assert_eq!(saved.nodes[1].state, NodeState::Pending);
        assert_eq!(git(&["worktree", "list", "--porcelain"])?, before);
        assert_eq!(git(&["-C", path, "rev-parse", "HEAD"])?, source);

        // A partial add may leave only a branch. General worktree creation
        // accepts a descendant base; recovery must still require exact source.
        let mut journal = FileJournal::open(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
            budget,
            &cancel,
            &clock,
        )?;
        let mut saved = journal.load("g-crash")?.ok_or("checkpoint")?;
        relay::fence_provisioning(&mut saved, "a--w", path, &source);
        journal.save(&saved)?;
        drop(journal);
        git(&["-C", path, "commit", "--allow-empty", "-qm", "later"])?;
        git(&["worktree", "remove", path])?;
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            budget,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert_eq!(
            relay::pending_provisioning(&saved),
            vec![("a--w".into(), path.into(), source.clone())]
        );
        git(&["-C", path, "reset", "--hard", &source])?;
        serve(
            &restarted,
            "caller",
            json!({"op":"children"}),
            WaitBudget::cli(clock.mono_now(), 5)?,
            &cancel,
            &clock,
        )?;
        let saved = FileJournal::read_checkpoint(
            &restarted.roots.state,
            restarted.identity.project,
            "g-crash",
        )?
        .ok_or("checkpoint")?;
        assert!(relay::pending_provisioning(&saved).is_empty());
        assert_eq!(git(&["worktree", "list", "--porcelain"])?, before);
        Ok(())
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    // Spec: docs/specs/routing.md#pinned-review-selection
    #[test]
    fn relayed_review_panels_use_their_explicit_source_not_the_callers()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphBaseBinding, GraphCheckpoint, GraphDocument};

        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project)?;
        let git = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let output = provenant_os::fixture::git_command()
                .current_dir(&project)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        git(&["init", "-q"])?;
        git(&["commit", "--allow-empty", "-qm", "review base"])?;
        let base = git(&["rev-parse", "HEAD"])?;

        let release = project.join(".github/workflows/release.yml");
        let credentials = project.join("credentials/identity.json");
        std::fs::create_dir_all(release.parent().ok_or("release parent")?)?;
        std::fs::create_dir_all(credentials.parent().ok_or("credentials parent")?)?;
        std::fs::write(&release, "name: Release\n")?;
        std::fs::write(&credentials, "{}\n")?;
        git(&[
            "add",
            ".github/workflows/release.yml",
            "credentials/identity.json",
        ])?;
        git(&["commit", "-qm", "touch configured protected paths"])?;
        let protected_subject = git(&["rev-parse", "HEAD"])?;

        git(&["checkout", "-q", "-b", "docs-only", &base])?;
        let guide = project.join("docs/guide.md");
        std::fs::create_dir_all(guide.parent().ok_or("docs parent")?)?;
        std::fs::write(&guide, "Review guide\n")?;
        git(&["add", "docs/guide.md"])?;
        git(&["commit", "-qm", "touch docs only"])?;
        let docs_subject = git(&["rev-parse", "HEAD"])?;

        let mut root_inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        root_inputs.state_dir = Some(root.path().join("state"));
        let roots = root_inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let review = provenant_routing::RoutingConfig::default().review;
        let crucial_reviewers: usize = review.shapes[&review.select.by_risk["crucial"]]
            .reviewers
            .iter()
            .map(|group| group.count)
            .sum();
        let library = TemplateLibrary::bundled();
        let limits = provenant_fabric_runtime::config::GraphLimits::default();
        let worktree_layout = provenant_config::PortTable::builtin().worktree_layout;

        for (caller_subject, child_subject, deep) in [
            (&protected_subject, &protected_subject, true),
            (&docs_subject, &docs_subject, false),
            (&docs_subject, &protected_subject, true),
        ] {
            let document: GraphDocument = serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "relay-review",
                "review_base": base,
                "nodes": [{
                    "id": "caller",
                    "access": "read_only",
                    "prompt": "Caller",
                    "route": {"alias": "senior"},
                    "source": {"type": "commit", "commit": caller_subject}
                }]
            }))?;
            let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "relay-review",
                "plan_digest": "fixture",
                "nodes": [{
                    "id": "caller",
                    "state": "running",
                    "source_commit": caller_subject,
                    "attempts": []
                }]
            }))?;
            checkpoint.bases =
                GraphBaseBinding::new(Some(provenant_core::GitOid::parse(&base)?), None);

            let patch: provenant_fabric_runtime::graph::patch::Patch =
                serde_json::from_value(json!({
                    "schema_version": 1,
                    "expected_revision": 0,
                    "idempotency_key": "nested-review",
                    "nodes": [{
                        "id": "review",
                        "use": "review-panel",
                        "source": {"type": "commit", "commit": child_subject}
                    }]
                }))?;
            let subjects = relay_patch_review_subjects(&patch, &library)?;
            let add_request: provenant_fabric_runtime::graph::relay::ChildRequest =
                serde_json::from_value(json!({
                    "id": "review",
                    "use": "task",
                    "prompt": "Review the selected subject",
                    "idempotency_key": "nested-review",
                    "launch": {
                        "node": {
                            "id": "review",
                            "use": "review-panel",
                            "source": {"type": "commit", "commit": child_subject}
                        }
                    }
                }))?;
            assert_eq!(
                relay_child_review_subjects(&add_request, &library)?,
                subjects
            );
            let resolvers = launch_resolvers_for_relay(
                &roots,
                &project,
                &review,
                &document,
                &checkpoint,
                &subjects,
            )?;
            let lenses = &resolvers["review-panel"];
            if deep {
                assert!(lenses.len() >= crucial_reviewers, "{lenses:?}");
                for lens in ["correctness-spec", "reliability", "security-privacy"] {
                    assert!(lenses.iter().any(|selected| selected == lens), "{lenses:?}");
                }
                for stage in ["review-frame", "review-verify", "review-synthesis"] {
                    assert_eq!(resolvers[stage], vec!["true"]);
                }
            } else {
                assert_eq!(lenses.len(), 1, "{lenses:?}");
                for stage in ["review-frame", "review-verify", "review-synthesis"] {
                    assert_eq!(resolvers[stage], vec!["false"]);
                }
            }

            let mut document = document;
            document.template_resolvers = resolvers.clone();
            let admitted = provenant_fabric_runtime::graph::patch::admit_patch(
                &mut LiveGraph {
                    document: &document,
                    checkpoint: &mut checkpoint,
                },
                Some("caller"),
                &patch,
                &PatchContext {
                    catalogue: None,
                    library: &library,
                    resolvers: &resolvers,
                    limits: &limits,

                    journal: None,
                    worktree_layout: worktree_layout.clone(),
                },
            )?;
            assert!(!admitted.added.is_empty());
            let expanded = &checkpoint.expansions.last().ok_or("relay expansion")?.nodes;
            let review_leaves: Vec<_> = expanded
                .iter()
                .filter(|node| node.review_lens.is_some())
                .collect();
            assert_eq!(
                review_leaves.len() >= crucial_reviewers,
                deep,
                "{} leaves",
                review_leaves.len()
            );
        }

        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version": 2,
            "graph_id": "relay-review-unresolved",
            "review_base": base,
            "nodes": [{
                "id": "caller",
                "access": "read_only",
                "prompt": "Caller",
                "route": {"alias": "senior"},
                "source": {"type": "commit", "commit": docs_subject}
            }]
        }))?;
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version": 2,
            "graph_id": "relay-review-unresolved",
            "plan_digest": "fixture",
            "nodes": [{
                "id": "caller",
                "state": "running",
                "source_commit": docs_subject,
                "attempts": []
            }, {
                "id": "writer",
                "state": "running",
                "source_commit": docs_subject,
                "attempts": []
            }]
        }))?;
        checkpoint.bases = GraphBaseBinding::new(
            Some(provenant_core::GitOid::parse(&base)?),
            Some(provenant_core::GitOid::parse(&docs_subject)?),
        );

        let missing_add: provenant_fabric_runtime::graph::relay::ChildRequest =
            serde_json::from_value(json!({
                "id": "missing",
                "use": "review-panel",
                "prompt": "Review the selected subject",
                "idempotency_key": "missing-source"
            }))?;
        let dynamic_add: provenant_fabric_runtime::graph::relay::ChildRequest =
            serde_json::from_value(json!({
                "id": "dynamic",
                "use": "task",
                "prompt": "Review the selected subject",
                "idempotency_key": "dynamic-source",
                "launch": {
                    "node": {
                        "id": "dynamic",
                        "use": "review-panel",
                        "source": {"type": "head", "node": "writer"}
                    }
                }
            }))?;
        let integration_add: provenant_fabric_runtime::graph::relay::ChildRequest =
            serde_json::from_value(json!({
                "id": "integration",
                "use": "task",
                "prompt": "Review the selected subject",
                "idempotency_key": "integration-source",
                "launch": {
                    "node": {
                        "id": "integration",
                        "use": "review-panel",
                        "source": {"type": "integration"}
                    }
                }
            }))?;
        let missing = relay_child_review_subjects(&missing_add, &library)?;
        let dynamic = relay_child_review_subjects(&dynamic_add, &library)?;
        let integration = relay_child_review_subjects(&integration_add, &library)?;
        assert!(checkpoint.integration_pin.is_none());
        assert!(
            checkpoint
                .bases
                .and_then(|bases| bases.integration)
                .is_some()
        );
        for subjects in [&missing, &dynamic, &integration] {
            let resolvers = launch_resolvers_for_relay(
                &roots,
                &project,
                &review,
                &document,
                &checkpoint,
                subjects,
            )?;
            assert!(resolvers["review-panel"].len() >= crucial_reviewers);
            for stage in ["review-frame", "review-verify", "review-synthesis"] {
                assert_eq!(resolvers[stage], vec!["true".to_owned()]);
            }
            assert!(
                resolvers["review-advice-correctness-spec"][0]
                    .contains("configured unreadable-path floors were applied")
            );
        }

        let mixed_patch: provenant_fabric_runtime::graph::patch::Patch = serde_json::from_value(
            json!({
                "schema_version": 1,
                "expected_revision": 0,
                "idempotency_key": "mixed-review-sources",
                "nodes": [
                    {"id": "docs-review", "use": "review-panel", "source": {"type": "commit", "commit": docs_subject}},
                    {"id": "writer-review", "use": "review-panel", "source": {"type": "head", "node": "writer"}}
                ]
            }),
        )?;
        let mixed_subjects = relay_patch_review_subjects(&mixed_patch, &library)?;
        let ReviewDiffSelection::Incomplete(mixed_diffs) =
            relay_review_diffs(&project, &checkpoint, &mixed_subjects)
        else {
            return Err("mixed sources should retain the known diff and unresolved head".into());
        };
        assert_eq!(
            combine_review_diffs(&mixed_diffs).0,
            vec!["docs/guide.md".to_owned()]
        );
        let mixed_resolvers = launch_resolvers_for_relay(
            &roots,
            &project,
            &review,
            &document,
            &checkpoint,
            &mixed_subjects,
        )?;
        assert!(mixed_resolvers["review-panel"].len() >= crucial_reviewers);
        assert!(
            mixed_resolvers["review-advice-correctness-spec"][0]
                .contains("configured unreadable-path floors were applied")
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#cli-and-relay-surfaces
    #[test]
    fn relay_review_resolver_uses_graph_risk_and_tags() {
        let review = provenant_routing::RoutingConfig::default().review;
        let mut document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(json!({
                "schema_version": 1, "graph_id": "panel-risk",
                "nodes": [{"id": "anchor", "access": "read_only", "prompt": "Graph task", "route": {"alias": "senior"}}],
                "context": {"risk_tier": "routine"},
            }))
            .unwrap();
        let routine = launch_resolvers(&review, &document).unwrap();
        assert!(
            routine["review-panel"]
                .iter()
                .any(|lens| lens == "correctness-spec")
        );
        assert!(!routine["review-panel"].iter().any(|lens| lens == "tests"));
        document.context.as_mut().unwrap().risk_tier = Some("crucial".into());
        document.review = Some(provenant_routing::ReviewRequest {
            risk_tags: vec!["security".into()],
            ..Default::default()
        });
        let crucial = launch_resolvers(&review, &document).unwrap();
        assert!(crucial["review-panel"].iter().any(|lens| lens == "tests"));
        assert!(
            crucial["review-panel"]
                .iter()
                .any(|lens| lens == "security-privacy")
        );
        let library = TemplateLibrary::bundled();
        let limits = provenant_fabric_runtime::config::GraphLimits::default();
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2, "graph_id": "panel-risk", "plan_digest": "fixture", "nodes": [],
            })).unwrap();
        let patch = serde_json::from_value(json!({
            "schema_version": 1,
            "expected_revision": 0,
            "idempotency_key": "review-child",
            "nodes": [{"id": "review", "use": "review-panel"}],
        }))
        .unwrap();
        provenant_fabric_runtime::graph::patch::admit_patch(
            &mut LiveGraph {
                document: &document,
                checkpoint: &mut checkpoint,
            },
            None,
            &patch,
            &PatchContext {
                catalogue: None,
                library: &library,
                resolvers: &crucial,
                limits: &limits,

                journal: None,

                worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
            },
        )
        .expect("relay resolvers expand the complete review composite");
    }

    // Spec: docs/specs/routing.md#pinned-review-selection
    #[test]
    // Spec: docs/specs/fabric.md#composite-settlement
    fn incomplete_pinned_review_metadata_uses_configured_unreadable_path_floors() {
        let mut review = provenant_routing::RoutingConfig::default().review;
        // Keep a non-default floor in the fixture so this catches a fallback
        // that silently drops project-configured coverage.
        review.floors.insert(
            "project-private".into(),
            provenant_routing::ReviewFloor {
                paths: vec!["docs/private/**".into()],
                tier: "routine".into(),
                lenses: vec!["tests".into()],
                hard: false,
            },
        );
        let configured_floor_lenses = review
            .floors
            .values()
            .flat_map(|floor| floor.lenses.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(configured_floor_lenses.contains("tests"));
        let document = serde_json::from_value(json!({
            "schema_version": 1, "graph_id": "incomplete-review-context",
            "nodes": [{"id": "anchor", "access": "read_only", "prompt": "Review", "route": {"alias": "senior"}}],
            "review": {"kind": "docs", "risk_tier": "routine"}
        }))
        .unwrap();
        let incomplete_size =
            ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
                paths: vec!["docs/guide.md".into()],
                numstat: None,
                tree: None,
            }]);

        for selection in [ReviewDiffSelection::Incomplete(Vec::new()), incomplete_size] {
            let resolvers = launch_resolvers_with(&review, &document, selection, None, |_, _| {
                Ok("fixture".into())
            })
            .unwrap();
            let lenses = &resolvers["review-panel"];
            for lens in &configured_floor_lenses {
                assert!(
                    lenses.contains(lens),
                    "missing configured floor lens {lens}: {lenses:?}"
                );
            }
            for lens in ["correctness-spec", "reliability", "security-privacy"] {
                assert!(lenses.iter().any(|selected| selected == lens), "{lenses:?}");
            }
            assert_eq!(resolvers["review-frame"], vec!["true".to_owned()]);
            assert_eq!(resolvers["review-verify"], vec!["true".to_owned()]);
            assert_eq!(resolvers["review-synthesis"], vec!["true".to_owned()]);
            let floor: Value =
                serde_json::from_str(&resolvers["review-coverage-floor"][0]).unwrap();
            assert_eq!(floor["hard"], false, "bundled floors never refuse");
            assert!(floor["required_lenses"].as_array().unwrap().is_empty());
            assert!(
                resolvers["review-advice-correctness-spec"][0]
                    .contains("configured unreadable-path floors were applied")
            );
        }

        let empty_diff = ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
            paths: Vec::new(),
            numstat: Some(String::new()),
            tree: Some(String::new()),
        }]);
        let quick = launch_resolvers_with(&review, &document, empty_diff, None, |_, _| {
            Ok("fixture".into())
        })
        .unwrap();
        assert!(
            quick
                .values()
                .flatten()
                .all(|value| !value.contains("configured unreadable-path floors were applied"))
        );
        assert!(!quick["review-panel"].iter().any(|lens| lens == "tests"));
        let quick_floor: Value = serde_json::from_str(&quick["review-coverage-floor"][0]).unwrap();
        assert_eq!(quick_floor["hard"], false);

        let boundary_diff = || {
            ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
                paths: vec!["credentials/token.json".into(), "src/unmatched.rs".into()],
                numstat: Some(
                    concat!("1\t0\tcredentials/token.json\0", "1\t0\tsrc/unmatched.rs\0").into(),
                ),
                tree: Some("tools/front-door/Cargo.toml".into()),
            }])
        };
        // A bundled boundary floor adds its lens, never deep, and never refuses.
        let bundled = launch_resolvers_with(&review, &document, boundary_diff(), None, |_, _| {
            Ok("fixture".into())
        })
        .unwrap();
        let floor: Value = serde_json::from_str(&bundled["review-coverage-floor"][0]).unwrap();
        assert_eq!(floor["hard"], false);
        assert!(floor["required_lenses"].as_array().unwrap().is_empty());
        assert!(
            bundled["review-panel"]
                .iter()
                .any(|lens| lens == "security-privacy")
        );
        let deep_reviewers: usize = review.shapes["deep"]
            .reviewers
            .iter()
            .map(|g| g.count)
            .sum();
        assert!(floor["minimum"].as_u64().unwrap() < deep_reviewers as u64);

        // Only a floor the project declares hard refuses, and only its own
        // lenses are required: the unknown class's lens is not.
        let mut project = review.clone();
        project.floors.insert(
            "project-boundary".into(),
            provenant_routing::ReviewFloor {
                paths: vec!["credentials/**".into()],
                tier: "crucial".into(),
                lenses: vec!["correctness-spec".into(), "adversarial".into()],
                hard: true,
            },
        );
        project.floors.get_mut("unknown").unwrap().lenses = vec!["tests".into()];
        let protected =
            launch_resolvers_with(&project, &document, boundary_diff(), None, |_, _| {
                Ok("fixture".into())
            })
            .unwrap();
        let floor: Value = serde_json::from_str(&protected["review-coverage-floor"][0]).unwrap();
        assert_eq!(floor["hard"], true);
        let required = floor["required_lenses"].as_array().unwrap();
        assert!(required.iter().any(|lens| lens == "adversarial"));
        assert!(!required.iter().any(|lens| lens == "tests"));
        let hard_reviewers: usize = project.shapes[&project.select.by_risk["crucial"]]
            .reviewers
            .iter()
            .map(|g| g.count)
            .sum();
        assert_eq!(floor["minimum"], hard_reviewers);

        let library = TemplateLibrary::bundled();
        let compiled = provenant_fabric_runtime::graph::template::Compiler {
            library: &library,
            resolvers: &protected,
        }
        .compile(&json!({"id":"protected-panel", "use":"review-panel", "min_ok":1}))
        .unwrap();
        let root = compiled.composite.as_ref().unwrap();
        assert!(root.hard_review_coverage);
        assert!(root.required_review_lenses.contains(&"adversarial".into()));
        let shape = root.nodes[0].composite.as_ref().unwrap();
        let lenses = shape.nodes.iter().find(|node| node.id == "lenses").unwrap();
        let lens_floor = lenses.composite.as_ref().unwrap();
        assert!(lens_floor.hard_review_coverage);
        assert_eq!(lens_floor.min_successful, Some(hard_reviewers));
        assert!(
            lens_floor
                .required_review_lenses
                .contains(&"adversarial".into())
        );

        let docs_policy = launch_resolvers_with(
            &review,
            &document,
            ReviewDiffSelection::Captured(vec![provenant_run::ReviewDiffContext {
                paths: vec!["docs/guide.md".into()],
                numstat: Some("1\t0\tdocs/guide.md\0".into()),
                tree: Some("tools/front-door/Cargo.toml".into()),
            }]),
            None,
            |_, _| Ok("fixture".into()),
        )
        .unwrap();
        let floor: Value = serde_json::from_str(&docs_policy["review-coverage-floor"][0]).unwrap();
        assert_eq!(floor["hard"], false);
    }

    #[test]
    fn review_family_constraints_use_eligible_pools_and_survive_template_expansion()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_routing::{
            Catalogue, RouteChoice, RouteName, RouteRequest, RoutingConfig, RoutingContext,
        };
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let family = provenant_records::AdapterSpec::get("codex")
            .ok_or("adapter")?
            .family;
        let named_family = provenant_records::AdapterSpec::get("claude")
            .ok_or("adapter")?
            .family;
        let availability: Vec<_> = ["codex", "claude"]
            .into_iter()
            .map(|adapter| provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: adapter.into(),
                executable: format!("/fixture/{adapter}"),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: "test".into(),
                catalogue_revision: catalogue.revision().to_owned(),
                account_key: Some("fixture-account".into()),
                plan: None,
                observed_models: vec!["test".into()],
                observed_unix_seconds: 80,
                expires_unix_seconds: 120,
            })
            .collect();
        let access = ["codex", "claude"].map(|adapter| provenant_routing::AccessContext {
            adapter,
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        });
        let context = RoutingContext {
            policy_account_hint: None,
            choice_context: None,
            selection_evidence: None,
            catalogue: &catalogue,
            config: &config,
            registry: provenant_providers::REGISTRY,
            availability: &availability,
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            readiness: None,
            access: &access,
            now: provenant_core::WallTime::from_micros(100_000_000)?,
            host: "family-fixture",
        };
        let document = serde_json::from_value(json!({
            "schema_version": 1, "graph_id": "family-panel",
            "nodes": [{"id": "anchor", "access": "read_only", "prompt": "Task", "route": {"alias": "senior"}}],
            "review": {
                "shape": "standard", "minimum_family_counts": {(family): 2},
                "lenses": [
                    {"name": "correctness", "tier": "senior"},
                    {"name": "tests", "tier": "senior"},
                    {"name": "contract", "tier": "senior", "required_family": named_family}
                ]
            }
        }))?;
        let resolvers = launch_resolvers_with(
            &config.review,
            &document,
            ReviewDiffSelection::NotRequested,
            None,
            |slot, required| {
                let result = provenant_routing::resolve_for_account_with_seed_excluding_family(
                    &RouteRequest {
                        intent: Default::default(),
                        choice: RouteChoice::NamedRoute(
                            RouteName::parse(slot.tier.as_str().unwrap()).unwrap(),
                        ),
                        effort: None,
                        confidential: false,
                        fallback: provenant_routing::Fallback::None,
                    },
                    &context,
                    None,
                    "family-panel",
                    0,
                    &[],
                    &[],
                    required,
                )
                .map_err(|error| error.to_string())?;
                Ok(provenant_records::AdapterSpec::get(&result.adapter)
                    .unwrap()
                    .family
                    .to_owned())
            },
        )?;
        let record: provenant_routing::PanelResolution =
            serde_json::from_str(&resolvers["review-family-resolution"][0])?;
        assert_eq!(
            record
                .slots
                .iter()
                .filter(|slot| slot.satisfied_family.as_deref() == Some(family))
                .count(),
            2
        );
        assert_eq!(
            record.slots[2].satisfied_family.as_deref(),
            Some(named_family)
        );
        let library = TemplateLibrary::bundled();
        let compiled = provenant_fabric_runtime::graph::template::Compiler {
            library: &library,
            resolvers: &resolvers,
        }
        .compile(&json!({
            "id": "review", "use": "review", "over": "@review-panel", "with": {"lens": "${item}"}
        }))
        .map_err(|error| format!("{error:?}"))?;
        for (member, slot) in compiled
            .composite
            .ok_or("panel")?
            .nodes
            .iter()
            .zip(&record.slots)
        {
            let wire = serde_json::to_value(member)?;
            let routes = wire
                .get("routes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| vec![wire["route"].clone()]);
            assert!(!routes.is_empty());
            for route in routes {
                assert_eq!(
                    route["required_family"].as_str(),
                    slot.satisfied_family.as_deref()
                );
            }
        }
        let refused = launch_resolvers_with(
            &config.review,
            &document,
            ReviewDiffSelection::NotRequested,
            None,
            |_, _| Err("candidate account unavailable; excluded by policy".into()),
        )
        .unwrap_err();
        assert!(refused.contains(named_family));
        assert!(refused.contains("count 1"));
        assert!(refused.contains("candidate account unavailable; excluded by policy"));
        Ok(())
    }
}
