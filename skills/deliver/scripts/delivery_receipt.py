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
import shlex
import subprocess
import sys
import tempfile
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

SCRIPTS_DIR = str(Path(__file__).resolve().parent)
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)
import delivery_receipt_lifecycle as lifecycle
import delivery_receipt_paths as paths
import delivery_receipt_process as process_runner

PRODUCT_ROOT = Path(__file__).resolve().parents[3]
TEMPLATE_PATH = Path(__file__).resolve().parents[1] / "templates" / "RUN.template.json"
RISK_POLICY_PATH = PRODUCT_ROOT / "config" / "risk-policy.json"
PROFILE_PATH = PRODUCT_ROOT / "config" / "delivery-profiles.json"
IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
SAFE_CLASSES = {"canonical", "evidence", "handoff", "scratch", "external"}
REVIEW_ROLES = {"targeted", "other-primary", "distinct-family"}
RISKS = ("routine", "substantial", "crucial", "terminal")
BIND_SECTIONS = dict(
    design="design", incident="incident", measures="measures",
    retrospective="retrospective",
    **{"assurance-plan": "assurance", "security-plan": "security",
       "observation-plan": "observation", "software-delivery": "software_delivery"},
)
BIND_GATE_STATES = {
    "design": "approved",
    "incident": "closed",
    "measures": "awaiting_acceptance",
    "retrospective": "closed",
    "assurance-plan": "awaiting_acceptance",
    "security-plan": "awaiting_acceptance",
    "observation-plan": "observing",
    "software-delivery": "accepted",
}
OBSERVATION_PLAN_FIELDS = (
    "window", "signals", "thresholds", "owner", "containment", "privacy",
    "close_condition",
)
MAX_LOG_BYTES = 64 * 1024
EVIDENCE_TIMEOUT_SECONDS = 30 * 60
CANONICAL_FULL_TEST_COMMAND = ("scripts/check-harness",)
DEFAULT_ARTIFACT_TYPES = dict(
    software="documentation", research="report", analysis="report",
    document="markdown", **{"agent-product": "policy"},
)

class ReceiptError(ValueError):
    """A refused producer operation."""


def utc_now() -> str:
    return datetime.utcnow().isoformat() + "Z"  # noqa: DTZ003 - adjudicated clock format


def digest_bytes(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def _utc(value: Any, field: str) -> datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise ReceiptError(f"{field} must be a UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value[:-1])
    except ValueError as exc:
        raise ReceiptError(f"{field} must be an ISO UTC timestamp") from exc
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _reject_future_timestamp(value: Any, field: str) -> None:
    if _utc(value, field) > datetime.now(timezone.utc) + timedelta(minutes=5):
        raise ReceiptError(f"{field} exceeds the future timestamp tolerance")


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
    return paths.safe_workspace_path(workspace, value, field, ReceiptError)


def ensure_allowed_artifact_target(
    run: dict[str, Any], workspace: Path, target: Path,
) -> None:
    paths.ensure_within_scope(run, workspace, target, "artifact", ReceiptError)


def ensure_allowed_source_target(
    run: dict[str, Any], workspace: Path, target: Path,
) -> None:
    paths.ensure_within_scope(run, workspace, target, "source", ReceiptError)


def check_evidence_sources(
    run: dict[str, Any],
    workspace: Path,
    source_paths: list[str],
    *,
    after_command: bool = False,
) -> None:
    paths.check_evidence_sources(
        run, workspace, source_paths, ReceiptError, after_command=after_command,
    )


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


def ensure_immutable_risk(run: dict[str, Any], workspace: Path) -> None:
    history = run.get("state_history")
    if not isinstance(history, list) or not history or not isinstance(history[0], dict):
        raise ReceiptError("state_history must retain its initial draft row")
    initial = history[0].get("risk_tier")
    if initial not in RISKS or run.get("risk_tier") != initial:
        raise ReceiptError("risk tier is immutable after init")
    derived = derive_risk(run.get("risk_assessment", {}))
    override = run.get("risk_override")
    if RISKS.index(initial) < RISKS.index(derived):
        if not isinstance(override, dict):
            raise ReceiptError("approved human risk override is missing")
        validate_override(override, derived)
    if isinstance(override, dict) and override.get("status") == "approved":
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
        artifact_id = linked[0].get("artifact_id")
        artifacts = [
            item for item in run.get("artifacts", [])
            if isinstance(item, dict) and item.get("id") == artifact_id
        ]
        if len(artifacts) != 1 or not artifacts[0].get("path"):
            raise ReceiptError("approved human risk override artifact is missing")
        target, _relative = safe_workspace_path(
            workspace, artifacts[0]["path"], "risk override artifact",
        )
        ensure_allowed_artifact_target(run, workspace, target)
        raw = target.read_bytes() if target.is_file() else b""
        if not raw or artifacts[0].get("digest") != digest_bytes(raw):
            raise ReceiptError(
                "risk override artifact digest does not match live bytes"
            )
    corrections = run.get("human_corrections")
    if not isinstance(corrections, list):
        raise ReceiptError("human_corrections must be a list")
    for index, correction in enumerate(corrections):
        if not isinstance(correction, dict):
            raise ReceiptError(f"human_corrections[{index}] must be an object")
        _reject_future_timestamp(
            correction.get("at"), f"human_corrections[{index}].at",
        )


def ensure_run_open(run: dict[str, Any]) -> None:
    if run.get("status") == "closed":
        raise ReceiptError("closed run is immutable")


def mutate_run(
    run_dir_value: str | Path,
    mutation: Callable[[dict[str, Any], Path, Path], dict[str, Any] | None],
) -> dict[str, Any]:
    run_dir, workspace = resolve_run_dir(run_dir_value)
    if not run_dir.is_dir():
        raise ReceiptError("run-dir does not exist")
    with run_lock(run_dir):
        run = load_run(run_dir)
        ensure_immutable_risk(run, workspace)
        ensure_run_open(run)
        result = mutation(run, run_dir, workspace) or {}
        ensure_immutable_risk(run, workspace)
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
        "artifact_digest": artifact["digest"],
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
    ensure_allowed_artifact_target({"authority": authority}, workspace, intent_path)
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
        override_target, _relative = safe_workspace_path(
            workspace, override_artifact["path"], "risk override artifact",
        )
        ensure_allowed_artifact_target(
            {"authority": authority}, workspace, override_target,
        )
        reserved_evidence_ids = {
            str(authority.get("evidence", "")),
            "intent-approval",
            "design-approval",
            "human-acceptance",
            "human-release",
        }
        if override_artifact["id"] == "intent":
            raise ReceiptError("risk override artifact id conflicts with reserved intent")
        if override_evidence["id"] in reserved_evidence_ids:
            raise ReceiptError(
                "risk override evidence id conflicts with a reserved approval id"
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
        gate_state = BIND_GATE_STATES[args.section]
        gate_passed = any(
            isinstance(item, dict) and item.get("state") == gate_state
            for item in run.get("state_history", [])
        )
        replacement = run.get(section) != value
        if args.section == "observation-plan" and gate_passed:
            current = run.get(section)
            closed = any(
                isinstance(item, dict) and item.get("state") == "closed"
                for item in run.get("state_history", [])
            )
            if not closed:
                replacement = (
                    not isinstance(current, dict)
                    or any(current.get(field) != value.get(field)
                           for field in OBSERVATION_PLAN_FIELDS)
                )
        if gate_passed and replacement:
            name = args.section.removesuffix("-plan")
            raise ReceiptError(f"{name} lifecycle gate has passed")
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
        ensure_allowed_artifact_target(run, workspace, target)
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
    if any(
        isinstance(item, dict)
        and item.get("gate") == "risk-override"
        and item.get("artifact_id") == artifact_id
        for item in run.get("evidence", [])
    ):
        raise ReceiptError("risk override artifact cannot be a deterministic bundle")
    if any(
        isinstance(item, dict)
        and item.get("kind") == "human"
        and item.get("artifact_id") == artifact_id
        for item in run.get("evidence", [])
    ):
        raise ReceiptError("human evidence artifact cannot be a deterministic bundle")
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
    ensure_allowed_artifact_target(run, workspace, target)
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
    changed_references: list[str] = []
    for item in rows:
        if item["result"].get("receipt_digest") != digest:
            references = evidence_references(run, item["id"])
            if references:
                changed_references.append(
                    f"{item['id']} is referenced by {', '.join(references)}"
                )
    if changed_references:
        raise ReceiptError(
            "evidence " + "; ".join(changed_references)
        )
    target = (
        target.parent / f"{artifact_id}.{digest.removeprefix('sha256:')}.json"
    ).resolve()
    ensure_allowed_artifact_target(run, workspace, target)
    artifact["path"] = target.relative_to(workspace).as_posix()
    artifact["digest"] = digest
    for item in rows:
        item["result"]["receipt_digest"] = digest
    return target, bundle, digest


def execute_bounded(
    command: list[str], *, cwd: Path, timeout_seconds: float = EVIDENCE_TIMEOUT_SECONDS,
) -> tuple[int, str, str]:
    return process_runner.execute_bounded(
        command, cwd=cwd, timeout_seconds=timeout_seconds,
        max_log_bytes=MAX_LOG_BYTES, error_type=ReceiptError,
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
        ensure_immutable_risk(run, workspace)
        ensure_run_open(run)
        ensure_new_evidence_id(run, evidence_id)
        bundle_artifact(run, args.artifact_id, workspace)
        check_evidence_sources(run, workspace, source_paths)
        canonical_harness = workspace / CANONICAL_FULL_TEST_COMMAND[0]
        if (
            args.gate == "tests"
            and canonical_harness.is_file()
            and tuple(command) != CANONICAL_FULL_TEST_COMMAND
        ):
            raise ReceiptError(
                "tests gate requires the canonical scripts/check-harness method; "
                "use a narrower gate for focused commands"
            )
        started = utc_now()
        exit_code, stdout, stderr = execute_bounded(command, cwd=workspace)
        ensure_immutable_risk(run, workspace)
        # The measured command runs with write access to the workspace, so the
        # paths recorded below must be revalidated against what they resolve to
        # now, not against what they resolved to before it ran.
        check_evidence_sources(run, workspace, source_paths, after_command=True)
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
        ensure_immutable_risk(run, workspace)
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
        if args.gate == "authority-approval":
            if not args.approver.strip():
                raise ReceiptError(
                    "authority approval requires a non-empty approver"
                )
            try:
                artifact_content = json.loads(raw)
            except (TypeError, UnicodeDecodeError, json.JSONDecodeError):
                artifact_content = None
            if not (
                isinstance(artifact_content, dict)
                and artifact_content.get("approved") is True
                and artifact_content.get("approver") == args.approver
            ):
                raise ReceiptError(
                    "authority approval artifact does not corroborate approver "
                    f"{args.approver}"
                )
        sources: list[str] = []
        for source in args.sources:
            source_target, relative = safe_workspace_path(
                workspace, source, "human evidence source",
            )
            ensure_allowed_source_target(run, workspace, source_target)
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
            "artifact_digest": artifact["digest"],
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
        ensure_immutable_risk(run, workspace)
        ensure_run_open(run)
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
        ensure_immutable_risk(run, workspace)
        write_json_atomic(run_dir / "RUN.json", run)
    return {"evidence_id": evidence_id, "removed": True}


def command_evidence_rebuild(args: argparse.Namespace) -> dict[str, Any]:
    artifact_id = require_identifier(args.artifact_id, "artifact id")
    run_dir, workspace = resolve_run_dir(args.run_dir)
    with run_lock(run_dir):
        run = load_run(run_dir)
        ensure_immutable_risk(run, workspace)
        ensure_run_open(run)
        target, bundle, digest = rebuild_bundle(run, artifact_id, workspace)
        write_bundle_atomic(target, bundle)
        ensure_immutable_risk(run, workspace)
        write_json_atomic(run_dir / "RUN.json", run)
    return {"artifact_id": artifact_id, "digest": digest}


def profile_judgement_gate(profile: str) -> str:
    return lifecycle.profile_judgement_gate(profile, PROFILE_PATH, ReceiptError)


def add_review_artifact(
    run: dict[str, Any], workspace: Path, *, artifact_id: str, path: str,
) -> tuple[dict[str, Any], str, str, bytes]:
    return lifecycle.add_review_artifact(
        run, workspace, artifact_id=artifact_id, path=path, api=globals(),
    )


def command_review_add(args: argparse.Namespace) -> dict[str, Any]:
    return lifecycle.command_review_add(args, globals())


def command_transition(args: argparse.Namespace) -> dict[str, Any]:
    return lifecycle.command_transition(args, globals())


def build_parser() -> argparse.ArgumentParser:
    return lifecycle.build_parser(globals())


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
