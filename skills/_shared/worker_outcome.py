"""Join dispatch, worker and optional Git evidence at the chair boundary."""

from __future__ import annotations

import hashlib
import errno
import json
from pathlib import Path
import re
from typing import Any

try:
    from .no_follow import NoFollowError, bound_path, lexical_path, read_file, relative_path
except ImportError:  # Direct spec loading used by the standalone worker tests.
    import sys

    sys.path.insert(0, str(Path(__file__).parent))
    from no_follow import NoFollowError, bound_path, lexical_path, read_file, relative_path


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
_KINDS = {"complete", "question", "failed", "unavailable", "blocked"}
_REVIEW_VERDICTS = {"approve", "approve-with-nits", "block", "pass"}
_WRAPPER_VERDICT_KEYS = {
    "verdict", "model", "family", "endpoint", "route_receipt", "terminal_result",
}


def _failure(reason: str) -> dict[str, object]:
    return {"status": "rejected", "reason": reason}


def _valid_wrapper_verdict(value: object) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == _WRAPPER_VERDICT_KEYS
        and all(isinstance(value.get(field), str) for field in _WRAPPER_VERDICT_KEYS)
    )


def _digest_bytes(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def _after_file_read(_path: Path, _label: str) -> None:
    """Narrow test seam for exercising the post-read identity check."""


def _reference_snapshot(
    root: Path,
    value: object,
    label: str,
    *,
    check_digest: bool = True,
    allow_absolute: bool = False,
) -> tuple[Path | None, bytes | None, tuple[int, int] | None, str | None]:
    if not isinstance(value, dict) or set(value) != _REFERENCE_KEYS:
        return None, None, None, f"{label} must contain only path and digest"
    path_value = value.get("path")
    digest = value.get("digest")
    path = Path(path_value) if isinstance(path_value, str) else Path("..")
    if (
        not isinstance(path_value, str)
        or (path.is_absolute() and not allow_absolute)
        or ".." in path.parts
        or not isinstance(digest, str)
        or not _DIGEST.fullmatch(digest)
    ):
        return None, None, None, f"{label} is invalid"
    try:
        relative = relative_path(root, path, allow_absolute=allow_absolute)
        target = bound_path(root, relative)
        data, identity, _logical = read_file(
            relative,
            label,
            root=root,
            allow_absolute=False,
            after_read=_after_file_read,
        )
    except (NoFollowError, OSError, RuntimeError, ValueError) as exc:
        message = str(exc)
        if getattr(exc, "errno", None) == errno.ELOOP:
            message = f"{label} is a symlink"
        if isinstance(exc, FileNotFoundError):
            message = f"{label} is missing"
        return None, None, None, message
    if check_digest and _digest_bytes(data) != digest:
        return None, None, None, f"{label} digest does not match"
    return target, data, identity, None


def _reference(root: Path, value: object, label: str) -> tuple[Path | None, bytes | None, str | None]:
    path, data, _identity_value, error = _reference_snapshot(root, value, label)
    return path, data, error


def _json_bytes(data: bytes, label: str) -> tuple[dict[str, Any] | None, str | None]:
    try:
        value = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None, f"{label} is not valid JSON"
    if not isinstance(value, dict):
        return None, f"{label} must be an object"
    return value, None


def _path_matches(root: Path, output_path: object, artifact: Path) -> bool:
    if not isinstance(output_path, str) or not output_path:
        return False
    try:
        return lexical_path(bound_path(root, output_path, allow_absolute=True)) == lexical_path(artifact)
    except (NoFollowError, OSError, RuntimeError, ValueError):
        return False


def _verify_answer_snapshot(
    root: Path,
    dispatch: dict[str, Any],
) -> tuple[Path | None, bytes | None, tuple[int, int] | None, str | None]:
    """Verify the human answer independently from the terminal JSON artifact."""
    output_path = dispatch.get("output_path")
    output_digest = dispatch.get("output_sha256")
    if not isinstance(output_path, str) or not output_path:
        return None, None, None, "human answer path is unavailable"
    if not isinstance(output_digest, str) or not _DIGEST.fullmatch(output_digest):
        return None, None, None, "human answer digest is unavailable"
    try:
        relative = relative_path(root, output_path, allow_absolute=True)
    except (NoFollowError, OSError, RuntimeError, ValueError):
        return None, None, None, "human answer path escapes the run directory"
    path, data, identity, error = _reference_snapshot(
        root,
        {"path": relative.as_posix(), "digest": output_digest},
        "human answer",
        check_digest=False,
        allow_absolute=False,
    )
    if error:
        return None, None, None, error
    assert path is not None and data is not None and identity is not None
    if not data:
        return None, None, None, "human answer is empty"
    if _digest_bytes(data) != output_digest:
        return None, None, None, "human answer digest does not match"
    return path, data, identity, None


def _validate_terminal(
    root: Path,
    terminal: dict[str, Any],
    *,
    outcome_id: str,
    attempt_id: str,
    require_verdict: bool = False,
) -> str | None:
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
    if kind == "complete" and require_verdict and (
        not isinstance(terminal.get("verdict"), str) or not terminal["verdict"].strip()
    ):
        return "complete terminal artifact requires verdict"
    if kind == "complete" and require_verdict and terminal.get("verdict") not in _REVIEW_VERDICTS:
        return "complete terminal artifact verdict is invalid"
    if kind == "question" and (
        not isinstance(terminal.get("question"), str) or not terminal["question"].strip()
    ):
        return "question terminal artifact requires question"
    if kind in {"failed", "unavailable", "blocked"} and (
        not isinstance(terminal.get("reason"), str) or not terminal["reason"].strip()
    ):
        return f"{kind} terminal artifact requires reason"
    required = {
        "complete": {"id", "attempt_id", "kind", "summary"},
        "question": {"id", "attempt_id", "kind", "question"},
        "failed": {"id", "attempt_id", "kind", "reason"},
        "unavailable": {"id", "attempt_id", "kind", "reason"},
        "blocked": {"id", "attempt_id", "kind", "reason"},
    }[kind]
    if set(terminal) - required - {"verdict", "artifact_refs"}:
        return f"{kind} terminal artifact contains fields for another outcome kind"
    if "artifact_refs" in terminal:
        refs = terminal["artifact_refs"]
        if not isinstance(refs, list):
            return "terminal artifact_refs must be a list"
        for index, value in enumerate(refs):
            _path, _bytes, error = _reference(root, value, f"terminal artifact_refs[{index}]")
            if error:
                return error
    if kind != "complete" and "verdict" in terminal:
        return f"{kind} terminal artifact must not contain a verdict"
    if "verdict" in terminal and terminal["verdict"] not in _REVIEW_VERDICTS:
        return "terminal artifact verdict is invalid"
    return None


def _validate_worktree(root: Path, value: object) -> str | None:
    if value is None:
        return None
    path, data, error = _reference(root, value, "worktree_receipt")
    if error:
        return error
    assert path is not None
    assert data is not None
    receipt, error = _json_bytes(data, "worktree_receipt")
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


def accept_worker_outcome(
    run_dir: Path,
    outcome: object,
    *,
    require_worktree_receipt: bool = False,
    wrapper_verdict: object = None,
) -> dict[str, object]:
    """Return a derived acceptance decision for one digest-bound attempt."""
    root = run_dir.expanduser().absolute()
    if wrapper_verdict is not None and not _valid_wrapper_verdict(wrapper_verdict):
        return _failure("worker outcome uses an invalid closed schema")
    if not isinstance(outcome, dict):
        return _failure("worker outcome uses an invalid closed schema")
    if "dispatch_terminal_artifact" not in outcome:
        return _failure("worker outcome requires dispatch_terminal_artifact")
    if set(outcome) != _OUTCOME_KEYS:
        return _failure("worker outcome uses an invalid closed schema")
    outcome_id = outcome.get("id")
    if not isinstance(outcome_id, str) or not outcome_id.strip():
        return _failure("worker outcome id is required")

    dispatch_path, dispatch_bytes, dispatch_identity, error = _reference_snapshot(
        root, outcome.get("dispatch_receipt"), "dispatch_receipt"
    )
    if error:
        return _failure(error)
    terminal_path, terminal_bytes, terminal_identity, error = _reference_snapshot(
        root, outcome.get("terminal_artifact"), "terminal_artifact"
    )
    if error:
        return _failure(error)
    assert dispatch_path is not None and dispatch_bytes is not None
    assert terminal_path is not None and terminal_bytes is not None
    dispatch, error = _json_bytes(dispatch_bytes, "dispatch_receipt")
    if error:
        return _failure(error)
    terminal, error = _json_bytes(terminal_bytes, "terminal_artifact")
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
    answer_path, answer_bytes, answer_identity, error = _verify_answer_snapshot(root, dispatch)
    if error:
        return _failure(error)
    assert answer_path is not None and answer_bytes is not None
    dispatch_terminal_ref = outcome["dispatch_terminal_artifact"]
    dispatch_terminal_path, dispatch_terminal_bytes, dispatch_terminal_identity, error = _reference_snapshot(
        root, dispatch_terminal_ref, "dispatch_terminal_artifact"
    )
    if error:
        return _failure(error)
    assert dispatch_terminal_path is not None and dispatch_terminal_bytes is not None
    if not _path_matches(root, dispatch.get("terminal_artifact_path"), dispatch_terminal_path):
        return _failure("dispatch receipt terminal_artifact_path does not match dispatch terminal artifact")
    if dispatch.get("terminal_artifact_sha256") != dispatch_terminal_ref["digest"]:
        return _failure("dispatch receipt terminal artifact digest does not match dispatch terminal artifact")
    dispatch_terminal, error = _json_bytes(dispatch_terminal_bytes, "dispatch_terminal_artifact")
    if error:
        return _failure(error)
    assert dispatch_terminal is not None
    error = _validate_terminal(root, dispatch_terminal, outcome_id=outcome_id, attempt_id=attempt_id)
    if error:
        return _failure(f"dispatch terminal artifact: {error}")
    dispatch_kind = dispatch_terminal["kind"]
    dispatch_succeeded = dispatch.get("status") == "ok" and exit_value == 0
    if dispatch_kind == "complete" and not dispatch_succeeded:
        return _failure("dispatcher terminal complete contradicts dispatch receipt status/exit")
    if dispatch_kind == "failed" and dispatch_succeeded:
        return _failure("dispatcher terminal failed contradicts dispatch receipt status/exit")
    paths = {
        "dispatch receipt": dispatch_path,
        "human answer": answer_path,
        "dispatcher terminal": dispatch_terminal_path,
        "semantic result": terminal_path,
    }
    try:
        identities = {
            identity
            for identity in (
                dispatch_identity,
                answer_identity,
                dispatch_terminal_identity,
                terminal_identity,
            )
            if identity is not None
        }
    except OSError:
        return _failure("worker evidence file identity is unavailable")
    if len(paths) != len(identities):
        return _failure("dispatch receipt, human answer, dispatcher terminal and semantic result must use distinct paths and inodes")
    error = _validate_terminal(
        root, terminal, outcome_id=outcome_id, attempt_id=attempt_id, require_verdict=True
    )
    if error:
        return _failure(error)

    snapshots = (
        ("dispatch receipt", outcome["dispatch_receipt"], dispatch_identity, False),
        (
            "human answer",
            {"path": dispatch["output_path"], "digest": dispatch["output_sha256"]},
            answer_identity,
            True,
        ),
        ("dispatcher terminal", dispatch_terminal_ref, dispatch_terminal_identity, False),
        ("semantic result", outcome["terminal_artifact"], terminal_identity, False),
    )
    for label, reference, expected_identity, allow_absolute in snapshots:
        assert expected_identity is not None
        _current_path, _current_bytes, current_identity, error = _reference_snapshot(
            root, reference, label, allow_absolute=allow_absolute
        )
        if error:
            return _failure(f"{label} changed during certification: {error}")
        if current_identity != expected_identity:
            return _failure(f"{label} inode changed during certification")
    if require_worktree_receipt and outcome.get("worktree_receipt") is None:
        return _failure("claimed implementation outcome requires worktree_receipt")
    error = _validate_worktree(root, outcome.get("worktree_receipt"))
    if error:
        return _failure(error)

    kind = terminal["kind"]
    result: dict[str, object] = {
        "status": "accepted",
        "id": outcome_id,
        "attempt_id": attempt_id,
        "kind": kind,
        "certifying": kind == "complete" and dispatch_kind == "complete" and dispatch_succeeded,
    }
    result["derived_evidence"] = {
        "human_answer": {
            "path": answer_path.absolute().relative_to(root.absolute()).as_posix(),
            "digest": dispatch["output_sha256"],
        },
        "dispatch_terminal": dispatch_terminal,
        "semantic_result": terminal,
    }
    if wrapper_verdict is not None:
        result["derived_evidence"]["wrapper_verdict"] = wrapper_verdict
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
