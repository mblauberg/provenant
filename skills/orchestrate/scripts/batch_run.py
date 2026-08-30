#!/usr/bin/env python3
"""Run a fixed, bounded set of ordinary dispatch attempts.

The batch owner validates a local task manifest, limits fan-out, preserves
partial outcomes, and delegates every attempt to ``dispatch_run.py``. It does
not schedule future work or create a second lifecycle ledger.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "skills"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _shared.bounded_process import stop_process_group
from dispatch_run import (
    ATTEMPT_ID_RE,
    AttemptEvidenceError,
    digest,
    ensure_manifest_appendable,
    ensure_owned_directory,
    reconcile_manifest,
    retained_path,
    cancellation_marker_present,
    remove_cancellation_marker,
)
from _shared.custody import OwnedFileError, contained_regular_path, open_contained_regular, read_bound_bytes, read_contained_regular

DISPATCH_RUN = Path(__file__).with_name("dispatch_run.py")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
MAX_TASKS = 64
MAX_CONCURRENCY = 8
DEFAULT_CONCURRENCY = 4
DEFAULT_TIMEOUT_SECONDS = 900.0
TERMINAL_TASK_STATUSES = {"blocked", "succeeded", "failed", "timed_out", "cancelled"}


class BatchInputError(ValueError):
    """The batch manifest cannot be safely executed."""


_state_lock = threading.Lock()
_cancel_event = threading.Event()
_active_processes: dict[str, subprocess.Popen[str]] = {}


def atomic_write(run_dir: Path, path: Path, content: str | bytes) -> None:
    """Write one run-owned file through a descriptor-relative single inode."""
    fd, _relative, _target = open_contained_regular(
        run_dir, path.relative_to(run_dir), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, label=path.name
    )
    binary = isinstance(content, bytes)
    with os.fdopen(fd, "wb" if binary else "w", encoding=None if binary else "utf-8") as stream:
        stream.write(content)
        stream.flush()
        os.fsync(stream.fileno())


def atomic_write_bytes(run_dir: Path, path: Path, content: bytes) -> None:
    """Compatibility wrapper for descriptor-bound byte writes."""
    atomic_write(run_dir, path, content)


def _digest_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def request_cancel() -> None:
    """Cancel this batch and stop/reap every dispatch currently in flight."""
    _cancel_event.set()
    with _state_lock:
        processes = list(_active_processes.values())
    for process in processes:
        try:
            stop_process_group(process)
        except OSError:
            pass


def _signal_handler(_signum: int, _frame: Any) -> None:
    request_cancel()


def _finite_timeout(value: Any, default: float = DEFAULT_TIMEOUT_SECONDS) -> float:
    if value is None:
        return default
    if isinstance(value, bool):
        raise BatchInputError("timeout must be a finite positive number")
    try:
        timeout = float(value)
    except (TypeError, ValueError) as exc:
        raise BatchInputError("timeout must be a finite positive number") from exc
    if timeout <= 0 or timeout == float("inf") or timeout != timeout:
        raise BatchInputError("timeout must be a finite positive number")
    return timeout


def _local_regular(path: Path, label: str) -> Path:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise BatchInputError(f"{label} is unavailable: {path}") from exc
    if path.is_symlink() or not path.is_file() or metadata.st_nlink != 1:
        raise BatchInputError(f"{label} must be a regular local file: {path}")
    return path.resolve()


def _validate_run_dir(run_dir: Path) -> Path:
    run_dir = run_dir.resolve()
    workspace = Path.cwd().resolve()
    if not run_dir.is_dir() or (run_dir != workspace and workspace not in run_dir.parents):
        raise BatchInputError("run directory must be an existing child of the workspace")
    receipt_path = run_dir / "RUN_RECEIPT.json"
    manifest_path = run_dir / "MANIFEST.md"
    try:
        contained_regular_path(run_dir, "RUN_RECEIPT.json", "RUN_RECEIPT.json")
        contained_regular_path(run_dir, "MANIFEST.md", "MANIFEST.md")
    except OwnedFileError as exc:
        raise BatchInputError(str(exc)) from exc
    try:
        receipt = json.loads(read_bound_bytes(run_dir, "RUN_RECEIPT.json", label="RUN_RECEIPT.json").decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BatchInputError("RUN_RECEIPT.json is not valid JSON") from exc
    if not isinstance(receipt, dict) or receipt.get("schema_version") != 1:
        raise BatchInputError("RUN_RECEIPT.json schema_version must be 1")
    if receipt.get("status") != "active" or receipt.get("closed_at") is not None:
        raise BatchInputError("batch requires an active orchestration run")
    return run_dir


def _load_manifest(path: Path, concurrency: int | None = None) -> tuple[list[dict[str, Any]], Path, bytes]:
    """Load and validate the fixed task list before launching any child."""
    manifest_path = _local_regular(path, "task manifest")
    try:
        source_bytes = manifest_path.read_bytes()
        value = json.loads(source_bytes.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BatchInputError(f"task manifest is not valid UTF-8 JSON: {manifest_path}") from exc
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise BatchInputError("task manifest schema_version must be 1")
    tasks = value.get("tasks")
    if not isinstance(tasks, list) or not tasks or len(tasks) > MAX_TASKS:
        raise BatchInputError(f"task manifest must contain 1-{MAX_TASKS} tasks")

    workspace = Path.cwd().resolve()
    seen: set[str] = set()
    loaded: list[dict[str, Any]] = []
    for index, task in enumerate(tasks):
        if not isinstance(task, dict):
            raise BatchInputError(f"task {index} must be an object")
        task_id = task.get("id")
        if not isinstance(task_id, str) or not TASK_ID_RE.fullmatch(task_id):
            raise BatchInputError(f"task {index} has an invalid id")
        if task_id in seen:
            raise BatchInputError(f"duplicate task id: {task_id}")
        seen.add(task_id)
        prompt_value = task.get("prompt_file")
        inline_prompt = task.get("prompt")
        if (prompt_value is None) == (inline_prompt is None):
            raise BatchInputError(f"task {task_id} requires exactly one of prompt_file or prompt")
        if inline_prompt is not None and not isinstance(inline_prompt, str):
            raise BatchInputError(f"task {task_id} prompt must be a string")
        prompt = None
        if prompt_value is not None:
            if not isinstance(prompt_value, str) or not prompt_value:
                raise BatchInputError(f"task {task_id} prompt_file must be a non-empty string")
            prompt = Path(prompt_value).expanduser()
            if not prompt.is_absolute():
                prompt = workspace / prompt
            prompt = _local_regular(prompt, f"task {task_id} prompt_file")
            try:
                prompt.relative_to(workspace)
            except ValueError as exc:
                raise BatchInputError(f"task {task_id} prompt_file must be inside the workspace") from exc

        adapter = task.get("adapter", task.get("tool"))
        if not isinstance(adapter, str) or not adapter:
            raise BatchInputError(f"task {task_id} requires adapter")
        if task.get("intent", "ordinary") != "ordinary":
            raise BatchInputError(f"task {task_id} must use ordinary intent")
        selectors = [name for name in ("alias", "task_class", "model") if task.get(name)]
        if len(selectors) != 1 or not isinstance(task[selectors[0]], str):
            raise BatchInputError(f"task {task_id} requires exactly one route selector")
        role = task.get("role", "worker")
        if not isinstance(role, str) or not role:
            raise BatchInputError(f"task {task_id} role must be a non-empty string")
        source_writing = task.get("source_writing", False)
        if not isinstance(source_writing, bool):
            raise BatchInputError(f"task {task_id} source_writing must be boolean")
        if task.get("access_mode", "read_only") != "read_only":
            raise BatchInputError(f"task {task_id} supports only read_only access_mode")
        if "non_overlapping" in task or "worktree_isolated" in task:
            raise BatchInputError(f"task {task_id} writer isolation declarations are unsupported")
        if source_writing:
            raise BatchInputError("source_writing tasks are unsupported in this read-only release")
        timeout = _finite_timeout(task.get("timeout"))
        normalized = dict(task)
        normalized.update({"id": task_id, "adapter": adapter, "role": role, "timeout": timeout})
        if prompt is not None:
            normalized["prompt_file"] = str(prompt)
        else:
            normalized["_inline_prompt"] = inline_prompt
        loaded.append(normalized)
    return loaded, manifest_path, source_bytes


def load_manifest(path: Path, concurrency: int | None = None) -> list[dict[str, Any]]:
    """Load and validate a manifest, retaining the historical list API."""
    return _load_manifest(path, concurrency)[0]


def _batch_number(run_dir: Path) -> int:
    root = run_dir / "dispatch" / "batches"
    numbers = []
    for path in root.glob("batch-*"):
        match = re.fullmatch(r"batch-(\d+)", path.name)
        if match and path.is_dir():
            numbers.append(int(match.group(1)))
    return max(numbers, default=0) + 1


def _acquire_batch_lock(run_dir: Path):
    """Hold one custody lock for the complete batch lifetime."""
    dispatch_dir = run_dir / "dispatch"
    try:
        ensure_owned_directory(run_dir, dispatch_dir)
        ensure_owned_directory(run_dir, dispatch_dir / "batches")
        fd, _relative, _target = open_contained_regular(
            run_dir, "MANIFEST.md", os.O_RDWR | os.O_APPEND, label="MANIFEST.md"
        )
        stream = os.fdopen(fd, "a+", encoding="utf-8")
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        return stream
    except (OSError, AttemptEvidenceError, OwnedFileError) as exc:
        if "stream" in locals():
            stream.close()
        raise BatchInputError("another batch already owns this orchestration run") from exc


def _index_batch_files(run_dir: Path, batch_id: str, source_path: Path, summary_path: Path, custody=None) -> None:
    date = time.strftime("%Y-%m-%d", time.gmtime())
    rows = (
        (f"dispatch-{batch_id}-manifest", source_path, "task manifest"),
        (f"dispatch-{batch_id}-summary", summary_path, "batch summary"),
    )
    text = "".join(
        f"| {name} | {path.relative_to(run_dir).as_posix()} | fixed batch {kind} | "
        f"batch_run | {date} | verified | evidence | |\n"
        for name, path, kind in rows
    )
    if custody is None:
        with (run_dir / "MANIFEST.md").open("a", encoding="utf-8") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
    else:
        custody.seek(0, os.SEEK_END)
        custody.write(text)
        custody.flush()
        os.fsync(custody.fileno())


def _command(task: dict[str, Any], run_dir: Path) -> list[str]:
    batch_id = task.get("_batch_id")
    command = [str(DISPATCH_RUN), "--run-dir", str(run_dir), "--task-id", task["id"],
               "--adapter", task["adapter"], "--prompt-file", task["prompt_file"],
               "--role", task["role"], "--intent", task.get("intent", "ordinary"),
               "--timeout", str(task["timeout"]), "--batch-child"]
    if batch_id:
        command.extend(("--batch-id", str(batch_id)))
    selector = next(name for name in ("alias", "task_class", "model") if task.get(name))
    command.extend((f"--{selector.replace('_', '-')}", str(task[selector])))
    for name, flag in (("orchestrator_family", "--orchestrator-family"),
                       ("risk_tier", "--risk-tier"), ("reviewer_id", "--reviewer-id"),
                       ("effort", "--effort"), ("retry_of", "--retry-of")):
        if task.get(name):
            command.extend((flag, str(task[name])))
    return command


def _parse_record(output: str) -> dict[str, Any] | None:
    for line in reversed(output.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
    return None


def _contained_file(run_dir: Path, value: Any, label: str) -> tuple[str, bytes]:
    relative = retained_path(run_dir, value)
    try:
        actual, _path, data = read_contained_regular(run_dir, relative, label=f"child {label}")
    except (OSError, OwnedFileError, ValueError) as exc:
        raise BatchInputError(f"child {label} path is outside or unavailable: {value}") from exc
    return actual, data


def _validate_child_record(task: dict[str, Any], record: dict[str, Any], run_dir: Path,
                           process_exit: int | None) -> dict[str, Any]:
    task_id = task["id"]
    if (
        record.get("schema_version") != 1
        or record.get("record_type") != "dispatch-attempt"
        or record.get("task_id") != task_id
    ):
        raise BatchInputError(f"child receipt task_id does not match {task_id}")
    attempt = None
    attempt_path = result_path = None
    if record.get("attempt_path") is not None:
        attempt_path, attempt_bytes = _contained_file(run_dir, record["attempt_path"], "attempt")
        try:
            attempt = json.loads(attempt_bytes.decode("utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise BatchInputError(f"child attempt receipt is not valid JSON: {attempt_path}") from exc
        if (
            not isinstance(attempt, dict)
            or attempt.get("schema_version") != 1
            or attempt.get("record_type") != "dispatch-attempt"
            or attempt.get("task_id") != task_id
        ):
            raise BatchInputError(f"child attempt receipt task_id does not match {task_id}")
        attempt_id = attempt.get("attempt_id")
        attempt_root = Path("dispatch") / "tasks" / task_id / str(attempt_id)
        expected_attempt_path = (attempt_root / "attempt.json").as_posix()
        if (
            not isinstance(attempt_id, str)
            or not ATTEMPT_ID_RE.fullmatch(attempt_id)
            or attempt_path != expected_attempt_path
            or record.get("attempt_id") != attempt_id
        ):
            raise BatchInputError(f"child attempt identity does not match retained path: {task_id}")
        if attempt.get("attempt_path") != attempt_path:
            raise BatchInputError(f"child attempt path does not match its receipt: {attempt_path}")
        if record.get("attempt_digest") != "sha256:" + hashlib.sha256(attempt_bytes).hexdigest():
            raise BatchInputError(f"child attempt digest does not match: {task_id}")
        status = attempt.get("status") if isinstance(attempt.get("status"), str) else "failed"
        if status not in TERMINAL_TASK_STATUSES:
            raise BatchInputError(f"child status is unsupported: {task_id}")
        if isinstance(record.get("status"), str) and record["status"] != status:
            raise BatchInputError(f"child status does not match retained attempt: {task_id}")
        outcome = attempt.get("outcome", status)
        if "outcome" in record and record["outcome"] != outcome:
            raise BatchInputError(f"child outcome does not match retained attempt: {task_id}")
        if "question" in record and record["question"] != attempt.get("question"):
            raise BatchInputError(f"child question does not match retained attempt: {task_id}")
        route = attempt.get("route")
        requested = attempt.get("requested_route")
        if not isinstance(requested, dict) or not isinstance(requested.get("role"), str):
            raise BatchInputError(f"child requested route is incomplete: {task_id}")
        selector = next(name for name in ("alias", "task_class", "model") if task.get(name))
        if requested.get("adapter") != task["adapter"] or requested.get("role") != task["role"]:
            raise BatchInputError(f"child requested route does not match task: {task_id}")
        if requested.get(selector) != task[selector]:
            raise BatchInputError(f"child requested selector does not match task: {task_id}")
        if requested.get("intent") != "ordinary":
            raise BatchInputError(f"child requested intent does not match task: {task_id}")
        if requested.get("orchestrator_family", "") != task.get("orchestrator_family", ""):
            raise BatchInputError(f"child requested provider family does not match task: {task_id}")
        route = route if isinstance(route, dict) else {}
        if status == "succeeded" and any(not isinstance(route.get(field), str) or not route[field]
                                         for field in ("adapter", "provider_family", "resolved_model", "execution_intent")):
            raise BatchInputError(f"successful child route identity is incomplete: {task_id}")
        if status == "succeeded" and route.get("execution_intent") != "ordinary":
            raise BatchInputError(f"successful child intent is not ordinary: {task_id}")
        if status == "succeeded" and route.get("adapter") != task["adapter"]:
            raise BatchInputError(f"successful child adapter does not match task: {task_id}")
        adapter_receipt = route.get("adapter_receipt")
        if status == "succeeded" and not isinstance(adapter_receipt, dict):
            raise BatchInputError(f"successful child adapter receipt is missing: {task_id}")
        if adapter_receipt is not None:
            if not isinstance(adapter_receipt, dict):
                raise BatchInputError(f"child adapter receipt is malformed: {task_id}")
            adapter_path, adapter_bytes = _contained_file(
                run_dir, adapter_receipt.get("path"), "adapter receipt"
            )
            if adapter_path != (attempt_root / "adapter-receipt.json").as_posix():
                raise BatchInputError(f"child adapter receipt path does not match attempt: {task_id}")
            if adapter_receipt.get("digest") != "sha256:" + hashlib.sha256(adapter_bytes).hexdigest():
                raise BatchInputError(f"child adapter receipt digest does not match: {task_id}")
        if status == "succeeded" and process_exit != 0:
            raise BatchInputError(f"succeeded child exited non-zero: {task_id}")
        retained_result = attempt.get("result")
        if retained_result is not None:
            if not isinstance(retained_result, dict):
                raise BatchInputError(f"child result receipt is malformed: {task_id}")
            result_path, result_bytes = _contained_file(run_dir, retained_result.get("path"), "result")
            if result_path != (attempt_root / "result.md").as_posix():
                raise BatchInputError(f"child result path does not match attempt: {task_id}")
            expected_digest = retained_result.get("digest")
            if not isinstance(expected_digest, str) or expected_digest != "sha256:" + hashlib.sha256(result_bytes).hexdigest():
                raise BatchInputError(f"child result digest does not match: {task_id}")
            if status == "succeeded" and not result_bytes:
                raise BatchInputError(f"successful child result is empty: {task_id}")
            child_result = record.get("result")
            if (
                not isinstance(child_result, dict)
                or child_result.get("path") != result_path
                or child_result.get("digest") != expected_digest
            ):
                raise BatchInputError(f"child result path does not match retained attempt: {task_id}")
        elif record.get("result") is not None:
            raise BatchInputError(f"child result receipt does not match retained attempt: {task_id}")
        return {
            "task_id": task_id, "status": status, "outcome": outcome,
            "dispatch_exit": process_exit, "attempt_path": attempt_path,
            "attempt_digest": record.get("attempt_digest"), "result_path": result_path,
            "requested_route": requested, "route": {
                field: route[field] for field in ("adapter", "provider_family", "resolved_model", "execution_intent")
                if isinstance(route.get(field), str)
            }, "question": attempt.get("question", record.get("question")),
        }
    status = record.get("status") if isinstance(record.get("status"), str) else "failed"
    if status not in TERMINAL_TASK_STATUSES:
        raise BatchInputError(f"child status is unsupported: {task_id}")
    if status == "succeeded":
        raise BatchInputError(f"successful child has no retained attempt: {task_id}")
    compact = {"task_id": task_id, "status": status, "outcome": record.get("outcome", status),
               "dispatch_exit": process_exit}
    if "question" in record:
        compact["question"] = record["question"]
    return compact


def _run_task(task: dict[str, Any], run_dir: Path, batch_dir: Path) -> dict[str, Any]:
    task_id = task["id"]
    if _cancel_event.is_set() or cancellation_marker_present(run_dir, batch_dir):
        return {"task_id": task_id, "status": "cancelled", "outcome": "batch_cancelled"}
    temporary_prompt: Path | None = None
    dispatch_task = {**task, "_batch_id": batch_dir.name}
    if "_inline_prompt" in task:
        temporary_prompt = batch_dir / "prompts" / f"{task_id}.md"
        ensure_owned_directory(run_dir, temporary_prompt.parent)
        atomic_write(run_dir, temporary_prompt, task["_inline_prompt"])
        dispatch_task = {**task, "prompt_file": str(temporary_prompt), "_batch_id": batch_dir.name}
    process: subprocess.Popen[str] | None = None
    started = time.monotonic()
    timed_out = False
    try:
        process = subprocess.Popen(_command(dispatch_task, run_dir), cwd=Path.cwd(), text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True)
        with _state_lock:
            _active_processes[task_id] = process
        if _cancel_event.is_set():
            stop_process_group(process)
        try:
            stdout, stderr = process.communicate(timeout=task["timeout"] + 5.0)
        except subprocess.TimeoutExpired:
            stop_process_group(process)
            stdout, stderr = process.communicate()
            timed_out = True
    except (OSError, subprocess.SubprocessError) as exc:
        return {"task_id": task_id, "status": "failed", "outcome": "dispatch_spawn_error",
                "message": str(exc)}
    finally:
        if process is not None:
            with _state_lock:
                _active_processes.pop(task_id, None)
        if temporary_prompt is not None:
            temporary_prompt.unlink(missing_ok=True)
    record = _parse_record(stdout)
    if record is None:
        if _cancel_event.is_set():
            return {"task_id": task_id, "status": "cancelled", "outcome": "batch_cancelled",
                    "dispatch_exit": process.returncode, "stderr": stderr[-1000:]}
        if timed_out:
            return {"task_id": task_id, "status": "timed_out", "outcome": "batch_timeout",
                    "dispatch_exit": process.returncode, "stderr": stderr[-1000:]}
        return {"task_id": task_id, "status": "failed", "outcome": "dispatch_output_invalid",
                "dispatch_exit": process.returncode, "stderr": stderr[-1000:]}
    try:
        compact = _validate_child_record(task, record, run_dir, process.returncode)
    except BatchInputError as exc:
        return {"task_id": task_id, "status": "failed", "outcome": "child_receipt_invalid",
                "dispatch_exit": process.returncode, "message": str(exc),
                "receipt_invalid": True}
    compact["duration_seconds"] = round(time.monotonic() - started, 6)
    return compact


def _execute_batch(args: argparse.Namespace, tasks: list[dict[str, Any]], run_dir: Path,
                   source_bytes: bytes, custody=None) -> int:
    batch_id = f"batch-{_batch_number(run_dir):03d}"
    batch_dir = run_dir / "dispatch" / "batches" / batch_id
    try:
        ensure_owned_directory(run_dir, batch_dir)
        source_copy = batch_dir / "task-manifest.json"
        atomic_write_bytes(run_dir, source_copy, source_bytes)
    except BaseException:
        if batch_dir.is_dir() and not batch_dir.is_symlink():
            shutil.rmtree(batch_dir)
        raise
    source_digest = _digest_bytes(source_bytes)
    results: dict[str, dict[str, Any]] = {}
    with ThreadPoolExecutor(max_workers=args.concurrency, thread_name_prefix="provenant-batch") as pool:
        futures = {pool.submit(_run_task, task, run_dir, batch_dir): task["id"] for task in tasks}
        for future in as_completed(futures):
            task_id = futures[future]
            try:
                results[task_id] = future.result()
            except Exception as exc:  # preserve partial batch visibility
                results[task_id] = {"task_id": task_id, "status": "failed",
                                    "outcome": "batch_worker_error", "message": str(exc)}
    ordered = [results.get(task["id"], {"task_id": task["id"], "status": "cancelled",
                                         "outcome": "batch_cancelled"}) for task in tasks]
    reconciliation_error = None
    try:
        reconcile_manifest(run_dir, custody)
    except (AttemptEvidenceError, OSError) as exc:
        reconciliation_error = str(exc)
    batch_cancelled = _cancel_event.is_set() or any(item["status"] == "cancelled" for item in ordered)
    status = "cancelled" if batch_cancelled else ("failed" if reconciliation_error else "completed")
    counts = dict(sorted(Counter(item["status"] for item in ordered).items()))
    summary_path = batch_dir / "summary.json"
    summary = {
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": batch_id,
        "status": status, "task_count": len(ordered), "concurrency": args.concurrency,
        "counts": counts, "source_manifest": {"path": str(source_copy.relative_to(run_dir)),
                                                "digest": source_digest},
        "tasks": ordered,
        "reducer_inputs": [
            {"task_id": item["task_id"], "status": item["status"],
             "attempt_path": item.get("attempt_path"), "result_path": item.get("result_path")}
            for item in ordered if item.get("attempt_path") or item.get("result_path")
        ],
    }
    if reconciliation_error:
        summary["reconciliation_error"] = reconciliation_error
    atomic_write(run_dir, summary_path, json.dumps(summary, indent=2, sort_keys=True) + "\n")
    remove_cancellation_marker(run_dir, batch_dir)
    index_error = None
    try:
        _index_batch_files(run_dir, batch_id, source_copy, summary_path, custody)
    except OSError as exc:
        index_error = str(exc)
    output = {**summary, "summary_path": str(summary_path.relative_to(run_dir))}
    if index_error:
        output["manifest_index_error"] = index_error
    print(json.dumps(output, sort_keys=True))
    return 1 if (status != "completed" or index_error or any(item["status"] != "succeeded" for item in ordered)) else 0


def batch(args: argparse.Namespace) -> int:
    try:
        tasks, _source_manifest, source_bytes = _load_manifest(args.manifest, args.concurrency)
        if not 1 <= args.concurrency <= MAX_CONCURRENCY:
            raise BatchInputError(f"concurrency must be between 1 and {MAX_CONCURRENCY}")
        args.concurrency = min(args.concurrency, len(tasks))
        run_dir = _validate_run_dir(args.run_dir)
        if not DISPATCH_RUN.is_file() or not os.access(DISPATCH_RUN, os.X_OK):
            raise BatchInputError(f"dispatch owner is unavailable: {DISPATCH_RUN}")
    except BatchInputError as exc:
        print(json.dumps({"schema_version": 1, "status": "invalid_manifest", "message": str(exc)}, sort_keys=True))
        return 2

    try:
        batch_lock = _acquire_batch_lock(run_dir)
    except BatchInputError as exc:
        print(json.dumps({"schema_version": 1, "status": "batch_busy", "message": str(exc)}, sort_keys=True))
        return 2
    _cancel_event.clear()
    old_handlers = {}
    if threading.current_thread() is threading.main_thread():
        old_handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
        for sig in old_handlers:
            signal.signal(sig, _signal_handler)
    try:
        try:
            run_dir = _validate_run_dir(args.run_dir)
            reconcile_manifest(run_dir, batch_lock)
            # The batch lock is the append handle; do not reopen MANIFEST.md.
        except (BatchInputError, AttemptEvidenceError, OSError) as exc:
            print(json.dumps({"schema_version": 1, "status": "custody_preflight_failed",
                              "message": str(exc)}, sort_keys=True))
            return 2
        return _execute_batch(args, tasks, run_dir, source_bytes, batch_lock)
    finally:
        if old_handlers:
            for sig, handler in old_handlers.items():
                signal.signal(sig, handler)
        fcntl.flock(batch_lock.fileno(), fcntl.LOCK_UN)
        batch_lock.close()


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""Manifest schema v1: {\"schema_version\":1,\"tasks\":[{\"id\":\"task-1\",\"prompt_file\":\"prompt.md\",\"adapter\":\"codex\",\"alias\":\"scout\",\"role\":\"worker\"}]}.
Each task uses ordinary intent and exactly one of prompt_file/prompt plus one
route selector (alias, task_class or model). Read-only tasks may run together;
source_writing is rejected in this read-only release; partitioned/worktree-isolated
writers are deferred.""",
    )
    root.add_argument("--run-dir", type=Path, required=True)
    root.add_argument("--manifest", type=Path, required=True)
    root.add_argument("--concurrency", type=int, default=DEFAULT_CONCURRENCY)
    return root


if __name__ == "__main__":
    raise SystemExit(batch(parser().parse_args()))
