#!/usr/bin/env python3
"""Validate the canonical domain-neutral delivery lifecycle receipt."""

from __future__ import annotations

import argparse
from datetime import datetime
from functools import lru_cache
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import re
import sys
from typing import Any

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "skills"))
from _shared.review_ladder import PRIMARY_FAMILIES, check_review_ladder

POLICY_VALIDATION_PATH = Path(__file__).with_name("delivery_policy_validation.py")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
SAFE_CLASSES = {"canonical", "evidence", "handoff", "scratch", "external"}
REVIEW_ROLES = {"targeted", "other-primary", "distinct-family"}
RISKS = ("routine", "substantial", "crucial", "terminal")
REPAIR_BUDGETS = {"routine": 2, "substantial": 4, "crucial": 5, "terminal": 5}
NORMAL_STATES = (
    "draft", "scoped", "approved", "executing", "verifying", "reviewing",
    "repairing", "awaiting_acceptance", "accepted", "awaiting_release",
    "observing", "closed",
)
SIDE_STATES = {"blocked", "cancelled", "degraded"}
TRANSITIONS = {
    "draft": {"scoped"},
    "scoped": {"approved"},
    "approved": {"executing"},
    "executing": {"verifying"},
    "verifying": {"reviewing", "executing"},
    "reviewing": {"repairing", "awaiting_acceptance"},
    "repairing": {"verifying"},
    "awaiting_acceptance": {"accepted", "repairing"},
    "accepted": {"awaiting_release"},
    "awaiting_release": {"observing"},
    "observing": {"closed"},
    "closed": set(),
}
AGENTIC_RISKS = {
    "goal-hijack", "tool-misuse", "excessive-privilege", "supply-chain",
    "code-execution", "memory-context-poisoning", "insecure-inter-agent-communication",
    "cascading-failures", "human-trust-exploitation",
}
EVALUATION_BINDING_FIELDS = {
    "status", "anchored_at", "evidence_id", "evaluation_artifact_id",
    "evaluation_id", "evaluation_digest", "plan_digest",
}

class Invalid(ValueError):
    pass

def fail(condition: bool, message: str) -> None:
    if condition:
        raise Invalid(message)

def _mapping(value: Any, field: str) -> dict[str, Any]:
    fail(not isinstance(value, dict), f"{field} must be an object")
    return value

def _list(value: Any, field: str) -> list[Any]:
    fail(not isinstance(value, list), f"{field} must be a list")
    return value

def _utc(value: Any, field: str) -> datetime:
    fail(not isinstance(value, str) or not value.endswith("Z"), f"{field} must be a UTC timestamp")
    try:
        return datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise Invalid(f"{field} must be an ISO UTC timestamp") from exc

def _digest(value: Any, field: str) -> None:
    fail(not isinstance(value, str) or not DIGEST.fullmatch(value), f"{field} must be a sha256 digest")

def _identifier(value: Any, field: str) -> str:
    fail(
        not isinstance(value, str) or not IDENTIFIER.fullmatch(value),
        f"{field} must be a bounded stable identifier",
    )
    return value

def _safe_path(value: Any, field: str) -> str:
    fail(not isinstance(value, str) or not value, f"{field} must be a non-empty path")
    path = Path(value)
    fail(path.is_absolute() or ".." in path.parts, f"{field} must be safe and relative")
    return path.as_posix().rstrip("/")

def _inside(path: str, scope: str) -> bool:
    return scope in {"", "."} or path == scope or path.startswith(scope + "/")

@lru_cache(maxsize=1)
def _policy_validation_module():
    spec = importlib.util.spec_from_file_location(
        "delivery_policy_validation", POLICY_VALIDATION_PATH,
    )
    fail(spec is None or spec.loader is None, "delivery policy validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

def _retrospect_validator():
    path = ROOT / "skills" / "retrospect" / "scripts" / "validate_retrospect.py"
    spec = importlib.util.spec_from_file_location("delivery_retrospect_validator", path)
    fail(not spec or not spec.loader, "retrospective validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

def _evaluate_validator():
    path = ROOT / "skills" / "evaluate" / "scripts" / "validate_evaluation.py"
    spec = importlib.util.spec_from_file_location("delivery_evaluate_validator", path)
    fail(not spec or not spec.loader, "evaluation validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    fail(not callable(getattr(module, "validate", None)), "evaluation validator API is unavailable")
    return module


@lru_cache(maxsize=1)
def _software_delivery_validator():
    spec = importlib.util.spec_from_file_location("software_delivery_validation", Path(__file__).with_name("software_delivery_validation.py"))
    fail(not spec or not spec.loader, "software delivery validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_bound_json(raw: bytes, field: str) -> dict[str, Any]:
    def no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            fail(key in result, f"{field} contains duplicate JSON key: {key}")
            result[key] = value
        return result

    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=no_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise Invalid(f"{field} is not readable JSON: {exc}") from exc
    fail(not isinstance(value, dict), f"{field} root must be an object")
    return value


def _validate_artifacts(
    artifacts: list[Any], *, workspace_root: Path | None, verify_hashes: bool,
    allowed_artifact_paths: list[str], allowed_source_paths: list[str],
    profile: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    by_id: dict[str, dict[str, Any]] = {}
    for index, raw in enumerate(artifacts):
        item = _mapping(raw, f"artifacts[{index}]")
        artifact_id = item.get("id")
        fail(not isinstance(artifact_id, str) or not artifact_id or artifact_id in by_id, f"artifact {index} id is missing or duplicate")
        path = item.get("path")
        uri = item.get("uri")
        path_present = "path" in item
        revision_present = "git_revision" in item
        _software_delivery_validator().validate_git_artifact(
            item, artifact_id, path, uri, workspace_root, allowed_source_paths,
            verify_hashes, _safe_path, _inside, Invalid,
        )
        if path_present:
            clean_path = _safe_path(path, f"artifact {artifact_id}.path")
            fail(not any(_inside(clean_path, scope) for scope in allowed_artifact_paths), f"artifact {artifact_id} is outside authority.allowed_artifact_paths")
        fail(not item.get("media_type"), f"artifact {artifact_id} requires media_type")
        fail(item.get("class") not in SAFE_CLASSES, f"artifact {artifact_id} has invalid class")
        artifact_type = item.get("artifact_type")
        fail(not isinstance(artifact_type, str) or not artifact_type, f"artifact {artifact_id} requires artifact_type")
        if item.get("class") == "canonical":
            fail(artifact_type not in profile["artifact_types"], f"canonical artifact {artifact_id} type is outside the selected profile")
        else:
            expected_type = {"evidence": "evidence", "handoff": "handoff", "scratch": "scratch", "external": "external-reference"}[item["class"]]
            fail(artifact_type != expected_type, f"artifact {artifact_id} type does not match its class")
        fail(not item.get("owner") or not item.get("retention"), f"artifact {artifact_id} requires owner and retention")
        if item.get("class") == "evidence":
            fail(item.get("retention") not in profile["evidence_policy"]["retention"], f"evidence artifact {artifact_id} retention violates the profile policy")
        digest = item.get("digest")
        _software_delivery_validator().validate_integrity_shape(
            item, artifact_id, revision_present, path_present, _digest, fail)
        if path_present and verify_hashes:
            fail(workspace_root is None, "verify_hashes requires workspace_root")
            target = workspace_root / path
            try:
                target.resolve().relative_to(workspace_root.resolve())
            except ValueError as exc:
                raise Invalid(f"artifact {artifact_id} resolves outside workspace_root") from exc
            fail(not target.is_file(), f"artifact {artifact_id} path does not exist")
            actual = "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
            fail(actual != digest, f"artifact {artifact_id} digest does not match live bytes")
        by_id[artifact_id] = item
    fail(not by_id, "at least one artifact is required")
    fail(not any(item.get("class") == "canonical" for item in by_id.values()), "profile requires a canonical outcome artifact")
    return by_id


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


def _validate_evidence(
    run: dict[str, Any], profile: dict[str, Any], artifacts: dict[str, dict[str, Any]],
    required_kinds: set[str], allowed_source_paths: list[str], *,
    artifact_root: Path | None, verify_hashes: bool,
) -> dict[str, dict[str, Any]]:
    by_id: dict[str, dict[str, Any]] = {}
    for index, raw in enumerate(_list(run.get("evidence"), "evidence")):
        item = _mapping(raw, f"evidence[{index}]")
        evidence_id = item.get("id")
        fail(not isinstance(evidence_id, str) or not evidence_id or evidence_id in by_id, f"evidence {index} id is missing or duplicate")
        fail(item.get("kind") not in {"deterministic", "judgement", "human", "observation"}, f"evidence {evidence_id} kind is invalid")
        fail(item.get("status") not in {"pass", "fail", "unavailable", "not_applicable"}, f"evidence {evidence_id} status is invalid")
        fail(not item.get("gate") or not item.get("method"), f"evidence {evidence_id} requires gate and method")
        fail(item.get("artifact_id") not in artifacts, f"evidence {evidence_id} must link an artifact")
        source_paths = [_safe_path(path, f"evidence {evidence_id}.source_paths") for path in _list(item.get("source_paths"), f"evidence {evidence_id}.source_paths")]
        if item.get("kind") != "human":
            fail(not source_paths, f"evidence {evidence_id} requires source_paths")
        fail(any(not any(_inside(path, scope) for scope in allowed_source_paths) for path in source_paths), f"evidence {evidence_id} reads outside authority.allowed_source_paths")
        if item.get("kind") == "judgement":
            lineage = _mapping(item.get("model_lineage"), f"evidence {evidence_id}.model_lineage")
            fail(not lineage.get("adapter") or not lineage.get("provider_family") or not lineage.get("model"), f"judgement evidence {evidence_id} requires model lineage")
        if item.get("kind") == "deterministic":
            result = _mapping(item.get("result"), f"evidence {evidence_id}.result")
            exit_code = result.get("exit_code")
            fail(isinstance(exit_code, bool) or not isinstance(exit_code, int), f"deterministic evidence {evidence_id} requires integer exit_code")
            _digest(result.get("receipt_digest"), f"evidence {evidence_id}.result.receipt_digest")
            declared_artifact = artifacts[item["artifact_id"]]
            fail(
                declared_artifact.get("digest") != result.get("receipt_digest"),
                f"deterministic evidence {evidence_id} receipt digest must bind its declared artifact",
            )
            fail((item.get("status") == "pass") != (exit_code == 0), f"deterministic evidence {evidence_id} status disagrees with its result")
        if item.get("kind") == "observation":
            _utc(item.get("observed_at"), f"evidence {evidence_id}.observed_at")
            measured = item.get("measured_value")
            fail(isinstance(measured, bool) or not isinstance(measured, (int, float)) or not math.isfinite(measured), f"observation evidence {evidence_id} requires a finite measured_value")
        by_id[evidence_id] = item
    if verify_hashes:
        fail(artifact_root is None, "deterministic evidence verification requires an artifact root")
        evaluation_artifact_ids = {
            item.get("evaluation_artifact_id")
            for item in _list(_mapping(run.get("assurance"), "assurance").get("evaluations"), "assurance.evaluations")
            if isinstance(item, dict) and item.get("evaluation_artifact_id")
        }
        for artifact_id in {
            item["artifact_id"] for item in by_id.values()
            if item.get("kind") == "deterministic"
        } - evaluation_artifact_ids:
            artifact = artifacts[artifact_id]
            fail(
                artifact.get("artifact_type") != "evidence"
                or artifact.get("media_type") != "application/json"
                or not artifact.get("path") or artifact.get("uri"),
                f"deterministic evidence artifact {artifact_id} must be a local JSON evidence bundle",
            )
            target = artifact_root / artifact["path"]
            try:
                bundle = json.loads(target.read_text())
            except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise Invalid(f"deterministic evidence artifact {artifact_id} must contain valid bundle JSON") from exc
            bundle = _mapping(bundle, f"deterministic evidence artifact {artifact_id}")
            fail(
                set(bundle) != {"schema_version", "contract", "checks"}
                or bundle.get("schema_version") != 1
                or bundle.get("contract") != "deterministic-evidence-bundle",
                f"deterministic evidence artifact {artifact_id} has an invalid bundle contract",
            )
            checks: dict[str, dict[str, Any]] = {}
            for check_index, raw_check in enumerate(_list(bundle.get("checks"), f"deterministic evidence artifact {artifact_id}.checks")):
                check = _mapping(raw_check, f"deterministic evidence artifact {artifact_id}.checks[{check_index}]")
                fail(
                    set(check) != {"id", "gate", "status", "method", "source_paths", "exit_code"}
                    or not isinstance(check.get("id"), str) or check["id"] in checks,
                    f"deterministic evidence artifact {artifact_id} has an invalid or duplicate check",
                )
                checks[check["id"]] = check
            linked = {
                item["id"]: item for item in by_id.values()
                if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact_id
            }
            fail(set(checks) != set(linked), f"deterministic evidence artifact {artifact_id} check set does not match its evidence rows")
            for evidence_id, item in linked.items():
                check = checks[evidence_id]
                fail(
                    check != {
                        "id": evidence_id,
                        "gate": item["gate"],
                        "status": item["status"],
                        "method": item["method"],
                        "source_paths": item["source_paths"],
                        "exit_code": item["result"]["exit_code"],
                    },
                    f"deterministic evidence artifact {artifact_id} check {evidence_id} does not match its evidence row",
                )
    required_evidence = _policy_validation_module().profile_evidence_requirements(profile, artifacts)
    for kind, gates in required_evidence.items():
        if kind not in required_kinds:
            continue
        for gate in gates:
            matches = [item for item in by_id.values() if item.get("gate") == gate and item.get("status") == "pass"]
            fail(not matches or any(item.get("kind") != kind for item in matches), f"profile gate {gate} requires passing {kind} evidence")
    return by_id

def _validate_reviews(run: dict[str, Any], evidence: dict[str, dict[str, Any]], *, required: bool) -> None:
    reviews = []
    for index, raw in enumerate(_list(run.get("reviews"), "reviews")):
        item = _mapping(raw, f"reviews[{index}]")
        fail(item.get("status") not in {"pass", "failed", "unavailable", "skipped"}, f"review {index} status is invalid")
        fail(not item.get("provider_family") or not item.get("adapter") or not item.get("role"), f"review {index} lacks lineage")
        fail(item.get("role") not in REVIEW_ROLES, f"review {index} role is invalid")
        fail(item.get("independent_of_authorship") is not True, f"review {index} is not independent")
        fail(not item.get("lenses"), f"review {index} requires lenses")
        if item.get("status") == "pass":
            fail(not item.get("model"), f"passing review {index} requires actual model identity")
            fail(item.get("evidence_id") not in evidence, f"review {index} must link evidence")
            linked = evidence[item["evidence_id"]]
            fail(linked.get("status") != "pass" or linked.get("kind") != "judgement", f"passing review {index} must link passing judgement evidence")
            lineage = _mapping(linked.get("model_lineage"), f"review {index} evidence lineage")
            fail(
                lineage.get("adapter") != item.get("adapter")
                or lineage.get("provider_family") != item.get("provider_family")
                or lineage.get("model") != item.get("model"),
                f"review {index} lineage does not match its evidence",
            )
        else:
            fail(not item.get("reason"), f"non-passing review {index} requires reason")
        reviews.append(item)
    optional = [item for item in reviews if item.get("role") == "distinct-family"]
    fail(any(item.get("provider_family") in {"openai", "anthropic"} for item in optional), "distinct-family review must use a non-primary family")
    if not required:
        return
    chair_family = run.get("chair_family")
    legs = []
    for item in reviews:
        role = item.get("role")
        ladder_role = "other-primary" if role == "other-primary" else "distinct-family" if role == "distinct-family" else "targeted"
        legs.append({
            "role": ladder_role,
            "family": item.get("provider_family"),
            # The delivery-run schema calls an intentional omission "skipped";
            # normalize it to the shared review-plan ladder vocabulary.
            "status": "omitted" if item.get("status") == "skipped" else item.get("status"),
            "lenses": item.get("lenses", []),
            "reason": item.get("reason"),
        })
    ladder_errors = check_review_ladder(run.get("risk_tier"), legs, chair_family=chair_family)
    if ladder_errors:
        raise Invalid(ladder_errors[0])

def _validate_security(run: dict[str, Any], registry: dict[str, Any], profile: dict[str, Any], artifacts: dict[str, dict[str, Any]], evidence: dict[str, dict[str, Any]], *, required: bool) -> None:
    security = _mapping(run.get("security"), "security")
    checks = _list(security.get("checks"), "security.checks")
    policy_path = ROOT / "config" / "security-evidence.json"
    policy = json.loads(policy_path.read_text())
    expected_policy_digest = "sha256:" + hashlib.sha256(policy_path.read_bytes()).hexdigest()
    fail(security.get("policy_sha256") != expected_policy_digest, "security policy digest does not match the global selector policy")
    surfaces = _list(security.get("changed_surfaces"), "security.changed_surfaces")
    fail(any(surface not in policy["surfaces"] for surface in surfaces), "security contains an unknown changed surface")
    fail(any(surface not in profile["security_surface_policy"] for surface in surfaces), "security surface is outside the selected profile policy")
    expected_pairs = {(surface, check) for surface in surfaces for check in policy["surfaces"][surface]}
    actual_pairs = {(item.get("surface"), item.get("id")) for item in checks if isinstance(item, dict)}
    fail(actual_pairs != expected_pairs, "security checks do not exactly match policy-selected surfaces")
    canonical = {artifact_id: artifact for artifact_id, artifact in artifacts.items() if artifact.get("class") == "canonical"}
    technical_types = set(registry["profiles"]["software"]["artifact_types"]) | set(registry["profiles"]["agent-product"]["artifact_types"])
    interactive_required = any(
        artifact.get("artifact_type") == "interactive-document"
        for artifact in canonical.values()
    )
    technical_required = interactive_required or any(
        artifact.get("artifact_type") in technical_types for artifact in canonical.values()
    )
    if required and run.get("risk_tier") in {"substantial", "crucial", "terminal"} and technical_required:
        if interactive_required:
            fail(
                "source" not in surfaces,
                "interactive document requires source security composition",
            )
        fail(not surfaces, "substantial+ technical profile requires changed security surfaces")
        fail(security.get("status") != "pass" or not checks, "substantial+ technical profile requires passing security evidence")
        mappings = _list(security.get("artifact_surfaces"), "security.artifact_surfaces")
        mapped_ids: set[str] = set()
        for index, raw in enumerate(mappings):
            mapping = _mapping(raw, f"security.artifact_surfaces[{index}]")
            artifact_id = mapping.get("artifact_id")
            fail(artifact_id not in canonical or artifact_id in mapped_ids, f"security artifact mapping {index} is missing, duplicate or non-canonical")
            mapped_ids.add(artifact_id)
            declared = set(_list(mapping.get("surfaces"), f"security.artifact_surfaces[{index}].surfaces"))
            minimum = set(registry["artifact_type_surfaces"][canonical[artifact_id]["artifact_type"]])
            fail(not minimum <= declared or not declared <= set(surfaces), f"security artifact mapping {index} omits its derived surfaces")
        fail(mapped_ids != set(canonical), "every canonical artifact requires a security surface mapping")
    for index, raw in enumerate(checks):
        check = _mapping(raw, f"security.checks[{index}]")
        allowed_status = {"pass"} if required else {"pending", "pass"}
        fail(not check.get("id") or not check.get("surface") or check.get("status") not in allowed_status, f"selected security check {index} has invalid status")
        if check.get("status") == "pending":
            continue
        linked = evidence.get(check.get("evidence_id"))
        fail(not linked or linked.get("kind") != "deterministic" or linked.get("status") != "pass" or linked.get("gate") != check.get("id"), f"security check {index} must link matching passing deterministic evidence")
    agentic_types = set(registry["profiles"]["agent-product"]["artifact_types"])
    if required and any(artifact.get("artifact_type") in agentic_types for artifact in canonical.values()):
        risks = _list(security.get("agentic_risks"), "security.agentic_risks")
        fail({item.get("id") for item in risks if isinstance(item, dict)} != AGENTIC_RISKS, "agent-product must disposition every agentic risk")
        for item in risks:
            fail(item.get("status") not in {"pass", "not_applicable"}, "agentic risk disposition is invalid")
            if item.get("status") == "pass":
                linked = evidence.get(item.get("evidence_id"))
                fail(not linked or linked.get("kind") != "deterministic" or linked.get("status") != "pass" or linked.get("gate") != f"agentic-risk:{item.get('id')}", "agentic risk pass must link matching passing deterministic evidence")
            else:
                fail(not item.get("reason"), "agentic risk not_applicable requires reason")

def _validate_gates_observation(run: dict[str, Any], evidence: dict[str, dict[str, Any]]) -> None:
    gates = _mapping(run.get("human_gates"), "human_gates")
    acceptance = _mapping(gates.get("acceptance"), "human_gates.acceptance")
    release = _mapping(gates.get("release"), "human_gates.release")
    for name, gate in (("acceptance", acceptance), ("release", release)):
        fail(gate.get("status") not in {"pending", "approved", "not-required"}, f"human {name} status is invalid")
        if gate.get("status") == "approved":
            fail(not gate.get("approver") or not gate.get("evidence"), f"human {name} approval requires approver and evidence")
            linked = evidence.get(gate.get("evidence"))
            fail(not linked or linked.get("kind") != "human" or linked.get("status") != "pass" or linked.get("gate") != f"human-{name}", f"human {name} approval must link matching passing human evidence")
    if run.get("status") in {"accepted", "awaiting_release", "observing", "closed"}:
        fail(acceptance.get("status") != "approved", "accepted state requires human acceptance")
        accepted_transition = next(item for item in run["state_history"] if item["state"] == "accepted")
        fail(acceptance.get("evidence") not in accepted_transition["evidence_ids"], "accepted transition must cite its human acceptance evidence")
    if run.get("status") in {"observing", "closed"}:
        fail(release.get("status") != "approved", "observation requires separate human release authority")
        observing_transition = next(item for item in run["state_history"] if item["state"] == "observing")
        fail(release.get("evidence") not in observing_transition["evidence_ids"], "observing transition must cite its human release evidence")
    observation = run.get("observation")
    fail(not isinstance(observation, dict), "observation contract is required")
    observation_status = observation.get("status")
    if run.get("status") == "observing":
        fail(observation_status not in {"active", "pass"}, "observing state requires observation status active or pass")
    elif run.get("status") == "closed":
        fail(observation_status != "pass", "closed state requires observation status pass")
    elif run.get("status") in NORMAL_STATES:
        fail(observation_status not in {"planned", "not_applicable"}, "pre-release lifecycle states require planned or not_applicable observation")
    if observation_status == "not_applicable":
        fail(not observation.get("reason"), "observation not_applicable requires profile justification")
    else:
        for field in ("window", "signals", "thresholds", "owner", "containment", "privacy", "close_condition"):
            fail(not observation.get(field), f"observation.{field} is required")
        window = _mapping(observation.get("window"), "observation.window")
        fail(window.get("kind") not in {"duration", "event-count"}, "observation window kind is invalid")
        minimum_field = "minimum_seconds" if window.get("kind") == "duration" else "minimum"
        minimum = window.get(minimum_field)
        fail(isinstance(minimum, bool) or not isinstance(minimum, int) or minimum < 1, "observation window minimum is invalid")
        fail(observation.get("status") not in {"planned", "active", "pass", "fail"}, "observation status is invalid")
        signals = _list(observation.get("signals"), "observation.signals")
        fail(any(not isinstance(signal, str) or not signal for signal in signals) or len(set(signals)) != len(signals), "observation signals must be unique tokens")
        thresholds = _mapping(observation.get("thresholds"), "observation.thresholds")
        fail(set(thresholds) != set(signals), "observation thresholds must bind every signal")
        for signal, raw in thresholds.items():
            threshold = _mapping(raw, f"observation.thresholds.{signal}")
            limit = threshold.get("limit")
            fail(set(threshold) != {"direction", "limit"} or threshold.get("direction") not in {"gte", "lte", "eq"} or isinstance(limit, bool) or not isinstance(limit, (int, float)) or not math.isfinite(limit), f"observation threshold {signal} is invalid")
        if run.get("status") == "closed":
            fail(observation.get("status") != "pass", "closed run requires passing observation")
            started = _utc(observation.get("started_at"), "observation.started_at")
            ended = _utc(observation.get("ended_at"), "observation.ended_at")
            fail(ended <= started, "observation window must be increasing")
            observing_at = _utc(next(item for item in run["state_history"] if item["state"] == "observing")["at"], "observing transition")
            closed_at = _utc(next(item for item in run["state_history"] if item["state"] == "closed")["at"], "closed transition")
            fail(started < observing_at or ended > closed_at, "observation window must fall between observing and closed transitions")
            if window["kind"] == "duration":
                fail((ended - started).total_seconds() < minimum, "observation duration is shorter than the declared window")
            else:
                observed = observation.get("observed_events")
                fail(isinstance(observed, bool) or not isinstance(observed, int) or observed < minimum, "observation event count is below the declared window")
            evidence_ids = _list(observation.get("evidence_ids"), "observation.evidence_ids")
            fail(not evidence_ids, "closed observation requires typed evidence")
            for evidence_id in evidence_ids:
                linked = evidence.get(evidence_id)
                fail(not linked or linked.get("kind") != "observation" or linked.get("status") != "pass", "observation evidence must be typed and passing")
                observed_at = _utc(linked.get("observed_at"), f"observation evidence {evidence_id}.observed_at")
                fail(observed_at < started or observed_at > ended, "observation measurement must fall inside the observation window")
                threshold = thresholds.get(linked.get("gate"), {})
                value, limit = linked.get("measured_value"), threshold.get("limit")
                direction = threshold.get("direction")
                passed = value >= limit if direction == "gte" else value <= limit if direction == "lte" else value == limit
                fail(not passed, f"observation evidence {evidence_id} misses its threshold")
            observed_gates = {evidence[evidence_id].get("gate") for evidence_id in evidence_ids}
            fail(not set(signals) <= observed_gates, "observation evidence must cover every declared signal")
            closed_transition = next(item for item in run["state_history"] if item["state"] == "closed")
            fail(not set(evidence_ids) <= set(closed_transition["evidence_ids"]), "closed transition must cite its observation evidence")

def _validate_high_stakes(run: dict[str, Any], registry: dict[str, Any], evidence: dict[str, dict[str, Any]]) -> None:
    if run.get("high_stakes") is not True:
        return
    controls = _mapping(run.get("high_stakes_controls"), "high_stakes_controls")
    required = registry["high_stakes_overlay"]["required"]
    evidence_ids: set[str] = set()
    for name, policy in required.items():
        control = _mapping(controls.get(name), f"high_stakes_controls.{name}")
        linked = evidence.get(control.get("evidence_id"))
        fail(control.get("evidence_id") in evidence_ids, f"high-stakes control {name} must use distinct evidence")
        evidence_ids.add(control.get("evidence_id"))
        fail(control.get("status") != "pass" or not linked or linked.get("status") != "pass" or linked.get("kind") not in policy["evidence_kinds"] or linked.get("gate") != policy["gate"], f"high-stakes control {name} requires matching passing typed evidence")
        if name == "source_authority":
            fail(not control.get("authority"), "high-stakes source_authority requires named authority")
        elif name == "privacy":
            fail(not control.get("privacy_boundary"), "high-stakes privacy requires a boundary")
        elif name == "qualified_domain_review":
            fail(any(not control.get(field) for field in ("domain", "reviewer", "qualification")), "qualified domain review requires domain, reviewer and qualification")
        elif name == "explicit_human_action_gate":
            fail(not control.get("action") or not control.get("approved_by"), "explicit human action gate requires action and approver")

def _validate_measures_assurance(
    run: dict[str, Any], profile: dict[str, Any], evidence: dict[str, dict[str, Any]],
    artifacts: dict[str, dict[str, Any]], *, required: bool,
    artifact_root: Path | None, verify_hashes: bool,
) -> None:
    measures = _mapping(run.get("measures"), "measures")
    for kind in ("outcome", "trajectory"):
        rows = _list(measures.get(kind), f"measures.{kind}")
        if required:
            fail(not rows, f"awaiting acceptance requires {kind} measures")
        seen: set[str] = set()
        for index, raw in enumerate(rows):
            item = _mapping(raw, f"measures.{kind}[{index}]")
            fail(not item.get("id") or item["id"] in seen, f"{kind} measure id is missing or duplicate")
            seen.add(item["id"])
            linked = evidence.get(item.get("evidence_id"))
            fail(item.get("status") != "pass" or not linked or linked.get("status") != "pass", f"{kind} measure must link passing evidence")
            fail(item.get("evidence_kind") != linked.get("kind"), f"{kind} measure evidence_kind does not match its evidence")
            fail("value" not in item or not item.get("target") or not item.get("aggregation"), f"{kind} measure requires value, target and aggregation")
        if required:
            fail(not set(profile["required_measures"][kind]) <= seen, f"profile-required {kind} measures are missing")
    assurance = _mapping(run.get("assurance"), "assurance")
    fail(not isinstance(assurance.get("stochastic_required"), bool) or not assurance.get("reason"), "assurance requires stochastic_required and reason")
    stochastic_policy = profile["stochastic_policy"]
    fail(stochastic_policy["required"] is True and assurance.get("stochastic_required") is not True, "profile requires stochastic assurance")
    classified_types = set(stochastic_policy.get("required_for_artifact_types", []))
    if classified_types:
        canonical_types = {
            artifact.get("artifact_type")
            for artifact in artifacts.values()
            if artifact.get("class") == "canonical"
        }
        classified_required = bool(canonical_types & classified_types)
        fail(
            assurance.get("stochastic_required") is not classified_required,
            "assurance.stochastic_required does not match the canonical artifact classification",
        )
    evaluations = _list(assurance.get("evaluations"), "assurance.evaluations")
    if required and assurance["stochastic_required"]:
        fail(not evaluations, "stochastic assurance requires evaluations")
    seen_evaluation_ids: set[str] = set()
    seen_artifact_ids: set[str] = set()
    history_times = [
        _utc(item.get("at"), f"state_history[{index}].at")
        for index, item in enumerate(run["state_history"])
    ]
    complete_count = 0
    for index, raw in enumerate(evaluations):
        item = _mapping(raw, f"assurance.evaluations[{index}]")
        fail(
            set(item) != EVALUATION_BINDING_FIELDS,
            f"evaluation {index} must contain only the canonical receipt binding fields",
        )
        binding_status = item.get("status")
        fail(
            binding_status not in {"planned", "complete", "failed", "incomplete"},
            f"evaluation {index}.status must be planned, complete, failed or incomplete",
        )
        anchored_at = _utc(item.get("anchored_at"), f"evaluation {index}.anchored_at")
        fail(anchored_at > max(history_times), f"evaluation {index}.anchored_at is after the current checkpoint")
        evaluation_id = item.get("evaluation_id")
        fail(not isinstance(evaluation_id, str) or not evaluation_id, f"evaluation {index}.evaluation_id is required")
        fail(evaluation_id in seen_evaluation_ids, f"evaluation {index}.evaluation_id is duplicate")
        seen_evaluation_ids.add(evaluation_id)
        plan_digest = item.get("plan_digest")
        _digest(plan_digest, f"evaluation {index}.plan_digest")

        if binding_status == "planned":
            fail(
                any(item.get(field) != "" for field in ("evaluation_artifact_id", "evaluation_digest", "evidence_id")),
                f"evaluation {index} planned binding must leave artifact, digest and evidence empty",
            )
            fail(
                required and assurance["stochastic_required"],
                f"evaluation {index} must be complete before stochastic acceptance",
            )
            continue

        linked = evidence.get(item.get("evidence_id"))
        if binding_status == "complete":
            complete_count += 1
            fail(
                not linked or linked.get("kind") != "judgement" or linked.get("status") != "pass",
                f"evaluation {index} complete binding must link passing judgement evidence",
            )
        else:
            fail(
                not linked or linked.get("kind") != "deterministic" or linked.get("status") != "pass",
                f"evaluation {index} terminal nonpass must link passing deterministic evidence",
            )
        artifact_id = item.get("evaluation_artifact_id")
        fail(not isinstance(artifact_id, str) or not artifact_id, f"evaluation {index}.evaluation_artifact_id is required")
        fail(artifact_id in seen_artifact_ids, f"evaluation {index}.evaluation_artifact_id is duplicate")
        seen_artifact_ids.add(artifact_id)
        evaluation_digest = item.get("evaluation_digest")
        _digest(evaluation_digest, f"evaluation {index}.evaluation_digest")
        artifact = artifacts.get(artifact_id)
        fail(not artifact, f"evaluation {index} references an unknown evaluation artifact")
        fail(
            not artifact.get("path") or artifact.get("class") != "evidence"
            or artifact.get("artifact_type") != "evidence"
            or artifact.get("media_type") != "application/json",
            f"evaluation {index} must reference a local JSON evidence artifact",
        )
        fail(evaluation_digest != artifact.get("digest"), f"evaluation {index}.evaluation_digest must match its artifact digest")

        if not required and not verify_hashes:
            continue
        fail(not verify_hashes, "accepted materialised evaluation assurance requires --verify-hashes")
        fail(artifact_root is None, "materialised evaluation assurance requires workspace_root or receipt_dir")
        assert artifact_root is not None
        try:
            root = artifact_root.resolve()
            target = (root / artifact["path"]).resolve(strict=True)
            target.relative_to(root)
            raw_receipt = target.read_bytes()
        except (OSError, ValueError) as exc:
            raise Invalid(f"evaluation {index} artifact is unreadable or outside the artifact root: {exc}") from exc
        actual_digest = "sha256:" + hashlib.sha256(raw_receipt).hexdigest()
        fail(actual_digest != evaluation_digest, f"evaluation {index} artifact digest does not match live bytes")
        receipt = _load_bound_json(raw_receipt, f"evaluation {index} artifact")
        validator = _evaluate_validator()
        try:
            errors = validator.validate(
                receipt,
                receipt_dir=target.parent,
                verify_hashes=True,
                require_pass=binding_status == "complete",
                expected_evaluation_id=evaluation_id,
                expected_plan_digest=plan_digest,
                expected_delivery_run_id=run["run_id"],
            )
        except Exception as exc:  # The subordinate validator must fail closed.
            raise Invalid(f"evaluation {index} validator failed: {exc}") from exc
        fail(not isinstance(errors, list), f"evaluation {index} validator returned an invalid result")
        fail(
            bool(errors),
            f"evaluation {index} failed its machine gate: "
            + "; ".join(str(error) for error in errors[:5]),
        )
        expected_receipt_status = {
            "complete": "pass", "failed": "fail", "incomplete": "incomplete",
        }[binding_status]
        fail(
            receipt.get("status") != expected_receipt_status,
            f"evaluation {index} binding status does not match its receipt status",
        )
        receipt_updated_at = _utc(
            receipt.get("updated_at"), f"evaluation {index}.updated_at",
        )
        fail(
            receipt_updated_at > max(history_times),
            f"evaluation {index} receipt completes after the current delivery checkpoint",
        )
        plan = _mapping(receipt.get("plan"), f"evaluation {index}.plan")
        frozen_at = _utc(plan.get("frozen_at"), f"evaluation {index}.plan.frozen_at")
        fail(frozen_at > anchored_at, f"evaluation {index} plan was frozen after its delivery anchor")
        execution_starts = [
            _utc(row.get("started_at"), f"evaluation {index}.{section}[{row_index}].started_at")
            for section in ("preflight", "attempts")
            for row_index, row in enumerate(_list(receipt.get(section), f"evaluation {index}.{section}"))
            if isinstance(row, dict) and row.get("started_at")
        ]
        fail(not execution_starts, f"evaluation {index} receipt lacks an execution start timestamp")
        fail(
            anchored_at >= min(execution_starts),
            f"evaluation {index} anchor must precede its nested evaluation execution",
        )
        if binding_status == "complete":
            schedule = _mapping(plan.get("schedule"), f"evaluation {index}.plan.schedule")
            repetitions = schedule.get("repetitions")
            fail(
                isinstance(repetitions, bool) or not isinstance(repetitions, int)
                or repetitions < stochastic_policy["minimum_repetitions"],
                f"evaluation {index} bound plan repetitions are below the profile minimum",
            )
            cases = _list(schedule.get("cases"), f"evaluation {index}.plan.schedule.cases")
            fail(
                len(cases) < stochastic_policy["minimum_sample_size"],
                f"evaluation {index} bound plan sample size is below the profile minimum",
            )
    if required and assurance["stochastic_required"]:
        fail(
            complete_count == 0,
            "stochastic acceptance requires at least one complete passing evaluation",
        )


def validate(
    run: Any,
    root: Path = ROOT,
    *,
    receipt_dir: Path | None = None,
    workspace_root: Path | None = None,
    project_policy_path: Path | None = None,
    verify_hashes: bool = False,
    validate_retrospective: bool = True,
) -> None:
    fail(not isinstance(run, dict), "RUN root must be an object")
    fail(root.resolve() != ROOT.resolve(), "global policy root cannot be replaced by a project registry")
    fail(run.get("schema_version") != 1 or run.get("contract") != "delivery-run", "delivery receipt must use contract delivery-run schema_version 1")
    fail(not run.get("run_id"), "run_id is required")
    policy_validation = _policy_validation_module()
    policy_validation.validate_fabric_relationships(run, invalid_type=Invalid)
    registry = policy_validation.apply_project_policy(
        policy_validation.load_profiles(ROOT, invalid_type=Invalid),
        run,
        project_policy_path=project_policy_path,
        workspace_root=workspace_root or receipt_dir,
        invalid_type=Invalid,
    )
    profile = registry["profiles"].get(run.get("profile"))
    fail(profile is None, "unknown delivery profile")
    risk_tier = run.get("risk_tier")
    fail(risk_tier not in RISKS, "risk_tier is invalid")
    fail(run.get("chair_family") not in PRIMARY_FAMILIES, "chair_family must be a primary family (openai or anthropic)")
    fail(run.get("status") not in set(NORMAL_STATES) | SIDE_STATES, "status is invalid")
    repairs = run.get("repair_cycles")
    fail(isinstance(repairs, bool) or not isinstance(repairs, int), f"repair_cycles must be an integer, got {type(repairs).__name__}")
    fail(repairs < 0, f"repair_cycles must be non-negative, got {repairs}")
    fail(repairs > REPAIR_BUDGETS[risk_tier], f"repair_cycles {repairs} exceeds budget for {risk_tier} tier (max {REPAIR_BUDGETS[risk_tier]})")
    fail(not isinstance(run.get("escaped_defect"), bool), "escaped_defect must be boolean")
    policy_validation.validate_risk(run, ROOT, risks=RISKS, invalid_type=Invalid)
    authority = _mapping(run.get("authority"), "authority")
    policy_validation.validate_authority(authority, run, ROOT, invalid_type=Invalid)
    allowed_artifact_paths = [_safe_path(item, "authority.allowed_artifact_paths") for item in authority["allowed_artifact_paths"]]
    allowed_source_paths = [_safe_path(item, "authority.allowed_source_paths") for item in authority["allowed_source_paths"]]
    artifacts = _validate_artifacts(
        _list(run.get("artifacts"), "artifacts"),
        workspace_root=workspace_root or receipt_dir,
        verify_hashes=verify_hashes,
        allowed_artifact_paths=allowed_artifact_paths,
        allowed_source_paths=allowed_source_paths,
        profile=profile,
    )
    _validate_history(run)
    _validate_checkpoint(run, artifacts, receipt_dir=receipt_dir, workspace_root=workspace_root)
    furthest = max(NORMAL_STATES.index(item["state"]) for item in run["state_history"] if item["state"] in NORMAL_STATES)
    approved_reached = furthest >= NORMAL_STATES.index("approved")
    reviewing_reached = furthest >= NORMAL_STATES.index("reviewing")
    acceptance_reached = furthest >= NORMAL_STATES.index("awaiting_acceptance")
    required_kinds = ({"deterministic"} if reviewing_reached else set()) | ({"judgement"} if acceptance_reached else set())
    evidence = _validate_evidence(
        run, profile, artifacts, required_kinds, allowed_source_paths,
        artifact_root=workspace_root or receipt_dir, verify_hashes=verify_hashes,
    )
    authority_evidence = evidence.get(authority.get("evidence"))
    fail(not authority_evidence or authority_evidence.get("kind") != "human" or authority_evidence.get("status") != "pass" or authority_evidence.get("gate") != "authority-approval", "authority must link matching passing human evidence")
    approval_artifact = artifacts.get(authority_evidence.get("artifact_id")) if authority_evidence else None
    fail(
        not approval_artifact or authority.get("evidence_digest") != approval_artifact.get("digest"),
        "authority.evidence_digest must bind the linked authority-approval artifact",
    )
    if run["risk_override"].get("status") == "approved":
        override_evidence = evidence.get(run["risk_override"].get("evidence"))
        fail(not override_evidence or override_evidence.get("kind") != "human" or override_evidence.get("status") != "pass" or override_evidence.get("gate") != "risk-override", "risk override must link matching passing human evidence")
    if approved_reached:
        _validate_intent_design(run, artifacts, evidence)
    corrections = _list(run.get("human_corrections"), "human_corrections")
    for index, raw in enumerate(corrections):
        correction = _mapping(raw, f"human_corrections[{index}]")
        _utc(correction.get("at"), f"human_corrections[{index}].at")
        fail(not correction.get("summary"), f"human correction {index} requires a summary")
        linked = evidence.get(correction.get("evidence_id"))
        fail(not linked or linked.get("kind") != "human" or linked.get("status") != "pass" or linked.get("gate") != "human-correction", f"human correction {index} must link matching passing human evidence")
    allowed_history_evidence = set(evidence)
    for index, item in enumerate(run["state_history"]):
        unknown = set(item["evidence_ids"]) - allowed_history_evidence
        fail(bool(unknown), f"state_history[{index}] references unknown evidence ids")
    if reviewing_reached:
        profile_evidence = policy_validation.profile_evidence_requirements(profile, artifacts)
        deterministic_ids = {
            item["id"] for item in evidence.values()
            if item.get("kind") == "deterministic" and item.get("status") == "pass"
            and item.get("gate") in profile_evidence["deterministic"]
        }
        first_review = next(item for item in run["state_history"] if item["state"] == "reviewing")
        fail(not deterministic_ids <= set(first_review["evidence_ids"]), "reviewing transition lacks deterministic gate evidence")
    _validate_reviews(run, evidence, required=acceptance_reached)
    _software_delivery_validator().validate_if_software(
        run, artifacts, workspace_root or receipt_dir, verify_hashes, Invalid,
    )
    if acceptance_reached:
        profile_evidence = policy_validation.profile_evidence_requirements(profile, artifacts)
        final_transition = next(item for item in run["state_history"] if item["state"] == "awaiting_acceptance")
        profile_ids = {
            item["id"] for item in evidence.values()
            if item.get("status") == "pass" and item.get("gate") in {
                *profile_evidence["deterministic"], *profile_evidence["judgement"]
            }
        }
        review_ids = {item.get("evidence_id") for item in run["reviews"] if item.get("status") == "pass" and item.get("role") in REVIEW_ROLES}
        fail(not (profile_ids | review_ids) <= set(final_transition["evidence_ids"]), "awaiting_acceptance transition lacks profile or review evidence")
    _validate_security(run, registry, profile, artifacts, evidence, required=acceptance_reached)
    _validate_measures_assurance(
        run, profile, evidence, artifacts, required=acceptance_reached,
        artifact_root=workspace_root or receipt_dir, verify_hashes=verify_hashes,
    )
    _validate_gates_observation(run, evidence)
    if acceptance_reached:
        _validate_high_stakes(run, registry, evidence)
    incident = run.get("incident")
    if incident is not None:
        incident = _mapping(incident, "incident")
        for field in ("release_id", "evidence_window", "containment", "diagnosis", "regression_case"):
            fail(not incident.get(field), f"incident.{field} is required")
    retrospective = run.get("retrospective")
    if validate_retrospective and run.get("status") == "closed" and (
        run.get("risk_tier") in {"crucial", "terminal"}
        or incident is not None
        or run.get("escaped_defect") is True
        or len(corrections) >= 2
    ):
        retrospective = _mapping(retrospective, "retrospective")
        fail(retrospective.get("status") not in {"closed", "no-change"}, "closed crucial or incident cycle requires a closed retrospective")
        artifact = artifacts.get(retrospective.get("artifact_id"))
        fail(not artifact or not artifact.get("path"), "retrospective must link a local declared artifact")
        _digest(retrospective.get("digest"), "retrospective.digest")
        fail(retrospective.get("digest") != artifact.get("digest"), "retrospective digest must match its artifact")
        fail(workspace_root is None, "required retrospective validation needs workspace_root")
        target = workspace_root / artifact["path"]
        try:
            target.resolve().relative_to(workspace_root.resolve())
            raw = target.read_bytes()
            data = json.loads(raw)
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            raise Invalid(f"retrospective artifact is unreadable: {exc}") from exc
        fail("sha256:" + hashlib.sha256(raw).hexdigest() != artifact.get("digest"), "retrospective artifact live digest does not match")
        validator = _retrospect_validator()
        try:
            validator.validate(
                data, "close", expected_cycle_id=run["run_id"],
                expected_profile=run["profile"],
            )
            validator.verify_hashes(
                data, target.parent, expected_cycle_id=run["run_id"],
                expected_profile=run["profile"], workspace_root=workspace_root,
            )
        except validator.Invalid as exc:
            raise Invalid(f"retrospective artifact failed its contract: {exc}") from exc
        fail(data.get("status") != retrospective.get("status"), "retrospective status does not match its artifact")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--verify-hashes", action="store_true")
    parser.add_argument("--workspace-root", type=Path, default=Path.cwd())
    parser.add_argument("--project-policy", type=Path)
    args = parser.parse_args(argv)
    try:
        run = json.loads(args.receipt.read_text())
        validate(run, ROOT, receipt_dir=args.receipt.parent.resolve(), workspace_root=args.workspace_root.resolve(), project_policy_path=args.project_policy, verify_hashes=args.verify_hashes)
        kind = "delivery-v1"
    except (OSError, json.JSONDecodeError, Invalid) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print(f"PASS: {kind} delivery receipt")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
