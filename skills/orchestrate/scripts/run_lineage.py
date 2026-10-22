"""Explicit attempt ancestry shared by dispatch, batch and provider boundaries."""

import fcntl
import hashlib
import json
from contextlib import contextmanager
import os
from pathlib import Path
import re
import sys
import uuid
from datetime import UTC, datetime

import process_info
from _shared.custody import atomic_write_contained, open_contained_regular, read_contained_regular


PARENT_ENV = "PROVENANT_PARENT_ATTEMPT"
TASK_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")


def parent_attempt():
    try:
        value = json.loads(os.environ.get(PARENT_ENV, "null"))
        if (isinstance(value, dict) and isinstance(value.get("run_id"), str)
                and value["run_id"] and isinstance(value.get("task_id"), str)
                and TASK_ID.fullmatch(value["task_id"])
                and type(value.get("attempt")) is int and value["attempt"] > 0
                and isinstance(value.get("run_dir"), str)
                and Path(value["run_dir"]).is_absolute()):
            return {key: value[key] for key in ("run_id", "task_id", "attempt", "run_dir")}
    except (ValueError, TypeError):
        pass
    return None


def _read(root, relative):
    value = json.loads(read_contained_regular(Path(root), relative, label="run lineage", max_bytes=1024 * 1024)[2])
    if not isinstance(value, dict):
        raise ValueError("run lineage record must be an object")
    return value


def _write(root, relative, value):
    atomic_write_contained(Path(root), relative, (json.dumps(value, indent=2) + "\n").encode(), label="run lineage")


def _journal(task_id, number):
    # The provider can write its private tmp even when the run lives outside
    # the workspace. Canonical receipts remain writable only by their owner.
    return Path(f"dispatch/tasks/{task_id}/attempt-{number:03d}/tmp/children")


@contextmanager
def journal_custody(run_dir, task_id, number):
    relative = _journal(task_id, number)
    if not (Path(run_dir) / relative).exists():
        yield  # Before provider startup, or after terminal tmp pruning.
        return
    fd, _, _ = open_contained_regular(Path(run_dir), relative / ".lock",
                                      os.O_RDWR | os.O_CREAT, label="child index custody")
    with os.fdopen(fd, "a+") as stream:
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX)
        yield


def fields(run_dir=None):
    parent = parent_attempt()
    if parent is None and run_dir is not None:
        parent = _read(run_dir, "RUN_RECEIPT.json").get("parent")
    return {"parent": parent} if parent else {}


def ensure_nested_owner(run_dir, kind, *, batch_child=False):
    """Re-exec direct nested CLIs with an initial environment the reaper can read.

    Fabric already supplies a session leader and fresh run identity. Direct
    CLIs inherit the provider's group and have neither. Changing os.environ
    alone is insufficient: OS process-environment probes read the exec image.
    """
    if batch_child or parent_attempt() is None:
        return
    run_dir = Path(run_dir).resolve()
    if os.environ.get("PROVENANT_RUN_DIR") == str(run_dir) and os.environ.get("PROVENANT_RUN_TOKEN"):
        return
    host_pid = os.getppid()
    host_started = process_info.start_time(host_pid)
    if os.getpgrp() != os.getpid():
        os.setsid()
    token = uuid.uuid4().hex
    run_id = "mcp-" + run_dir.name.rsplit("-", 1)[-1]
    record = {
        "schema_version": 1, "kind": kind, "run_dir": str(run_dir),
        "workspace": str(Path.cwd()), "run_token": token,
        "owner_pid": os.getpid(), "owner_pgid": os.getpgrp(),
        "owner_started_at": process_info.start_time(os.getpid()),
        "host_pid": host_pid, "host_started_at": host_started,
        "started_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "owner_stdout": "", "owner_stderr": "",
        **fields(),
    }
    environment = {**os.environ, "PROVENANT_RUN_DIR": str(run_dir),
                   "PROVENANT_RUN_TOKEN": token, "PROVENANT_RUN_ID": run_id,
                   "PROVENANT_RUN_OWNER": json.dumps(record)}
    os.execvpe(sys.executable, [sys.executable, *sys.argv], environment)


def record_owner(run_dir):
    """Publish a direct CLI's identity only after acquiring its run custody."""
    record = json.loads(os.environ.get("PROVENANT_RUN_OWNER", "null"))
    if (isinstance(record, dict) and record.get("owner_pid") == os.getpid()
            and record.get("run_dir") == str(run_dir)
            and record.get("run_token") == os.environ.get("PROVENANT_RUN_TOKEN")):
        _write(run_dir, "dispatch-owner.json", record)


def _check_parent(parent):
    row = _read(parent["run_dir"], f"tasks/{parent['task_id']}/attempt-{parent['attempt']:03d}/attempt.json")
    if (row.get("run_id"), row.get("task_id"), row.get("attempt"), row.get("state")) != (
            parent["run_id"], parent["task_id"], parent["attempt"], "running"):
        raise ValueError("nested parent attempt is not running or its identity changed")


def register_run(run_dir, run_id):
    """Journal one child run without racing the parent's receipt publisher."""
    parent = parent_attempt()
    if parent is None or Path(parent["run_dir"]).resolve() == Path(run_dir).resolve():
        return
    if os.environ.get("PROVENANT_PARENT_REMOTE") == "1":
        receipt = _read(run_dir, "RUN_RECEIPT.json")
        receipt["parent"] = parent
        _write(run_dir, "RUN_RECEIPT.json", receipt)
        return  # The launching host owns the source-side child journal.
    parent_dir = Path(parent["run_dir"])
    with journal_custody(parent_dir, parent["task_id"], parent["attempt"]):
        _check_parent(parent)
        receipt = _read(run_dir, "RUN_RECEIPT.json")
        receipt["parent"] = parent
        _write(run_dir, "RUN_RECEIPT.json", receipt)
        relative = _journal(parent["task_id"], parent["attempt"])
        key = hashlib.sha256(str(run_dir).encode()).hexdigest()
        _write(parent_dir, relative / f"{key}.json", {"run_id": run_id, "run_dir": str(run_dir)})


def register_external(result):
    parent = parent_attempt()
    if parent is None:
        return
    run_dir = Path(parent["run_dir"])
    relative = _journal(parent["task_id"], parent["attempt"])
    with journal_custody(run_dir, parent["task_id"], parent["attempt"]):
        _check_parent(parent)
        for row in result.get("runs") or [result]:
            run_id = row.get("run_id") or row.get("id")
            if not run_id:
                continue
            key = hashlib.sha256(f"remote:{run_id}:{row.get('task_id')}".encode()).hexdigest()
            _write(run_dir, relative / f"{key}.json", {
                "run_id": run_id, "remote": True,
                **{name: row.get(name) for name in ("task_id", "attempt", "status", "error", "reaped")}})


def refresh_external(run_dir, task_id, number, fetch):
    relative = _journal(task_id, number)
    remote = []
    for path in sorted((Path(run_dir) / relative).glob("*.json")):
        try:
            child = _read(run_dir, relative / path.name)
            if isinstance(child, dict) and child.get("remote"):
                remote.append((path, child))
        except (OSError, ValueError, TypeError):
            continue  # children() retains the unreadable-entry diagnostic.
    if not remote:
        return
    try:
        observed = fetch(list(dict.fromkeys(child["run_id"] for _, child in remote))).get("runs", [])
    except (OSError, ValueError, RuntimeError):
        observed = []
    with journal_custody(run_dir, task_id, number):
        for path, child in remote:
            row = next((row for row in observed if row.get("run_id") == child["run_id"]
                        and row.get("task_id") == child.get("task_id")), None)
            if row is not None:
                child.update({name: row.get(name) for name in ("attempt", "status", "error", "reaped")})
                child["error"] = child.get("error") or (row.get("evidence") or {}).get("signature")
            else:
                child.update(status="lost", error="remote completion evidence unavailable")
            _write(run_dir, relative / path.name, child)


def _owner_gone(child_dir):
    try:
        record = _read(child_dir, "dispatch-owner.json")
        pid = record.get("owner_pid")
        if type(pid) is not int or pid <= 1:
            return False
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        except PermissionError:
            return False
        started = process_info.start_time(pid)
        return bool(started and record.get("owner_started_at") and started != record["owner_started_at"])
    except (OSError, ValueError):
        return False


def _child_attempts(child):
    child_dir = Path(child["run_dir"])
    receipt = _read(child_dir, "RUN_RECEIPT.json")
    latest = {}
    for attempt in sorted((child_dir / "tasks").glob("*/attempt-*/attempt.json")):
        row = _read(child_dir, attempt.relative_to(child_dir))
        if row.get("run_id") != child["run_id"]:
            continue
        task = row.get("task_id")
        if row.get("attempt", 0) > latest.get(task, {}).get("attempt", 0):
            latest[task] = row
    if not latest:
        status = receipt.get("status", "active")
        if status == "active" and _owner_gone(child_dir):
            return [{**child, "status": "lost", "error": "owner exited without terminal attempt evidence"}]
        return [{**child, "status": "running" if status == "active" else status,
                 "error": receipt.get("terminal_reason")}]
    result = []
    for row in latest.values():
        result.append({**child, **{key: row.get(key) for key in ("task_id", "attempt", "status")},
                       "error": row.get("error") or row.get("reason") or (row.get("evidence") or {}).get("signature"),
                       **({"reaped": row["reaped"]} if row.get("reaped") else {})})
        if row.get("state") != "terminal" and _owner_gone(child_dir):
            result[-1].update(status="lost", error="owner exited without terminal attempt evidence")
    return result


def children(run_dir, task_id, number):
    relative = _journal(task_id, number)
    rows = []
    for path in sorted((Path(run_dir) / relative).glob("*.json")):
        try:
            child = _read(run_dir, relative / path.name)
            if not isinstance(child, dict) or not isinstance(child.get("run_id"), str):
                raise ValueError("invalid child identity")
            if not child.get("remote") and (not isinstance(child.get("run_dir"), str)
                                             or not Path(child["run_dir"]).is_absolute()):
                raise ValueError("invalid child run directory")
        except (OSError, ValueError, TypeError):
            rows.append({"run_id": "unknown", "status": "lost",
                         "error": f"lineage journal entry {path.name} is unreadable"})
            continue
        if child.get("remote"):
            rows.append(child)
            continue
        try:
            rows.extend(_child_attempts(child))
        except (OSError, ValueError, TypeError, AttributeError, KeyError):
            rows.append({**child, "status": "lost", "error": "completion evidence unavailable"})
    return rows


def child_errors(children):
    return [f"child run {child['run_id']} task {child.get('task_id', '?')}: "
            + (str(child.get("error") or child.get("status")) if not child.get("reaped")
               else f"{child.get('status')}; reaped {len(child['reaped'])} leftover process(es)")
            for child in children if child.get("status") not in (None, "running", "ok", "partial") or child.get("reaped")]


def annotate_child_errors(row):
    errors = [message for message in child_errors(row.get("children", []))
              if message not in (row.get("error") or "")]
    if errors:
        row["error"] = "; ".join(([row["error"]] if row.get("error") else []) + errors)
