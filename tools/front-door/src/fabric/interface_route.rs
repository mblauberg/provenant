//! `--route` tier lowering for graph nodes. Kept beside interface.rs so the
//! route helpers grow without pushing that production file past its line
//! ceiling. Spec: docs/specs/fabric.md#one-model-spelling-per-route
use super::RouteChoice;
use serde_json::{Value, json};

/// The role a launched node plays, which `--route role=tier` addresses.
pub(super) fn route_role(node: &provenant_fabric_runtime::graph::GraphNode) -> &'static str {
    use provenant_fabric_runtime::graph::{AccessMode, NodeKind};
    if node.review_behaviour.is_some() || provenant_fabric_runtime::graph::is_review_node(node) {
        "review"
    } else if node.plan.is_some() {
        "plan"
    } else if node.kind == Some(NodeKind::Synthesise) {
        "synthesis"
    } else if node.access == AccessMode::WorktreeWrite {
        "writer"
    } else {
        "task"
    }
}

/// Lower a `--route` tier to the node route it selects. A full or short
/// `ADAPTER/ID` becomes an adapter and model route; a pool name stays an alias.
/// The host entry point canonicalises the model spelling once, so this keeps the
/// declared id verbatim.
fn route_for_tier(tier: &str) -> Value {
    match crate::graph_support::tier_route("route", &json!(tier), None, false, None) {
        Ok((_, route, _)) => route,
        // A malformed value stays a tier so graph admission names its node and
        // validator reason instead of failing this early without that context.
        Err(_) => json!({"alias": tier}),
    }
}

pub(super) fn override_tier(
    node: &mut provenant_fabric_runtime::graph::GraphNode,
    choice: &RouteChoice,
) {
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            override_tier(child, choice);
        }
    } else if let Some(body) = &mut node.loop_body {
        for child in &mut body.body {
            override_tier(child, choice);
        }
    } else if !matches!(
        node.kind,
        Some(
            provenant_fabric_runtime::graph::NodeKind::Gate
                | provenant_fabric_runtime::graph::NodeKind::Decide
                | provenant_fabric_runtime::graph::NodeKind::Land
                | provenant_fabric_runtime::graph::NodeKind::Hygiene
                | provenant_fabric_runtime::graph::NodeKind::Publish
        )
    ) && let Some(alias) = choice.tier_for(route_role(node))
    {
        let Ok(route) = serde_json::from_value::<provenant_fabric_runtime::graph::GraphRoute>(
            route_for_tier(alias),
        ) else {
            return;
        };
        let constraints: Vec<_> = node
            .route
            .iter()
            .chain(node.routes.iter().flatten())
            .map(|existing| existing.required_family.clone())
            .collect();
        let replace = |family: Option<&String>| {
            let mut replacement = route.clone();
            replacement.required_family = family.cloned();
            replacement
        };
        if constraints.windows(2).any(|pair| pair[0] != pair[1]) {
            // Keep different requirements on their original alternatives.
            node.routes = Some(
                constraints
                    .iter()
                    .map(|family| replace(family.as_ref()))
                    .collect(),
            );
            node.route = None;
        } else {
            node.route = Some(replace(constraints.first().and_then(Option::as_ref)));
            node.routes = None;
        }
    }
    if let Some(fallback) = node.plan.as_mut().and_then(|plan| plan.fallback.as_mut()) {
        for child in &mut fallback.nodes {
            override_tier(child, choice);
        }
    }
}
