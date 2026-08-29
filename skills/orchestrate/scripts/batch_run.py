#!/usr/bin/env python3
"""Run a fixed, bounded set of ordinary dispatch attempts.

The batch owner validates a local task manifest, limits fan-out, preserves
partial outcomes, and delegates every attempt to ``dispatch_run.py``. It does
not schedule future work or create a second lifecycle ledger.
"""

from __future__ import annotations

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
import fcntl
import json
import os
from pathlib import Path
import re
import signal
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "skills"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _shared.bounded_process import stop_process_group
from dispatch_run import AttemptEvidenceError, digest, ensure_owned_directory, reconcile_manifest, retained_path


DISPATCH_RUN = Path(__file__).with_name("dispatch_run.py")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
MAX_TASKS = 64
MAX_CONCURRENCY = 8
DEFAULT_CONCURRENCY = 4
DEFAULT_TIMEOUT_SECONDS = 900.0


class BatchInputError(ValueError):
    """The batch manifest cannot be safely executed."""


_state_lock = threading.Lock()
_cancel_event = threading.Event()
_active_processes: dict[str, subprocess.Popen[str]] = {}


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
    if not receipt_path.is_file() or not manifest_path.is_file():
        raise BatchInputError("run directory is missing RUN_RECEIPT.json or MANIFEST.md")
    try:
        receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BatchInputError("RUN_RECEIPT.json is not valid JSON") from exc
    if not isinstance(receipt, dict) or receipt.get("schema_version") != 1:
        raise BatchInputError("RUN_RECEIPT.json schema_version must be 1")
    if receipt.get("status") != "active" or receipt.get("closed_at") is not None:
        raise BatchInputError("batch requires an active orchestration run")
    return run_dir


def load_manifest(path: Path, concurrency: int | None = None) -> list[dict[str, Any]]:
    """Load and validate the fixed task list before launching any child."""
    manifest_path = _local_regular(path, "task manifest")
    try:
        value = json.loads(manifest_path.read_text(encoding="utf-8"))
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
        if source_writing and concurrency != 1:
            raise BatchInputError("source_writing tasks require serialized concurrency=1")
        timeout = _finite_timeout(task.get("timeout"))
        normalized = dict(task)
        normalized.update({"id": task_id, "adapter": adapter, "role": role, "timeout": timeout})
        if prompt is not None:
            normalized["prompt_file"] = str(prompt)
        else:
            normalized["_inline_prompt"] = inline_prompt
        loaded.append(normalized)
    return loaded


def _batch_number(run_dir: Path) -> int:
    root = run_dir / "dispatch" / "batches"
    numbers = []
    for path in root.glob("batch-*"):
        match = re.fullmatch(r"batch-(\d{3})", path.name)
        if match and path.is_dir():
            numbers.append(int(match.group(1)))
    return max(numbers, default=0) + 1


def _acquire_batch_lock(run_dir: Path):
    """Hold one custody lock for the complete batch lifetime."""
    dispatch_dir = run_dir / "dispatch"
    try:
        ensure_owned_directory(run_dir, dispatch_dir)
        ensure_owned_directory(run_dir, dispatch_dir / "batches")
        lock_path = dispatch_dir / ".batch.lock"
        flags = os.O_RDWR | os.O_CREAT | os.O_APPEND | getattr(os, "O_NOFOLLOW", 0)
        stream = os.fdopen(os.open(lock_path, flags, 0o600), "a+", encoding="utf-8")
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        return stream
    except (OSError, AttemptEvidenceError) as exc:
        if "stream" in locals():
            stream.close()
        raise BatchInputError("another batch already owns this orchestration run") from exc


def _index_batch_files(run_dir: Path, batch_id: str, source_path: Path, summary_path: Path) -> None:
    date = time.strftime("%Y-%m-%d", time.gmtime())
    rows = (
        (f"dispatch-{batch_id}-manifest", source_path, "task manifest"),
        (f"dispatch-{batch_id}-summary", summary_path, "batch summary"),
    )
    with (run_dir / "MANIFEST.md").open("a", encoding="utf-8") as stream:
        for name, path, kind in rows:
            stream.write(
                f"| {name} | {path.relative_to(run_dir).as_posix()} | fixed batch {kind} | "
                f"batch_run | {date} | verified | evidence | |\n"
            )


def _command(task: dict[str, Any], run_dir: Path) -> list[str]:
    command = [str(DISPATCH_RUN), "--run-dir", str(run_dir), "--task-id", task["id"],
               "--adapter", task["adapter"], "--prompt-file", task["prompt_file"],
               "--role", task["role"], "--intent", task.get("intent", "ordinary"),
               "--timeout", str(task["timeout"]), "--batch-child"]
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


def _contained_file(run_dir: Path, value: Any, label: str) -> tuple[str, Path]:
    relative = retained_path(run_dir, value)
    path = run_dir / relative
    try:
        metadata = path.lstat()
        path.resolve().relative_to(run_dir.resolve())
    except (OSError, ValueError) as exc:
        raise BatchInputError(f"child {label} path is outside or unavailable: {value}") from exc
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise BatchInputError(f"child {label} path is not a regular file: {value}")
    return relative, path


def _validate_child_record(task: dict[str, Any], record: dict[str, Any], run_dir: Path,
                           process_exit: int | None) -> dict[str, Any]:
    task_id = task["id"]
    if record.get("task_id") != task_id:
        raise BatchInputError(f"child receipt task_id does not match {task_id}")
    status = record.get("status") if isinstance(record.get("status"), str) else "failed"
    attempt = None
    attempt_path = result_path = None
    if record.get("attempt_path") is not None:
        attempt_path, attempt_file = _contained_file(run_dir, record["attempt_path"], "attempt")
        try:
            attempt = json.loads(attempt_file.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise BatchInputError(f"child attempt receipt is not valid JSON: {attempt_path}") from exc
        if not isinstance(attempt, dict) or attempt.get("task_id") != task_id:
            raise BatchInputError(f"child attempt receipt task_id does not match {task_id}")
        if attempt.get("attempt_path") != attempt_path:
            raise BatchInputError(f"child attempt path does not match its receipt: {attempt_path}")
        if record.get("attempt_digest") != digest(attempt_file):
            raise BatchInputError(f"child attempt digest does not match: {task_id}")
        if attempt.get("status") != status:
            raise BatchInputError(f"child status does not match retained attempt: {task_id}")
        route = attempt.get("route")
        requested = attempt.get("requested_route")
        required_route = ("adapter", "provider_family", "resolved_model", "execution_intent")
        if not isinstance(route, dict) or any(not isinstance(route.get(field), str) or not route[field]
                                               for field in required_route):
            raise BatchInputError(f"child route identity is incomplete: {task_id}")
        adapter_receipt = route.get("adapter_receipt")
        if not isinstance(adapter_receipt, dict):
            raise BatchInputError(f"child adapter receipt is missing: {task_id}")
        _, adapter_file = _contained_file(run_dir, adapter_receipt.get("path"), "adapter receipt")
        if adapter_receipt.get("digest") != digest(adapter_file):
            raise BatchInputError(f"child adapter receipt digest does not match: {task_id}")
        if not isinstance(requested, dict) or not isinstance(requested.get("role"), str):
            raise BatchInputError(f"child requested route is incomplete: {task_id}")
        if process_exit != 0 and status == "succeeded":
            raise BatchInputError(f"succeeded child exited non-zero: {task_id}")
        retained_result = attempt.get("result")
        if retained_result is not None:
            if not isinstance(retained_result, dict):
                raise BatchInputError(f"child result receipt is malformed: {task_id}")
            result_path, result_file = _contained_file(run_dir, retained_result.get("path"), "result")
            expected_digest = retained_result.get("digest")
            if not isinstance(expected_digest, str) or expected_digest != digest(result_file):
                raise BatchInputError(f"child result digest does not match: {task_id}")
            child_result = record.get("result")
            if not isinstance(child_result, dict) or child_result.get("path") != result_path:
                raise BatchInputError(f"child result path does not match retained attempt: {task_id}")
        elif record.get("result") is not None:
            raise BatchInputError(f"child result receipt does not match retained attempt: {task_id}")
        return {
            "task_id": task_id, "status": status, "outcome": record.get("outcome", status),
            "dispatch_exit": process_exit, "attempt_path": attempt_path,
            "attempt_digest": record.get("attempt_digest"), "result_path": result_path,
            "requested_route": requested, "route": {
                field: route[field] for field in required_route
            }, "question": record.get("question"),
        }
    if status == "succeeded":
        raise BatchInputError(f"successful child has no retained attempt: {task_id}")
    compact = {"task_id": task_id, "status": status, "outcome": record.get("outcome", status),
               "dispatch_exit": process_exit}
    if "question" in record:
        compact["question"] = record["question"]
    return compact


def _run_task(task: dict[str, Any], run_dir: Path, batch_dir: Path) -> dict[str, Any]:
    task_id = task["id"]
    if _cancel_event.is_set():
        return {"task_id": task_id, "status": "cancelled", "outcome": "batch_cancelled"}
    temporary_prompt: Path | None = None
    dispatch_task = task
    if "_inline_prompt" in task:
        temporary_prompt = batch_dir / "prompts" / f"{task_id}.md"
        atomic_write(temporary_prompt, task["_inline_prompt"])
        dispatch_task = {**task, "prompt_file": str(temporary_prompt)}
    process: subprocess.Popen[str] | None = None
    started = time.monotonic()
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
            return {"task_id": task_id, "status": "timed_out", "outcome": "batch_timeout",
                    "dispatch_exit": process.returncode, "stderr": stderr[-1000:]}
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


def _execute_batch(args: argparse.Namespace, tasks: list[dict[str, Any]], run_dir: Path) -> int:
    batch_id = f"batch-{_batch_number(run_dir):03d}"
    batch_dir = run_dir / "dispatch" / "batches" / batch_id
    batch_dir.mkdir()
    source_copy = batch_dir / "task-manifest.json"
    shutil.copyfile(Path(args.manifest).resolve(), source_copy)
    source_digest = digest(source_copy)
    old_handlers = {}
    if threading.current_thread() is threading.main_thread():
        old_handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
        for sig in old_handlers:
            signal.signal(sig, _signal_handler)
    results: dict[str, dict[str, Any]] = {}
    try:
        with ThreadPoolExecutor(max_workers=args.concurrency, thread_name_prefix="provenant-batch") as pool:
            futures = {pool.submit(_run_task, task, run_dir, batch_dir): task["id"] for task in tasks}
            for future in as_completed(futures):
                task_id = futures[future]
                try:
                    results[task_id] = future.result()
                except Exception as exc:  # preserve partial batch visibility
                    results[task_id] = {"task_id": task_id, "status": "failed",
                                        "outcome": "batch_worker_error", "message": str(exc)}
    finally:
        if old_handlers:
            for sig, handler in old_handlers.items():
                signal.signal(sig, handler)
    ordered = [results.get(task["id"], {"task_id": task["id"], "status": "cancelled",
                                         "outcome": "batch_cancelled"}) for task in tasks]
    reconciliation_error = None
    try:
        reconcile_manifest(run_dir)
    except (AttemptEvidenceError, OSError) as exc:
        reconciliation_error = str(exc)
    status = "cancelled" if _cancel_event.is_set() else ("failed" if reconciliation_error else "completed")
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
    atomic_write(summary_path, json.dumps(summary, indent=2, sort_keys=True) + "\n")
    index_error = None
    try:
        _index_batch_files(run_dir, batch_id, source_copy, summary_path)
    except OSError as exc:
        index_error = str(exc)
    output = {**summary, "summary_path": str(summary_path.relative_to(run_dir))}
    if index_error:
        output["manifest_index_error"] = index_error
    print(json.dumps(output, sort_keys=True))
    return 1 if (status != "completed" or index_error or any(item["status"] != "succeeded" for item in ordered)) else 0


def batch(args: argparse.Namespace) -> int:
    try:
        tasks = load_manifest(args.manifest, args.concurrency)
        if not 1 <= args.concurrency <= min(MAX_CONCURRENCY, len(tasks)):
            raise BatchInputError(f"concurrency must be between 1 and {min(MAX_CONCURRENCY, len(tasks))}")
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
    try:
        return _execute_batch(args, tasks, run_dir)
    finally:
        fcntl.flock(batch_lock.fileno(), fcntl.LOCK_UN)
        batch_lock.close()


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--run-dir", type=Path, required=True)
    root.add_argument("--manifest", type=Path, required=True)
    root.add_argument("--concurrency", type=int, default=DEFAULT_CONCURRENCY)
    return root


if __name__ == "__main__":
    raise SystemExit(batch(parser().parse_args()))
