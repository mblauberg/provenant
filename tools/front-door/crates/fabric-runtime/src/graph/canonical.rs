//! One spelling of every route model, decided once at graph admission.
//! Spec: docs/specs/fabric.md#one-model-spelling-per-route
use super::{GraphDocument, GraphNode, GraphRoute};

/// Canonicalise every route model in a document in place and return warnings
/// for uncatalogued ids. The shared admission compiler calls this so submit,
/// patch admission and tier lowering all see one full `ADAPTER/ID` spelling.
pub fn canonicalise_route_models(
    document: &mut GraphDocument,
    catalogue: Option<&provenant_routing::Catalogue>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    canonicalise_route_nodes(&mut document.nodes, catalogue, &mut warnings);
    warnings
}

/// Canonicalise a node slice, used by patch admission for the nodes it stores
/// in an expansion as well as by the document-level admission compiler.
pub fn canonicalise_route_nodes(
    nodes: &mut [GraphNode],
    catalogue: Option<&provenant_routing::Catalogue>,
    warnings: &mut Vec<String>,
) {
    walk(nodes, catalogue, warnings);
}

fn walk(
    nodes: &mut [GraphNode],
    catalogue: Option<&provenant_routing::Catalogue>,
    warnings: &mut Vec<String>,
) {
    for node in nodes {
        for route in node
            .route
            .iter_mut()
            .chain(node.routes.iter_mut().flatten())
        {
            if let Some(warning) = route_model(route, catalogue) {
                warnings.push(warning);
            }
        }
        if let Some(composite) = node.composite.as_mut() {
            walk(&mut composite.nodes, catalogue, warnings);
        }
        if let Some(loop_body) = node.loop_body.as_mut() {
            walk(&mut loop_body.body, catalogue, warnings);
        }
        // A planner's fallback stage lives on the plan spec and is cloned into
        // an expansion later, so it must carry the same spelling from admission.
        if let Some(plan) = node.plan.as_mut()
            && let Some(fallback) = plan.fallback.as_mut()
        {
            walk(&mut fallback.nodes, catalogue, warnings);
        }
    }
}

fn route_model(
    route: &mut GraphRoute,
    catalogue: Option<&provenant_routing::Catalogue>,
) -> Option<String> {
    let (Some(adapter), Some(model)) = (route.adapter.as_deref(), route.model.as_deref()) else {
        return None;
    };
    let (canonical, warning) = provenant_routing::canonical_route_model(adapter, model, catalogue);
    if route.model.as_deref() != Some(canonical.as_str()) {
        route.model = Some(canonical);
    }
    warning
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalogue() -> provenant_routing::Catalogue {
        provenant_routing::Catalogue::bundled().expect("bundled catalogue")
    }

    // Spec: docs/specs/fabric.md#one-model-spelling-per-route
    #[test]
    fn plan_fallback_routes_are_canonicalised() {
        let mut document: GraphDocument = serde_json::from_value(json!({
            "schema_version": 1,
            "graph_id": "fallback-model",
            "nodes": [{
                "id": "planner",
                "access": "read_only",
                "prompt": "plan",
                "plan": {
                    "max_nodes": 1,
                    "fallback": {
                        "nodes": [{
                            "id": "fallback-node",
                            "access": "read_only",
                            "prompt": "fallback",
                            "route": {"adapter":"opencode","model":"opencode/hy4-flash-free"}
                        }],
                        "exports": []
                    }
                }
            }]
        }))
        .expect("valid document with a plan fallback");
        let warnings = canonicalise_route_models(&mut document, Some(&catalogue()));
        assert!(warnings.is_empty(), "{warnings:?}");
        let fallback = document.nodes[0]
            .plan
            .as_ref()
            .and_then(|plan| plan.fallback.as_ref())
            .expect("fallback stage");
        assert_eq!(
            fallback.nodes[0]
                .route
                .as_ref()
                .and_then(|route| route.model.as_deref()),
            Some("opencode/opencode/hy4-flash-free")
        );
    }

    // Spec: docs/specs/fabric.md#one-model-spelling-per-route
    #[test]
    fn patch_admitted_relative_model_becomes_the_full_id() {
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version": 1,
            "graph_id": "patch-model",
            "nodes": [{
                "id": "anchor",
                "access": "read_only",
                "prompt": "anchor",
                "route": {"alias": "senior"}
            }]
        }))
        .expect("valid initial graph");
        let limits = crate::config::GraphLimits::default();
        document.admit(&limits).expect("initial graph is valid");
        let expanded = super::super::plans::expand(&document, &[], &[], &limits)
            .expect("initial graph expands");
        let (plan, _) =
            super::super::validate_flat_plan(&expanded, &limits).expect("initial graph validates");
        let digest = super::super::journal::plan_digest(&plan)
            .expect("plan digest")
            .to_string();
        let mut checkpoint = super::super::transitions::initial_checkpoint(
            &document, &plan, digest, None, None, None,
        );
        let library = super::super::template::TemplateLibrary::bundled();
        let resolvers = std::collections::BTreeMap::new();
        let patch = super::super::patch::Patch {
            schema_version: super::super::patch::PATCH_SCHEMA_VERSION,
            expected_revision: Some(0),
            nodes: vec![json!({
                "id": "added",
                "access": "read_only",
                "prompt": "added",
                "route": {"adapter":"opencode","model":"opencode/hy4-flash-free"}
            })],
            ..Default::default()
        };
        let context = super::super::patch::PatchContext {
            library: &library,
            resolvers: &resolvers,
            journal: None,
            limits: &limits,
            worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
        };
        super::super::patch::admit_patch(
            &mut super::super::patch::LiveGraph {
                document: &document,
                checkpoint: &mut checkpoint,
            },
            None,
            &patch,
            &context,
        )
        .expect("patch admits");
        let added = checkpoint
            .expansions
            .last()
            .and_then(|expansion| expansion.nodes.iter().find(|node| node.id == "added"))
            .expect("added node");
        assert_eq!(
            added
                .route
                .as_ref()
                .and_then(|route| route.model.as_deref()),
            Some("opencode/opencode/hy4-flash-free")
        );
    }
}
