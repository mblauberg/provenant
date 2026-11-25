#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn operation(value: u8) -> OperationId {
        OperationId::parse(&format!("{value:02x}").repeat(16)).unwrap()
    }

    #[test]
    fn operation_fixture_preserves_all_bytes_and_distinguishes_operations() {
        assert_eq!(operation(1).to_string(), "01".repeat(16));
        assert_ne!(operation(1), operation(2));
    }

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn metadata(revision: u64, operation_id: OperationId) -> Metadata {
        Metadata {
            action: Action::Retry,
            expected_revision: revision,
            operation_id,
            review_node: None,
            review_head: None,
            graph_id: Some("queue-1".into()),
        }
    }

    #[test]
    fn pending_control_survives_checkpoint_restart_and_blocks_competitors() {
        let control = metadata(4, operation(1));
        let intent = Intent {
            digest: "digest".into(),
            metadata: control.clone(),
            graph_id: "queue-1".into(),
            target_node: Some("node".into()),
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::new(),
            request_id: None,
            payload: json!({"op":"retry"}),
            phase: Phase::Pending,
            answer_id: None,
            applied_revision: None,
        };
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": []
            }))
            .unwrap();
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(control.operation_id.to_string(), intent.clone());
        save_ledger(&mut checkpoint, &saved);

        // Model a process restart by round-tripping the journal checkpoint.
        let persisted = serde_json::to_value(&checkpoint).unwrap();
        let restored: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(persisted).unwrap();
        let restored_ledger = ledger(&restored);
        assert_eq!(
            restored_ledger
                .intents
                .get(&control.operation_id.to_string()),
            Some(&intent)
        );
        assert!(competes_with(&restored_ledger, &metadata(5, operation(2))));

        let mut completed = restored_ledger.clone();
        completed
            .intents
            .get_mut(&control.operation_id.to_string())
            .unwrap()
            .phase = Phase::Applied;
        assert!(competes_with(&completed, &metadata(4, operation(2))));
        assert!(!competes_with(&completed, &metadata(5, operation(2))));

        let mut dropping = restored_ledger;
        dropping
            .intents
            .get_mut(&control.operation_id.to_string())
            .unwrap()
            .phase = Phase::DropAdmitted;
        let persisted = serde_json::to_value(&dropping).unwrap();
        let restored: Ledger = serde_json::from_value(persisted).unwrap();
        assert!(!Phase::DropAdmitted.terminal());
        assert!(competes_with(&restored, &metadata(5, operation(2))));
    }

    #[test]
    fn malformed_control_ledger_is_not_treated_as_absent() {
        let checkpoint = json!({
            "schema_version": 2,
            "graph_id": "queue-1",
            "plan_digest": "digest",
            "nodes": [],
            "delivery_queue_controls": {"intents": "invalid"}
        });
        assert!(
            serde_json::from_value::<provenant_fabric_runtime::graph::GraphCheckpoint>(checkpoint)
                .is_err()
        );
    }

    #[test]
    fn operation_replay_digest_binds_target_and_payload() {
        let control = metadata(4, operation(1));
        let payload = json!({"op":"retry"});
        let first = control_digest(&control, "queue-1", Some("node"), None, &payload).unwrap();
        let replay = control_digest(&control, "queue-1", Some("node"), None, &payload).unwrap();
        let changed =
            control_digest(&control, "queue-1", Some("other-node"), None, &payload).unwrap();
        assert_eq!(first, replay);
        assert_ne!(first, changed);
    }

    #[test]
    fn accept_gate_retry_is_bound_to_saved_answer_and_failed_gate_generation() {
        use provenant_core::GitOid;
        let operation_id = operation(8);
        let request_id = HumanRequestId::from_bytes([1; 16]);
        let answer_id = HumanAnswerId::from_bytes([2; 16]);
        let head = GitOid::parse(&"a".repeat(40)).unwrap();
        let reason = "accept this exact reviewed head";
        let control = Metadata {
            action: Action::Accept,
            expected_revision: 4,
            operation_id: operation_id.clone(),
            review_node: Some("review".into()),
            review_head: Some(head),
            graph_id: Some("queue-1".into()),
        };
        let payload = serde_json::to_value(InboxInput::Answer {
            queue_control: None,
            extra: Default::default(),
            id: request_id,
            action: AnswerAction::Reply,
            option_id: None,
            text: Some(reason.into()),
            expect_revision: 4,
            channel: provenant_records::inbox::InteractionChannel::Cli,
            operation_id: Some(operation_id.clone()),
        })
        .unwrap();
        let intent = Intent {
            digest: "saved-accept-intent".into(),
            metadata: control,
            graph_id: "queue-1".into(),
            target_node: Some("review".into()),
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::from([("gate".into(), 2)]),
            request_id: Some(request_id),
            payload,
            phase: Phase::DispositionAdmitted,
            answer_id: Some(answer_id),
            applied_revision: Some(5),
        };
        let disposition = DeliveryReviewDisposition {
            operation_id: operation_id.clone(),
            request_id,
            answer_id,
            head,
            reason: reason.into(),
        };
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": [
                    {
                        "id":"review", "state":"failed", "worktree":null, "attempts":[],
                        "delivery_review_disposition": disposition
                    },
                    {
                        "id":"gate", "state":"failed", "worktree":null, "attempts":[],
                        "retry_generation":2
                    }
                ]
            }))
            .unwrap();
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(operation_id.to_string(), intent.clone());
        save_ledger(&mut checkpoint, &saved);

        let marker = json!({
            "operation_id":operation_id,
            "gate":"gate",
            "baseline_generation":2
        });
        let (key, digest) =
            authorise_accept_gate_retry(&checkpoint, "queue-1", "gate", &marker).unwrap();
        assert!(key.starts_with("accept-gate-"));
        assert!(digest.starts_with("sha256:"));
        assert!(authorise_accept_gate_retry(&checkpoint, "queue-1", "other", &marker).is_err());
        assert!(authorise_accept_gate_retry(&checkpoint, "other-graph", "gate", &marker).is_err());

        let mut stale = marker.clone();
        stale["baseline_generation"] = json!(3);
        assert!(authorise_accept_gate_retry(&checkpoint, "queue-1", "gate", &stale).is_err());

        let mut dropping = saved;
        let mut drop_intent = intent;
        drop_intent.metadata.action = Action::Drop;
        drop_intent.phase = Phase::DropAdmitted;
        dropping.intents.insert("drop-op".into(), drop_intent);
        let mut fenced = checkpoint;
        save_ledger(&mut fenced, &dropping);
        assert!(authorise_accept_gate_retry(&fenced, "queue-1", "gate", &marker).is_err());
    }

    #[test]
    fn only_pending_controls_that_can_wake_a_writer_need_a_parallel_slot() {
        let retry = metadata(4, operation(4));
        let retry = Intent {
            digest: "retry".into(),
            metadata: retry,
            graph_id: "queue-1".into(),
            target_node: Some("node".into()),
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::new(),
            request_id: None,
            payload: json!({"op":"retry"}),
            phase: Phase::Pending,
            answer_id: None,
            applied_revision: None,
        };
        let mut drop_intent = retry.clone();
        drop_intent.metadata.action = Action::Drop;
        drop_intent.phase = Phase::DropAdmitted;
        let mut accept = retry.clone();
        accept.metadata.action = Action::Accept;
        accept.phase = Phase::Answered;
        let mut applied = retry.clone();
        applied.phase = Phase::Applied;

        assert!(intent_requires_parallel_slot(&retry));
        assert!(intent_requires_parallel_slot(&accept));
        assert!(!intent_requires_parallel_slot(&drop_intent));
        assert!(!intent_requires_parallel_slot(&applied));

        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": []
            }))
            .unwrap();
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(retry.metadata.operation_id.to_string(), drop_intent);
        save_ledger(&mut checkpoint, &saved);
        assert!(checkpoint_has_pending_control(&checkpoint).unwrap());
        assert!(!checkpoint_has_pending_parallel_control(&checkpoint).unwrap());
    }

    #[test]
    fn admitted_drop_fences_retry_and_edit_after_restart() {
        let mut checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(json!({
                "schema_version": 2,
                "graph_id": "queue-1",
                "plan_digest": "digest",
                "nodes": []
            }))
            .unwrap();
        let mut drop_metadata = metadata(4, operation(3));
        drop_metadata.action = Action::Drop;
        let drop_intent = Intent {
            digest: "drop-digest".into(),
            metadata: drop_metadata.clone(),
            graph_id: "queue-1".into(),
            target_node: None,
            retry_generations: BTreeMap::new(),
            gate_generations: BTreeMap::new(),
            request_id: None,
            payload: json!({"op":"stop","id":"queue-1"}),
            phase: Phase::DropAdmitted,
            answer_id: None,
            applied_revision: None,
        };
        let mut saved = Ledger::default();
        saved
            .intents
            .insert(drop_metadata.operation_id.to_string(), drop_intent);
        save_ledger(&mut checkpoint, &saved);

        let persisted = serde_json::to_value(&checkpoint).unwrap();
        let restored: provenant_fabric_runtime::graph::GraphCheckpoint =
            serde_json::from_value(persisted).unwrap();
        assert!(drop_fence_active(&restored).unwrap());

        let mut refused = ledger(&restored);
        refused
            .intents
            .get_mut(&drop_metadata.operation_id.to_string())
            .unwrap()
            .phase = Phase::Refused;
        save_ledger(&mut checkpoint, &refused);
        assert!(!drop_fence_active(&checkpoint).unwrap());
    }
}
