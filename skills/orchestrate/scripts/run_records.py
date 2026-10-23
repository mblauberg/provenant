"""File-backed records and evidence helpers for orchestration runs."""

from __future__ import annotations

import hashlib
import json
import os
import re
import io
import tokenize
import stat
import sys
from contextlib import contextmanager
from pathlib import Path
from typing import Any

SCRIPT_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_ROOT.parents[1]))
sys.path.insert(0, str(SCRIPT_ROOT))
from _shared.custody import (
    OwnedFileError, atomic_write_contained, contained_regular_path,
    ensure_contained_directory, open_contained_regular, read_bound_bytes,
    read_contained_regular,
)
from attempt_evidence import AttemptEvidenceError as SharedAttemptEvidenceError, is_success_status, validate_successful_attempt

TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
ATTEMPT_ID_RE = re.compile(r"^attempt-(?:\d{3}|[1-9]\d{3,})$")

DELIVERY_RUN_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")

MAX_WORKER_QUESTION_PROMPT = 4096

MAX_WORKER_TERMINAL_ENVELOPE_BYTES = 64 * 1024

WORKER_TERMINAL_RECORD_TYPE = "provenant-worker-terminal"

GIT_EVIDENCE_RECORD_TYPE = "provenant-git-evidence"

CANCEL_MARKER_NAME = "cancel.request"

class AttemptEvidenceError(ValueError):
    """A retained attempt cannot be reconciled without inventing evidence."""

class TerminalEnvelopeIntegrityError(ValueError):
    """A bounded terminal candidate changed or became unsafe to reread."""

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

def valid_regular_result(run_dir: Path, path: Path) -> bool:
    try:
        contained_regular_path(run_dir, path.relative_to(run_dir), "retained evidence")
    except (OSError, ValueError, OwnedFileError):
        return False
    return True

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
                    and row.get("status") in {"interrupted", "lost"} and row.get("task_id") == directory.parent.name
                    and directory.name == f"attempt-{row.get('attempt', 0):03d}"):
                recoverable.add(directory)
        except (OSError, OwnedFileError):
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

def prompt_temp_roots() -> tuple[Path, ...]:
    roots = [Path(value).expanduser() for value in
             (os.environ.get("TMPDIR", ""), "/tmp", "/private/tmp", "/private/var/folders") if value]
    return tuple(dict.fromkeys(candidate for root in roots if root.is_absolute() for candidate in (root, root.resolve())))

def run_identity(run_dir, receipt=None):
    return os.environ.get("PROVENANT_RUN_ID") or (receipt or {}).get("run_id") or (run_dir.name if run_dir.name.startswith("mcp-") else "mcp-"+run_dir.name.rsplit("-",1)[-1])

def delivery_run_id(value):
    if not isinstance(value, str) or not DELIVERY_RUN_ID_RE.fullmatch(value):
        raise ValueError("delivery_run_id must match RUN.json run_id: 1-128 letters, numbers, '.', '_', ':' or '-', starting with a letter or number")
    return value
