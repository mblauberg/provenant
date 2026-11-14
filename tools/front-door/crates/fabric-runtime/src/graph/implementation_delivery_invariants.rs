//! Planning must not turn delivery links into links to a read-only container.
use super::*;
use serde_json::json;

#[test]
fn unplanned_delivery_links_resolve_to_its_exported_writer() {
    let library = TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([
        ("review-frame".into(), vec!["false".into()]),
        ("review-verify".into(), vec!["false".into()]),
        ("review-synthesis".into(), vec!["false".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
        ("review-panel".into(), vec!["correctness".into()]),
        ("review-advice-correctness".into(), vec![String::new()]),
    ]);
    let node = Compiler { library:&library, resolvers:&resolvers }.compile(&json!({
        "id":"delivery", "use":"deliver", "with":{"worktree":"/fixture/writer","write_scope":["src"]}
    })).unwrap();
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2,"graph_id":"planned-delivery","nodes":[node]
    }))
    .unwrap();
    let expanded = document
        .expanded(&crate::config::GraphLimits::default())
        .unwrap();
    let writer = expanded
        .nodes
        .iter()
        .find(|node| {
            node.access == AccessMode::WorktreeWrite
                && node.delivery == Some(provenant_records::run::DeliveryMode::Pr)
                && node.continues.is_none()
        })
        .unwrap();
    assert!(expanded.nodes.iter().any(|node| {
        node.extra
            .get(template::TEMPLATE_FIELD)
            .and_then(serde_json::Value::as_str)
            == Some("plan")
    }));
    let mut continuations = 0;
    let mut head_sources = 0;
    let boundary = expanded
        .nodes
        .iter()
        .find(|node| {
            node.composite.as_ref().is_some_and(|spec| {
                spec.exports.iter().any(|export| {
                    export.node == writer.id && export.outputs.contains(&OutputKind::Commit)
                })
            })
        })
        .unwrap();
    let mut node_sources = 0;
    for node in &expanded.nodes {
        if let Some(target) = &node.continues {
            assert_eq!(target, &writer.id);
            assert_eq!(node.worktree, writer.worktree);
            continuations += 1;
        }
        if let Some(SourceBinding::Head { node: target }) = &node.source {
            assert_eq!(target, &writer.id);
            head_sources += 1;
        }
        if let Some(SourceBinding::Node { node: target }) = &node.source
            && target == &writer.id
        {
            assert!(node.depends_on.contains(&writer.id));
            assert!(node.depends_on.contains(&boundary.id));
            node_sources += 1;
        }
        if let Some(land) = &node.land {
            assert_eq!(land.node, writer.id);
        }
        if let Some(GateCondition::Ci {
            target: CiTarget::Node { node: target, .. },
            ..
        }) = &node.condition
        {
            assert_eq!(target, &writer.id);
        }
        if let Some(plan) = &node.plan {
            for pattern in &plan.templates {
                if let Some(target) = pattern.node.get("continues") {
                    assert_eq!(target.as_str(), Some(writer.id.as_str()));
                }
            }
        }
    }
    assert!(continuations > 0 && head_sources > 0 && node_sources > 0);
}
