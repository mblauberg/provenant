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
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "skills"))
from _shared.bounded_process import stop_process_group


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


def load_manifest(path: Path) -> list[dict[str, Any]]:
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
        if not isinstance(prompt_value, str) or not prompt_value:
            raise BatchInputError(f"task {task_id} requires prompt_file")
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
        non_overlapping = task.get("non_overlapping", False)
        worktree_isolated = task.get("worktree_isolated", False)
        if not isinstance(non_overlapping, bool) or not isinstance(worktree_isolated, bool):
            raise BatchInputError(f"task {task_id} writer isolation flags must be boolean")
        if source_writing and not (non_overlapping or worktree_isolated):
            raise BatchInputError(
                f"writer task {task_id} requires non_overlapping or worktree_isolated"
            )
        timeout = _finite_timeout(task.get("timeout"))
        normalized = dict(task)
        normalized.update({"id": task_id, "prompt_file": str(prompt), "adapter": adapter,
                           "role": role, "timeout": timeout})
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


def _command(task: dict[str, Any], run_dir: Path) -> list[str]:
    command = [str(DISPATCH_RUN), "--run-dir", str(run_dir), "--task-id", task["id"],
               "--adapter", task["adapter"], "--prompt-file", task["prompt_file"],
               "--role", task["role"], "--intent", task.get("intent", "ordinary"),
               "--timeout", str(task["timeout"])]
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


def _run_task(task: dict[str, Any], run_dir: Path) -> dict[str, Any]:
    task_id = task["id"]
    if _cancel_event.is_set():
        return {"task_id": task_id, "status": "cancelled", "outcome": "batch_cancelled"}
    process: subprocess.Popen[str] | None = None
    started = time.monotonic()
    try:
        process = subprocess.Popen(_command(task, run_dir), cwd=Path.cwd(), text=True,
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
    record = _parse_record(stdout)
    if record is None:
        return {"task_id": task_id, "status": "failed", "outcome": "dispatch_output_invalid",
                "dispatch_exit": process.returncode, "stderr": stderr[-1000:]}
    status = record.get("status") if isinstance(record.get("status"), str) else "failed"
    result = record.get("result") if isinstance(record.get("result"), dict) else None
    return {
        "task_id": task_id,
        "status": status,
        "outcome": record.get("outcome", status),
        "dispatch_exit": process.returncode,
        "attempt_path": record.get("attempt_path"),
        "attempt_digest": record.get("attempt_digest"),
        "result_path": result.get("path") if result else None,
        "duration_seconds": round(time.monotonic() - started, 6),
    }


def batch(args: argparse.Namespace) -> int:
    try:
        tasks = load_manifest(args.manifest)
        if not 1 <= args.concurrency <= min(MAX_CONCURRENCY, len(tasks)):
            raise BatchInputError(f"concurrency must be between 1 and {min(MAX_CONCURRENCY, len(tasks))}")
        run_dir = _validate_run_dir(args.run_dir)
        if not DISPATCH_RUN.is_file() or not os.access(DISPATCH_RUN, os.X_OK):
            raise BatchInputError(f"dispatch owner is unavailable: {DISPATCH_RUN}")
    except BatchInputError as exc:
        print(json.dumps({"schema_version": 1, "status": "invalid_manifest", "message": str(exc)}, sort_keys=True))
        return 2

    _cancel_event.clear()
    batch_id = f"batch-{_batch_number(run_dir):03d}"
    batch_dir = run_dir / "dispatch" / "batches" / batch_id
    batch_dir.mkdir(parents=True)
    old_handlers = {}
    if threading.current_thread() is threading.main_thread():
        old_handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGHUP)}
        signal.signal(signal.SIGTERM, _signal_handler)
        signal.signal(signal.SIGHUP, _signal_handler)
    results: dict[str, dict[str, Any]] = {}
    try:
        with ThreadPoolExecutor(max_workers=args.concurrency, thread_name_prefix="provenant-batch") as pool:
            futures = {pool.submit(_run_task, task, run_dir): task["id"] for task in tasks}
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
    status = "cancelled" if _cancel_event.is_set() else "completed"
    counts = dict(sorted(Counter(item["status"] for item in ordered).items()))
    summary = {
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": batch_id,
        "status": status, "task_count": len(ordered), "concurrency": args.concurrency,
        "counts": counts,
        "tasks": ordered,
        "reducer_inputs": [
            {"task_id": item["task_id"], "status": item["status"],
             "attempt_path": item.get("attempt_path"), "result_path": item.get("result_path")}
            for item in ordered if item.get("attempt_path") or item.get("result_path")
        ],
    }
    summary_path = batch_dir / "summary.json"
    atomic_write(summary_path, json.dumps(summary, indent=2, sort_keys=True) + "\n")
    output = {**summary, "summary_path": str(summary_path.relative_to(run_dir))}
    print(json.dumps(output, sort_keys=True))
    return 1 if status == "cancelled" or any(item["status"] != "succeeded" for item in ordered) else 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--run-dir", type=Path, required=True)
    root.add_argument("--manifest", type=Path, required=True)
    root.add_argument("--concurrency", type=int, default=DEFAULT_CONCURRENCY)
    return root


if __name__ == "__main__":
    raise SystemExit(batch(parser().parse_args()))
