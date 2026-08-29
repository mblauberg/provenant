#!/usr/bin/env python3
"""Run one configured provider attempt and retain compact execution evidence.

This is deliberately a one-attempt owner.  Provider command construction stays
with ``cf_dispatch.sh``; this module owns only the regular-file boundary,
process wait and attempt custody. Provider execution is bounded to 900 seconds
by default; callers may provide a smaller or larger finite positive timeout.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "skills"))
CF_DISPATCH = Path(__file__).with_name("cf_dispatch.sh")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
DEFAULT_TIMEOUT_SECONDS = 900.0

from _shared.bounded_process import stop_process_group


class AttemptEvidenceError(ValueError):
    """A retained attempt cannot be reconciled without inventing evidence."""


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


def atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary_path = Path(temporary)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_path, path)
    finally:
        temporary_path.unlink(missing_ok=True)


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


def append_manifest(run_dir: Path, record: dict[str, Any]) -> None:
    manifest = run_dir / "MANIFEST.md"
    date = record["finished_at"][:10]
    prefix = f"dispatch-{record['task_id']}-{record['attempt_id']}"
    paths = manifest_rows(run_dir, record)
    rows_text = "".join(
        f"| {prefix}-{kind} | {path} | single dispatch {kind} | dispatch_run | "
        f"{date} | verified | evidence | |\n"
        for kind, path in paths
    )
    with manifest.open("a", encoding="utf-8") as stream:
        stream.write(rows_text)


def ensure_manifest_appendable(run_dir: Path) -> None:
    """Check append access without changing the manifest."""
    with (run_dir / "MANIFEST.md").open("a", encoding="utf-8"):
        pass


def reconcile_manifest(run_dir: Path) -> None:
    """Index complete prior attempts whose manifest rows were lost on re-entry."""
    manifest = run_dir / "MANIFEST.md"
    existing = manifest.read_text(encoding="utf-8", errors="replace")
    for attempt_path in sorted((run_dir / "dispatch" / "tasks").glob("*/attempt-*/attempt.json")):
        try:
            discovered_attempt_path = relative_path(run_dir, attempt_path)
        except ValueError as exc:
            raise AttemptEvidenceError(f"attempt record path escapes the run: {attempt_path}") from exc
        try:
            record = json.loads(attempt_path.read_text(encoding="utf-8"))
        except (OSError, TypeError, ValueError) as exc:
            raise AttemptEvidenceError(f"attempt record is unreadable: {attempt_path}") from exc
        if not isinstance(record, dict) or record.get("record_type") != "dispatch-attempt":
            raise AttemptEvidenceError(f"attempt record has an invalid type: {attempt_path}")
        try:
            if retained_path(run_dir, record["attempt_path"]) != discovered_attempt_path:
                raise AttemptEvidenceError(
                    f"attempt record path does not match its retained file: {attempt_path}"
                )
            sidecar = attempt_path.with_name("attempt.sha256")
            if not sidecar.is_file():
                atomic_write(sidecar, f"{digest(attempt_path)}  {attempt_path.name}\n")
            record["attempt_digest_path"] = relative_path(run_dir, sidecar)
            rows = [
                (kind, retained_path(run_dir, path))
                for kind, path in manifest_rows(run_dir, record)
            ]
            absent = [path for _, path in rows if not (run_dir / path).is_file()]
            if absent:
                raise AttemptEvidenceError(
                    f"attempt evidence is missing for {attempt_path}: {', '.join(absent)}"
                )
            missing = [(kind, path) for kind, path in rows if f"| {path} |" not in existing]
            if not missing:
                continue
            date = record["finished_at"][:10]
            prefix = f"dispatch-{record['task_id']}-{record['attempt_id']}"
            with manifest.open("a", encoding="utf-8") as stream:
                stream.writelines(
                    f"| {prefix}-{kind} | {path} | single dispatch {kind} | dispatch_run | "
                    f"{date} | verified | evidence | |\n"
                    for kind, path in missing
                )
            existing = manifest.read_text(encoding="utf-8", errors="replace")
        except AttemptEvidenceError:
            raise
        except (KeyError, TypeError, ValueError) as exc:
            raise AttemptEvidenceError(f"attempt record is malformed: {attempt_path}") from exc


def existing_attempt_number(task_dir: Path) -> int:
    numbers = []
    for candidate in task_dir.glob("attempt-*"):
        match = re.fullmatch(r"attempt-(\d{3})", candidate.name)
        if match and candidate.is_dir():
            numbers.append(int(match.group(1)))
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


def dispatch(args: argparse.Namespace) -> int:
    run_dir = args.run_dir.resolve()
    workspace = Path.cwd().resolve()
    if run_dir != workspace and workspace not in run_dir.parents:
        return fail(run_dir, "run_dir_invalid", "run directory must be inside the current workspace")
    if not run_dir.is_dir():
        return fail(run_dir, "run_custody_missing", f"run directory does not exist: {run_dir}")
    missing = [name for name in ("MANIFEST.md", "RUN_RECEIPT.json") if not (run_dir / name).is_file()]
    if missing:
        return fail(run_dir, "run_custody_missing", f"missing custody files: {', '.join(missing)}")
    try:
        run_receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return fail(run_dir, "run_custody_invalid", "RUN_RECEIPT.json is not valid JSON")
    if (
        not isinstance(run_receipt, dict)
        or run_receipt.get("status") != "active"
        or run_receipt.get("closed_at") is not None
    ):
        return fail(run_dir, "run_custody_closed", "dispatch requires an active orchestration run")
    if not TASK_ID_RE.fullmatch(args.task_id):
        return fail(run_dir, "invalid_task_id", "task id must contain only letters, numbers, '.', '_' or '-'")
    try:
        reconcile_manifest(run_dir)
        ensure_manifest_appendable(run_dir)
    except AttemptEvidenceError as exc:
        return fail(run_dir, "attempt_evidence_incomplete", str(exc))
    except OSError as exc:
        return fail(run_dir, "manifest_not_appendable", f"MANIFEST.md is not appendable: {exc}")

    prompt_source = args.prompt_file.resolve()
    if not prompt_source.is_file() or not os.access(prompt_source, os.R_OK):
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
    if prompt_source.stat().st_nlink > 1:
        return fail(run_dir, "prompt_hard_link_denied", "prompt file has multiple hard links")
    if not CF_DISPATCH.is_file() or not os.access(CF_DISPATCH, os.X_OK):
        return fail(run_dir, "adapter_unavailable", f"provider adapter is missing or not executable: {CF_DISPATCH}")

    task_dir = run_dir / "dispatch" / "tasks" / args.task_id
    retry_of = None
    if args.retry_of:
        retry_ref = Path(args.retry_of)
        if retry_ref.is_absolute() or retry_ref.name != args.retry_of or not re.fullmatch(r"attempt-\d{3}", args.retry_of):
            return fail(run_dir, "retry_of_invalid", "retry-of must name an attempt under the same task")
        retry_dir = task_dir / args.retry_of
        if not (retry_dir.is_dir() and (retry_dir / "attempt.json").is_file()):
            return fail(run_dir, "retry_of_missing", f"retry attempt does not exist: {args.retry_of}")
        retry_of = args.retry_of
    attempt_number = existing_attempt_number(task_dir)
    attempt_id = f"attempt-{attempt_number:03d}"
    attempt_dir = task_dir / attempt_id
    attempt_dir.mkdir(parents=True)
    prompt_path = attempt_dir / "prompt.md"
    result_path = attempt_dir / "result.md"
    adapter_path = attempt_dir / "adapter-receipt.json"
    stderr_path = attempt_dir / "stderr.log"
    shutil.copyfile(prompt_source, prompt_path)
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
    }
    started_at = now()
    started = time.monotonic()
    observed_exit = False
    exit_code: int | None = None
    process_error = ""
    process = None
    cancelled = False
    try:
        with adapter_path.open("w", encoding="utf-8") as adapter_stream, stderr_path.open(
            "w", encoding="utf-8"
        ) as stderr_stream:
            process = subprocess.Popen(
                command,
                cwd=workspace,
                stdout=adapter_stream,
                stderr=stderr_stream,
                env=os.environ.copy(),
                start_new_session=True,
            )
            def cancel_handler(_signum: int, _frame: Any) -> None:
                nonlocal cancelled
                cancelled = True
                try:
                    stop_process_group(process)
                except OSError:
                    pass

            old_handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGHUP)}
            signal.signal(signal.SIGTERM, cancel_handler)
            signal.signal(signal.SIGHUP, cancel_handler)
            try:
                try:
                    exit_code = process.wait(timeout=args.timeout_seconds)
                except subprocess.TimeoutExpired:
                    stop_process_group(process)
                    exit_code = process.wait()
                    process_error = "timeout"
                except KeyboardInterrupt:
                    cancelled = True
                    stop_process_group(process)
                    exit_code = process.wait()
            finally:
                for sig, handler in old_handlers.items():
                    signal.signal(sig, handler)
            if cancelled:
                process_error = "cancelled"
            observed_exit = True
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

    result_exists = result_path.is_file()
    result_nonempty = result_exists and result_path.stat().st_size > 0
    adapter_status = str(adapter.get("status", "")) if adapter else "adapter_receipt_invalid"
    if process_error == "timeout":
        status = "timed_out"
        outcome = "timeout"
    elif process_error == "cancelled":
        status = "cancelled"
        outcome = "cancelled"
    elif not observed_exit:
        status = "failed"
        outcome = "process_spawn_error"
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
        "failure_code": None if status == "succeeded" else outcome,
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_seconds": round(time.monotonic() - started, 6),
        "workspace": {"cwd": str(workspace), "root": str(workspace)},
        "prompt": {"path": relative_path(run_dir, prompt_path), "digest": digest(prompt_path)},
        "result": (
            {"path": relative_path(run_dir, result_path), "digest": digest(result_path)}
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
    if process_error:
        record["process_error"] = process_error
    attempt_path = attempt_dir / "attempt.json"
    record["attempt_path"] = relative_path(run_dir, attempt_path)
    digest_path = attempt_dir / "attempt.sha256"
    record["attempt_digest_path"] = relative_path(run_dir, digest_path)
    atomic_write(attempt_path, json.dumps(record, indent=2, sort_keys=True) + "\n")
    attempt_digest = digest(attempt_path)
    atomic_write(digest_path, f"{attempt_digest}  {attempt_path.name}\n")
    manifest_error = False
    try:
        append_manifest(run_dir, record)
    except OSError as exc:
        manifest_error = True
        record["status"] = "failed"
        record["outcome"] = "manifest_write_error"
        record["failure_code"] = "manifest_write_error"
        record["manifest_error"] = str(exc)
        atomic_write(attempt_path, json.dumps(record, indent=2, sort_keys=True) + "\n")
        attempt_digest = digest(attempt_path)
        atomic_write(digest_path, f"{attempt_digest}  {attempt_path.name}\n")
    output_record = {**record, "attempt_digest": attempt_digest}
    print(json.dumps(output_record, sort_keys=True))
    return 0 if status == "succeeded" and not manifest_error else 1


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--run-dir", type=Path, required=True)
    root.add_argument("--task-id", default="dispatch-001")
    adapter = root.add_mutually_exclusive_group(required=True)
    adapter.add_argument("--adapter", "--tool", dest="tool")
    root.add_argument("--prompt-file", type=Path, required=True)
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
    return root


if __name__ == "__main__":
    raise SystemExit(dispatch(parser().parse_args()))
