//! Static composites retain one settlement boundary and selected public outputs.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeSpec {
    /// Accept partial success: members that fail are recorded as unavailable
    /// and only successful members are exported. The composite itself fails
    /// when fewer than this many of its exporting members succeeded, capped at
    /// their count. Absent, a failed member blocks the composite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_successful: Option<usize>,
    /// Reduce successful exporting members' verdicts using the worst outcome.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reduce_verdict: bool,
    pub nodes: Vec<GraphNode>,
    pub exports: Vec<GraphInput>,
}

/// Expand only at the execution boundary. The submitted document remains nested.
pub(super) fn expand(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<GraphDocument, AdmissionError> {
    expand_document(document, limits).map_err(AdmissionError::Composite)
}

fn expand_document(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<GraphDocument, String> {
    let mut expanded = document.clone();
    let depth_bound = limits.expansion_depth_bound();
    let nodes_bound = crate::config::GraphLimits::composite_nodes_bound(limits.nodes);
    expanded.nodes = children(
        &document.nodes,
        "",
        &BTreeMap::new(),
        &[],
        &[],
        None,
        &[],
        &[],
        false,
        0,
        &depth_bound,
        &nodes_bound,
    )?;
    if expanded.nodes.iter().any(|node| node.composite.is_some())
        && let Some(reason) = nodes_bound.refusal(expanded.nodes.len())
    {
        return Err(reason);
    }
    Ok(expanded)
}

fn scoped(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_owned()
    } else {
        format!("{prefix}--{local}")
    }
}

fn reference(
    prefix: &str,
    name: &str,
    locals: &BTreeSet<&str>,
    incoming: &[String],
) -> Option<String> {
    if locals.contains(name) {
        return Some(scoped(prefix, name));
    }
    if incoming.iter().any(|parent| parent == name) {
        return Some(name.to_owned());
    }
    prefix
        .rsplit_once("--")
        .map(|(parent, _)| scoped(parent, name))
        .filter(|name| incoming.contains(name))
}

#[allow(
    clippy::too_many_arguments,
    reason = "bounded recursive composite expansion carries inherited execution bindings"
)]
fn children(
    nodes: &[GraphNode],
    prefix: &str,
    inherited_names: &BTreeMap<String, String>,
    incoming: &[String],
    inputs: &[GraphInput],
    source: Option<&SourceBinding>,
    read_roots: &[String],
    attachments: &[String],
    require_skill_location: bool,
    depth: usize,
    depth_bound: &crate::config::GraphBound<'_>,
    nodes_bound: &crate::config::GraphBound<'_>,
) -> Result<Vec<GraphNode>, String> {
    if let Some(reason) = depth_bound.refusal(depth) {
        return Err(reason);
    }
    let mut locals = BTreeSet::new();
    for node in nodes {
        if !locals.insert(node.id.as_str()) {
            return Err(format!(
                "expanded node id {:?} is duplicated; use distinct node names",
                bounded(&scoped(prefix, &node.id), 256)
            ));
        }
    }
    let mut visible_names = inherited_names.clone();
    visible_names.extend(
        nodes
            .iter()
            .map(|node| (node.id.clone(), scoped(prefix, &node.id))),
    );
    let composite_bound = depth > 0 || nodes.iter().any(|node| node.composite.is_some());
    let mut expanded = Vec::new();
    for original in nodes {
        let mut node = original.clone();
        node.require_skill_location |= require_skill_location;
        node.id = scoped(prefix, &node.id);
        if !valid_node_identifier(&node.id) {
            return Err(format!(
                "expanded node id {:?} is invalid; use at most 200 ASCII letters, digits, dots, underscores or hyphens; shorten the composite or node name",
                bounded(&node.id, 256)
            ));
        }
        for parent in &mut node.depends_on {
            *parent = reference(prefix, parent, &locals, incoming).ok_or("dependency")?;
        }
        for input in &mut node.inputs {
            if !input.node.starts_with("previous.") {
                // A loop body may select an outer node's outputs in every
                // round, which resolves through the enclosing scopes.
                input.node = reference(prefix, &input.node, &locals, incoming)
                    .or_else(|| {
                        prefix
                            .is_empty()
                            .then(|| visible_names.get(&input.node).cloned())
                            .flatten()
                    })
                    .unwrap_or_else(|| scoped(prefix, &input.node));
            }
        }
        if let Some(target) = &mut node.on_fail {
            *target = scoped(prefix, target);
        }
        if let Some(SourceBinding::Node { node: source }) = &mut node.source
            && !source.starts_with("previous.")
        {
            *source = reference(prefix, source, &locals, incoming)
                .unwrap_or_else(|| scoped(prefix, source));
        }
        if let Some(SourceBinding::Head { node: writer }) = &mut node.source {
            *writer = visible_names
                .get(writer)
                .cloned()
                .unwrap_or_else(|| writer.clone());
        }
        if let Some(GateCondition::Ci {
            target: CiTarget::Node { node: target, .. },
            ..
        }) = &mut node.condition
            && !target.starts_with("previous.")
        {
            *target = visible_names
                .get(target)
                .cloned()
                .unwrap_or_else(|| target.clone());
        }
        // A continued or landed writer may sit inside or outside the composite.
        for writer in node
            .continues
            .iter_mut()
            .chain(node.land.as_mut().map(|spec| &mut spec.node))
        {
            // Outer writers resolve through every enclosing scope, so a loop
            // body or nested composite can continue or land a writer that is
            // not one of its direct dependencies.
            *writer = reference(prefix, writer, &locals, incoming)
                .or_else(|| visible_names.get(writer.as_str()).cloned())
                .unwrap_or_else(|| scoped(prefix, writer));
        }
        if original
            .depends_on
            .iter()
            .all(|parent| !locals.contains(parent.as_str()))
        {
            for parent in incoming {
                if !node.depends_on.contains(parent) {
                    node.depends_on.push(parent.clone());
                }
            }
            for input in inputs {
                if let Some(own) = node.inputs.iter_mut().find(|own| own.node == input.node) {
                    for kind in &input.outputs {
                        if !own.outputs.contains(kind) {
                            own.outputs.push(*kind);
                        }
                    }
                    for kind in &input.required_outputs {
                        if !own.required_outputs.contains(kind) {
                            own.required_outputs.push(*kind);
                        }
                    }
                } else {
                    node.inputs.push(input.clone());
                }
            }
        }
        if node.source.is_none() {
            node.source = source.cloned();
            if let Some(SourceBinding::Node { node: source }) = &node.source
                && !node.depends_on.contains(source)
            {
                node.depends_on.push(source.clone());
            }
        }
        for root in read_roots {
            if !node.read_roots.contains(root) {
                node.read_roots.push(root.clone());
            }
        }
        for path in attachments {
            if !node.attachments.contains(path) {
                node.attachments.push(path.clone());
            }
        }
        if let Some(spec) = &mut node.loop_body {
            for child in &spec.body {
                if child.id.starts_with("previous.") {
                    return Err("previous. is reserved for prior round inputs".into());
                }
                if child.loop_body.is_some()
                    || child.kind == Some(NodeKind::Loop)
                    || child.on_fail.is_some()
                    || child.delivery.as_ref().is_some_and(|mode| {
                        !matches!(mode, provenant_records::run::DeliveryMode::Integrate(_))
                    })
                {
                    return Err("loop body cannot nest loops or route on_fail, and may only deliver by integrate:".into());
                }
            }
            spec.body = children(
                &spec.body,
                "",
                &visible_names,
                &[],
                &[],
                None,
                &[],
                &node.attachments,
                node.require_skill_location,
                depth,
                depth_bound,
                nodes_bound,
            )?;
        }
        if (node.kind == Some(NodeKind::Composite)) != node.composite.is_some() {
            return Err("composite requires nodes and exports".into());
        }
        if let Some(spec) = node.composite.take() {
            if spec.nodes.is_empty()
                || node.access != AccessMode::ReadOnly
                || node.route.is_some()
                || node.routes.is_some()
                || node.worktree.is_some()
                || node.delivery.is_some()
                || node.condition.is_some()
                || node.on_fail.is_some()
                || node.plan.is_some()
                || node.loop_body.is_some()
                || !node.prompt.is_empty()
                || node.required_verdict.is_some()
                || !node.write_scope.is_empty()
                || !node.acceptance_checks.is_empty()
            {
                return Err("composite cannot carry agent configuration".into());
            }
            let member_ids: BTreeSet<_> =
                spec.nodes.iter().map(|child| child.id.as_str()).collect();
            let mut selected = BTreeSet::new();
            let mut scalar = Vec::new();
            for export in &spec.exports {
                if !member_ids.contains(export.node.as_str())
                    || export.outputs.is_empty()
                    || !selected.insert(&export.node)
                {
                    return Err("composite export must select a unique direct child".into());
                }
                let mut kinds = Vec::new();
                for kind in &export.outputs {
                    if kinds.contains(kind) {
                        return Err("duplicate composite export output".into());
                    }
                    kinds.push(*kind);
                    if matches!(
                        kind,
                        OutputKind::Verdict
                            | OutputKind::Branch
                            | OutputKind::Commit
                            | OutputKind::Remaining
                    ) {
                        if scalar.contains(kind) {
                            return Err("composite scalar output has multiple sources".into());
                        }
                        scalar.push(*kind);
                    }
                }
            }
            let body = children(
                &spec.nodes,
                &node.id,
                &visible_names,
                &node.depends_on,
                &node.inputs,
                node.source.as_ref(),
                &node.read_roots,
                &node.attachments,
                node.require_skill_location,
                depth + 1,
                depth_bound,
                nodes_bound,
            )?;
            node.composite_members = spec
                .nodes
                .iter()
                .map(|child| scoped(&node.id, &child.id))
                .collect();
            node.after_expansion.extend(node.composite_members.clone());
            node.inputs.clear();
            node.source = None;
            node.read_roots.clear();
            node.attachments.clear();
            node.composite = Some(CompositeSpec {
                min_successful: spec.min_successful,
                reduce_verdict: spec.reduce_verdict,
                nodes: Vec::new(),
                exports: spec
                    .exports
                    .into_iter()
                    .map(|mut export| {
                        export.node = scoped(&node.id, &export.node);
                        export
                    })
                    .collect(),
            });
            expanded.extend(body);
        }
        expanded.push(node);
        if composite_bound && let Some(reason) = nodes_bound.refusal(expanded.len()) {
            return Err(reason);
        }
    }
    Ok(expanded)
}

fn outputs(
    spec: &CompositeSpec,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    legacy: bool,
) -> Result<(NodeOutputs, Option<NodeDecisionCause>), String> {
    let mut selected = NodeOutputs {
        extra: Default::default(),
        schema_version: 2,
        result: None,
        verdict: None,
        verdict_error: None,
        findings: Vec::new(),
        findings_available: None,
        branch: None,
        commit: None,
        warnings: Vec::new(),
        plan: None,
        remaining: None,
    };
    let mut results = Vec::new();
    let mut finding_members = Vec::new();
    let mut truncation_header = None;
    let mut inherited_notices = Vec::new();
    let mut family_notices = Vec::new();
    let mut truncation_warning_indices = Vec::new();
    let partial = spec.min_successful.is_some() || spec.reduce_verdict;
    let unavailable = unavailable_with_wording(spec, checkpoint, positions, legacy);
    let finding_exports: Vec<_> = spec
        .exports
        .iter()
        .filter(|export| export.outputs.contains(&OutputKind::Findings))
        .collect();
    if !finding_exports.is_empty() {
        selected.findings_available = Some(finding_exports.iter().all(|export| {
            positions.get(&export.node).is_some_and(|index| {
                let member = &checkpoint.nodes[*index];
                (!partial || member.state == NodeState::Succeeded)
                    && member
                        .outputs
                        .as_ref()
                        .and_then(|outputs| outputs.findings_available)
                        == Some(true)
            })
        }));
    }
    for export in &spec.exports {
        let member = &checkpoint.nodes[positions[&export.node]];
        // A partial composite exports only members that succeeded: a failed
        // member's output, such as a result without findings, is not evidence.
        if partial && member.state != NodeState::Succeeded {
            continue;
        }
        let Some(source) = member.outputs.as_ref() else {
            continue;
        };
        if member
            .separation
            .as_ref()
            .is_some_and(|separation| separation.outcome == SeparationOutcome::Unavailable)
        {
            let notice = bounded(
                &format!(
                    "{}: no separate model family was available; this review is not cross-family evidence",
                    export.node
                ),
                WARNING_BYTES,
            );
            if selected.warnings.len() < MAX_WARNINGS {
                selected.warnings.push(notice.clone());
            }
            family_notices.push(notice);
        }
        for warning in source
            .warnings
            .iter()
            .filter(|warning| warning.contains("malformed finding:"))
        {
            if selected.warnings.len() < MAX_WARNINGS {
                selected.warnings.push(bounded(
                    &format!("{}: {warning}", export.node),
                    WARNING_BYTES,
                ));
            }
        }
        for kind in &export.outputs {
            match kind {
                OutputKind::Result => {
                    if let Some(result) = &source.result {
                        results.push((&export.node, result));
                    }
                }
                OutputKind::Verdict => {
                    selected.verdict = source.verdict.clone();
                    selected.verdict_error = source.verdict_error.clone();
                }
                OutputKind::Findings => {
                    if !member.truncation_warnings().is_empty()
                        && let Some(notice) = member.truncation_notices().first()
                    {
                        inherited_notices.push((*notice).to_owned());
                    }
                    let findings =
                        materialised_findings(positions[&export.node], plan, checkpoint, positions);
                    if !findings.is_empty() {
                        finding_members.push((
                            export.node.clone(),
                            member
                                .attempts
                                .iter()
                                .rev()
                                .find_map(|attempt| attempt.attempt_ref.clone()),
                        ));
                    }
                    selected
                        .findings
                        .extend(findings.into_iter().map(|mut finding| {
                            finding.id = format!("{}:{}", export.node, finding.id);
                            finding
                        }))
                }
                OutputKind::Branch => selected.branch = source.branch.clone(),
                OutputKind::Commit => selected.commit = source.commit.clone(),
                OutputKind::Remaining => selected.remaining = source.remaining.clone(),
            }
        }
    }
    for entry in &unavailable {
        if selected.warnings.len() < MAX_WARNINGS {
            selected
                .warnings
                .push(bounded(&format!("unavailable {entry}"), WARNING_BYTES));
        }
    }
    if spec.reduce_verdict {
        use provenant_records::workflow::ReviewVerdict;
        let rank = |verdict| match verdict {
            ReviewVerdict::Approve => 0,
            ReviewVerdict::ApproveWithNits => 1,
            ReviewVerdict::Inconclusive => 2,
            ReviewVerdict::RequestChanges => 3,
        };
        let verdict = exporting_members(spec, positions)
            .into_iter()
            .map(|index| &checkpoint.nodes[index])
            .filter(|member| member.state == NodeState::Succeeded)
            .map(|member| {
                member
                    .outputs
                    .as_ref()
                    .and_then(|output| {
                        if output.verdict_error.is_some() {
                            return None;
                        }
                        output
                            .verdict
                            .as_deref()
                            .and_then(ReviewVerdict::from_token)
                    })
                    .unwrap_or(ReviewVerdict::Inconclusive)
            })
            .max_by_key(|verdict| rank(*verdict));
        selected.verdict = verdict.map(|verdict| format!("VERDICT: {}", verdict.as_token()));
        selected.verdict_error = None;
    }
    if selected
        .findings
        .iter()
        .any(|finding| finding.id.len() > OUTPUT_FIELD_BYTES)
    {
        return Err("composite exported findings exceed cap".into());
    }
    let total = selected.findings.len();
    if total > MAX_FINDINGS || canonical_len(&selected.findings) > FINDINGS_BYTES {
        selected.findings_available = Some(false);
        selected.findings = bounded_findings(std::mem::take(&mut selected.findings));
        let dropped = total - selected.findings.len();
        let members = finding_members
            .iter()
            .map(|(name, reference)| match reference {
                Some(reference) => format!("{name} (attempt {reference})"),
                None => name.clone(),
            })
            .collect::<Vec<_>>()
            .join("; ");
        let warning = format!(
            "composite findings truncated: {dropped} dropped; full member results in staged inputs: {members}"
        );
        let mut rest = warning.as_str();
        let mut notices = Vec::new();
        while !rest.is_empty() && notices.len() < MAX_WARNINGS {
            let part = bounded(rest, WARNING_BYTES);
            if part.is_empty() {
                break;
            }
            rest = &rest[part.len()..];
            notices.push(part);
        }
        if !rest.is_empty()
            && let Some(last) = notices.last_mut()
        {
            *last = format!(
                "{}... full members in composite result",
                bounded(last, WARNING_BYTES - 40)
            );
        }
        if !notices.is_empty() {
            truncation_warning_indices.push(0);
        }
        notices.extend(selected.warnings);
        notices.truncate(MAX_WARNINGS);
        selected.warnings = notices;
        truncation_header = Some(warning);
    }
    if !inherited_notices.is_empty() {
        selected.findings_available = Some(false);
        let inherited = inherited_notices.join("\n");
        for notice in &inherited_notices {
            if selected.warnings.len() < MAX_WARNINGS {
                truncation_warning_indices.push(selected.warnings.len());
                selected.warnings.push(bounded(notice, WARNING_BYTES));
            }
        }
        truncation_header = Some(match truncation_header {
            Some(own) => format!("{own}\n{inherited}"),
            None => inherited,
        });
    }
    selected.result = match results.as_slice() {
        [] => None,
        [(_, result)] => Some((*result).clone()),
        _ => {
            let mut excerpt = String::new();
            let mut truncated = false;
            for (name, result) in results {
                excerpt.push_str(&format!("{name}:\n{}\n", result.excerpt));
                truncated |= result.truncated;
            }
            let digest = CanonicalV1::file_digest(excerpt.as_bytes()).to_string();
            let bounded_excerpt = bounded(&excerpt, COMPOSITE_EXCERPT_BYTES);
            truncated |= bounded_excerpt.len() < excerpt.len();
            Some(ResultOutput {
                extra: Default::default(),
                digest,
                excerpt: bounded_excerpt,
                truncated,
                path: None,
            })
        }
    };
    let truncation_cause =
        truncation_header
            .as_ref()
            .map(|header| NodeDecisionCause::CompositeFindingsTruncated {
                warning_indices: truncation_warning_indices,
                notice_lines: bounded(header, COMPOSITE_EXCERPT_BYTES).lines().count(),
            });
    if !unavailable.is_empty() || truncation_header.is_some() || !family_notices.is_empty() {
        // The unavailable list leads and is bounded on its own, so a long
        // review body can never truncate it away.
        let mut headers = Vec::new();
        let header_limit = if truncation_header.is_some() {
            COMPOSITE_EXCERPT_BYTES
        } else {
            COMPOSITE_EXCERPT_BYTES / 2
        };
        if let Some(notice) = truncation_header {
            headers.push(notice);
        }
        if !family_notices.is_empty() {
            headers.push(family_notices.join("\n"));
        }
        if !unavailable.is_empty() {
            headers.push(format!(
                "Unavailable {}:\n{}",
                if legacy { "lenses" } else { "members" },
                unavailable.join("\n")
            ));
        }
        let full_header = headers.join("\n");
        let header = bounded(&full_header, header_limit);
        let header_truncated = header.len() < full_header.len();
        let (body, source_truncated) = selected
            .result
            .as_ref()
            .map_or((String::new(), false), |result| {
                (result.excerpt.clone(), result.truncated)
            });
        let room = COMPOSITE_EXCERPT_BYTES.saturating_sub(header.len() + 2);
        let kept = bounded(&body, room);
        let truncated = header_truncated || source_truncated || kept.len() < body.len();
        let combined = if kept.is_empty() {
            header
        } else {
            format!("{header}\n\n{kept}")
        };
        selected.result = Some(ResultOutput {
            extra: Default::default(),
            digest: CanonicalV1::file_digest(combined.as_bytes()).to_string(),
            truncated,
            excerpt: combined,
            path: None,
        });
    }
    Ok((selected, truncation_cause))
}

/// Reserve each member's strongest finding when all such reservations fit,
/// then fill remaining space in severity order. Ties preserve export order.
fn bounded_findings(findings: Vec<Finding>) -> Vec<Finding> {
    fn severity(value: &str) -> u8 {
        match value.to_ascii_lowercase().as_str() {
            "critical" | "blocker" | "blocking" | "p0" => 0,
            "high" | "error" | "p1" => 1,
            "medium" | "moderate" | "should-fix" | "p2" => 2,
            "low" | "warning" | "p3" => 3,
            "info" | "informational" | "note" | "p4" => 4,
            _ => 5,
        }
    }
    let mut order: Vec<usize> = (0..findings.len()).collect();
    order.sort_by_key(|&index| severity(&findings[index].severity));
    let mut members = BTreeSet::new();
    let mut reserved = Vec::new();
    for &index in &order {
        let member = findings[index]
            .id
            .split_once(':')
            .map_or("", |(member, _)| member);
        if members.insert(member) {
            reserved.push(index);
        }
    }
    let fits = |indices: &[usize]| {
        indices.len() <= MAX_FINDINGS
            && canonical_len(&indices.iter().map(|&i| &findings[i]).collect::<Vec<_>>())
                <= FINDINGS_BYTES
    };
    let mut kept = if fits(&reserved) {
        reserved
    } else {
        // A member's strongest finding may be much larger than another one.
        // Try its smallest finding before deciding that coverage cannot fit.
        let mut smallest: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, finding) in findings.iter().enumerate() {
            let member = finding.id.split_once(':').map_or("", |(member, _)| member);
            smallest
                .entry(member)
                .and_modify(|old| {
                    if canonical_len(finding) < canonical_len(&findings[*old]) {
                        *old = index;
                    }
                })
                .or_insert(index);
        }
        let mut compact: Vec<_> = smallest.into_values().collect();
        if fits(&compact) {
            for &index in &order {
                let member = findings[index]
                    .id
                    .split_once(':')
                    .map_or("", |(member, _)| member);
                if let Some(slot) = compact.iter().position(|&old| {
                    findings[old]
                        .id
                        .split_once(':')
                        .map_or("", |(name, _)| name)
                        == member
                        && severity(&findings[index].severity) < severity(&findings[old].severity)
                }) {
                    let old = compact[slot];
                    compact[slot] = index;
                    if !fits(&compact) {
                        compact[slot] = old;
                    }
                }
            }
            compact
        } else {
            Vec::new()
        }
    };
    for index in order {
        if !kept.contains(&index) {
            kept.push(index);
            if !fits(&kept) {
                kept.pop();
            }
        }
    }
    kept.sort_by_key(|&index| (severity(&findings[index].severity), index));
    kept.into_iter()
        .map(|index| findings[index].clone())
        .collect()
}

/// Distinct exporting members, in export order.
fn exporting_members(spec: &CompositeSpec, positions: &BTreeMap<String, usize>) -> Vec<usize> {
    let mut members: Vec<usize> = Vec::new();
    for export in &spec.exports {
        let member = positions[&export.node];
        if !members.contains(&member) {
            members.push(member);
        }
    }
    members
}

/// Failed exporting members as `id: reason`.
fn unavailable_with_wording(
    spec: &CompositeSpec,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    legacy: bool,
) -> Vec<String> {
    if spec.min_successful.is_none() && !spec.reduce_verdict {
        return Vec::new();
    }
    exporting_members(spec, positions)
        .into_iter()
        .map(|member| &checkpoint.nodes[member])
        .filter(|progress| {
            progress.state == NodeState::Failed
                || (spec.reduce_verdict
                    && matches!(progress.state, NodeState::Blocked | NodeState::Skipped))
        })
        .map(|progress| {
            let reason = progress.advance_error.as_deref().unwrap_or_else(|| {
                match progress.attempts.last().and_then(|a| a.outcome.as_ref()) {
                    Some(AttemptOutcome::AdapterUnavailable) => "adapter unavailable",
                    _ if legacy => "lens failed",
                    _ => "member failed",
                }
            });
            format!("{}: {reason}", progress.id)
        })
        .collect()
}

/// The recorded reason when a partial composite has too few successful members.
fn coverage_shortfall(
    spec: &CompositeSpec,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Option<String> {
    coverage_shortfall_with_wording(spec, checkpoint, positions, false)
}

fn coverage_shortfall_with_wording(
    spec: &CompositeSpec,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    legacy: bool,
) -> Option<String> {
    let members = exporting_members(spec, positions);
    let minimum = if spec.reduce_verdict {
        spec.min_successful.unwrap_or(members.len()).max(1)
    } else {
        spec.min_successful?.min(members.len())
    };
    let succeeded = members
        .iter()
        .filter(|member| checkpoint.nodes[**member].state == NodeState::Succeeded)
        .count();
    (succeeded < minimum).then(|| {
        format!(
            "coverage_below_minimum: {succeeded} of {} {} succeeded, minimum {minimum}; unavailable: {}",
            members.len(),
            if legacy { "lenses" } else { "members" },
            unavailable_with_wording(spec, checkpoint, positions, legacy).join("; ")
        )
    })
}

pub(super) fn settle<R: AttemptOwner, J: GraphJournal>(
    index: usize,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[index];
    let spec = node
        .composite
        .as_ref()
        .ok_or(GraphError::Invalid("composite boundary".into()))?;
    let mut state = state(node, plan, checkpoint, positions);
    let shortfall = (state == NodeState::Succeeded)
        .then(|| coverage_shortfall(spec, checkpoint, positions))
        .flatten();
    if shortfall.is_some() {
        state = NodeState::Failed;
    }
    let mut exported = None;
    let mut export_error = None;
    if state == NodeState::Succeeded {
        match outputs(spec, plan, checkpoint, positions, false) {
            Ok(value) => exported = Some(value),
            Err(reason) => {
                state = NodeState::Failed;
                export_error = Some(reason);
            }
        }
    }
    let (exported, truncation_cause) =
        exported.map_or((None, None), |(output, cause)| (Some(output), cause));
    let progress = &mut checkpoint.nodes[index];
    if progress.state != state
        || progress.outputs != exported
        || progress.truncation_cause != truncation_cause
    {
        progress.state = state;
        progress.outputs = exported;
        progress.truncation_cause = truncation_cause;
        if let Some(reason) = export_error {
            progress.advance_error_code = Some("composite_findings_exceed_cap".into());
            progress.advance_error = Some(reason);
        } else if let Some(reason) = shortfall {
            progress.advance_error_code = Some("coverage_below_minimum".into());
            progress.advance_error = Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
        }
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(())
}

fn state(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> NodeState {
    if node
        .composite
        .as_ref()
        .is_some_and(|spec| spec.min_successful.is_some() || spec.reduce_verdict)
    {
        // The group's own dependencies decide first; the rules below judge
        // its members, whose failures are the point of a partial group.
        let upstream: Vec<_> = node
            .depends_on
            .iter()
            .filter_map(|parent| {
                let index = positions[parent];
                let state = checkpoint.nodes[index].state;
                (!(plan.nodes[index].advisory
                    && matches!(state, NodeState::Failed | NodeState::Blocked)))
                .then_some(state)
            })
            .collect();
        if upstream
            .iter()
            .any(|state| matches!(state, NodeState::Failed | NodeState::Blocked))
        {
            return NodeState::Blocked;
        }
        if upstream.contains(&NodeState::Skipped) {
            return NodeState::Skipped;
        }
        let members: Vec<_> = node
            .composite_members
            .iter()
            .map(|child| checkpoint.nodes[positions[child]].state)
            .collect();
        if members
            .iter()
            .any(|state| matches!(state, NodeState::Pending | NodeState::Running))
        {
            return if members.iter().any(|state| *state != NodeState::Pending) {
                NodeState::Running
            } else {
                NodeState::Pending
            };
        }
        if !node
            .composite
            .as_ref()
            .is_some_and(|spec| spec.reduce_verdict)
            && members
                .iter()
                .any(|state| matches!(state, NodeState::Blocked | NodeState::Skipped))
        {
            return NodeState::Blocked;
        }
        // A review group can still produce a useful degraded receipt when no
        // member succeeds: the boundary exports the unavailable causes below.
        return NodeState::Succeeded;
    }
    match dependency_state(node, plan, checkpoint, positions) {
        Some(NodeState::Pending)
            if node
                .composite_members
                .iter()
                .any(|child| checkpoint.nodes[positions[child]].state != NodeState::Pending) =>
        {
            NodeState::Running
        }
        Some(state) => state,
        None => NodeState::Succeeded,
    }
}

pub(super) fn validate_progress(
    progress: &NodeProgress,
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Result<(), ()> {
    let state = state(node, plan, checkpoint, positions);
    let shortfall = node
        .composite
        .as_ref()
        .and_then(|spec| coverage_shortfall(spec, checkpoint, positions))
        .map(|reason| bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
    let failed_for_coverage = progress.state == NodeState::Failed
        && state == NodeState::Succeeded
        && progress.advance_error_code.as_deref() == Some("coverage_below_minimum")
        && (progress.advance_error == shortfall
            || node
                .composite
                .as_ref()
                .and_then(|spec| {
                    coverage_shortfall_with_wording(spec, checkpoint, positions, true)
                        .map(|reason| bounded(&reason, MAX_ADVANCE_ERROR_BYTES))
                })
                .is_some_and(|legacy| progress.advance_error.as_ref() == Some(&legacy)));
    let mut export = node
        .composite
        .as_ref()
        .map(|spec| outputs(spec, plan, checkpoint, positions, false));
    if progress
        .outputs
        .as_ref()
        .is_some_and(|outputs| outputs.findings_available.is_none())
        && let Some(Ok(expected)) = &mut export
    {
        // An older checkpoint retains unknown availability on replay.
        expected.0.findings_available = None;
    }
    let failed_for_export = progress.state == NodeState::Failed
        && state == NodeState::Succeeded
        && shortfall.is_none()
        && progress.advance_error_code.as_deref() == Some("composite_findings_exceed_cap")
        && matches!(&export, Some(Err(reason)) if progress.advance_error.as_ref() == Some(reason));
    let valid = progress.attempts.is_empty()
        && (progress.state == NodeState::Succeeded || progress.truncation_cause.is_none())
        && progress.gate.is_none()
        && progress.ci.is_none()
        && (progress.advance_error.is_none() || failed_for_coverage || failed_for_export)
        && progress.source_commit.is_none()
        && match progress.state {
            NodeState::Failed => {
                (failed_for_coverage || failed_for_export) && progress.outputs.is_none()
            }
            NodeState::Pending => progress.outputs.is_none(),
            NodeState::Running => state == NodeState::Running && progress.outputs.is_none(),
            NodeState::Succeeded => {
                state == NodeState::Succeeded && shortfall.is_none() && {
                    export.and_then(Result::ok).is_some_and(|(output, cause)| {
                        progress.outputs.as_ref() == Some(&output)
                            && progress.truncation_cause == cause
                    }) || node.composite.as_ref().is_some_and(|spec| {
                        outputs(spec, plan, checkpoint, positions, true)
                            .ok()
                            .is_some_and(|(output, cause)| {
                                progress.outputs.as_ref() == Some(&output)
                                    && progress.truncation_cause == cause
                            })
                    })
                }
            }
            NodeState::Blocked | NodeState::Skipped => {
                state == progress.state && progress.outputs.is_none()
            }
        };
    if valid { Ok(()) } else { Err(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_findings_reserves_members_then_prefers_severity() {
        let finding = |id: &str, severity: &str| {
            serde_json::from_value::<Finding>(serde_json::json!({
                "id": id, "severity": severity, "file": "a", "summary": "s", "status": "open"
            }))
            .unwrap()
        };
        let mut findings: Vec<_> = (0..40)
            .map(|i| finding(&format!("a:F{i}"), if i < 34 { "high" } else { "low" }))
            .collect();
        findings.push(finding("b:F1", "low"));
        let selected = bounded_findings(findings);
        assert_eq!(selected.len(), MAX_FINDINGS);
        assert!(selected.iter().any(|f| f.id == "b:F1"));
        assert_eq!(selected.iter().filter(|f| f.severity == "high").count(), 31);
        assert!(canonical_len(&selected) <= FINDINGS_BYTES);
    }
    #[test]
    fn member_coverage_uses_smaller_entries_when_strongest_do_not_fit() {
        let finding = |id: &str, severity: &str, summary: String| {
            serde_json::from_value::<Finding>(serde_json::json!({
                "id": id, "severity": severity, "file": "a", "summary": summary, "status": "open"
            }))
            .unwrap()
        };
        let mut findings = Vec::new();
        for member in 0..10 {
            findings.push(finding(
                &format!("m{member}:strong"),
                "high",
                "x".repeat(1000),
            ));
            findings.push(finding(&format!("m{member}:small"), "low", "s".into()));
        }
        let kept = bounded_findings(findings);
        assert!(canonical_len(&kept) <= FINDINGS_BYTES);
        for member in 0..10 {
            assert!(
                kept.iter()
                    .any(|f| f.id.starts_with(&format!("m{member}:")))
            );
        }
    }
}
