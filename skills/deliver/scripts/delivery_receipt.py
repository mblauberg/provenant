#!/usr/bin/env python3
"""The sole producer for canonical delivery-run ``RUN.json`` receipts."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from typing import Any, Callable, Iterator

SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))
import delivery_receipt_lifecycle as lifecycle
import delivery_receipt_process as process_runner
import delivery_receipt_compatibility as compatibility

PRODUCT_ROOT = Path(__file__).resolve().parents[3]
TEMPLATE_PATH = PRODUCT_ROOT / "skills" / "deliver" / "templates" / "RUN.template.json"
PROFILE_PATH = PRODUCT_ROOT / "config" / "delivery-profiles.json"
RISK_POLICY_PATH = PRODUCT_ROOT / "config" / "risk-policy.json"
LIFECYCLE_PATH = PRODUCT_ROOT / "skills" / "deliver" / "contract" / "lifecycle.v1.json"

IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
RISKS = ("routine", "substantial", "crucial", "terminal")
CHAIR_FAMILIES = {"openai", "anthropic"}
SAFE_CLASSES = {"canonical", "evidence", "handoff", "scratch", "external"}
REVIEW_ROLES = {"targeted", "other-primary", "distinct-family"}
BIND_SECTIONS = {
    "design": "design",
    "incident": "incident",
    "measures": "measures",
    "retrospective": "retrospective",
    "assurance-plan": "assurance",
    "security-plan": "security",
    "observation-plan": "observation",
    "software-delivery": "software_delivery",
}
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
MAX_LOG_BYTES = 64 * 1024
EVIDENCE_TIMEOUT_SECONDS = 30 * 60
LOCK_TIMEOUT_SECONDS = 5.0
TIMESTAMP_FIELDS = {
    "at", "started_at", "finished_at", "recorded_at", "observed_at", "updated_at",
}
SCAFFOLD_ENTRIES = {
    ".RUN.lock", "findings", "crossfamily", "traces", "patches", "MANIFEST.md",
    "RUN_RECEIPT.json", "decisions.md", "SYNTHESIS.md", "FINAL_GATE.md",
    "intent.md", "authority-approval.json",
}

class ReceiptError(ValueError):
    """A producer operation was refused."""

def build_scenario_receipt(case: dict[str, Any], fixture: dict[str, Any], root: Path = PRODUCT_ROOT) -> dict[str, Any]:
    import delivery_receipt_reference as reference_fixtures
    return reference_fixtures.build_scenario_receipt(case, fixture, root)

def build_reference_run(profile_name: str, root: Path = PRODUCT_ROOT, *, high_stakes: bool = False) -> dict[str, Any]:
    import delivery_receipt_reference as reference_fixtures
    return reference_fixtures.make_reference_run(profile_name, root, high_stakes=high_stakes)
def utc_now() -> str:
    """Return the producer clock in an unambiguous UTC representation."""
    return datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")

def digest_bytes(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()
def parse_utc(value: Any, field: str) -> datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise ReceiptError(f"{field} must be a UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value[:-1])
    except ValueError as exc:
        raise ReceiptError(f"{field} must be an ISO UTC timestamp") from exc
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)

def reject_future(value: Any, field: str) -> None:
    if parse_utc(value, field) > datetime.now(timezone.utc) + timedelta(minutes=5):
        raise ReceiptError(f"{field} exceeds the future timestamp tolerance")

def require_identifier(value: str, field: str) -> str:
    if not isinstance(value, str) or not IDENTIFIER.fullmatch(value):
        raise ReceiptError(f"{field} must be a bounded stable identifier")
    return value
def _json_object(value: str | None, field: str) -> dict[str, Any]:
    if value is None:
        raise ReceiptError(f"{field} is required")
    candidate = Path(value[1:] if value.startswith("@") else value)
    raw: str
    if value.startswith("@") or (not value.lstrip().startswith("{") and candidate.is_file()):
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
def _safe_path(workspace: Path, value: str, field: str) -> tuple[Path, str]:
    if not isinstance(value, str) or not value:
        raise ReceiptError(f"{field} must be safe and workspace-relative")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        raise ReceiptError(f"{field} must be safe and workspace-relative")
    target = (workspace / relative).resolve()
    try:
        target.relative_to(workspace.resolve())
    except ValueError as exc:
        raise ReceiptError(f"{field} escapes the workspace") from exc
    return target, relative.as_posix().rstrip("/") or "."

def safe_workspace_path(workspace: Path, value: str, field: str) -> tuple[Path, str]:
    return _safe_path(workspace.resolve(), value, field)

def _scoped_path(workspace: Path, scope: str, field: str) -> Path:
    target, _ = safe_workspace_path(workspace, scope, field)
    return target

def ensure_scope(run: dict[str, Any], workspace: Path, target: Path, field: str) -> None:
    authority = run.get("authority")
    scopes = authority.get(field) if isinstance(authority, dict) else None
    if not isinstance(scopes, list) or not scopes:
        raise ReceiptError(f"authority.{field} must be a non-empty list")
    resolved = target.resolve()
    roots = [_scoped_path(workspace, value, f"authority.{field}") for value in scopes]
    if not any(resolved == root or resolved.is_relative_to(root) for root in roots):
        raise ReceiptError(f"{field.removeprefix('allowed_')} leaves authority.{field}")

def ensure_allowed_artifact_target(run: dict[str, Any], workspace: Path, target: Path) -> None:
    ensure_scope(run, workspace, target, "allowed_artifact_paths")

def ensure_allowed_source_target(run: dict[str, Any], workspace: Path, target: Path) -> None:
    ensure_scope(run, workspace, target, "allowed_source_paths")

def _lexical_absolute(value: str | Path) -> Path:
    candidate = Path(value)
    if not candidate.is_absolute():
        candidate = Path.cwd() / candidate
    return Path(os.path.abspath(os.fspath(candidate)))

def _reject_symlink(path: Path, field: str) -> None:
    if path.is_symlink():
        raise ReceiptError(f"{field} must not be a symlink: {path}")

def resolve_receipt(
    value: str | Path,
    *,
    run_id: str | None = None,
    allow_missing: bool = False,
    workspace_root: Path | None = None,
) -> tuple[Path, Path, Path]:
    """Resolve the only receipt identity accepted by producer entry points.

    The input may name the canonical run directory for compatibility or its
    exact ``RUN.json`` child.  The workspace is taken from the current working
    directory only after the lexical path has been proved to be its
    ``.agent-run/<run_id>`` child.  No arbitrary path is ever resolved first.
    """
    candidate = _lexical_absolute(value)
    if candidate.name == "RUN.json":
        run_candidate = candidate.parent
    elif candidate.name:
        run_candidate = candidate
    else:
        raise ReceiptError("receipt path must be exactly .agent-run/<run_id>/RUN.json")
    candidate_run_id = run_candidate.name
    require_identifier(candidate_run_id, "run-id")
    if run_id is not None and candidate_run_id != run_id:
        raise ReceiptError("run-dir name must match run-id")

    workspace = (workspace_root or Path.cwd()).resolve()
    agent_run = workspace / ".agent-run"
    expected_run = agent_run / candidate_run_id
    expected_receipt = expected_run / "RUN.json"
    if run_candidate != expected_run:
        raise ReceiptError("receipt path must be beneath the current workspace .agent-run/<run_id>")
    _reject_symlink(agent_run, ".agent-run")
    _reject_symlink(run_candidate, "run directory")
    _reject_symlink(expected_receipt, "RUN.json")
    _reject_symlink(expected_run / ".RUN.lock", "receipt lock")
    if not allow_missing and (not expected_run.is_dir() or not expected_receipt.is_file()):
        raise ReceiptError("run-dir does not contain RUN.json")
    return expected_run, expected_receipt, workspace

def resolve_run_dir(value: str | Path, *, run_id: str | None = None) -> tuple[Path, Path]:
    run_dir, _receipt, workspace = resolve_receipt(value, run_id=run_id, allow_missing=True)
    return run_dir, workspace

def _receipt_path(value: str | Path) -> tuple[Path, Path]:
    run_dir, receipt, _workspace = resolve_receipt(value)
    return run_dir, receipt

def fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)

@contextmanager
def run_lock(
    run_dir: Path, *, timeout_seconds: float = LOCK_TIMEOUT_SECONDS,
    workspace_root: Path | None = None,
) -> Iterator[None]:
    run_dir, _receipt, _workspace = resolve_receipt(
        run_dir, allow_missing=True, workspace_root=workspace_root,
    )
    run_dir.mkdir(parents=True, exist_ok=True)
    _reject_symlink(run_dir, "run directory")
    lock_path = run_dir / ".RUN.lock"
    _reject_symlink(lock_path, "receipt lock")
    started = time.monotonic()
    flags = os.O_CREAT | os.O_RDWR
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(lock_path, flags, 0o600)
    except OSError as exc:
        if lock_path.is_symlink():
            raise ReceiptError(f"receipt lock must not be a symlink: {lock_path}") from exc
        raise
    with os.fdopen(descriptor, "a+") as handle:
        while True:
            try:
                fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() - started >= timeout_seconds:
                    raise ReceiptError(
                        f"could not acquire {lock_path} within {timeout_seconds:g}s; "
                        "another receipt writer may be active"
                    )
                time.sleep(0.02)
        try:
            if _receipt.is_file() and not _receipt.is_symlink():
                _reconcile_binding_journals(run_dir, _receipt, _workspace)
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)

def _written_shape(value: Any) -> None:
    if not isinstance(value, dict):
        raise ReceiptError("RUN.json root must be an object")
    if value.get("schema_version") != 1 or value.get("contract") != "delivery-run":
        raise ReceiptError("RUN.json must be a canonical delivery-run v1 receipt")
    if not isinstance(value.get("run_id"), str) or not IDENTIFIER.fullmatch(value["run_id"]):
        raise ReceiptError("RUN.json run_id must be a bounded stable identifier")
    if value.get("risk_tier") not in RISKS:
        raise ReceiptError("RUN.json risk_tier is invalid")
    if not isinstance(value.get("state_history"), list) or not value["state_history"]:
        raise ReceiptError("RUN.json state_history must be non-empty")
    first_history = value["state_history"][0]
    if not isinstance(first_history, dict) or first_history.get("state") != "draft":
        raise ReceiptError("RUN.json state_history must start at draft")
    previous: datetime | None = None
    transitions = _transitions()
    side_states = _side_states()
    for index, row in enumerate(value["state_history"]):
        if not isinstance(row, dict) or not isinstance(row.get("state"), str):
            raise ReceiptError(f"state_history[{index}] is invalid")
        timestamp = parse_utc(row.get("at"), f"state_history[{index}].at")
        reject_future(row["at"], f"state_history[{index}].at")
        if previous is not None and timestamp <= previous:
            raise ReceiptError("state history timestamps must strictly increase")
        previous = timestamp
        if index:
            previous_row = value["state_history"][index - 1]
            previous_state = previous_row.get("state")
            if row["state"] in side_states:
                if previous_state in side_states or row.get("resume_state") != previous_state:
                    raise ReceiptError("side state must resume the state it interrupted")
                if not row.get("reason") or not row.get("recovery"):
                    raise ReceiptError("side state requires reason and recovery")
            elif previous_state in side_states:
                if row["state"] != previous_row.get("resume_state"):
                    raise ReceiptError("invalid lifecycle side-state recovery")
            elif row["state"] not in transitions.get(previous_state, set()):
                raise ReceiptError(f"invalid lifecycle transition {previous_state} -> {row['state']}")
    if value.get("status") != value["state_history"][-1].get("state"):
        raise ReceiptError("RUN.json status must equal its final state")
    for key in ("artifacts", "evidence", "reviews"):
        rows = value.get(key)
        if not isinstance(rows, list):
            raise ReceiptError(f"RUN.json {key} must be a list")
        # The pinned reference contract predates explicit review IDs.  Review
        # lineage is identified by its evidence_id, while producer-created
        # review rows do carry an id.  Do not treat legacy absent IDs as a
        # duplicate; continue enforcing uniqueness for every explicit ID.
        identifiers = []
        for index, row in enumerate(rows):
            if not isinstance(row, dict):
                raise ReceiptError(f"RUN.json {key}[{index}] must be an object")
            identifier = row.get("id")
            if identifier is None and key == "reviews":
                continue
            identifiers.append(require_identifier(identifier, f"RUN.json {key}[{index}].id"))
        if len(identifiers) != len(set(identifiers)):
            raise ReceiptError(f"RUN.json {key} contains duplicate ids")

def _checkpoint_compatibility_shape(value: Any) -> None:
    compatibility.validate_checkpoint(value, ReceiptError, _written_shape)


def _load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"RUN.json is unreadable: {exc}") from exc


def load_run(run_dir: Path, *, workspace_root: Path | None = None) -> dict[str, Any]:
    _run_dir, receipt, _workspace = resolve_receipt(
        run_dir, workspace_root=workspace_root,
    )
    value = _load_json(receipt)
    _written_shape(value)
    return value


def create_json_exclusive(path: Path, value: dict[str, Any]) -> None:
    """Create the canonical receipt once; never replace an existing authority."""
    if not isinstance(value, dict):
        raise ReceiptError("initial receipt must be an object")
    _written_shape(value)
    _run_dir, receipt, _workspace = resolve_receipt(path, allow_missing=True)
    if receipt.exists() or receipt.is_symlink():
        raise ReceiptError(f"run-dir already contains a canonical receipt: {receipt.parent}")
    raw = (json.dumps(value, indent=2) + "\n").encode()
    flags = os.O_CREAT | os.O_EXCL | os.O_WRONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor: int | None = None
    try:
        descriptor = os.open(receipt, flags, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = None
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        fsync_directory(receipt.parent)
    except FileExistsError as exc:
        raise ReceiptError(f"run-dir already contains a canonical receipt: {receipt.parent}") from exc
    finally:
        if descriptor is not None:
            os.close(descriptor)


def write_json_atomic(
    path: Path, value: dict[str, Any], *, validate: bool = True,
    workspace_root: Path | None = None,
) -> None:
    if validate:
        _written_shape(value)
    _run_dir, path, _workspace = resolve_receipt(path, workspace_root=workspace_root)
    if path.is_symlink():
        raise ReceiptError(f"RUN.json must not be a symlink: {path}")
    if not path.parent.is_dir():
        raise ReceiptError("receipt parent directory does not exist")
    temporary: Path | None = None
    raw = (json.dumps(value, indent=2) + "\n").encode()
    try:
        with tempfile.NamedTemporaryFile("wb", dir=path.parent, prefix=".RUN.", suffix=".tmp", delete=False) as handle:
            temporary = Path(handle.name)
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        fsync_directory(path.parent)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()
    reread = _load_json(path)
    if reread != value:
        raise ReceiptError("atomic receipt write verification failed")
    if validate:
        _written_shape(reread)


def write_bytes_atomic(path: Path, raw: bytes) -> bool:
    """Publish a run-owned artifact without replacing a conflicting file."""
    if path.is_symlink():
        raise ReceiptError(f"run-owned artifact must not be a symlink: {path}")
    path = Path(os.path.abspath(os.fspath(path)))
    if path.exists():
        if path.read_bytes() != raw:
            raise ReceiptError(f"refusing to replace conflicting artifact: {path}")
        return False
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile("wb", dir=path.parent, prefix=".RUN.", suffix=".tmp", delete=False) as handle:
            temporary = Path(handle.name)
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        fsync_directory(path.parent)
        return True
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()


def _transitions() -> dict[str, set[str]]:
    try:
        contract = json.loads(LIFECYCLE_PATH.read_text())
        rows = contract["transitions"]
        states = contract["states"]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"delivery lifecycle contract is unreadable: {exc}") from exc
    if not isinstance(states, list) or not isinstance(rows, list):
        raise ReceiptError("delivery lifecycle contract is invalid")
    transitions = {state: set() for state in states}
    for row in rows:
        if not isinstance(row, dict) or row.get("state") not in transitions or row.get("to_state") not in transitions:
            raise ReceiptError("delivery lifecycle contract contains an invalid transition")
        transitions[row["state"]].add(row["to_state"])
    return transitions


def _side_states() -> set[str]:
    try:
        contract = json.loads(LIFECYCLE_PATH.read_text())
        states = contract["side_states"]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"delivery lifecycle contract is unreadable: {exc}") from exc
    if not isinstance(states, list) or any(not isinstance(state, str) for state in states):
        raise ReceiptError("delivery lifecycle contract side states are invalid")
    return set(states)


def _timestamp_values(value: Any, key: str = "") -> Iterator[tuple[str, str]]:
    if isinstance(value, dict):
        for name, child in value.items():
            if name in TIMESTAMP_FIELDS and isinstance(child, str) and child:
                yield name, child
            yield from _timestamp_values(child, name)
    elif isinstance(value, list):
        for child in value:
            yield from _timestamp_values(child, key)


def _latest_timestamp(run: dict[str, Any]) -> datetime | None:
    latest: datetime | None = None
    for field, value in _timestamp_values(run):
        parsed = parse_utc(value, field)
        reject_future(value, field)
        if latest is None or parsed > latest:
            latest = parsed
    return latest


def timestamp_after(run: dict[str, Any], *, minimum: datetime | None = None) -> str:
    value = utc_now()
    parsed = parse_utc(value, "producer timestamp")
    latest = _latest_timestamp(run)
    if minimum is not None and parsed <= minimum:
        raise ReceiptError("producer timestamp must strictly increase")
    if latest is not None and parsed <= latest:
        raise ReceiptError("producer timestamp must strictly increase")
    return value


def derive_risk(assessment: dict[str, Any]) -> str:
    try:
        policy = json.loads(RISK_POLICY_PATH.read_text())
        factors = policy["factors"]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"risk policy is unreadable: {exc}") from exc
    if set(assessment) != set(factors):
        raise ReceiptError("risk-assessment must cover every policy factor")
    index = 0
    for factor, choices in factors.items():
        selected = assessment.get(factor)
        if selected not in choices:
            raise ReceiptError(f"risk-assessment.{factor} is invalid")
        index = max(index, RISKS.index(choices[selected]))
    return RISKS[index]


def _check_override(override: dict[str, Any], derived: str) -> None:
    if (
        override.get("status") != "approved"
        or not str(override.get("approved_by", "")).strip()
        or not override.get("evidence")
        or not override.get("reason")
    ):
        raise ReceiptError(f"risk tier below derived {derived} requires an approved human override")


def ensure_immutable_risk(run: dict[str, Any], workspace: Path) -> None:
    history = run.get("state_history")
    if not isinstance(history, list) or not history:
        raise ReceiptError("state_history must retain its initial draft row")
    initial = history[0].get("risk_tier")
    if initial is not None and (initial not in RISKS or run.get("risk_tier") != initial):
        raise ReceiptError("risk tier is immutable after init")
    _latest_timestamp(run)
    derived = derive_risk(run.get("risk_assessment", {}))
    if RISKS.index(run.get("risk_tier")) < RISKS.index(derived):
        override = run.get("risk_override")
        if not isinstance(override, dict):
            raise ReceiptError(f"risk tier below derived {derived} requires an approved human override")
        _check_override(override, derived)
        evidence = next(
            (item for item in run.get("evidence", [])
             if isinstance(item, dict) and item.get("id") == override.get("evidence")),
            None,
        )
        if not isinstance(evidence, dict) or evidence.get("status") != "pass" or evidence.get("gate") != "risk-override":
            raise ReceiptError("risk override evidence is not a passing risk-override row")
        artifact = find_artifact(run, evidence.get("artifact_id", ""))
        target, _ = safe_workspace_path(workspace, artifact.get("path", ""), "risk override artifact")
        ensure_allowed_artifact_target(run, workspace, target)
        if not target.is_file() or artifact.get("digest") != digest_bytes(target.read_bytes()):
            raise ReceiptError("risk override artifact digest does not match its live bytes")
        if evidence.get("artifact_digest") != artifact.get("digest"):
            raise ReceiptError("risk override evidence digest does not match its artifact")


def ensure_mutable(run: dict[str, Any]) -> None:
    if run.get("status") == "closed":
        raise ReceiptError("closed run is immutable")


def _mutate(
    value: str | Path,
    mutation: Callable[[dict[str, Any], Path, Path], dict[str, Any] | None],
    *,
    workspace: Path | None = None,
    validation: str = "canonical",
) -> dict[str, Any]:
    if validation not in {"canonical", "checkpoint-compatibility"}:
        raise ReceiptError(f"unknown receipt validation mode: {validation}")
    run_dir, receipt, canonical_workspace = resolve_receipt(value)
    if workspace is not None and Path(workspace).resolve() != canonical_workspace:
        raise ReceiptError("mutation workspace must be the canonical current workspace")
    working_workspace = canonical_workspace
    with run_lock(run_dir):
        value_before = _load_json(receipt)
        if not isinstance(value_before, dict) or value_before.get("contract") != "delivery-run" or value_before.get("schema_version") != 1:
            raise ReceiptError("RUN.json must be a canonical delivery-run v1 receipt")
        if validation == "canonical":
            _written_shape(value_before)
            ensure_immutable_risk(value_before, working_workspace)
            if value_before.get("status") == "closed":
                raise ReceiptError("closed run is immutable")
        else:
            _checkpoint_compatibility_shape(value_before)
            if "run_id" in value_before:
                ensure_immutable_risk(value_before, working_workspace)
        result = mutation(value_before, run_dir, working_workspace) or {}
        value_before["updated_at"] = timestamp_after(value_before)
        if validation == "canonical":
            ensure_immutable_risk(value_before, working_workspace)
            _written_shape(value_before)
        else:
            _checkpoint_compatibility_shape(value_before)
        write_json_atomic(receipt, value_before, validate=validation == "canonical")
        if validation == "checkpoint-compatibility":
            _checkpoint_compatibility_shape(_load_json(receipt))
        return result


def mutate_receipt(
    value: str | Path,
    mutation: Callable[[dict[str, Any], Path, Path], dict[str, Any] | None],
    *,
    workspace: Path | None = None,
    validation: str = "canonical",
) -> dict[str, Any]:
    return _mutate(value, mutation, workspace=workspace, validation=validation)


def _reconcile_binding_journals(run_dir: Path, receipt: Path, workspace: Path) -> None:
    try:
        current = _load_json(receipt)
    except ReceiptError:
        return
    references = {
        (item.get("path"), item.get("digest"))
        for item in current.get("artifacts", [])
        if isinstance(item, dict)
    }
    for journal in sorted(run_dir.glob(".bind-recovery-*.json")):
        if journal.is_symlink():
            raise ReceiptError(f"binding recovery journal must not be a symlink: {journal}")
        try:
            value = _load_json(journal)
        except ReceiptError:
            continue
        for target in value.get("targets", []):
            if not isinstance(target, dict):
                continue
            relative = target.get("path")
            digest = target.get("digest")
            if (relative, digest) in references:
                continue
            if target.get("created"):
                path, _ = safe_workspace_path(workspace, relative, "binding recovery target")
                if path.is_file() and digest_bytes(path.read_bytes()) == digest:
                    path.unlink()
        journal.unlink()
    if any(run_dir.glob(".bind-recovery-*.json")):
        fsync_directory(run_dir)


def mutate_receipt_with_artifacts(
    value: str | Path,
    mutation: Callable[[dict[str, Any], Path, Path], dict[str, Any] | None],
    side_artifacts: list[tuple[str, bytes]],
    *,
    before_commit: Callable[[dict[str, Any], Path, Path], None] | None = None,
    workspace: Path | None = None,
) -> dict[str, Any]:
    """Own a receipt mutation and its run-local side-artifact commit."""
    run_dir, receipt, canonical_workspace = resolve_receipt(value, workspace_root=workspace)
    workspace = canonical_workspace
    with run_lock(run_dir, workspace_root=workspace):
        run = load_run(run_dir, workspace_root=workspace)
        ensure_immutable_risk(run, workspace)
        ensure_mutable(run)
        result = mutation(run, run_dir, workspace) or {}
        run["updated_at"] = timestamp_after(run)
        ensure_immutable_risk(run, workspace)
        _written_shape(run)
        old_receipt = receipt.read_bytes()
        journal_digest = digest_bytes(old_receipt + b"".join(raw for _path, raw in side_artifacts))
        journal = run_dir / f".bind-recovery-{journal_digest.removeprefix('sha256:')}.json"
        targets: list[dict[str, Any]] = []
        published: list[Path] = []
        try:
            with tempfile.TemporaryDirectory(prefix=".bind-stage-", dir=run_dir) as temporary:
                stage = Path(temporary)
                for relative, raw in side_artifacts:
                    target, clean = safe_workspace_path(workspace, relative, "binding artifact")
                    ensure_allowed_artifact_target(run, workspace, target)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    if target.is_symlink():
                        raise ReceiptError(f"binding artifact must not be a symlink: {target}")
                    if target.exists() and target.read_bytes() != raw:
                        raise ReceiptError(f"refusing to replace conflicting binding artifact: {target}")
                    staged = stage / f"{len(targets)}.artifact"
                    staged.write_bytes(raw)
                    with staged.open("rb") as handle:
                        os.fsync(handle.fileno())
                    targets.append({"path": clean, "digest": digest_bytes(raw), "created": not target.exists()})
                journal_raw = (json.dumps({"schema_version": 1, "targets": targets}, sort_keys=True) + "\n").encode()
                write_bytes_atomic(journal, journal_raw)
                for index, (relative, _raw) in enumerate(side_artifacts):
                    target, _clean = safe_workspace_path(workspace, relative, "binding artifact")
                    if not target.exists():
                        os.replace(stage / f"{index}.artifact", target)
                        published.append(target)
                if side_artifacts:
                    fsync_directory(safe_workspace_path(workspace, side_artifacts[0][0], "binding artifact")[0].parent)
                if before_commit is not None:
                    before_commit(run, run_dir, workspace)
                write_json_atomic(receipt, run, workspace_root=workspace)
                journal.unlink(missing_ok=True)
                fsync_directory(run_dir)
        except Exception:
            try:
                receipt_is_old = receipt.read_bytes() == old_receipt
            except OSError:
                receipt_is_old = False
            if receipt_is_old:
                for target in published:
                    if target.is_file():
                        target.unlink()
                if journal.exists():
                    journal.unlink()
                if published:
                    fsync_directory(published[0].parent)
            raise
        return result


import delivery_receipt_api as _api_adapter


def _api() -> dict[str, Any]:
    return _api_adapter.build(_module_api())


def command_init(args: argparse.Namespace) -> dict[str, Any]:
    run_id = require_identifier(args.run_id, "run-id")
    if args.profile not in json.loads(PROFILE_PATH.read_text()).get("profiles", {}):
        raise ReceiptError(f"unknown delivery profile: {args.profile}")
    if args.chair_family not in CHAIR_FAMILIES:
        raise ReceiptError("chair-family must be a primary family (openai or anthropic)")
    run_dir, _receipt, workspace = resolve_receipt(args.run_dir, run_id=run_id, allow_missing=True)
    intent_target, intent_relative = safe_workspace_path(workspace, args.intent, "intent")
    intent_raw = intent_target.read_bytes() if intent_target.is_file() else b""
    if not intent_raw:
        raise ReceiptError("intent must reference an existing non-empty file")
    assessment = _json_object(args.risk_assessment, "risk-assessment")
    authority = _json_object(args.authority, "authority")
    ensure_allowed_artifact_target({"authority": authority}, workspace, intent_target)
    derived = derive_risk(assessment)
    declared = args.risk_tier or derived
    if declared not in RISKS:
        raise ReceiptError("risk-tier is invalid")
    override = {"status": "not-required", "approved_by": "", "evidence": "", "reason": ""}
    override_artifact = None
    override_evidence = None
    if RISKS.index(declared) < RISKS.index(derived):
        if not args.risk_override:
            raise ReceiptError(f"risk tier below derived {derived} requires an approved human override")
        override_input = _json_object(args.risk_override, "risk-override")
        _check_override(override_input, derived)
        artifact_id = require_identifier(str(override_input.get("artifact_id", "")), "risk override artifact id")
        evidence_id = require_identifier(str(override_input.get("evidence", "")), "risk override evidence id")
        if artifact_id == "intent":
            raise ReceiptError("risk override artifact id conflicts with reserved intent")
        if evidence_id in {"authority-approval", "intent-approval"}:
            raise ReceiptError("risk override evidence id conflicts with reserved approval id")
        target, relative = safe_workspace_path(workspace, str(override_input.get("artifact", "")), "risk override artifact")
        ensure_allowed_artifact_target({"authority": authority}, workspace, target)
        raw = target.read_bytes() if target.is_file() else b""
        if not raw:
            raise ReceiptError("risk override requires an existing non-empty approval artifact")
        override = {
            key: str(override_input[key])
            for key in ("status", "approved_by", "evidence", "reason")
        }
        override_artifact = {
            "id": artifact_id, "path": relative,
            "media_type": "application/json" if target.suffix == ".json" else "text/plain",
            "artifact_type": "evidence", "digest": digest_bytes(raw),
            "class": "evidence", "owner": override["approved_by"], "retention": "risk-policy",
        }
        override_evidence = {
            "id": evidence_id, "kind": "human", "gate": "risk-override", "status": "pass",
            "method": "producer-bound risk override artifact", "artifact_id": artifact_id,
            "source_paths": [], "approver": override["approved_by"],
            "artifact_digest": override_artifact["digest"], "recorded_at": utc_now(),
        }
    elif args.risk_override:
        raise ReceiptError("risk-override is only valid below the derived tier")
    try:
        template = json.loads(TEMPLATE_PATH.read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"RUN template is unreadable: {exc}") from exc
    if not isinstance(template, dict):
        raise ReceiptError("RUN template must be an object")
    relationships = _json_object(args.fabric_relationships, "fabric-relationships") if args.fabric_relationships else {
        "mode": "independent", "delivery_run_id": run_id,
        "project_session_id": "not_applicable", "coordination_run_id": "not_applicable",
        "workstream_id": "not_applicable", "lead_agent_id": "not_applicable",
    }
    timestamp = utc_now()
    owner = str(authority.get("approved_by") or "human-maintainer")
    run = copy.deepcopy(template)
    run.update({
        "run_id": run_id, "fabric_relationships": relationships, "profile": args.profile,
        "status": "draft", "risk_tier": declared, "chair_family": args.chair_family,
        "risk_assessment": assessment, "risk_override": override,
        "authority": authority,
        "artifacts": [{
            "id": "intent", "path": intent_relative, "media_type": "text/markdown",
            "artifact_type": "documentation" if args.profile == "software" else template["artifacts"][0]["artifact_type"],
            "digest": digest_bytes(intent_raw), "class": "canonical", "owner": owner,
            "retention": "project-policy",
        }],
        "state_history": [{"state": "draft", "at": timestamp, "evidence_ids": [], "risk_tier": declared}],
        "evidence": [], "reviews": [], "repair_cycles": 0,
        "intent": {
            "artifact": intent_relative, "digest": digest_bytes(intent_raw), "decision_owner": owner,
            "approval": {"status": "pending", "approver": "", "evidence": ""},
        },
        "checkpoint": {"generation": 0, "current_slice": "draft", "next_action": "complete scope and authority", "in_flight": [], "artifact_paths": ["RUN.json"]},
    })
    if override_artifact:
        run["artifacts"].append(override_artifact)
        run["evidence"].append(override_evidence)
    with run_lock(run_dir):
        if not run_dir.is_dir():
            raise ReceiptError("canonical run directory is unavailable")
        if (run_dir / "RUN.json").exists() or (run_dir / "RUN.json").is_symlink():
            raise ReceiptError(f"run-dir already contains a canonical receipt: {run_dir}")
        unexpected = sorted(
            item.name for item in run_dir.iterdir() if item.name not in SCAFFOLD_ENTRIES
        )
        if unexpected:
            raise ReceiptError(
                "run-dir contains non-scaffold files: " + ", ".join(unexpected)
            )
        create_json_exclusive(run_dir / "RUN.json", run)
    return {"path": str(run_dir / "RUN.json"), "run_id": run_id, "risk_tier": declared}


def find_artifact(run: dict[str, Any], artifact_id: str) -> dict[str, Any]:
    matches = [item for item in run.get("artifacts", []) if isinstance(item, dict) and item.get("id") == artifact_id]
    if len(matches) != 1:
        raise ReceiptError(f"unknown or duplicate artifact id: {artifact_id}")
    return matches[0]


def ensure_artifact_contract(
    run: dict[str, Any], artifact_class: str, artifact_type: str,
    media_type: str, owner: str, retention: str,
) -> None:
    """Enforce the narrow producer-side artifact shape shared with validation."""
    if artifact_class not in SAFE_CLASSES:
        raise ReceiptError("artifact class is invalid")
    if not all(isinstance(value, str) and value.strip() for value in (media_type, artifact_type, owner, retention)):
        raise ReceiptError("artifact media type, type, owner and retention are required")
    try:
        registry = json.loads(PROFILE_PATH.read_text())
        profile = registry["profiles"][run["profile"]]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"delivery profile registry is unreadable: {exc}") from exc
    if artifact_class == "canonical":
        if artifact_type not in profile.get("artifact_types", []):
            raise ReceiptError("canonical artifact type is outside the selected profile")
    else:
        expected = {
            "evidence": "evidence", "handoff": "handoff", "scratch": "scratch",
            "external": "external-reference",
        }[artifact_class]
        if artifact_type != expected:
            raise ReceiptError("artifact type does not match its class")
    if artifact_class == "evidence" and retention not in profile.get("evidence_policy", {}).get("retention", []):
        raise ReceiptError("evidence artifact retention violates the profile policy")


def ensure_new_evidence_id(run: dict[str, Any], evidence_id: str) -> None:
    require_identifier(evidence_id, "evidence id")
    if any(isinstance(item, dict) and item.get("id") == evidence_id for item in run.get("evidence", [])):
        raise ReceiptError(f"evidence id already exists: {evidence_id}")


def command_bind(args: argparse.Namespace) -> dict[str, Any]:
    section = BIND_SECTIONS[args.section]
    value = _json_object(args.from_json, args.section)

    def apply(run: dict[str, Any], _run_dir: Path, _workspace: Path) -> dict[str, Any]:
        passed = any(isinstance(row, dict) and row.get("state") == BIND_GATE_STATES[args.section] for row in run.get("state_history", []))
        if passed and run.get(section) != value:
            raise ReceiptError(f"{args.section.removesuffix('-plan')} lifecycle gate has passed")
        run[section] = value
        return {"section": args.section}
    return mutate_receipt(args.run_dir, apply)


def command_artifact_add(args: argparse.Namespace) -> dict[str, Any]:
    artifact_id = require_identifier(args.artifact_id, "artifact id")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        if any(isinstance(item, dict) and item.get("id") == artifact_id for item in run.get("artifacts", [])):
            raise ReceiptError(f"artifact id already exists: {artifact_id}")
        target, relative = safe_workspace_path(workspace, args.path, "artifact path")
        ensure_allowed_artifact_target(run, workspace, target)
        if not target.is_file():
            raise ReceiptError("artifact path must reference an existing file")
        raw = target.read_bytes()
        ensure_artifact_contract(
            run, args.artifact_class, args.artifact_type, args.media_type,
            args.owner, args.retention,
        )
        run["artifacts"].append({
            "id": artifact_id, "path": relative, "media_type": args.media_type,
            "artifact_type": args.artifact_type, "digest": digest_bytes(raw),
            "class": args.artifact_class, "owner": args.owner, "retention": args.retention,
        })
        return {"artifact_id": artifact_id, "digest": run["artifacts"][-1]["digest"]}
    return mutate_receipt(args.run_dir, apply)


def _bundle_artifact(run: dict[str, Any], artifact_id: str, workspace: Path) -> tuple[dict[str, Any], Path]:
    artifact = find_artifact(run, artifact_id)
    if any(
        isinstance(item, dict) and item.get("artifact_id") == artifact_id
        and item.get("kind") != "deterministic"
        for item in run.get("evidence", [])
    ):
        raise ReceiptError("deterministic bundle artifacts are exclusive to deterministic evidence")
    if artifact.get("class") != "evidence" or artifact.get("artifact_type") != "evidence" or artifact.get("media_type") != "application/json" or not artifact.get("path"):
        raise ReceiptError("deterministic bundle artifact must be local JSON evidence")
    target, _ = safe_workspace_path(workspace, artifact["path"], "evidence bundle path")
    ensure_allowed_artifact_target(run, workspace, target)
    if not target.is_file() or artifact.get("digest") != digest_bytes(target.read_bytes()):
        raise ReceiptError("deterministic bundle artifact does not match its live bytes")
    return artifact, target


def _bundle_bytes(run: dict[str, Any], artifact_id: str) -> bytes:
    rows = [row for row in run.get("evidence", []) if isinstance(row, dict) and row.get("kind") == "deterministic" and row.get("artifact_id") == artifact_id]
    checks = [{
        "id": row["id"], "gate": row["gate"], "status": row["status"], "method": row["method"],
        "source_paths": row["source_paths"], "started_at": row["started_at"],
        "finished_at": row["finished_at"], "result": {
            key: value for key, value in row["result"].items() if key != "receipt_digest"
        },
    } for row in rows]
    return (json.dumps({"schema_version": 1, "contract": "deterministic-evidence-bundle", "checks": checks}, indent=2) + "\n").encode()


def _hashed_bundle_path(original: Path, artifact_id: str, digest: str) -> Path:
    return original.parent / f"{artifact_id}.{digest.removeprefix('sha256:')}.json"


def _publish_bundle_and_receipt(
    receipt: Path, run: dict[str, Any], bundle_path: Path, bundle_raw: bytes,
) -> str:
    digest = digest_bytes(bundle_raw)
    existed_before = bundle_path.exists()
    old_receipt = receipt.read_bytes()
    try:
        write_bytes_atomic(bundle_path, bundle_raw)
        write_json_atomic(receipt, run)
    except Exception:
        # If the receipt write failed before replacement, remove the newly
        # published bundle. If it failed after os.replace (for example while
        # fsyncing the directory), the new receipt is already visible and the
        # bundle must remain so the pair stays consistent for recovery.
        receipt_is_old = False
        try:
            receipt_is_old = receipt.read_bytes() == old_receipt
        except OSError:
            pass
        if receipt_is_old and not existed_before and bundle_path.exists():
            bundle_path.unlink()
            fsync_directory(bundle_path.parent)
        raise
    return digest


def execute_bounded(
    command: list[str], *, cwd: Path, timeout_seconds: float = EVIDENCE_TIMEOUT_SECONDS,
) -> dict[str, Any]:
    return process_runner.execute_bounded(
        command, cwd=cwd, timeout_seconds=timeout_seconds,
        max_log_bytes=MAX_LOG_BYTES, error_type=ReceiptError,
    )


import delivery_receipt_commands as _evidence_commands


def command_evidence_run(args: argparse.Namespace) -> dict[str, Any]:
    return _evidence_commands.command_evidence_run(args, _module_api())


def command_reference(args: argparse.Namespace) -> dict[str, Any]:
    return _evidence_commands.command_reference(args, _module_api())

def command_evidence_human(args: argparse.Namespace) -> dict[str, Any]:
    return _evidence_commands.command_evidence_human(args, _module_api())


def command_evidence_rebuild(args: argparse.Namespace) -> dict[str, Any]:
    return _evidence_commands.command_evidence_rebuild(args, _module_api())


import delivery_receipt_cli as _cli


def _module_api() -> Any:
    return sys.modules.get(__name__) or SimpleNamespace(**globals())


def build_parser() -> argparse.ArgumentParser:
    return _cli.build_parser(_module_api())


def main(argv: list[str] | None = None) -> int:
    return _cli.main(_module_api(), argv)


if __name__ == "__main__":
    raise SystemExit(main())
