#[test]
fn queue_knobs_keep_shipped_defaults_and_resolve_config_and_authored_sources() {
    use super::*;
    use provenant_records::{Layer, Source, Sourced};

    fn user_origin() -> provenant_config::Origin {
        let path: provenant_records::PathWire = provenant_records::PathWire::from_relative(
            &provenant_core::RelativePath::parse("config.toml").unwrap(),
        )
        .unwrap();
        provenant_config::Origin::User { path }
    }

    fn sourced(value: usize, layer: Layer, origin: &str) -> Sourced<usize> {
        Sourced {
            value,
            source: Source {
                layer,
                origin: Some(origin.into()),
            },
        }
    }

    use provenant_fabric_runtime::graph::{
        QUEUE_CI_SLOTS_KEY, QUEUE_MAX_PARALLEL_KEY, QUEUE_PREP_RETRIES_KEY,
    };

    let defaults = seal_delivery_queue_knobs(&Default::default(), &BTreeMap::new(), None).unwrap();
    assert_eq!(defaults.max_parallel.value, 4);
    assert_eq!(defaults.max_parallel.source.layer, Layer::Default);
    assert_eq!(defaults.max_parallel.source.origin, None);
    assert_eq!(defaults.ci_slots.value, 1);
    assert_eq!(defaults.prep_retries.value, 2);

    let origins = BTreeMap::from([
        (QUEUE_MAX_PARALLEL_KEY.into(), user_origin()),
        (QUEUE_CI_SLOTS_KEY.into(), user_origin()),
        (QUEUE_PREP_RETRIES_KEY.into(), user_origin()),
    ]);
    let configured = seal_delivery_queue_knobs(
        &provenant_fabric_runtime::config::QueueConfig {
            max_parallel: 6,
            ci_slots: 0,
            prep_retries: 3,
        },
        &origins,
        None,
    )
    .unwrap();
    assert_eq!(configured.max_parallel.value, 6);
    assert_eq!(configured.max_parallel.source.layer, Layer::Host);
    assert_eq!(
        configured.max_parallel.source.origin.as_deref(),
        Some("config.toml#domains.fabric.queue.max_parallel")
    );
    assert_eq!(configured.ci_slots.value, 0);
    assert_eq!(configured.ci_slots.source.layer, Layer::Host);
    assert_eq!(configured.prep_retries.value, 3);
    assert_eq!(configured.prep_retries.source.layer, Layer::Host);

    let authored = QueueKnobs {
        max_parallel: sourced(9, Layer::Project, ".provenant/config.toml"),
        ci_slots: sourced(0, Layer::Launch, "--set queue.ci_slots=0"),
        prep_retries: sourced(2, Layer::Default, "compiled"),
    };
    let overridden = seal_delivery_queue_knobs(
        &provenant_fabric_runtime::config::QueueConfig {
            max_parallel: 6,
            ci_slots: 1,
            prep_retries: 3,
        },
        &origins,
        Some(&authored),
    )
    .unwrap();
    assert_eq!(overridden.max_parallel.value, 9);
    assert_eq!(overridden.max_parallel.source.layer, Layer::Project);
    assert_eq!(overridden.ci_slots.value, 0);
    assert_eq!(overridden.ci_slots.source.layer, Layer::Launch);
    assert_eq!(
        overridden.ci_slots.source.origin.as_deref(),
        Some("--set queue.ci_slots=0")
    );
    assert_eq!(overridden.prep_retries.value, 3);
    assert_eq!(overridden.prep_retries.source.layer, Layer::Host);
}

#[test]
fn legacy_deliver_without_queue_selection_keeps_its_graph_shape() {
    use super::*;

    fn document(graph_id: &str, queue: Option<QueueKnobs>) -> GraphDocument {
        let mut value = json!({"schema_version": 1, "graph_id": graph_id, "nodes": []});
        if let Some(queue) = queue {
            value["queue"] = json!(queue);
        }
        serde_json::from_value(value).unwrap()
    }

    let mut document = document("deliver-1", None);
    seal_document_queue(&mut document, None, None).unwrap();
    assert_eq!(document.queue, None);
    assert_eq!(document.schema_version, 1);
}

#[test]
fn explicit_queue_on_named_full_document_is_sealed_with_config_sources() {
    use super::*;
    use provenant_records::{Layer, Source, Sourced};

    fn user_origin() -> provenant_config::Origin {
        let path: provenant_records::PathWire = provenant_records::PathWire::from_relative(
            &provenant_core::RelativePath::parse("config.toml").unwrap(),
        )
        .unwrap();
        provenant_config::Origin::User { path }
    }

    fn sourced(value: usize, layer: Layer, origin: &str) -> Sourced<usize> {
        Sourced {
            value,
            source: Source {
                layer,
                origin: Some(origin.into()),
            },
        }
    }

    fn document(graph_id: &str, queue: Option<QueueKnobs>) -> GraphDocument {
        let mut value = json!({"schema_version": 1, "graph_id": graph_id, "nodes": []});
        if let Some(queue) = queue {
            value["queue"] = json!(queue);
        }
        serde_json::from_value(value).unwrap()
    }

    use provenant_fabric_runtime::graph::{
        QUEUE_CI_SLOTS_KEY, QUEUE_MAX_PARALLEL_KEY, QUEUE_PREP_RETRIES_KEY,
    };

    let authored = QueueKnobs {
        max_parallel: sourced(9, Layer::Project, ".provenant/config.toml"),
        ci_slots: sourced(0, Layer::Launch, "--set queue.ci_slots=0"),
        prep_retries: sourced(2, Layer::Default, "compiled"),
    };
    let mut document = document("named-queue", Some(authored));
    let config = provenant_fabric_runtime::config::QueueConfig {
        max_parallel: 6,
        ci_slots: 1,
        prep_retries: 3,
    };
    let origins = BTreeMap::from([
        (QUEUE_MAX_PARALLEL_KEY.into(), user_origin()),
        (QUEUE_CI_SLOTS_KEY.into(), user_origin()),
        (QUEUE_PREP_RETRIES_KEY.into(), user_origin()),
    ]);
    seal_document_queue(&mut document, Some((&config, &origins)), None).unwrap();

    let queue = document.queue.unwrap();
    assert_eq!(document.schema_version, 2);
    assert_eq!(queue.max_parallel.value, 9);
    assert_eq!(queue.max_parallel.source.layer, Layer::Project);
    assert_eq!(queue.ci_slots.value, 0);
    assert_eq!(queue.ci_slots.source.layer, Layer::Launch);
    assert_eq!(
        queue.ci_slots.source.origin.as_deref(),
        Some("--set queue.ci_slots=0")
    );
    assert_eq!(queue.prep_retries.value, 3);
    assert_eq!(queue.prep_retries.source.layer, Layer::Host);
}

#[test]
fn queue_delivery_binds_the_bundled_execution_seams() {
    let library = TemplateLibrary::bundled();
    let queue = json!({"queue": {}});
    for (raw, expected) in [(None, false), (Some(&queue), true)] {
        let mut parameters = json!({"delivery_execution_wired": false});
        bind_queue_delivery(raw, "deliver", &library, &mut parameters);
        let node = provenant_fabric_runtime::graph::template::Compiler::new(&library)
            .compile(&json!({"id":"delivery", "use":"deliver", "with":parameters}))
            .unwrap();
        let children = &node.composite.unwrap().nodes;
        for role in ["delivery-hygiene", "delivery-publish", "delivery-conflict"] {
            let seam = children
                .iter()
                .find(|node| node.role.as_deref() == Some(role))
                .unwrap();
            assert_eq!(
                provenant_fabric_runtime::graph::when::holds(&seam.when),
                expected
            );
        }
    }
}

