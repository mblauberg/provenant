"""Evidence validation for delivery receipts."""

from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

from delivery_receipt_paths import ensure_within_scope, safe_workspace_path
from delivery_validation_common import (
    Invalid, _digest, _inside, _list, _mapping, _policy_validation_module,
    _safe_path, _utc, fail, _load_bound_json,
)

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
        if artifact_root is None:
            fail(any(not any(_inside(path, scope) for scope in allowed_source_paths) for path in source_paths), f"evidence {evidence_id} reads outside authority.allowed_source_paths")
        else:
            for path in source_paths:
                target, _relative = safe_workspace_path(
                    artifact_root, path, f"evidence {evidence_id}.source_paths", Invalid,
                )
                ensure_within_scope(run, artifact_root, target, "source", Invalid)
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
            if (
                item.get("gate") == "tests"
                and artifact_root is not None
                and (artifact_root / "scripts" / "check-harness").is_file()
            ):
                fail(item.get("method") != "scripts/check-harness", "tests gate requires the canonical scripts/check-harness method")
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
