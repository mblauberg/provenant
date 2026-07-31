"""Lifecycle, checkpoint, and intent validation for delivery receipts."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from delivery_validation_common import (
    Invalid, NORMAL_STATES, SIDE_STATES, TRANSITIONS, _digest, _list,
    _mapping, _safe_path, _utc, fail,
)

def _validate_history(run: dict[str, Any]) -> None:
    history = _list(run.get("state_history"), "state_history")
    fail(not history, "state_history must be non-empty")
    fail(_mapping(history[0], "state_history[0]").get("state") != "draft", "state_history must start at draft")
    previous_state = None
    previous_at = None
    for index, raw in enumerate(history):
        item = _mapping(raw, f"state_history[{index}]")
        state = item.get("state")
        fail(state not in set(NORMAL_STATES) | SIDE_STATES, f"unknown state at history {index}")
        at = _utc(item.get("at"), f"state_history[{index}].at")
        _list(item.get("evidence_ids"), f"state_history[{index}].evidence_ids")
        fail(previous_at is not None and at <= previous_at, "state history timestamps must increase")
        if state in SIDE_STATES:
            for field in ("reason", "recovery", "resume_state"):
                fail(not item.get(field), f"side state {state} requires {field}")
            fail(item.get("resume_state") != previous_state, f"side state {state} must resume the state it interrupted")
            fail(previous_state is None or previous_state in SIDE_STATES, f"side state {state} requires a normal from-state")
        elif previous_state is not None:
            if previous_state in SIDE_STATES:
                previous_item = _mapping(history[index - 1], f"state_history[{index - 1}]")
                fail(state != previous_item.get("resume_state"), f"invalid lifecycle recovery {previous_state} -> {state}")
            else:
                fail(state not in TRANSITIONS.get(previous_state, set()), f"invalid lifecycle transition {previous_state} -> {state}")
        previous_state, previous_at = state, at
    fail(history[-1].get("state") != run.get("status"), "status must equal the final state history entry")
    repair_count = sum(_mapping(item, "state history item").get("state") == "repairing" for item in history)
    fail(run.get("repair_cycles") != repair_count, "repair_cycles must equal repairing transitions in state_history")
    if run.get("status") in SIDE_STATES:
        degradation = _mapping(run.get("degradation"), "degradation")
        fail(not degradation.get("reason") or not degradation.get("recovery"), "side state requires reason and recovery")
        if run.get("status") == "degraded":
            fail(degradation.get("kind") not in {"kernel_degraded", "runtime_degraded"}, "degraded run requires a typed degradation kind")
            if degradation.get("kind") == "kernel_degraded":
                fail(not degradation.get("fallback_skill"), "kernel_degraded requires the specialised fallback skill")


def _validate_checkpoint(
    run: dict[str, Any], artifacts: dict[str, dict[str, Any]], *,
    receipt_dir: Path | None, workspace_root: Path | None,
) -> None:
    checkpoint = _mapping(run.get("checkpoint"), "checkpoint")
    fail(set(checkpoint) != {"generation", "current_slice", "next_action", "in_flight", "artifact_paths"}, "checkpoint fields are invalid")
    generation = checkpoint.get("generation")
    fail(isinstance(generation, bool) or not isinstance(generation, int) or generation < 0, "checkpoint.generation must be non-negative")
    fail(not checkpoint.get("current_slice") or not checkpoint.get("next_action"), "checkpoint requires current_slice and next_action")
    for field in ("in_flight", "artifact_paths"):
        values = _list(checkpoint.get(field), f"checkpoint.{field}")
        fail(any(not isinstance(value, str) or not value for value in values), f"checkpoint.{field} values must be strings")
    for path in checkpoint["artifact_paths"]:
        _safe_path(path, "checkpoint.artifact_paths")
    required_slices = {
        "awaiting_acceptance": "awaiting-acceptance",
        "accepted": "accepted",
        "awaiting_release": "awaiting-release",
        "observing": "observing",
        "closed": "closed",
    }
    expected_slice = required_slices.get(run.get("status"))
    fail(
        expected_slice is not None and checkpoint.get("current_slice") != expected_slice,
        f"checkpoint.current_slice must be {expected_slice} while status is {run.get('status')}",
    )
    fail(run.get("status") == "closed" and bool(checkpoint["in_flight"]),
         "closed checkpoint must not retain in-flight work")
    declared_paths = {item.get("path") for item in artifacts.values() if item.get("path")}
    roots = [root.resolve() for root in (receipt_dir, workspace_root) if root is not None]
    for path in checkpoint["artifact_paths"]:
        if path == "RUN.json" or path in declared_paths:
            continue
        live = False
        for root in roots:
            target = (root / path).resolve()
            try:
                target.relative_to(root)
            except ValueError:
                continue
            if target.is_file():
                live = True
                break
        fail(not live, f"checkpoint artifact {path} must be declared or live inside the run/workspace root")


def _validate_intent_design(run: dict[str, Any], artifacts: dict[str, dict[str, Any]], evidence: dict[str, dict[str, Any]]) -> None:
    intent = _mapping(run.get("intent"), "intent")
    approval = _mapping(intent.get("approval"), "intent.approval")
    fail(approval.get("status") != "approved", "intent approval must be approved")
    for field in ("artifact", "digest", "decision_owner"):
        fail(not intent.get(field), f"intent.{field} is required")
    for field in ("approver", "evidence"):
        fail(not approval.get(field), f"intent approval {field} is required")
    approval_evidence = evidence.get(approval.get("evidence"))
    fail(not approval_evidence or approval_evidence.get("kind") != "human" or approval_evidence.get("status") != "pass" or approval_evidence.get("gate") != "intent-approval", "intent approval must link matching passing human evidence")
    _digest(intent.get("digest"), "intent.digest")
    matching = [item for item in artifacts.values() if item.get("path") == intent.get("artifact") or item.get("uri") == intent.get("artifact")]
    fail(not matching or matching[0].get("digest") != intent.get("digest"), "intent digest must bind a declared artifact")

    risk = run.get("risk_tier")
    design = _mapping(run.get("design"), "design")
    if risk in {"substantial", "crucial", "terminal"}:
        fail(design.get("status") != "approved", "substantial+ design must be approved")
        for field in ("artifact_id", "digest", "approver", "evidence"):
            fail(not design.get(field), f"design.{field} is required")
        bound = artifacts.get(design.get("artifact_id"))
        fail(not bound or bound.get("digest") != design.get("digest"), "design digest must bind its artifact")
        design_evidence = evidence.get(design.get("evidence"))
        fail(not design_evidence or design_evidence.get("kind") != "human" or design_evidence.get("status") != "pass" or design_evidence.get("gate") != "design-approval", "design approval must link matching passing human evidence")
    if risk in {"crucial", "terminal"}:
        fail(not design.get("alternatives"), "crucial design requires alternatives")
        fail(not design.get("failure_analysis"), "crucial design requires failure analysis")
        fail(not design.get("containment"), "crucial design requires containment")
        doors = _list(design.get("one_way_doors"), "design.one_way_doors")
        for index, raw in enumerate(doors):
            door = _mapping(raw, f"design.one_way_doors[{index}]")
            fail(not door.get("id") or not door.get("decision"), f"one-way door {index} requires id and decision")
            fail(door.get("classification") != "design-decision", f"one-way door {index} cannot be an implementation detail")
            fail(door.get("status") not in {"resolved", "deferred"}, f"one-way door {index} is unresolved")
            fail(not door.get("evidence"), f"one-way door {index} requires decision evidence")
            linked = evidence.get(door.get("evidence"))
            fail(not linked or linked.get("kind") != "human" or linked.get("status") != "pass" or linked.get("gate") != f"one-way-door:{door.get('id')}", f"one-way door {index} must link matching passing human evidence")
            if door.get("status") == "deferred":
                fail(not door.get("approved_by") or not door.get("reason"), f"deferred one-way door {index} requires human approval and reason")



