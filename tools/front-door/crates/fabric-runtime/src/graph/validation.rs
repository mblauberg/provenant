use super::*;

pub(super) fn nodes_have_v2(nodes: &[GraphNode]) -> bool {
    nodes.iter().any(|node| {
        node.review.is_some()
            || node.extra.contains_key("review_base")
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
                spec.patterns.iter().any(|pattern| {
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
                || map.get("review").is_some_and(serde_json::Value::is_object)
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
    if document_requires_schema_v2(document) {
        2
    } else {
        1
    }
}

pub(super) fn document_requires_schema_v2(document: &GraphDocument) -> bool {
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

pub(super) fn validate_output_kinds(document: &GraphDocument) -> Result<(), String> {
    let by_id: std::collections::BTreeMap<_, _> =
        document.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for consumer in &document.nodes {
        if consumer.plan.as_ref().is_some_and(|plan| {
            plan.patterns.iter().any(|pattern| {
                pattern.exports.iter().any(|input| {
                    !input.required_outputs.is_empty() || input.extra.contains_key("required")
                })
            }) || plan.fallback.as_ref().is_some_and(|fallback| {
                fallback.exports.iter().any(|input| {
                    !input.required_outputs.is_empty() || input.extra.contains_key("required")
                })
            })
        }) {
            return Err(format!(
                "node {} declares requirements on planner exports, which are not supported",
                consumer.id
            ));
        }
        if consumer.composite.as_ref().is_some_and(|composite| {
            composite
                .exports
                .iter()
                .any(|input| !input.required_outputs.is_empty())
        }) {
            return Err(format!(
                "node {} declares requirements on composite exports, which are not supported",
                consumer.id
            ));
        }
        // A composite's export declaration is its public output contract. Its
        // members are validated in their own scopes; consumers are checked
        // against the declared composite outputs through `supplied_kinds`.
        for input in consumer.inputs.iter().chain(
            consumer
                .composite
                .iter()
                .flat_map(|composite| &composite.exports),
        ) {
            if input.extra.contains_key("required") {
                return Err(format!(
                    "node {} declares required output semantics that this runtime cannot enforce",
                    consumer.id
                ));
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
                return Err(format!(
                    "node {} requires outputs from {}, whose expansion cannot yet be verified",
                    consumer.id, input.node
                ));
            }
            let available = supplied_kinds(source);
            for kind in &input.outputs {
                if !available.contains(kind) {
                    let labels = available
                        .iter()
                        .map(|v| format!("{v:?}").to_lowercase())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "node {} selects {} from {}, which supplies only {}",
                        consumer.id,
                        format!("{kind:?}").to_lowercase(),
                        source_id,
                        labels
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_schema(document: &GraphDocument) -> Result<(), &'static str> {
    let has_v2 = document.review_base.is_some() || nodes_have_v2(&document.nodes);
    // Version 2 is a superset: it may omit every v2 field, but version 1
    // cannot carry one.
    if !matches!(document.schema_version, 1 | 2) || document.schema_version == 1 && has_v2 {
        return Err("schema version");
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
        .map_err(|reason| &*Box::leak(reason.into_boxed_str()))
}

pub(super) fn validate_plan_with_limits(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), String> {
    document.admit(limits)?;
    validate_replay_plan(document, limits)
}

pub(super) fn validate_replay_plan(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), String> {
    validate_schema(document).map_err(String::from)?;
    let expansion_limits = super::model::expansion_safety_limits(limits);
    let expanded = loops::expand(document, &[], &expansion_limits)?;
    let plan = validate_flat_plan(&expanded, &expansion_limits).map_err(String::from)?;
    Ok(plan)
}

pub(super) fn validate_flat_plan(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), &'static str> {
    if !matches!(document.schema_version, 1 | 2) {
        return Err("schema version");
    }
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
        return Err("invalid work context");
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
        return Err("graph delivery requires an integration branch and at most one PR");
    }
    if !valid_identifier(&document.graph_id) {
        return Err("graph id");
    }
    if document
        .decision_session
        .as_ref()
        .is_some_and(|session| crate::inbox::session_id(session).is_err())
    {
        return Err("invalid decision_session");
    }
    if document.nodes.is_empty() || document.nodes.len() > limits.nodes {
        return Err("node count");
    }

    let mut positions = std::collections::BTreeMap::<String, usize>::new();
    for (index, node) in document.nodes.iter().enumerate() {
        if !valid_identifier(&node.id) || positions.insert(node.id.clone(), index).is_some() {
            return Err("node id");
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
            return Err("node review_base must be a nonempty reference");
        }
        if node.extra.contains_key("composite_members")
            || node.extra.contains_key("after_expansion")
        {
            return Err("computed topology fields are reserved to checkpoint expansions");
        }
        if node.deadline_seconds == Some(0) {
            return Err("node deadline_seconds must be positive");
        }
        if node.review.is_some()
            && (node.kind != Some(NodeKind::Plan)
                || node.plan.is_some()
                || node.access != AccessMode::ReadOnly
                || node.source.is_none())
        {
            return Err("review requires a read-only plan node with a source and no plan");
        }
        if node
            .review
            .as_ref()
            .is_some_and(|review| review.max_nodes == 0)
        {
            return Err("review max_nodes must be positive");
        }
        if node.review.is_none() {
            plans::validate_spec(node)?;
        }
        let is_composite = node.kind == Some(NodeKind::Composite);
        if is_composite != node.composite.is_some() {
            return Err("composite requires nodes and exports");
        }
        let is_loop = node.kind == Some(NodeKind::Loop);
        if is_loop != node.loop_body.is_some() {
            return Err("loop requires a body");
        }
        let is_decide = node.kind == Some(NodeKind::Decide);
        if is_decide != node.decide.is_some() {
            return Err("decide requires a question and options");
        }
        asks::validate_decide(
            &document.graph_id,
            node,
            document.decision_session.as_deref(),
        )?;
        let is_land = node.kind == Some(NodeKind::Land);
        if is_land != node.land.is_some() {
            return Err("land requires a land spec naming its writer");
        }
        land::validate_node(node)?;
        let is_gate = node.kind == Some(NodeKind::Gate);
        if is_gate != node.condition.is_some()
            || !is_gate && node.on_fail.is_some()
            || is_gate
                && (node.route.is_some()
                    || node.routes.is_some()
                    || node.access != AccessMode::ReadOnly
                    || node.delivery.is_some()
                    || node.worktree.is_some()
                    || !node.prompt.is_empty()
                    || node.required_verdict.is_some())
        {
            return Err("gate requires a condition and no agent configuration");
        }
        if let Some(target) = &node.on_fail
            && (target == &node.id
                || !positions
                    .get(target)
                    .is_some_and(|index| document.nodes[*index].depends_on.contains(&node.id)))
        {
            return Err("on_fail target must depend on its gate");
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
                    return Err("invalid verdict gate");
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
                    return Err("invalid CI gate check names");
                }
                ci::validate_target(target, node, document)?;
                if !(1..=604800).contains(timeout_seconds) || !node.inputs.is_empty() {
                    return Err("invalid CI gate target or timeout");
                }
            }
            Some(GateCondition::Budget { .. }) if !node.inputs.is_empty() => {
                return Err("budget gate has no selected inputs");
            }
            _ => {}
        }
        if let (Some(integration), Some(DeliveryMode::Integrate(target))) =
            (&document.integration_branch, &node.delivery)
            && integration != target
        {
            return Err("node integration target differs from graph integration branch");
        }
        if node.required_verdict.as_ref().is_some_and(|line| {
            line.trim().is_empty()
                || line.len() > limits.node_prompt_bytes
                || line.contains(['\n', '\r'])
        }) {
            return Err("required verdict must be a non-empty single line");
        }
        if let Some(title) = &node.title {
            provenant_core::validate_task_title(title)?;
        }
        if node
            .role
            .as_ref()
            .is_some_and(|role| !valid_identifier(role))
        {
            return Err("invalid role name");
        }
        let mut roots = std::collections::BTreeSet::new();
        if node.read_roots.len() > 32
            || node.read_roots.iter().any(|root| {
                root.is_empty() || root.len() > 4096 || root.contains('\0') || !roots.insert(root)
            })
        {
            return Err("invalid read roots");
        }
        match &node.source {
            Some(SourceBinding::Commit { commit })
                if commit.parse::<provenant_core::GitOid>().is_err() =>
            {
                return Err("source requires a full commit ID");
            }
            Some(SourceBinding::Node { node: source }) if !node.depends_on.contains(source) => {
                return Err("source node must be a direct dependency");
            }
            Some(SourceBinding::Integration) if document.integration_branch.is_none() => {
                return Err("integration source requires an integration branch");
            }
            _ => {}
        }
        if (is_gate || is_loop || is_composite || is_decide || is_land)
            && (node.source.is_some() || !node.read_roots.is_empty())
        {
            return Err("execution bindings require an agent node");
        }
        validate_access(node.access, node.worktree.as_deref())?;
        if node.access == AccessMode::ReadOnly
            && !matches!(node.delivery, None | Some(DeliveryMode::None))
        {
            return Err("read-only node cannot deliver");
        }
        if document.delivery == Some(DeliveryMode::Pr) && node.delivery == Some(DeliveryMode::Pr) {
            return Err("graph PR is exclusive with node PRs");
        }
        if !is_gate
            && !is_loop
            && !is_composite
            && !is_decide
            && !is_land
            && node.review.is_none()
            && node.prompt.is_empty()
            || plans::prompt(node).len() > limits.node_prompt_bytes
        {
            return Err("prompt size");
        }
        prompt_bytes = prompt_bytes
            .checked_add(node.content_bytes())
            .ok_or("prompt size")?;
        if prompt_bytes > limits.graph_prompt_bytes {
            return Err("total prompt size");
        }
        edge_count = edge_count
            .checked_add(node.dependencies().count())
            .ok_or("edge count")?;
        if edge_count > limits.edges {
            return Err("edge count");
        }

        let routes = match (&node.route, &node.routes) {
            (None, None)
                if is_gate
                    || is_loop
                    || is_composite
                    || is_decide
                    || is_land
                    || node.review.is_some() =>
            {
                Vec::new()
            }
            (Some(route), None) => vec![route.clone()],
            (None, Some(routes)) if !routes.is_empty() => routes.clone(),
            (Some(_), Some(_)) => return Err("route and routes are exclusive"),
            (None, None) | (None, Some(_)) => return Err("route list is empty"),
        };
        if routes.len() > limits.routes_per_node {
            return Err("route count");
        }
        if routes
            .iter()
            .any(|route| route.adapter.is_none() && route.alias.is_none())
        {
            return Err("route requires an adapter or alias");
        }
        if routes
            .iter()
            .any(|route| (route.model.is_some() || route.name.is_some()) && route.adapter.is_none())
        {
            return Err("model or named adapter route requires an adapter");
        }
        if routes.iter().any(|route| !route_is_bounded(route)) {
            return Err("route field size");
        }
        // Configured aliases resolve model providers, whose registry excludes host-native.
        for (index, route) in routes.iter().enumerate() {
            if route.adapter.as_deref() == Some("host-native")
                && !routes[index + 1..]
                    .iter()
                    .any(|fallback| fallback.adapter.as_deref() != Some("host-native"))
            {
                return Err("host-native route requires a later non-native fallback");
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
                    write: node.access == AccessMode::WorktreeWrite,
                    ..Default::default()
                })
            {
                return Err(error.field.refusal());
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for parent in node.dependencies() {
            if !seen.insert(parent) || parent == &node.id || !positions.contains_key(parent) {
                return Err("dependency");
            }
        }
        if node.kind == Some(NodeKind::Synthesise) && node.inputs.is_empty() {
            return Err("synthesise requires selected inputs");
        }
        let mut input_nodes = std::collections::BTreeSet::new();
        for input in &node.inputs {
            if input.extra.contains_key("required") {
                return Err("required output semantics are not supported");
            }
            if !node.depends_on.contains(&input.node)
                || !input_nodes.insert(&input.node)
                || input.outputs.is_empty()
            {
                return Err("input must select outputs from a unique direct dependency");
            }
            let mut kinds = Vec::new();
            for kind in &input.outputs {
                if kinds.contains(kind) {
                    return Err("duplicate input output");
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
                    return Err(
                        "required outputs must be unique selected commit, verdict or findings values",
                    );
                }
                required.push(*kind);
            }
        }
        let mut depends_on = node.depends_on.clone();
        depends_on.sort();
        nodes.push(PlannedNode {
            review: node.review.clone(),
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
            capabilities: node.capabilities.clone(),
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
                    NodeKind::Plan
                    | NodeKind::Synthesise
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
            require_findings: node.require_findings,
            skip_without_findings: node.skip_without_findings,
        });
    }

    let mut indegree = vec![0usize; nodes.len()];
    let mut children = vec![Vec::new(); nodes.len()];
    for (child, node) in nodes.iter().enumerate() {
        indegree[child] = node.dependencies().count();
        for parent in node.dependencies() {
            let parent_index = *positions.get(parent).ok_or("dependency")?;
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
        return Err("dependency cycle");
    }
    land::validate_relations(&nodes, &positions)?;

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
                    return Err("node requires both gate outcomes");
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

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn route_is_bounded(route: &GraphRoute) -> bool {
    fn field(value: &str, limit: usize) -> bool {
        !value.is_empty() && value.len() <= limit && !value.contains('\0')
    }
    route
        .adapter
        .as_deref()
        .is_none_or(|value| field(value, 128))
        && [
            route.model.is_some(),
            route.alias.is_some(),
            route.name.is_some(),
        ]
        .into_iter()
        .filter(|selected| *selected)
        .count()
            == 1
        && route.name.as_deref().is_none_or(|value| field(value, 128))
        && (route.name.is_none() || (route.effort.is_none() && route.account.is_none()))
        && route.model.as_deref().is_none_or(|value| field(value, 256))
        && route.alias.as_deref().is_none_or(|value| field(value, 128))
        && route.effort.as_deref().is_none_or(|value| field(value, 64))
        && route
            .account
            .as_deref()
            .is_none_or(|value| field(value, 64))
}

/// Why settled outputs lack the dispositions a node declares, if they do.
/// A findings block must parse, each finding needs a disposition from the
/// declared list with a non-empty reason, and every selected upstream finding
/// identity must appear exactly once.
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
    if checkpoint
        .instance_id
        .as_ref()
        .is_some_and(|id| id.parse::<provenant_core::RunId>().is_err())
    {
        return Err(GraphError::BadCheckpoint {
            node: plan.graph_id.clone(),
            invariant: "instance identity must be a run ID",
        });
    }
    if checkpoint
        .bound_review_base
        .iter()
        .chain(checkpoint.bound_integration_base.iter())
        .any(|base| base.parse::<provenant_core::GitOid>().is_err())
    {
        return Err(GraphError::BadCheckpoint {
            node: plan.graph_id.clone(),
            invariant: "graph bases must be full object IDs",
        });
    }
    if checkpoint.schema_version != 1
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
    for expansion in &checkpoint.expansions {
        if let Some(review) = &expansion.review {
            let progress = positions
                .get(&expansion.source)
                .map(|index| &checkpoint.nodes[*index])
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: expansion.source.clone(),
                    invariant: "review expansion must retain its source",
                })?;
            let expected = progress
                .review_base
                .as_deref()
                .or(checkpoint.bound_review_base.as_deref())
                .or(checkpoint.bound_integration_base.as_deref());
            if expected.is_some_and(|base| review.base.to_string() != base)
                || progress.review_base.as_deref().is_some_and(|base| {
                    expansion
                        .nodes
                        .iter()
                        .filter(|node| node.composite.is_none())
                        .any(|node| {
                            node.extra
                                .get("review_base")
                                .and_then(serde_json::Value::as_str)
                                != Some(base)
                        })
                })
            {
                return Err(GraphError::BadCheckpoint {
                    node: expansion.source.clone(),
                    invariant: "review expansion must retain its bound base",
                });
            }
        }
    }
    for (index, (progress, node)) in checkpoint.nodes.iter().zip(&plan.nodes).enumerate() {
        let bad = |invariant| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant,
        };
        for (invalid, invariant) in [
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
        } else if progress.gate.is_some() || progress.ci.is_some() {
            return Err(bad("only gate nodes may carry gate or CI evidence"));
        }
        if progress.state == NodeState::Skipped
            && (dependency_state(node, plan, checkpoint, &positions) != Some(NodeState::Skipped)
                && !loops::inactive_round(node, checkpoint, &positions)
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
                None => false,
            },
            // A planner whose failure was settled by its static fallback is
            // succeeded though its last attempt failed (or never launched).
            NodeState::Succeeded => {
                last.is_some_and(|attempt| {
                    attempt.state == AttemptState::Terminal
                        && attempt.outcome == Some(AttemptOutcome::Succeeded)
                }) || fallback_settled(node, progress, document, checkpoint)
                    || (node.review.is_some()
                        && progress.source_commit.as_ref().is_some_and(|subject| {
                            checkpoint.expansions.iter().any(|expansion| {
                                expansion.source == node.id
                                    && expansion.review.as_ref().is_some_and(|review| {
                                        review.subject.to_string() == *subject
                                    })
                            })
                        }))
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
    for expansion in &checkpoint.expansions {
        let Some(review) = &expansion.review else {
            continue;
        };
        let mut expected = Vec::new();
        for (index, member) in plan.nodes.iter().enumerate() {
            if review_panel_contains(expansion, member, plan, &positions) {
                for attempt in &checkpoint.nodes[index].attempts {
                    if attempt.review_substitution {
                        expected.push(ReviewSubstitution {
                            member: member.id.clone(),
                            route_index: attempt.key.route_index,
                        });
                    }
                }
            }
        }
        expected.sort();
        if review.substitutions != expected {
            return Err(GraphError::BadCheckpoint {
                node: expansion.source.clone(),
                invariant: "review substitutions must match member attempts",
            });
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
        && plans::fallback_expansion(&node.id, spec, document, node.require_skill_location)
            .is_ok_and(|derived| {
                checkpoint
                    .expansions
                    .iter()
                    .any(|expansion| expansion.source == node.id && *expansion == derived)
            })
}
