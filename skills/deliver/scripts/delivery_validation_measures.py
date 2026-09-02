"""Measures and assurance validation for delivery receipts."""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Any

from delivery_run_shape import recorded_timestamps
from delivery_validation_common import (
    EVALUATION_BINDING_FIELDS, _digest, _evaluate_validator, _list,
    _load_bound_json, _mapping, _utc, fail, Invalid,
)

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
    recorded_times = [
        _utc(value, "recorded timestamp")
        for value in recorded_timestamps(run)
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
        fail(bool(recorded_times) and anchored_at > max(recorded_times), f"evaluation {index}.anchored_at is after the newest recorded evidence")
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
            bool(recorded_times) and receipt_updated_at > max(recorded_times),
            f"evaluation {index} receipt completes after the newest recorded evidence",
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



