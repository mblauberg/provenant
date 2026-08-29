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
import tempfile
from pathlib import Path
from typing import Any

ATTEMPT_ID_RE = re.compile(r"^attempt-(?:\d{3}|[1-9]\d{3,})$")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
TERMINAL_STATUSES = {"blocked", "succeeded", "failed", "timed_out", "cancelled"}
DISPATCH_RUN = Path(__file__).with_name("dispatch_run.py")


class ControlError(ValueError):
    """A run-control request cannot be answered from safe retained evidence."""


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return f"sha256:{hasher.hexdigest()}"


def _fail(message: str, status: str = "invalid_request") -> int:
    print(json.dumps({"schema_version": 1, "status": status, "message": message}, sort_keys=True))
    return 2


def _run_dir(value: Path) -> Path:
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
    return run_dir


def _relative(run_dir: Path, value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ControlError(f"{label} path is missing")
    path = Path(value)
    if path.is_absolute() or ".." in path.parts:
        raise ControlError(f"{label} path escapes the run: {value}")
    return path.as_posix()


def _regular(run_dir: Path, value: Any, label: str, expected: str | None = None) -> tuple[str, Path]:
    relative = _relative(run_dir, value, label)
    if expected is not None and relative != expected:
        raise ControlError(f"{label} path does not match its attempt: {relative}")
    path = run_dir / relative
    current = run_dir
    try:
        for part in Path(relative).parts:
            current /= part
            if current.is_symlink():
                raise ControlError(f"{label} path contains a symlink: {relative}")
        path.resolve().relative_to(run_dir.resolve())
        metadata = path.lstat()
    except (OSError, ValueError) as exc:
        raise ControlError(f"{label} path is outside or unavailable: {relative}") from exc
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise ControlError(f"{label} path is not a regular single-link file: {relative}")
    return relative, path


def _attempt(run_dir: Path, task_id: str, attempt_id: str) -> tuple[dict[str, Any], dict[str, str], Path]:
    if not TASK_ID_RE.fullmatch(task_id):
        raise ControlError("task id is invalid")
    if not ATTEMPT_ID_RE.fullmatch(attempt_id):
        raise ControlError("attempt id is invalid")
    root = Path("dispatch") / "tasks" / task_id / attempt_id
    attempt_rel = (root / "attempt.json").as_posix()
    _, attempt_path = _regular(run_dir, attempt_rel, "attempt", attempt_rel)
    try:
        record = json.loads(attempt_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ControlError(f"attempt receipt is not valid JSON: {attempt_rel}") from exc
    if (
        not isinstance(record, dict)
        or record.get("schema_version") != 1
        or record.get("record_type") != "dispatch-attempt"
        or record.get("task_id") != task_id
        or record.get("attempt_id") != attempt_id
        or record.get("attempt_path") != attempt_rel
        or record.get("status") not in TERMINAL_STATUSES
    ):
        raise ControlError(f"attempt receipt has invalid identity or status: {attempt_rel}")
    sidecar_rel, sidecar = _regular(run_dir, (root / "attempt.sha256").as_posix(), "attempt digest", (root / "attempt.sha256").as_posix())
    expected_sidecar = f"{digest(attempt_path)}  attempt.json\n"
    if sidecar.read_text(encoding="utf-8") != expected_sidecar:
        raise ControlError(f"attempt digest does not match retained receipt: {attempt_rel}")
    artifacts: dict[str, str] = {"attempt": attempt_rel, "attempt_digest": sidecar_rel}
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
        rel, path = _regular(run_dir, claim.get("path"), label, expected[name].as_posix())
        if claim.get("digest") != digest(path):
            raise ControlError(f"{label} digest does not match retained evidence: {rel}")
        artifacts[name] = rel
    result_claim = record.get("result")
    if result_claim is not None:
        if not isinstance(result_claim, dict):
            raise ControlError(f"result evidence is malformed: {attempt_rel}")
        rel, path = _regular(run_dir, result_claim.get("path"), "result", (root / "result.md").as_posix())
        if result_claim.get("digest") != digest(path):
            raise ControlError(f"result digest does not match retained evidence: {rel}")
        artifacts["result"] = rel
    elif record.get("status") == "succeeded":
        raise ControlError(f"successful attempt has no result evidence: {attempt_rel}")
    if record.get("status") == "blocked":
        question = record.get("question")
        if not isinstance(question, dict) or not isinstance(question.get("prompt"), str) or not question["prompt"]:
            raise ControlError(f"blocked attempt has no question: {attempt_rel}")
    return record, artifacts, attempt_path


def _continuation(record: dict[str, Any]) -> str:
    return f"dispatch/tasks/{record['task_id']}/{record['attempt_id']}/attempt.json"


def _inspect(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    if args.attempt_id and not args.task_id:
        raise ControlError("--attempt-id requires --task-id")
    selected: list[tuple[dict[str, Any], dict[str, str]]] = []
    if args.task_id:
        task_dir = run_dir / "dispatch/tasks" / args.task_id
        if args.attempt_id:
            record, artifacts, _ = _attempt(run_dir, args.task_id, args.attempt_id)
            selected.append((record, artifacts))
        else:
            if task_dir.is_symlink() or not task_dir.is_dir():
                raise ControlError(f"task does not exist: {args.task_id}")
            for path in sorted(task_dir.glob("attempt-*")):
                if path.is_dir():
                    record, artifacts, _ = _attempt(run_dir, args.task_id, path.name)
                    selected.append((record, artifacts))
            if not selected:
                raise ControlError(f"task has no retained attempts: {args.task_id}")
    else:
        tasks_root = run_dir / "dispatch/tasks"
        if tasks_root.exists() and (tasks_root.is_symlink() or not tasks_root.is_dir()):
            raise ControlError("dispatch task directory is invalid")
        for task_dir in sorted(tasks_root.iterdir()) if tasks_root.is_dir() else []:
            if task_dir.is_dir() and not task_dir.is_symlink() and TASK_ID_RE.fullmatch(task_dir.name):
                for path in sorted(task_dir.glob("attempt-*")):
                    if path.is_dir():
                        record, artifacts, _ = _attempt(run_dir, task_dir.name, path.name)
                        selected.append((record, artifacts))
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
    tasks = [{"task_id": task_id, "attempts": attempts} for task_id, attempts in sorted(by_task.items())]
    receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text(encoding="utf-8"))
    print(json.dumps({"schema_version": 1, "run_id": run_dir.name,
                      "run_status": receipt.get("status"), "tasks": tasks}, sort_keys=True))
    return 0


def _artifact(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    record, artifacts, _ = _attempt(run_dir, args.task_id, args.attempt_id)
    key = {"prompt": "prompt", "result": "result", "diagnostic-log": "diagnostic_log"}[args.command]
    relative = artifacts.get(key)
    if relative is None:
        raise ControlError(f"{args.command} evidence is unavailable for {args.task_id}/{args.attempt_id}")
    sys.stdout.buffer.write((run_dir / relative).read_bytes())
    return 0


def _route_selector(values: dict[str, Any]) -> tuple[str, str]:
    selectors = [(name, values.get(name)) for name in ("alias", "task_class", "model") if values.get(name)]
    if len(selectors) != 1 or not isinstance(selectors[0][1], str):
        raise ControlError("exactly one route selector is required")
    return selectors[0]


def _dispatch_command(run_dir: Path, task_id: str, prompt: Path, route: dict[str, Any], retry_of: str | None = None) -> list[str]:
    if not isinstance(route.get("adapter"), str) or not route["adapter"]:
        raise ControlError("adapter is required")
    command = [str(DISPATCH_RUN), "--run-dir", str(run_dir), "--task-id", task_id,
               "--adapter", str(route["adapter"]), "--prompt-file", str(prompt),
               "--role", str(route.get("role", "worker")), "--intent", str(route.get("intent", "ordinary"))]
    selector, value = _route_selector(route)
    command.extend((f"--{selector.replace('_', '-')}", value))
    for key, flag in (("orchestrator_family", "--orchestrator-family"), ("risk_tier", "--risk-tier"),
                      ("reviewer_id", "--reviewer-id"), ("effort", "--effort")):
        if route.get(key):
            command.extend((flag, str(route[key])))
    if retry_of:
        command.extend(("--retry-of", retry_of))
    return command


def _run_child(command: list[str]) -> int:
    try:
        completed = subprocess.run(command, cwd=Path.cwd(), text=False, check=False)
    except OSError as exc:
        raise ControlError(f"dispatch owner cannot be executed: {exc}") from exc
    return completed.returncode


def _retry(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    parent, artifacts, _ = _attempt(run_dir, args.task_id, args.attempt_id)
    if parent["status"] == "succeeded":
        raise ControlError("successful attempts cannot be retried")
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
    prompt = run_dir / artifacts["prompt"]
    return _run_child(_dispatch_command(run_dir, args.task_id, prompt, route, args.attempt_id))


def _load_summaries(run_dir: Path) -> list[dict[str, Any]]:
    root = run_dir / "dispatch/batches"
    if root.is_symlink() or (root.exists() and not root.is_dir()):
        raise ControlError("batch directory is invalid")
    summaries: list[dict[str, Any]] = []
    for batch_dir in sorted(root.iterdir()) if root.is_dir() else []:
        if batch_dir.is_symlink() or not batch_dir.is_dir():
            continue
        summary_path = batch_dir / "summary.json"
        if not summary_path.exists():
            continue
        _, summary_file = _regular(run_dir, summary_path.relative_to(run_dir).as_posix(), "batch summary")
        try:
            summary = json.loads(summary_file.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ControlError(f"batch summary is not valid JSON: {summary_path}") from exc
        if not isinstance(summary, dict) or summary.get("schema_version") != 1 or not isinstance(summary.get("tasks"), list):
            raise ControlError(f"batch summary has invalid schema: {summary_path}")
        seen: set[str] = set()
        for item in summary["tasks"]:
            if not isinstance(item, dict) or not isinstance(item.get("task_id"), str) or item["task_id"] in seen:
                raise ControlError(f"batch summary has invalid task universe: {summary_path}")
            seen.add(item["task_id"])
            if item.get("status") not in TERMINAL_STATUSES:
                raise ControlError(f"batch summary has invalid task status: {summary_path}")
            if item.get("attempt_path") is not None:
                attempt_rel, _ = _regular(run_dir, item["attempt_path"], "batch attempt")
                expected_prefix = f"dispatch/tasks/{item['task_id']}/"
                if not attempt_rel.startswith(expected_prefix) or not attempt_rel.endswith("/attempt.json"):
                    raise ControlError(f"batch summary attempt identity is invalid: {summary_path}")
                attempt_id = attempt_rel[len(expected_prefix):-len("/attempt.json")]
                _attempt(run_dir, item["task_id"], attempt_id)
            elif item.get("status") == "succeeded":
                raise ControlError(f"successful batch task has no attempt path: {summary_path}")
        summaries.append(summary)
    return summaries


def _input_ref(value: str) -> tuple[str, str]:
    task_id, separator, attempt_id = value.partition("/")
    if not separator or not task_id or not attempt_id:
        raise ControlError("each --input must be TASK_ID/ATTEMPT_ID")
    return task_id, attempt_id


def _prompt_file(value: Path) -> Path:
    path = value.expanduser().resolve()
    workspace = Path.cwd().resolve()
    if path != workspace and workspace not in path.parents:
        raise ControlError("prompt file must be inside the workspace")
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise ControlError(f"prompt file is unavailable: {path}") from exc
    if path.is_symlink() or not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise ControlError("prompt file must be a regular single-link file")
    return path


def _reduce(args: argparse.Namespace) -> int:
    run_dir = _run_dir(args.run_dir)
    prompt_file = _prompt_file(args.prompt_file)
    if not args.inputs:
        raise ControlError("reduce requires at least one --input")
    selected: list[tuple[dict[str, Any], dict[str, str]]] = []
    seen: set[tuple[str, str]] = set()
    for value in args.inputs:
        task_id, attempt_id = _input_ref(value)
        if (task_id, attempt_id) in seen:
            raise ControlError(f"duplicate reduction input: {value}")
        seen.add((task_id, attempt_id))
        record, artifacts, _ = _attempt(run_dir, task_id, attempt_id)
        if record["status"] != "succeeded" or "result" not in artifacts:
            raise ControlError(f"reduction input is not a successful retained result: {value}")
        selected.append((record, artifacts))
    summaries = _load_summaries(run_dir)
    selected_paths = {(record["task_id"], artifacts["attempt"]) for record, artifacts in selected}
    omitted: set[str] = set()
    unsuccessful: dict[str, str] = {}
    for summary in summaries:
        for item in summary["tasks"]:
            task_id = item["task_id"]
            attempt_path = item.get("attempt_path")
            if item.get("status") == "succeeded":
                if (task_id, attempt_path) not in selected_paths:
                    omitted.add(task_id)
            elif isinstance(item.get("status"), str):
                unsuccessful[task_id] = item["status"]
            if (task_id, attempt_path) in selected_paths and item.get("status") != "succeeded":
                raise ControlError(f"batch summary contradicts successful reduction input: {task_id}")
    lines = ["# Provenant reduction inputs", "", "The following exact retained successful attempts are supplied to the reducer:", ""]
    for record, artifacts in selected:
        lines.extend([f"## {record['task_id']} / {record['attempt_id']}",
                      f"Result path: {artifacts['result']}",
                      f"Result digest: {record['result']['digest']}", "", (run_dir / artifacts["result"]).read_text(encoding="utf-8", errors="replace"), ""])
    omitted_lines = [f"- {task_id}" for task_id in sorted(omitted)] or ["- none"]
    unsuccessful_lines = [f"- {task_id}: {status}" for task_id, status in sorted(unsuccessful.items())] or ["- none"]
    lines.extend(["## Omitted successful batch tasks", "", *omitted_lines,
                  "", "## Non-successful batch tasks", "", *unsuccessful_lines,
                  "", "## Operator prompt", "", prompt_file.read_text(encoding="utf-8")])
    fd, temporary = tempfile.mkstemp(prefix=".provenant-reducer-", suffix=".md", dir=run_dir)
    prompt = Path(temporary)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write("\n".join(lines))
        route = {"adapter": args.adapter, "role": args.role, "intent": "ordinary",
                 "orchestrator_family": args.orchestrator_family or ""}
        for key in ("alias", "task_class", "model"):
            value = getattr(args, key)
            if value:
                route[key] = value
        _route_selector(route)
        command = _dispatch_command(run_dir, args.task_id, prompt, route)
        return _run_child(command)
    finally:
        prompt.unlink(missing_ok=True)


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

    reduce = commands.add_parser("reduce", help="reduce explicit successful retained results")
    reduce.add_argument("--run-dir", type=Path, required=True)
    reduce.add_argument("--task-id", required=True)
    reduce.add_argument("--prompt-file", type=Path, required=True)
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
        if args.command == "reduce":
            return _reduce(args)
        raise ControlError(f"unsupported run command: {args.command}")
    except ControlError as exc:
        return _fail(str(exc))


if __name__ == "__main__":
    raise SystemExit(run(parser().parse_args()))
