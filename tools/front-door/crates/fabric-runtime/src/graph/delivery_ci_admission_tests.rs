use super::*;

fn wait(clock_missing: bool) -> DeliveryCiAdmissionWait {
    DeliveryCiAdmissionWait {
        started_at: provenant_core::WallTime::from_micros(10_000_000),
        deadline: provenant_core::WallTime::from_micros(65_000_000),
        wake_at: provenant_core::WallTime::from_micros(11_000_000),
        policy: provenant_records::Sourced {
            value: 55,
            source: provenant_records::Source {
                layer: provenant_records::Layer::Default,
                origin: None,
            },
        },
        clock_missing,
    }
}

#[test]
fn unavailable_wall_clock_never_proves_ci_admission_wait_elapsed() {
    assert!(!delivery_ci_wait_expired(
        &wait(true),
        Some(provenant_core::WallTime::from_micros(i64::MAX))
    ));
    assert!(!delivery_ci_wait_expired(&wait(false), None));
    assert!(delivery_ci_wait_ready(&wait(false), None).is_none());
    assert!(
        delivery_ci_wait_ready(
            &wait(false),
            Some(provenant_core::WallTime::from_micros(10_999_999))
        )
        .is_none()
    );
    assert_eq!(
        delivery_ci_wait_ready(
            &wait(false),
            Some(provenant_core::WallTime::from_micros(11_000_000))
        ),
        Some(provenant_core::WallTime::from_micros(11_000_000))
    );
    assert!(delivery_ci_wait_expired(
        &wait(false),
        Some(provenant_core::WallTime::from_micros(65_000_000))
    ));
}

#[test]
fn exact_ci_supersession_is_durable_and_idempotent() {
    let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "queue-graph",
        "plan_digest": "sha256:test",
        "nodes": [],
    }))
    .unwrap();
    let head = "a".repeat(64);

    checkpoint
        .record_delivery_ci_supersession("publish", &head)
        .unwrap();
    checkpoint
        .record_delivery_ci_supersession("publish", &head)
        .unwrap();

    let restored: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
    assert_eq!(restored.delivery_ci_superseded.len(), 1);
    assert_eq!(restored.delivery_ci_superseded[0].node_id, "publish");
    assert_eq!(restored.delivery_ci_superseded[0].head_sha, head);
}

#[test]
fn adopted_publication_is_the_expected_head_for_a_republish() {
    let head = "a".repeat(40);
    let operation = format!("sha256:{}", "b".repeat(64));
    let mut document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":2, "graph_id":"adopted", "integration_branch":"topic",
        "nodes":[{"id":"adopted-publish", "kind":"publish", "access":"worktree_write",
            "worktree":"/repo/topic", "delivery_adoption":{
                "operation_id":operation, "project":"project", "tracker":"github", "item":"issue",
                "repository":"owner/repo", "issue_identity":"issue", "issue_number":7,
                "pull_request":8, "pull_request_url":"https://example.test/pr/8",
                "branch":"topic", "worktree":"/repo/topic", "source_directory_identity":{"device":1,"inode":2},
                "custody_host":"host", "custody_attempt":"attempt", "custody_operation":"custody",
                "head":head, "base_repository":"owner/repo", "base_branch":"main", "base_head":"c".repeat(40)
            }}]
    })).unwrap();
    document.queue = Some(QueueKnobs::default());
    let (plan, _) = validate_flat_plan(&document, &Default::default()).unwrap();
    let mut checkpoint =
        transitions::initial_checkpoint(&document, &plan, "digest".into(), None, None, None);
    checkpoint.nodes[0].state = NodeState::Succeeded;
    checkpoint.nodes[0].set_delivery_adoption(&DeliveryAdoptionProgress {
        operation_id: operation,
        ci_admission: Some(DeliveryCiAdmission::Unavailable {
            reason: "offline".into(),
        }),
        ci_admission_wait: None,
        receipt: Some(DeliveryPublication {
            branch: "topic".into(),
            head: head.clone(),
            pull_request: "https://example.test/pr/8".into(),
        }),
        error: None,
    });
    assert_eq!(
        prior_publication_head(&plan.nodes[0], &checkpoint.nodes[0], "topic"),
        Some(head)
    );
    assert_eq!(
        prior_publication_head(&plan.nodes[0], &checkpoint.nodes[0], "another"),
        None
    );
    checkpoint.nodes[0].state = NodeState::Pending;
    assert_eq!(
        prior_publication_head(&plan.nodes[0], &checkpoint.nodes[0], "topic"),
        None
    );
}

#[test]
fn conflict_gate_exposes_bounded_exact_head_evidence_for_repair() {
    let head = "d".repeat(40);
    let evidence = ci::ConflictEvidence {
        state: ci::ConflictState::Conflicting,
        head_sha: Some(head.clone()),
        summary: "conflict at the base branch".into(),
        wake_at: None,
        pending_observations: 0,
        ci_rechecks: 0,
    };
    let outputs = ci::conflict_outputs(false, Some(&evidence));
    assert_eq!(outputs.commit.as_deref(), Some(head.as_str()));
    let text = &outputs.result.as_ref().unwrap().excerpt;
    assert!(text.contains("Conflicting"));
    assert!(text.contains(&head));
    assert!(text.contains("untrusted") || text.contains("Untrusted"));
}
