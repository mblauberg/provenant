#!/usr/bin/env python3
"""Validate a portable RELEASE.json readiness or terminal receipt."""

from __future__ import annotations

import argparse
from datetime import datetime
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shlex
import sys
from typing import Any


PRODUCT_ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[3])
).expanduser()
SKILLS_ROOT = Path(__file__).resolve().parents[2]
WINDOW_DURATION = re.compile(r"^(\d+)([smhd])$")
GIT_OID = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
SHA256 = re.compile(r"^sha256:[0-9a-f]{64}$")
RELEASE_DIGEST_ARTIFACT_FIELDS = {"id", "digest", "acceptance_receipt"}
RELEASE_GIT_ARTIFACT_FIELDS = {"id", "git_revision", "acceptance_receipt"}


def load_delivery_validator():
    path = SKILLS_ROOT / "deliver" / "scripts" / "validate_delivery.py"
    spec = importlib.util.spec_from_file_location("release_delivery_validator", path)
    if not spec or not spec.loader:
        raise RuntimeError("cannot load delivery validator")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DELIVERY_VALIDATOR = load_delivery_validator()


def mapping(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def items(value: Any) -> list[Any]:
    return value if isinstance(value, list) else []


def substantive_text(value: Any) -> bool:
    return (
        isinstance(value, str)
        and bool(value.strip())
        and value.strip().lower() not in {"n/a", "na", "none", "not-applicable", "tbd", "todo", "unknown"}
    )


def nonempty_string_list(value: Any) -> bool:
    values = items(value)
    return bool(values) and all(substantive_text(item) for item in values)


def timestamp(value: Any) -> bool:
    if not isinstance(value, str) or not value.endswith("Z"):
        return False
    try:
        datetime.fromisoformat(value[:-1] + "+00:00")
        return True
    except ValueError:
        return False


def parse_timestamp(value: Any) -> datetime | None:
    if not timestamp(value):
        return None
    return datetime.fromisoformat(str(value)[:-1] + "+00:00")


def command_tokens(value: Any) -> list[str] | None:
    if isinstance(value, list) and value and all(isinstance(item, str) and item for item in value):
        return value
    if not isinstance(value, str) or not value or re.search(r"[;&|><$`()\n\r]", value):
        return None
    try:
        tokens = shlex.split(value)
    except ValueError:
        return None
    return tokens or None


def receipt_reference_path(
    value: Any, *, base_dir: Path | None, workspace_root: Path | None,
    field: str, errors: list[str],
) -> Path | None:
    """Resolve a receipt reference inside its declared verification root."""
    anchor = base_dir or workspace_root
    root = workspace_root or base_dir
    if anchor is None or root is None:
        return None
    candidate = Path(str(value))
    candidate = candidate if candidate.is_absolute() else anchor / candidate
    try:
        root_real = root.resolve(strict=True)
        candidate_real = candidate.resolve(strict=True)
    except OSError:
        errors.append(f"{field} must reference a readable accepted delivery receipt")
        return None
    try:
        candidate_real.relative_to(root_real)
    except ValueError:
        errors.append(f"{field} must remain inside the verification root")
        return None
    return candidate_real


ACTION_TARGET_KINDS = {
    "deploy": {"environment"},
    "publish": {"audience"},
    "share": {"recipient", "audience"},
    "send": {"recipient"},
    "activate": {"environment"},
}
DISCLOSURES = {"private", "restricted", "internal", "public"}
ENVIRONMENT_TIERS = {"development", "test", "staging", "production"}
EXECUTION_MODES = {"command", "connector", "human"}
REVERSAL_MODES = {"rollback", "revoke", "recall", "deactivate", "replace", "contain", "none"}
STATE_CHANGES = {"none", "reversible", "destructive"}
COMPATIBILITY_MODES = {"not-applicable", "backward-compatible", "non-backward-compatible"}
READINESS_PURPOSES = {"general", "state-change", "compatibility", "recovery"}


def accepted_artifact_errors(
    artifact: dict[str, Any], gate: str, base_dir: Path | None,
    workspace_root: Path | None, *, structural_only: bool,
) -> list[str]:
    """Validate that an artifact is pinned to the canonical accepted delivery."""
    errors: list[str] = []
    for field in ("id", "acceptance_receipt"):
        if field not in artifact or not substantive_text(artifact.get(field)):
            errors.append(f"artifact.{field} is required")
    fields = set(artifact)
    digest_shape = fields == RELEASE_DIGEST_ARTIFACT_FIELDS
    revision_shape = fields == RELEASE_GIT_ARTIFACT_FIELDS
    if not digest_shape and not revision_shape:
        errors.append("artifact fields must be exactly one digest or git_revision shape")
    digest = artifact.get("digest") if digest_shape else None
    revision = artifact.get("git_revision") if revision_shape else None
    if digest_shape and (not isinstance(digest, str) or not SHA256.fullmatch(digest)):
        errors.append("artifact.digest must be a lowercase SHA-256 digest")
    if revision_shape and (
        not isinstance(revision, dict)
        or set(revision) != {"repository", "commit", "tree"}
        or not substantive_text(revision.get("repository"))
        or not GIT_OID.fullmatch(str(revision.get("commit", "")))
        or not GIT_OID.fullmatch(str(revision.get("tree", "")))
        or len(str(revision.get("commit", ""))) != len(str(revision.get("tree", "")))
    ):
        errors.append("artifact.git_revision must contain an exact repository, commit and tree")
    if not artifact.get("acceptance_receipt"):
        return errors
    if structural_only:
        return errors

    verification_root = base_dir or workspace_root
    if verification_root is None:
        errors.append(
            "release ready/complete gate requires base_dir or workspace_root "
            "for canonical accepted-artifact verification"
        )
        return errors

    receipt_path = receipt_reference_path(
        artifact["acceptance_receipt"], base_dir=base_dir,
        workspace_root=workspace_root,
        field="artifact.acceptance_receipt", errors=errors,
    )
    if receipt_path is None:
        return errors
    try:
        delivery = json.loads(receipt_path.read_text())
    except (OSError, json.JSONDecodeError):
        errors.append("artifact.acceptance_receipt must reference a readable accepted delivery receipt")
        return errors
    if not isinstance(delivery, dict) or delivery.get("schema_version") != 1 or delivery.get("contract") != "delivery-run":
        errors.append("artifact.acceptance_receipt must use the canonical delivery-run contract")
        return errors

    try:
        live_root = workspace_root or base_dir
        project_policy = mapping(delivery.get("project_policy"))
        project_policy_path = (
            live_root / project_policy["path"]
            if live_root and project_policy.get("path") else None
        )
        DELIVERY_VALIDATOR.validate(
            delivery, PRODUCT_ROOT, receipt_dir=receipt_path.parent,
            workspace_root=live_root, project_policy_path=project_policy_path,
            verify_hashes=True,
        )
    except DELIVERY_VALIDATOR.Invalid:
        errors.append("artifact.acceptance_receipt must be a valid neutral delivery receipt")

    if delivery.get("profile") == "software" and not delivery.get("software_delivery"):
        errors.append("software promotion requires the canonical post-merge delivery binding")

    gates = mapping(delivery.get("human_gates"))
    accepted = mapping(gates.get("acceptance")).get("status") == "approved"
    released = mapping(gates.get("release")).get("status") == "approved"
    if gate == "ready" and (not accepted or released):
        errors.append("artifact.acceptance_receipt must be accepted and not yet released at the ready gate")
    if gate == "complete" and not released:
        errors.append("terminal promotion requires an approved canonical release gate")
    observation_status = mapping(delivery.get("observation")).get("status")
    if gate == "complete" and observation_status not in ("active", "pass"):
        errors.append("terminal promotion requires active or passing canonical observation")

    delivered = next(
        (item for item in items(delivery.get("artifacts")) if mapping(item).get("id") == artifact.get("id")),
        None,
    )
    delivered_artifact = mapping(delivered)
    delivered_revision = delivered_artifact.get("git_revision")
    legacy_archive = (
        delivered_artifact.get("media_type") == "application/x-git-archive"
        and "digest" in delivered_artifact
    )
    if legacy_archive:
        if not digest_shape or delivered_artifact.get("digest") != artifact.get("digest"):
            errors.append("artifact.digest must match the accepted delivery artifact digest")
    elif delivered_revision:
        if artifact.get("git_revision") != delivered_revision or artifact.get("digest"):
            errors.append("artifact.git_revision must match the accepted delivery Git revision")
    elif not delivered or delivered_artifact.get("digest") != artifact.get("digest") or artifact.get("git_revision"):
        errors.append("artifact.digest must match the accepted delivery artifact digest")
    return errors


def evidence_checks(
    values: Any, field: str, *, require_pass: bool, require_items: bool = True,
    not_after: datetime | None = None,
) -> list[str]:
    errors: list[str] = []
    checks = items(values)
    if require_items and not checks:
        return [f"{field} must not be empty"]
    seen: set[str] = set()
    for index, raw in enumerate(checks):
        check = mapping(raw)
        check_id = check.get("id")
        if not isinstance(check_id, str) or not check_id or check_id in seen:
            errors.append(f"{field}[{index}] requires a unique id")
        else:
            seen.add(check_id)
        valid_statuses = {"pass"} if require_pass else {"pass", "fail", "not-run"}
        check_status = check.get("status")
        if not isinstance(check_status, str) or check_status not in valid_statuses:
            errors.append(f"{field}[{index}].status is invalid")
        if check_status != "not-run" and not items(check.get("evidence")):
            errors.append(f"{field}[{index}] requires evidence")
        checked_at = parse_timestamp(check.get("checked_at"))
        if not checked_at:
            errors.append(f"{field}[{index}].checked_at must be a UTC timestamp")
        elif not_after and checked_at > not_after:
            errors.append(f"{field}[{index}].checked_at cannot postdate the receipt")
        purpose = check.get("purpose")
        if purpose is not None and (
            not isinstance(purpose, str) or purpose not in READINESS_PURPOSES
        ):
            errors.append(f"{field}[{index}].purpose is invalid")
    return errors


def passing_evidence_refs(values: Any, *, purposes: set[str]) -> set[str]:
    refs: set[str] = set()
    for raw in items(values):
        check = mapping(raw)
        if check.get("status") != "pass" or check.get("purpose") not in purposes:
            continue
        check_id = check.get("id")
        if substantive_text(check_id):
            refs.add(check_id)
        refs.update(
            evidence for evidence in items(check.get("evidence"))
            if substantive_text(evidence)
        )
    return refs


def operation_errors(
    values: Any, field: str, authority: dict[str, Any], *, require_success: bool,
    require_items: bool = True,
) -> list[str]:
    errors: list[str] = []
    operations = items(values)
    if require_items and not operations:
        return [f"{field} must not be empty"]
    allowed_modes = {
        mode for mode in items(authority.get("allowed_execution_modes"))
        if isinstance(mode, str)
    }
    allowed_operations = {
        operation for operation in items(authority.get("allowed_operations"))
        if isinstance(operation, str)
    }
    prefixes = items(authority.get("allowed_command_prefixes"))
    expiry = parse_timestamp(authority.get("expires_at"))
    for index, raw in enumerate(operations):
        operation = mapping(raw)
        mode = operation.get("mode")
        name = operation.get("operation")
        if not isinstance(mode, str) or mode not in EXECUTION_MODES or mode not in allowed_modes:
            errors.append(f"{field}[{index}].mode is outside promotion authority")
        if not isinstance(name, str) or not name or name not in allowed_operations:
            errors.append(f"{field}[{index}].operation is outside promotion authority")
        if not operation.get("actor"):
            errors.append(f"{field}[{index}].actor is required")
        status = operation.get("status")
        valid_statuses = {"succeeded"} if require_success else {"succeeded", "failed"}
        if not isinstance(status, str) or status not in valid_statuses:
            errors.append(f"{field}[{index}].status is invalid")
        if not items(operation.get("evidence")):
            errors.append(f"{field}[{index}] requires evidence")
        started = parse_timestamp(operation.get("started_at"))
        finished = parse_timestamp(operation.get("finished_at"))
        if not started or not finished:
            errors.append(f"{field}[{index}] requires UTC start and finish timestamps")
        elif started > finished:
            errors.append(f"{field}[{index}] cannot finish before it starts")
        if expiry and finished and expiry < finished:
            errors.append(f"{field}[{index}] finished after promotion authority expired")
        if mode == "command":
            command = operation.get("command")
            tokens = command_tokens(command)
            allowed = [command_tokens(prefix) for prefix in prefixes]
            if tokens is None:
                errors.append(f"{field}[{index}].command must be argv or shell-free text")
            elif not any(prefix and tokens[:len(prefix)] == prefix for prefix in allowed):
                errors.append(f"{field}[{index}].command is outside promotion authority")
            exit_code = operation.get("exit_code")
            if not isinstance(exit_code, int) or isinstance(exit_code, bool):
                errors.append(f"{field}[{index}].exit_code must be an integer")
            elif (status == "succeeded") != (exit_code == 0):
                errors.append(f"{field}[{index}] status must agree with exit_code")
    return errors


def validate(
    receipt: dict[str, Any], gate: str, base_dir: Path | None = None,
    workspace_root: Path | None = None,
    *, structural_only: bool = False, product_root: Path = PRODUCT_ROOT,
) -> list[str]:
    """Validate a generic accepted-artifact promotion receipt.

    ``structural_only`` is for isolated policy tests. It deliberately skips the
    live accepted-delivery binding and therefore cannot certify promotion.
    """
    global PRODUCT_ROOT
    PRODUCT_ROOT = product_root
    errors: list[str] = []
    if receipt.get("schema_version") != 2:
        errors.append("schema_version must be 2")
    for field in ("release_id", "owner"):
        if not receipt.get(field):
            errors.append(f"{field} is required")
    if not timestamp(receipt.get("updated_at")):
        errors.append("updated_at must be a UTC timestamp")
    updated = parse_timestamp(receipt.get("updated_at"))

    action_type = receipt.get("action_type")
    valid_action_type = isinstance(action_type, str) and action_type in ACTION_TARGET_KINDS
    if not valid_action_type:
        errors.append("action_type is invalid")
    target = mapping(receipt.get("target"))
    for field in ("id", "kind", "environment_tier", "disclosure"):
        if not target.get(field):
            errors.append(f"target.{field} is required")
    target_kind = target.get("kind")
    if not isinstance(target_kind, str) or target_kind not in {"environment", "recipient", "audience"}:
        errors.append("target.kind is invalid")
    elif valid_action_type and target_kind not in ACTION_TARGET_KINDS[action_type]:
        errors.append("target.kind is incompatible with action_type")
    disclosure = target.get("disclosure")
    if not isinstance(disclosure, str) or disclosure not in DISCLOSURES:
        errors.append("target.disclosure is invalid")
    environment_tier = target.get("environment_tier")
    if target_kind == "environment":
        if not isinstance(environment_tier, str) or environment_tier not in ENVIRONMENT_TIERS:
            errors.append("environment target requires a valid environment_tier")
    elif environment_tier != "not-applicable":
        errors.append("recipient or audience target must use environment_tier not-applicable")

    artifact = mapping(receipt.get("artifact"))
    errors.extend(accepted_artifact_errors(
        artifact, gate, base_dir, workspace_root,
        structural_only=structural_only,
    ))

    authority = mapping(receipt.get("release_authority"))
    if not authority.get("approved_by") or not timestamp(authority.get("expires_at")):
        errors.append("release_authority requires approved_by and UTC expires_at")
    expiry = parse_timestamp(authority.get("expires_at"))
    if expiry and updated and expiry <= updated:
        errors.append("release_authority must cover the promotion checkpoint")
    if action_type not in items(authority.get("action_types")):
        errors.append("release_authority does not include action_type")
    if target.get("id") not in items(authority.get("target_ids")):
        errors.append("release_authority does not include target")
    target_tiers = items(authority.get("target_environment_tiers"))
    if environment_tier not in target_tiers:
        errors.append("release_authority does not include target environment tier")
    if artifact.get("id") not in items(authority.get("artifact_ids")):
        errors.append("release_authority does not include artifact")
    allowed_modes = items(authority.get("allowed_execution_modes"))
    if not allowed_modes or any(
        not isinstance(mode, str) or mode not in EXECUTION_MODES for mode in allowed_modes
    ):
        errors.append("release_authority.allowed_execution_modes is invalid")
    allowed_operations = items(authority.get("allowed_operations"))
    if not allowed_operations or any(not isinstance(item, str) or not item for item in allowed_operations):
        errors.append("release_authority.allowed_operations must not be empty")
    prefixes = authority.get("allowed_command_prefixes")
    if "command" in allowed_modes and (
        not isinstance(prefixes, list) or not prefixes or any(command_tokens(item) is None for item in prefixes)
    ):
        errors.append("command execution requires safe allowed_command_prefixes")
    if not isinstance(authority.get("secrets_access"), str) or authority.get("secrets_access") not in {
        "none", "use-without-disclosure"
    }:
        errors.append("release_authority.secrets_access is invalid")
    for field in ("external_communication", "public_disclosure", "irreversible_action"):
        if not isinstance(authority.get(field), bool):
            errors.append(f"release_authority.{field} must be boolean")
    if action_type in ("publish", "share", "send") and authority.get("external_communication") is not True:
        errors.append("publish/share/send requires explicit external communication authority")
    if target.get("disclosure") == "public" and authority.get("public_disclosure") is not True:
        errors.append("public target requires explicit public disclosure authority")

    data_policy = mapping(receipt.get("data_policy"))
    for field in ("classification", "allowed_disclosure", "secret_handling", "retention_or_expiry"):
        if not data_policy.get(field):
            errors.append(f"data_policy.{field} is required")

    readiness_values = receipt.get("readiness_checks")
    errors.extend(evidence_checks(
        readiness_values, "readiness_checks", require_pass=True, not_after=updated,
    ))
    state_change_refs = passing_evidence_refs(
        readiness_values, purposes={"state-change"},
    )
    compatibility_refs = passing_evidence_refs(
        readiness_values, purposes={"compatibility"},
    )
    recovery_refs = passing_evidence_refs(
        readiness_values, purposes={"recovery"},
    )
    impact_refs = state_change_refs | compatibility_refs | recovery_refs

    change_impact = mapping(receipt.get("change_impact"))
    state_change = change_impact.get("state_change")
    compatibility = change_impact.get("compatibility")
    if not isinstance(state_change, str) or state_change not in STATE_CHANGES:
        errors.append("change_impact.state_change is invalid")
    if not isinstance(compatibility, str) or compatibility not in COMPATIBILITY_MODES:
        errors.append("change_impact.compatibility is invalid")
    has_stateful_impact = state_change in ("reversible", "destructive") or compatibility in (
        "backward-compatible", "non-backward-compatible"
    )
    if has_stateful_impact:
        impact_evidence = items(change_impact.get("readiness_evidence"))
        recovery_point = change_impact.get("recovery_point")
        valid_impact_evidence = nonempty_string_list(impact_evidence)
        if not nonempty_string_list(change_impact.get("ordered_steps")):
            errors.append("stateful change requires non-empty ordered change_impact steps")
        if not valid_impact_evidence:
            errors.append("stateful change requires non-empty change_impact readiness evidence")
        elif any(reference not in impact_refs for reference in impact_evidence):
            errors.append("change_impact readiness evidence must reference purpose-typed passing checks")
        if (
            state_change in ("reversible", "destructive")
            and (
                not valid_impact_evidence
                or not any(reference in state_change_refs for reference in impact_evidence)
            )
        ):
            errors.append("state change requires passing state-change readiness evidence")
        if (
            compatibility == "non-backward-compatible"
            and (
                not valid_impact_evidence
                or not any(reference in compatibility_refs for reference in impact_evidence)
            )
        ):
            errors.append("non-backward-compatible change requires passing compatibility evidence")
        if not substantive_text(recovery_point):
            errors.append("stateful change requires a verified recovery point")
        elif recovery_point not in recovery_refs:
            errors.append("change_impact recovery point must reference a passing recovery check")
    if (
        compatibility == "non-backward-compatible"
        and not substantive_text(change_impact.get("compatibility_window"))
    ):
        errors.append("non-backward-compatible change requires a compatibility window")
    consequential_change = state_change == "destructive" or compatibility == "non-backward-compatible"
    if consequential_change and authority.get("irreversible_action") is not True:
        errors.append("destructive or non-backward-compatible change requires explicit irreversible-action authority")

    plan = mapping(receipt.get("promotion_plan"))
    if not plan.get("plan") or not plan.get("exposure_cap"):
        errors.append("promotion_plan must record plan and exposure_cap")
    if not items(plan.get("stop_conditions")):
        errors.append("promotion_plan.stop_conditions must not be empty")

    reversal = mapping(receipt.get("reversal"))
    reversal_mode = reversal.get("mode")
    if not isinstance(reversal_mode, str) or reversal_mode not in REVERSAL_MODES:
        errors.append("reversal.mode is invalid")
    for field in ("plan", "owner", "time_bound"):
        if not reversal.get(field):
            errors.append(f"reversal.{field} is required")
    for field in ("tested", "irreversible"):
        if not isinstance(reversal.get(field), bool):
            errors.append(f"reversal.{field} must be boolean")
    if environment_tier == "production" and reversal.get("tested") is not True:
        errors.append("production environment reversal must be tested")
    if consequential_change and reversal_mode == "none":
        errors.append("destructive or non-backward-compatible change requires a recovery or containment mode")
    if reversal_mode == "none" and reversal.get("irreversible") is not True:
        errors.append("reversal.mode none must declare irreversible true")
    if target.get("disclosure") == "public" and reversal.get("irreversible") is not True:
        errors.append("public promotion must acknowledge irreversible redistribution risk")
    if reversal.get("irreversible") is True:
        if authority.get("irreversible_action") is not True or not reversal.get("limitations"):
            errors.append("irreversible promotion requires explicit authority and documented limitations")

    proof = mapping(receipt.get("proof"))
    for field in ("plan", "owner", "close_condition"):
        if not proof.get(field):
            errors.append(f"proof.{field} is required")
    requirements = [mapping(item) for item in items(proof.get("requirements"))]
    requirement_ids: set[str] = set()
    if not requirements:
        errors.append("proof.requirements must not be empty")
    for index, requirement in enumerate(requirements):
        requirement_id = requirement.get("id")
        if not isinstance(requirement_id, str) or not requirement_id or requirement_id in requirement_ids:
            errors.append(f"proof.requirements[{index}] requires a unique id")
        else:
            requirement_ids.add(requirement_id)
        if not requirement.get("description"):
            errors.append(f"proof.requirements[{index}].description is required")
    observation_window = proof.get("observation_window")
    window_match = WINDOW_DURATION.fullmatch(str(observation_window)) if observation_window else None
    if observation_window and (not window_match or int(window_match.group(1)) <= 0):
        errors.append("proof.observation_window must be empty or a positive typed duration")

    if gate == "ready":
        if receipt.get("status") != "awaiting-promotion":
            errors.append("status must be awaiting-promotion at the ready gate")
        return errors

    promotion = mapping(receipt.get("human_promotion"))
    if promotion.get("status") != "approved" or not promotion.get("approved_by"):
        errors.append("human_promotion must be approved by a named human")
    approved_at = parse_timestamp(promotion.get("approved_at"))
    if not approved_at:
        errors.append("human_promotion.approved_at must be a UTC timestamp")

    terminal_status = receipt.get("status")
    if terminal_status not in ("complete", "reversed", "failed"):
        errors.append("terminal status must be complete, reversed, or failed")
    execution = mapping(receipt.get("execution"))
    operations = items(execution.get("operations"))
    errors.extend(operation_errors(
        operations, "execution.operations", authority,
        require_success=terminal_status == "complete",
    ))
    operation_starts = [parse_timestamp(mapping(item).get("started_at")) for item in operations]
    operation_finishes = [parse_timestamp(mapping(item).get("finished_at")) for item in operations]
    starts = [value for value in operation_starts if value]
    finishes = [value for value in operation_finishes if value]
    execution_started = min(starts) if starts else None
    execution_finished = max(finishes) if finishes else None
    if approved_at and execution_started and approved_at > execution_started:
        errors.append("promotion execution cannot start before human approval")
    if updated and execution_finished and updated < execution_finished:
        errors.append("terminal receipt must be updated after promotion execution")

    checks = [mapping(item) for item in items(proof.get("checks"))]
    seen_checks: set[str] = set()
    for index, check in enumerate(checks):
        requirement_id = check.get("requirement_id")
        if (
            not isinstance(requirement_id, str)
            or requirement_id not in requirement_ids
            or requirement_id in seen_checks
        ):
            errors.append(f"proof.checks[{index}] must reference one unique requirement")
        else:
            seen_checks.add(requirement_id)
        valid_statuses = {"pass"} if terminal_status == "complete" else {"pass", "fail", "not-run"}
        check_status = check.get("status")
        if not isinstance(check_status, str) or check_status not in valid_statuses:
            errors.append(f"proof.checks[{index}].status is invalid")
        if check_status != "not-run" and not items(check.get("evidence")):
            errors.append(f"proof.checks[{index}] requires evidence")
        observed_at = parse_timestamp(check.get("observed_at"))
        if not observed_at:
            errors.append(f"proof.checks[{index}].observed_at must be a UTC timestamp")
        elif execution_finished and observed_at < execution_finished:
            errors.append(f"proof.checks[{index}] predates promotion completion")
        if updated and observed_at and updated < observed_at:
            errors.append(f"proof.checks[{index}] postdates the terminal receipt")
    if terminal_status == "complete" and seen_checks != requirement_ids:
        errors.append("complete promotion must pass every proof requirement")

    if observation_window and terminal_status == "complete":
        window_match = WINDOW_DURATION.fullmatch(str(observation_window))
        window_started = parse_timestamp(proof.get("window_started_at"))
        window_ended = parse_timestamp(proof.get("window_ended_at"))
        if not window_match or not window_started or not window_ended or window_ended <= window_started:
            errors.append("proof requires a typed increasing observation window")
        else:
            amount = int(window_match.group(1))
            seconds = amount * {"s": 1, "m": 60, "h": 3600, "d": 86400}[window_match.group(2)]
            if amount <= 0 or (window_ended - window_started).total_seconds() < seconds:
                errors.append("proof observation window is shorter than declared")
            if execution_finished and window_started < execution_finished:
                errors.append("proof observation window cannot start before promotion finishes")
            if updated and updated < window_ended:
                errors.append("terminal receipt must be updated after the proof window")
            for index, check in enumerate(checks):
                observed_at = parse_timestamp(check.get("observed_at"))
                if observed_at and (observed_at < window_started or observed_at > window_ended):
                    errors.append(f"proof.checks[{index}] falls outside the proof window")

    if terminal_status == "reversed":
        reversal_execution = mapping(receipt.get("reversal_execution"))
        reversal_operations = items(reversal_execution.get("operations"))
        reversal_checks = items(reversal_execution.get("checks"))
        if reversal.get("mode") == "none":
            errors.append("reversed outcome requires an actionable reversal mode")
        errors.extend(operation_errors(
            reversal_operations, "reversal_execution.operations", authority,
            require_success=True,
        ))
        errors.extend(evidence_checks(
            reversal_checks, "reversal_execution.checks", require_pass=True,
        ))
        reversal_starts = [
            parse_timestamp(mapping(item).get("started_at")) for item in reversal_operations
        ]
        reversal_finishes = [
            parse_timestamp(mapping(item).get("finished_at")) for item in reversal_operations
        ]
        valid_reversal_starts = [value for value in reversal_starts if value]
        valid_reversal_finishes = [value for value in reversal_finishes if value]
        if execution_finished and valid_reversal_starts and min(valid_reversal_starts) < execution_finished:
            errors.append("reversal execution cannot start before promotion execution finishes")
        if updated and valid_reversal_finishes and updated < max(valid_reversal_finishes):
            errors.append("terminal receipt must be updated after reversal execution")
        latest_reversal_finish = max(valid_reversal_finishes) if valid_reversal_finishes else None
        for index, check in enumerate(reversal_checks):
            checked_at = parse_timestamp(mapping(check).get("checked_at"))
            if latest_reversal_finish and checked_at and checked_at < latest_reversal_finish:
                errors.append(f"reversal_execution.checks[{index}] predates reversal completion")
            if updated and checked_at and updated < checked_at:
                errors.append(f"reversal_execution.checks[{index}] postdates the terminal receipt")

    outcome = mapping(receipt.get("outcome"))
    if outcome.get("status") != terminal_status or not items(outcome.get("evidence")):
        errors.append("outcome must match terminal status and contain evidence")
    if terminal_status != "complete" and not outcome.get("follow_up_owner"):
        errors.append("non-success terminal outcome requires follow_up_owner")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--gate", choices=("ready", "complete"), default="ready")
    parser.add_argument("--workspace-root", type=Path, default=Path.cwd())
    parser.add_argument("--product-root", type=Path, default=PRODUCT_ROOT)
    args = parser.parse_args(argv)
    try:
        receipt = json.loads(args.receipt.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        print(f"invalid release receipt: {exc}", file=sys.stderr)
        return 2
    errors = validate(
        receipt if isinstance(receipt, dict) else {},
        args.gate,
        args.receipt.parent,
        args.workspace_root.resolve(),
        product_root=args.product_root.resolve(),
    )
    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(f"PASS: {args.gate} gate")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
