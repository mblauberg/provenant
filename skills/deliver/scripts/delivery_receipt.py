#!/usr/bin/env python3
"""Create and mutate canonical delivery-run receipts without self-validating them."""

from __future__ import annotations

import argparse
import copy
import fcntl
import hashlib
import json
import os
import re
import selectors
import shlex
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path
from typing import Any

SCRIPTS_DIR = str(Path(__file__).resolve().parent)
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)
import delivery_receipt_lifecycle as lifecycle

PRODUCT_ROOT = Path(__file__).resolve().parents[3]
TEMPLATE_PATH = Path(__file__).resolve().parents[1] / "templates" / "RUN.template.json"
RISK_POLICY_PATH = PRODUCT_ROOT / "config" / "risk-policy.json"
PROFILE_PATH = PRODUCT_ROOT / "config" / "delivery-profiles.json"
IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
SAFE_CLASSES = {"canonical", "evidence", "handoff", "scratch", "external"}
REVIEW_ROLES = {"targeted", "other-primary", "distinct-family"}
RISKS = ("routine", "substantial", "crucial", "terminal")
BIND_SECTIONS = dict(
    design="design", incident="incident", retrospective="retrospective",
    **{"assurance-plan": "assurance", "security-plan": "security",
       "observation-plan": "observation", "software-delivery": "software_delivery"},
)
MAX_LOG_BYTES = 64 * 1024
EVIDENCE_TIMEOUT_SECONDS = 30 * 60
DEFAULT_ARTIFACT_TYPES = dict(
    software="documentation", research="report", analysis="report",
    document="markdown", **{"agent-product": "policy"},
)

# Duplicated from validate_delivery.py:36-49 (shared written contract)
# Updated together when delivery lifecycle changes
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


class ReceiptError(ValueError):
    """A refused producer operation."""


def utc_now() -> str:
    return datetime.utcnow().isoformat() + "Z"  # noqa: DTZ003 - adjudicated clock format


def digest_bytes(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def require_identifier(value: str, field: str) -> str:
    if not IDENTIFIER.fullmatch(value):
        raise ReceiptError(f"{field} must be a bounded stable identifier")
    return value


def load_json_argument(value: str, field: str) -> dict[str, Any]:
    raw: str
    candidate = Path(value)
    if value.startswith("@"):
        candidate = Path(value[1:])
        try:
            raw = candidate.read_text()
        except OSError as exc:
            raise ReceiptError(f"{field} JSON is unreadable: {exc}") from exc
    elif not value.lstrip().startswith(("{", "[")) and candidate.is_file():
        try:
            raw = candidate.read_text()
        except OSError as exc:
            raise ReceiptError(f"{field} JSON is unreadable: {exc}") from exc
    else:
        raw = value
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ReceiptError(f"{field} must be valid JSON: {exc}") from exc
    if not isinstance(parsed, dict):
        raise ReceiptError(f"{field} must be a JSON object")
    return parsed


def safe_workspace_path(workspace: Path, value: str, field: str) -> tuple[Path, str]:
    path = Path(value)
    if path.is_absolute() or ".." in path.parts or not value:
        raise ReceiptError(f"{field} must be safe and workspace-relative")
    target = (workspace / path).resolve()
    try:
        target.relative_to(workspace)
    except ValueError as exc:
        raise ReceiptError(f"{field} escapes the workspace") from exc
    return target, path.as_posix().rstrip("/")


def resolve_run_dir(value: str | Path, *, run_id: str | None = None) -> tuple[Path, Path]:
    candidate = Path(value)
    resolved = candidate.resolve()
    if resolved.parent.name != ".agent-run" or not resolved.name:
        raise ReceiptError("run-dir must be a canonical .agent-run/<id> directory")
    if run_id is not None and resolved.name != run_id:
        raise ReceiptError("run-dir name must match run-id")
    workspace = resolved.parent.parent.resolve()
    return resolved, workspace


def fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


@contextmanager
def run_lock(run_dir: Path) -> Iterator[None]:
    lock_path = run_dir / ".RUN.lock"
    with lock_path.open("a+") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def write_json_atomic(path: Path, value: dict[str, Any]) -> None:
    root = path.parent
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w", dir=root, prefix=".RUN.", suffix=".tmp", delete=False,
        ) as handle:
            temporary = Path(handle.name)
            json.dump(value, handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        fsync_directory(root)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()
    reread = json.loads(path.read_text())
    if reread != value:
        raise ReceiptError(f"atomic write verification failed for {path.name}")


def write_bundle_atomic(path: Path, value: dict[str, Any]) -> None:
    root = path.parent
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w", dir=root, prefix=".RUN.", suffix=".tmp", delete=False,
        ) as handle:
            temporary = Path(handle.name)
            json.dump(value, handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        fsync_directory(root)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()
    if json.loads(path.read_text()) != value:
        raise ReceiptError(f"bundle write verification failed for {path.name}")


def load_run(run_dir: Path) -> dict[str, Any]:
    try:
        run = json.loads((run_dir / "RUN.json").read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"RUN.json is unreadable: {exc}") from exc
    if (
        not isinstance(run, dict)
        or run.get("contract") != "delivery-run"
        or run.get("schema_version") != 1
    ):
        raise ReceiptError("RUN.json must be a canonical delivery-run v1 receipt")
    return run


def ensure_immutable_risk(run: dict[str, Any]) -> None:
    history = run.get("state_history")
    if not isinstance(history, list) or not history or not isinstance(history[0], dict):
        raise ReceiptError("state_history must retain its initial draft row")
    initial = history[0].get("risk_tier")
    if initial not in RISKS or run.get("risk_tier") != initial:
        raise ReceiptError("risk tier is immutable after init")
    derived = derive_risk(run.get("risk_assessment", {}))
    if RISKS.index(initial) < RISKS.index(derived):
        override = run.get("risk_override")
        if not isinstance(override, dict):
            raise ReceiptError("approved human risk override is missing")
        validate_override(override, derived)
        linked = [
            item for item in run.get("evidence", [])
            if isinstance(item, dict)
            and item.get("id") == override.get("evidence")
            and item.get("kind") == "human"
            and item.get("status") == "pass"
            and item.get("gate") == "risk-override"
        ]
        if len(linked) != 1:
            raise ReceiptError("approved human risk override evidence is missing")


def mutate_run(
    run_dir_value: str | Path,
    mutation: Callable[[dict[str, Any], Path, Path], dict[str, Any] | None],
) -> dict[str, Any]:
    run_dir, workspace = resolve_run_dir(run_dir_value)
    if not run_dir.is_dir():
        raise ReceiptError("run-dir does not exist")
    with run_lock(run_dir):
        run = load_run(run_dir)
        ensure_immutable_risk(run)
        result = mutation(run, run_dir, workspace) or {}
        write_json_atomic(run_dir / "RUN.json", run)
        return result


def load_risk_policy() -> dict[str, Any]:
    try:
        policy = json.loads(RISK_POLICY_PATH.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"risk policy is unreadable: {exc}") from exc
    if (
        not isinstance(policy, dict)
        or policy.get("tier_order") != list(RISKS)
        or not isinstance(policy.get("factors"), dict)
    ):
        raise ReceiptError("risk policy is invalid")
    return policy


def derive_risk(assessment: dict[str, Any]) -> str:
    policy = load_risk_policy()
    factors = policy["factors"]
    if set(assessment) != set(factors):
        raise ReceiptError("risk-assessment must cover every policy factor")
    index = 0
    for factor, mappings in factors.items():
        selected = assessment.get(factor)
        if selected not in mappings:
            raise ReceiptError(f"risk-assessment.{factor} is invalid")
        index = max(index, RISKS.index(mappings[selected]))
    return RISKS[index]


def validate_override(value: dict[str, Any], derived: str) -> None:
    if (
        value.get("status") != "approved"
        or not value.get("approved_by")
        or not value.get("evidence")
        or not value.get("reason")
    ):
        raise ReceiptError(
            f"risk tier below derived {derived} requires an approved human override"
        )


def materialise_risk_override(
    override: dict[str, Any], workspace: Path, derived: str,
) -> tuple[dict[str, str], dict[str, Any], dict[str, Any]]:
    validate_override(override, derived)
    evidence_id = require_identifier(str(override["evidence"]), "risk override evidence id")
    artifact_id = require_identifier(
        str(override.get("artifact_id", "")), "risk override artifact id",
    )
    target, relative = safe_workspace_path(
        workspace, str(override.get("artifact", "")), "risk override artifact",
    )
    raw = target.read_bytes() if target.is_file() else b""
    if not raw:
        raise ReceiptError("risk override requires an existing non-empty approval artifact")
    cleaned = {
        key: str(override[key])
        for key in ("status", "approved_by", "evidence", "reason")
    }
    artifact = {
        "id": artifact_id,
        "path": relative,
        "media_type": "application/json" if target.suffix == ".json" else "text/plain",
        "artifact_type": "evidence",
        "digest": digest_bytes(raw),
        "class": "evidence",
        "owner": cleaned["approved_by"],
        "retention": "risk-policy",
    }
    evidence = {
        "id": evidence_id,
        "kind": "human",
        "gate": "risk-override",
        "status": "pass",
        "method": f"existing risk override artifact attributed to {cleaned['approved_by']}",
        "artifact_id": artifact_id,
        "source_paths": [],
        "approver": cleaned["approved_by"],
        "recorded_at": utc_now(),
    }
    return cleaned, artifact, evidence


def default_relationships(run_id: str) -> dict[str, str]:
    return {
        "mode": "independent",
        "delivery_run_id": run_id,
        "project_session_id": "not_applicable",
        "coordination_run_id": "not_applicable",
        "workstream_id": "not_applicable",
        "lead_agent_id": "not_applicable",
    }


def command_init(args: argparse.Namespace) -> dict[str, Any]:
    run_id = require_identifier(args.run_id, "run-id")
    run_dir, workspace = resolve_run_dir(args.run_dir, run_id=run_id)
    intent_path, intent_relative = safe_workspace_path(workspace, args.intent, "intent")
    if not intent_path.is_file() or not intent_path.read_bytes():
        raise ReceiptError("intent must reference an existing non-empty file")
    assessment = load_json_argument(args.risk_assessment, "risk-assessment")
    authority = load_json_argument(args.authority, "authority")
    relationships = (
        load_json_argument(args.fabric_relationships, "fabric-relationships")
        if args.fabric_relationships
        else default_relationships(run_id)
    )
    derived = derive_risk(assessment)
    declared = args.risk_tier or derived
    if declared not in RISKS:
        raise ReceiptError("risk-tier is invalid")
    override = {
        "status": "not-required",
        "approved_by": "",
        "evidence": "",
        "reason": "",
    }
    override_artifact: dict[str, Any] | None = None
    override_evidence: dict[str, Any] | None = None
    if RISKS.index(declared) < RISKS.index(derived):
        if not args.risk_override:
            raise ReceiptError(
                f"risk tier below derived {derived} requires an approved human override"
            )
        override, override_artifact, override_evidence = materialise_risk_override(
            load_json_argument(args.risk_override, "risk-override"), workspace, derived,
        )
    elif args.risk_override:
        raise ReceiptError("risk-override is only valid below the derived tier")

    try:
        template = json.loads(TEMPLATE_PATH.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"RUN template is unreadable: {exc}") from exc
    run = copy.deepcopy(template)
    intent_raw = intent_path.read_bytes()
    intent_digest = digest_bytes(intent_raw)
    timestamp = utc_now()
    run.update({
        "run_id": run_id,
        "fabric_relationships": relationships,
        "profile": args.profile,
        "status": "draft",
        "risk_tier": declared,
        "chair_family": args.chair_family,
        "risk_assessment": assessment,
        "risk_override": override,
        "authority": authority,
        "artifacts": [{
            "id": "intent",
            "path": intent_relative,
            "media_type": "text/markdown",
            "artifact_type": DEFAULT_ARTIFACT_TYPES.get(args.profile, "report"),
            "digest": intent_digest,
            "class": "canonical",
            "owner": authority.get("approved_by") or "human-maintainer",
            "retention": "project-policy",
        }, *([override_artifact] if override_artifact else [])],
        "state_history": [{
            "state": "draft",
            "at": timestamp,
            "evidence_ids": [],
            "risk_tier": declared,
        }],
        "evidence": [override_evidence] if override_evidence else [],
        "reviews": [],
        "repair_cycles": 0,
    })
    run["intent"] = {
        "artifact": intent_relative,
        "digest": intent_digest,
        "decision_owner": authority.get("approved_by") or "human-maintainer",
        "approval": {
            "status": "approved",
            "approver": authority.get("approved_by") or "",
            "evidence": "intent-approval",
        },
    }
    run["design"]["artifact_id"] = "intent"
    run["design"]["digest"] = intent_digest
    run["checkpoint"] = {
        "generation": 0,
        "current_slice": "draft",
        "next_action": "complete scope and authority",
        "in_flight": [],
        "artifact_paths": ["RUN.json"],
    }
    if workspace != Path.cwd().resolve():
        raise ReceiptError("init run-dir must be beneath the current workspace root")
    if not run_dir.parent.exists():
        run_dir.parent.mkdir()
        fsync_directory(workspace)
    run_dir.mkdir(exist_ok=False)
    fsync_directory(run_dir.parent)
    with run_lock(run_dir):
        write_json_atomic(run_dir / "RUN.json", run)
    return {"path": str(run_dir / "RUN.json"), "run_id": run_id, "risk_tier": declared}


def command_bind(args: argparse.Namespace) -> dict[str, Any]:
    section = BIND_SECTIONS[args.section]
    value = load_json_argument(args.from_json, args.section)

    def apply(run: dict[str, Any], _run_dir: Path, _workspace: Path) -> dict[str, Any]:
        run[section] = value
        return {"section": args.section}

    return mutate_run(args.run_dir, apply)


def command_artifact_add(args: argparse.Namespace) -> dict[str, Any]:
    artifact_id = require_identifier(args.artifact_id, "artifact id")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        artifacts = run.get("artifacts")
        if not isinstance(artifacts, list):
            raise ReceiptError("artifacts must be a list")
        if any(
            isinstance(item, dict) and item.get("id") == artifact_id
            for item in artifacts
        ):
            raise ReceiptError(f"artifact id already exists: {artifact_id}")
        target, relative = safe_workspace_path(workspace, args.path, "artifact path")
        if not target.is_file():
            raise ReceiptError("artifact path must reference an existing file")
        raw = target.read_bytes()
        artifacts.append({
            "id": artifact_id,
            "path": relative,
            "media_type": args.media_type,
            "artifact_type": args.artifact_type,
            "digest": digest_bytes(raw),
            "class": args.artifact_class,
            "owner": args.owner,
            "retention": args.retention,
        })
        return {"artifact_id": artifact_id, "digest": artifacts[-1]["digest"]}

    return mutate_run(args.run_dir, apply)


def find_artifact(run: dict[str, Any], artifact_id: str) -> dict[str, Any]:
    artifacts = run.get("artifacts")
    if not isinstance(artifacts, list):
        raise ReceiptError("artifacts must be a list")
    matches = [
        item for item in artifacts
        if isinstance(item, dict) and item.get("id") == artifact_id
    ]
    if len(matches) != 1:
        raise ReceiptError(f"unknown or duplicate artifact id: {artifact_id}")
    return matches[0]


def find_evidence(run: dict[str, Any], evidence_id: str) -> dict[str, Any]:
    evidence = run.get("evidence")
    if not isinstance(evidence, list):
        raise ReceiptError("evidence must be a list")
    matches = [
        item for item in evidence
        if isinstance(item, dict) and item.get("id") == evidence_id
    ]
    if len(matches) != 1:
        raise ReceiptError(f"unknown or duplicate evidence id: {evidence_id}")
    return matches[0]


def ensure_new_evidence_id(run: dict[str, Any], evidence_id: str) -> None:
    require_identifier(evidence_id, "evidence id")
    if any(
        isinstance(item, dict) and item.get("id") == evidence_id
        for item in run.get("evidence", [])
    ):
        raise ReceiptError(f"evidence id already exists: {evidence_id}")


def bundle_artifact(
    run: dict[str, Any], artifact_id: str, workspace: Path,
) -> tuple[dict[str, Any], Path]:
    artifact = find_artifact(run, artifact_id)
    if (
        artifact.get("class") != "evidence"
        or artifact.get("artifact_type") != "evidence"
        or artifact.get("media_type") != "application/json"
        or not artifact.get("path")
    ):
        raise ReceiptError("deterministic bundle artifact must be local JSON evidence")
    target, _relative = safe_workspace_path(
        workspace, artifact["path"], "evidence bundle path",
    )
    return artifact, target


def rebuild_bundle(
    run: dict[str, Any], artifact_id: str, workspace: Path,
) -> tuple[Path, dict[str, Any], str]:
    artifact, target = bundle_artifact(run, artifact_id, workspace)
    rows = [
        item for item in run.get("evidence", [])
        if isinstance(item, dict)
        and item.get("kind") == "deterministic"
        and item.get("artifact_id") == artifact_id
    ]
    checks = [{
        "id": item["id"],
        "gate": item["gate"],
        "status": item["status"],
        "method": item["method"],
        "source_paths": item["source_paths"],
        "exit_code": item["result"]["exit_code"],
    } for item in rows]
    bundle = {
        "schema_version": 1,
        "contract": "deterministic-evidence-bundle",
        "checks": checks,
    }
    raw = (json.dumps(bundle, indent=2) + "\n").encode()
    digest = digest_bytes(raw)
    target = target.parent / f"{artifact_id}.{digest.removeprefix('sha256:')}.json"
    artifact["path"] = target.relative_to(workspace).as_posix()
    artifact["digest"] = digest
    for item in rows:
        item["result"]["receipt_digest"] = digest
    return target, bundle, digest


def execute_bounded(
    command: list[str], *, timeout_seconds: float = EVIDENCE_TIMEOUT_SECONDS,
) -> tuple[int, str, str]:
    if not command:
        raise ReceiptError("evidence run requires a command after --")
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert process.stdout is not None and process.stderr is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, 0)
    selector.register(process.stderr, selectors.EVENT_READ, 1)
    buffers = [bytearray(), bytearray()]
    deadline = time.monotonic() + timeout_seconds
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                process.kill()
                process.wait()
                raise ReceiptError(
                    f"evidence command timed out after {timeout_seconds:g} seconds"
                )
            for key, _mask in selector.select(min(0.1, remaining)):
                chunk = os.read(key.fileobj.fileno(), 8192)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                buffer = buffers[key.data]
                buffer.extend(chunk)
                if len(buffer) > MAX_LOG_BYTES:
                    del buffer[:-MAX_LOG_BYTES]
        exit_code = process.wait()
    finally:
        selector.close()
        if process.poll() is None:
            process.kill()
            process.wait()
    return (
        exit_code,
        buffers[0].decode("utf-8", errors="replace"),
        buffers[1].decode("utf-8", errors="replace"),
    )


def command_evidence_run(args: argparse.Namespace) -> dict[str, Any]:
    evidence_id = require_identifier(args.evidence_id, "evidence id")
    command = list(args.command_args)
    if command and command[0] == "--":
        command = command[1:]
    source_paths = list(dict.fromkeys(args.sources))

    run_dir, workspace = resolve_run_dir(args.run_dir)
    with run_lock(run_dir):
        run = load_run(run_dir)
        ensure_immutable_risk(run)
        ensure_new_evidence_id(run, evidence_id)
        bundle_artifact(run, args.artifact_id, workspace)
        for source in source_paths:
            target, _relative = safe_workspace_path(workspace, source, "evidence source")
            if not target.exists():
                raise ReceiptError(f"evidence source does not exist: {source}")
        started = utc_now()
        exit_code, stdout, stderr = execute_bounded(command)
        finished = utc_now()
        if datetime.fromisoformat(finished[:-1]) <= datetime.fromisoformat(started[:-1]):
            raise ReceiptError("evidence timestamps must strictly increase")
        status = "pass" if exit_code == 0 else "fail"
        row = {
            "id": evidence_id,
            "kind": "deterministic",
            "gate": args.gate,
            "status": status,
            "method": shlex.join(command),
            "artifact_id": args.artifact_id,
            "source_paths": source_paths,
            "result": {"exit_code": exit_code, "receipt_digest": "sha256:" + "0" * 64},
            "started_at": started,
            "finished_at": finished,
            "stdout": stdout,
            "stderr": stderr,
        }
        run["evidence"].append(row)
        target, bundle, digest = rebuild_bundle(run, args.artifact_id, workspace)
        write_bundle_atomic(target, bundle)
        write_json_atomic(run_dir / "RUN.json", run)
    return {
        "evidence_id": evidence_id,
        "status": status,
        "exit_code": exit_code,
        "receipt_digest": digest,
    }


def command_evidence_human(args: argparse.Namespace) -> dict[str, Any]:
    evidence_id = require_identifier(args.evidence_id, "evidence id")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        ensure_new_evidence_id(run, evidence_id)
        artifact = find_artifact(run, args.artifact_id)
        if not artifact.get("path"):
            raise ReceiptError("human evidence must use a local artifact")
        target, _relative = safe_workspace_path(
            workspace, artifact["path"], "human evidence artifact",
        )
        raw = target.read_bytes() if target.is_file() else b""
        if not raw:
            raise ReceiptError("human evidence artifact must be existing and non-empty")
        if artifact.get("digest") != digest_bytes(raw):
            raise ReceiptError("human evidence artifact digest does not match live bytes")
        sources: list[str] = []
        for source in args.sources:
            source_target, relative = safe_workspace_path(
                workspace, source, "human evidence source",
            )
            if not source_target.exists():
                raise ReceiptError(f"human evidence source does not exist: {source}")
            sources.append(relative)
        run["evidence"].append({
            "id": evidence_id,
            "kind": "human",
            "gate": args.gate,
            "status": "pass",
            "method": f"existing approval artifact attributed to {args.approver}",
            "artifact_id": args.artifact_id,
            "source_paths": list(dict.fromkeys(sources)),
            "approver": args.approver,
            "recorded_at": utc_now(),
        })
        if args.gate == "authority-approval":
            run["authority"].update({
                "approved_by": args.approver,
                "evidence": evidence_id,
                "evidence_digest": artifact["digest"],
            })
        elif args.gate == "intent-approval":
            run["intent"]["approval"] = {
                "status": "approved",
                "approver": args.approver,
                "evidence": evidence_id,
            }
        elif args.gate == "design-approval":
            run["design"].update({
                "status": "approved",
                "approver": args.approver,
                "evidence": evidence_id,
            })
        elif args.gate in {"human-acceptance", "human-release"}:
            name = "acceptance" if args.gate == "human-acceptance" else "release"
            run["human_gates"][name] = {
                "status": "approved",
                "approver": args.approver,
                "evidence": evidence_id,
            }
        return {"evidence_id": evidence_id, "status": "pass"}

    return mutate_run(args.run_dir, apply)


def evidence_references(run: dict[str, Any], evidence_id: str) -> list[str]:
    references: list[str] = []
    for index, item in enumerate(run.get("state_history", [])):
        if isinstance(item, dict) and evidence_id in item.get("evidence_ids", []):
            references.append(f"state_history[{index}]")
    for index, item in enumerate(run.get("reviews", [])):
        if isinstance(item, dict) and item.get("evidence_id") == evidence_id:
            references.append(f"reviews[{index}]")
    authority = run.get("authority")
    if isinstance(authority, dict) and authority.get("evidence") == evidence_id:
        references.append("authority")
    for field, value in run.items():
        if field in {"evidence", "state_history", "reviews", "authority"}:
            continue
        if _contains_evidence_reference(value, evidence_id):
            references.append(field)
    return references


def _contains_evidence_reference(value: Any, evidence_id: str) -> bool:
    if isinstance(value, dict):
        for key, child in value.items():
            if key in {"evidence", "evidence_id"} and child == evidence_id:
                return True
            if key == "evidence_ids" and isinstance(child, list) and evidence_id in child:
                return True
            if _contains_evidence_reference(child, evidence_id):
                return True
    elif isinstance(value, list):
        return any(_contains_evidence_reference(child, evidence_id) for child in value)
    return False


def command_evidence_remove(args: argparse.Namespace) -> dict[str, Any]:
    evidence_id = require_identifier(args.evidence_id, "evidence id")
    run_dir, workspace = resolve_run_dir(args.run_dir)
    with run_lock(run_dir):
        run = load_run(run_dir)
        ensure_immutable_risk(run)
        row = find_evidence(run, evidence_id)
        references = evidence_references(run, evidence_id)
        if references:
            raise ReceiptError(
                f"evidence {evidence_id} is referenced by {', '.join(references)}"
            )
        run["evidence"].remove(row)
        bundle_update: tuple[Path, dict[str, Any], str] | None = None
        if row.get("kind") == "deterministic":
            bundle_update = rebuild_bundle(run, row["artifact_id"], workspace)
            write_bundle_atomic(bundle_update[0], bundle_update[1])
        write_json_atomic(run_dir / "RUN.json", run)
    return {"evidence_id": evidence_id, "removed": True}


def command_evidence_rebuild(args: argparse.Namespace) -> dict[str, Any]:
    artifact_id = require_identifier(args.artifact_id, "artifact id")
    run_dir, workspace = resolve_run_dir(args.run_dir)
    with run_lock(run_dir):
        run = load_run(run_dir)
        ensure_immutable_risk(run)
        target, bundle, digest = rebuild_bundle(run, artifact_id, workspace)
        write_bundle_atomic(target, bundle)
        write_json_atomic(run_dir / "RUN.json", run)
    return {"artifact_id": artifact_id, "digest": digest}


def profile_judgement_gate(profile: str) -> str:
    return lifecycle.profile_judgement_gate(profile, PROFILE_PATH, ReceiptError)


def add_review_artifact(
    run: dict[str, Any], workspace: Path, *, artifact_id: str, path: str,
) -> tuple[dict[str, Any], str, str]:
    return lifecycle.add_review_artifact(
        run, workspace, artifact_id=artifact_id, path=path, api=globals(),
    )


def command_review_add(args: argparse.Namespace) -> dict[str, Any]:
    review_id = require_identifier(args.review_id, "review id")
    require_identifier(args.reviewer_id, "reviewer-id")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        ensure_new_evidence_id(run, review_id)
        if not args.lenses or any(not lens for lens in args.lenses):
            raise ReceiptError("review requires at least one non-empty lens")
        review_artifact_id = f"{review_id}.artifact"
        route_artifact_id = f"{review_id}.route"
        _artifact, review_path, review_digest = add_review_artifact(
            run, workspace, artifact_id=review_artifact_id, path=args.artifact,
        )
        _route, route_path, route_digest = add_review_artifact(
            run, workspace, artifact_id=route_artifact_id, path=args.route_receipt,
        )
        try:
            route = json.loads((workspace / route_path).read_text())
        except (OSError, json.JSONDecodeError) as exc:
            raise ReceiptError(f"route receipt must be readable JSON: {exc}") from exc
        if (
            not isinstance(route, dict)
            or route.get("status") != "ok"
            or route.get("adapter") != args.adapter
            or route.get("reviewer_id") != args.reviewer_id
            or route.get("resolved_model", route.get("model")) != args.model
            or route.get("model_family") != args.provider_family
            or route.get("certification_eligible") is not True
        ):
            raise ReceiptError("route receipt identity does not match review lineage")
        lineage = {
            "adapter": args.adapter,
            "provider_family": args.provider_family,
            "model": args.model,
        }
        run["evidence"].append({
            "id": review_id,
            "kind": "judgement",
            "gate": profile_judgement_gate(run["profile"]),
            "status": "pass",
            "method": f"independent review artifact by {args.reviewer_id}",
            "artifact_id": review_artifact_id,
            "source_paths": [review_path, route_path],
            "model_lineage": lineage,
            "reviewer_id": args.reviewer_id,
            "review_artifact_digest": review_digest,
            "route_receipt": {"path": route_path, "digest": route_digest},
            "recorded_at": utc_now(),
        })
        run["reviews"].append({
            "id": review_id,
            "role": args.role,
            "provider_family": args.provider_family,
            "adapter": args.adapter,
            "model": args.model,
            "reviewer_id": args.reviewer_id,
            "independent_of_authorship": True,
            "lenses": list(dict.fromkeys(args.lenses)),
            "status": "pass",
            "evidence_id": review_id,
            "reason": "",
            "route_receipt_digest": route_digest,
        })
        return {"review_id": review_id, "evidence_id": review_id}

    return mutate_run(args.run_dir, apply)


def command_transition(args: argparse.Namespace) -> dict[str, Any]:
    return lifecycle.command_transition(args, globals())


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    init = subcommands.add_parser("init")
    init.add_argument("--run-dir", required=True)
    init.add_argument("--run-id", required=True)
    init.add_argument("--profile", required=True)
    init.add_argument("--chair-family", required=True)
    init.add_argument("--risk-assessment", required=True)
    init.add_argument("--intent", required=True)
    init.add_argument("--authority", required=True)
    init.add_argument("--fabric-relationships")
    init.add_argument("--risk-tier", choices=RISKS)
    init.add_argument("--risk-override")
    init.set_defaults(handler=command_init)

    bind = subcommands.add_parser("bind")
    bind.add_argument("--run-dir", required=True)
    bind.add_argument("--section", choices=sorted(BIND_SECTIONS), required=True)
    bind.add_argument("--from", dest="from_json", required=True)
    bind.set_defaults(handler=command_bind)

    artifact = subcommands.add_parser("artifact")
    artifact_commands = artifact.add_subparsers(dest="artifact_command", required=True)
    artifact_add = artifact_commands.add_parser("add")
    artifact_add.add_argument("--run-dir", required=True)
    artifact_add.add_argument("--id", dest="artifact_id", required=True)
    artifact_add.add_argument("--path", required=True)
    artifact_add.add_argument("--class", dest="artifact_class", choices=sorted(SAFE_CLASSES), required=True)
    artifact_add.add_argument("--media-type", required=True)
    artifact_add.add_argument("--artifact-type", required=True)
    artifact_add.add_argument("--owner", required=True)
    artifact_add.add_argument("--retention", required=True)
    artifact_add.set_defaults(handler=command_artifact_add)

    evidence = subcommands.add_parser("evidence")
    evidence_commands = evidence.add_subparsers(dest="evidence_command", required=True)
    evidence_run = evidence_commands.add_parser("run")
    evidence_run.add_argument("--run-dir", required=True)
    evidence_run.add_argument("--id", dest="evidence_id", required=True)
    evidence_run.add_argument("--gate", required=True)
    evidence_run.add_argument("--artifact-id", required=True)
    evidence_run.add_argument("--source", dest="sources", action="append", required=True)
    evidence_run.add_argument("command_args", nargs=argparse.REMAINDER)
    evidence_run.set_defaults(handler=command_evidence_run)

    evidence_human = evidence_commands.add_parser("human")
    evidence_human.add_argument("--run-dir", required=True)
    evidence_human.add_argument("--id", dest="evidence_id", required=True)
    evidence_human.add_argument("--gate", required=True)
    evidence_human.add_argument("--artifact-id", required=True)
    evidence_human.add_argument("--approver", required=True)
    evidence_human.add_argument("--source", dest="sources", action="append", default=[])
    evidence_human.set_defaults(handler=command_evidence_human)

    evidence_remove = evidence_commands.add_parser("remove")
    evidence_remove.add_argument("--run-dir", required=True)
    evidence_remove.add_argument("--id", dest="evidence_id", required=True)
    evidence_remove.set_defaults(handler=command_evidence_remove)

    evidence_rebuild = evidence_commands.add_parser("rebuild")
    evidence_rebuild.add_argument("--run-dir", required=True)
    evidence_rebuild.add_argument("--artifact-id", required=True)
    evidence_rebuild.set_defaults(handler=command_evidence_rebuild)

    review = subcommands.add_parser("review")
    review_commands = review.add_subparsers(dest="review_command", required=True)
    review_add = review_commands.add_parser("add")
    review_add.add_argument("--run-dir", required=True)
    review_add.add_argument("--id", dest="review_id", required=True)
    review_add.add_argument("--role", choices=sorted(REVIEW_ROLES), required=True)
    review_add.add_argument("--artifact", required=True)
    review_add.add_argument("--route-receipt", required=True)
    review_add.add_argument("--reviewer-id", required=True)
    review_add.add_argument("--adapter", required=True)
    review_add.add_argument("--provider-family", required=True)
    review_add.add_argument("--model", required=True)
    review_add.add_argument("--lens", dest="lenses", action="append", required=True)
    review_add.set_defaults(handler=command_review_add)

    checkpoint = subcommands.add_parser("checkpoint")
    checkpoint_commands = checkpoint.add_subparsers(dest="checkpoint_command", required=True)
    checkpoint_set = checkpoint_commands.add_parser("set")
    checkpoint_set.add_argument("--run-dir", required=True)
    checkpoint_set.add_argument("--current-slice", required=True)
    checkpoint_set.add_argument("--next-action", required=True)
    checkpoint_set.add_argument("--in-flight", action="append", default=[])
    checkpoint_set.add_argument("--artifact", dest="artifacts", action="append", default=[])
    checkpoint_set.set_defaults(handler=lambda args: lifecycle.command_checkpoint_set(args, globals()))

    transition = subcommands.add_parser("transition")
    transition.add_argument("--run-dir", required=True)
    transition.add_argument("--to", dest="target", required=True)
    transition.add_argument("--evidence", dest="evidence_ids", action="append", default=[])
    transition.set_defaults(handler=command_transition)

    show = subcommands.add_parser("show")
    show.add_argument("--run-dir", required=True)
    show.set_defaults(handler=lambda args: lifecycle.command_show(args, globals()))
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    try:
        args = parser.parse_args(argv)
        result = args.handler(args)
    except (OSError, ReceiptError, subprocess.SubprocessError) as exc:
        print(json.dumps({"ok": False, "error": str(exc)}), file=sys.stderr)
        return 1
    if "_raw_receipt" in result:
        print(json.dumps(result["_raw_receipt"], indent=2))
    else:
        print(json.dumps({"ok": True, **result}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
