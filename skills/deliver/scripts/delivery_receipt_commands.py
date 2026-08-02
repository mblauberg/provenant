"""Evidence command handlers for the canonical delivery receipt producer."""

from __future__ import annotations

import copy
import hashlib
import json
import os
import platform
import re
import shlex
import subprocess
import sys

import delivery_receipt_reference as reference_fixtures
import reference_evaluation as reference_materializer
from pathlib import Path
from typing import Any


def _source_records(
    api: Any, sources: list[str], workspace: Path, run: dict[str, Any],
) -> list[dict[str, Any]]:
    records = []
    for source in sources:
        target, _ = api.safe_workspace_path(workspace, source, "evidence source")
        api.ensure_allowed_source_target(run, workspace, target)
        if not target.is_file():
            raise api.ReceiptError(f"evidence source does not exist: {source}")
        raw = target.read_bytes()
        records.append({"path": source, "digest": api.digest_bytes(raw), "bytes": len(raw)})
    return records


def _git_record(workspace: Path) -> dict[str, Any]:
    git_environment = os.environ.copy()
    for name in (
        "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR",
    ):
        git_environment.pop(name, None)
    try:
        root = subprocess.check_output(
            ["git", "-C", str(workspace), "rev-parse", "--show-toplevel"],
            text=True, stderr=subprocess.DEVNULL, env=git_environment,
        ).strip()
        head = subprocess.check_output(
            ["git", "-C", str(workspace), "rev-parse", "HEAD"],
            text=True, stderr=subprocess.DEVNULL, env=git_environment,
        ).strip()
        if Path(root).resolve() != workspace.resolve() or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", head):
            return {"available": False, "reason": "workspace is not a committed Git root"}
        return {"available": True, "root": str(workspace.resolve()), "head": head}
    except (OSError, subprocess.CalledProcessError):
        return {"available": False, "reason": "Git identity unavailable"}


def _git_required(run: dict[str, Any], workspace: Path) -> bool:
    """Require Git provenance for Git-backed execution workspaces."""
    return run.get("profile") in {"software", "agent-product"} and (
        (workspace / ".git").is_dir() or (workspace / ".git").is_file()
    )


def _environment_provenance() -> dict[str, Any]:
    """Capture non-secret runtime identity needed to interpret a gate result."""
    identity = {
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
        },
        "python": {
            "executable": str(Path(sys.executable).resolve()),
            "version": platform.python_version(),
        },
        "variables": {
            name: os.environ.get(name, "")
            for name in ("PATH", "VIRTUAL_ENV", "CONDA_PREFIX", "PYTHONHOME", "NODE_PATH")
        },
    }
    canonical = json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()
    return {**identity, "digest": "sha256:" + hashlib.sha256(canonical).hexdigest()}


def _require_git_record(
    record: dict[str, Any], *, required: bool, error_type: type[ValueError] = ValueError,
) -> None:
    if not isinstance(record, dict) or not record:
        raise error_type("execution Git provenance is empty")
    if record.get("available") is True:
        if (
            not isinstance(record.get("root"), str)
            or not record["root"]
            or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", record.get("head", ""))
        ):
            raise error_type("execution Git provenance is invalid")
    elif required:
        raise error_type("execution Git provenance is required for this workspace")
    elif not isinstance(record.get("reason"), str) or not record["reason"].strip():
        raise error_type("execution Git provenance is invalid")


def _next_bundle_version(api: Any, run: dict[str, Any], artifact_id: str) -> str:
    existing = {
        item.get("id") for item in run.get("artifacts", [])
        if isinstance(item, dict)
    }
    version = 2
    while True:
        candidate = api.require_identifier(f"{artifact_id}-v{version}", "bundle artifact id")
        if candidate not in existing:
            return candidate
        version += 1


def _gate_report(
    api: Any, workspace: Path, args: Any, command: list[str],
) -> tuple[str, str, dict[str, Any]]:
    target, relative = api.safe_workspace_path(workspace, args.gate_report, "gate report")
    try:
        raw = target.read_bytes()
        report = json.loads(raw)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise api.ReceiptError(f"gate report is unreadable: {exc}") from exc
    if not isinstance(report, dict) or set(report) != {
        "schema_version", "contract", "gate", "argv", "scope", "counts", "baseline",
    }:
        raise api.ReceiptError("gate report contract is incomplete")
    if (
        report["schema_version"] != 1
        or report["contract"] != "delivery-gate-report"
        or report["gate"] != args.gate
        or report["argv"] != command
        or report["scope"] != args.scope
    ):
        raise api.ReceiptError("gate report does not bind the exact gate identity")
    counts = report["counts"]
    if not isinstance(counts, dict) or set(counts) != {
        "collected", "passed", "failed", "skipped", "expected_collected",
    }:
        raise api.ReceiptError("gate report counts are incomplete")
    if any(
        isinstance(counts[field], bool) or not isinstance(counts[field], int) or counts[field] < 0
        for field in ("collected", "passed", "failed", "skipped", "expected_collected")
    ):
        raise api.ReceiptError("gate report counts are non-negative integers")
    if counts["passed"] + counts["failed"] + counts["skipped"] != counts["collected"]:
        raise api.ReceiptError("gate report counts must account for every collected result")
    baseline = report["baseline"]
    if not isinstance(baseline, dict) or set(baseline) != {"kind", "expected_collected"}:
        raise api.ReceiptError("gate report baseline is incomplete")
    if baseline["kind"] != "structured-runner" or baseline["expected_collected"] != counts["expected_collected"]:
        raise api.ReceiptError("gate report baseline does not bind the structured runner population")
    if counts["expected_collected"] == 0 or (
        args.scope == "full" and counts["collected"] < counts["expected_collected"]
    ) or (
        args.scope == "scoped" and counts["collected"] >= counts["expected_collected"]
    ):
        raise api.ReceiptError("gate report counts do not bind declared scope")
    supplied = {
        field: getattr(args, field, None)
        for field in ("collected", "passed", "failed", "skipped", "expected_collected")
    }
    if any(value is not None and value != counts[field] for field, value in supplied.items()):
        raise api.ReceiptError("caller-supplied gate counts disagree with the structured runner report")
    return relative, api.digest_bytes(raw), counts


def command_evidence_run(args: Any, api: Any) -> dict[str, Any]:
    command = list(args.command_args)
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        raise api.ReceiptError("evidence run requires a command after --")
    if args.scope not in {"scoped", "full"}:
        raise api.ReceiptError("evidence gate scope must be scoped or full")
    run_dir, receipt = api._receipt_path(args.run_dir)
    workspace = run_dir.parent.parent.resolve()
    report_relative, report_digest, report_counts = _gate_report(api, workspace, args, command)
    counts = {"scope": args.scope, **report_counts}
    with api.run_lock(run_dir):
        run = api.load_run(run_dir)
        api.ensure_immutable_risk(run, workspace)
        api.ensure_mutable(run)
        api.ensure_new_evidence_id(run, args.evidence_id)
        artifact, original_bundle = api._bundle_artifact(run, args.artifact_id, workspace)
        sources = []
        for source in dict.fromkeys([*args.sources, report_relative]):
            _target, relative = api.safe_workspace_path(workspace, source, "evidence source")
            sources.append(relative)
        source_records = _source_records(api, sources, workspace, run)
        git_before = _git_record(workspace)
        _require_git_record(git_before, required=_git_required(run, workspace), error_type=api.ReceiptError)
        started = api.timestamp_after(run)
        started_dt = api.parse_utc(started, "evidence started_at")
        observed = api.execute_bounded(command, cwd=workspace)
        source_records_after = _source_records(api, sources, workspace, run)
        git_after = _git_record(workspace)
        _require_git_record(git_after, required=_git_required(run, workspace), error_type=api.ReceiptError)
        if source_records_after != source_records:
            raise api.ReceiptError("execution source digests changed during command")
        if git_after != git_before:
            raise api.ReceiptError("execution Git identity changed during command")
        finished = api.timestamp_after(run, minimum=started_dt)
        observed.update({
            "argv": command,
            "cwd": str(workspace.resolve()),
            "run_identity": {
                "run_id": run["run_id"],
                "receipt": receipt.relative_to(workspace).as_posix(),
            },
            "source_digests": source_records,
            "source_digests_after": source_records_after,
            "git": {"before": git_before, "after": git_after},
            "started_at": started,
            "finished_at": finished,
            "gate_identity": {"id": args.gate, "argv": command, "scope": counts["scope"]},
            "counts": counts,
            "gate_report": {
                "path": report_relative,
                "digest": report_digest,
                "baseline": {
                    "kind": "structured-runner",
                    "expected_collected": counts["expected_collected"],
                },
            },
            "environment": _environment_provenance(),
        })
        exit_code = observed.get("exit_code")
        complete_output = all(
            isinstance(observed.get(stream), dict) and observed[stream].get("complete") is True
            for stream in ("stdout", "stderr")
        )
        status = "pass" if (
            exit_code == 0
            and counts["failed"] == 0
            and observed.get("custody", {}).get("status") == "posix-process-group-cleanup"
            and not observed.get("timed_out")
            and complete_output
        ) else "fail"
        retained_stdout = observed.pop("retained_stdout", "")
        retained_stderr = observed.pop("retained_stderr", "")
        row = {
            "id": args.evidence_id, "kind": "deterministic", "gate": args.gate,
            "status": status, "method": shlex.join(command),
            "artifact_id": args.artifact_id, "source_paths": sources,
            "result": {**observed, "receipt_digest": "sha256:" + "0" * 64},
            "started_at": started, "finished_at": finished,
            "stdout": retained_stdout, "stderr": retained_stderr,
        }
        run["evidence"].append(row)
        bundle_artifact_id = args.artifact_id
        if any(
            isinstance(item, dict) and item.get("kind") == "deterministic"
            and item.get("artifact_id") == args.artifact_id and item is not row
            for item in run["evidence"]
        ):
            bundle_artifact_id = api.require_identifier(f"{args.artifact_id}-{args.evidence_id}", "bundle artifact id")
            if any(item.get("id") == bundle_artifact_id for item in run["artifacts"] if isinstance(item, dict)):
                raise api.ReceiptError(f"bundle artifact id already exists: {bundle_artifact_id}")
            artifact = copy.deepcopy(artifact)
            artifact["id"] = bundle_artifact_id
            run["artifacts"].append(artifact)
            row["artifact_id"] = bundle_artifact_id
        bundle_raw = api._bundle_bytes(run, bundle_artifact_id)
        bundle_digest = api.digest_bytes(bundle_raw)
        target = api._hashed_bundle_path(original_bundle, bundle_artifact_id, bundle_digest)
        artifact["path"] = target.relative_to(workspace).as_posix()
        artifact["digest"] = bundle_digest
        for item in run["evidence"]:
            if isinstance(item, dict) and item.get("kind") == "deterministic" and item.get("artifact_id") == bundle_artifact_id:
                item["result"]["receipt_digest"] = bundle_digest
        run["updated_at"] = finished
        api._publish_bundle_and_receipt(receipt, run, target, bundle_raw)
    return {"evidence_id": args.evidence_id, "status": row["status"], "exit_code": exit_code, "artifact_id": bundle_artifact_id, "receipt_digest": bundle_digest}


def command_reference(args: Any, api: Any) -> dict[str, Any]:
    run_id = api.require_identifier(args.run_id, "run-id")
    if args.profile not in json.loads(api.PROFILE_PATH.read_text()).get("profiles", {}):
        raise api.ReceiptError(f"unknown delivery profile: {args.profile}")
    if not re.fullmatch(r"[0-9a-f]{64}", args.artifact_sha256):
        raise api.ReceiptError("reference artifact SHA-256 is invalid")
    run_dir, receipt, workspace = api.resolve_receipt(args.run_dir, run_id=run_id, allow_missing=True)
    target, relative = api.safe_workspace_path(workspace, args.artifact_path, "reference artifact")
    if not target.is_file() or api.digest_bytes(target.read_bytes()) != f"sha256:{args.artifact_sha256}":
        raise api.ReceiptError("reference artifact does not match its declared digest")
    run = reference_fixtures.make_reference_run(args.profile, api.PRODUCT_ROOT)
    run["run_id"] = run_id
    if isinstance(run.get("fabric_relationships"), dict):
        run["fabric_relationships"]["delivery_run_id"] = run_id
    run["artifacts"].append({
        "id": "fabric-coordination-receipt", "path": relative,
        "media_type": "application/json", "artifact_type": "evidence",
        "digest": f"sha256:{args.artifact_sha256}", "class": "evidence",
        "owner": "chair", "retention": "risk-policy",
    })
    if args.accepted:
        if args.profile != "agent-product":
            raise api.ReceiptError("only an agent-product reference can be accepted")
        run["status"] = "accepted"
        run["state_history"].append({"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["acceptance-approval"]})
        run["human_gates"]["acceptance"] = {"status": "approved", "approver": "human-maintainer", "evidence": "acceptance-approval"}
        run["checkpoint"].update({"current_slice": "accepted", "next_action": "prepare release", "in_flight": []})
    with api.run_lock(run_dir):
        if not run_dir.is_dir():
            raise api.ReceiptError("canonical run directory is unavailable")
        if receipt.exists() or receipt.is_symlink():
            raise api.ReceiptError(f"run-dir already contains a canonical receipt: {run_dir}")
        reference_materializer.materialise_reference_run(
            run, workspace, api.PRODUCT_ROOT,
            receipt_identity=f".agent-run/{run_id}/RUN.json",
        )
        api.create_json_exclusive(receipt, run)
    return {"path": str(receipt), "run_id": run_id, "profile": args.profile}


def command_evidence_human(args: Any, api: Any) -> dict[str, Any]:
    evidence_id = api.require_identifier(args.evidence_id, "evidence id")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        api.ensure_new_evidence_id(run, evidence_id)
        artifact = api.find_artifact(run, args.artifact_id)
        target, _ = api.safe_workspace_path(workspace, artifact.get("path", ""), "human evidence artifact")
        api.ensure_allowed_artifact_target(run, workspace, target)
        raw = target.read_bytes() if target.is_file() else b""
        if not raw or artifact.get("digest") != api.digest_bytes(raw):
            raise api.ReceiptError("human evidence artifact must match existing non-empty bytes")
        approver = args.approver.strip()
        if args.gate == "authority-approval":
            if not approver:
                raise api.ReceiptError("authority approval requires a non-empty approver")
            try:
                content = json.loads(raw)
            except (UnicodeDecodeError, json.JSONDecodeError):
                content = None
            if not isinstance(content, dict) or content.get("approved") is not True or content.get("approver") != approver:
                raise api.ReceiptError(f"authority approval artifact does not corroborate approver {approver}")
        sources = []
        for source in args.sources:
            source_target, relative = api.safe_workspace_path(workspace, source, "human evidence source")
            api.ensure_allowed_source_target(run, workspace, source_target)
            if not source_target.exists():
                raise api.ReceiptError(f"human evidence source does not exist: {source}")
            sources.append(relative)
        recorded = api.timestamp_after(run)
        row = {
            "id": evidence_id, "kind": "human", "gate": args.gate, "status": "pass",
            "method": "producer-bound human evidence", "artifact_id": args.artifact_id,
            "source_paths": list(dict.fromkeys(sources)), "approver": approver,
            "artifact_digest": artifact["digest"], "recorded_at": recorded,
        }
        run["evidence"].append(row)
        if args.gate == "intent-approval":
            run["intent"]["approval"] = {"status": "approved", "approver": approver, "evidence": evidence_id}
        elif args.gate == "human-acceptance":
            run["human_gates"]["acceptance"] = {"status": "approved", "approver": approver, "evidence": evidence_id}
        elif args.gate == "human-release":
            run["human_gates"]["release"] = {"status": "approved", "approver": approver, "evidence": evidence_id}
        return {"evidence_id": evidence_id, "status": "pass"}
    return api.mutate_receipt(args.run_dir, apply)


def command_evidence_rebuild(args: Any, api: Any) -> dict[str, Any]:
    run_dir, receipt = api._receipt_path(args.run_dir)
    workspace = run_dir.parent.parent.resolve()
    with api.run_lock(run_dir):
        run = api.load_run(run_dir)
        api.ensure_immutable_risk(run, workspace)
        api.ensure_mutable(run)
        artifact, original = api._bundle_artifact(run, args.artifact_id, workspace)
        bundle_artifact_id = _next_bundle_version(api, run, args.artifact_id)
        versioned = copy.deepcopy(artifact)
        versioned["id"] = bundle_artifact_id
        run["artifacts"].append(versioned)
        for row in run["evidence"]:
            if isinstance(row, dict) and row.get("kind") == "deterministic" and row.get("artifact_id") == args.artifact_id:
                row["artifact_id"] = bundle_artifact_id
        bundle_raw = api._bundle_bytes(run, bundle_artifact_id)
        digest = api.digest_bytes(bundle_raw)
        target = api._hashed_bundle_path(original, bundle_artifact_id, digest)
        versioned["path"] = target.relative_to(workspace).as_posix()
        versioned["digest"] = digest
        for row in run["evidence"]:
            if isinstance(row, dict) and row.get("kind") == "deterministic" and row.get("artifact_id") == bundle_artifact_id:
                row["result"]["receipt_digest"] = digest
        run["updated_at"] = api.timestamp_after(run)
        api._publish_bundle_and_receipt(receipt, run, target, bundle_raw)
    return {"artifact_id": bundle_artifact_id, "digest": digest}
