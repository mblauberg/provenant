"""Independent evidence validation for delivery receipts."""

from __future__ import annotations

import base64
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
from typing import Any

from delivery_validation_common import (
    Invalid, _digest, _inside, _list, _mapping, _policy_validation_module,
    _safe_path, _utc, fail,
)


def _decode_output_bytes(value: Any, field: str) -> bytes:
    fail(not isinstance(value, str), f"{field} must be base64 text")
    try:
        return base64.b64decode(value.encode("ascii"), validate=True)
    except (UnicodeEncodeError, ValueError) as exc:
        raise Invalid(f"{field} must be valid base64") from exc


def _validate_git_identity(value: Any, field: str, *, required: bool) -> dict[str, Any]:
    identity = _mapping(value, field)
    available = identity.get("available")
    fail(not isinstance(available, bool), f"{field}.available must be boolean")
    if available:
        fail(not isinstance(identity.get("root"), str) or not identity["root"], f"{field}.root is invalid")
        fail(not isinstance(identity.get("head"), str) or not re.fullmatch(r"[0-9a-f]{40}", identity["head"]), f"{field}.head is invalid")
    else:
        fail(required, f"{field} is required for this workspace")
        fail(not isinstance(identity.get("reason"), str) or not identity["reason"].strip(), f"{field}.reason is invalid")
    return identity


def _git_required(run: dict[str, Any], workspace_root: Path | None) -> bool:
    return (
        workspace_root is not None
        and run.get("profile") in {"software", "agent-product"}
        and ((workspace_root / ".git").is_dir() or (workspace_root / ".git").is_file())
    )


def _validate_execution_result(
    run: dict[str, Any], item: dict[str, Any], source_paths: list[str], *,
    receipt_dir: Path | None, workspace_root: Path | None, verify_hashes: bool,
) -> None:
    evidence_id = item["id"]
    result = _mapping(item.get("result"), f"deterministic evidence {evidence_id}.result")
    exit_code = result.get("exit_code")
    fail(isinstance(exit_code, bool) or not isinstance(exit_code, int), f"deterministic evidence {evidence_id} requires integer exit_code")
    legacy = run.get("execution_contract") == "legacy-reference-v1"
    if "argv" not in result:
        fail(run.get("execution_contract") != "legacy-reference-v1", f"deterministic evidence {evidence_id} requires exact argv")
        _digest(result.get("receipt_digest"), f"evidence {evidence_id}.result.receipt_digest")
        declared_artifact = item["_declared_artifact"]
        fail(declared_artifact.get("digest") != result.get("receipt_digest"), f"deterministic evidence {evidence_id} receipt digest must bind its declared artifact")
        fail((item.get("status") == "pass") != (exit_code == 0), f"deterministic evidence {evidence_id} status disagrees with its result")
        return
    signal_value = result.get("signal")
    fail(signal_value is not None and (isinstance(signal_value, bool) or not isinstance(signal_value, int)), f"deterministic evidence {evidence_id} signal is invalid")
    fail(not isinstance(result.get("timed_out"), bool), f"deterministic evidence {evidence_id} requires timed_out")
    argv = _list(result.get("argv"), f"deterministic evidence {evidence_id}.result.argv")
    fail(not argv or any(not isinstance(value, str) for value in argv), f"deterministic evidence {evidence_id} requires exact argv")
    fail(not isinstance(result.get("cwd"), str) or not result["cwd"], f"deterministic evidence {evidence_id} requires cwd")
    identity = _mapping(result.get("run_identity"), f"deterministic evidence {evidence_id}.result.run_identity")
    identity_error = f"deterministic evidence {evidence_id} run identity is invalid"
    if run.get("profile") == "agent-product" and identity.get("run_id") != run.get("run_id"):
        identity_error += "; enclosing_delivery_run_id does not match"
    fail(identity.get("run_id") != run.get("run_id") or not identity.get("receipt"), identity_error)
    source_digests = _list(result.get("source_digests"), f"deterministic evidence {evidence_id}.result.source_digests")
    fail({row.get("path") for row in source_digests if isinstance(row, dict)} != set(source_paths), f"deterministic evidence {evidence_id} source digest set does not match source_paths")
    for source_index, source in enumerate(source_digests):
        source_row = _mapping(source, f"deterministic evidence {evidence_id}.result.source_digests[{source_index}]")
        fail(source_row.get("path") not in source_paths, f"deterministic evidence {evidence_id} source digest path is not declared")
        _digest(source_row.get("digest"), f"deterministic evidence {evidence_id}.source_digests[{source_index}].digest")
        fail(isinstance(source_row.get("bytes"), bool) or not isinstance(source_row.get("bytes"), int) or source_row["bytes"] < 0, f"deterministic evidence {evidence_id} source byte count is invalid")
    if not legacy:
        source_digests_after = _list(result.get("source_digests_after"), f"deterministic evidence {evidence_id}.result.source_digests_after")
        fail(source_digests_after != source_digests, f"deterministic evidence {evidence_id} source identity changed during execution")
    for stream in ("stdout", "stderr"):
        output = _mapping(result.get(stream), f"deterministic evidence {evidence_id}.result.{stream}")
        _digest(output.get("digest"), f"deterministic evidence {evidence_id}.{stream}.digest")
        for field in ("bytes", "retained_bytes"):
            fail(isinstance(output.get(field), bool) or not isinstance(output.get(field), int) or output[field] < 0, f"deterministic evidence {evidence_id}.{stream}.{field} is invalid")
        fail(not isinstance(output.get("truncated"), bool), f"deterministic evidence {evidence_id}.{stream}.truncated is invalid")
        fail(not isinstance(output.get("complete"), bool), f"deterministic evidence {evidence_id}.{stream}.complete is invalid")
        if legacy:
            fail(output["retained_bytes"] > output["bytes"], f"deterministic evidence {evidence_id}.{stream} truncation state is invalid")
            fail(output["truncated"] != (output["retained_bytes"] < output["bytes"]), f"deterministic evidence {evidence_id}.{stream} truncation state is inconsistent")
            continue
        captured = _decode_output_bytes(output.get("captured_b64"), f"deterministic evidence {evidence_id}.{stream}.captured_b64")
        retained = _decode_output_bytes(output.get("retained_b64"), f"deterministic evidence {evidence_id}.{stream}.retained_b64")
        fail(len(captured) != output["bytes"], f"deterministic evidence {evidence_id}.{stream} byte count does not match captured bytes")
        fail("sha256:" + hashlib.sha256(captured).hexdigest() != output["digest"], f"deterministic evidence {evidence_id}.{stream} digest does not match captured bytes")
        fail(len(retained) != output["retained_bytes"] or len(retained) > len(captured), f"deterministic evidence {evidence_id}.{stream} retained byte count is invalid")
        expected_retained = captured[-len(retained):] if retained else b""
        fail(retained != expected_retained, f"deterministic evidence {evidence_id}.{stream} retained bytes do not match captured bytes")
        fail(output["truncated"] != (len(retained) < len(captured)), f"deterministic evidence {evidence_id}.{stream} truncation state is inconsistent")
    custody = _mapping(result.get("custody"), f"deterministic evidence {evidence_id}.result.custody")
    fail(custody.get("status") != "posix-process-group-cleanup", f"deterministic evidence {evidence_id} custody status is invalid")
    fail(custody.get("unsupported") != "Commands that daemonise or call setsid are unsupported.", f"deterministic evidence {evidence_id} custody contract is incomplete")
    cleanup = _mapping(custody.get("cleanup"), f"deterministic evidence {evidence_id}.custody.cleanup")
    fail(cleanup.get("strategy") != "posix-process-group-cleanup", f"deterministic evidence {evidence_id} cleanup strategy is invalid")
    fail(not isinstance(cleanup.get("term_sent"), bool) or not isinstance(cleanup.get("kill_sent"), bool), f"deterministic evidence {evidence_id} cleanup outcome is incomplete")
    fail(isinstance(cleanup.get("grace_seconds"), bool) or not isinstance(cleanup.get("grace_seconds"), (int, float)) or cleanup["grace_seconds"] < 0, f"deterministic evidence {evidence_id} cleanup grace is invalid")
    _utc(result.get("started_at"), f"deterministic evidence {evidence_id}.result.started_at")
    _utc(result.get("finished_at"), f"deterministic evidence {evidence_id}.result.finished_at")
    git = _mapping(result.get("git"), f"deterministic evidence {evidence_id}.result.git")
    git_before: dict[str, Any] | None = None
    if legacy:
        git_for_live = git
    else:
        fail(set(git) != {"before", "after"}, f"deterministic evidence {evidence_id}.result.git must contain before and after identities")
        git_before = _validate_git_identity(
            git.get("before"), f"deterministic evidence {evidence_id}.git.before",
            required=_git_required(run, workspace_root),
        )
        git_after = _validate_git_identity(
            git.get("after"), f"deterministic evidence {evidence_id}.git.after",
            required=_git_required(run, workspace_root),
        )
        fail(git_before != git_after, f"deterministic evidence {evidence_id} Git identity changed during execution")
        git_for_live = git_before
    _digest(result.get("receipt_digest"), f"evidence {evidence_id}.result.receipt_digest")
    declared_artifact = item["_declared_artifact"]
    fail(declared_artifact.get("digest") != result.get("receipt_digest"), f"deterministic evidence {evidence_id} receipt digest must bind its declared artifact")
    passing = (
        signal_value is None
        and not result["timed_out"]
        and custody["status"] == "posix-process-group-cleanup"
        and exit_code == 0
        and all(result[stream].get("complete") is True for stream in ("stdout", "stderr"))
    )
    fail((item.get("status") == "pass") != passing, f"deterministic evidence {evidence_id} status disagrees with its observed result")
    fail(item.get("started_at") != result["started_at"] or item.get("finished_at") != result["finished_at"], f"deterministic evidence {evidence_id} timestamps do not bind execution result")

    if verify_hashes and workspace_root is not None:
        fail(result["cwd"] != str(workspace_root.resolve()), f"deterministic evidence {evidence_id} cwd is not the canonical workspace")
        if receipt_dir is not None:
            receipt_relative = receipt_dir.resolve().relative_to(workspace_root.resolve()).as_posix()
            expected_receipt = "RUN.json" if receipt_relative == "." else f"{receipt_relative}/RUN.json"
            fail(identity["receipt"] != expected_receipt, f"deterministic evidence {evidence_id} receipt identity is not canonical")
        for source in source_digests:
            target = workspace_root / source["path"]
            try:
                raw_source = target.read_bytes()
            except OSError as exc:
                raise Invalid(f"deterministic evidence {evidence_id} source is unreadable") from exc
            fail("sha256:" + hashlib.sha256(raw_source).hexdigest() != source["digest"] or len(raw_source) != source["bytes"], f"deterministic evidence {evidence_id} source digest does not match live bytes")
        if git_for_live.get("available") is True:
            try:
                head = subprocess.check_output(["git", "-C", str(workspace_root), "rev-parse", "HEAD"], text=True, stderr=subprocess.DEVNULL).strip()
                root = subprocess.check_output(["git", "-C", str(workspace_root), "rev-parse", "--show-toplevel"], text=True, stderr=subprocess.DEVNULL).strip()
            except (OSError, subprocess.CalledProcessError) as exc:
                raise Invalid(f"deterministic evidence {evidence_id} Git identity is unavailable") from exc
            fail(git_for_live.get("head") != head or git_for_live.get("root") != str(workspace_root.resolve()) or root != str(workspace_root.resolve()), f"deterministic evidence {evidence_id} Git identity does not match live source")


def _validate_evidence(
    run: dict[str, Any], profile: dict[str, Any], artifacts: dict[str, dict[str, Any]],
    required_kinds: set[str], allowed_source_paths: list[str], *,
    artifact_root: Path | None, verify_hashes: bool,
    receipt_dir: Path | None = None, workspace_root: Path | None = None,
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
            item["_declared_artifact"] = artifacts[item["artifact_id"]]
            _validate_execution_result(run, item, source_paths, receipt_dir=receipt_dir, workspace_root=workspace_root, verify_hashes=verify_hashes)
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
            fail(artifact.get("artifact_type") != "evidence" or artifact.get("media_type") != "application/json" or not artifact.get("path") or artifact.get("uri"), f"deterministic evidence artifact {artifact_id} must be a local JSON evidence bundle")
            target = artifact_root / artifact["path"]
            try:
                bundle = json.loads(target.read_text())
            except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise Invalid(f"deterministic evidence artifact {artifact_id} must contain valid bundle JSON") from exc
            bundle = _mapping(bundle, f"deterministic evidence artifact {artifact_id}")
            fail(set(bundle) != {"schema_version", "contract", "checks"} or bundle.get("schema_version") != 1 or bundle.get("contract") != "deterministic-evidence-bundle", f"deterministic evidence artifact {artifact_id} has an invalid bundle contract")
            checks: dict[str, dict[str, Any]] = {}
            for check_index, raw_check in enumerate(_list(bundle.get("checks"), f"deterministic evidence artifact {artifact_id}.checks")):
                check = _mapping(raw_check, f"deterministic evidence artifact {artifact_id}.checks[{check_index}]")
                fail(set(check) != {"id", "gate", "status", "method", "source_paths", "started_at", "finished_at", "result"} or not isinstance(check.get("id"), str) or check["id"] in checks, f"deterministic evidence artifact {artifact_id} has an invalid or duplicate check")
                checks[check["id"]] = check
            linked = {item["id"]: item for item in by_id.values() if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact_id}
            fail(set(checks) != set(linked), f"deterministic evidence artifact {artifact_id} check set does not match its evidence rows")
            for linked_id, item in linked.items():
                expected = {
                    "id": linked_id, "gate": item["gate"], "status": item["status"], "method": item["method"],
                    "source_paths": item["source_paths"], "started_at": item["started_at"], "finished_at": item["finished_at"],
                    "result": {key: value for key, value in item["result"].items() if key != "receipt_digest"},
                }
                fail(checks[linked_id] != expected, f"deterministic evidence artifact {artifact_id} check {linked_id} does not match its evidence row")
    required_evidence = _policy_validation_module().profile_evidence_requirements(profile, artifacts)
    for kind, gates in required_evidence.items():
        if kind not in required_kinds:
            continue
        for gate in gates:
            matches = [item for item in by_id.values() if item.get("gate") == gate and item.get("status") == "pass"]
            fail(not matches or any(item.get("kind") != kind for item in matches), f"profile gate {gate} requires passing {kind} evidence")
    for item in by_id.values():
        item.pop("_declared_artifact", None)
    return by_id
