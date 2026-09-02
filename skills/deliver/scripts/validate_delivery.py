#!/usr/bin/env python3
"""Validate the canonical domain-neutral delivery lifecycle receipt."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sys
from typing import Any

SCRIPT_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_ROOT))

from delivery_validation_artifacts import _validate_artifacts
from delivery_validation_common import (
    AGENTIC_RISKS, DIGEST, EVALUATION_BINDING_FIELDS, IDENTIFIER, Invalid,
    NORMAL_STATES, POLICY_VALIDATION_PATH, PRIMARY_FAMILIES, REPAIR_BUDGETS,
    REVIEW_ROLES, RISKS, ROOT, SAFE_CLASSES, SIDE_STATES, SKILLS_ROOT,
    TRANSITIONS, _digest, _identifier, _inside, _list,
    _load_bound_json, _mapping, _policy_validation_module,
    _safe_path, _software_delivery_validator, _utc, fail,
)
from delivery_validation_evidence import _validate_evidence
from delivery_validation_lifecycle import _validate_checkpoint, _validate_history, _validate_intent_design
from delivery_validation_measures import _validate_measures_assurance
from delivery_validation_reviews import _validate_reviews
from delivery_validation_security import _validate_gates_observation, _validate_high_stakes, _validate_security

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
    _software_delivery_validator().configure_product_root(root)
    fail(run.get("schema_version") != 1 or run.get("contract") != "delivery-run", "delivery receipt must use contract delivery-run schema_version 1")
    fail(not run.get("run_id"), "run_id is required")
    policy_validation = _policy_validation_module()
    policy_validation.validate_fabric_relationships(run, invalid_type=Invalid)
    registry = policy_validation.apply_project_policy(
        policy_validation.load_profiles(root, invalid_type=Invalid),
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
    policy_validation.validate_risk(run, root, risks=RISKS, invalid_type=Invalid)
    authority = _mapping(run.get("authority"), "authority")
    policy_validation.validate_authority(authority, run, root, invalid_type=Invalid)
    allowed_artifact_paths = [_safe_path(item, "authority.allowed_artifact_paths") for item in authority["allowed_artifact_paths"]]
    allowed_source_paths = [_safe_path(item, "authority.allowed_source_paths") for item in authority["allowed_source_paths"]]
    override_evidence_id = _mapping(run.get("risk_override"), "risk_override").get("evidence")
    override_artifact_ids = {
        item.get("artifact_id") for item in _list(run.get("evidence"), "evidence")
        if isinstance(item, dict) and item.get("id") == override_evidence_id and item.get("artifact_id")
    }
    artifacts = _validate_artifacts(
        _list(run.get("artifacts"), "artifacts"),
        workspace_root=workspace_root or receipt_dir,
        verify_hashes=verify_hashes,
        allowed_artifact_paths=allowed_artifact_paths,
        allowed_source_paths=allowed_source_paths,
        profile=profile, override_artifact_ids=override_artifact_ids,
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
    _validate_reviews(
        run, evidence, required=acceptance_reached, artifacts=artifacts,
        artifact_root=workspace_root or receipt_dir, verify_hashes=verify_hashes,
    )
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
    _validate_security(run, registry, profile, artifacts, evidence, required=acceptance_reached, product_root=root)
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
        fail(not isinstance(data, dict), "retrospective artifact root must be an object")
        scope = data.get("scope") if isinstance(data.get("scope"), dict) else {}
        cycle_ids = scope.get("cycle_ids")
        fail(
            not isinstance(cycle_ids, list) or run["run_id"] not in cycle_ids,
            "retrospective scope does not include the current delivery cycle",
        )
        fail(scope.get("profile") != run["profile"], "retrospective scope profile does not match the delivery profile")
        fail(data.get("status") != retrospective.get("status"), "retrospective status does not match its artifact")
def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--verify-hashes", action="store_true")
    parser.add_argument("--workspace-root", type=Path, default=Path.cwd())
    parser.add_argument("--project-policy", type=Path)
    parser.add_argument("--product-root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    try:
        run = json.loads(args.receipt.read_text())
        validate(run, args.product_root.resolve(), receipt_dir=args.receipt.parent.resolve(), workspace_root=args.workspace_root.resolve(), project_policy_path=args.project_policy, verify_hashes=args.verify_hashes)
        kind = "delivery-v1"
    except (OSError, json.JSONDecodeError, Invalid) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print(f"PASS: {kind} delivery receipt (product_root={args.product_root.resolve()})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
