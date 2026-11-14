use super::*;

pub(super) fn nodes_have_v2(nodes: &[GraphNode]) -> bool {
    nodes.iter().any(|node| {
        node.extra.contains_key("review_base")
            || node.family_group.is_some()
            || node
                .inputs
                .iter()
                .any(|input| !input.required_outputs.is_empty())
            || node.composite.as_ref().is_some_and(|spec| {
                spec.exports
                    .iter()
                    .any(|input| !input.required_outputs.is_empty())
            })
            || node.plan.as_ref().is_some_and(|spec| {
                spec.templates.iter().any(|pattern| {
                    pattern.nodes.iter().any(value_has_v2)
                        || pattern
                            .exports
                            .iter()
                            .any(|input| !input.required_outputs.is_empty())
                }) || spec.fallback.as_ref().is_some_and(|fallback| {
                    fallback
                        .exports
                        .iter()
                        .any(|input| !input.required_outputs.is_empty())
                })
            })
            || node
                .composite
                .as_ref()
                .is_some_and(|spec| nodes_have_v2(&spec.nodes))
            || node
                .loop_body
                .as_ref()
                .is_some_and(|spec| nodes_have_v2(&spec.body))
            || node
                .plan
                .as_ref()
                .and_then(|spec| spec.fallback.as_ref())
                .is_some_and(|spec| nodes_have_v2(&spec.nodes))
    })
}

fn value_has_v2(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key("family_group")
                || map.contains_key("review_base")
                || map
                    .get("required_outputs")
                    .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty()))
                || map.values().any(value_has_v2)
        }
        serde_json::Value::Array(values) => values.iter().any(value_has_v2),
        _ => false,
    }
}

/// Minimum document schema capable of representing the document's fields.
/// Schema 2 is a superset, so callers may retain an authored version 2 even
/// when the minimum is 1.
pub fn minimum_schema_version(document: &GraphDocument) -> u32 {
    if document_requires_review_fields(document) {
        2
    } else {
        1
    }
}

pub(super) fn document_requires_review_fields(document: &GraphDocument) -> bool {
    document.review_base.is_some() || nodes_have_v2(&document.nodes)
}

pub(super) fn supplied_kinds(node: &GraphNode) -> Vec<OutputKind> {
    if let Some(composite) = &node.composite {
        let mut kinds = composite
            .exports
            .iter()
            .flat_map(|input| input.outputs.iter().copied())
            .fold(Vec::new(), |mut kinds, kind| {
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
                kinds
            });
        if composite.reduce_verdict && !kinds.contains(&OutputKind::Verdict) {
            kinds.push(OutputKind::Verdict);
        }
        return kinds;
    }
    let mut kinds = vec![
        OutputKind::Result,
        OutputKind::Findings,
        OutputKind::Verdict,
        OutputKind::Remaining,
    ];
    if node.kind == Some(NodeKind::Loop) {
        return kinds;
    }
    let write = node.access == AccessMode::WorktreeWrite;
    if write || node.kind == Some(NodeKind::Land) {
        kinds.push(OutputKind::Branch);
    }
    let pinned = node.source.is_some();
    if write || pinned || node.kind == Some(NodeKind::Land) {
        kinds.push(OutputKind::Commit);
    }
    kinds
}

pub(super) fn validate_output_kinds(document: &GraphDocument) -> Result<(), AdmissionError> {
    let by_id: std::collections::BTreeMap<_, _> =
        document.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for consumer in &document.nodes {
        if consumer.plan.as_ref().is_some_and(|plan| {
            plan.templates.iter().any(|pattern| {
                pattern.exports.iter().any(|input| {
                    !input.required_outputs.is_empty() || input.extra.contains_key("required")
                })
            }) || plan.fallback.as_ref().is_some_and(|fallback| {
                fallback.exports.iter().any(|input| {
                    !input.required_outputs.is_empty() || input.extra.contains_key("required")
                })
            })
        }) {
            return Err(AdmissionError::OutputKind {
                field: GraphOutputField::CompositeExport,
                reason: format!(
                    "node {} declares requirements on planner exports, which are not supported",
                    consumer.id
                ),
            });
        }
        if consumer.composite.as_ref().is_some_and(|composite| {
            composite
                .exports
                .iter()
                .any(|input| !input.required_outputs.is_empty())
        }) {
            return Err(AdmissionError::OutputKind {
                field: GraphOutputField::CompositeExport,
                reason: format!(
                    "node {} declares requirements on composite exports, which are not supported",
                    consumer.id
                ),
            });
        }
        // A composite's export declaration is its public output contract. Its
        // members are validated in their own scopes; consumers are checked
        // against the declared composite outputs through `supplied_kinds`.
        for (input, field) in consumer
            .inputs
            .iter()
            .map(|input| (input, GraphOutputField::Input))
            .chain(
                consumer
                    .composite
                    .iter()
                    .flat_map(|composite| &composite.exports)
                    .map(|input| (input, GraphOutputField::CompositeExport)),
            )
        {
            if input.extra.contains_key("required") {
                return Err(AdmissionError::OutputKind {
                    field,
                    reason: format!(
                        "node {} declares required output semantics that this runtime cannot enforce",
                        consumer.id
                    ),
                });
            }
            let source_id = input.node.strip_prefix("previous.").unwrap_or(&input.node);
            let Some(source) = by_id.get(source_id) else {
                continue;
            };
            if !input.required_outputs.is_empty()
                && (source.composite.is_some()
                    && input
                        .required_outputs
                        .iter()
                        .any(|kind| *kind != OutputKind::Findings)
                    || source.loop_body.is_some()
                    || source.plan.is_some()
                    || input.node.starts_with("previous."))
            {
                return Err(AdmissionError::OutputKind {
                    field,
                    reason: format!(
                        "node {} requires outputs from {}, whose expansion cannot yet be verified",
                        consumer.id, input.node
                    ),
                });
            }
            let available = supplied_kinds(source);
            for kind in &input.outputs {
                if !available.contains(kind) {
                    let labels = available
                        .iter()
                        .map(|v| format!("{v:?}").to_lowercase())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(AdmissionError::OutputKind {
                        field,
                        reason: format!(
                            "node {} selects {} from {}, which supplies only {}",
                            consumer.id,
                            format!("{kind:?}").to_lowercase(),
                            source_id,
                            labels
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_schema(document: &GraphDocument) -> Result<(), AdmissionError> {
    let has_v2 = document.review_base.is_some() || nodes_have_v2(&document.nodes);
    // Version 2 is a superset: it may omit every v2 field, but version 1
    // cannot carry one.
    if !matches!(document.schema_version, 1 | 2) || document.schema_version == 1 && has_v2 {
        return Err(AdmissionError::Schema("schema version".into()));
    }
    Ok(())
}

pub(crate) fn validate_access(
    access: AccessMode,
    worktree: Option<&str>,
) -> Result<(), &'static str> {
    if access == AccessMode::WorktreeWrite
        && worktree.is_none_or(|path| {
            path.is_empty()
                || path.len() > 4096
                || path.contains('\0')
                || std::path::Path::new(path)
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
        })
    {
        return Err("writer worktree");
    }
    if access == AccessMode::ReadOnly && worktree.is_some() {
        return Err("read-only worktree");
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn validate_plan(
    document: &GraphDocument,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), &'static str> {
    // Test-only: leak the rare formatted reason so assertions keep comparing &str.
    validate_plan_with_limits(document, &crate::config::GraphLimits::default())
        .map_err(|reason| &*Box::leak(reason.to_string().into_boxed_str()))
}

pub(super) fn validate_plan_with_limits(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), AdmissionError> {
    document.admit(limits)?;
    validate_replay_plan(document, limits)
}

pub(super) fn validate_replay_plan(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), AdmissionError> {
    validate_schema(document)?;
    let expansion_limits = super::model::expansion_safety_limits(limits);
    let expanded = loops::expand(document, &[], &expansion_limits)?;
    let plan = validate_flat_plan(&expanded, &expansion_limits)?;
    Ok(plan)
}

fn plan_error(reason: &str) -> AdmissionError {
    AdmissionError::Plan(reason.into())
}

fn node_plan_error(node: &GraphNode, reason: impl std::fmt::Display) -> AdmissionError {
    AdmissionError::Plan(format!("node {}: {reason}", node.id))
}

pub(super) fn validate_flat_plan(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), AdmissionError> {
    if !matches!(document.schema_version, 1 | 2) {
        return Err(AdmissionError::Plan("schema version".into()));
    }
    let pruned = when::prune(document)?;
    let document = pruned.as_ref().unwrap_or(document);
    if document
        .context
        .as_ref()
        .is_some_and(|context| !context.valid())
        || document.nodes.iter().any(|node| {
            node.context
                .as_ref()
                .is_some_and(|context| !context.valid())
        })
    {
        return Err(AdmissionError::Plan("invalid work context".into()));
    }
    use provenant_records::run::DeliveryMode;
    if !matches!(
        document.delivery,
        None | Some(DeliveryMode::None) | Some(DeliveryMode::Pr)
    ) || document.delivery == Some(DeliveryMode::Pr) && document.integration_branch.is_none()
        || document.integration_branch.as_ref().is_some_and(|branch| {
            format!("integrate:{branch}")
                .parse::<DeliveryMode>()
                .is_err()
        })
    {
        return Err(AdmissionError::Plan(
            "graph delivery requires an integration branch and at most one PR".into(),
        ));
    }
    if !valid_identifier(&document.graph_id) {
        return Err(AdmissionError::Plan("graph id".into()));
    }
    if document
        .decision_session
        .as_ref()
        .is_some_and(|session| crate::inbox::session_id(session).is_err())
    {
        return Err(AdmissionError::Plan("invalid decision_session".into()));
    }
    if document.nodes.is_empty() || document.nodes.len() > limits.nodes {
        return Err(AdmissionError::Plan("node count".into()));
    }

    let mut positions = std::collections::BTreeMap::<String, usize>::new();
    for (index, node) in document.nodes.iter().enumerate() {
        if !valid_node_identifier(&node.id) || positions.insert(node.id.clone(), index).is_some() {
            return Err(node_plan_error(node, "node id"));
        }
    }

    let mut edge_count = 0usize;
    let mut prompt_bytes = 0usize;
    let mut nodes = Vec::with_capacity(document.nodes.len());
    for node in &document.nodes {
        if node.extra.get("review_base").is_some_and(|value| {
            value.as_str().is_none_or(|reference| {
                reference.is_empty() || reference.len() > 4096 || reference.contains('\0')
            })
        }) {
            return Err(node_plan_error(
                node,
                "node review_base must be a nonempty reference",
            ));
        }
        if node.extra.contains_key("composite_members")
            || node.extra.contains_key("after_expansion")
        {
            return Err(node_plan_error(
                node,
                "computed topology fields are reserved to checkpoint expansions",
            ));
        }
        if node.deadline_seconds == Some(0) {
            return Err(node_plan_error(
                node,
                "node deadline_seconds must be positive",
            ));
        }
        if let Some(reason) = node
            .attempt
            .as_ref()
            .and_then(|spec| attempt_error(node, spec))
        {
            return Err(node_plan_error(node, reason));
        }
        plans::validate_spec(node).map_err(|reason| node_plan_error(node, reason))?;
        let is_composite = node.kind == Some(NodeKind::Composite);
        if is_composite != node.composite.is_some() {
            return Err(node_plan_error(
                node,
                "composite requires nodes and exports",
            ));
        }
        let is_loop = node.kind == Some(NodeKind::Loop);
        if is_loop != node.loop_body.is_some() {
            return Err(node_plan_error(node, "loop requires a body"));
        }
        let is_decide = node.kind == Some(NodeKind::Decide);
        if is_decide != node.decide.is_some() {
            return Err(node_plan_error(
                node,
                "decide requires a question and options",
            ));
        }
        asks::validate_decide(
            &document.graph_id,
            node,
            document.decision_session.as_deref(),
        )
        .map_err(|reason| node_plan_error(node, reason))?;
        let is_land = node.kind == Some(NodeKind::Land);
        if is_land != node.land.is_some() {
            return Err(node_plan_error(
                node,
                "land requires a land spec naming its writer",
            ));
        }
        land::validate_node(node).map_err(|reason| node_plan_error(node, reason))?;
        let is_gate = node.kind == Some(NodeKind::Gate);
        if is_gate != node.condition.is_some()
            || !is_gate && node.loop_body.is_none() && node.on_fail.is_some()
            || is_gate
                && (node.route.is_some()
                    || node.routes.is_some()
                    || node.access != AccessMode::ReadOnly
                    || node.delivery.is_some()
                    || node.worktree.is_some()
                    || !node.prompt.is_empty()
                    || node.required_verdict.is_some())
        {
            return Err(node_plan_error(
                node,
                "gate requires a condition and no agent configuration",
            ));
        }
        if let Some(target) = &node.on_fail
            && (target == &node.id
                || !positions
                    .get(target)
                    .is_some_and(|index| document.nodes[*index].depends_on.contains(&node.id)))
        {
            return Err(node_plan_error(
                node,
                "on_fail target must depend on its gate",
            ));
        }
        match &node.condition {
            Some(GateCondition::Verdict {
                expected,
                threshold,
                ..
            }) => {
                if expected.trim().is_empty()
                    || expected.trim() != expected
                    || expected.len() > limits.node_prompt_bytes
                    || expected.contains(['\n', '\r'])
                    || node.inputs.is_empty()
                    || node
                        .inputs
                        .iter()
                        .any(|input| input.outputs != [OutputKind::Verdict])
                    || matches!(threshold, VerdictThreshold::Count(n) if *n == 0 || *n > node.inputs.len())
                {
                    return Err(node_plan_error(node, "invalid verdict gate"));
                }
            }
            Some(GateCondition::Ci {
                target,
                timeout_seconds,
                checks,
                ..
            }) => {
                let mut seen = std::collections::HashSet::new();
                if checks.iter().any(|name| {
                    name.trim().is_empty()
                        || name.trim() != name
                        || name.contains(['\n', '\r'])
                        || name.len() > 1024
                        || !seen.insert(name)
                }) {
                    return Err(node_plan_error(node, "invalid CI gate check names"));
                }
                ci::validate_target(target, node, document)
                    .map_err(|reason| node_plan_error(node, reason))?;
                if !(1..=604800).contains(timeout_seconds) || !node.inputs.is_empty() {
                    return Err(node_plan_error(node, "invalid CI gate target or timeout"));
                }
            }
            Some(GateCondition::Budget { .. }) if !node.inputs.is_empty() => {
                return Err(node_plan_error(node, "budget gate has no selected inputs"));
            }
            _ => {}
        }
        if let (Some(integration), Some(DeliveryMode::Integrate(target))) =
            (&document.integration_branch, &node.delivery)
            && integration != target
        {
            return Err(node_plan_error(
                node,
                "node integration target differs from graph integration branch",
            ));
        }
        if node.required_verdict.as_ref().is_some_and(|line| {
            line.trim().is_empty()
                || line.len() > limits.node_prompt_bytes
                || line.contains(['\n', '\r'])
        }) {
            return Err(node_plan_error(
                node,
                "required verdict must be a non-empty single line",
            ));
        }
        if let Some(title) = &node.title {
            provenant_core::validate_task_title(title)
                .map_err(|reason| node_plan_error(node, reason))?;
        }
        if node
            .role
            .as_ref()
            .is_some_and(|role| !valid_identifier(role))
        {
            return Err(node_plan_error(node, "invalid role name"));
        }
        let mut roots = std::collections::BTreeSet::new();
        if node.read_roots.len() > 32
            || node.read_roots.iter().any(|root| {
                root.is_empty() || root.len() > 4096 || root.contains('\0') || !roots.insert(root)
            })
        {
            return Err(node_plan_error(node, "invalid read roots"));
        }
        if node.attachments.len() > 32
            || node
                .attachments
                .iter()
                .any(|path| path.is_empty() || path.len() > 4096 || path.contains('\0'))
        {
            return Err(node_plan_error(node, "invalid attachments"));
        }
        match &node.source {
            Some(SourceBinding::Commit { commit })
                if commit.parse::<provenant_core::GitOid>().is_err() =>
            {
                return Err(node_plan_error(node, "source requires a full commit ID"));
            }
            Some(SourceBinding::Node { node: source }) if !node.depends_on.contains(source) => {
                return Err(node_plan_error(
                    node,
                    "source node must be a direct dependency",
                ));
            }
            Some(SourceBinding::Integration) if document.integration_branch.is_none() => {
                return Err(node_plan_error(
                    node,
                    "integration source requires an integration branch",
                ));
            }
            _ => {}
        }
        if (is_gate || is_loop || is_composite || is_decide || is_land)
            && (node.source.is_some()
                || !node.read_roots.is_empty()
                || !node.attachments.is_empty())
        {
            return Err(node_plan_error(
                node,
                "execution bindings require an agent node",
            ));
        }
        validate_access(node.access, node.worktree.as_deref())
            .map_err(|reason| node_plan_error(node, reason))?;
        if node.access == AccessMode::ReadOnly
            && !matches!(node.delivery, None | Some(DeliveryMode::None))
        {
            return Err(node_plan_error(node, "read-only node cannot deliver"));
        }
        if document.delivery == Some(DeliveryMode::Pr) && node.delivery == Some(DeliveryMode::Pr) {
            return Err(node_plan_error(node, "graph PR is exclusive with node PRs"));
        }
        if !is_gate && !is_loop && !is_composite && !is_decide && !is_land && node.prompt.is_empty()
            || plans::prompt(node).len() > limits.node_prompt_bytes
        {
            return Err(node_plan_error(node, "prompt size"));
        }
        prompt_bytes = prompt_bytes
            .checked_add(node.content_bytes())
            .ok_or_else(|| node_plan_error(node, "prompt size"))?;
        if prompt_bytes > limits.graph_prompt_bytes {
            return Err(node_plan_error(node, "total prompt size"));
        }
        edge_count = edge_count
            .checked_add(node.dependencies().count())
            .ok_or_else(|| node_plan_error(node, "edge count"))?;
        if edge_count > limits.edges {
            return Err(node_plan_error(node, "edge count"));
        }

        let routes = match (&node.route, &node.routes) {
            (None, None) if is_gate || is_loop || is_composite || is_decide || is_land => {
                Vec::new()
            }
            (Some(route), None) => vec![route.clone()],
            (None, Some(routes)) if !routes.is_empty() => routes.clone(),
            (Some(_), Some(_)) => {
                return Err(node_plan_error(node, "route and routes are exclusive"));
            }
            (None, None) | (None, Some(_)) => {
                return Err(node_plan_error(node, "route list is empty"));
            }
        };
        if routes.len() > limits.routes_per_node {
            return Err(node_plan_error(node, "route count"));
        }
        if routes
            .iter()
            .any(|route| route.adapter.is_none() && route.alias.is_none())
        {
            return Err(node_plan_error(node, "route requires an adapter or alias"));
        }
        for route in &routes {
            if matches!(route.selector(), GraphRouteSelector::Alias(Some(alias)) if provenant_routing::RouteName::parse(alias).is_err())
            {
                return Err(node_plan_error(
                    node,
                    "route alias must be an identifier; exact models require an adapter or --model",
                ));
            }
        }
        if routes
            .iter()
            .any(|route| (route.model.is_some() || route.name.is_some()) && route.adapter.is_none())
        {
            return Err(node_plan_error(
                node,
                "model or named adapter route requires an adapter",
            ));
        }
        if routes.iter().any(|route| !route_is_bounded(route)) {
            return Err(node_plan_error(node, "route field size"));
        }
        // Configured aliases resolve model providers, whose registry excludes host-native.
        for (index, route) in routes.iter().enumerate() {
            if route.adapter.as_deref() == Some("host-native")
                && !routes[index + 1..]
                    .iter()
                    .any(|fallback| fallback.adapter.as_deref() != Some("host-native"))
            {
                return Err(node_plan_error(
                    node,
                    "host-native route requires a later non-native fallback",
                ));
            }
        }
        for route in &routes {
            if let Some(spec) = route
                .adapter
                .as_deref()
                .and_then(provenant_records::AdapterSpec::get)
                && let Err(error) = spec.check_launch(&provenant_records::LaunchRequest {
                    model: route.model.as_deref(),
                    effort: route.effort.as_deref(),
                    write: node.access == AccessMode::WorktreeWrite
                        || node
                            .attempt
                            .as_ref()
                            .is_some_and(|spec| !spec.write_roots.is_empty()),
                    ..Default::default()
                })
            {
                return Err(node_plan_error(node, error.field.refusal()));
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for parent in node.dependencies() {
            if !seen.insert(parent) || parent == &node.id || !positions.contains_key(parent) {
                return Err(node_plan_error(node, "dependency"));
            }
        }
        if node.kind == Some(NodeKind::Synthesise) && node.inputs.is_empty() {
            return Err(node_plan_error(node, "synthesise requires selected inputs"));
        }
        let mut input_nodes = std::collections::BTreeSet::new();
        for input in &node.inputs {
            if input.extra.contains_key("required") {
                return Err(node_plan_error(
                    node,
                    "required output semantics are not supported",
                ));
            }
            if !node.depends_on.contains(&input.node)
                || !input_nodes.insert(&input.node)
                || input.outputs.is_empty()
            {
                return Err(node_plan_error(
                    node,
                    "input must select outputs from a unique direct dependency",
                ));
            }
            let mut kinds = Vec::new();
            for kind in &input.outputs {
                if kinds.contains(kind) {
                    return Err(node_plan_error(node, "duplicate input output"));
                }
                kinds.push(*kind);
            }
            let mut required = Vec::new();
            for kind in &input.required_outputs {
                if !input.outputs.contains(kind)
                    || required.contains(kind)
                    || !matches!(
                        kind,
                        OutputKind::Commit | OutputKind::Verdict | OutputKind::Findings
                    )
                {
                    return Err(node_plan_error(
                        node,
                        "required outputs must be unique selected commit, verdict or findings values",
                    ));
                }
                required.push(*kind);
            }
        }
        next_step::validate_on_stop(node).map_err(AdmissionError::Plan)?;
        let mut depends_on = node.depends_on.clone();
        depends_on.sort();
        nodes.push(PlannedNode {
            review_launch: is_review_node(node),
            on_stop: node.on_stop.clone(),
            family_group: node.family_group.clone(),
            context: node.context.clone(),
            extra: node.extra.clone(),
            composite_members: node.composite_members.clone(),
            composite: node.composite.clone(),
            role: node.role.clone(),
            distinct_from: node.distinct_from,
            distinct_strength: node.distinct_strength,
            dispositions: node.dispositions.clone(),
            read_roots: node.read_roots.clone(),
            attachments: node.attachments.clone(),
            // An explicit node set wins; a nonempty attempt set stands in for it.
            capabilities: node.capabilities.clone().or_else(|| {
                node.attempt
                    .as_ref()
                    .filter(|spec| !spec.capabilities.is_empty())
                    .map(|spec| spec.capabilities.clone())
            }),
            attempt: node.attempt.clone().filter(|spec| !spec.is_empty()),
            source: node.source.clone(),
            after_expansion: node.after_expansion.clone(),
            plan: node.plan.clone(),
            write_scope: node.write_scope.clone(),
            acceptance_checks: node.acceptance_checks.clone(),
            loop_body: node.loop_body.clone(),
            decide: node.decide.clone(),
            continues: node.continues.clone(),
            land: node.land.clone(),
            round: node.round.clone(),
            kind: match node.kind {
                Some(
                    NodeKind::Synthesise
                    | NodeKind::Gate
                    | NodeKind::Loop
                    | NodeKind::Composite
                    | NodeKind::Decide
                    | NodeKind::Land,
                ) => node.kind,
                _ => None,
            },
            condition: node.condition.clone(),
            on_fail: node.on_fail.clone(),
            delivery: node.delivery.clone(),
            access: node.access,
            require_skill_location: node.require_skill_location,
            worktree: node.worktree.clone(),
            id: node.id.clone(),
            title: node.title.clone(),
            prompt: plans::prompt(node),
            depends_on,
            advisory: node.advisory,
            inputs: node.inputs.clone(),
            routes,
            required_verdict: node.required_verdict.clone(),
            deadline_seconds: node.deadline_seconds,
            when: node.when.clone(),
            require_findings: node.require_findings,
            skip_without_findings: node.skip_without_findings,
        });
    }

    let mut indegree = vec![0usize; nodes.len()];
    let mut children = vec![Vec::new(); nodes.len()];
    for (child, node) in nodes.iter().enumerate() {
        indegree[child] = node.dependencies().count();
        for parent in node.dependencies() {
            let parent_index = *positions
                .get(parent)
                .ok_or_else(|| plan_error("dependency"))?;
            children[parent_index].push(child);
        }
    }
    let mut ready: std::collections::VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(index) = ready.pop_front() {
        order.push(index);
        for child in &children[index] {
            indegree[*child] -= 1;
            if indegree[*child] == 0 {
                ready.push_back(*child);
            }
        }
    }
    if order.len() != nodes.len() {
        let node = nodes
            .iter()
            .enumerate()
            .find(|(index, _)| indegree[*index] > 0)
            .map(|(_, node)| node.id.as_str())
            .unwrap_or("unknown");
        return Err(AdmissionError::Plan(format!(
            "node {node}: dependency cycle"
        )));
    }
    land::validate_relations(&nodes, &positions).map_err(AdmissionError::Plan)?;

    // Dependencies are conjunctive. Reject nodes that require mutually
    // exclusive branches, including through transitive dependencies.
    // Analyse one gate at a time so over-limit plans can pause without
    // materialising a quadratic ancestry table first.
    for (gate, node) in nodes.iter().enumerate() {
        let Some(target) = &node.on_fail else {
            continue;
        };
        let mut required = vec![None; nodes.len()];
        let mut pending: std::collections::VecDeque<_> = children[gate]
            .iter()
            .filter(|child| nodes[**child].depends_on.contains(&node.id))
            .map(|child| {
                (
                    *child,
                    target != &nodes[*child].id
                        && !nodes[*child]
                            .round
                            .as_ref()
                            .is_some_and(|r| &r.loop_id == target),
                )
            })
            .collect();
        while let Some((index, outcome)) = pending.pop_front() {
            if let Some(prior) = required[index] {
                if prior != outcome {
                    return Err(AdmissionError::Plan(format!(
                        "node {}: requires both gate outcomes",
                        nodes[index].id
                    )));
                }
                continue;
            }
            required[index] = Some(outcome);
            pending.extend(
                children[index]
                    .iter()
                    .filter(|child| nodes[**child].depends_on.contains(&nodes[index].id))
                    .map(|child| (*child, outcome)),
            );
        }
    }

    let topological_nodes: Vec<PlannedNode> =
        order.iter().map(|index| nodes[*index].clone()).collect();
    let topological_positions = topological_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    Ok((
        PlannedGraph {
            review_base: document.review_base.clone(),
            context: document.context.clone(),
            extra: document.extra.clone(),
            integration_branch: document.integration_branch.clone(),
            delivery: document.delivery.clone(),
            decision_session: document.decision_session.clone(),
            schema_version: document.schema_version,
            graph_id: document.graph_id.clone(),
            nodes: topological_nodes,
        },
        topological_positions,
    ))
}

pub(super) fn valid_node_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Why a node's attempt spec cannot launch through the graph, if it cannot.
/// Worktree creation is owned by the graph, not an individual attempt.
fn attempt_error(node: &GraphNode, spec: &AttemptSpec) -> Option<&'static str> {
    if spec.directory.as_ref().is_some_and(|directory| {
        directory.len() > 4096
            || directory.contains('\0')
            || !std::path::Path::new(directory).is_absolute()
    }) {
        return Some("invalid attempt directory");
    }
    if let Some(new) = &spec.new_worktree {
        if node.access != AccessMode::WorktreeWrite {
            return Some("new worktree requires a writer");
        }
        if node.source.is_some() || node.continues.is_some() {
            return Some("new worktree cannot override a source binding");
        }
        if provenant_confinement::repo::GitRefName::branch(&new.branch).is_err()
            || node.worktree.as_deref().is_none_or(|path| path.len() > 251)
            || provenant_confinement::repo::GitRevision::new(&new.base).is_err()
        {
            return Some("invalid new worktree binding");
        }
    }
    if spec.write_roots.len() > 32
        || spec
            .write_roots
            .iter()
            .any(|root| root.is_empty() || root.len() > 4096 || root.contains('\0'))
    {
        return Some("invalid write roots");
    }
    if node.capabilities.is_some() && !spec.capabilities.is_empty() {
        return Some("set capabilities on the node or its attempt, not both");
    }
    if !spec.preset_known() {
        return Some("unknown attempt preset");
    }
    for (value, allowed) in [
        (&spec.isolation, ["private", "shared"]),
        (&spec.supervision, ["standard", "minimal"]),
        (&spec.receipts, ["delivery", "none"]),
    ] {
        if value.as_deref().is_some_and(|v| !allowed.contains(&v)) {
            return Some("invalid attempt isolation, supervision or receipts");
        }
    }
    if node.access == AccessMode::WorktreeWrite && spec.effective_isolation() == "shared" {
        return Some("a writer keeps its own worktree; shared isolation is for read-only nodes");
    }
    if spec.capabilities.iter().any(|capability| {
        capability.is_empty() || capability.len() > 64 || capability.contains('\0')
    }) {
        return Some("attempt capability must be a bounded name");
    }
    let account = spec.account.as_deref()?;
    if account.is_empty() || account.len() > 64 || account.contains('\0') {
        return Some("attempt account must be a bounded name");
    }
    if node
        .route
        .iter()
        .chain(node.routes.iter().flatten())
        .any(|route| {
            route
                .account
                .as_deref()
                .is_some_and(|pinned| pinned != account)
        })
    {
        return Some("attempt account conflicts with a route account");
    }
    None
}

pub(super) fn route_is_bounded(route: &GraphRoute) -> bool {
    fn field(value: &str, limit: usize) -> bool {
        !value.is_empty() && value.len() <= limit && !value.contains('\0')
    }
    route
        .adapter
        .as_deref()
        .is_none_or(|value| field(value, 128))
        && (route.model.is_some() || route.alias.is_some() || route.name.is_some())
        && !(route.name.is_some() && (route.model.is_some() || route.alias.is_some()))
        && route.name.as_deref().is_none_or(|value| field(value, 128))
        && (!matches!(route.selector(), GraphRouteSelector::Adapter(_))
            || (route.effort.is_none() && route.account.is_none()))
        && route.model.as_deref().is_none_or(|value| field(value, 256))
        && route.alias.as_deref().is_none_or(|value| field(value, 128))
        && route.effort.as_deref().is_none_or(|value| field(value, 64))
        && route.tier.as_deref().is_none_or(|value| field(value, 128))
        && route
            .account
            .as_deref()
            .is_none_or(|value| field(value, 64))
}

/// Why settled outputs lack the dispositions a node declares, if they do.
/// A findings block must parse, each finding needs a disposition from the
/// declared list with a non-empty reason, and every selected upstream finding
/// identity must appear exactly once.
pub(super) fn review_verification_error(
    node: &PlannedNode,
    upstream: &[Finding],
    required: &[Finding],
    overflow: &[(String, String)],
    outputs: &NodeOutputs,
) -> Option<(&'static str, String)> {
    if !template::review_verifier(node) {
        return None;
    }
    if template::review_behaviour(node).is_some()
        && outputs.findings.iter().any(|finding| {
            !upstream.iter().any(|source| source.id == finding.id)
                && !overflow.iter().any(|(id, _)| id == &finding.id)
                && (finding
                    .extra
                    .get("verification")
                    .and_then(serde_json::Value::as_str)
                    != Some("new")
                    || finding
                        .reason
                        .as_ref()
                        .is_none_or(|reason| reason.trim().is_empty()))
        })
    {
        return Some((
            "verification_invalid",
            "new verifier findings need verification=new and an evidence reason".into(),
        ));
    }
    for finding in required {
        let matches: Vec<_> = outputs
            .findings
            .iter()
            .filter(|output| output.id == finding.id)
            .collect();
        if matches.len()
            != required
                .iter()
                .filter(|source| source.id == finding.id)
                .count()
            || matches.iter().any(|output| {
                !output
                    .extra
                    .get("verification")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| matches!(value, "upheld" | "falsified" | "unverifiable"))
                    || output
                        .reason
                        .as_ref()
                        .is_none_or(|reason| reason.trim().is_empty())
            })
        {
            return Some((
                "verification_incomplete",
                format!(
                    "finding {} needs exactly one verification and an evidence reason",
                    finding.id
                ),
            ));
        }
    }
    None
}

pub(super) fn disposition_error(
    dispositions: &[String],
    upstream: &[Finding],
    outputs: &NodeOutputs,
) -> Option<(&'static str, String)> {
    if dispositions.is_empty() || outputs.result.is_none() {
        return None;
    }
    let allowed = dispositions.join(", ");
    if let Some(warning) = outputs
        .warnings
        .iter()
        .find(|warning| warning.starts_with(FINDINGS_INVALID))
    {
        return Some((
            "disposition_findings_invalid",
            format!("dispositions required ({allowed}): {warning}"),
        ));
    }
    for finding in &outputs.findings {
        let valid = finding
            .disposition
            .as_ref()
            .is_some_and(|value| dispositions.contains(value));
        if !valid {
            return Some((
                "disposition_invalid",
                format!(
                    "finding {} needs a disposition of {allowed}; found {}",
                    finding.id,
                    finding.disposition.as_deref().unwrap_or("none"),
                ),
            ));
        }
        if finding
            .reason
            .as_ref()
            .is_none_or(|reason| reason.trim().is_empty())
        {
            return Some((
                "disposition_reason_missing",
                format!("finding {} needs a reason for its disposition", finding.id),
            ));
        }
    }
    for id in upstream
        .iter()
        .map(|finding| &finding.id)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let count = outputs.findings.iter().filter(|f| &f.id == id).count();
        let copies = upstream.iter().filter(|f| &f.id == id).count();
        if count != copies {
            return Some((
                "disposition_count",
                format!(
                    "upstream finding {id} needs exactly one disposition of {allowed}; found {}",
                    count / copies
                ),
            ));
        }
    }
    None
}

/// Whether the attempt ran outside the families that served its upstream.
pub(super) fn separation(
    strength: FamilyStrength,
    excluded: Vec<String>,
    applied: Option<String>,
) -> FamilySeparation {
    let outcome = if excluded.is_empty() {
        SeparationOutcome::Unconstrained
    } else if applied
        .as_ref()
        .is_some_and(|family| !excluded.contains(family))
    {
        SeparationOutcome::Separated
    } else {
        SeparationOutcome::Unavailable
    };
    FamilySeparation {
        extra: Default::default(),
        strength,
        excluded: excluded
            .iter()
            .take(SEPARATION_FAMILIES)
            .map(|family| bounded(family, FAMILY_BYTES))
            .collect(),
        preferred: Vec::new(),
        applied: applied.map(|family| bounded(&family, FAMILY_BYTES)),
        outcome,
    }
}

pub(super) fn validate_expanded_rows<R: Error + 'static, J: Error + 'static>(
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
) -> Result<(), GraphError<R, J>> {
    if checkpoint.nodes.len() != plan.nodes.len() {
        return Err(GraphError::BadCheckpoint {
            node: plan.graph_id.clone(),
            invariant: "checkpoint node count must match expanded plan",
        });
    }
    for (progress, node) in checkpoint.nodes.iter().zip(&plan.nodes) {
        if progress.id != node.id || progress.round != node.round {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "checkpoint row identity and round must match expanded plan",
            });
        }
    }
    Ok(())
}

pub(super) fn validate_checkpoint<R: Error + 'static, J: Error + 'static>(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
    digest: &str,
) -> Result<(), GraphError<R, J>> {
    if checkpoint.schema_version != 2
        || checkpoint.graph_id != plan.graph_id
        || checkpoint.delivery_required
            != (plan.delivery == Some(provenant_records::run::DeliveryMode::Pr))
    {
        return Err(GraphError::BadCheckpoint {
            node: plan.graph_id.clone(),
            invariant: "schema, graph identity and delivery policy must match plan",
        });
    }
    if checkpoint.plan_digest != digest {
        return Err(GraphError::PlanChanged);
    }
    if checkpoint.nodes.len() != plan.nodes.len()
        || plan
            .decision_session
            .as_ref()
            .is_some_and(|session| crate::inbox::session_id(session).is_err())
    {
        return Err(GraphError::BadCheckpoint {
            node: plan.graph_id.clone(),
            invariant: "node count and decision session must match plan",
        });
    }
    asks::validate(checkpoint, plan).map_err(|_| GraphError::BadCheckpoint {
        node: plan.graph_id.clone(),
        invariant: "asks must match checkpoint identities and decision sessions",
    })?;
    let positions: std::collections::BTreeMap<String, usize> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.clone(), i))
        .collect();
    let active_expansions = checkpoint
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.expansion_cause,
                Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
            )
        })
        .count();
    if active_expansions > 1
        || (active_expansions == 1)
            != (checkpoint.pause_cause() == Some(PauseCause::InvalidPlanExpansion))
    {
        return Err(GraphError::BadCheckpoint {
            node: checkpoint.graph_id.clone(),
            invariant: "active expansion pause must own the graph pause",
        });
    }
    if let Some(cause) = checkpoint
        .pause_cause()
        .and_then(NodeDecisionCause::from_pause)
        && (!checkpoint
            .nodes
            .iter()
            .any(|node| node.loop_cause.as_ref() == Some(&cause)))
    {
        return Err(GraphError::BadCheckpoint {
            node: checkpoint.graph_id.clone(),
            invariant: "loop pause code must have a matching typed node cause",
        });
    }
    for (index, (progress, node)) in checkpoint.nodes.iter().zip(&plan.nodes).enumerate() {
        let mut historical;
        let checkpoint = if progress.state.settled() {
            if let Some(snapshots) = progress
                .extra
                .get("settled_dependency_snapshots")
                .and_then(serde_json::Value::as_object)
            {
                historical = checkpoint.clone();
                for (id, snapshot) in snapshots {
                    let position = *positions.get(id).ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "dependency snapshot must name a graph node",
                    })?;
                    let mut prior: NodeProgress = serde_json::from_value(snapshot.clone())
                        .map_err(|_| GraphError::BadCheckpoint {
                            node: node.id.clone(),
                            invariant: "dependency snapshot must be valid progress",
                        })?;
                    if prior.id != *id || !prior.state.settled() {
                        return Err(GraphError::BadCheckpoint {
                            node: node.id.clone(),
                            invariant: "dependency snapshot must retain settled identity",
                        });
                    }
                    prior.round = historical.nodes[position].round.clone();
                    historical.nodes[position] = prior;
                }
                &historical
            } else {
                checkpoint
            }
        } else {
            checkpoint
        };
        let bad = |invariant| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant,
        };
        let valid_loop_cause = match (
            &progress.loop_cause,
            progress.loop_pause_code.as_deref(),
            &progress.loop_pause,
        ) {
            (None, None, None) => true,
            (Some(NodeDecisionCause::LoopPaused), Some("loop_paused"), Some(_)) => {
                node.loop_body.is_some()
            }
            (Some(NodeDecisionCause::LoopAskClosed), Some("loop_ask_closed"), Some(_)) => {
                node.loop_body.is_some()
            }
            (
                Some(NodeDecisionCause::InvalidLoopDecision),
                Some("invalid_loop_decision"),
                Some(_),
            ) => node.loop_body.is_some(),
            _ => false,
        };
        let valid_expansion_cause = match (&progress.expansion_cause, &progress.expansion_pause) {
            (None, None) => true,
            (
                Some(NodeDecisionCause::PlanExpansionPaused {
                    active,
                    fallback,
                    fragment_error,
                }),
                pause,
            ) => {
                plans::agent(node)
                    && (pause.is_some() || (*active && *fallback))
                    && fragment_error.is_none_or(|fact| match fact {
                        PlanFailureFact::Structural(limit) => limit.actual > limit.max,
                        PlanFailureFact::Envelope(limit) => {
                            limit.bound > MAX_CHECKPOINT_BYTES as usize
                        }
                        _ => true,
                    })
            }
            _ => false,
        };
        let valid_truncation_owner = match &progress.truncation_cause {
            None => true,
            Some(NodeDecisionCause::CompositeFindingsTruncated { .. }) => node.composite.is_some(),
            _ => false,
        };
        for (invalid, invariant) in [
            (
                !valid_expansion_cause,
                "expansion pause cause must match its node",
            ),
            (
                !valid_loop_cause,
                "loop pause cause must match its code and loop boundary",
            ),
            (
                !valid_truncation_owner,
                "truncation cause requires a composite boundary",
            ),
            (
                !valid_compacted_findings(index, plan, checkpoint, &positions),
                "compacted findings must reconstruct from the node inputs",
            ),
            (
                progress.round != node.round,
                "round must match expanded plan",
            ),
            (
                progress.children != node.composite_members,
                "composite children must match expanded plan membership",
            ),
            (
                progress.min_successful.is_some()
                    && progress.min_successful
                        != node.composite.as_ref().and_then(|spec| spec.min_successful),
                "composite minimum must match expanded plan",
            ),
            (
                node.composite.is_some()
                    && (node.composite_members.is_empty()
                        || node
                            .composite_members
                            .iter()
                            .any(|id| !node.after_expansion.contains(id))),
                "composite completion edges must include every member",
            ),
            (
                document.schema_version >= 2 && progress.source_binding != node.source,
                "source selection must match the expanded plan",
            ),
            (
                progress.extra.get("new_worktree_base").is_some_and(|base| {
                    node.attempt
                        .as_ref()
                        .is_none_or(|spec| spec.new_worktree.is_none())
                        || base
                            .as_str()
                            .is_none_or(|base| base.parse::<provenant_core::GitOid>().is_err())
                }),
                "writer provisioning must retain a full commit base",
            ),
            (
                progress.source_commit.is_some()
                    && node.source.is_none()
                    && progress.writer_binding.is_none(),
                "resolved source requires a source binding",
            ),
            (
                progress
                    .source_commit
                    .as_ref()
                    .is_some_and(|commit| commit.parse::<provenant_core::GitOid>().is_err()),
                "resolved source must be a full commit ID",
            ),
            (
                progress.id != node.id,
                "row identity must match expanded plan order",
            ),
            (
                progress.worktree != node.worktree,
                "worktree must match expanded plan",
            ),
            (
                progress.attempts.len() > node.routes.len(),
                "attempt count must not exceed configured routes",
            ),
            (
                progress
                    .outputs
                    .as_ref()
                    .is_some_and(|outputs| outputs.schema_version != 2),
                "output schema must be version 2",
            ),
        ] {
            if invalid {
                return Err(bad(invariant));
            }
        }
        if let Some(condition) = &node.condition {
            let valid = match (&progress.gate, progress.state) {
                (None, NodeState::Pending | NodeState::Blocked | NodeState::Skipped) => {
                    progress.outputs.is_none() && progress.ci.is_none()
                }
                (None, NodeState::Running) => {
                    matches!(node.condition, Some(GateCondition::Ci { .. }))
                        && progress.ci.as_ref().is_some_and(|ci| {
                            ci.state == CiState::Pending
                                && ci.wake_at.is_some()
                                && ci.pending_observations > 0
                        })
                        && progress.outputs.is_none()
                        && dependency_state(node, plan, checkpoint, &positions).is_none()
                }
                (Some(result), NodeState::Succeeded | NodeState::Failed) => {
                    let mut expected = if matches!(condition, GateCondition::Ci { .. }) {
                        ci::outputs(result.passed, progress.ci.as_ref())
                    } else {
                        gate_outputs(result.passed)
                    };
                    // Older checkpoints lack availability evidence; retain
                    // that unknown state rather than reconstructing it.
                    if progress
                        .outputs
                        .as_ref()
                        .is_some_and(|outputs| outputs.findings_available.is_none())
                    {
                        expected.findings_available = None;
                    }
                    result.passed == (progress.state == NodeState::Succeeded)
                        && result.passed == result.reason.is_none()
                        && result.on_fail == node.on_fail
                        && progress.outputs == Some(expected)
                        && dependency_state(node, plan, checkpoint, &positions).is_none()
                }
                _ => false,
            };
            if !valid
                || (progress.state != NodeState::Running
                    && progress.ci.as_ref().is_some_and(|ci| ci.wake_at.is_some()))
                || progress
                    .ci
                    .as_ref()
                    .is_some_and(|evidence| !ci::valid_evidence(evidence, condition))
                || !progress.attempts.is_empty()
                || progress.advance_error.is_some()
                || (progress.ci.is_some()
                    && !matches!(node.condition, Some(GateCondition::Ci { .. })))
                || (matches!(node.condition, Some(GateCondition::Ci { .. }))
                    && progress.gate.is_some()
                    && progress.ci.is_none())
            {
                return Err(bad(
                    "gate evidence must match state, dependencies and condition",
                ));
            }
        } else if progress.gate.is_some() && !repair::is_repair(node) || progress.ci.is_some() {
            return Err(bad("only gate nodes may carry gate or CI evidence"));
        }
        if !when::holds(&node.when) {
            // A node its own condition skips is a leaf of the live graph: it
            // never runs, whatever its former parents do.
            if progress.state != NodeState::Skipped
                || !progress.attempts.is_empty()
                || progress.outputs.is_some()
            {
                return Err(bad(
                    "a node skipped by its when condition has no attempts or outputs",
                ));
            }
            continue;
        }
        if progress.state == NodeState::Skipped
            && (dependency_state(node, plan, checkpoint, &positions) != Some(NodeState::Skipped)
                && !loops::inactive_round(node, plan, checkpoint, &positions)
                && !repair::inactive_lens(node, plan, checkpoint, &positions)
                && !patch::skipped_by_patch(checkpoint, &node.id)
                || !progress.attempts.is_empty()
                || progress.outputs.is_some())
        {
            return Err(bad(
                "skipped node must have an inactive dependency and no attempts or outputs",
            ));
        }
        if node.loop_body.is_some() {
            loops::validate_progress(progress, node, checkpoint, &positions)
                .map_err(|_| bad("loop progress must match round state"))?;
            continue;
        }
        if node.composite.is_some() {
            composites::validate_progress(progress, node, plan, checkpoint, &positions)
                .map_err(|_| bad("composite state and outputs must match member settlement"))?;
            continue;
        }
        if node.decide.is_some() {
            asks::validate_progress(progress, node, checkpoint)
                .map_err(|_| bad("decision progress must match its ask"))?;
            continue;
        }
        if node.land.is_some() {
            land::validate_progress(progress, checkpoint)
                .map_err(|_| bad("land progress must match its record"))?;
            continue;
        } else if progress.land.is_some() {
            return Err(bad("land record requires a land node"));
        }
        let requests_review_base =
            node.access == AccessMode::ReadOnly && node.extra.contains_key("review_base");
        if progress.review_base_pending
            && (!requests_review_base
                || progress.review_base.is_some()
                || !progress.attempts.is_empty())
        {
            return Err(bad(
                "pending review base must belong to an unlaunched reader",
            ));
        }
        if document.schema_version >= 2
            && requests_review_base
            && progress.review_base.is_none()
            && !progress.review_base_pending
        {
            return Err(bad("reader must retain its review base binding"));
        }
        if progress.writer_binding_pending
            && (!plans::writer(node)
                || progress.writer_binding.is_some()
                || !progress.attempts.is_empty())
        {
            return Err(bad(
                "pending writer binding must belong to an unlaunched writer",
            ));
        }
        if let Some(binding) = &progress.writer_binding {
            if !plans::writer(node)
                || !std::path::Path::new(&binding.worktree).is_absolute()
                || binding.worktree.len() > 4096
                || binding.head.parse::<provenant_core::GitOid>().is_err()
                || (node.source.is_none()
                    && node.continues.is_none()
                    && progress.source_commit.as_ref() != Some(&binding.head))
            {
                return Err(bad(
                    "writer binding must retain its worktree and initial head",
                ));
            }
        } else if document.schema_version >= 2
            && plans::writer(node)
            && !progress.writer_binding_pending
        {
            return Err(bad("writer must retain its binding"));
        }
        if !progress.attempts.is_empty()
            && node.source.is_some()
            && progress.source_commit.is_none()
        {
            return Err(bad(
                "launched source-bound node must retain its resolved source commit",
            ));
        }
        if let Some(commit) = &progress.source_commit {
            let expected = match &node.source {
                Some(SourceBinding::Commit { commit }) => Some(commit),
                Some(SourceBinding::Node { node: source }) => positions
                    .get(source)
                    .and_then(|index| checkpoint.nodes[*index].outputs.as_ref())
                    .and_then(|outputs| outputs.commit.as_ref()),
                _ => None,
            };
            if matches!(
                node.source,
                Some(SourceBinding::Commit { .. } | SourceBinding::Node { .. })
            ) && expected != Some(commit)
            {
                return Err(bad("resolved source commit must match its binding"));
            }
        }
        if node.condition.is_some() {
            continue;
        }
        let mut previous_number = 0;
        for attempt in progress.recorded_attempts() {
            if attempt.key.graph_id != plan.graph_id
                || attempt.key.node_id != node.id
                || attempt.key.attempt <= previous_number
            {
                return Err(bad(
                    "attempts must retain distinct WorkKey identities for this node",
                ));
            }
            previous_number = attempt.key.attempt;
        }
        for (index, attempt) in progress.attempts.iter().enumerate() {
            if document.schema_version >= 2 && attempt.input_manifest.is_none() {
                return Err(bad("attempt must retain its input manifest"));
            }
            if let Some(manifest) = &attempt.input_manifest {
                let count: usize = node.inputs.iter().map(|input| input.outputs.len()).sum();
                if manifest.schema_version != 1
                    || manifest.inputs.len() > MANIFEST_INPUT_LIMIT
                    || manifest.inputs.len().saturating_add(manifest.omitted) != count
                    || manifest.source_commit != progress.source_commit
                    || manifest.review_base != progress.review_base
                    || manifest
                        .inputs
                        .iter()
                        .zip(
                            node.inputs.iter().flat_map(|input| {
                                input.outputs.iter().map(move |kind| (input, kind))
                            }),
                        )
                        .any(|(evidence, (input, kind))| {
                            evidence.producer_node != input.node
                                || evidence.kind != *kind
                                || evidence.required != input.required_outputs.contains(kind)
                        })
                    || (attempt.state != AttemptState::Starting
                        && manifest
                            .inputs
                            .iter()
                            .any(|input| input.file_delivery == InputDelivery::Pending))
                {
                    return Err(bad(
                        "attempt input manifest must match its selected inputs and pins",
                    ));
                }
            }
            if attempt.key.graph_id != plan.graph_id
                || attempt.key.node_id != node.id
                || usize::from(attempt.key.route_index) != index
                || attempt.route != node.routes[index]
                || (attempt.review_substitution
                    && (index == 0
                        || !(review_substitution(node, &progress.attempts[index - 1])
                            || review_preparation_substitution(
                                node,
                                &progress.attempts[index - 1],
                            ))))
                || (index > 0
                    && review_substitution(node, &progress.attempts[index - 1])
                    && !attempt.review_substitution)
                || (attempt.state == AttemptState::Terminal) != attempt.outcome.is_some()
                || (index + 1 < progress.attempts.len()
                    && (attempt.state != AttemptState::Terminal
                        || !(review_substitution(node, attempt)
                            || attempt.outcome.is_some_and(|outcome| {
                                fallback_candidate(outcome, node.access, attempt.launched)
                            }))))
            {
                return Err(bad(
                    "attempt key, route, terminal outcome and fallback order must match plan",
                ));
            }
        }
        let last = progress.attempts.last();
        let state_is_consistent = match progress.state {
            NodeState::Pending | NodeState::Skipped => progress.attempts.is_empty(),
            NodeState::Blocked => {
                progress.attempts.is_empty()
                    || (matches!(
                        progress.advance_error_code.as_deref(),
                        Some("launch_refused" | "launch_retry_exhausted")
                    ) && last.is_some_and(|attempt| {
                        attempt.state != AttemptState::Terminal && attempt.attempt_ref.is_none()
                    }))
            }
            NodeState::Running => match last {
                Some(attempt) if attempt.state != AttemptState::Terminal => true,
                Some(attempt) => {
                    (review_substitution(node, attempt)
                        || attempt.outcome.is_some_and(|outcome| {
                            fallback_candidate(outcome, node.access, attempt.launched)
                        }))
                        && progress.attempts.len() < node.routes.len()
                }
                // Writer binding and provisioning wait before the first
                // attempt exists. Only the bounded contention state emitted by
                // defer_launch_contention may replay as Running here.
                None => {
                    plans::writer(node)
                        && progress.writer_binding_pending
                        && progress.outputs.is_none()
                        && progress.advance_error.is_none()
                        && matches!(
                            progress.advance_owner_code.as_deref(),
                            Some("busy" | "writer_lock_busy" | "writer_unknown" | "writer_busy")
                        )
                        && progress.launch_retry.as_ref().is_some_and(|retry| {
                            retry.failures > 0
                                && matches!((retry.wake_at, retry.deadline),
                                    (Some(wake), Some(deadline)) if wake <= deadline)
                                && retry.reason.as_ref().is_some_and(|reason| {
                                    !reason.is_empty() && reason.len() <= MAX_ADVANCE_ERROR_BYTES
                                })
                        })
                }
            },
            // A planner whose failure was settled by its static fallback is
            // succeeded though its last attempt failed (or never launched).
            NodeState::Succeeded => {
                last.is_some_and(|attempt| {
                    attempt.state == AttemptState::Terminal
                        && matches!(
                            attempt.outcome,
                            Some(AttemptOutcome::Succeeded | AttemptOutcome::ReviewRequired)
                        )
                }) || fallback_settled(node, progress, document, checkpoint)
            }
            NodeState::Failed if progress.advance_error.is_some() => last.is_none_or(|attempt| {
                attempt.state == AttemptState::Terminal
                    && attempt
                        .outcome
                        .is_some_and(|outcome| outcome != AttemptOutcome::Succeeded)
            }),
            NodeState::Failed => last.is_some_and(|attempt| {
                attempt.state == AttemptState::Terminal
                    && match attempt.outcome {
                        Some(
                            AttemptOutcome::TaskFailed
                            | AttemptOutcome::OtherFailure
                            | AttemptOutcome::ReviewRequired,
                        ) => true,
                        Some(outcome) if outcome != AttemptOutcome::Succeeded => {
                            !fallback_candidate(outcome, node.access, attempt.launched)
                                || progress.attempts.len() == node.routes.len()
                        }
                        _ => false,
                    }
            }),
        };
        if !state_is_consistent {
            return Err(bad(
                "node state must agree with terminal attempt outcome or validated planner fallback",
            ));
        }
    }
    Ok(())
}

/// A planner settled by its static fallback: it recorded the reason and
/// persisted exactly the stage its fallback spec derives from the document.
fn fallback_settled(
    node: &PlannedNode,
    progress: &NodeProgress,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
) -> bool {
    let Some(spec) = node.plan.as_ref().filter(|spec| spec.fallback.is_some()) else {
        return false;
    };
    progress
        .outputs
        .as_ref()
        .is_some_and(|outputs| outputs.plan.is_some())
        && plans::fallback_expansion_for_generation(
            &node.id,
            spec,
            document,
            node.require_skill_location,
            progress.extra.get("retry_generation"),
        )
        .is_ok_and(|derived| {
            checkpoint
                .expansions
                .iter()
                .any(|expansion| expansion.source == node.id && *expansion == derived)
        })
}
