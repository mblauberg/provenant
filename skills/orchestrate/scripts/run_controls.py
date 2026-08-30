#!/usr/bin/env python3
"""Small file-backed operator controls for retained dispatch attempts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

ATTEMPT_ID_RE = re.compile(r"^attempt-(?:\d{3}|[1-9]\d{3,})$")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
BATCH_ID_RE = re.compile(r"^batch-(?:\d{3}|[1-9]\d{3,})$")
TERMINAL_STATUSES = {"blocked", "succeeded", "failed", "timed_out", "cancelled"}
DISPATCH_RUN = Path(__file__).with_name("dispatch_run.py")
MAX_WORKER_QUESTION_PROMPT = 4096
MAX_OPERATOR_RESPONSE_BYTES = 64 * 1024
MAX_CANCEL_WAIT_SECONDS = 60.0

sys.path.insert(0, str(Path(__file__).resolve().parent))
from dispatch_run import create_cancellation_marker
from _shared.custody import OwnedFileError, read_contained_regular


class ControlError(ValueError):
    """A run-control request cannot be answered from safe retained evidence."""


def _valid_worker_question(value: Any) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == {"code", "prompt"}
        and value.get("code") == "needs_input"
        and isinstance(value.get("prompt"), str)
        and bool(value["prompt"])
        and len(value["prompt"]) <= MAX_WORKER_QUESTION_PROMPT
        and "\x00" not in value["prompt"]
    )


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return f"sha256:{hasher.hexdigest()}"


def _digest_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def _fail(message: str, status: str = "invalid_request") -> int:
    print(json.dumps({"schema_version": 1, "status": status, "message": message}, sort_keys=True))
    return 2


def _cancel_result(status: str, message: str, *, code: int = 0) -> int:
    print(json.dumps({"status": status, "message": message}, sort_keys=True))
    return code


def _cancel_wait_seconds(value: Any) -> float:
    if isinstance(value, bool):
        raise ControlError("wait-seconds must be a finite number between 0 and 60")
    try:
        result = float(value)
    except (TypeError, ValueError) as exc:
        raise ControlError("wait-seconds must be a finite number between 0 and 60") from exc
    if result < 0 or result != result or result == float("inf") or result > MAX_CANCEL_WAIT_SECONDS:
        raise ControlError("wait-seconds must be a finite number between 0 and 60")
    return result


def _create_cancel_marker(run_dir: Path, directory: Path) -> None:
    try:
        create_cancellation_marker(run_dir, directory)
    except ValueError as exc:
        raise ControlError(str(exc)) from exc


def _attempt_terminal(run_dir: Path, task_id: str, attempt_id: str) -> dict[str, Any] | None:
    attempt_path = run_dir / "dispatch" / "tasks" / task_id / attempt_id / "attempt.json"
    try:
        metadata = attempt_path.lstat()
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise ControlError("attempt evidence is unavailable") from exc
    if attempt_path.is_symlink() or not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise ControlError("attempt evidence is invalid")
    try:
        record, _, _, _ = _attempt(run_dir, task_id, attempt_id)
    except ControlError as exc:
        raise ControlError("attempt evidence is invalid") from exc
    process = record.get("process")
    if not isinstance(process, dict) or process.get("observed_exit") is not True:
        raise ControlError("attempt evidence does not prove observed completion")
    return record


def _wait_for_attempt_terminal(run_dir: Path, task_id: str, attempt_id: str, wait_seconds: float) -> dict[str, Any] | None:
    deadline = time.monotonic() + wait_seconds
    while True:
        record = _attempt_terminal(run_dir, task_id, attempt_id)
        if record is not None:
            return record
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        time.sleep(min(0.05, remaining))


def _cancel(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir, require_active=True)
    wait_seconds = _cancel_wait_seconds(args.wait_seconds)
    task_target = args.task_id is not None or args.attempt_id is not None
    batch_target = args.batch_id is not None
    if task_target == batch_target or (args.task_id is None) != (args.attempt_id is None):
        raise ControlError("cancel requires exactly one of --task-id with --attempt-id or --batch-id")

    if task_target:
        if (not isinstance(args.task_id, str) or not TASK_ID_RE.fullmatch(args.task_id)
                or not isinstance(args.attempt_id, str) or not ATTEMPT_ID_RE.fullmatch(args.attempt_id)):
            raise ControlError("cancel target identity is invalid")
        target = run_dir / "dispatch" / "tasks" / args.task_id / args.attempt_id
        if target.is_symlink() or not target.is_dir():
            raise ControlError("cancel target attempt directory does not exist")
        existing = _attempt_terminal(run_dir, args.task_id, args.attempt_id)
        if existing is not None:
            return _cancel_result("already_terminal", "attempt already has validated terminal evidence")
        _create_cancel_marker(run_dir, target)
        existing = _wait_for_attempt_terminal(run_dir, args.task_id, args.attempt_id, wait_seconds)
        if existing is None:
            return _cancel_result("completion_evidence_missing", "owner did not produce validated terminal attempt evidence", code=1)
        status = "cancelled" if existing.get("status") == "cancelled" else "already_terminal"
        return _cancel_result(status, "validated terminal attempt evidence is durable")

    if not isinstance(args.batch_id, str) or not BATCH_ID_RE.fullmatch(args.batch_id):
        raise ControlError("cancel target identity is invalid")
    target = run_dir / "dispatch" / "batches" / args.batch_id
    if target.is_symlink() or not target.is_dir():
        raise ControlError("cancel target batch directory does not exist")
    summary_path = target / "summary.json"
    try:
        metadata = summary_path.lstat()
        summary_present = True
    except FileNotFoundError:
        summary_present = False
    except OSError as exc:
        raise ControlError("batch summary evidence is unavailable") from exc
    if summary_present:
        if summary_path.is_symlink() or not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ControlError("batch summary evidence is invalid")
        _load_summary(run_dir, args.batch_id, require_complete=True)
        return _cancel_result("already_terminal", "batch already has validated terminal summary evidence")
    _create_cancel_marker(run_dir, target)
    deadline = time.monotonic() + wait_seconds
    while True:
        try:
            summary = _load_summary(run_dir, args.batch_id, require_complete=True)
        except ControlError:
            try:
                summary_path.lstat()
                summary_present = True
            except FileNotFoundError:
                summary_present = False
            if summary_present:
                raise ControlError("batch summary evidence is invalid")
            summary = None
        if summary is not None:
            status = "cancelled" if summary.get("status") == "cancelled" else "already_terminal"
            return _cancel_result(status, "validated terminal batch summary evidence is durable")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return _cancel_result("completion_evidence_missing", "owner did not produce validated terminal batch summary evidence", code=1)
        time.sleep(min(0.05, remaining))


def _run_dir(value: Path, *, require_active: bool = False) -> Path:
    run_dir = value.resolve()
    workspace = Path.cwd().resolve()
    if not run_dir.is_dir() or (run_dir != workspace and workspace not in run_dir.parents):
        raise ControlError("run directory must be an existing child of the workspace")
    for name in ("RUN_RECEIPT.json", "MANIFEST.md"):
        path = run_dir / name
        if path.is_symlink() or not path.is_file() or path.stat().st_nlink != 1:
            raise ControlError(f"run directory is missing a regular {name}")
    try:
        receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ControlError("RUN_RECEIPT.json is not valid JSON") from exc
    if not isinstance(receipt, dict) or receipt.get("schema_version") != 1:
        raise ControlError("RUN_RECEIPT.json schema_version must be 1")
    if require_active and (receipt.get("status") != "active" or receipt.get("closed_at") is not None):
        raise ControlError("cancellation requires an active orchestration run")
    return run_dir


def _relative(run_dir: Path, value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ControlError(f"{label} path is missing")
    path = Path(value)
    if path.is_absolute() or not path.parts or path == Path(".") or ".." in path.parts:
        raise ControlError(f"{label} path escapes the run: {value}")
    return path.as_posix()


def _read_regular(
    run_dir: Path,
    value: Any,
    label: str,
    expected: str | None = None,
    max_bytes: int | None = None,
) -> tuple[str, Path, bytes]:
    relative = _relative(run_dir, value, label)
    if expected is not None and relative != expected:
        raise ControlError(f"{label} path does not match its attempt: {relative}")
    try:
        return read_contained_regular(run_dir, relative, label=label, max_bytes=max_bytes)
    except (OwnedFileError, OSError, ValueError) as exc:
        raise ControlError(f"{label} path is outside or unavailable: {relative}") from exc


def _attempt(run_dir: Path, task_id: str, attempt_id: str) -> tuple[dict[str, Any], dict[str, str], Path, dict[str, bytes]]:
    if not TASK_ID_RE.fullmatch(task_id):
        raise ControlError("task id is invalid")
    if not ATTEMPT_ID_RE.fullmatch(attempt_id):
        raise ControlError("attempt id is invalid")
    root = Path("dispatch") / "tasks" / task_id / attempt_id
    attempt_rel = (root / "attempt.json").as_posix()
    _, attempt_path, attempt_bytes = _read_regular(run_dir, attempt_rel, "attempt", attempt_rel)
    try:
        record = json.loads(attempt_bytes.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ControlError(f"attempt receipt is not valid JSON: {attempt_rel}") from exc
    if (
        not isinstance(record, dict)
        or record.get("schema_version") != 1
        or record.get("record_type") != "dispatch-attempt"
        or record.get("run_id") != run_dir.name
        or record.get("task_id") != task_id
        or record.get("attempt_id") != attempt_id
        or record.get("attempt_path") != attempt_rel
        or record.get("status") not in TERMINAL_STATUSES
    ):
        raise ControlError(f"attempt receipt has invalid identity or status: {attempt_rel}")
    sidecar_rel, _, sidecar_bytes = _read_regular(run_dir, (root / "attempt.sha256").as_posix(), "attempt digest", (root / "attempt.sha256").as_posix())
    expected_sidecar = f"{_digest_bytes(attempt_bytes)}  attempt.json\n"
    if sidecar_bytes.decode("utf-8") != expected_sidecar:
        raise ControlError(f"attempt digest does not match retained receipt: {attempt_rel}")
    artifacts: dict[str, str] = {"attempt": attempt_rel, "attempt_digest": sidecar_rel}
    payloads: dict[str, bytes] = {"attempt": attempt_bytes}
    expected = {
        "prompt": root / "prompt.md",
        "diagnostic_log": root / "stderr.log",
        "adapter_receipt": root / "adapter-receipt.json",
    }
    claims: list[tuple[str, Any, str]] = [
        ("prompt", record.get("prompt"), "prompt"),
        ("diagnostic_log", record.get("stderr"), "diagnostic log"),
    ]
    route = record.get("route")
    adapter_claim = route.get("adapter_receipt") if isinstance(route, dict) else None
    claims.append(("adapter_receipt", adapter_claim, "adapter receipt"))
    for name, claim, label in claims:
        if not isinstance(claim, dict):
            raise ControlError(f"{label} evidence is missing: {attempt_rel}")
        rel, path, data = _read_regular(run_dir, claim.get("path"), label, expected[name].as_posix())
        if claim.get("digest") != _digest_bytes(data):
            raise ControlError(f"{label} digest does not match retained evidence: {rel}")
        artifacts[name] = rel
        payloads[name] = data
    result_claim = record.get("result")
    if result_claim is not None:
        if not isinstance(result_claim, dict):
            raise ControlError(f"result evidence is malformed: {attempt_rel}")
        rel, path, data = _read_regular(run_dir, result_claim.get("path"), "result", (root / "result.md").as_posix())
        if result_claim.get("digest") != _digest_bytes(data):
            raise ControlError(f"result digest does not match retained evidence: {rel}")
        artifacts["result"] = rel
        payloads["result"] = data
    elif record.get("status") == "succeeded":
        raise ControlError(f"successful attempt has no result evidence: {attempt_rel}")
    if record.get("status") == "blocked":
        question = record.get("question")
        if not _valid_worker_question(question):
            raise ControlError(f"blocked attempt has no question: {attempt_rel}")
    return record, artifacts, attempt_path, payloads


def _validate_successful_adapter(
    run_dir: Path, record: dict[str, Any], payloads: dict[str, bytes]
) -> None:
    """Validate successful adapter claims against the already-read evidence bytes."""
    adapter_bytes = payloads.get("adapter_receipt")
    if adapter_bytes is None:
        raise ControlError("successful attempt has no adapter receipt")
    try:
        lines = [line for line in adapter_bytes.decode("utf-8").splitlines() if line.strip()]
        adapter = json.loads(lines[-1]) if lines else None
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ControlError("successful adapter receipt is not valid JSON") from exc
    if not isinstance(adapter, dict) or adapter.get("status") != "ok":
        raise ControlError("successful adapter receipt is invalid")
    route = record.get("route")
    if not isinstance(route, dict):
        raise ControlError("successful attempt route is invalid")
    required = (
        "tool", "adapter", "execution_intent", "resolved_model", "provider_family",
        "model_family", "endpoint_provider", "identity_source", "output_path", "output_digest",
        "read_only_guarantee",
    )
    if any(not isinstance(adapter.get(field), str) or not adapter[field] for field in required):
        raise ControlError("successful adapter receipt is missing route identity")
    requested = record.get("requested_route")
    expected_adapter = requested.get("adapter") if isinstance(requested, dict) else None
    expected_intent = requested.get("intent") if isinstance(requested, dict) else None
    if adapter["tool"] != expected_adapter or adapter["adapter"] != expected_adapter:
        raise ControlError("successful adapter receipt does not match the requested adapter")
    if adapter["execution_intent"] != expected_intent:
        raise ControlError("successful adapter receipt does not match the requested intent")
    for field in ("adapter", "execution_intent", "provider_family", "resolved_model"):
        if route.get(field) != adapter.get(field):
            raise ControlError(f"successful attempt route disagrees with adapter receipt: {field}")
    result = record.get("result")
    result_bytes = payloads.get("result")
    if not isinstance(result, dict) or result_bytes is None:
        raise ControlError("successful adapter receipt has no retained result")
    try:
        output_matches = Path(adapter["output_path"]).resolve() == (run_dir / result["path"]).resolve()
    except (OSError, TypeError, ValueError):
        output_matches = False
    if not output_matches or adapter["output_digest"] != _digest_bytes(result_bytes):
        raise ControlError("successful adapter receipt does not match the retained output")
    if not isinstance(adapter.get("exit"), int) or isinstance(adapter["exit"], bool) or adapter["exit"] != 0:
        raise ControlError("successful adapter receipt must record exit 0")
    if not isinstance(adapter.get("cross_family"), bool) or not isinstance(adapter.get("certification_eligible"), bool):
        raise ControlError("successful adapter receipt is missing assurance flags")
    if expected_intent == "ordinary" and adapter["certification_eligible"]:
        raise ControlError("ordinary execution cannot be certification eligible")


def _continuation(record: dict[str, Any]) -> str:
    return f"dispatch/tasks/{record['task_id']}/{record['attempt_id']}/attempt.json"


def _inspect_batches(run_dir: Path) -> list[dict[str, Any]]:
    root = run_dir / "dispatch/batches"
    if root.is_symlink() or (root.exists() and not root.is_dir()):
        raise ControlError("batch directory is invalid")
    batches: list[dict[str, Any]] = []
    for batch_dir in sorted(root.iterdir()) if root.is_dir() else []:
        if batch_dir.is_symlink() or not batch_dir.is_dir():
            continue
        summary_path = batch_dir / "summary.json"
        if not summary_path.exists():
            continue
        summary_rel, summary_file, summary_bytes = _read_regular(run_dir, summary_path.relative_to(run_dir).as_posix(), "batch summary")
        try:
            summary = json.loads(summary_bytes.decode("utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ControlError(f"batch summary is not valid JSON: {summary_path}") from exc
        if (not isinstance(summary, dict) or summary.get("schema_version") != 1
                or summary.get("record_type") != "dispatch-batch"
                or summary.get("batch_id") != batch_dir.name or not isinstance(summary.get("tasks"), list)):
            raise ControlError(f"batch summary has invalid schema or identity: {summary_path}")
        tasks: list[dict[str, Any]] = []
        seen: set[str] = set()
        for item in summary["tasks"]:
            if (not isinstance(item, dict) or not isinstance(item.get("task_id"), str)
                    or not TASK_ID_RE.fullmatch(item["task_id"]) or item["task_id"] in seen):
                raise ControlError(f"batch summary has invalid task universe: {summary_path}")
            seen.add(item["task_id"])
            task = {"task_id": item["task_id"], "status": item.get("status"),
                    "receipt_status": "unavailable"}
            if task["status"] not in TERMINAL_STATUSES:
                raise ControlError(f"batch summary has invalid task status: {summary_path}")
            attempt_path = item.get("attempt_path")
            if attempt_path is None:
                task["claimed_status"] = task["status"]
                task["status"] = "incomplete"
                task["receipt_status"] = "receipt_unavailable"
            if attempt_path is not None and not isinstance(attempt_path, str):
                raise ControlError(f"batch summary attempt path is not a string: {summary_path}")
            if isinstance(attempt_path, str):
                try:
                    attempt_rel = _relative(run_dir, attempt_path, "batch attempt")
                    expected_prefix = f"dispatch/tasks/{item['task_id']}/"
                    if not attempt_rel.startswith(expected_prefix) or not attempt_rel.endswith("/attempt.json"):
                        raise ControlError("batch summary attempt identity is invalid")
                    attempt_id = attempt_rel[len(expected_prefix):-len("/attempt.json")]
                    attempt_file = run_dir / attempt_rel
                    if not attempt_file.exists():
                        task["attempt_path"] = attempt_rel
                        task["claimed_status"] = task["status"]
                        task["status"] = "incomplete"
                        task["receipt_status"] = "receipt_unavailable"
                    else:
                        record, artifacts, _, _ = _attempt(run_dir, item["task_id"], attempt_id)
                        if record["status"] != item["status"]:
                            raise ControlError(f"batch summary status disagrees with attempt: {item['task_id']}")
                        if item["status"] == "succeeded" and item.get("result_path") != artifacts.get("result"):
                            raise ControlError(f"batch summary result identity disagrees with attempt: {item['task_id']}")
                        task.update({"attempt_id": record["attempt_id"], "attempt_path": artifacts["attempt"],
                                     "receipt_status": "validated"})
                except ControlError:
                    raise
            tasks.append(task)
        batches.append({"batch_id": batch_dir.name, "status": summary.get("status"),
                        "summary_path": summary_rel, "tasks": tasks})
    return batches


def _inspect(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    if args.task_id and not TASK_ID_RE.fullmatch(args.task_id):
        raise ControlError("task id is invalid")
    if args.attempt_id and not ATTEMPT_ID_RE.fullmatch(args.attempt_id):
        raise ControlError("attempt id is invalid")
    if args.attempt_id and not args.task_id:
        raise ControlError("--attempt-id requires --task-id")
    selected: list[tuple[dict[str, Any], dict[str, str]]] = []
    incomplete: dict[str, list[dict[str, Any]]] = {}

    def collect(task_id: str, path: Path) -> None:
        if not path.is_dir() or path.is_symlink():
            return
        attempt_file = path / "attempt.json"
        if not attempt_file.exists():
            incomplete.setdefault(task_id, []).append({
                "attempt_id": path.name, "status": "incomplete",
                "receipt_status": "unavailable",
                "artifacts": {"attempt": f"dispatch/tasks/{task_id}/{path.name}/attempt.json"},
            })
            return
        record, artifacts, _, _ = _attempt(run_dir, task_id, path.name)
        selected.append((record, artifacts))

    if args.task_id:
        task_dir = run_dir / "dispatch/tasks" / args.task_id
        if args.attempt_id:
            attempt_dir = task_dir / args.attempt_id
            if attempt_dir.is_dir() and not attempt_dir.is_symlink() and not (attempt_dir / "attempt.json").exists():
                collect(args.task_id, attempt_dir)
            else:
                record, artifacts, _, _ = _attempt(run_dir, args.task_id, args.attempt_id)
                selected.append((record, artifacts))
        else:
            if task_dir.is_symlink() or not task_dir.is_dir():
                raise ControlError(f"task does not exist: {args.task_id}")
            for path in sorted(task_dir.glob("attempt-*")):
                collect(args.task_id, path)
            if not selected and not incomplete.get(args.task_id):
                raise ControlError(f"task has no retained attempts: {args.task_id}")
    else:
        tasks_root = run_dir / "dispatch/tasks"
        if tasks_root.exists() and (tasks_root.is_symlink() or not tasks_root.is_dir()):
            raise ControlError("dispatch task directory is invalid")
        for task_dir in sorted(tasks_root.iterdir()) if tasks_root.is_dir() else []:
            if task_dir.is_dir() and not task_dir.is_symlink() and TASK_ID_RE.fullmatch(task_dir.name):
                for path in sorted(task_dir.glob("attempt-*")):
                    collect(task_dir.name, path)
    by_task: dict[str, list[dict[str, Any]]] = {}
    for record, artifacts in selected:
        item: dict[str, Any] = {
            "attempt_id": record["attempt_id"], "status": record["status"],
            "outcome": record.get("outcome"), "artifacts": artifacts,
        }
        if record["status"] == "blocked":
            item["question"] = record["question"]
            item["continuation_ref"] = _continuation(record)
        by_task.setdefault(record["task_id"], []).append(item)
    for task_id, items in incomplete.items():
        by_task.setdefault(task_id, []).extend(items)
    tasks = [{"task_id": task_id, "attempts": attempts} for task_id, attempts in sorted(by_task.items())]
    receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text(encoding="utf-8"))
    print(json.dumps({"schema_version": 1, "run_id": run_dir.name,
                      "run_status": receipt.get("status"), "tasks": tasks,
                      "batches": _inspect_batches(run_dir)}, sort_keys=True))
    return 0


def _artifact(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    _, artifacts, _, payloads = _attempt(run_dir, args.task_id, args.attempt_id)
    key = {"prompt": "prompt", "result": "result", "diagnostic-log": "diagnostic_log"}[args.command]
    relative = artifacts.get(key)
    if relative is None:
        raise ControlError(f"{args.command} evidence is unavailable for {args.task_id}/{args.attempt_id}")
    sys.stdout.buffer.write(payloads[key])
    return 0


def _route_selector(values: dict[str, Any]) -> tuple[str, str]:
    selectors = [(name, values.get(name)) for name in ("alias", "task_class", "model") if values.get(name)]
    if len(selectors) != 1 or not isinstance(selectors[0][1], str):
        raise ControlError("exactly one route selector is required")
    return selectors[0]


def _dispatch_command(run_dir: Path, task_id: str, prompt: Path | None, route: dict[str, Any], retry_of: str | None = None, *, prompt_stdin: bool = False) -> list[str]:
    if not isinstance(route.get("adapter"), str) or not route["adapter"]:
        raise ControlError("adapter is required")
    command = [str(DISPATCH_RUN), "--run-dir", str(run_dir), "--task-id", task_id,
               "--adapter", str(route["adapter"]),
               "--role", str(route.get("role", "worker")), "--intent", str(route.get("intent", "ordinary"))]
    if prompt_stdin:
        command.append("--prompt-stdin")
    elif prompt is not None:
        command.extend(("--prompt-file", str(prompt)))
    else:
        raise ControlError("dispatch prompt source is required")
    selector, value = _route_selector(route)
    command.extend((f"--{selector.replace('_', '-')}", value))
    for key, flag in (("orchestrator_family", "--orchestrator-family"), ("risk_tier", "--risk-tier"),
                      ("reviewer_id", "--reviewer-id"), ("effort", "--effort")):
        if route.get(key):
            command.extend((flag, str(route[key])))
    if retry_of:
        command.extend(("--retry-of", retry_of))
    return command


def _run_child(command: list[str], input_bytes: bytes | None = None) -> int:
    try:
        completed = subprocess.run(command, cwd=Path.cwd(), input=input_bytes, text=False, check=False)
    except OSError as exc:
        raise ControlError(f"dispatch owner cannot be executed: {exc}") from exc
    return completed.returncode


def _operator_response(args: argparse.Namespace) -> bytes:
    if args.response_file is not None:
        _, response = _prompt_file(args.response_file, max_bytes=MAX_OPERATOR_RESPONSE_BYTES)
    else:
        try:
            response = sys.stdin.buffer.read(MAX_OPERATOR_RESPONSE_BYTES + 1)
        except OSError as exc:
            raise ControlError(f"operator response is unavailable: {exc}") from exc
    if len(response) > MAX_OPERATOR_RESPONSE_BYTES:
        raise ControlError(f"operator response exceeds the {MAX_OPERATOR_RESPONSE_BYTES}-byte limit")
    try:
        response_text = response.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ControlError("operator response must be valid UTF-8") from exc
    if not response_text or not response_text.strip():
        raise ControlError("operator response must not be empty or whitespace-only")
    if "\x00" in response_text:
        raise ControlError("operator response must not contain NUL")
    return response


def _continuation_prompt(original: bytes, question: dict[str, Any], response: bytes) -> bytes:
    question_bytes = json.dumps(question, ensure_ascii=True, sort_keys=True).encode("utf-8")
    original_prefix = original if original.endswith(b"\n") else original + b"\n"
    return original_prefix + b"\n".join((
        b"## Provenant blocked question",
        question_bytes,
        b"## Operator response",
        response,
    )) + b"\n"


def _retry(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    parent, artifacts, _, payloads = _attempt(run_dir, args.task_id, args.attempt_id)
    if parent["status"] == "succeeded":
        raise ControlError("successful attempts cannot be retried")
    has_response_source = args.response_file is not None or args.response_stdin
    if parent["status"] == "blocked":
        if not has_response_source:
            raise ControlError("blocked retries require exactly one operator response source")
        question = parent.get("question")
        if not _valid_worker_question(question):
            raise ControlError("blocked attempt has an invalid question")
        prompt_bytes = _continuation_prompt(payloads["prompt"], question, _operator_response(args))
    else:
        if has_response_source:
            raise ControlError("operator response is only valid for blocked retries")
        prompt_bytes = payloads["prompt"]
    if args.same_route == args.reroute:
        raise ControlError("retry requires exactly one of --same-route or --reroute")
    if args.same_route:
        if any((args.adapter, args.alias, args.task_class, args.model, args.orchestrator_family)):
            raise ControlError("--same-route cannot be combined with reroute options")
        route = dict(parent.get("requested_route") or {})
        route["adapter"] = route.get("adapter")
        _route_selector(route)
    else:
        if not args.adapter:
            raise ControlError("--reroute requires --adapter")
        route = {"adapter": args.adapter, "role": (parent.get("requested_route") or {}).get("role", "worker"),
                 "intent": (parent.get("requested_route") or {}).get("intent", "ordinary"),
                 "orchestrator_family": args.orchestrator_family or ""}
        for key in ("alias", "task_class", "model"):
            value = getattr(args, key)
            if value:
                route[key] = value
        _route_selector(route)
    return _run_child(
        _dispatch_command(run_dir, args.task_id, None, route, args.attempt_id, prompt_stdin=True),
        prompt_bytes,
    )


def _load_summary(run_dir: Path, batch_id: str, *, require_complete: bool = False) -> dict[str, Any]:
    if not BATCH_ID_RE.fullmatch(batch_id):
        raise ControlError("batch id is invalid")
    summary_path = run_dir / "dispatch/batches" / batch_id / "summary.json"
    _, _, summary_bytes = _read_regular(run_dir, summary_path.relative_to(run_dir).as_posix(), "batch summary")
    try:
        summary = json.loads(summary_bytes.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ControlError(f"batch summary is not valid JSON: {summary_path}") from exc
    if (not isinstance(summary, dict) or summary.get("schema_version") != 1
            or summary.get("record_type") != "dispatch-batch"
            or summary.get("batch_id") != batch_id or not isinstance(summary.get("tasks"), list)):
        raise ControlError(f"batch summary has invalid schema or identity: {summary_path}")
    source = summary.get("source_manifest")
    expected_source = f"dispatch/batches/{batch_id}/task-manifest.json"
    source_value = None
    if source is None and not require_complete:
        pass
    else:
        if not isinstance(source, dict) or source.get("path") != expected_source:
            raise ControlError(f"batch summary source manifest identity is invalid: {summary_path}")
        _, _, source_bytes = _read_regular(run_dir, expected_source, "batch source manifest", expected_source)
        if source.get("digest") != _digest_bytes(source_bytes):
            raise ControlError(f"batch source manifest digest does not match: {summary_path}")
        try:
            source_value = json.loads(source_bytes.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ControlError(f"batch source manifest is not valid JSON: {summary_path}") from exc
        if require_complete:
            source_tasks = source_value.get("tasks") if isinstance(source_value, dict) else None
            source_ids = [task.get("id") for task in source_tasks] if isinstance(source_tasks, list) else None
            if (
                not isinstance(source_value, dict)
                or source_value.get("schema_version") != 1
                or not isinstance(source_tasks, list)
                or not source_tasks
                or any(not isinstance(task, dict) or not isinstance(task.get("id"), str) for task in source_tasks)
                or source_ids != [item.get("task_id") for item in summary["tasks"]]
            ):
                raise ControlError(f"batch source task universe disagrees with summary: {summary_path}")
    if require_complete:
        if summary.get("status") not in {"completed", "cancelled", "failed"}:
            raise ControlError(f"batch summary has invalid terminal status: {summary_path}")
        task_count = summary.get("task_count")
        if type(task_count) is not int or task_count < 1 or task_count != len(summary["tasks"]):
            raise ControlError(f"batch summary has invalid task count: {summary_path}")
    seen: set[str] = set()
    actual_counts: dict[str, int] = {}
    for item in summary["tasks"]:
        if (not isinstance(item, dict) or not isinstance(item.get("task_id"), str)
                or not TASK_ID_RE.fullmatch(item["task_id"]) or item["task_id"] in seen):
            raise ControlError(f"batch summary has invalid task universe: {summary_path}")
        seen.add(item["task_id"])
        status = item.get("status")
        if status not in TERMINAL_STATUSES:
            raise ControlError(f"batch summary has invalid task status: {summary_path}")
        actual_counts[status] = actual_counts.get(status, 0) + 1
        attempt_path = item.get("attempt_path")
        if attempt_path is None:
            if status == "succeeded":
                raise ControlError(f"successful batch task has no attempt path: {summary_path}")
            continue
        attempt_rel = _relative(run_dir, attempt_path, "batch attempt")
        expected_prefix = f"dispatch/tasks/{item['task_id']}/"
        if not attempt_rel.startswith(expected_prefix) or not attempt_rel.endswith("/attempt.json"):
            raise ControlError(f"batch summary attempt identity is invalid: {summary_path}")
        attempt_id = attempt_rel[len(expected_prefix):-len("/attempt.json")]
        record, artifacts, _, payloads = _attempt(run_dir, item["task_id"], attempt_id)
        if record["status"] != status:
            raise ControlError(f"batch summary status disagrees with attempt: {item['task_id']}")
        for field in ("requested_route", "route"):
            flattened = item.get(field)
            canonical = record.get(field)
            if flattened is not None:
                if not isinstance(flattened, dict) or not isinstance(canonical, dict) or any(
                    key not in canonical or canonical[key] != value for key, value in flattened.items()
                ):
                    raise ControlError(f"batch summary {field} disagrees with attempt: {item['task_id']}")
        if require_complete and item.get("outcome") != record.get("outcome"):
            raise ControlError(f"batch summary outcome disagrees with attempt: {item['task_id']}")
        attempt_digest = item.get("attempt_digest")
        if require_complete and not isinstance(attempt_digest, str):
            raise ControlError(f"batch summary attempt digest is missing: {item['task_id']}")
        if attempt_digest is not None and attempt_digest != _digest_bytes(payloads["attempt"]):
            raise ControlError(f"batch summary attempt digest disagrees with attempt: {item['task_id']}")
        process = record.get("process")
        if not isinstance(process, dict) or not isinstance(process.get("observed_exit"), bool):
            raise ControlError(f"batch attempt has invalid observed completion: {item['task_id']}")
        if status == "succeeded" and process.get("observed_exit") is not True:
            raise ControlError(f"successful batch attempt lacks observed completion: {item['task_id']}")
        result_path = item.get("result_path")
        if result_path is not None and result_path != artifacts.get("result"):
            raise ControlError(f"batch summary result identity disagrees with attempt: {item['task_id']}")
        if status == "succeeded" and result_path is None:
            raise ControlError(f"successful batch task has no result path: {summary_path}")
        if require_complete and status == "succeeded":
            _validate_successful_adapter(run_dir, record, payloads)
    if require_complete:
        if summary.get("counts") != dict(sorted(actual_counts.items())):
            raise ControlError(f"batch summary counts disagree with its tasks: {summary_path}")
        if (summary["status"] == "cancelled") != (actual_counts.get("cancelled", 0) > 0):
            raise ControlError(f"batch summary cancellation status disagrees with its tasks: {summary_path}")
        reducer_inputs = summary.get("reducer_inputs")
        if not isinstance(reducer_inputs, list):
            raise ControlError(f"batch summary reducer inputs are missing: {summary_path}")
        expected_reducers = {
            item["task_id"]: item for item in summary["tasks"]
            if item.get("attempt_path") or item.get("result_path")
        }
        seen_reducers: set[str] = set()
        for reducer in reducer_inputs:
            if not isinstance(reducer, dict) or not isinstance(reducer.get("task_id"), str):
                raise ControlError(f"batch summary reducer input identity is invalid: {summary_path}")
            task_id = reducer["task_id"]
            if task_id in seen_reducers or task_id not in expected_reducers:
                raise ControlError(f"batch summary reducer input is foreign or duplicated: {task_id}")
            seen_reducers.add(task_id)
            expected = expected_reducers[task_id]
            if any(reducer.get(key) != expected.get(key) for key in ("status", "attempt_path", "result_path")):
                raise ControlError(f"batch summary reducer input disagrees with task: {task_id}")
        if seen_reducers != set(expected_reducers):
            raise ControlError(f"batch summary reducer inputs do not cover its tasks: {summary_path}")
    return summary


def validate_retained_dispatch(run_dir: Path) -> list[str]:
    """Validate every retained attempt and complete batch through control owners."""
    errors: list[str] = []
    task_root = run_dir / "dispatch" / "tasks"
    if task_root.is_dir() and not task_root.is_symlink():
        for attempt_path in sorted(task_root.glob("*/attempt-*/attempt.json")):
            task_id = attempt_path.parent.parent.name
            attempt_id = attempt_path.parent.name
            try:
                record, artifacts, _path, payloads = _attempt(run_dir, task_id, attempt_id)
                process = record.get("process")
                if not isinstance(process, dict) or not isinstance(process.get("observed_exit"), bool):
                    raise ControlError("attempt has invalid observed completion")
                if record.get("status") == "succeeded" and process.get("observed_exit") is not True:
                    raise ControlError("successful attempt does not prove observed completion")
                if record.get("status") == "succeeded" and process.get("exit_code") != 0:
                    raise ControlError("successful attempt does not prove exit 0")
                if record.get("status") == "succeeded":
                    _validate_successful_adapter(run_dir, record, payloads)
            except (ControlError, OSError, ValueError) as exc:
                errors.append(f"dispatch attempt {attempt_path.relative_to(run_dir)}: {exc}")
    batch_root = run_dir / "dispatch" / "batches"
    if batch_root.is_dir() and not batch_root.is_symlink():
        for batch_dir in sorted(batch_root.iterdir()):
            if not batch_dir.is_dir() or batch_dir.is_symlink() or not BATCH_ID_RE.fullmatch(batch_dir.name):
                continue
            try:
                _load_summary(run_dir, batch_dir.name, require_complete=True)
            except (ControlError, OSError, ValueError) as exc:
                errors.append(f"dispatch batch {batch_dir.name}: {exc}")
    return errors


def _input_ref(value: str) -> tuple[str, str]:
    task_id, separator, attempt_id = value.partition("/")
    if not separator or not task_id or not attempt_id:
        raise ControlError("each --input must be TASK_ID/ATTEMPT_ID")
    return task_id, attempt_id


def _prompt_file(value: Path, max_bytes: int | None = None) -> tuple[Path, bytes]:
    supplied = value.expanduser()
    if supplied.is_symlink():
        raise ControlError("prompt file must not be a symlink")
    workspace = Path.cwd().resolve()
    lexical = Path(os.path.abspath(supplied))
    try:
        relative = lexical.relative_to(workspace).as_posix()
    except ValueError:
        raise ControlError("prompt file must be inside the workspace")
    if not relative or relative == ".":
        raise ControlError("prompt file must be a regular single-link file")
    parts = [part.casefold() for part in lexical.parts]
    sensitive_roots = {".ssh", ".aws", ".azure", ".gnupg"}
    sensitive_files = {
        ".env", ".env.local", ".env.production", "credentials.json",
        "application_default_credentials.json", "token.json",
    }
    config_auth_dirs = {"gcloud", "gh", "claude", "codex", "openai"}
    config_auth = any(
        part == ".config" and index + 1 < len(parts) and parts[index + 1] in config_auth_dirs
        for index, part in enumerate(parts)
    )
    if sensitive_roots.intersection(parts) or lexical.name.casefold() in sensitive_files or config_auth:
        raise ControlError("prompt file is a credential or authentication store")
    try:
        _, _, prompt_bytes = _read_regular(workspace, relative, "prompt", max_bytes=max_bytes)
    except ControlError as exc:
        raise ControlError(f"prompt file is unavailable: {lexical}") from exc
    return lexical, prompt_bytes


def _reduce(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    _, prompt_bytes = _prompt_file(args.prompt_file)
    if not args.inputs:
        raise ControlError("reduce requires at least one --input")
    selected: list[tuple[dict[str, Any], dict[str, str], bytes]] = []
    seen: set[tuple[str, str]] = set()
    for value in args.inputs:
        task_id, attempt_id = _input_ref(value)
        if (task_id, attempt_id) in seen:
            raise ControlError(f"duplicate reduction input: {value}")
        seen.add((task_id, attempt_id))
        record, artifacts, _, payloads = _attempt(run_dir, task_id, attempt_id)
        if record["status"] != "succeeded" or "result" not in artifacts:
            raise ControlError(f"reduction input is not a successful retained result: {value}")
        selected.append((record, artifacts, payloads["result"]))
    summary = _load_summary(run_dir, args.batch_id)
    selected_paths = {(record["task_id"], artifacts["attempt"]) for record, artifacts, _ in selected}
    omitted: set[str] = set()
    unsuccessful: dict[str, str] = {}
    universe: set[tuple[str, str]] = set()
    for item in summary["tasks"]:
        task_id = item["task_id"]
        attempt_path = item.get("attempt_path")
        label = task_id
        if isinstance(attempt_path, str):
            prefix = f"dispatch/tasks/{task_id}/"
            if attempt_path.startswith(prefix) and attempt_path.endswith("/attempt.json"):
                label = f"{task_id}/{attempt_path[len(prefix):-len('/attempt.json')]}"
        if attempt_path is not None:
            universe.add((task_id, attempt_path))
        if item.get("status") == "succeeded":
            if (task_id, attempt_path) not in selected_paths:
                omitted.add(label)
        elif isinstance(item.get("status"), str):
            unsuccessful[label] = item["status"]
    absent = [record["task_id"] for record, artifacts, _ in selected if (record["task_id"], artifacts["attempt"]) not in universe]
    if absent:
        raise ControlError("reduction input is not an exact attempt in the selected batch: " + ", ".join(absent))
    lines = ["# Provenant reduction inputs", "", "The following exact retained successful attempts are supplied to the reducer:", ""]
    for record, artifacts, result_bytes in selected:
        lines.extend([f"## {record['task_id']} / {record['attempt_id']}",
                      f"Result path: {artifacts['result']}",
                      f"Result digest: {record['result']['digest']}", "", result_bytes.decode("utf-8", errors="replace"), ""])
    omitted_lines = [f"- {task_id}" for task_id in sorted(omitted)] or ["- none"]
    unsuccessful_lines = [f"- {task_id}: {status}" for task_id, status in sorted(unsuccessful.items())] or ["- none"]
    lines.extend(["## Omitted successful batch tasks", "", *omitted_lines,
                  "", "## Non-successful batch tasks", "", *unsuccessful_lines,
                  "", "## Operator prompt", "", prompt_bytes.decode("utf-8", errors="replace")])
    route = {"adapter": args.adapter, "role": args.role, "intent": "ordinary",
             "orchestrator_family": args.orchestrator_family or ""}
    for key in ("alias", "task_class", "model"):
        value = getattr(args, key)
        if value:
            route[key] = value
    _route_selector(route)
    command = _dispatch_command(run_dir, args.task_id, None, route, prompt_stdin=True)
    return _run_child(command, "\n".join(lines).encode())


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    inspect = commands.add_parser("inspect", help="report retained task and attempt state")
    inspect.add_argument("--run-dir", type=Path, required=True)
    inspect.add_argument("--task-id")
    inspect.add_argument("--attempt-id")

    for name in ("prompt", "result", "diagnostic-log"):
        command = commands.add_parser(name, help=f"emit one exact retained {name} artifact")
        command.add_argument("--run-dir", type=Path, required=True)
        command.add_argument("--task-id", required=True)
        command.add_argument("--attempt-id", required=True)

    retry = commands.add_parser("retry", help="create a new explicit retry attempt")
    retry.add_argument("--run-dir", type=Path, required=True)
    retry.add_argument("--task-id", required=True)
    retry.add_argument("--attempt-id", required=True)
    choice = retry.add_mutually_exclusive_group(required=True)
    choice.add_argument("--same-route", action="store_true")
    choice.add_argument("--reroute", action="store_true")
    retry.add_argument("--adapter")
    route = retry.add_mutually_exclusive_group()
    route.add_argument("--alias")
    route.add_argument("--task-class")
    route.add_argument("--model")
    retry.add_argument("--orchestrator-family")
    response = retry.add_mutually_exclusive_group()
    response.add_argument("--response-file", type=Path)
    response.add_argument("--response-stdin", action="store_true")

    cancel = commands.add_parser("cancel", help="request cooperative cancellation of one attempt or batch")
    cancel.add_argument("--run-dir", type=Path, required=True)
    cancel.add_argument("--task-id")
    cancel.add_argument("--attempt-id")
    cancel.add_argument("--batch-id")
    cancel.add_argument("--wait-seconds", type=float, default=5.0)

    reduce = commands.add_parser("reduce", help="reduce explicit successful retained results")
    reduce.add_argument("--run-dir", type=Path, required=True)
    reduce.add_argument("--task-id", required=True)
    reduce.add_argument("--prompt-file", type=Path, required=True)
    reduce.add_argument("--batch-id", required=True)
    reduce.add_argument("--input", dest="inputs", action="append", required=True)
    reduce.add_argument("--adapter", required=True)
    selector = reduce.add_mutually_exclusive_group(required=True)
    selector.add_argument("--alias")
    selector.add_argument("--task-class")
    selector.add_argument("--model")
    reduce.add_argument("--role", default="reducer")
    reduce.add_argument("--orchestrator-family")
    return root


def run(args: argparse.Namespace) -> int:
    try:
        if args.command == "inspect":
            return _inspect(args)
        if args.command in {"prompt", "result", "diagnostic-log"}:
            return _artifact(args)
        if args.command == "retry":
            return _retry(args)
        if args.command == "cancel":
            return _cancel(args)
        if args.command == "reduce":
            return _reduce(args)
        raise ControlError(f"unsupported run command: {args.command}")
    except ControlError as exc:
        return _fail(str(exc), "invalid_target" if args.command == "cancel" else "invalid_request")


if __name__ == "__main__":
    raise SystemExit(run(parser().parse_args()))
