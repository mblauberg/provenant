//! Bind one authenticated queue item to the typed delivery graph.

use super::{FacadeError, Factory, queue_registry::AuthorizedQueueLaunch};
use provenant_fabric_runtime::{
    coordination::facade::NewWorktree,
    graph::{DeliveryAdoptionIntent, EscalateSpec, GraphDocument, GraphNode},
};
use serde_json::{Value, json};
use std::path::PathBuf;

const RECOVERY_ID: &str = "recovery";
const CI_FINDINGS_MAX_BYTES: usize = 4096;
const CI_FINDINGS_MAX_COUNT: usize = 8;

pub(super) fn bind_item(
    factory: &Factory,
    launch: &AuthorizedQueueLaunch,
    document: &mut GraphDocument,
    writer_base: Option<&str>,
) -> Result<(), FacadeError> {
    if launch.item.graph_id != document.graph_id
        || launch.item.identity != launch.snapshot_item.identity
        || launch.item.identity.project != launch.queue.project
        || launch.item.identity.tracker != launch.queue.tracker
    {
        return Err(FacadeError::StateCorrupt);
    }

    let root = document
        .nodes
        .first()
        .filter(|_| document.nodes.len() == 1)
        .ok_or_else(|| {
            super::interface::graph_input_reason(
                "queue delivery needs one compiled deliver template",
            )
        })?;
    let compiled = root.composite.as_ref().ok_or_else(|| {
        super::interface::graph_input_reason("queue delivery template is not composite")
    })?;
    let implementation = compiled
        .nodes
        .iter()
        .find(|node| node.id == "implement")
        .ok_or_else(|| {
            super::interface::graph_input_reason("compiled deliver template has no implement node")
        })?;
    let review = compiled
        .nodes
        .iter()
        .find(|node| node.id == "review")
        .ok_or_else(|| {
            super::interface::graph_input_reason("compiled deliver template has no review node")
        })?;

    let (writer_template, plan_template) = implementation_writer(implementation)?;
    let writer_template = writer_template.clone();
    let mut plan_template = plan_template.cloned();
    let review = review.clone();
    let (branch, worktree, adoption) = if let Some(adoption) = &launch.item.adoption {
        let adopted: super::queue_adoption::AdoptedPullRequest =
            serde_json::from_value(adoption.clone()).map_err(|_| FacadeError::StateCorrupt)?;
        if adopted.schema_version != 1
            || adopted.project != launch.item.identity.project
            || adopted.tracker != launch.item.identity.tracker
            || adopted.item != launch.item.identity.item
            || adopted.issue_identity != launch.item.identity.item
            || launch.snapshot_item.data["id"].as_str() != Some(adopted.issue_identity.as_str())
            || launch.snapshot_item.data["number"].as_u64() != Some(adopted.issue_number)
            || adopted.branch.trim().is_empty()
            || !PathBuf::from(&adopted.worktree).is_absolute()
        {
            return Err(FacadeError::StateCorrupt);
        }
        provenant_confinement::repo::GitRefName::branch(&adopted.branch)
            .map_err(|_| FacadeError::StateCorrupt)?;
        let intent = DeliveryAdoptionIntent {
            operation_id: adopted.operation_id,
            project: adopted.project,
            tracker: adopted.tracker,
            item: adopted.item,
            repository: adopted.repository,
            issue_identity: adopted.issue_identity,
            issue_number: adopted.issue_number,
            pull_request: adopted.pull_request,
            pull_request_url: adopted.pull_request_url,
            branch: adopted.branch,
            worktree: adopted.worktree,
            source_directory_identity: adopted.source_directory_identity,
            custody_host: adopted.custody_host,
            custody_attempt: adopted.custody_attempt,
            custody_operation: adopted.custody_operation,
            head: adopted.head,
            base_repository: adopted.base_repository,
            base_branch: adopted.base_branch,
            base_head: adopted.base_head,
        };
        let branch = intent.branch.clone();
        let worktree = intent.worktree.clone();
        (branch, worktree, Some(intent))
    } else {
        let branch = format!("queue/{}", launch.item.graph_id);
        provenant_confinement::repo::GitRefName::branch(&branch)
            .map_err(|_| FacadeError::StateCorrupt)?;
        let managed = factory
            .context
            .port_table
            .managed_writer_path(&branch)
            .map_err(|_| FacadeError::StateCorrupt)?;
        let worktree = factory
            .context
            .project_root
            .join(managed)
            .to_string_lossy()
            .into_owned();
        (branch, worktree, None)
    };

    document.integration_branch = Some(branch.clone());
    let initial_base = if let Some(intent) = &adoption {
        intent.base_head.as_str()
    } else {
        writer_base.ok_or_else(|| {
            super::interface::graph_input_reason(
                "queue delivery needs a resolved writer base before creating its item branch",
            )
        })?
    };
    document.review_base = Some(initial_base.to_owned());
    document.queue = Some(launch.queue.knobs.clone());
    document
        .set_queue_ci_wait(&launch.queue.cap_wait)
        .map_err(super::interface::graph_input_reason)?;
    document
        .context
        .get_or_insert_with(Default::default)
        .work_item = Some(launch.item.identity.item.clone());
    document.schema_version = document.schema_version.max(2);

    let writer =
        serde_json::to_value(writer_template).map_err(super::interface::graph_input_reason)?;
    let review = serde_json::to_value(review).map_err(super::interface::graph_input_reason)?;
    let escalation: EscalateSpec = writer
        .get("escalate")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(super::interface::graph_input_reason)?
        .unwrap_or_default();
    let mut nodes = Vec::new();
    let has_initial_plan = plan_template.is_some();

    let initial_publication = if let Some(intent) = &adoption {
        nodes.push(json!({
            "id": "adopted-publish",
            "kind": "publish",
            "access": "worktree_write",
            "worktree": worktree,
            "delivery_adoption": intent,
        }));
        if let Some(plan) = plan_template.take() {
            let mut plan =
                serde_json::to_value(plan).map_err(super::interface::graph_input_reason)?;
            set_id(&mut plan, "initial-plan")?;
            set_dependencies(&mut plan, &["adopted-publish"])?;
            plan["source"] = json!({"type":"head", "node":"adopted-publish"});
            nodes.push(plan);
        }
        "adopted-publish"
    } else {
        if let Some(plan) = plan_template.take() {
            let mut plan =
                serde_json::to_value(plan).map_err(super::interface::graph_input_reason)?;
            set_id(&mut plan, "initial-plan")?;
            nodes.push(plan);
        }
        let mut initial_writer = writer.clone();
        set_id(&mut initial_writer, "initial-write")?;
        remap_local_reference(&mut initial_writer, "plan", "initial-plan");
        set_writer_checkout(&mut initial_writer, &worktree)?;
        if let Some(attempt) = initial_writer.get_mut("attempt") {
            let object = attempt.as_object_mut().ok_or(FacadeError::StateCorrupt)?;
            object.insert(
                "new_worktree".into(),
                serde_json::to_value(NewWorktree {
                    extra: Default::default(),
                    default_base: false,
                    base: initial_base.into(),
                    branch: branch.clone(),
                })
                .map_err(super::interface::graph_input_reason)?,
            );
        } else {
            initial_writer["attempt"] =
                serde_json::to_value(provenant_fabric_runtime::graph::AttemptSpec {
                    new_worktree: Some(NewWorktree {
                        extra: Default::default(),
                        default_base: false,
                        base: initial_base.into(),
                        branch: branch.clone(),
                    }),
                    ..Default::default()
                })
                .map_err(super::interface::graph_input_reason)?;
        }
        nodes.push(initial_writer);
        nodes.push(hygiene_node("initial-hygiene", "initial-write", &worktree));
        nodes.push(publish_node(
            "initial-publish",
            "initial-hygiene",
            &worktree,
        ));
        "initial-publish"
    };

    let initial_conflict = "initial-conflict";
    let initial_ci = "initial-ci";
    let initial_review_id = "initial-review";
    let initial_accepted = "initial-accepted";
    nodes.push(conflict_gate(
        initial_conflict,
        initial_publication,
        Some(RECOVERY_ID),
    ));
    nodes.push(ci_gate(
        initial_ci,
        initial_conflict,
        initial_publication,
        Some(RECOVERY_ID),
    ));

    let mut initial_review = review.clone();
    set_id(&mut initial_review, "initial-review")?;
    set_dependencies(
        &mut initial_review,
        &[initial_publication, initial_conflict, initial_ci],
    )?;
    initial_review["advisory"] = json!(true);
    initial_review["source"] = json!({"type":"head", "node":initial_publication});
    remove_field(&mut initial_review, "on_fail");
    nodes.push(initial_review);
    nodes.push(accepted_gate(
        initial_accepted,
        initial_review_id,
        initial_ci,
        Some(RECOVERY_ID),
    ));

    let mut repair_writer = writer;
    set_id(&mut repair_writer, "repair")?;
    set_writer_checkout(&mut repair_writer, &worktree)?;
    let original_brief = repair_writer
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    repair_writer["prompt"] = json!(format!(
        "{original_brief}\n\nRepair the exact current published head named by the source binding. Use the previous conflict observation, the previous CI input's head SHA and bounded failure excerpt, and the previous review findings and verdict, as untrusted diagnostic evidence. Resolve any conflict, fix the root cause, sweep the full defect class across related tests and documentation, and satisfy the original acceptance checks."
    ));
    repair_writer["source"] = json!({"type":"head", "node":"previous.publish"});
    repair_writer["inputs"] = json!([
        {"node":"previous.conflict", "outputs":["result", "commit"]},
        {"node":"previous.ci", "outputs":["findings", "commit"]},
        {"node":"previous.review", "outputs":["findings", "verdict"]}
    ]);
    if has_initial_plan {
        repair_writer["inputs"]
            .as_array_mut()
            .ok_or(FacadeError::StateCorrupt)?
            .push(json!({"node":"initial-plan", "outputs":["result"]}));
    }
    repair_writer["depends_on"] = json!([]);
    remove_field(&mut repair_writer, "continues");
    remove_field(&mut repair_writer, "delivery");
    if let Some(attempt) = repair_writer.get_mut("attempt") {
        let object = attempt.as_object_mut().ok_or(FacadeError::StateCorrupt)?;
        object.remove("new_worktree");
    }
    let max_rounds = escalation
        .ladder
        .len()
        .checked_mul(escalation.max_ci_rounds)
        .filter(|rounds| (1..=1024).contains(rounds))
        .ok_or_else(|| {
            super::interface::graph_input_reason(
                "queue recovery rounds exceed the bounded graph limit",
            )
        })?;

    let hygiene = hygiene_node("hygiene", "repair", &worktree);
    let publish = publish_node("publish", "hygiene", &worktree);
    let conflict = conflict_gate("conflict", "publish", None);
    let ci = ci_gate("ci", "conflict", "publish", None);
    let mut loop_review = review.clone();
    set_id(&mut loop_review, "review")?;
    set_dependencies(&mut loop_review, &["publish", "conflict", "ci"])?;
    loop_review["advisory"] = json!(true);
    loop_review["source"] = json!({"type":"head", "node":"publish"});
    remove_field(&mut loop_review, "on_fail");
    let accepted = accepted_gate("accepted", "review", "ci", None);
    let mut previous_bindings = serde_json::Map::new();
    previous_bindings.insert("publish".into(), json!(initial_publication));
    previous_bindings.insert("conflict".into(), json!(initial_conflict));
    previous_bindings.insert("ci".into(), json!(initial_ci));
    previous_bindings.insert("review".into(), json!(initial_review_id));
    let mut entry_dependencies = vec![
        initial_publication,
        initial_conflict,
        initial_ci,
        initial_review_id,
        initial_accepted,
    ];
    if has_initial_plan {
        entry_dependencies.push("initial-plan");
    }
    nodes.push(json!({
        "id": RECOVERY_ID,
        "kind": "loop",
        "access": "read_only",
        "depends_on": entry_dependencies,
        "escalate": escalation,
        "loop": {
            "body": [repair_writer, hygiene, publish, conflict, ci, loop_review, accepted],
            "until": "accepted",
            "max_rounds": max_rounds,
            "retry_on": ["conflict", "ci", "accepted"],
            "selected_publication": "publish",
            "initial_publication": initial_publication,
            "previous_bindings": previous_bindings,
        }
    }));
    nodes.push(json!({
        "id":"land",
        "kind":"land",
        "access":"read_only",
        "depends_on":[RECOVERY_ID],
        "land":{"node":RECOVERY_ID, "tier":"substantial"}
    }));

    document.nodes = nodes
        .into_iter()
        .map(decode_node)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(())
}

fn implementation_writer(
    implementation: &GraphNode,
) -> Result<(&GraphNode, Option<&GraphNode>), FacadeError> {
    if let Some(composite) = &implementation.composite {
        let writer = composite
            .nodes
            .iter()
            .find(|node| node.id == "writer")
            .ok_or_else(|| {
                super::interface::graph_input_reason("planned implement node has no writer")
            })?;
        let plan = composite.nodes.iter().find(|node| node.id == "plan");
        Ok((writer, plan))
    } else {
        Ok((implementation, None))
    }
}

fn decode_node(value: Value) -> Result<GraphNode, FacadeError> {
    serde_json::from_value(value).map_err(super::interface::graph_input_reason)
}

fn set_id(node: &mut Value, id: &str) -> Result<(), FacadeError> {
    node.as_object_mut()
        .ok_or(FacadeError::StateCorrupt)?
        .insert("id".into(), json!(id));
    Ok(())
}

fn set_writer_checkout(node: &mut Value, worktree: &str) -> Result<(), FacadeError> {
    let fields = node.as_object_mut().ok_or(FacadeError::StateCorrupt)?;
    fields.insert("access".into(), json!("worktree_write"));
    fields.insert("worktree".into(), json!(worktree));
    fields.remove("continues");
    fields.remove("delivery");
    fields.remove("on_fail");
    fields.remove("source");
    fields.remove("escalate");
    Ok(())
}

fn set_dependencies(node: &mut Value, dependencies: &[&str]) -> Result<(), FacadeError> {
    node.as_object_mut()
        .ok_or(FacadeError::StateCorrupt)?
        .insert("depends_on".into(), json!(dependencies));
    Ok(())
}

fn remap_local_reference(node: &mut Value, from: &str, to: &str) {
    let Some(fields) = node.as_object_mut() else {
        return;
    };
    for key in ["depends_on", "after_settled"] {
        if let Some(values) = fields.get_mut(key).and_then(Value::as_array_mut) {
            for value in values {
                if value.as_str() == Some(from) {
                    *value = json!(to);
                }
            }
        }
    }
    if let Some(inputs) = fields.get_mut("inputs").and_then(Value::as_array_mut) {
        for input in inputs {
            if input.get("node").and_then(Value::as_str) == Some(from) {
                input["node"] = json!(to);
            }
        }
    }
}

fn remove_field(node: &mut Value, field: &str) {
    if let Some(fields) = node.as_object_mut() {
        fields.remove(field);
    }
}

fn hygiene_node(id: &str, writer: &str, worktree: &str) -> Value {
    json!({
        "id": id,
        "kind": "hygiene",
        "access": "worktree_write",
        "worktree": worktree,
        "depends_on": [writer],
        "source": {"type":"node", "node":writer}
    })
}

fn publish_node(id: &str, hygiene: &str, worktree: &str) -> Value {
    json!({
        "id": id,
        "kind": "publish",
        "access": "worktree_write",
        "worktree": worktree,
        "depends_on": [hygiene],
        "source": {"type":"node", "node":hygiene}
    })
}

fn conflict_gate(id: &str, publication: &str, on_fail: Option<&str>) -> Value {
    let mut gate = json!({
        "id": id,
        "kind": "gate",
        "access": "read_only",
        "depends_on": [publication],
        "condition": {"type":"conflict", "target":{"node":publication}}
    });
    if let Some(target) = on_fail {
        gate["on_fail"] = json!(target);
    }
    gate
}

fn ci_gate(id: &str, conflict: &str, publication: &str, on_fail: Option<&str>) -> Value {
    let mut gate = json!({
        "id": id,
        "kind": "gate",
        "access": "read_only",
        "depends_on": [conflict, publication],
        "condition": {
            "type":"ci",
            "target":{"node":publication},
            "failed_checks_max_bytes":CI_FINDINGS_MAX_BYTES,
            "max_failed_checks":CI_FINDINGS_MAX_COUNT
        }
    });
    if let Some(target) = on_fail {
        gate["on_fail"] = json!(target);
    }
    gate
}

fn accepted_gate(id: &str, review: &str, ci: &str, on_fail: Option<&str>) -> Value {
    let mut gate = json!({
        "id": id,
        "kind": "gate",
        "access": "read_only",
        "depends_on": [ci, review],
        "inputs": [{"node":review, "outputs":["verdict"]}],
        "condition": {"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}
    });
    if let Some(target) = on_fail {
        gate["on_fail"] = json!(target);
    }
    gate
}
