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
