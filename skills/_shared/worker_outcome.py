"""Join dispatch, worker and optional Git evidence at the chair boundary."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
from typing import Any


_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
_COMMIT = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
_REFERENCE_KEYS = {"path", "digest"}
_OUTCOME_KEYS = {
    "id", "dispatch_receipt", "terminal_artifact", "dispatch_terminal_artifact", "worktree_receipt",
}
_DISPATCH_KEYS = {
    "id", "attempt_id", "status", "exit", "terminal_observed", "output_path", "output_sha256",
}
_TERMINAL_KEYS = {"id", "attempt_id", "kind", "summary", "question", "reason", "verdict", "artifact_refs"}
_KINDS = {"complete", "question", "failed", "unavailable"}


def _failure(reason: str) -> dict[str, object]:
    return {"status": "rejected", "reason": reason}


def _digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def _reference(root: Path, value: object, label: str) -> tuple[Path | None, str | None]:
    if not isinstance(value, dict) or set(value) != _REFERENCE_KEYS:
        return None, f"{label} must contain only path and digest"
    path_value = value.get("path")
    digest = value.get("digest")
    path = Path(path_value) if isinstance(path_value, str) else Path("..")
    if (
        not isinstance(path_value, str)
        or path.is_absolute()
        or ".." in path.parts
        or not isinstance(digest, str)
        or not _DIGEST.fullmatch(digest)
    ):
        return None, f"{label} is invalid"
    target = root / path
    try:
        target.resolve().relative_to(root.resolve())
    except ValueError:
        return None, f"{label} escapes the run directory"
    if not target.is_file():
        return None, f"{label} is missing"
    try:
        actual = _digest(target)
    except OSError:
        return None, f"{label} is unreadable"
    if actual != digest:
        return None, f"{label} digest does not match"
    return target, None


def _json_file(path: Path, label: str) -> tuple[dict[str, Any] | None, str | None]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        return None, f"{label} is not valid JSON"
    if not isinstance(value, dict):
        return None, f"{label} must be an object"
    return value, None


def _path_matches(root: Path, output_path: object, artifact: Path) -> bool:
    if not isinstance(output_path, str) or not output_path:
        return False
    candidate = Path(output_path)
    if candidate.is_absolute():
        return candidate.resolve() == artifact.resolve()
    return (root / candidate).resolve() == artifact.resolve() and ".." not in candidate.parts


def _verify_answer(root: Path, dispatch: dict[str, Any]) -> str | None:
    """Verify the human answer independently from the terminal JSON artifact."""
    output_path = dispatch.get("output_path")
    output_digest = dispatch.get("output_sha256")
    if not isinstance(output_path, str) or not output_path:
        if dispatch.get("status") == "ok" and dispatch.get("exit") == 0:
            return "successful dispatch has no human answer path"
        return None
    if not isinstance(output_digest, str) or not _DIGEST.fullmatch(output_digest):
        return "human answer digest is unavailable"
    candidate = Path(output_path)
    target = candidate if candidate.is_absolute() else root / candidate
    try:
        target.resolve().relative_to(root.resolve())
    except ValueError:
        return "human answer path escapes the run directory"
    if not target.is_file():
        return "human answer is missing"
    try:
        actual = _digest(target)
    except OSError:
        return "human answer is unreadable"
    if actual != output_digest:
        return "human answer digest does not match"
    return None


def _validate_terminal(root: Path, terminal: dict[str, Any], *, outcome_id: str, attempt_id: str) -> str | None:
    if set(terminal) - _TERMINAL_KEYS:
        return "terminal artifact uses an open schema"
    if terminal.get("id") != outcome_id or terminal.get("attempt_id") != attempt_id:
        return "terminal artifact identity does not match dispatch"
    kind = terminal.get("kind")
    if kind not in _KINDS:
        return "terminal artifact kind is invalid"
    for field in ("summary", "question", "reason"):
        if field in terminal and not isinstance(terminal[field], str):
            return f"terminal artifact {field} must be a string"
    if "verdict" in terminal and (
        not isinstance(terminal["verdict"], str) or not terminal["verdict"].strip()
    ):
        return "terminal artifact verdict must be a non-empty string"
    if kind == "complete" and (
        not isinstance(terminal.get("summary"), str) or not terminal["summary"].strip()
    ):
        return "complete terminal artifact requires summary"
    if kind == "question" and (
        not isinstance(terminal.get("question"), str) or not terminal["question"].strip()
    ):
        return "question terminal artifact requires question"
    if kind in {"failed", "unavailable"} and (
        not isinstance(terminal.get("reason"), str) or not terminal["reason"].strip()
    ):
        return f"{kind} terminal artifact requires reason"
    required = {
        "complete": {"id", "attempt_id", "kind", "summary"},
        "question": {"id", "attempt_id", "kind", "question"},
        "failed": {"id", "attempt_id", "kind", "reason"},
        "unavailable": {"id", "attempt_id", "kind", "reason"},
    }[kind]
    if set(terminal) - required - {"verdict", "artifact_refs"}:
        return f"{kind} terminal artifact contains fields for another outcome kind"
    if "artifact_refs" in terminal:
        refs = terminal["artifact_refs"]
        if not isinstance(refs, list):
            return "terminal artifact_refs must be a list"
        for index, value in enumerate(refs):
            _path, error = _reference(root, value, f"terminal artifact_refs[{index}]")
            if error:
                return error
    return None


def _validate_worktree(root: Path, value: object) -> str | None:
    if value is None:
        return None
    path, error = _reference(root, value, "worktree_receipt")
    if error:
        return error
    assert path is not None
    receipt, error = _json_file(path, "worktree_receipt")
    if error:
        return error
    assert receipt is not None
    if receipt.get("status") != "accepted":
        return "worktree receipt is not accepted"
    if receipt.get("clean") is not True:
        return "worktree receipt does not prove a clean worktree"
    base = receipt.get("base_revision")
    head = receipt.get("head_revision")
    claimed = receipt.get("claimed_commit")
    if not all(isinstance(value, str) and _COMMIT.fullmatch(value) for value in (base, head, claimed)):
        return "worktree receipt is missing base or head evidence"
    if base.lower() == head.lower() or claimed.lower() != head.lower():
        return "worktree receipt does not prove a new claimed head"
    return None


def accept_worker_outcome(run_dir: Path, outcome: object) -> dict[str, object]:
    """Return a derived acceptance decision for one digest-bound attempt."""
    root = run_dir.expanduser().resolve()
    if not isinstance(outcome, dict) or set(outcome) not in (
        _OUTCOME_KEYS,
        _OUTCOME_KEYS - {"dispatch_terminal_artifact"},
    ):
        return _failure("worker outcome uses an invalid closed schema")
    outcome_id = outcome.get("id")
    if not isinstance(outcome_id, str) or not outcome_id.strip():
        return _failure("worker outcome id is required")

    dispatch_path, error = _reference(root, outcome.get("dispatch_receipt"), "dispatch_receipt")
    if error:
        return _failure(error)
    terminal_path, error = _reference(root, outcome.get("terminal_artifact"), "terminal_artifact")
    if error:
        return _failure(error)
    assert dispatch_path is not None and terminal_path is not None
    dispatch, error = _json_file(dispatch_path, "dispatch_receipt")
    if error:
        return _failure(error)
    terminal, error = _json_file(terminal_path, "terminal_artifact")
    if error:
        return _failure(error)
    assert dispatch is not None and terminal is not None

    if dispatch.get("id") != outcome_id:
        return _failure("dispatch receipt identity does not match worker outcome")
    attempt_id = dispatch.get("attempt_id")
    if not isinstance(attempt_id, str) or not attempt_id.strip():
        return _failure("dispatch receipt attempt_id is required")
    if dispatch.get("terminal_observed") is not True:
        return _failure("dispatch receipt does not prove observed terminality")
    exit_value = dispatch.get("exit")
    if not isinstance(exit_value, int) or isinstance(exit_value, bool):
        return _failure("dispatch receipt exit status is unavailable")
    error = _verify_answer(root, dispatch)
    if error:
        return _failure(error)
    terminal_path_value = dispatch.get("terminal_artifact_path")
    terminal_digest = outcome["terminal_artifact"]["digest"]
    if terminal_path_value or dispatch.get("terminal_artifact_sha256"):
        dispatch_terminal_ref = outcome.get("dispatch_terminal_artifact")
        dispatch_terminal_path, error = _reference(
            root, dispatch_terminal_ref, "dispatch_terminal_artifact"
        )
        if error:
            return _failure(error)
        assert dispatch_terminal_path is not None
        if not _path_matches(root, terminal_path_value, dispatch_terminal_path):
            return _failure("dispatch receipt terminal_artifact_path does not match dispatch terminal artifact")
        if dispatch.get("terminal_artifact_sha256") != dispatch_terminal_ref["digest"]:
            return _failure("dispatch receipt terminal artifact digest does not match dispatch terminal artifact")
        dispatch_terminal, error = _json_file(dispatch_terminal_path, "dispatch_terminal_artifact")
        if error:
            return _failure(error)
        assert dispatch_terminal is not None
        error = _validate_terminal(root, dispatch_terminal, outcome_id=outcome_id, attempt_id=attempt_id)
        if error:
            return _failure(f"dispatch terminal artifact: {error}")
    else:
        if not _path_matches(root, dispatch.get("output_path"), terminal_path):
            return _failure("dispatch receipt output_path does not match terminal artifact")
        if dispatch.get("output_sha256") != terminal_digest:
            return _failure("dispatch receipt output digest does not match terminal artifact")
    error = _validate_terminal(root, terminal, outcome_id=outcome_id, attempt_id=attempt_id)
    if error:
        return _failure(error)
    error = _validate_worktree(root, outcome.get("worktree_receipt"))
    if error:
        return _failure(error)

    kind = terminal["kind"]
    result: dict[str, object] = {
        "status": "accepted",
        "id": outcome_id,
        "attempt_id": attempt_id,
        "kind": kind,
        "certifying": kind == "complete" and exit_value == 0,
    }
    for field in ("summary", "question", "reason", "verdict", "artifact_refs"):
        if field in terminal:
            result[field] = terminal[field]
    if exit_value != 0:
        result["certifying"] = False
        result["reason"] = "nonzero dispatch exit"
    elif dispatch.get("status") != "ok":
        result["certifying"] = False
        result["reason"] = "dispatch did not complete successfully"
    elif kind != "complete":
        result["reason"] = f"worker outcome is {kind}"
    return result
