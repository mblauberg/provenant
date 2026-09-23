#!/usr/bin/env python3
"""Own a durable run of provider attempts, including fallback and session resume.

Routing stays with cf_dispatch.sh --plan-only; provider_exec runs the provider
in this process. Legacy receipts remain additive while Fabric v1 rows provide
live progress, typed terminal states, provenance, and controls.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager, redirect_stdout
import fcntl
import hashlib
import importlib.util
import io
import json
import math
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
import tempfile
import uuid
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

# The shared library sits one level above this skill. Resolving it from the
# skill rather than from a repository root two levels further up keeps the
# installed per-entry layout working even when the instance directory that
# holds the linked skills is not itself named "skills" (#755).
SKILLS_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SKILLS_ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import process_info
CF_DISPATCH = Path(__file__).with_name("cf_dispatch.sh")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
ATTEMPT_ID_RE = re.compile(r"^attempt-(?P<number>\d{3}|[1-9]\d{3,})$")
BATCH_ID_RE = re.compile(r"^batch-(?:\d{3}|[1-9]\d{3,})$")
DEFAULT_TIMEOUT_SECONDS = 3600.0
MAX_WORKER_QUESTION_PROMPT = 4096
MAX_WORKER_TERMINAL_ENVELOPE_BYTES = 64 * 1024
MAX_GIT_EVIDENCE_HEADER_BYTES = 64 * 1024
WORKER_TERMINAL_RECORD_TYPE = "provenant-worker-terminal"
GIT_EVIDENCE_RECORD_TYPE = "provenant-git-evidence"
CANCEL_MARKER_NAME = "cancel.request"

from _shared.bounded_process import stop_process_group
from layout import run_workspace, run_root, contains_run
import provider_exec
import exec_routing
import context_usage
from fabric_records import render_digest, write_cooldown, append_index, TERMINAL_STATUSES
from _shared.custody import (
    OwnedFileError, OwnedLinkError, atomic_write_contained, contained_regular_path,
    ensure_contained_directory, create_contained_directory, open_contained_regular, read_bound_bytes,
    read_contained_regular, unlink_contained_regular,
)
from attempt_evidence import AttemptEvidenceError as SharedAttemptEvidenceError, canonical_success_status, is_success_status, successful_adapter_error, validate_successful_attempt

class AttemptEvidenceError(ValueError):
    """A retained attempt cannot be reconciled without inventing evidence."""


class TerminalEnvelopeIntegrityError(ValueError):
    """A bounded terminal candidate changed or became unsafe to reread."""


def now() -> str:
    return datetime.now(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return f"sha256:{hasher.hexdigest()}"


def json_digest(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def write_owned(run_dir: Path, path: Path, content: str) -> None:
    atomic_write_contained(run_dir, path.relative_to(run_dir), content.encode(), label=str(path.name))


@contextmanager
def owned_text_file(run_dir: Path, path: Path, mode: str):
    flags = os.O_WRONLY | os.O_CREAT | (os.O_APPEND if "a" in mode else os.O_EXCL)
    fd, _relative, _target = open_contained_regular(
        run_dir, path.relative_to(run_dir), flags, label=str(path.name)
    )
    with os.fdopen(fd, mode, encoding="utf-8") as stream:
        yield stream


def _valid_cancel_directory(run_dir: Path, directory: Path) -> bool:
    try:
        relative = directory.relative_to(run_dir)
    except ValueError:
        return False
    current = run_dir
    for part in relative.parts:
        current /= part
        try:
            metadata = current.lstat()
        except OSError:
            return False
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            return False
    return True


def cancellation_marker_present(run_dir: Path, directory: Path) -> bool:
    """Return true only for the exact empty regular single-link marker."""
    if not _valid_cancel_directory(run_dir, directory):
        return False
    marker = directory / CANCEL_MARKER_NAME
    try:
        metadata = marker.lstat()
    except OSError:
        return False
    return stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1 and metadata.st_size == 0


def create_cancellation_marker(run_dir: Path, directory: Path) -> None:
    """Create the exact marker atomically and idempotently."""
    if not _valid_cancel_directory(run_dir, directory):
        raise ValueError("cancellation target directory is unavailable or unsafe")
    marker = directory / CANCEL_MARKER_NAME
    try:
        fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    except FileExistsError:
        if not cancellation_marker_present(run_dir, directory):
            raise ValueError("cancellation marker is invalid")
        return
    except OSError as exc:
        raise ValueError("cancellation marker cannot be created") from exc
    os.close(fd)


def remove_cancellation_marker(run_dir: Path, directory: Path) -> None:
    """Remove only the exact valid marker after owner evidence is durable."""
    if cancellation_marker_present(run_dir, directory):
        try:
            (directory / CANCEL_MARKER_NAME).unlink()
        except FileNotFoundError:
            pass


def relative_path(run_dir: Path, path: Path) -> str:
    return path.resolve().relative_to(run_dir.resolve()).as_posix()


def retained_path(run_dir: Path, value: Any) -> str:
    if not isinstance(value, str) or not value:
        raise AttemptEvidenceError("attempt evidence path is missing or invalid")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        raise AttemptEvidenceError(f"attempt evidence path escapes the run: {value}")
    try:
        (run_dir / relative).resolve().relative_to(run_dir.resolve())
    except ValueError as exc:
        raise AttemptEvidenceError(f"attempt evidence path escapes the run: {value}") from exc
    return relative.as_posix()


def ensure_owned_directory(run_dir: Path, path: Path) -> None:
    try:
        relative = path.relative_to(run_dir)
    except ValueError as exc:
        raise AttemptEvidenceError(f"attempt directory escapes the run: {path}") from exc
    try:
        ensure_contained_directory(run_dir, relative, label="attempt directory")
    except OwnedFileError as exc:
        raise AttemptEvidenceError(str(exc)) from exc


def active_receipt_error(receipt: Any) -> str | None:
    if not isinstance(receipt, dict):
        return "RUN_RECEIPT.json root must be an object"
    if receipt.get("status") != "active" or receipt.get("closed_at") is not None:
        return "dispatch requires an active orchestration run"
    if receipt.get("schema_version") != 1:
        return "RUN_RECEIPT.json schema_version must be 1"
    try:
        created_at = receipt["created_at"]
        parsed = datetime.fromisoformat(created_at.replace("Z", "+00:00"))
        if (
            not isinstance(created_at, str)
            or not created_at.endswith("Z")
            or parsed.tzinfo is None
            or parsed.utcoffset() != UTC.utcoffset(parsed)
        ):
            raise ValueError
    except (KeyError, AttributeError, TypeError, ValueError):
        return "RUN_RECEIPT.json created_at must be a UTC timestamp"
    if not isinstance(receipt.get("owner"), str) or not receipt["owner"]:
        return "RUN_RECEIPT.json owner is required"
    if not isinstance(receipt.get("retention_policy"), str) or not receipt["retention_policy"]:
        return "RUN_RECEIPT.json retention_policy is required"
    for field in (
        "owned_panes", "closed_panes", "handed_off_panes", "unclassified_paths", "pruned_paths"
    ):
        if not isinstance(receipt.get(field), list):
            return f"RUN_RECEIPT.json {field} must be a list"
    pair = receipt.get("pair")
    if not isinstance(pair, dict) or pair.get("mode") not in {"solo", "paired-primary"}:
        return "RUN_RECEIPT.json pair must declare solo or paired-primary mode"
    if not isinstance(pair.get("status"), str) or not pair["status"]:
        return "RUN_RECEIPT.json pair status is required"
    return None


def workspace_identity(workspace: Path, provider_cwd: Path | None = None) -> dict[str, Any]:
    identity: dict[str, Any] = {
        "cwd": str((provider_cwd or workspace).resolve()),
        "root": str(workspace.resolve()),
        "base_revision": None,
        "working_tree": "unavailable",
    }
    try:
        base = subprocess.run(
            ["git", "rev-parse", "--show-toplevel", "HEAD"],
            cwd=workspace,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
            check=True,
        ).stdout.splitlines()
        if len(base) != 2 or not re.fullmatch(r"[0-9a-fA-F]{40,64}", base[1]):
            return identity
        dirty = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=normal"],
            cwd=workspace,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
            check=True,
        ).stdout
        identity.update(
            base_revision=base[1].lower(),
            working_tree="dirty" if dirty else "clean",
        )
    except (OSError, subprocess.SubprocessError):
        pass
    return identity


def valid_regular_result(run_dir: Path, path: Path) -> bool:
    try:
        contained_regular_path(run_dir, path.relative_to(run_dir), "retained evidence")
    except (OSError, ValueError, OwnedFileError):
        return False
    return True


def success_receipt_error(
    adapter: dict[str, Any], args: argparse.Namespace, result_path: Path, result_digest: str
) -> str | None:
    return successful_adapter_error(adapter, args.tool, args.intent, result_path, result_digest)


class _JSONObject(dict[str, Any]):
    """JSON object retaining whether a duplicate key was supplied."""

    def __init__(self, pairs: list[tuple[str, Any]]) -> None:
        super().__init__()
        self.duplicate_keys: set[str] = set()
        self.values_by_key: dict[str, list[Any]] = {}
        for key, value in pairs:
            if key in self:
                self.duplicate_keys.add(key)
            self.values_by_key.setdefault(key, []).append(value)
            self[key] = value


def worker_question_envelope_bytes(
    candidate: bytes, expected_digest: str | None = None
) -> dict[str, Any] | None:
    """Validate one already-bound worker terminal result envelope."""
    if len(candidate) > MAX_WORKER_TERMINAL_ENVELOPE_BYTES:
        return None
    if expected_digest is not None:
        candidate_hash = hashlib.sha256(candidate).hexdigest()
        if expected_digest != f"sha256:{candidate_hash}":
            raise TerminalEnvelopeIntegrityError(
                "terminal candidate digest does not match retained result"
            )
    try:
        value = json.loads(candidate.decode("utf-8"), object_pairs_hook=_JSONObject)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    if not isinstance(value, _JSONObject) or value.get("record_type") != WORKER_TERMINAL_RECORD_TYPE:
        if (
            isinstance(value, _JSONObject)
            and "record_type" in value.duplicate_keys
            and WORKER_TERMINAL_RECORD_TYPE in value.values_by_key.get("record_type", [])
        ):
            raise ValueError("terminal worker envelope has a duplicate record_type")
        return None
    if value.duplicate_keys or set(value) != {
        "schema_version", "record_type", "classification", "question"
    }:
        raise ValueError("terminal worker envelope has an invalid root")
    if type(value.get("schema_version")) is not int or value.get("schema_version") != 1:
        raise ValueError("terminal worker envelope schema_version must be 1")
    if value.get("classification") != "question":
        raise ValueError("terminal worker envelope classification must be question")
    question = value.get("question")
    if not isinstance(question, _JSONObject) or question.duplicate_keys or set(question) != {"code", "prompt"}:
        raise ValueError("terminal worker envelope question is invalid")
    prompt = question.get("prompt")
    if (
        question.get("code") != "needs_input"
        or not isinstance(prompt, str)
        or not prompt
        or len(prompt) > MAX_WORKER_QUESTION_PROMPT
        or "\x00" in prompt
    ):
        raise ValueError("terminal worker envelope prompt is invalid")
    return {"code": "needs_input", "prompt": prompt}


def worker_question_envelope(result_path: Path, expected_digest: str) -> dict[str, Any] | None:
    """Return a validated worker question, or fail closed for a reserved record."""
    try:
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(result_path, flags)
    except OSError as exc:
        raise TerminalEnvelopeIntegrityError("terminal candidate cannot be safely reopened") from exc
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise TerminalEnvelopeIntegrityError("terminal candidate is not a regular single-link file")
        chunks: list[bytes] = []
        total = 0
        while total <= MAX_WORKER_TERMINAL_ENVELOPE_BYTES:
            chunk = os.read(
                fd, min(1024 * 1024, MAX_WORKER_TERMINAL_ENVELOPE_BYTES + 1 - total)
            )
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
        candidate = b"".join(chunks)
    except OSError as exc:
        raise TerminalEnvelopeIntegrityError("terminal candidate cannot be safely read") from exc
    finally:
        os.close(fd)
    return worker_question_envelope_bytes(candidate, expected_digest)


def fail(run_dir: Path | None, status: str, message: str, error: str | None = None) -> int:
    record = {"schema_version": 1, "status": status, "message": message, **({"error": error} if error else {})}
    print(json.dumps(record, sort_keys=True))
    return 2


def timeout_value(value: str) -> float:
    try:
        timeout = float(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("timeout must be a finite positive number") from exc
    if not math.isfinite(timeout) or timeout <= 0:
        raise argparse.ArgumentTypeError("timeout must be a finite positive number")
    return timeout


def manifest_rows(run_dir: Path, record: dict[str, Any]) -> list[tuple[str, str]]:
    paths = [
        ("attempt", record["attempt_path"]),
        ("prompt", record["prompt"]["path"]),
        ("adapter", record["route"]["adapter_receipt"]["path"]),
        ("stderr", record["stderr"]["path"]),
        ("attempt-digest", record["attempt_digest_path"]),
    ]
    if record["result"] is not None:
        paths.append(("result", record["result"]["path"]))
    if record.get("git_evidence") is not None:
        paths.append(("git-evidence", record["git_evidence"]["path"]))
    return paths


def append_manifest(run_dir: Path, record: dict[str, Any], custody=None) -> None:
    append_manifest_to(run_dir, record, custody)


def append_manifest_to(run_dir: Path, record: dict[str, Any], custody=None) -> None:
    manifest = run_dir / "MANIFEST.md"
    date = record["finished_at"][:10]
    prefix = f"dispatch-{record['task_id']}-{record['attempt_id']}"
    paths = manifest_rows(run_dir, record)
    rows_text = "".join(
        f"| {prefix}-{kind} | {path} | single dispatch {kind} | dispatch_run | "
        f"{date} | verified | evidence | |\n"
        for kind, path in paths
    )
    if custody is None:
        with manifest.open("a", encoding="utf-8") as stream:
            stream.write(rows_text)
            stream.flush()
            os.fsync(stream.fileno())
    else:
        custody.seek(0, os.SEEK_END)
        custody.write(rows_text)
        custody.flush()
        os.fsync(custody.fileno())


def ensure_manifest_appendable(run_dir: Path) -> None:
    """Check append access without changing the manifest."""
    fd, _relative, _target = open_contained_regular(
        run_dir, "MANIFEST.md", os.O_RDWR | os.O_APPEND, label="MANIFEST.md"
    )
    os.close(fd)


def acquire_run_custody(run_dir: Path):
    """Acquire the shared manifest lock without creating a new run artifact."""
    fd, _relative, _target = open_contained_regular(
        run_dir, "MANIFEST.md", os.O_RDWR | os.O_APPEND, label="MANIFEST.md"
    )
    stream = os.fdopen(fd, "a+", encoding="utf-8")
    try:
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        stream.close()
        raise
    return stream


def reconcile_manifest(run_dir: Path, custody=None) -> None:
    """Index complete prior attempts whose manifest rows were lost on re-entry."""
    manifest = run_dir / "MANIFEST.md"
    if custody is None:
        existing = manifest.read_text(encoding="utf-8", errors="replace")
    else:
        custody.seek(0)
        existing = custody.read()
    attempt_dirs = sorted((run_dir / "dispatch" / "tasks").glob("*/attempt-*"))
    # A recovered interrupted attempt has no legacy terminal envelope. Its
    # canonical terminal row is the recovery evidence; retain partial files.
    recoverable = set()
    for directory in attempt_dirs:
        if (directory / "attempt.json").exists():
            continue
        canonical = Path("tasks") / directory.parent.name / directory.name / "attempt.json"
        try:
            row = json.loads(read_bound_bytes(run_dir, canonical, label="interrupted attempt"))
            if (row.get("schema") == "fabric.attempt.v1" and row.get("state") == "terminal"
                    and row.get("status") == "interrupted" and row.get("task_id") == directory.parent.name
                    and directory.name == f"attempt-{row.get('attempt', 0):03d}"):
                recoverable.add(directory)
        except (OSError, ValueError, OwnedFileError):
            pass
    attempt_dirs = [directory for directory in attempt_dirs if directory not in recoverable]
    for attempt_dir in attempt_dirs:
        try:
            relative_path(run_dir, attempt_dir)
        except ValueError as exc:
            raise AttemptEvidenceError(f"attempt directory escapes the run: {attempt_dir}") from exc
        if (
            attempt_dir.is_symlink()
            or not attempt_dir.is_dir()
            or not ATTEMPT_ID_RE.fullmatch(attempt_dir.name)
            or not (attempt_dir / "attempt.json").is_file()
            or (attempt_dir / "attempt.json").is_symlink()
        ):
            raise AttemptEvidenceError(f"attempt directory is incomplete: {attempt_dir}")
    for attempt_path in (attempt_dir / "attempt.json" for attempt_dir in attempt_dirs):
        try:
            discovered_attempt_path = relative_path(run_dir, attempt_path)
        except ValueError as exc:
            raise AttemptEvidenceError(f"attempt record path escapes the run: {attempt_path}") from exc
        try:
            record = json.loads(attempt_path.read_text(encoding="utf-8"))
        except (OSError, TypeError, ValueError) as exc:
            raise AttemptEvidenceError(f"attempt record is unreadable: {attempt_path}") from exc
        task_id = attempt_path.parent.parent.name
        attempt_id = attempt_path.parent.name
        if (
            not isinstance(record, dict)
            or record.get("schema_version") != 1
            or record.get("record_type") != "dispatch-attempt"
            or record.get("task_id") != task_id
            or record.get("attempt_id") != attempt_id
            or not TASK_ID_RE.fullmatch(task_id)
        ):
            raise AttemptEvidenceError(f"attempt record has invalid schema or identity: {attempt_path}")
        try:
            if retained_path(run_dir, record["attempt_path"]) != discovered_attempt_path:
                raise AttemptEvidenceError(
                    f"attempt record path does not match its retained file: {attempt_path}"
                )
            sidecar = attempt_path.with_name("attempt.sha256")
            if not sidecar.is_file():
                write_owned(run_dir, sidecar, f"{digest(attempt_path)}  {attempt_path.name}\n")
            if not valid_regular_result(run_dir, sidecar):
                raise AttemptEvidenceError(f"attempt digest is not a regular retained file: {sidecar}")
            expected_sidecar = f"{digest(attempt_path)}  {attempt_path.name}\n"
            if sidecar.read_text(encoding="utf-8") != expected_sidecar:
                raise AttemptEvidenceError(f"attempt digest does not match retained record: {attempt_path}")
            record["attempt_digest_path"] = relative_path(run_dir, sidecar)
            rows = [
                (kind, retained_path(run_dir, path))
                for kind, path in manifest_rows(run_dir, record)
            ]
            attempt_root = Path("dispatch") / "tasks" / task_id / attempt_id
            expected = {
                "attempt": (attempt_root / "attempt.json").as_posix(),
                "prompt": (attempt_root / "prompt.md").as_posix(),
                "adapter": (attempt_root / "adapter-receipt.json").as_posix(),
                "stderr": (attempt_root / "stderr.log").as_posix(),
                "attempt-digest": (attempt_root / "attempt.sha256").as_posix(),
                "result": (attempt_root / "result.md").as_posix(),
            }
            if record.get("git_evidence") is not None:
                expected["git-evidence"] = (attempt_root / "evidence" / "git-evidence.md").as_posix()
            mismatched = [kind for kind, path in rows if path != expected[kind]]
            if mismatched:
                raise AttemptEvidenceError(
                    f"attempt evidence path is not canonical for {attempt_path}: {', '.join(mismatched)}"
                )
            absent = [path for _, path in rows if not valid_regular_result(run_dir, run_dir / path)]
            if absent:
                raise AttemptEvidenceError(
                    f"attempt evidence is missing for {attempt_path}: {', '.join(absent)}"
                )
            claimed_digests = {
                "prompt": record["prompt"]["digest"],
                "adapter": record["route"]["adapter_receipt"]["digest"],
                "stderr": record["stderr"]["digest"],
            }
            if record["result"] is not None:
                claimed_digests["result"] = record["result"]["digest"]
            if record.get("git_evidence") is not None:
                evidence = record["git_evidence"]
                if not isinstance(evidence, dict) or not isinstance(evidence.get("digest"), str):
                    raise AttemptEvidenceError(f"Git evidence receipt is malformed: {attempt_path}")
                claimed_digests["git-evidence"] = evidence["digest"]
            mismatched_digests = [
                kind
                for kind, path in rows
                if kind in claimed_digests and claimed_digests[kind] != digest(run_dir / path)
            ]
            if mismatched_digests:
                raise AttemptEvidenceError(
                    f"attempt evidence digest does not match {attempt_path}: "
                    + ", ".join(mismatched_digests)
                )
            if is_success_status(record.get("status")):
                try:
                    _adapter_rel, _adapter_path, adapter_bytes = read_contained_regular(
                        run_dir, expected["adapter"], label="adapter receipt"
                    )
                    _result_rel, _result_path, result_bytes = read_contained_regular(
                        run_dir, expected["result"], label="result"
                    )
                    validate_successful_attempt(run_dir, record, {
                        "adapter_receipt": adapter_bytes, "result": result_bytes,
                    })
                except (OSError, ValueError, SharedAttemptEvidenceError) as exc:
                    raise AttemptEvidenceError(str(exc)) from exc
            missing = [(kind, path) for kind, path in rows if f"| {path} |" not in existing]
            if not missing:
                continue
            date = record["finished_at"][:10]
            prefix = f"dispatch-{record['task_id']}-{record['attempt_id']}"
            rows_text = "".join(
                f"| {prefix}-{kind} | {path} | single dispatch {kind} | dispatch_run | "
                f"{date} | verified | evidence | |\n"
                for kind, path in missing
            )
            if custody is None:
                with manifest.open("a", encoding="utf-8") as stream:
                    stream.write(rows_text)
                    stream.flush()
                    os.fsync(stream.fileno())
                existing = manifest.read_text(encoding="utf-8", errors="replace")
            else:
                custody.seek(0, os.SEEK_END)
                custody.write(rows_text)
                custody.flush()
                os.fsync(custody.fileno())
                existing += rows_text
        except AttemptEvidenceError:
            raise
        except (KeyError, TypeError, ValueError) as exc:
            raise AttemptEvidenceError(f"attempt record is malformed: {attempt_path}") from exc


def existing_attempt_number(task_dir: Path) -> int:
    numbers = []
    for candidate in task_dir.glob("attempt-*"):
        match = ATTEMPT_ID_RE.fullmatch(candidate.name)
        if match and candidate.is_dir():
            numbers.append(int(match.group("number")))
    return max(numbers, default=0) + 1


ACCESS_MODES = ("read_only", "worktree_write")
WORKTREE_WRITER_LOCK = "provenant-dispatch-writer.lock"
WORKTREE_WRITE_ADAPTERS = ("claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot")


class WorktreeLeaseError(ValueError):
    """The requested writer worktree cannot be owned exclusively."""


def _git_path(worktree: Path, which: str) -> Path | None:
    """Resolve one absolute Git path for a worktree, or None when it is not one."""
    try:
        completed = subprocess.run(
            ["git", "-C", str(worktree), "rev-parse", "--path-format=absolute", which],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    value = completed.stdout.strip()
    return Path(value) if value else None


def resolve_writer_worktree(worktree: Path) -> Path:
    """Return the resolved root of a Git worktree the caller may write inside."""
    try:
        resolved = worktree.expanduser().resolve(strict=True)
    except OSError as exc:
        raise WorktreeLeaseError(f"worktree is not a readable directory: {worktree}") from exc
    if not resolved.is_dir():
        raise WorktreeLeaseError(f"worktree is not a directory: {resolved}")
    top = _git_path(resolved, "--show-toplevel")
    if top is None:
        raise WorktreeLeaseError(f"worktree is not inside a Git repository: {resolved}")
    try:
        if top.resolve() != resolved:
            raise WorktreeLeaseError(f"worktree must be the root of a Git worktree: {resolved}")
    except OSError as exc:
        raise WorktreeLeaseError(f"worktree root could not be resolved: {resolved}") from exc
    try:
        listing = subprocess.run(["git", "-C", str(resolved), "worktree", "list", "--porcelain", "-z"],
                                 env={key: value for key, value in os.environ.items() if not key.startswith("GIT_")}, capture_output=True, timeout=3, check=True)
        roots = [Path(os.fsdecode(field[9:])).resolve() for field in listing.stdout.split(b"\0") if field.startswith(b"worktree ")]
        if resolved not in roots:
            raise WorktreeLeaseError("worktree must be registered with Git")
    except (OSError, subprocess.SubprocessError) as exc:
        raise WorktreeLeaseError("cannot verify registered Git worktree") from exc
    return resolved


def acquire_worktree_lease(worktree: Path):
    """Hold the one-writer lease for a worktree, or refuse a concurrent writer."""
    git_dir = _git_path(worktree, "--git-dir")
    if git_dir is None or not git_dir.is_dir():
        raise WorktreeLeaseError(f"worktree has no usable Git directory: {worktree}")
    lock_path = git_dir / WORKTREE_WRITER_LOCK
    try:
        handle = lock_path.open("a+")
    except OSError as exc:
        raise WorktreeLeaseError(f"cannot open the worktree writer lease: {exc}") from exc
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as exc:
        handle.close()
        raise WorktreeLeaseError(f"another writer already owns this worktree: {worktree}") from exc
    return handle


def release_worktree_lease(handle) -> None:
    if handle is None:
        return
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    finally:
        handle.close()


# Provider deadlines sit slightly under the owner's own deadline so the provider
# reaches its limit first and exits through its normal path, flushing whatever
# result it has. Without the gap the owner kills the process group mid-write and
# the truncated result is diagnosed as a corrupt receipt rather than a timeout.
PROVIDER_TIMEOUT_MARGIN_SECONDS = 5.0


def provider_timeout_seconds(timeout_seconds: float) -> int:
    """Return the whole-second deadline handed to the provider CLI.

    Only the adapters whose CLI accepts a headless timeout consume it: on this
    machine that is agy (``--print-timeout``). Neither ``claude`` nor
    ``codex exec`` exposes a timeout flag, so on those arms the owner's deadline
    below remains the only bound.
    """
    margin = max(1.0, min(PROVIDER_TIMEOUT_MARGIN_SECONDS, timeout_seconds * 0.1))
    return max(1, int(timeout_seconds - margin))


def build_command(
    args: argparse.Namespace, prompt_path: Path, result_path: Path,
    evidence_dir: Path | None = None,
) -> list[str]:
    command = [
        str(CF_DISPATCH),
        "--intent",
        args.intent,
        "--tool",
        args.tool,
        "--prompt-file",
        str(prompt_path),
        "--out",
        str(result_path),
        "--role",
        args.role,
    ]
    for flag, value in (
        ("--orchestrator-family", args.orchestrator_family),
        ("--alias", args.alias),
        ("--task-class", args.task_class),
        ("--risk-tier", args.risk_tier),
        ("--model-override-tier", args.model_override_tier),
        ("--reviewer-id", args.reviewer_id),
        ("--model", args.model),
        ("--effort", args.effort),
        ("--access-mode", args.access_mode),
        ("--worktree", str(args.worktree) if args.worktree else ""),
        ("--timeout-seconds", str(provider_timeout_seconds(args.timeout_seconds))),
    ):
        if value:
            command.extend((flag, value))
    if evidence_dir is not None:
        command.extend(("--add-dir", str(evidence_dir)))
    for flag, value in (("--cwd",getattr(args,"provider_cwd",None)),("--sandbox", getattr(args,"sandbox",None)),("--network",getattr(args,"network",None)),("--resume-session",getattr(args,"resume_session",None))):
        if value is not None: command.extend((flag,str(value)))
    for directory in getattr(args,"add_dirs",[]): command.extend(("--add-dir",str(directory)))
    if not getattr(args,"preface",True): command.append("--no-preface")
    policy = exec_routing.validate_policy(getattr(args, "fallback", None))
    if policy is not None:
        command.extend(("--fallback", "true" if isinstance(policy, list) else json.dumps(policy) if type(policy) is bool else policy))
    return command


FAST_PLAN_SHELL_ENV_READS = {
    "CF_DISPATCH_IDLE_SECONDS", "CF_DISPATCH_AGY_ADD_DIR", "CF_DISPATCH_ENABLE_KIRO",
    "CF_DISPATCH_ENABLE_COPILOT", "CF_DISPATCH_CURSOR_MODEL", "CF_DISPATCH_KIRO_MODEL",
    "CF_DISPATCH_COPILOT_MODEL", "CF_DISPATCH_OPENCODE_MODEL", "CF_DISPATCH_ENDPOINT",
    "CF_DISPATCH_CODEX_NETWORK", "CF_DISPATCH_AGY_SANDBOX",
}
FAST_PLAN_ROUTE_REQUIRED = (
    "resolved_model", "model_family", "endpoint_provider", "identity_source",
)
FAST_PLAN_ROUTE_FIELDS = (
    "status", "resolved_model", "model_family", "endpoint_provider",
    "identity_source", "requested_effort", "effort", "effort_source",
    "effort_capability_source", "effort_substitution", "substitution",
    "fallback_model", "catalog_model", "model_selection",
    "model_override_tier", "policy_override", "alias", "reason",
    "endpoint_profile", "endpoint_base_url", "endpoint_token_env", "endpoint_wire_api",
)


def parse_fast_route_json(raw: str) -> dict:
    """Apply the shell planner's parse_route_json field contract before fast planning."""
    def reject_duplicate_members(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate route member: {key}")
            value[key] = item
        return value

    route = json.loads(raw, object_pairs_hook=reject_duplicate_members)
    if not isinstance(route, dict):
        raise ValueError("route must be a JSON object")
    if route.get("status") == "ok":
        for key in FAST_PLAN_ROUTE_REQUIRED:
            value = route.get(key)
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"successful route requires non-empty {key}")
    for key in FAST_PLAN_ROUTE_FIELDS:
        value = route.get(key, "")
        if value is not None and (not isinstance(value, str) or "\0" in value):
            raise ValueError(f"route field {key} must be a NUL-free string")
    return route


PLANNER_MODEL_STATUSES = {"capability_model_unavailable", "no_candidate_available", "alias_unavailable",
                          "adapter_default_model_invalid", "model_required_for_broker"}
PLANNER_TRANSIENT_STATUSES = {"capability_discovery_failed", "capability_snapshot_untrusted",
                              "capability_snapshot_stale", "probe_unavailable", "probe_cache_busy"}


def planner_result(planning):
    """The route plan, or a typed failure when the planner produced none."""
    try:
        return json.loads(planning.stdout)
    except ValueError:
        if planning.returncode in (-signal.SIGTERM, -signal.SIGINT, -signal.SIGHUP):
            return {"status": "interrupted", "fix": f"route planning stopped by signal {-planning.returncode}; retry"}
        if planning.returncode < 0:
            return {"status": "failed", "fix": f"route planner crashed (signal {-planning.returncode}); inspect stderr"}
        return {"status": "rejected", "fix": "route planner returned invalid JSON"}


def route_refusal(record, adapter=""):
    """A planner refusal in the router's vocabulary; provider-exec records carry provenance."""
    status = record.get("status")
    if not status or status in TERMINAL_STATUSES or "provenance" in record or record.get("schema") == "fabric.exec-plan.v1":
        return None
    return router_failure(str(status), adapter)


def router_failure(signature, adapter=""):
    """Fabric's status and fix for a router refusal; the router's own status stays the signature."""
    if signature in PLANNER_MODEL_STATUSES:
        status = "model_unavailable"
        fix = provider_exec._model_unavailable_fix({"adapter": adapter, "route": {"identity_source": "passed-through"}})
    elif signature in PLANNER_TRANSIENT_STATUSES:
        status, fix = "failed", f"provider capability check failed ({signature}); check the provider CLI login and retry"
    else:
        status, fix = "rejected", f"route planner refused the route ({signature}); change the requested route"
    return {"status": status, "error": signature, "fix": fix,
            "evidence": {"exit": None, "signal": None, "signature": signature, "excerpt": ""}}


def fast_fabric_plan(args, prompt_path: Path, result_path: Path, workspace: Path):
    """Plan a simple explicit route in the owner process using the same router and supervisor."""
    if (not args.model or args.tool not in {"claude", "codex"}
            or any(str(value).startswith("-") for value in (args.model, args.effort, args.role) if value)
            or any(value for key, value in os.environ.items() if key.startswith("CF_DISPATCH_"))
            or not Path(os.environ.get("TMPDIR") or "/tmp").is_dir()
            or args.access_mode != "read_only"
            or args.task_class or args.alias or args.resume or getattr(args, "resume_session", None) or args.worktree
            or args.provider_cwd or args.sandbox or args.network is not None or args.add_dirs
            or args.git_evidence or args.model_override_tier or args.orchestrator_family
            or args.intent != "ordinary" or args.fallback not in (None, False, "false")):
        return None
    executable = {"cursor": "cursor-agent", "kiro": "kiro-cli"}.get(args.tool, args.tool)
    if not shutil.which(executable):
        return None
    environment = routing_environment()
    product = Path(environment.get("AGENT_FABRIC_PRODUCT_ROOT") or SKILLS_ROOT.parent)
    path = product / "scripts/model_route.py"
    if not path.is_file():
        return None
    try:
        prompt = prompt_path.read_text(encoding="utf-8")
        if not prompt or "\0" in prompt:
            return None
        context_spec = importlib.util.spec_from_file_location("fabric_fast_worktree", product / "scripts/worktree.py")
        context_module = importlib.util.module_from_spec(context_spec)
        context_spec.loader.exec_module(context_module)
        try:
            context_module.validate_context(argparse.Namespace(repo=workspace, allow_non_git=True))
        except context_module.PolicyError:
            return None
        spec = importlib.util.spec_from_file_location("fabric_fast_model_route", path)
        module = importlib.util.module_from_spec(spec)
        overrides = {"FABRIC_ALIAS_IMPLIED": "1", "AGENT_FABRIC_PRODUCT_ROOT": str(product),
                     "AGENT_FABRIC_INSTANCE_ROOT": environment.get("AGENT_FABRIC_INSTANCE_ROOT")
                     or str(Path.home() / ".agents")}
        previous = {key: os.environ.get(key) for key in overrides}
        os.environ.update(overrides)
        try:
            with redirect_stdout(io.StringIO()) as output:
                spec.loader.exec_module(module)
                argv = ["resolve", "--adapter", args.tool, "--role", args.role,
                        "--lead-family", "", "--alias", "flagship", f"--model={args.model}"]
                if args.effort:
                    argv += ["--effort", args.effort]
                if args.fallback is not None:
                    argv += ["--fallback", "false"]
                code = module.main(argv)
        finally:
            for key, value in previous.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
        if code != 0:
            return None
        route = parse_fast_route_json(output.getvalue())
        if route.get("status") != "ok":
            return None
        plan = provider_exec.build_plan(
            args.tool, route, prompt,
            cwd=workspace, workspace_root=workspace, mode=args.access_mode,
            timeout_seconds=provider_timeout_seconds(args.timeout_seconds),
            intent=args.intent, preface=args.preface, requested_model=args.model,
            requested_effort=args.effort or "", run_id=os.environ.get("PROVENANT_RUN_ID", ""),
            chair=os.environ.get("PROVENANT_CHAIR", ""),
            reviewer_id=args.reviewer_id or "", risk_tier=args.risk_tier or "",
            model_override_tier=args.model_override_tier or "", orchestrator_family="",
        )
        plan["output_path"] = str(result_path.absolute())
        return plan
    except (Exception, SystemExit):
        return None


def _read_prompt_once(workspace: Path, prompt_source: Path) -> bytes:
    """Read the validated prompt inode once for both retention and provider use."""
    try:
        relative = prompt_source.relative_to(workspace)
    except ValueError as exc:
        raise OwnedFileError("prompt file must be inside the current workspace") from exc
    fd, _relative, _target = open_contained_regular(
        workspace, relative, os.O_RDONLY, label="prompt file"
    )
    try:
        chunks: list[bytes] = []
        while True:
            chunk = os.read(fd, 1024 * 1024)
            if not chunk:
                return b"".join(chunks)
            chunks.append(chunk)
    finally:
        os.close(fd)


def _validate_git_evidence_metadata(metadata: Any) -> dict[str, Any]:
    """Validate the bounded JSON header of one Git evidence packet."""
    try:
        valid = (
            isinstance(metadata, dict)
            and set(metadata) == {
                "schema_version", "record_type", "repository", "git_root", "head",
                "diff_base", "working_tree", "diff_from", "paths", "encoding",
            }
            and metadata.get("schema_version") == 1
            and metadata.get("record_type") == GIT_EVIDENCE_RECORD_TYPE
            and isinstance(metadata.get("repository"), str)
            and isinstance(metadata.get("git_root"), str)
            and isinstance(metadata.get("head"), str)
            and re.fullmatch(r"[0-9a-f]{40,64}", metadata["head"]) is not None
            and isinstance(metadata.get("diff_base"), str)
            and re.fullmatch(r"[0-9a-f]{40,64}", metadata["diff_base"]) is not None
            and metadata.get("working_tree") in {"clean", "dirty"}
            and isinstance(metadata.get("diff_from"), str)
            and bool(metadata["diff_from"])
            and isinstance(metadata.get("paths"), list)
            and all(isinstance(path, str) for path in metadata["paths"])
            and metadata.get("encoding") == "utf-8-replacement"
        )
    except (KeyError, TypeError):
        valid = False
    if not valid:
        raise AttemptEvidenceError("Git evidence packet header is invalid")
    return metadata


def _copy_git_evidence(
    workspace: Path, run_dir: Path, source: Path, destination: Path
) -> tuple[dict[str, Any], str]:
    """Validate and copy a packet in bounded memory while hashing its bytes."""
    try:
        source.relative_to(run_dir)
        source_relative = source.relative_to(run_dir)
        destination_relative = destination.relative_to(run_dir)
    except ValueError as exc:
        raise AttemptEvidenceError("Git evidence must be materialised inside the run directory") from exc
    source_fd = destination_fd = -1
    try:
        source_fd, _source_rel, _source_target = open_contained_regular(
            run_dir, source_relative, os.O_RDONLY, label="Git evidence source"
        )
        destination_fd, _destination_rel, _destination_target = open_contained_regular(
            run_dir,
            destination_relative,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL,
            label="Git evidence packet",
        )
    except (OwnedLinkError, OwnedFileError) as exc:
        if source_fd >= 0:
            os.close(source_fd)
        raise AttemptEvidenceError(str(exc)) from exc
    hasher = hashlib.sha256()
    header = bytearray()
    metadata: dict[str, Any] | None = None
    status_prefix = b"--- status ---\n"
    body_prefix = bytearray()
    marker = b"\n--- diff ---\n"
    tail = b""

    def write_all(fd: int, value: bytes) -> None:
        offset = 0
        while offset < len(value):
            offset += os.write(fd, value[offset:])

    try:
        while chunk := os.read(source_fd, 1024 * 1024):
            hasher.update(chunk)
            write_all(destination_fd, chunk)
            remaining = chunk
            if metadata is None:
                header.extend(remaining)
                if len(header) > MAX_GIT_EVIDENCE_HEADER_BYTES and b"\n" not in header:
                    raise AttemptEvidenceError("Git evidence packet header exceeds its 64 KiB limit")
                separator = header.find(b"\n")
                if separator < 0:
                    continue
                if separator > MAX_GIT_EVIDENCE_HEADER_BYTES:
                    raise AttemptEvidenceError("Git evidence packet header exceeds its 64 KiB limit")
                first_line = bytes(header[:separator])
                metadata = _validate_git_evidence_metadata(
                    json.loads(first_line.decode("utf-8"))
                )
                remaining = bytes(header[separator + 1:])
            body_prefix.extend(remaining[: len(status_prefix) - len(body_prefix)])
            if len(body_prefix) >= len(status_prefix) and bytes(body_prefix) != status_prefix:
                raise AttemptEvidenceError("Git evidence packet is missing its status section")
            combined = tail + remaining
            if marker in combined:
                tail = marker
            else:
                tail = combined[-(len(marker) - 1):]
        if metadata is None:
            raise AttemptEvidenceError("Git evidence packet has no JSON header")
        if len(body_prefix) < len(status_prefix):
            raise AttemptEvidenceError("Git evidence packet is missing its status section")
        if marker not in tail:
            raise AttemptEvidenceError("Git evidence packet is missing its diff section")
        os.fsync(destination_fd)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        try:
            unlink_contained_regular(run_dir, destination_relative, label="Git evidence packet")
        except (OSError, OwnedFileError):
            pass
        raise AttemptEvidenceError("Git evidence packet has an invalid JSON header") from exc
    except BaseException:
        try:
            unlink_contained_regular(run_dir, destination_relative, label="Git evidence packet")
        except (OSError, OwnedFileError):
            pass
        raise
    finally:
        if source_fd >= 0:
            os.close(source_fd)
        if destination_fd >= 0:
            os.close(destination_fd)
    return metadata, f"sha256:{hasher.hexdigest()}"


def _record_provider_process(run_dir: Path, process: subprocess.Popen[Any]) -> None:
    """Record the provider's identity so a detached run stays reachable.

    The MCP front door spawns this owner detached and then lets go of it. If
    that host dies, the only handle left on the provider is what is written
    here: its pid, the group it leads, and a start timestamp that tells it
    apart from a recycled pid. The run token proves the record belongs to this
    run rather than to an earlier one in a reused directory. A failure to write
    it costs later reaping, never the dispatch itself.
    """
    token = os.environ.get("PROVENANT_RUN_TOKEN") or run_identity(run_dir)
    try:
        started_at = process_info.start_time(process.pid)
        record = {
            "schema_version": 1,
            "run_token": token,
            "provider_pid": process.pid,
            "provider_pgid": os.getpgid(process.pid),
            "provider_started_at": started_at or None,
        }
        (run_dir / "dispatch-provider.json").write_text(
            json.dumps(record, indent=2) + "\n", encoding="utf-8",
        )
    except (OSError, ValueError, subprocess.SubprocessError):
        return


class PreflightError(ValueError):
    def __init__(self, code: str, fix: str):
        super().__init__(fix)
        self.code = code


def read_prompt_input(prompt_file: Path, workspace: Path, run_dir: Path) -> bytes:
    prompt_source = None
    if prompt_file is not None:
        prompt_source = prompt_file.expanduser()
        if not prompt_source.is_absolute():
            prompt_source = workspace / prompt_source
    prompt_bytes: bytes | None = None
    if prompt_source is not None:
        if not prompt_source.exists():
            raise PreflightError("prompt_unavailable", f"cannot read prompt file: {prompt_source}")
        prompt_root = next((root for root in (run_dir, workspace, run_workspace(run_dir, workspace))
                            if prompt_source.is_relative_to(root)), None)
        if prompt_root is None:
            raise PreflightError("prompt_path_forbidden", "prompt file must be inside the run directory or current workspace")
        sensitive_roots = {".ssh", ".aws", ".azure", ".gnupg"}
        sensitive_files = {
            ".env", ".env.local", ".env.production", "credentials.json",
            "application_default_credentials.json", "token.json",
        }
        parts = [part.casefold() for part in prompt_source.parts]
        config_auth_dirs = {"gcloud", "gh", "claude", "codex", "openai"}
        config_auth = any(
            part == ".config" and index + 1 < len(parts) and parts[index + 1] in config_auth_dirs
            for index, part in enumerate(parts)
        )
        if sensitive_roots.intersection(parts) or prompt_source.name.casefold() in sensitive_files or config_auth:
            raise PreflightError("credential_or_auth_store_denied", "prompt path is a credential or authentication store")
        try:
            prompt_bytes = _read_prompt_once(prompt_root, prompt_source)
        except OwnedLinkError as exc:
            raise PreflightError("prompt_hard_link_denied", str(exc))
        except OwnedFileError as exc:
            raise PreflightError("prompt_unavailable", str(exc))
    return prompt_bytes


def routing_environment() -> dict[str, str]:
    """Keep discovery and worker routing on the same instance/product fallback."""
    env = os.environ.copy()
    instance = Path(env.get("AGENT_FABRIC_INSTANCE_ROOT") or Path.home() / ".agents").expanduser()
    try:
        (instance / "config/model-routing.json").lstat()
    except FileNotFoundError:
        env["AGENT_FABRIC_INSTANCE_ROOT"] = env.get("AGENT_FABRIC_PRODUCT_ROOT", str(SKILLS_ROOT.parent))
    except OSError:
        pass
    return env


def preflight_tasks(tasks: list[dict[str, Any]]) -> dict[str, Any]:
    """Use the dispatcher router before creating custody or launching any task."""
    workspace = Path.cwd().resolve()
    product = Path(os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", SKILLS_ROOT.parent))
    instance = Path(routing_environment().get("AGENT_FABRIC_INSTANCE_ROOT") or Path.home() / ".agents").expanduser()
    catalog = instance / "config/model-routing.json"
    errors = []
    routes = []
    seen = set()
    writers = set()
    probes: dict[str, Path | None] = {}
    resolved_routes: dict[tuple[str, ...], dict[str, Any]] = {}
    with tempfile.TemporaryDirectory(prefix="fabric-preflight-") as scratch:
        for task in tasks:
            task_id = task.get("id", "task-1")
            try:
                if not isinstance(task_id, str) or not TASK_ID_RE.fullmatch(task_id) or task_id in seen:
                    raise PreflightError("invalid_task_id", "Pass unique task ids containing letters, numbers, '.', '_' or '-'.")
                seen.add(task_id)
                try:
                    exec_routing.validate_policy(task.get("fallback"))
                except ValueError as exc:
                    raise PreflightError("fallback_invalid", str(exc)) from exc
                if task.get("sandbox") not in {None,"read-only","workspace-write","full"}:
                    raise PreflightError("sandbox_invalid","Pass read-only, workspace-write or full.")
                if task.get("access_mode","read_only")=="read_only" and task.get("sandbox") not in {None,"read-only"}:
                    raise PreflightError("sandbox_forbidden","A read-only run requires a read-only sandbox.")
                if "network" in task and type(task["network"]) is not bool:
                    raise PreflightError("network_invalid","Pass network true or false.")
                if any(provider_exec.credential_path(path) for path in task.get("add_dirs",[])):
                    raise PreflightError("credential_or_auth_store_denied","Additional directories must exclude credential stores.")
                if (task.get("prompt") is None) == (task.get("prompt_file") is None):
                    raise PreflightError("prompt_required", "Pass exactly one of prompt or prompt_file.")
                if task.get("prompt_file") is not None:
                    read_prompt_input(Path(task["prompt_file"]), workspace, workspace)
                adapter = task["adapter"]
                mode = task.get("access_mode", "read_only")
                if mode not in ACCESS_MODES:
                    raise PreflightError("access_mode_invalid", "Pass mode read_only or worktree_write.")
                if mode == "worktree_write":
                    if adapter not in WORKTREE_WRITE_ADAPTERS:
                        raise PreflightError("worktree_write_adapter_unsupported", "Pass mode read_only or adapter " + ", ".join(sorted(WORKTREE_WRITE_ADAPTERS)) + ".")
                    if not task.get("worktree"):
                        raise PreflightError("worktree_required", "Pass worktree=<registered Git worktree root> with mode worktree_write.")
                    worktree = resolve_writer_worktree(Path(task["worktree"]))
                    if worktree in writers:
                        raise PreflightError("worktree_conflict", "Pass a different registered worktree for each writer task.")
                    writers.add(worktree)
                elif task.get("worktree"):
                    raise PreflightError("worktree_not_applicable", "Pass mode worktree_write with worktree, or omit worktree.")
                command = [sys.executable, str(product / "scripts/model_route.py"), "resolve",
                           "--catalog", str(catalog), "--adapter", adapter, "--role", "worker"]
                if task.get("alias"):
                    command.extend(("--alias", task["alias"]))
                elif not task.get("model"):
                    command.extend(("--alias", "workhorse"))
                policy = exec_routing.validate_policy(task.get("fallback"))
                if policy is not None:
                    command.extend(("--fallback", "true" if isinstance(policy, list) else json.dumps(policy) if type(policy) is bool else policy))
                for key in ("model", "effort"):
                    if task.get(key):
                        command.extend(["--" + key, task[key]])
                if os.environ.get("CF_DISPATCH_ENDPOINT"):
                    command.extend(["--endpoint", os.environ["CF_DISPATCH_ENDPOINT"]])
                def resolve_route():
                    key = tuple(command)
                    if key in resolved_routes:
                        return resolved_routes[key]
                    result = subprocess.run(command, text=True, capture_output=True, timeout=15, env=routing_environment())
                    try:
                        route = json.loads(result.stdout)
                        resolved_routes[key] = route
                        return route
                    except ValueError:
                        raise PreflightError("model_routing_unavailable", "Restore the routing catalogue and the harness Python environment.")
                route = resolve_route()
                if (adapter == "codex" and route.get("status") == "capability_discovery_failed") or (
                    adapter == "agy" and route.get("status") in {"ok", "model_required_for_broker"}
                ):
                    if adapter not in probes:
                        path = Path(scratch) / (adapter + ".json")
                        probe = subprocess.run([sys.executable, str(CF_DISPATCH.with_name("capabilities.py")),
                                                adapter, "--out", str(path)], capture_output=True, timeout=20)
                        probes[adapter] = path if probe.returncode == 0 else None
                    if probes[adapter] is not None:
                        command.extend(["--capabilities-file", str(probes[adapter])])
                        route = resolve_route()
                if route.get("status") != "ok":
                    code = route.get("status", "routing_record_invalid")
                    try:
                        routing_catalog = exec_routing.snapshot() or json.loads(catalog.read_text())
                    except (OSError, ValueError):
                        routing_catalog = {}
                    fixes = {
                        "model_required_for_broker": "Pass model=<provider/model id> for " + adapter + ".",
                        "effort_unsupported": "Pass an effort supported by the selected model, or omit effort.",
                        "capability_discovery_failed": "Pass an installed, authenticated adapter with a readable model catalogue.",
                    }
                    adapter_config = routing_catalog.get("adapters", {}).get(adapter, {})
                    family = adapter_config.get("fixed_model_family")
                    families = [family] if family else adapter_config.get("model_family_preferences", {}).get("preferred", [])
                    aliases: dict[str, list[str]] = {}
                    for name in families:
                        for alias, models in routing_catalog.get("families", {}).get(name, {}).get("aliases", {}).items():
                            aliases.setdefault(alias, []).extend(models)
                    choices = list(dict.fromkeys(model for models in aliases.values() for model in models))
                    fix = ("Pass alias " + ", ".join(aliases) + " or model " + ", ".join(choices) + "."
                           if choices else "Pass an explicit provider/model id supported by " + adapter + ".")
                    if "effort" in code:
                        fix = "Omit effort, or pass a supported level: low, medium, high, xhigh, max, ultra."
                    raise PreflightError(code, fixes.get(code, fix))
                routes.append(route)
            except (PreflightError, WorktreeLeaseError, OSError, subprocess.TimeoutExpired) as exc:
                prompt_fixes = {
                    "prompt_unavailable": "Pass prompt text or prompt_file=<readable regular file inside the workspace>.",
                    "prompt_path_forbidden": "Pass prompt_file=<readable regular file inside the workspace>.",
                    "credential_or_auth_store_denied": "Pass prompt text or a workspace prompt file outside credential and authentication stores.",
                    "prompt_hard_link_denied": "Pass prompt_file=<workspace file with one hard link>.",
                }
                errors.append({"task_id": task_id, "error": getattr(exc, "code", "worktree_invalid" if isinstance(exc, WorktreeLeaseError) else "preflight_unavailable"),
                               "fix": prompt_fixes.get(exc.code, str(exc)) if isinstance(exc, PreflightError) else "Pass a readable prompt and registered Git worktree; check adapter availability."})
    return ({"status": "rejected", "error": errors[0]["error"], "fix": errors[0]["fix"], "errors": errors}
            if errors else {"status": "validated", "routes": routes})


def run_identity(run_dir, receipt=None):
    return os.environ.get("PROVENANT_RUN_ID") or (receipt or {}).get("run_id") or (run_dir.name if run_dir.name.startswith("mcp-") else "mcp-"+run_dir.name.rsplit("-",1)[-1])


def contract_row(args,run_dir,number,attempt_dir,plan,started_at):
    route=plan.get("route",{})
    model=plan.get("model") or route.get("resolved_model") or args.model or ""
    effort=plan.get("effort") or ""
    family=route.get("model_family") or "unknown"
    label=args.tool+"/"+model+("@"+effort if effort else "")
    identity="resolved" if model else "unknown"
    provenance={"requested":{"adapter":args.tool,"alias":"" if args.model and not getattr(args,"alias_supplied",True) else args.alias or "","model":args.model,"effort":args.effort},
        "resolved_model":model,"observed_model":None,"observed_source":None,"identity":identity,
        "provider":route.get("endpoint_provider") or args.tool,"transport":args.tool,"family":family,
        "effort_requested":args.effort,"effort_applied":effort,"cli_version":route.get("cli_version"),
        "fallback_from":getattr(args,"fallback_from",None),"notes":[],"line":f"Route: {label} ({family}; {identity})"}
    return {"schema":"fabric.attempt.v1","run_id":plan.get("run_id") or run_identity(run_dir),"task_id":args.task_id,
        "attempt":number,"state":"running","status":None,"mode":args.access_mode,"cwd":plan.get("cwd") or str(Path.cwd().resolve()),
        "worktree":str(args.worktree) if args.worktree else None,"started_at":started_at,"ended_at":None,"last_progress_at":started_at,
        "pgid":None,"session_id":plan.get("session_id"),"retryable":False,"reset_at":None,"retry_after":None,"fix":None,
        "evidence":{"exit":None,"signal":None,"signature":None,"excerpt":""},"question":None,
        "applied":plan.get("applied",{"sandbox":None,"network":None,"add_dirs":[],"guarantee":"prompt_only"}),
        "warnings":list(plan.get("warnings",[])),"provenance":provenance,
        "timing":{"phases":getattr(args,"_phase_timings",{}).copy()},
        "paths":{"result":relative_path(run_dir,attempt_dir/"result.md"),"stderr":relative_path(run_dir,attempt_dir/"stderr.log"),
                 "events":relative_path(run_dir,attempt_dir/"events.jsonl"),"receipt":f"tasks/{args.task_id}/attempt-{number:03d}/attempt.json"},"digest":""}


def publish_contract(run_dir,row):
    row["run_dir"] = str(run_dir)
    row["digest"]=render_digest(row)
    path=run_dir/row["paths"]["receipt"]
    ensure_owned_directory(run_dir,path.parent)
    write_owned(run_dir,path,json.dumps(row,indent=2)+"\n")


def terminal_contract(args,run_dir,legacy,adapter,number,attempt_dir):
    row=contract_row(args,run_dir,number,attempt_dir,getattr(args,"_last_plan",{}),legacy["started_at"])
    status=adapter.get("status")
    if legacy["outcome"] in {"result_invalid_path","result_integrity_error","adapter_receipt_invalid","manifest_write_error","terminal_envelope_invalid"}: status="failed"
    elif legacy["status"] in {"cancelled","timed_out"}: status=legacy["status"]
    elif legacy["status"]=="blocked": status="input_required"
    refusal=route_refusal(adapter,args.tool)
    if refusal: status=refusal["status"]
    if status not in TERMINAL_STATUSES: status="failed"
    for field in ("session_id","retryable","reset_at","retry_after","fix","evidence","applied","context","warnings","reaped","spared","provenance","pgid","last_progress_at"):
        if field in adapter: row[field]=adapter[field]
    if refusal:
        row["fix"]=adapter.get("fix") or adapter.get("reason") or refusal["fix"];row["evidence"]=refusal["evidence"];row["error"]=refusal["error"]
    row.update(state="terminal",status=status,ended_at=legacy["finished_at"],question=adapter.get("question") or (legacy.get("question") or {}).get("prompt"))
    if not legacy.get("result"): row["paths"]["result"]=None
    row["legacy_attempt_path"]=legacy["attempt_path"]
    row["requested_route"]=legacy["requested_route"]
    return row


def resume_relaunch_context(run_dir, previous):
    tail=""
    result=previous["paths"].get("result")
    if result:
        retained=retained_path(run_dir,result)
        try:
            (run_dir / retained).lstat()
        except FileNotFoundError:
            pass  # An interrupted provider may never have produced a result.
        else:
            fd, _, _ = open_contained_regular(run_dir, retained, os.O_RDONLY, label="resume result")
            try:
                os.lseek(fd,max(0,os.fstat(fd).st_size-4096),os.SEEK_SET)
                tail=os.read(fd,4096).decode(errors="replace")
            finally:
                os.close(fd)
    return (previous.get("question") or "")+"\n"+tail


class ResumeError(ValueError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def prepare_resume(args):
    paths=sorted((args.run_dir.resolve()/"tasks").glob("*/attempt-*/attempt.json"))
    rows=[json.loads(path.read_text()) for path in paths]
    rows=[row for row in rows if row.get("run_id")==args.resume]
    if not rows: raise ValueError("resume run not found")
    tasks={row["task_id"] for row in rows}
    if args.task_id is not None:
        rows=[row for row in rows if row["task_id"]==args.task_id]
        if not rows: raise ResumeError("resume_task_unknown",f"task {args.task_id} has no attempt in this run")
    elif len(tasks)>1: raise ResumeError("resume_task_required","resume a batch task: pass task_id")
    previous=max(rows,key=lambda row:row["attempt"])
    if previous["state"]!="terminal": raise ValueError("resume requires a terminal attempt")
    route=previous.get("requested_route") or {}
    requested=previous["provenance"]["requested"]
    if (args.tool and args.tool!=requested["adapter"]) or (args.model and args.model!=previous["provenance"]["resolved_model"]):
        raise ValueError("dispatch a new run")
    args.tool=requested["adapter"];args.model=previous["provenance"]["resolved_model"];args.alias=None;args.task_class=None
    # An effort the provider only reported was never sent, so resume does not send it.
    args.effort=None if previous["provenance"].get("effort_observed_source") else previous["provenance"]["effort_applied"];args.task_id=previous["task_id"]
    args.access_mode=previous["mode"];args.worktree=Path(previous["worktree"]) if previous.get("worktree") else None
    args.provider_cwd=Path(previous["cwd"]) if previous["mode"]=="read_only" else None
    args.workspace_root=Path((previous.get("workspace") or {}).get("root") or Path.cwd()).expanduser().resolve()
    args.sandbox=previous["applied"]["sandbox"];args.network=None if previous["applied"]["network"] is None else str(previous["applied"]["network"]).lower()
    args.add_dirs=previous["applied"]["add_dirs"];args.resume_session=previous["session_id"]
    args.fallback="false"
    for field in ("intent", "orchestrator_family", "role", "risk_tier", "model_override_tier", "reviewer_id", "preface"):
        if field in route:
            setattr(args, field, route[field])
    args.resume_previous=previous
    observed_session=False
    if args.tool=="claude" and previous["status"] in {"timed_out","cancelled","stalled","interrupted"}:
        events=previous["paths"].get("events")
        if events:
            retained=retained_path(args.run_dir,events)
            try:
                (args.run_dir / retained).lstat()
            except FileNotFoundError:
                data=b""
            else:
                data=read_bound_bytes(args.run_dir,retained,label="resume events")
            for line in data.splitlines():
                if b'"session_id"' not in line:
                    continue
                try:
                    event=json.loads(line)
                except ValueError:
                    continue
                if isinstance(event,dict) and event.get("session_id")==args.resume_session:
                    observed_session=True
                    break
    if not args.resume_session or args.tool=="copilot" or (
        args.tool=="claude" and previous["status"] in {"timed_out","cancelled","stalled","interrupted"}
        and not observed_session
    ):
        if (args.tool=="claude" and previous["mode"]=="worktree_write"
            and previous["status"] in {"timed_out","cancelled","stalled","interrupted"}):
            raise ValueError("Claude session unavailable after incomplete writer turn; review worktree changes, then dispatch a new run")
        args.resume_session=None
        args.resume_relaunch=resume_relaunch_context(args.run_dir,previous)
    if args.context_ceiling is None:
        prior=previous.get("applied") or {}
        args.context_ceiling=prior.get("context_ceiling_requested") or (prior.get("context_ceiling_tokens") if prior.get("context_ceiling")=="enforced" else None)
    if args.resume_session:
        args.resume_advice=(previous,previous["task_id"] if len(tasks)>1 else None)
    receipt=json.loads(read_bound_bytes(args.run_dir,"RUN_RECEIPT.json",label="RUN_RECEIPT.json"))
    receipt.update(status="active",closed_at=None)
    write_owned(args.run_dir,args.run_dir/"RUN_RECEIPT.json",json.dumps(receipt,indent=2)+"\n")


def _dispatch(args: argparse.Namespace, custody=None) -> int:
    args._last_plan={}
    args._last_row=None
    owner_started_ms = None
    try:
        incoming = ({} if getattr(args, "retry_of", None) or getattr(args, "fallback_from", None)
                    else json.loads(os.environ.get("PROVENANT_FABRIC_PHASES", "{}")))
        owner_started_ms = incoming.get("owner_started_at_ms")
        measured = {key: round(float(value), 3) for key, value in incoming.items()
                    if key in {"validate", "run_dir_init", "snapshot"}
                    and type(value) in (int, float) and math.isfinite(value) and value >= 0}
    except (ValueError, TypeError, AttributeError):
        measured = {}
    args._phase_timings = dict.fromkeys(
        ("validate", "run_dir_init", "owner_setup", "route_plan", "snapshot", "spawn", "provider", "finalize")
    )
    args._phase_timings.update(measured)
    run_dir = args.run_dir.resolve()
    workspace = Path(getattr(args, "workspace_root", None) or Path.cwd()).expanduser().resolve()
    provider_cwd = Path(args.provider_cwd).expanduser().resolve() if args.provider_cwd else workspace
    workspace_observation = workspace_identity(workspace, provider_cwd)
    if not contains_run(run_dir, workspace):
        return fail(run_dir, "run_dir_invalid", "run directory must be inside run_root(cwd)")
    if not run_dir.is_dir():
        return fail(run_dir, "run_custody_missing", f"run directory does not exist: {run_dir}")
    try:
        (run_dir / "RUN_RECEIPT.json").lstat()
    except FileNotFoundError:
        return fail(run_dir, "run_custody_missing", "RUN_RECEIPT.json does not exist")
    except OSError:
        return fail(run_dir, "run_custody_invalid", "RUN_RECEIPT.json is unavailable")
    try:
        (run_dir / "MANIFEST.md").lstat()
    except FileNotFoundError:
        return fail(run_dir, "run_custody_missing", "MANIFEST.md does not exist")
    except OSError:
        return fail(run_dir, "run_custody_invalid", "MANIFEST.md is unavailable")
    try:
        contained_regular_path(run_dir, "MANIFEST.md", "MANIFEST.md")
        run_receipt = json.loads(
            read_bound_bytes(run_dir, "RUN_RECEIPT.json", label="RUN_RECEIPT.json").decode("utf-8")
        )
    except (OSError, UnicodeDecodeError, ValueError, OwnedFileError):
        return fail(run_dir, "run_custody_invalid", "RUN_RECEIPT.json is not valid JSON")
    receipt_error = active_receipt_error(run_receipt)
    if receipt_error:
        status = "run_custody_closed" if "active orchestration run" in receipt_error else "run_custody_invalid"
        return fail(run_dir, status, receipt_error)
    if not TASK_ID_RE.fullmatch(args.task_id):
        return fail(run_dir, "invalid_task_id", "task id must contain only letters, numbers, '.', '_' or '-'")
    batch_dir = None
    if args.batch_id is not None:
        if not BATCH_ID_RE.fullmatch(args.batch_id):
            return fail(run_dir, "invalid_batch_id", "batch id is invalid")
        batch_dir = run_dir / "dispatch" / "batches" / args.batch_id
        if batch_dir.is_symlink() or not batch_dir.is_dir():
            return fail(run_dir, "batch_path_invalid", "batch directory does not exist")
    try:
        ensure_owned_directory(run_dir, run_dir / "dispatch" / "tasks")
        if not args.batch_child:
            reconcile_manifest(run_dir, custody)
            ensure_manifest_appendable(run_dir)
    except AttemptEvidenceError as exc:
        return fail(run_dir, "attempt_evidence_incomplete", str(exc))
    except OSError as exc:
        return fail(run_dir, "manifest_not_appendable", f"MANIFEST.md is not appendable: {exc}")

    try:
        prompt_bytes = (read_prompt_input(args.prompt_file, workspace, run_dir)
                        if args.prompt_file is not None else sys.stdin.buffer.read())
    except PreflightError as exc:
        return fail(run_dir, exc.code, str(exc))
    except OSError as exc:
        return fail(run_dir, "prompt_unavailable", str(exc))
    git_evidence_requested = args.git_evidence is not None
    git_evidence_identity: dict[str, Any] | None = None
    git_evidence_source: Path | None = None
    if args.git_evidence is not None:
        if args.tool != "agy":
            return fail(run_dir, "git_evidence_requires_agy", "Git evidence is currently supported only for Agy")
        git_evidence_source = args.git_evidence.expanduser()
        if not git_evidence_source.is_absolute():
            git_evidence_source = workspace / git_evidence_source
    if args.access_mode == "worktree_write":
        # The writable route stays an explicit, ordinary-intent request bound to
        # one worktree. Assurance work keeps the read-only guarantee it certifies.
        if args.worktree is None:
            return fail(run_dir, "worktree_required", "worktree_write access requires --worktree")
        if args.intent != "ordinary":
            return fail(run_dir, "worktree_write_intent_denied", "worktree_write access requires ordinary intent")
        if args.tool not in WORKTREE_WRITE_ADAPTERS:
            return fail(
                run_dir, "worktree_write_adapter_unsupported",
                f"worktree_write access is unsupported for adapter: {args.tool}",
            )
        try:
            args.worktree = resolve_writer_worktree(args.worktree)
        except WorktreeLeaseError as exc:
            return fail(run_dir, "worktree_invalid", str(exc))
    elif args.worktree is not None:
        return fail(run_dir, "worktree_not_applicable", "--worktree requires --access-mode worktree_write")
    if not CF_DISPATCH.is_file() or not os.access(CF_DISPATCH, os.X_OK):
        return fail(run_dir, "adapter_unavailable", f"provider adapter is missing or not executable: {CF_DISPATCH}")

    task_dir = run_dir / "dispatch" / "tasks" / args.task_id
    try:
        ensure_owned_directory(run_dir, task_dir)
    except AttemptEvidenceError as exc:
        return fail(run_dir, "attempt_path_invalid", str(exc))
    retry_of = None
    if args.retry_of:
        retry_ref = Path(args.retry_of)
        if retry_ref.is_absolute() or retry_ref.name != args.retry_of or not ATTEMPT_ID_RE.fullmatch(args.retry_of):
            return fail(run_dir, "retry_of_invalid", "retry-of must name an attempt under the same task")
        retry_dir = task_dir / args.retry_of
        if not (retry_dir.is_dir() and (retry_dir / "attempt.json").is_file()):
            return fail(run_dir, "retry_of_missing", f"retry attempt does not exist: {args.retry_of}")
        retry_of = args.retry_of
    attempt_number = max(existing_attempt_number(task_dir), existing_attempt_number(run_dir / "tasks" / args.task_id))
    attempt_id = f"attempt-{attempt_number:03d}"
    attempt_dir = task_dir / attempt_id
    try:
        create_contained_directory(
            run_dir, attempt_dir.relative_to(run_dir), label="attempt directory"
        )
    except OwnedFileError as exc:
        return fail(run_dir, "attempt_path_invalid", str(exc))
    prompt_path = attempt_dir / "prompt.md"
    result_path = attempt_dir / "result.md"
    adapter_path = attempt_dir / "adapter-receipt.json"
    stderr_path = attempt_dir / "stderr.log"
    evidence_dir = attempt_dir / "evidence" if git_evidence_requested else None
    evidence_path = evidence_dir / "git-evidence.md" if evidence_dir is not None else None
    if evidence_dir is not None and evidence_path is not None:
        try:
            create_contained_directory(run_dir, evidence_dir.relative_to(run_dir), label="Git evidence directory")
            git_evidence_identity, git_evidence_digest = _copy_git_evidence(
                workspace,
                run_dir,
                git_evidence_source if git_evidence_source is not None else evidence_path,
                evidence_path,
            )
        except (OSError, OwnedFileError, AttemptEvidenceError) as exc:
            return fail(run_dir, "git_evidence_invalid", str(exc))
        prompt_bytes = (
            (f"Read the supplied evidence files under {evidence_dir}. In particular, read "
             f"{evidence_path}. Use file-reading tools only; do not invoke shell or Git; "
             "answer from the supplied files.\n\n").encode("utf-8")
            + (prompt_bytes or b"")
        )
    try:
        atomic_write_contained(
            run_dir, prompt_path.relative_to(run_dir), prompt_bytes or b"", label="prompt"
        )
    except OwnedFileError as exc:
        return fail(run_dir, "attempt_path_invalid", str(exc))
    command = build_command(args, prompt_path, result_path, evidence_dir)
    requested_route = {
        "intent": args.intent,
        "preface": args.preface,
        "adapter": args.tool,
        "alias": args.alias or "",
        "task_class": args.task_class or "",
        "role": args.role,
        "model": args.model or "",
        "effort": args.effort or "",
        "orchestrator_family": args.orchestrator_family or "",
        "risk_tier": args.risk_tier or "",
        "model_override_tier": args.model_override_tier or "",
        "reviewer_id": args.reviewer_id or "",
        "access_mode": args.access_mode,
        "worktree": str(args.worktree) if args.worktree else "",
    }
    worktree_lease = None
    if args.access_mode == "worktree_write" and args.worktree is not None:
        try:
            worktree_lease = acquire_worktree_lease(args.worktree)
        except WorktreeLeaseError as exc:
            return fail(run_dir, "worktree_busy", str(exc))
    started_at = now()
    started = time.monotonic()
    observed_exit = False
    exit_code: int | None = None
    process_error = ""
    process = None
    provider_temporary = None
    cancelled = False
    old_handlers: dict[int, Any] = {}
    if CF_DISPATCH == Path(__file__).with_name("cf_dispatch.sh"):
        owner_cancel=[False]
        def cancel_owner(_signal,_frame): owner_cancel[0]=True
        old_handlers={sig:signal.getsignal(sig) for sig in (signal.SIGTERM,signal.SIGHUP)}
        for sig in old_handlers: signal.signal(sig,cancel_owner)
        try:
            plan_started = time.monotonic()
            if (type(owner_started_ms) in (int, float) and math.isfinite(owner_started_ms)
                    and not getattr(args, "fallback_from", None)):
                args._phase_timings["owner_setup"] = round(max(0, time.time() * 1000 - owner_started_ms), 3)
            plan_environment=routing_environment()
            if git_evidence_requested: plan_environment.pop("CF_DISPATCH_AGY_ADD_DIR",None)
            fast_plan = fast_fabric_plan(args, prompt_path, result_path, workspace)
            planning = None
            if fast_plan is None:
                planning = subprocess.run([*command,"--plan-only"],cwd=workspace,env=plan_environment,capture_output=True,text=True,timeout=30)
            args._phase_timings["route_plan"] = round((time.monotonic() - plan_started) * 1000, 3)
            plan = fast_plan if fast_plan is not None else planner_result(planning)
            if plan.get("schema") == "fabric.exec-plan.v1":
                provider_cwd = Path(plan.get("cwd") or workspace).resolve()
                workspace_observation["cwd"] = str(provider_cwd)
                plan.update(timeout_seconds=args.timeout_seconds,run_id=run_identity(run_dir,run_receipt),chair=os.environ.get("PROVENANT_CHAIR") or os.environ.get("AGENT_FABRIC_SEAT", ""),fallback_from=getattr(args,"fallback_from",None))
                if hasattr(args,"resume_relaunch"):
                    plan["prompt"] += "\n\nPrevious turn and question:\n"+args.resume_relaunch
                    plan["argv"] = provider_exec.profile(args.tool).argv(plan)
                cooling=exec_routing.cooling(args.tool,plan["model"])
                if cooling:
                    if args.model:
                        plan["warnings"].append("explicit model is cooling until "+cooling["cooling_until"])
                    else:
                        for candidate in exec_routing.candidates(plan,True):
                            if exec_routing.cooling(candidate["adapter"],candidate["model"]): continue
                            alternate=argparse.Namespace(**vars(args))
                            alternate.tool=candidate["adapter"];alternate.model=candidate["model"];alternate.alias=None;alternate.task_class=None;alternate.effort=candidate.get("effort")
                            resolved=subprocess.run([*build_command(alternate,prompt_path,result_path,evidence_dir),"--plan-only"],cwd=workspace,env=routing_environment(),capture_output=True,text=True,timeout=30)
                            try: replacement=json.loads(resolved.stdout)
                            except ValueError: continue
                            if replacement.get("schema")!="fabric.exec-plan.v1": continue
                            replacement.update(run_id=plan["run_id"],chair=plan["chair"])
                            replacement["warnings"].append("skipped cooling alias candidate "+args.tool+"/"+plan["model"])
                            plan=replacement;args.tool=candidate["adapter"];break
                        else:
                            plan["warnings"].append("all alias candidates cooling")
                            plan["cooldown_blocked"]=cooling
                if args.context_ceiling is not None:
                    context_usage.apply_ceiling(plan,args.context_ceiling,provider_exec.profile(plan["adapter"]).argv)
                if getattr(args,"resume_advice",None) and plan.get("resume_session"):
                    previous_row,advice_task=args.resume_advice
                    args.resume_warning=context_usage.resume_warning(previous_row,plan["adapter"],
                        context_usage.effective_ceiling(plan["applied"]),args.resume,task_id=advice_task)
                    if args.resume_warning: plan["warnings"].insert(0,args.resume_warning)
                active = contract_row(args,run_dir,attempt_number,attempt_dir,plan,started_at)
                active["requested_route"] = requested_route
                publish_contract(run_dir,active)
                spawn_started = time.monotonic()
                provider_started_at = [None]
                def provider_started(child):
                    nonlocal process
                    process=child
                    provider_started_at[0] = time.monotonic()
                    args._phase_timings["spawn"] = round((provider_started_at[0] - spawn_started) * 1000, 3)
                    active["pgid"]=child.pid
                    _record_provider_process(run_dir,child)
                    write_owned(run_dir,attempt_dir/"pgid",str(child.pid)+"\n")
                    publish_contract(run_dir,active)
                progress_publication=[0.0]
                def progress(at):
                    active["last_progress_at"]=at
                    if time.monotonic()-progress_publication[0]>=1:
                        publish_contract(run_dir,active)
                        progress_publication[0]=time.monotonic()
                def cancellation():
                    return owner_cancel[0] or cancellation_marker_present(run_dir,attempt_dir) or (batch_dir is not None and cancellation_marker_present(run_dir,batch_dir))
                adapter_record=provider_exec.execute(plan,result_path,events_path=attempt_dir/"events.jsonl",stderr_path=stderr_path,
                    on_start=provider_started,on_progress=progress,cancelled=cancellation)
                args._phase_timings["provider"] = round((time.monotonic() - (provider_started_at[0] or spawn_started)) * 1000, 3)
                if (args.resume and args.tool=="claude" and plan.get("resume_session")
                    and adapter_record.get("status")=="failed"
                    and re.search(r"no conversation found",adapter_record.get("evidence",{}).get("excerpt") or "",re.I)):
                    previous=args.resume_previous
                    if (previous["mode"]=="worktree_write" and previous["status"] in {"timed_out","cancelled","stalled","interrupted"}):
                        adapter_record["status"]="rejected"
                        adapter_record["fix"]="Review worktree changes, then dispatch a new run."
                        adapter_record["evidence"]["signature"]="resume_session_missing"
                    else:
                        for diagnostic in (attempt_dir/"events.jsonl",stderr_path):
                            if diagnostic.exists(): diagnostic.rename(diagnostic.with_name(diagnostic.name+".resume-failed"))
                        plan["resume_session"]=None
                        if getattr(args,"resume_warning",None) in plan["warnings"]: plan["warnings"].remove(args.resume_warning)
                        plan["session_id"]=str(uuid.uuid4())
                        args.resume_relaunch=resume_relaunch_context(run_dir,previous)
                        plan["prompt"] += "\n\nPrevious turn and question:\n"+args.resume_relaunch
                        plan["argv"]=provider_exec.profile(args.tool).argv(plan)
                        active["session_id"]=plan["session_id"]
                        publish_contract(run_dir,active)
                        adapter_record=provider_exec.execute(plan,result_path,events_path=attempt_dir/"events.jsonl",stderr_path=stderr_path,
                            on_start=provider_started,on_progress=progress,cancelled=cancellation)
                if hasattr(args,"resume_relaunch"):
                    adapter_record["provenance"]["notes"].append("resumed_by_relaunch")
                    adapter_record["warnings"].append("resume: relaunched")
                args._last_plan=plan
            else:
                adapter_record=plan
                write_owned(run_dir,stderr_path,planning.stderr if planning is not None else "")
            write_owned(run_dir,adapter_path,json.dumps(adapter_record)+"\n")
            exit_code=adapter_record.get("exit")
            if type(exit_code) is not int: exit_code=1
            observed_exit=True
            if adapter_record.get("status")=="cancelled" or adapter_record.get("evidence",{}).get("signature")=="wall_clock":
                process_error="cancelled" if adapter_record["status"]=="cancelled" else "timeout"
        except (OSError,ValueError,subprocess.SubprocessError) as exc:
            process_error=str(exc)
            if not adapter_path.exists(): write_owned(run_dir,adapter_path,"{}\n")
            if not stderr_path.exists(): write_owned(run_dir,stderr_path,str(exc))
        finally:
            release_worktree_lease(worktree_lease)
    else:
        try:
            with owned_text_file(run_dir, adapter_path, "w") as adapter_stream, owned_text_file(
                run_dir, stderr_path, "w"
            ) as stderr_stream:
                attempt_cancelled = cancellation_marker_present(run_dir, attempt_dir)
                batch_cancelled = batch_dir is not None and cancellation_marker_present(run_dir, batch_dir)
                if attempt_cancelled or batch_cancelled:
                    # The owner still writes the ordinary attempt evidence, but no
                    # provider process is created for a pre-launch request.
                    process_error = "cancelled"
                    observed_exit = True
                else:
                    cancel_pending = False

                    def cancel_handler(_signum: int, _frame: Any) -> None:
                        nonlocal cancel_pending
                        # Popen can have spawned the provider before returning.
                        # Keep the handler signal-safe: normal control flow
                        # reconciles the intent and owns process-group cleanup.
                        cancel_pending = True

                    old_handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGHUP)}
                    signal.signal(signal.SIGTERM, cancel_handler)
                    signal.signal(signal.SIGHUP, cancel_handler)
                    provider_environment = os.environ.copy()
                    # Owners retain chair custody; provider work must discover its own
                    # seat, state directory and checkout rather than inherit the chair's.
                    for name in ("AGENT_FABRIC_STATE_DIRECTORY", "AGENT_FABRIC_SEAT",
                                 "AGENT_FABRIC_CLIENT_LABEL", "AGENT_FABRIC_LABEL", "AGENT_FABRIC_PRODUCT_ROOT",
                                 "PROVENANT_FABRIC_PHASES", "PROVENANT_NO_OS_CONFINEMENT"):
                        provider_environment.pop(name, None)
                    for name in list(provider_environment):
                        if name.startswith(("PROVENANT_RUN_", "PROVENANT_PREFLIGHT_")):
                            provider_environment.pop(name)
                    if os.environ.get("PROVENANT_RUN_TOKEN"):
                        # cf_dispatch buffers stdout/stderr here until completion.
                        # Status can observe mtimes without reading provider output.
                        provider_temporary = tempfile.TemporaryDirectory(prefix="fabric-provider-", ignore_cleanup_errors=True)
                        provider_environment["TMPDIR"] = provider_temporary.name
                        atomic_write_contained(run_dir, (attempt_dir / "provider-output.json").relative_to(run_dir),
                                               (json.dumps({"directory": provider_temporary.name}) + "\n").encode(), label="provider output location")
                    if git_evidence_requested:
                        provider_environment.pop("CF_DISPATCH_AGY_ADD_DIR", None)
                    process = subprocess.Popen(
                        command,
                        cwd=workspace,
                        stdout=adapter_stream,
                        stderr=stderr_stream,
                        env=provider_environment,
                        start_new_session=True,
                    )
                    _record_provider_process(run_dir, process)

                    # A request can arrive after the provider is spawned but
                    # before Popen returns. Reconcile it before entering the
                    # normal wait loop, preserving natural exit at the boundary.
                    marker_seen = cancellation_marker_present(run_dir, attempt_dir)
                    if batch_dir is not None:
                        marker_seen = marker_seen or cancellation_marker_present(run_dir, batch_dir)
                    if cancel_pending or marker_seen:
                        exit_code = process.poll()
                        if exit_code is None:
                            cancelled = True
                            stop_process_group(process)
                            exit_code = process.wait()
                            process_error = "cancelled"
                            observed_exit = True
                    try:
                        if not observed_exit:
                            deadline = time.monotonic() + args.timeout_seconds
                        while not observed_exit:
                            # Poll first so an already-observed natural exit wins a
                            # marker race.
                            exit_code = process.poll()
                            if exit_code is not None:
                                observed_exit = True
                                break
                            if cancel_pending:
                                stop_process_group(process)
                                exit_code = process.wait()
                                process_error = "cancelled"
                                observed_exit = True
                                break
                            marker_seen = cancellation_marker_present(run_dir, attempt_dir)
                            if batch_dir is not None:
                                marker_seen = marker_seen or cancellation_marker_present(run_dir, batch_dir)
                            if marker_seen:
                                # Re-check before stopping to preserve a natural
                                # exit that became observable at the boundary.
                                exit_code = process.poll()
                                if exit_code is not None:
                                    observed_exit = True
                                    break
                                stop_process_group(process)
                                exit_code = process.wait()
                                process_error = "cancelled"
                                observed_exit = True
                                break
                            remaining = deadline - time.monotonic()
                            if remaining <= 0:
                                stop_process_group(process)
                                exit_code = process.wait()
                                process_error = "timeout"
                                observed_exit = True
                                break
                            try:
                                exit_code = process.wait(timeout=min(0.1, remaining))
                                observed_exit = True
                                break
                            except subprocess.TimeoutExpired:
                                continue
                            except KeyboardInterrupt:
                                cancelled = True
                                stop_process_group(process)
                                exit_code = process.wait()
                                observed_exit = True
                                break
                    finally:
                        # Signal ownership deliberately remains with this
                        # dispatch owner through evidence publication below.
                        pass
                    if cancelled:
                        process_error = "cancelled"
        except OSError as exc:
            process_error = str(exc)
        finally:
            release_worktree_lease(worktree_lease)
            if provider_temporary is not None:
                provider_temporary.cleanup()

    finalize_started = time.monotonic()
    finished_at = now()
    duration_seconds = round(time.monotonic() - started, 6)
    adapter: dict[str, Any] = {}
    adapter_text = adapter_path.read_text(encoding="utf-8", errors="replace") if adapter_path.exists() else ""
    try:
        parsed_lines = [json.loads(line) for line in adapter_text.splitlines() if line.strip()]
        if parsed_lines and isinstance(parsed_lines[-1], dict):
            adapter = parsed_lines[-1]
        else:
            raise ValueError("adapter did not emit a JSON object")
    except (ValueError, json.JSONDecodeError):
        pass

    try:
        result_metadata = result_path.lstat()
    except OSError:
        result_metadata = None
    result_invalid = result_metadata is not None and not valid_regular_result(run_dir, result_path)
    if result_invalid and result_metadata is not None and not stat.S_ISDIR(result_metadata.st_mode):
        result_path.unlink(missing_ok=True)
    result_exists = valid_regular_result(run_dir, result_path)
    result_nonempty = result_exists and result_path.stat().st_size > 0
    result_digest = digest(result_path) if result_exists else ""
    adapter_status = str(adapter.get("status", "")) if adapter else "adapter_receipt_invalid"
    receipt_error = (
        success_receipt_error(adapter, args, result_path, result_digest)
        if adapter_status == "ok" and result_exists
        else None
    )
    question: dict[str, Any] | None = None
    terminal_envelope_error = False
    result_integrity_error = False
    if observed_exit and exit_code == 0 and adapter_status == "ok" and not receipt_error and result_nonempty:
        try:
            question = worker_question_envelope(result_path, result_digest)
        except TerminalEnvelopeIntegrityError:
            result_integrity_error = True
        except ValueError:
            terminal_envelope_error = True
    # A result that is missing, empty or truncated because the deadline expired is
    # a timeout, not a content or receipt defect, so the deadline is checked
    # before any of those classifications. `completed_cleanly` keeps a run that
    # answered inside its budget out of the timeout arm even when it finished at
    # the boundary.
    completed_cleanly = (
        observed_exit and exit_code == 0 and adapter_status == "ok"
        and result_nonempty and not receipt_error
        and not result_integrity_error and not terminal_envelope_error
    )
    deadline_reached = duration_seconds >= provider_timeout_seconds(args.timeout_seconds)
    if process_error == "timeout":
        status = "timed_out"
        outcome = "timeout"
    elif process_error == "cancelled":
        status = "cancelled"
        outcome = "cancelled"
    elif not observed_exit:
        status = "failed"
        outcome = "process_spawn_error"
    elif result_invalid:
        status = "failed"
        outcome = "result_invalid_path"
    elif adapter_status in {"timeout","timed_out"}:
        # The provider reached its own deadline first, which is what the smaller
        # provider timeout is for.
        status = "timed_out"
        outcome = "provider_timeout"
    elif deadline_reached and not completed_cleanly:
        status = "timed_out"
        outcome = "deadline_exceeded"
    elif adapter_status == "ok" and receipt_error:
        status = "failed"
        outcome = "adapter_receipt_invalid"
    elif result_integrity_error:
        status = "failed"
        outcome = "result_integrity_error"
    elif terminal_envelope_error:
        status = "failed"
        outcome = "terminal_envelope_invalid"
    elif adapter_status == "input_required":
        question={"code":"needs_input","prompt":adapter.get("question") or "Input required"}
        status="blocked"
        outcome="question"
    elif question is not None:
        status = "blocked"
        outcome = "question"
    elif exit_code == 0 and adapter_status == "ok" and result_nonempty:
        status = "ok"
        outcome = "ok"
    elif exit_code == 0 and adapter_status == "ok":
        status = "failed"
        outcome = "result_missing_or_empty"
    else:
        status = "failed"
        outcome = adapter_status or ("adapter_exit" if exit_code else "empty_result")

    try:
        preflight_route = json.loads(os.environ.get("PROVENANT_PREFLIGHT_ROUTES", "{}"))[args.task_id]
        if not isinstance(preflight_route, dict):
            preflight_route = {}
    except (ValueError, KeyError, TypeError):
        preflight_route = {}
    record: dict[str, Any] = {
        "schema_version": 1,
        "record_type": "dispatch-attempt",
        "run_id": run_dir.name,
        "task_id": args.task_id,
        "attempt_id": attempt_id,
        "retry_of": retry_of,
        "intent": args.intent,
        "requested_route": requested_route,
        "route": {
            "adapter": args.tool, "alias": args.alias or "", "model": args.model or "", "effort": args.effort or "",
            **{key: value for key, value in preflight_route.items() if key in {
                "adapter", "alias", "model", "effort", "resolved_model", "provider_family", "model_family", "execution_intent",
            }},
            **adapter,
            "adapter_receipt": {
                "path": relative_path(run_dir, adapter_path),
                "digest": digest(adapter_path),
            },
        },
        "outcome": outcome,
        "failure_code": (
            None if status == "ok" else (question["code"] if question is not None else outcome)
        ),
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_seconds": duration_seconds,
        "workspace": workspace_observation,
        "prompt": {"path": relative_path(run_dir, prompt_path), "digest": digest(prompt_path)},
        "result": (
            {"path": relative_path(run_dir, result_path), "digest": result_digest}
            if result_exists
            else None
        ),
        "stderr": {"path": relative_path(run_dir, stderr_path), "digest": digest(stderr_path)},
        "process": {
            "pid": process.pid if process is not None else None,
            "started_at": started_at,
            "finished_at": finished_at,
            "exit_code": exit_code,
            "observed_exit": observed_exit,
            "terminating_signal": -exit_code if isinstance(exit_code, int) and exit_code < 0 else None,
        },
        "argv_digest": json_digest(command),
        "retry_lineage": [retry_of] if retry_of else [],
    }
    if evidence_path is not None and git_evidence_identity is not None:
        record["git_evidence"] = {
            "path": relative_path(run_dir, evidence_path),
            "digest": git_evidence_digest,
            "checkout": git_evidence_identity,
        }
    if question is not None:
        record["question"] = question
    if process_error:
        record["process_error"] = process_error
    attempt_path = attempt_dir / "attempt.json"
    record["attempt_path"] = relative_path(run_dir, attempt_path)
    digest_path = attempt_dir / "attempt.sha256"
    record["attempt_digest_path"] = relative_path(run_dir, digest_path)
    write_owned(run_dir, attempt_path, json.dumps(record, indent=2, sort_keys=True) + "\n")
    attempt_digest = digest(attempt_path)
    write_owned(run_dir, digest_path, f"{attempt_digest}  {attempt_path.name}\n")
    manifest_error = False
    try:
        if args.batch_child:
            manifest_error = False
        else:
            append_manifest(run_dir, record, custody)
    except OSError as exc:
        manifest_error = True
        record["status"] = "failed"
        record["outcome"] = "manifest_write_error"
        record["failure_code"] = "manifest_write_error"
        record["manifest_error"] = str(exc)
        write_owned(run_dir, attempt_path, json.dumps(record, indent=2, sort_keys=True) + "\n")
        attempt_digest = digest(attempt_path)
        write_owned(run_dir, digest_path, f"{attempt_digest}  {attempt_path.name}\n")
    remove_cancellation_marker(run_dir, attempt_dir)
    for sig, handler in old_handlers.items():
        signal.signal(sig, handler)
    row = terminal_contract(args,run_dir,record,adapter,attempt_number,attempt_dir)
    for action in (lambda: write_cooldown(row),lambda: append_index(row,run_dir,root=run_workspace(run_dir, Path.cwd())/".agent-run")):
        try: action()
        except (OSError,ValueError) as exc: row["warnings"].append("terminal index unavailable: "+str(exc))
    publish_contract(run_dir,row)
    # Include the first durable terminal publication; rewrite only its timing.
    row["timing"]["phases"]["finalize"] = round((time.monotonic() - finalize_started) * 1000, 3)
    publish_contract(run_dir,row)
    args._last_row=row
    output_record = {**record, "attempt_digest": attempt_digest, "fabric":row, "digest":row["digest"]}
    # Batch children retain the legacy record for the batch evidence validator.
    # The MCP front door consumes the canonical attempt as its terminal row.
    print(json.dumps(row if os.environ.get("PROVENANT_RUN_TOKEN") and not args.batch_child else output_record, sort_keys=True))
    return 0 if status == "ok" and not manifest_error else 1


def close_mcp_run(run_dir: Path) -> None:
    """Close execution custody only; this is not a delivery/assurance final gate."""
    if run_dir.parent.name != "runs" and (not os.environ.get("PROVENANT_RUN_TOKEN") or os.environ.get("PROVENANT_RUN_DIR") != str(run_dir)):
        return
    try:
        attempts = list((run_dir / "dispatch/tasks").glob("*/attempt-*/attempt.json"))
        records = [json.loads(read_bound_bytes(run_dir, path.relative_to(run_dir), label="attempt.json")) for path in attempts]
        # The batch owner calls this after joining all children, under its custody lock.
        receipt = json.loads(read_bound_bytes(run_dir, "RUN_RECEIPT.json", label="RUN_RECEIPT.json"))
        if receipt.get("status") != "active":
            return
        statuses_by_task = {record["task_id"]: canonical_success_status(record["status"]) for record in records}
        for summary_path in (run_dir / "dispatch/batches").glob("*/summary.json"):
            summary = json.loads(read_bound_bytes(run_dir, summary_path.relative_to(run_dir), label="summary.json"))
            if summary.get("status") not in {"completed", "failed", "cancelled"}:
                return
            statuses_by_task.update({task["task_id"]: canonical_success_status(task.get("status", "failed")) for task in summary.get("tasks", [])})
        canonical = [json.loads(read_bound_bytes(run_dir, path.relative_to(run_dir), label="attempt.json"))
                     for path in (run_dir / "tasks").glob("*/attempt-*/attempt.json")]
        if canonical:
            latest={}
            for row in canonical:
                if row["task_id"] not in latest or row["attempt"]>latest[row["task_id"]]["attempt"]: latest[row["task_id"]]=row
            if any(row.get("state") != "terminal" or canonical_success_status(row.get("status")) not in TERMINAL_STATUSES for row in latest.values()):
                return
            receipt["attempts"]=sorted(({**row, "status": canonical_success_status(row["status"])} for row in canonical),
                                       key=lambda row:(row["task_id"],row["attempt"]))
            receipt["run_id"]=canonical[0]["run_id"]
            receipt["resumable"]=any(row.get("status")=="input_required" for row in latest.values())
            statuses_by_task.update({task_id: canonical_success_status(row["status"])
                                     for task_id, row in latest.items()})
        elif any(canonical_success_status(record.get("status")) not in {"ok", "failed", "blocked", "timed_out", "cancelled"} for record in records):
            return
        statuses = set(statuses_by_task.values())
        if not statuses:
            return
        receipt.update(status="ok" if statuses == {"ok"} else "cancelled" if statuses == {"cancelled"} else "input_required" if receipt.get("resumable") else "failed",
                       closed_at=now(), terminal_reason=None if statuses == {"ok"} else "MCP execution attempts are terminal")
        write_owned(run_dir, run_dir / "RUN_RECEIPT.json", json.dumps(receipt, indent=2) + "\n")
    except (OSError, ValueError, OwnedFileError):
        pass


def execute_attempt_sequence(args,custody=None):
    try:
        exec_routing.validate_policy(args.fallback)
    except ValueError as exc:
        return fail(args.run_dir, "fallback_invalid", str(exc))
    sequence_start=time.monotonic()
    sequence_budget=args.timeout_seconds
    result = _dispatch(args, custody)
    previous=getattr(args,"_last_row",None)
    plan=getattr(args,"_last_plan",None)
    if plan and previous and previous["retryable"] and not args.resume:
        for candidate in exec_routing.candidates(plan,args.fallback):
            remaining=sequence_budget-(time.monotonic()-sequence_start)
            if remaining<=0: break
            args.timeout_seconds=remaining
            if exec_routing.cooling(candidate["adapter"],candidate["model"]): continue
            args.fallback_from={"attempt":previous["attempt"],"status":previous["status"],"reset_at":previous.get("reset_at"),"route":previous["provenance"]["line"].split(" (",1)[0].removeprefix("Route: ")}
            args.tool=candidate["adapter"];args.model=candidate["model"];args.effort=candidate.get("effort")
            args.alias=None;args.task_class=None;args.retry_of=f"attempt-{previous['attempt']:03d}"
            result=_dispatch(args,custody)
            previous=getattr(args,"_last_row",None)
            if not previous or not previous["retryable"]: break
    return result


def dispatch(args: argparse.Namespace) -> int:
    """Run one attempt while serialising standalone run-ledger mutation."""
    if args.timeout_seconds is None:
        args.timeout_seconds = 10800.0 if args.access_mode == "worktree_write" else DEFAULT_TIMEOUT_SECONDS
    run_dir = args.run_dir.resolve()
    workspace = Path.cwd().resolve()
    owned = contains_run(run_dir, workspace) and run_dir.is_dir() and (run_dir / "MANIFEST.md").is_file()
    if args.task_id is None and not (args.resume and owned and not args.batch_child):
        args.task_id = "dispatch-001"  # A resume names its task, or takes the run's only one.
    if not owned:
        return _dispatch(args)
    if args.batch_child:
        return execute_attempt_sequence(args)
    try:
        custody = acquire_run_custody(run_dir)
    except (OSError, OwnedFileError):
        return fail(run_dir, "run_custody_busy", "another dispatch, batch or finalizer owns the run")
    try:
        if args.resume:
            try: prepare_resume(args)
            except (OSError,ValueError) as exc: return fail(run_dir,"rejected",str(exc),getattr(exc,"code",None))
        if not args.tool or not any((args.alias,args.task_class,args.model)):
            return fail(run_dir,"rejected","adapter and alias or model are required")
        result = execute_attempt_sequence(args,custody)
        close_mcp_run(run_dir)
        return result
    finally:
        fcntl.flock(custody.fileno(), fcntl.LOCK_UN)
        custody.close()


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--run-dir", type=Path, required=True)
    root.add_argument("--task-id")
    adapter = root.add_mutually_exclusive_group(required=False)
    adapter.add_argument("--adapter", "--tool", dest="tool")
    prompt = root.add_mutually_exclusive_group(required=True)
    prompt.add_argument("--prompt-file", type=Path)
    prompt.add_argument("--prompt-stdin", action="store_true", help="read the prompt bytes from stdin")
    root.add_argument("--intent", choices=("ordinary", "assurance"), default="ordinary")
    root.add_argument("--orchestrator-family")
    selector = root.add_mutually_exclusive_group(required=False)
    selector.add_argument("--alias")
    selector.add_argument("--task-class")
    selector.add_argument("--model")
    root.add_argument("--role", default="worker")
    root.add_argument("--risk-tier")
    root.add_argument(
        "--model-override-tier",
        choices=("routine", "substantial", "crucial", "terminal"),
        help="explicit special-model selection; independent of lifecycle risk metadata",
    )
    root.add_argument("--reviewer-id")
    root.add_argument(
        "--access-mode", choices=ACCESS_MODES, default="read_only",
        help="read_only (default) or worktree_write for a worker that owns a worktree",
    )
    root.add_argument(
        "--worktree", type=Path,
        help="Git worktree root the writer owns exclusively; requires worktree_write",
    )
    root.add_argument("--effort")
    root.add_argument(
        "--timeout", "--timeout-seconds", dest="timeout_seconds", type=timeout_value,
        default=None,
        help="maximum provider runtime: read_only 3600 seconds, worktree_write 10800",
    )
    root.add_argument("--retry-of", help="existing attempt id under this task, for lineage only")
    root.add_argument(
        "--git-evidence", type=Path,
        help="run-owned Git evidence packet to copy into an Agy attempt",
    )
    root.add_argument("--batch-child", action="store_true", help=argparse.SUPPRESS)
    root.add_argument("--batch-id", help=argparse.SUPPRESS)
    root.add_argument("--cwd",dest="provider_cwd",type=Path)
    root.add_argument("--resume", help="resume this run id; inherit route and controls")
    root.add_argument("--context-ceiling", type=float, help="auto-compaction threshold in tokens, clamped to 100k-1M")
    root.add_argument("--sandbox", choices=("read-only","workspace-write","full"))
    root.add_argument("--network", choices=("true","false"))
    root.add_argument("--add-dir", dest="add_dirs", action="append", default=[])
    root.add_argument("--no-preface", dest="preface", action="store_false")
    root.add_argument("--fallback", default=None, help="false, true, any, or JSON route list")
    return root


if __name__ == "__main__":
    if sys.argv[1:] == ["--preflight-json"]:
        print(json.dumps(preflight_tasks(json.load(sys.stdin)["tasks"])))
    else:
        raise SystemExit(dispatch(parser().parse_args()))
