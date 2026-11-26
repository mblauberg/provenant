use super::*;

/// The live holder calls this only after success and confinement checks. A
/// sealed start marker lets recovery reconcile an interrupted external effect.
pub(crate) fn deliver_once(
    host: &HostDeliveryContext,
    writer: &WriterRequest,
    request: &WriterDeliveryRequest,
    attempt: &Directory,
    identity: &str,
) -> Result<WriterDeliveryReceipt> {
    if let Some(raw) =
        crate::artifacts::read_sealed(attempt, "delivery-cancelled.json", crate::RECORD_LIMIT)?
    {
        return crate::db::decode(&raw);
    }
    if let Some(raw) = crate::artifacts::read_sealed(attempt, "delivery.json", crate::RECORD_LIMIT)?
    {
        return crate::db::decode(&raw);
    }
    let started =
        crate::artifacts::read_sealed(attempt, "delivery-started.json", crate::RECORD_LIMIT)?;
    if host
        .bound
        .as_ref()
        .is_none_or(|bound| bound.destination != *request)
    {
        return Err(RunError::Validation(
            "delivery destination changed after launch",
        ));
    }
    let mut receipt = match &started {
        Some(raw) => crate::db::decode(raw)?,
        None => WriterDeliveryReceipt {
            extra: Default::default(),
            schema_version: 1,
            remote: request.remote.clone(),
            branch: request.branch.clone(),
            head: host.head(writer).ok(),
            review_acceptance: None,
            state: WriterDeliveryState::Failed {
                reason: None,
                extra: Default::default(),
                stage: Stage::Interrupted,
            },
            integration: None,
        },
    };
    if started.is_some()
        && (receipt.remote != request.remote
            || receipt.branch != request.branch
            || receipt.head != Some(host.head(writer)?))
    {
        return Err(RunError::Validation(HEAD_MISMATCH));
    }
    if started.is_none() {
        crate::artifacts::seal(
            attempt,
            "delivery-started.json",
            &crate::db::bytes(&receipt)?,
            crate::RECORD_LIMIT,
        )?;
    }
    {
        let unchanged = receipt
            .head
            .zip(host.bound.as_ref())
            .is_some_and(|(head, bound)| head == bound.base_head);
        receipt.state = if unchanged {
            WriterDeliveryState::NoNewCommits {
                extra: Default::default(),
            }
        } else {
            match receipt
                .head
                .ok_or_else(|| delivery_refusal(Stage::Head, "delivery head was not captured"))
                .and_then(|head| {
                    let body = host.commit_body(writer, head, identity)?;
                    crate::artifacts::seal(
                        attempt,
                        "pull-request-body.md",
                        body.as_bytes(),
                        crate::RESULT_LIMIT,
                    )?;
                    host.push(writer, request, head, &body)
                }) {
                Ok(published) => {
                    receipt.integration = published.integration;
                    WriterDeliveryState::Delivered {
                        extra: Default::default(),
                        pull_request: published.pull_request,
                    }
                }
                Err(error)
                    if matches!(
                        error,
                        RunError::Delivery {
                            stage: Stage::Push,
                            ..
                        }
                    ) && !matches!(request.mode, crate::DeliveryMode::Integrate(_))
                        && !host
                            .cancellation
                            .as_ref()
                            .is_some_and(|cancel| cancel.is_cancelled()) =>
                {
                    // A failed push command may have reached the remote. Leave
                    // the start marker unresolved for a bound remote lookup.
                    return Err(error);
                }
                Err(error) => failed_state(&error),
            }
        };
    }
    // Keep the start marker so recovery can reconcile an aborted remote command.
    let name = if matches!(receipt.state, WriterDeliveryState::Failed { .. })
        && host
            .cancellation
            .as_ref()
            .is_some_and(|cancel| cancel.is_cancelled())
    {
        "delivery-cancelled.json"
    } else {
        "delivery.json"
    };
    crate::artifacts::seal(
        attempt,
        name,
        &crate::db::bytes(&receipt)?,
        crate::RECORD_LIMIT,
    )?;
    Ok(receipt)
}
