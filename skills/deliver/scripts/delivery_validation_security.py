"""Security, observation, and high-stakes validation for delivery receipts."""

from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
from typing import Any

from delivery_validation_common import (
    AGENTIC_RISKS, NORMAL_STATES, _list, _mapping, _utc, fail,
)

def _validate_security(run: dict[str, Any], registry: dict[str, Any], profile: dict[str, Any], artifacts: dict[str, dict[str, Any]], evidence: dict[str, dict[str, Any]], *, required: bool, product_root: Path) -> None:
    security = _mapping(run.get("security"), "security")
    checks = _list(security.get("checks"), "security.checks")
    policy_path = product_root / "config" / "security-evidence.json"
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


