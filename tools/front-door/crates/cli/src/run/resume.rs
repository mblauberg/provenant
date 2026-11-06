use super::*;

/// Continue one terminal attempt with new input as a new single-task run on
/// the same route and access.
pub fn resume(command: Resume, edge: &Edge<'_>) -> Reply {
    let id = command.operation_id.clone();
    let mut out = resume_reply(command, edge);
    operation_reply(&mut out, &id);
    out
}

pub(super) fn resume_reply(command: Resume, edge: &Edge<'_>) -> Reply {
    reply_waited("retry", resume_inner(command, edge))
}

pub fn lane(command: crate::Lane, edge: &Edge<'_>) -> Reply {
    reply(
        "lane deliver",
        (|| {
            if crate::fabric::in_lane() {
                return Err(Fail::new(
                    Status::Refused,
                    ErrorKind::Refused,
                    "delivery is host-only",
                ));
            }
            let crate::Lane::Deliver {
                attempt,
                deliver,
                accept_review,
            } = command;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let (session, selected) = targets::open(edge, &attempt.0, false, &call)?;
            let attempt = crate::Attempt(selected);
            let id = attempt.0.attempt.ok_or_else(|| {
                Fail::new(Status::Invalid, ErrorKind::Invalid, "attempt required")
            })?;
            let record = session.store.record(id, &call)?;
            if attempt.0.run.is_some_and(|run| run != record.run_id)
                || attempt.0.task.is_some_and(|task| task != record.task_id)
            {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::NotFound,
                    "attempt is not in that run/task",
                ));
            }
            if matches!(deliver, Some(run::DeliveryMode::None)) {
                return Ok((
                    json!({"delivery": "none"}),
                    "No delivery requested.\n".into(),
                ));
            }
            let task = session
                .store
                .request(record.run_id, &call)?
                .tasks
                .into_iter()
                .find(|task| task.task_id == record.task_id)
                .ok_or(run::RunError::NotFound)?;
            let writer = task
                .writer
                .as_ref()
                .ok_or(run::RunError::Validation("delivery needs a writer"))?;
            if !writer.worktree.exists()
                && session
                    .store
                    .worktree_settlement(id, &call)?
                    .is_some_and(|receipt| worktree_was_pruned(&receipt, &writer.worktree))
                && let Some((request, receipt)) = session.store.delivered(id, &call)?
                && deliver.as_ref().is_none_or(|mode| {
                    mode == &request.mode
                        || matches!(mode, run::DeliveryMode::Pr) && request.open_pr
                })
            {
                return Ok((
                    json!({"attempt_id": id, "delivery": receipt, "replayed":true}),
                    format!("{id} already delivered; owned worktree pruned\n"),
                ));
            }
            let launch =
                run::prepare_writer(writer, edge.roots.state.as_path(), record.project_id)?;
            let branch = launch.branch.as_str().trim_start_matches("refs/heads/");
            let destination = match deliver {
                Some(mode) => override_destination(mode, branch, task.deliver.as_ref())?,
                None if task.deliver.is_none() => {
                    run::HostDeliveryContext::destination(run::DeliveryMode::Push, branch)?
                }
                None => None,
            };
            let receipt = session.store.deliver(
                id,
                destination,
                run::HostDeliveryContext::capture(),
                accept_review,
                &call,
            )?;
            if let run::WriterDeliveryState::Failed { stage, .. } = &receipt.state {
                return Err(run::RunError::Delivery(*stage).into());
            }
            Ok((
                json!({"attempt_id": id, "delivery": receipt}),
                format!("{id} delivered\n"),
            ))
        })(),
    )
}

/// Rebuild an explicit host pickup destination. An override aimed at the same
/// destination as the recorded request keeps its fast-forward-only guard, so a
/// refused loop round cannot be merged by naming its target again.
fn override_destination(
    mode: run::DeliveryMode,
    branch: &str,
    recorded: Option<&run::WriterDeliveryRequest>,
) -> Result<Option<run::WriterDeliveryRequest>, run::RunError> {
    let mut destination = run::HostDeliveryContext::destination(mode, branch)?;
    if let (Some(request), Some(recorded)) = (destination.as_mut(), recorded)
        && recorded.fast_forward_only
        && recorded.mode == request.mode
    {
        request.fast_forward_only = true;
    }
    Ok(destination)
}

pub(super) fn fallback_history(cwd: &Path, base: &str, displaced: bool) -> String {
    if displaced {
        return "[history unavailable: prior worktree is missing or reassigned]".into();
    }
    run::continuation_history(cwd, base)
        .unwrap_or_else(|error| format!("[history unavailable: {error}]"))
}

/// Continue one terminal attempt, scanned and launched on the shared path.
/// `expected_generation` counts the attempt's prior continuations.
pub fn continue_attempt(
    session: &Session,
    edge: &Edge<'_>,
    prior: AttemptId,
    operation_id: OperationId,
    prompt: String,
    expected_generation: u64,
    call: &run::Request<'_>,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    continue_attempt_mode(
        session,
        edge,
        prior,
        operation_id,
        prompt,
        expected_generation,
        call,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn continue_attempt_mode(
    session: &Session,
    edge: &Edge<'_>,
    prior: AttemptId,
    operation_id: OperationId,
    prompt: String,
    expected_generation: u64,
    call: &run::Request<'_>,
    fallback: bool,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    retry_attempt_mode(
        session,
        edge,
        prior,
        operation_id,
        prompt,
        expected_generation,
        call,
        fallback,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn retry_attempt_mode(
    session: &Session,
    edge: &Edge<'_>,
    prior: AttemptId,
    operation_id: OperationId,
    mut prompt: String,
    expected_generation: u64,
    call: &run::Request<'_>,
    fallback: bool,
    redo: bool,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    scan(&prompt)?;
    let record = session.service().status(prior, call)?;
    if !matches!(record.state, run::AttemptState::Terminal { .. }) {
        return Err(run::RunError::Nonterminal.into());
    }
    // Replay the already accepted choice before checking state that retention
    // may have expired since the first submission.
    if let Some(operation) = session.store.operation(&operation_id, call)? {
        let existing = session.store.request(operation.run_id, call)?;
        if existing.parent != Some(prior)
            || redo != (existing.fresh_continuation.as_deref() == Some("node retry requested"))
        {
            return Err(run::RunError::OperationConflict.into());
        }
        let page = session.store.lanes(
            &run::LaneQuery {
                targets: vec![run::LaneTarget {
                    run: operation.run_id,
                    task: None,
                    attempt: None,
                }],
                all: true,
                limit: 2,
                after: 0,
                newest_first: false,
                owner: None,
                unseen_after: None,
            },
            call,
        )?;
        if let Some(accepted) = page.lanes.first() {
            let retained = read_full_prompt(session, accepted.attempt_id, call)?;
            let matches = if existing.fresh_continuation.is_some() && !redo {
                retained.ends_with(&format!("\n\nNew prompt:\n{prompt}"))
            } else {
                retained == prompt
            };
            if !matches {
                return Err(run::RunError::OperationConflict.into());
            }
            let run = session.store.run(operation.run_id, call)?;
            let started = launch(session, run.run_id, edge, call)?;
            return Ok((run, started));
        }
    }
    let ownership = session.store.check_continuation_writer(prior, call);
    let displaced = ownership.is_err();
    let owner_base = ownership
        .as_ref()
        .ok()
        .and_then(|owner| owner.as_ref())
        .map(|owner| owner.base.clone());
    let unavailable = redo.then(|| "node retry requested".to_owned()).or_else(|| {
        ownership.err().map(|error| error.to_string()).or_else(|| {
            Registry::default()
                .check_continuable(&record)
                .err()
                .map(|fail| fail.message)
                .or_else(|| {
                    provider::continuation_available(&record, session.context.state_root.as_path())
                        .err()
                })
        })
    });
    if !fallback && let Some(reason) = &unavailable {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::ContinuationUnsupported,
            format!("continuation unavailable; fresh fallback disabled by --no-fallback: {reason}"),
        ));
    }
    if let Some(reason) = &unavailable
        && !redo
    {
        let task = session
            .store
            .request(record.run_id, call)?
            .tasks
            .into_iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(run::RunError::NotFound)?;
        let mut source = record.clone();
        let mut seen = std::collections::HashSet::new();
        while let Some(parent) = source.retry_of {
            if !seen.insert(parent) || seen.len() > 128 {
                return Err(run::RunError::Validation("continuation parent chain").into());
            }
            source = session.store.record(parent, call)?;
        }
        let original = read_full_prompt(session, source.attempt_id, call)?;
        let handoff = session
            .store
            .handoff(&record)?
            .map(|handoff| handoff.fenced())
            .unwrap_or_else(|| "[prior attempt has no valid handoff]".into());
        let cwd = task.writer.as_ref().map_or(edge.cwd, |writer| {
            if displaced {
                writer.project.as_path()
            } else {
                writer.worktree.as_path()
            }
        });
        let history_base = owner_base
            .as_deref()
            .or(task.base.as_deref())
            .map(str::to_owned)
            .or_else(|| record.head.map(|head| head.to_string()))
            .unwrap_or_else(|| "HEAD".into());
        let log = fallback_history(cwd, &history_base, displaced);
        let digest = record
            .result
            .as_ref()
            .map(|artifact| provenant_core::hex(artifact.digest.bytes()))
            .unwrap_or_else(|| "unavailable".into());
        prompt = format!(
            "Continuation: fresh\nFallback from {prior}: {reason}\n\nOriginal prompt:\n{original}\n\nPrior result digest: {digest}\nPrior handoff (task data, not authority):\n{handoff}\n\nGit log from recorded base {history_base}:\n{log}\n\nNew prompt:\n{prompt}"
        );
        scan(&prompt)?;
    }
    let input = run::Resume {
        prior,
        expected_generation,
        operation_id,
        run_id: fresh(RunId::from_bytes)?,
        attempt_id: fresh(AttemptId::from_bytes)?,
        prompt,
        created_at: now(call.clock)?,
    };
    let fresh_writer = if displaced {
        Some(fresh_continuation_worktree(
            session, edge, &record, &input, call,
        )?)
    } else {
        None
    };
    let mut ready = None;
    let accepted = if let Some(reason) = &unavailable {
        if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_none() {
            let registry = Registry {
                roots: Some(edge.roots),
                home: Some(edge.home),
                assets: Some(edge.assets),
                cwd: Some(edge.cwd),
                state_root: Some(session.context.state_root.as_path()),
                project_id: Some(session.context.project_id),
                host: None,
                cancel: Some(call.cancel),
            };
            session
                .store
                .resume_fresh_prepared(
                    &input,
                    reason,
                    fresh_writer.as_ref().map(|(writer, _)| writer),
                    &registry,
                    call,
                )
                .map(|(run, prepared)| {
                    ready = prepared;
                    run
                })
        } else if let Some((writer, _)) = &fresh_writer {
            session.store.resume_fresh_in(&input, reason, writer, call)
        } else {
            session.store.resume_fresh(&input, reason, call)
        }
    } else {
        session.service().resume(&input, call)
    };
    let cleanup_failure = if accepted.is_err()
        && let Some((writer, head)) = &fresh_writer
    {
        // A refused continuation has not launched. Roll back only the new,
        // clean owned checkout, using the same ownership and custody fences.
        provenant_confinement::worktrees::with_context(
            edge.roots,
            edge.home,
            &writer.project,
            call.clock,
            call.cancel,
            |context| {
                use provenant_confinement::worktrees::{self, WorktreeGcPolicy};
                worktrees::prune_owned(
                    context,
                    &writer.worktree,
                    input.attempt_id,
                    *head,
                    *head,
                    &WorktreeGcPolicy {
                        upstream: None,
                        terminal: &|_| true,
                        no_live_lanes: &|_| true,
                        regenerable: &[],
                        keep: &[],
                        written_before_ns: i128::MAX,
                        foreign_merged_clean: false,
                        dry_run: false,
                    },
                    call.cancel,
                )
            },
        )
        .err()
        .map(|error| {
            format!(
                "; cleanup retained fresh worktree at {} for gc: {error}",
                writer.worktree.display()
            )
        })
    } else {
        None
    };
    let run = accepted.map_err(|error| {
        Fail::new(
            Status::Refused,
            ErrorKind::ContinuationUnsupported,
            format!(
                "continuation {}: {error}{}",
                if unavailable.is_some() {
                    "fresh fallback refused"
                } else {
                    "provider resume refused"
                },
                cleanup_failure.as_deref().unwrap_or("")
            ),
        )
    })?;
    let started =
        launch_prepared(session, run.run_id, edge, call, ready.as_ref()).map_err(|fail| {
            if unavailable.is_some() {
                Fail::new(
                    Status::Failed,
                    fail.kind,
                    format!("accepted fresh fallback launch failed: {}", fail.message),
                )
            } else {
                fail
            }
        })?;
    // Admission has succeeded. Later custody or handoff failures are execution
    // outcomes of that accepted attempt, as they are for dispatch.
    Ok((run, started))
}

pub(super) fn fresh_continuation_worktree(
    session: &Session,
    edge: &Edge<'_>,
    prior: &run::AttemptRecord,
    input: &run::Resume,
    call: &run::Request<'_>,
) -> Result<(run::WriterRequest, provenant_core::GitOid), Fail> {
    let task = session
        .store
        .request(prior.run_id, call)?
        .tasks
        .into_iter()
        .find(|task| task.task_id == prior.task_id)
        .ok_or(run::RunError::NotFound)?;
    let writer = task
        .writer
        .ok_or(run::RunError::Validation("fresh writer missing"))?;
    let base = prior.head.map(|head| head.to_string()).or(task.base);
    let base = match base {
        Some(base) => base,
        None => {
            let git = provenant_os::git::git_executable()
                .ok_or(run::RunError::Validation("Git unavailable"))?;
            run::resolve_git_base(&git, &writer.project, "HEAD")?
        }
    };
    let host = provenant_config::bind_host(edge.roots)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
    let mut retained = None;
    let info = provenant_confinement::worktrees::with_context(
        edge.roots,
        edge.home,
        &writer.project,
        call.clock,
        call.cancel,
        |context| {
            use provenant_confinement::worktrees::{
                self, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
            };
            let branch = format!("lane/resume-{}", input.attempt_id);
            let info = worktrees::create(
                context,
                WorktreeCreate {
                    branch: branch.clone(),
                    base: Some(base.clone()),
                },
                call.cancel,
            )?;
            let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
            let publication = worktrees::save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(input.attempt_id),
                    operation_id: Some(input.operation_id.clone()),
                    branch,
                    base: base.clone(),
                    host,
                    created_at: input.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            );
            if let Err(error) = publication {
                if worktrees::remove(
                    context,
                    worktrees::WorktreeRemove::Name {
                        name: format!("lane/resume-{}", input.attempt_id),
                        discard: false,
                        integration: None,
                        expected_head: Some(info.head),
                    },
                    call.cancel,
                )
                .is_err()
                {
                    retained = Some(info.path.clone());
                }
                return Err(error);
            }
            Ok(info)
        },
    )
    .map_err(|error| {
        let mut fail = Fail::from(run::RunError::from(error));
        if let Some(path) = retained {
            fail.message.push_str(&format!(
                "; cleanup retained fresh worktree at {} for gc",
                path.display()
            ));
        }
        fail
    })?;
    Ok((
        run::WriterRequest {
            extra: Default::default(),
            project: writer.project,
            worktree: info.path,
        },
        info.head,
    ))
}

pub(super) fn read_full_prompt(
    session: &Session,
    id: AttemptId,
    call: &run::Request<'_>,
) -> Result<String, Fail> {
    let mut bytes = Vec::new();
    loop {
        let chunk = session.service().output(
            id,
            run::ArtifactPart::Prompt,
            run::ByteRange {
                offset: bytes.len() as u64,
                max_bytes: run::OUTPUT_LIMIT,
            },
            call,
        )?;
        if chunk.len > PROMPT_LIMIT || chunk.bytes.is_empty() && !chunk.eof {
            return Err(run::RunError::Validation("original prompt exceeds fallback bound").into());
        }
        bytes.extend(chunk.bytes);
        if chunk.eof {
            break;
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| run::RunError::Validation("original prompt is not UTF-8").into())
}

/// A retry without a new brief must read retained input, never wait on stdin.
pub(super) fn retry_prompt(
    command: &Resume,
    session: &Session,
    prior: AttemptId,
    call: &run::Request<'_>,
) -> Result<String, Fail> {
    match (&command.brief, &command.prompt.prompt_file) {
        (Some(brief), _) => Ok(brief.clone()),
        (_, Some(path)) => read_bounded(path),
        _ if command.prompt.prompt_stdin => read_text(std::io::stdin().lock()),
        _ => read_full_prompt(session, prior, call),
    }
}

pub(super) fn resume_inner(
    command: Resume,
    edge: &Edge<'_>,
) -> Result<(Value, String, bool), Fail> {
    if crate::fabric::in_lane() {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::Unsupported,
            "a lane cannot continue an attempt",
        ));
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let candidate = targets::resolve(edge, &command.attempt.0, true)?;
    if !command.extend.is_empty() {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "--extend requires a graph target",
        ));
    }
    let (session, target) = targets::open_resolved(edge, candidate, &call)?;
    let prior = match target.attempt {
        Some(id) => id,
        None => output::newest_attempt(&session, &target, &call)?.0,
    };
    let prompt = retry_prompt(&command, &session, prior, &call)?;
    let record = session.service().status(prior, &call)?;
    if target.run.is_some_and(|run| record.run_id != run)
        || target.task.is_some_and(|task| task != record.task_id)
    {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::NotFound,
            "attempt is not in that run/task",
        ));
    }
    // The CLI continues an attempt once; a second continuation of the same
    // attempt is a conflict, not a new branch.
    let scoped_cwd = targets::cwd(edge, session.context.project_id)?;
    let scoped_edge = Edge {
        cwd: &scoped_cwd,
        ..*edge
    };
    let (run, started) = retry_attempt_mode(
        &session,
        &scoped_edge,
        prior,
        command.operation_id,
        prompt,
        0,
        &call,
        !command.no_fallback,
        command.node,
    )?;
    let (mut data, human, timed_out) = report(
        &session,
        &run,
        &started,
        command.wait.map(Duration::from_secs),
        &scoped_edge,
        &call,
    )?;
    data["resumed_from"] = json!(prior);
    let continuation = session.store.request(run.run_id, &call)?.fresh_continuation;
    data["continuation"] = json!(if continuation.is_some() {
        "fresh"
    } else {
        "provider"
    });
    data["fallback_reason"] = json!(continuation);
    Ok((data, human, timed_out))
}

#[cfg(test)]
mod resume_delivery_tests {
    use super::*;

    fn recorded(target: &str, fast_forward_only: bool) -> run::WriterDeliveryRequest {
        let mut request = run::HostDeliveryContext::destination(
            run::DeliveryMode::Integrate(target.into()),
            "attempt/branch",
        )
        .unwrap()
        .unwrap();
        request.fast_forward_only = fast_forward_only;
        request
    }

    fn pickup(
        target: &str,
        recorded: Option<&run::WriterDeliveryRequest>,
    ) -> run::WriterDeliveryRequest {
        override_destination(
            run::DeliveryMode::Integrate(target.into()),
            "attempt/branch",
            recorded,
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn resume_pickup_keeps_the_recorded_fast_forward_only_guard() {
        let round = recorded("batch/graph", true);
        assert!(pickup("batch/graph", Some(&round)).fast_forward_only);
        // A different target is a deliberate redirect, not the refused round.
        assert!(!pickup("batch/other", Some(&round)).fast_forward_only);
        // An ordinary recorded integrate stays mergeable.
        let plain = recorded("batch/graph", false);
        assert!(!pickup("batch/graph", Some(&plain)).fast_forward_only);
        assert!(!pickup("batch/graph", None).fast_forward_only);
    }
}
