//! The graph/target boundary preserves RunStore continuation identities.
#![allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
use super::*;
use provenant_fabric_runtime::graph::*;
use std::convert::Infallible;

#[derive(Default)]
struct Journal(Option<GraphCheckpoint>);
impl GraphJournal for Journal {
    type Error = Infallible;
    fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.0.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        // Exercise the persisted checkpoint, rather than sharing mutable rows.
        self.0 = Some(serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap());
        Ok(())
    }
}

struct WrapUpOwner {
    successor: provenant_records::WorkKey,
    complete: bool,
}
impl AttemptOwner for WrapUpOwner {
    type Error = Infallible;
    fn actor(&self) -> String {
        "chair".into()
    }
    fn continuation(
        &mut self,
        reference: &str,
    ) -> Result<Option<AttemptContinuation>, Self::Error> {
        Ok((reference == "host/original").then(|| AttemptContinuation {
            key: self.successor.clone(),
            attempt_ref: "host/wrap-up".into(),
        }))
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        node: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        _: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        assert_eq!(node.context.attempt, Some(key.attempt));
        let reference = if key.attempt == 1 {
            assert!(replay.stored_ref.is_none());
            "host/original"
        } else {
            assert_eq!(
                key.work_key(self.successor.project()).unwrap(),
                self.successor
            );
            assert_eq!(replay.stored_ref, Some("host/wrap-up"));
            if !self.complete {
                return Ok(AttemptObservation::Pending {
                    attempt_ref: replay.stored_ref.map(Into::into),
                });
            }
            "host/wrap-up"
        };
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(reference.into()),
            outcome: if key.attempt == 1 {
                AttemptOutcome::TaskFailed
            } else {
                AttemptOutcome::Succeeded
            },
            launched: true,
            result: None,
            result_warning: None,
            result_path: None,
            branch: None,
            commit: None,
            settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
            stop: None,
        })
    }
}

#[test]
fn continuation_handles_keep_their_admitted_work_identity_across_reload() {
    let project = "11111111111111111111111111111111".parse().unwrap();
    let original = provenant_records::WorkKey::new(project, "wrap", "writer", 1).unwrap();
    let mut owner = WrapUpOwner {
        successor: original.next_attempt().unwrap(),
        complete: false,
    };
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"wrap","nodes":[{
            "id":"writer","access":"read_only","prompt":"finish",
            "route":{"adapter":"fixture","model":"fixture/model"}
        }]
    }))
    .unwrap();
    let mut journal = Journal::default();
    let advance = |owner: &mut WrapUpOwner, journal: &mut Journal| {
        advance_with_budget(
            &document,
            owner,
            journal,
            &provenant_fabric_runtime::config::GraphLimits::default(),
            &provenant_fabric_runtime::config::GraphBudget::default(),
        )
        .unwrap();
    };
    advance(&mut owner, &mut journal);
    for complete in [false, true] {
        owner.complete = complete;
        advance(&mut owner, &mut journal);
        let checkpoint = journal.0.as_ref().unwrap();
        for (work, reference) in [
            (&original, "host/original"),
            (&owner.successor, "host/wrap-up"),
        ] {
            let target = Target::parse(&work.to_string()).unwrap();
            let resolved = resolve(&target, Want::Attempt { settled: false }, |_| {
                Ok::<_, Infallible>(Some(checkpoint.clone()))
            })
            .unwrap();
            assert_eq!(resolved.work_key(project).unwrap(), *work);
            assert_eq!(resolved.attempt_ref.as_deref(), Some(reference));
        }
        assert_eq!(
            checkpoint.nodes[0].previous_attempts[0].state,
            AttemptState::Terminal
        );
        assert_eq!(
            checkpoint.nodes[0].state,
            if complete {
                NodeState::Succeeded
            } else {
                NodeState::Running
            }
        );
    }
    // Resolution is based on identity even in a projection without the older row.
    let mut projected = journal.0.unwrap();
    projected.nodes[0].previous_attempts.clear();
    let target = Target::parse(&owner.successor.to_string()).unwrap();
    let resolved = resolve(&target, Want::Attempt { settled: true }, |_| {
        Ok::<_, Infallible>(Some(projected.clone()))
    })
    .unwrap();
    assert_eq!(resolved.work_key(project).unwrap(), owner.successor);
}
