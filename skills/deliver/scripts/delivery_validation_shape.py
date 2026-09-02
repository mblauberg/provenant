"""Checkpoint, intent and design validation for the flat delivery receipt."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from delivery_validation_common import (
    Invalid, _digest, _list, _mapping, _safe_path, _utc, fail,
)


def _validate_checkpoint(
    run: dict[str, Any], artifacts: dict[str, dict[str, Any]], *,
    receipt_dir: Path | None, workspace_root: Path | None, closed: bool,
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
    fail(closed and bool(checkpoint["in_flight"]),
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
