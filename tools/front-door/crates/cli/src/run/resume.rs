use super::*;

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
                target,
                deliver,
                accept_review,
            } = command;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let (session, id) =
                key::open_attempt(edge, &target, crate::Want::Attempt { settled: true }, &call)?;
            let record = session.store.record(id, &call)?;
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
            let launch = run::prepare_writer_with_facts(
                writer,
                edge.roots.state.as_path(),
                record.project_id,
                session.store.repo_facts(),
            )?;
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
