//! Host side of a lane's graph frames.
//!
//! The relay serves one attempt, so the host finds the running node that owns
//! that attempt and acts as it: the lane never names its graph or parent.
//! Admission takes the graph's journal lock, goes through
//! `provenant_fabric_runtime::graph::relay#admit_child` and saves the
//! checkpoint before the receipt is returned, so a lost reply is answered by
//! replaying the same idempotency key.
use super::Context;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_mcp::tools::{FacadeError, ServiceError};
use provenant_fabric_runtime::graph::{
    FileJournal, GraphJournal,
    patch::{LiveGraph, PatchContext, scan_for_secrets},
    relay::{self, RelayOp},
    template::{Layer, TemplateLibrary},
};
use serde_json::{Value, json};

fn refused(code: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::Cli {
        code: code.into(),
        message: message.into(),
        data: Value::Null,
    }
}

fn journal_error<E>(_: E) -> ServiceError {
    ServiceError::Facade(FacadeError::io("access graph journal"))
}

/// Serve one graph frame for `attempt` on this host.
pub fn serve(
    context: &Context,
    attempt: &str,
    arguments: Value,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<Value, ServiceError> {
    let op: RelayOp = serde_json::from_value(arguments)
        .map_err(|error| refused("invalid_input", format!("invalid graph frame: {error}")))?;
    let state = &context.roots.state;
    let project = context.identity.project;
    let reference = format!("{}/{attempt}", context.identity.host);
    let not_a_node = || {
        refused(
            "graph_relay_unavailable",
            "this attempt is not a running graph node",
        )
    };
    let graph_id = FileJournal::pending_graph_ids(state, project)
        .map_err(journal_error)?
        .into_iter()
        .find(|id| {
            FileJournal::read_checkpoint(state, project, id)
                .ok()
                .flatten()
                .is_some_and(|checkpoint| {
                    relay::parent_of_attempt(&checkpoint, &reference).is_some()
                })
        })
        .ok_or_else(not_a_node)?;
    let mut journal = FileJournal::open(state, project, &graph_id, budget, cancel, clock)
        .map_err(journal_error)?;
    let mut checkpoint = journal
        .load(&graph_id)
        .map_err(journal_error)?
        .ok_or_else(not_a_node)?;
    let document = FileJournal::execution(state, project, &graph_id)
        .map_err(journal_error)?
        .ok_or_else(not_a_node)?
        .document;
    // Checked again under the lock: the parent may have settled meanwhile.
    let parent = relay::parent_of_attempt(&checkpoint, &reference)
        .ok_or_else(not_a_node)?
        .to_owned();
    let value = match op {
        RelayOp::Add(request) => {
            let library = template_library(&context.home, &context.cwd)
                .map_err(|error| refused("invalid_template", error))?;
            let config =
                provenant_config::read_with_domains(&context.roots, &crate::domains::SECTIONS)
                    .map_err(|error| refused("invalid_config", error.to_string()))?;
            let resolvers = launch_resolvers(&config.domains.routing.review, &document)
                .map_err(|error| refused("invalid_review", error))?;
            let patch_context = PatchContext {
                journal: None,
                library: &library,
                resolvers: &resolvers,
                scan: &scan_for_secrets,
                limits: &context.config.graph.limits,
            };
            let writer_base = if request.writer {
                let path = relay::parent_worktree(
                    &document,
                    &checkpoint,
                    &parent,
                    &context.config.graph.limits,
                )
                .map_err(|error| refused("invalid_graph", error))?
                .ok_or_else(|| {
                    refused(
                        "writer_parent_required",
                        "writer children require a parent worktree",
                    )
                })?;
                let writer = provenant_run::resolve_writer(
                    state.as_path(),
                    &context.cwd,
                    std::path::Path::new(&path),
                )
                .map_err(|error| refused("writer_parent_required", error.to_string()))?;
                let launch = provenant_run::prepare_writer(&writer, state.as_path(), project)
                    .map_err(|error| refused("writer_parent_required", error.to_string()))?;
                Some((
                    launch
                        .branch
                        .as_str()
                        .trim_start_matches("refs/heads/")
                        .to_owned(),
                    launch.source_commit().to_string(),
                ))
            } else {
                None
            };
            let receipt = relay::admit_child_bound(
                &mut LiveGraph {
                    document: &document,
                    checkpoint: &mut checkpoint,
                },
                &parent,
                &request,
                &patch_context,
                writer_base
                    .as_ref()
                    .map(|(branch, commit)| (branch.as_str(), commit.as_str())),
            )
            .map_err(|error| refused(error.code, error.message))?;
            if receipt.replayed {
                // Nothing changed; the earlier admission is already journalled.
                return Ok(serde_json::to_value(receipt)?);
            }
            if let Some(path) = receipt.worktree.as_deref() {
                let source = relay::child_source(
                    &document,
                    &checkpoint,
                    &receipt.node,
                    &context.config.graph.limits,
                )
                .map_err(|error| refused("invalid_graph", error))?
                .ok_or_else(|| refused("invalid_graph", "writer child has no source"))?;
                provision_child(context, path, &source, cancel)?;
            }
            serde_json::to_value(receipt)?
        }
        RelayOp::Children => {
            return Ok(relay::child_states(&checkpoint, &parent));
        }
        RelayOp::Wait { waiting } => {
            let call = provenant_run::Request { clock, cancel };
            let execution = provenant_run::ExecutionContext {
                state_root: state.clone(),
                project_id: project,
                actor_id: context.identity.inbox.actor,
                read_prompts: false,
            };
            let store = provenant_run::RunStore::open(&execution, &call).map_err(journal_error)?;
            let id = provenant_core::AttemptId::parse(attempt).map_err(journal_error)?;
            let admitted = match store.set_child_wait(id, waiting, &call) {
                Ok(admitted) => admitted,
                // Store/ledger locks are another admission wait. Preserve the
                // completed child result and retry after releasing the journal.
                Err(provenant_run::RunError::Contention) if !waiting => false,
                Err(error) => return Err(journal_error(error)),
            };
            // Keep the graph slot yielded until host admission is reclaimed.
            // Each retry releases the journal lock so children can settle.
            if !admitted {
                return Ok(json!({"graph_id": graph_id, "node": parent,
                    "waiting": true, "reclaiming": true}));
            }
            relay::set_waiting(&mut checkpoint, &parent, waiting);
            json!({"graph_id": graph_id, "node": parent, "waiting": waiting})
        }
    };
    journal.save(&checkpoint).map_err(journal_error)?;
    Ok(value)
}

/// Resolve dynamic panels at the host, using the graph's declared review risk.
fn launch_resolvers(
    review: &provenant_routing::ReviewConfig,
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let mut request: provenant_routing::ReviewRequest = document
        .extra
        .get("review")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid graph review request: {error}"))?
        .unwrap_or_default();
    if let Some(context) = &document.context {
        request.risk_tier = request.risk_tier.or_else(|| context.risk_tier.clone());
        request.kind = request.kind.or_else(|| context.task_kind.clone());
    }
    let panel = review.resolve_panel(&request, None, &[], false)?;
    Ok(std::collections::BTreeMap::from([(
        "review-panel".into(),
        panel.slots.into_iter().map(|slot| slot.lens).collect(),
    )]))
}

fn provision_child(
    context: &Context,
    path: &str,
    source: &str,
    cancel: &CancelToken,
) -> Result<(), ServiceError> {
    use provenant_confinement::worktrees;
    worktrees::with_context(
        &context.roots,
        &context.home,
        &context.cwd,
        &provenant_os::clock::SystemClock::new(),
        cancel,
        |worktree_context| provision_in_context(worktree_context, path, source, cancel),
    )
    .map_err(|error| refused("writer_worktree", error.to_string()))
}

fn provision_in_context(
    context: &provenant_confinement::worktrees::WorktreeContext<'_>,
    path: &str,
    source: &str,
    cancel: &CancelToken,
) -> Result<(), provenant_confinement::worktrees::WorktreeError> {
    use provenant_confinement::worktrees::{self, WorktreeCreate, WorktreeError};
    let branch = path
        .strip_prefix(".worktrees/")
        .ok_or(WorktreeError::InvalidRequest)?;
    let expected = context.repo.root_path().join(path);
    if let Some(existing) = worktrees::list(context, cancel)?
        .into_iter()
        .find(|entry| entry.path == expected)
    {
        if existing.branch.as_deref() == Some(format!("refs/heads/{branch}").as_str())
            && existing.head.to_string() == source
            && existing.dirty == worktrees::WorktreeCondition::Clean
            && existing.writer == worktrees::WriterState::Free
        {
            return Ok(());
        }
        return Err(WorktreeError::InvalidRequest);
    }
    worktrees::create(
        context,
        WorktreeCreate {
            branch: branch.into(),
            base: Some(source.into()),
        },
        cancel,
    )
    .map(|_| ())
}

/// Lane side: send `op` to the host relay of the attempt this lane runs as.
/// `provenant run` and `fabric_run` inside a lane call this to add a child.
pub fn from_lane(
    op: &RelayOp,
    cancel: &CancelToken,
    within: std::time::Duration,
) -> Result<Value, ServiceError> {
    let attempt = std::env::var("PROVENANT_PARENT_ATTEMPT_ID")
        .ok()
        .and_then(|id| provenant_core::AttemptId::parse(&id).ok())
        .ok_or_else(|| refused("graph_relay_unavailable", "not running inside a lane"))?;
    provenant_fabric_mcp::lane::graph(
        &provenant_providers::LaneRuntime::fabric_socket(attempt),
        serde_json::to_value(op)?,
        cancel,
        within,
    )
}

/// Wait for this lane's children, yielding its graph concurrency slot. Intake
/// calls this after adding children when a wait is requested. Returns terminal
/// child states, including failures, for the caller to report.
pub fn wait_for_children(
    cancel: &CancelToken,
    within: std::time::Duration,
) -> Result<Value, ServiceError> {
    while_waiting(cancel, within, || {
        poll_children(cancel, within, |remaining| {
            from_lane(&RelayOp::Children, cancel, remaining)
        })
    })
}

fn poll_children(
    cancel: &CancelToken,
    within: std::time::Duration,
    mut poll: impl FnMut(std::time::Duration) -> Result<Value, ServiceError>,
) -> Result<Value, ServiceError> {
    let started = std::time::Instant::now();
    loop {
        if cancel.is_cancelled() {
            return Err(refused("cancelled", "child wait cancelled"));
        }
        let remaining = within
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| refused("timeout", "child wait timed out"))?;
        let value = poll(remaining)?;
        let children = value["children"]
            .as_array()
            .ok_or_else(|| refused("invalid_reply", "missing child states"))?;
        if children.iter().all(|child| child["settled"] == true) {
            return Ok(value);
        }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(100)));
    }
}

fn template_library(
    home: &std::path::Path,
    repository: &std::path::Path,
) -> Result<TemplateLibrary, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    if !config.is_absolute() {
        return Err("XDG_CONFIG_HOME must be absolute".into());
    }
    layered_templates(
        &config.join("provenant/templates"),
        &repository.join(".provenant/templates"),
    )
}

fn layered_templates(
    user: &std::path::Path,
    repository: &std::path::Path,
) -> Result<TemplateLibrary, String> {
    let mut library = TemplateLibrary::bundled();
    library.load_dir(Layer::User, user)?;
    library.load_dir(Layer::Repository, repository)?;
    Ok(library)
}

/// Yield the parent slot for a child wait, then reclaim it on every return path.
/// Cleanup uses its own cancellation token so cancelling the wait still clears
/// the host's retained waiting flag.
pub fn while_waiting<T>(
    cancel: &CancelToken,
    within: std::time::Duration,
    wait: impl FnOnce() -> Result<T, ServiceError>,
) -> Result<T, ServiceError> {
    with_waiting(
        |waiting| {
            let cleanup = CancelToken::new();
            wait_for_admission(|| {
                from_lane(
                    &RelayOp::Wait { waiting },
                    if waiting { cancel } else { &cleanup },
                    within,
                )
            })
        },
        wait,
    )
}

fn wait_for_admission(
    mut send: impl FnMut() -> Result<Value, ServiceError>,
) -> Result<(), ServiceError> {
    while send()?["reclaiming"] == true {
        // Each frame releases the graph lock. Retain the completed child-wait
        // result in with_waiting until the parent has admission again.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

fn with_waiting<T>(
    mut send: impl FnMut(bool) -> Result<(), ServiceError>,
    wait: impl FnOnce() -> Result<T, ServiceError>,
) -> Result<T, ServiceError> {
    // A failed reply may follow a committed Wait true frame. Always clear it.
    let result = send(true).and_then(|()| wait());
    let cleared = send(false);
    match result {
        Ok(value) => cleared.map(|()| value),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod wait_tests {
    use super::*;

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
                    poll_children(&cancel, std::time::Duration::from_secs(2), |_| {
                        polls += 1;
                        if cancelled {
                            cancel.cancel();
                        }
                        Ok(json!({"children":[{"node":"a--b", "settled":polls == 2}]}))
                    })
                },
            );
            assert_eq!(frames, [true, false]);
            assert_eq!(result.is_ok(), !cancelled);
            assert_eq!(polls, if cancelled { 1 } else { 2 });
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
        let result = with_waiting(
            |_| Ok(()),
            || {
                poll_children(&CancelToken::new(), std::time::Duration::ZERO, |_| {
                    panic!("expired")
                })
            },
        );
        assert!(matches!(result, Err(ServiceError::Cli { code, .. }) if code == "timeout"));
    }
}

#[cfg(test)]
mod provisioning_tests {
    use super::*;

    #[test]
    fn writer_child_worktree_is_created_at_source_and_dirty_reuse_is_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
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
        inputs.state_dir = Some(root.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots, &home, &project)?;
        let cancel = CancelToken::new();
        let path = ".worktrees/graph-parent--child";
        provision_child(&context, path, &source, &cancel)?;
        let output = provenant_os::fixture::git_command()
            .current_dir(project.join(path))
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?.trim(), source);
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
        let provision = || {
            provenant_confinement::worktrees::with_context_using_workers(
                &context.roots,
                &context.home,
                &context.cwd,
                &provenant_os::clock::SystemClock::new(),
                &cancel,
                &Free,
                |ctx| provision_in_context(ctx, path, &source, &cancel),
            )
        };
        provision()?;
        std::fs::write(project.join(path).join("parent-work"), "preserve")?;
        assert!(provision().is_err());
        assert_eq!(
            std::fs::read_to_string(project.join(path).join("parent-work"))?,
            "preserve"
        );
        Ok(())
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    #[test]
    fn relay_review_resolver_uses_graph_risk_and_tags() {
        let review = provenant_routing::RoutingConfig::default().review;
        let mut document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(json!({
                "schema_version": 1, "graph_id": "panel-risk", "nodes": [],
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
        document
            .extra
            .insert("review".into(), json!({"risk_tags": ["security"]}));
        let crucial = launch_resolvers(&review, &document).unwrap();
        assert!(crucial["review-panel"].iter().any(|lens| lens == "tests"));
        assert!(
            crucial["review-panel"]
                .iter()
                .any(|lens| lens == "security-privacy")
        );
    }
}
