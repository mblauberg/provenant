#!/usr/bin/env python3
"""Run one configured provider attempt and retain compact execution evidence.

This is deliberately a one-attempt owner.  Provider command construction stays
with ``cf_dispatch.sh``; this module owns only the regular-file boundary,
process wait and attempt custody. Provider execution is bounded to 900 seconds
by default; callers may provide a smaller or larger finite positive timeout.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
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
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "skills"))
CF_DISPATCH = Path(__file__).with_name("cf_dispatch.sh")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
ATTEMPT_ID_RE = re.compile(r"^attempt-(?P<number>\d{3}|[1-9]\d{3,})$")
BATCH_ID_RE = re.compile(r"^batch-(?:\d{3}|[1-9]\d{3,})$")
DEFAULT_TIMEOUT_SECONDS = 900.0
MAX_WORKER_QUESTION_PROMPT = 4096
MAX_WORKER_TERMINAL_ENVELOPE_BYTES = 64 * 1024
WORKER_TERMINAL_RECORD_TYPE = "provenant-worker-terminal"
CANCEL_MARKER_NAME = "cancel.request"

from _shared.bounded_process import stop_process_group
from _shared.custody import (
    OwnedFileError, OwnedLinkError, atomic_write_contained, contained_regular_path,
    ensure_contained_directory, create_contained_directory, open_contained_regular, read_bound_bytes,
)

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


def workspace_identity(workspace: Path) -> dict[str, Any]:
    identity: dict[str, Any] = {
        "cwd": str(workspace),
        "root": str(workspace),
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
            root=str(Path(base[0]).resolve()),
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
    required = (
        "tool", "adapter", "execution_intent", "resolved_model", "provider_family",
        "model_family", "endpoint_provider", "identity_source", "output_path", "output_digest",
        "read_only_guarantee",
    )
    if any(not isinstance(adapter.get(field), str) or not adapter[field] for field in required):
        return "successful adapter receipt is missing route identity"
    if adapter["tool"] != args.tool or adapter["adapter"] != args.tool:
        return "successful adapter receipt does not match the requested adapter"
    if adapter["execution_intent"] != args.intent:
        return "successful adapter receipt does not match the requested intent"
    try:
        output_matches = Path(adapter["output_path"]).resolve() == result_path.resolve()
    except (OSError, ValueError):
        output_matches = False
    if not output_matches or adapter["output_digest"] != result_digest:
        return "successful adapter receipt does not match the retained output"
    if (
        not isinstance(adapter.get("exit"), int)
        or isinstance(adapter["exit"], bool)
        or adapter["exit"] != 0
    ):
        return "successful adapter receipt must record exit 0"
    if not isinstance(adapter.get("cross_family"), bool) or not isinstance(
        adapter.get("certification_eligible"), bool
    ):
        return "successful adapter receipt is missing assurance flags"
    if args.intent == "ordinary" and adapter["certification_eligible"]:
        return "ordinary execution cannot be certification eligible"
    return None


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


def fail(run_dir: Path | None, status: str, message: str) -> int:
    record = {"schema_version": 1, "status": status, "message": message}
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


def build_command(args: argparse.Namespace, prompt_path: Path, result_path: Path) -> list[str]:
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
        ("--reviewer-id", args.reviewer_id),
        ("--model", args.model),
        ("--effort", args.effort),
    ):
        if value:
            command.extend((flag, value))
    return command


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


def _dispatch(args: argparse.Namespace, custody=None) -> int:
    run_dir = args.run_dir.resolve()
    workspace = Path.cwd().resolve()
    if run_dir != workspace and workspace not in run_dir.parents:
        return fail(run_dir, "run_dir_invalid", "run directory must be inside the current workspace")
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

    prompt_source = None
    if args.prompt_file is not None:
        prompt_source = args.prompt_file.expanduser()
        if not prompt_source.is_absolute():
            prompt_source = workspace / prompt_source
    prompt_bytes: bytes | None = None
    if prompt_source is not None:
        if not prompt_source.exists():
            return fail(run_dir, "prompt_unavailable", f"cannot read prompt file: {prompt_source}")
        if not (prompt_source == run_dir or run_dir in prompt_source.parents or workspace in prompt_source.parents):
            return fail(run_dir, "prompt_path_forbidden", "prompt file must be inside the run directory or current workspace")
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
            return fail(run_dir, "credential_or_auth_store_denied", "prompt path is a credential or authentication store")
        try:
            prompt_bytes = _read_prompt_once(workspace, prompt_source)
        except OwnedLinkError as exc:
            return fail(run_dir, "prompt_hard_link_denied", str(exc))
        except OwnedFileError as exc:
            return fail(run_dir, "prompt_unavailable", str(exc))
    else:
        try:
            prompt_bytes = sys.stdin.buffer.read()
        except OSError as exc:
            return fail(run_dir, "prompt_unavailable", f"cannot read prompt stdin: {exc}")
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
    attempt_number = existing_attempt_number(task_dir)
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
    try:
        atomic_write_contained(
            run_dir, prompt_path.relative_to(run_dir), prompt_bytes or b"", label="prompt"
        )
    except OwnedFileError as exc:
        return fail(run_dir, "attempt_path_invalid", str(exc))
    command = build_command(args, prompt_path, result_path)
    requested_route = {
        "intent": args.intent,
        "adapter": args.tool,
        "alias": args.alias or "",
        "task_class": args.task_class or "",
        "role": args.role,
        "model": args.model or "",
        "effort": args.effort or "",
        "orchestrator_family": args.orchestrator_family or "",
        "risk_tier": args.risk_tier or "",
        "reviewer_id": args.reviewer_id or "",
    }
    workspace_observation = workspace_identity(workspace)
    started_at = now()
    started = time.monotonic()
    observed_exit = False
    exit_code: int | None = None
    process_error = ""
    process = None
    cancelled = False
    old_handlers: dict[int, Any] = {}
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
                process = subprocess.Popen(
                    command,
                    cwd=workspace,
                    stdout=adapter_stream,
                    stderr=stderr_stream,
                    env=os.environ.copy(),
                    start_new_session=True,
                )

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

    finished_at = now()
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
    elif adapter_status == "ok" and receipt_error:
        status = "failed"
        outcome = "adapter_receipt_invalid"
    elif result_integrity_error:
        status = "failed"
        outcome = "result_integrity_error"
    elif terminal_envelope_error:
        status = "failed"
        outcome = "terminal_envelope_invalid"
    elif question is not None:
        status = "blocked"
        outcome = "question"
    elif exit_code == 0 and adapter_status == "ok" and result_nonempty:
        status = "succeeded"
        outcome = "ok"
    elif exit_code == 0 and adapter_status == "ok":
        status = "failed"
        outcome = "result_missing_or_empty"
    else:
        status = "failed"
        outcome = adapter_status or ("adapter_exit" if exit_code else "empty_result")

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
            **adapter,
            "adapter_receipt": {
                "path": relative_path(run_dir, adapter_path),
                "digest": digest(adapter_path),
            },
        },
        "outcome": outcome,
        "failure_code": (
            None if status == "succeeded" else (question["code"] if question is not None else outcome)
        ),
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_seconds": round(time.monotonic() - started, 6),
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
    output_record = {**record, "attempt_digest": attempt_digest}
    print(json.dumps(output_record, sort_keys=True))
    return 0 if status == "succeeded" and not manifest_error else 1


def dispatch(args: argparse.Namespace) -> int:
    """Run one attempt while serialising standalone run-ledger mutation."""
    run_dir = args.run_dir.resolve()
    workspace = Path.cwd().resolve()
    if (
        args.batch_child
        or (run_dir != workspace and workspace not in run_dir.parents)
        or not run_dir.is_dir()
        or not (run_dir / "MANIFEST.md").is_file()
    ):
        return _dispatch(args)
    try:
        custody = acquire_run_custody(run_dir)
    except (OSError, OwnedFileError):
        return fail(run_dir, "run_custody_busy", "another dispatch, batch or finalizer owns the run")
    try:
        return _dispatch(args, custody)
    finally:
        fcntl.flock(custody.fileno(), fcntl.LOCK_UN)
        custody.close()


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--run-dir", type=Path, required=True)
    root.add_argument("--task-id", default="dispatch-001")
    adapter = root.add_mutually_exclusive_group(required=True)
    adapter.add_argument("--adapter", "--tool", dest="tool")
    prompt = root.add_mutually_exclusive_group(required=True)
    prompt.add_argument("--prompt-file", type=Path)
    prompt.add_argument("--prompt-stdin", action="store_true", help="read the prompt bytes from stdin")
    root.add_argument("--intent", choices=("ordinary", "assurance"), default="ordinary")
    root.add_argument("--orchestrator-family")
    selector = root.add_mutually_exclusive_group(required=True)
    selector.add_argument("--alias")
    selector.add_argument("--task-class")
    selector.add_argument("--model")
    root.add_argument("--role", required=True)
    root.add_argument("--risk-tier")
    root.add_argument("--reviewer-id")
    root.add_argument("--effort")
    root.add_argument(
        "--timeout", "--timeout-seconds", dest="timeout_seconds", type=timeout_value,
        default=DEFAULT_TIMEOUT_SECONDS,
        help=f"maximum provider runtime in seconds (default: {DEFAULT_TIMEOUT_SECONDS:g})",
    )
    root.add_argument("--retry-of", help="existing attempt id under this task, for lineage only")
    root.add_argument("--batch-child", action="store_true", help=argparse.SUPPRESS)
    root.add_argument("--batch-id", help=argparse.SUPPRESS)
    return root


if __name__ == "__main__":
    raise SystemExit(dispatch(parser().parse_args()))
