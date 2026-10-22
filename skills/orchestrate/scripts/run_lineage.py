"""Explicit attempt ancestry shared by dispatch, batch and provider boundaries."""

import fcntl
import hashlib
import json
from contextlib import contextmanager
import os
from pathlib import Path
import re
import sys
import signal
import subprocess
import time
import uuid
from datetime import UTC, datetime

import process_info
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from _shared.custody import (
    OwnedFileError,
    atomic_write_contained,
    open_contained_regular,
    read_contained_regular,
)


PARENT_ENV = "PROVENANT_PARENT_ATTEMPT"
PARENT_TOKEN_ENV = "PROVENANT_PARENT_RUN_TOKEN"
_nested_custody = {}
CUSTODY_ENV = "PROVENANT_NESTED_CUSTODY_FD"
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
    if batch_child:
        run_dir = Path(run_dir).resolve()

        def validate_batch_owner():
            try:
                record = _read(run_dir, "dispatch-owner.json")
            except OwnedFileError as exc:
                if isinstance(exc.__cause__, FileNotFoundError):
                    raise FileNotFoundError("batch run owner is missing") from exc
                raise
            token = os.environ.get("PROVENANT_RUN_TOKEN")
            if (record.get("schema_version") != 1
                    or record.get("kind") not in {"dispatch", "batch"}
                    or record.get("run_dir") != str(run_dir)
                    or not isinstance(record.get("run_token"), str)
                    or not record["run_token"]
                    or (token and record["run_token"] != token)
                    or record.get("owner_pid") not in {os.getpid(), os.getppid()}
                    or not record.get("owner_started_at")
                    or record["owner_started_at"] != process_info.start_time(record["owner_pid"])):
                raise ValueError("batch run owner identity is invalid")

        try:
            validate_batch_owner()
            return
        except FileNotFoundError:
            # A standalone batch child can initialise an owner. A nested child
            # must use the batch parent's already-published owner identity.
            if parent_attempt() is not None or os.environ.get("PROVENANT_RUN_TOKEN"):
                raise ValueError("nested batch run owner is missing")
        custody = acquire_owner_custody(run_dir)
        try:
            try:
                validate_batch_owner()
            except FileNotFoundError:
                record_owner(run_dir, kind)
                validate_batch_owner()
        finally:
            custody.close()
        return
    if parent_attempt() is None:
        return
    run_dir = Path(run_dir).resolve()
    if os.environ.get("PROVENANT_RUN_DIR") == str(run_dir) and os.environ.get("PROVENANT_RUN_TOKEN"):
        inherited = os.environ.pop(CUSTODY_ENV, None)
        if inherited is not None:
            fd = int(inherited)
            check, _, _ = open_contained_regular(run_dir, "MANIFEST.md", os.O_RDWR | os.O_APPEND, label="MANIFEST.md")
            try:
                if (os.fstat(fd).st_dev, os.fstat(fd).st_ino) != (os.fstat(check).st_dev, os.fstat(check).st_ino):
                    raise ValueError("nested run custody identity changed")
            finally:
                os.close(check)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                os.close(fd)
                return  # Let the ordinary entry point report busy custody.
            os.set_inheritable(fd, False)
            _nested_custody[str(run_dir)] = os.fdopen(fd, "a+", encoding="utf-8")
            # The exec image and published owner now agree before detaching.
            if os.getpgrp() != os.getpid():
                os.setsid()
        return
    try:
        custody = acquire_owner_custody(run_dir)
    except OSError:
        return  # The ordinary owner entry point reports a busy run.
    host_pid = os.getppid()
    host_started = process_info.start_time(host_pid)
    token = uuid.uuid4().hex
    run_id = "mcp-" + run_dir.name.rsplit("-", 1)[-1]
    record = {
        "schema_version": 1, "kind": kind, "run_dir": str(run_dir),
        "workspace": str(Path.cwd()), "run_token": token,
        "owner_pid": os.getpid(), "owner_pgid": os.getpid(),
        "owner_started_at": process_info.start_time(os.getpid()),
        "host_pid": host_pid, "host_started_at": host_started,
        "started_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "owner_stdout": "", "owner_stderr": "",
        **fields(),
    }
    environment = {**os.environ, "PROVENANT_RUN_DIR": str(run_dir),
                   "PROVENANT_RUN_TOKEN": token, "PROVENANT_RUN_ID": run_id,
                   "PROVENANT_RUN_OWNER": json.dumps(record)}
    try:
        _write(run_dir, "dispatch-owner.json", record)
        os.set_inheritable(custody.fileno(), True)
        environment[CUSTODY_ENV] = str(custody.fileno())
        os.execvpe(sys.executable, [sys.executable, *sys.argv], environment)
    finally:
        custody.close()


def acquire_owner_custody(run_dir):
    """Acquire manifest custody, including a direct CLI's pre-exec lock."""
    inherited = _nested_custody.pop(str(Path(run_dir).resolve()), None)
    if inherited is not None:
        return inherited
    fd, _, _ = open_contained_regular(Path(run_dir), "MANIFEST.md", os.O_RDWR | os.O_APPEND, label="MANIFEST.md")
    stream = os.fdopen(fd, "a+", encoding="utf-8")
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        stream.close()
        raise
    return stream


def record_owner(run_dir, kind="dispatch"):
    """Publish a direct CLI's identity only after acquiring its run custody."""
    record = json.loads(os.environ.get("PROVENANT_RUN_OWNER", "null"))
    if (isinstance(record, dict) and record.get("owner_pid") == os.getpid()
            and record.get("run_dir") == str(run_dir)
            and record.get("run_token") == os.environ.get("PROVENANT_RUN_TOKEN")):
        _write(run_dir, "dispatch-owner.json", record)
    else:
        # Custody authorises this owner to replace a previous owner's identity.
        # Keep the initial token inherited by the children this batch launches.
        token = os.environ.get("PROVENANT_RUN_TOKEN") or uuid.uuid4().hex
        _write(run_dir, "dispatch-owner.json", {
            "schema_version": 1, "kind": kind, "run_dir": str(run_dir), "run_token": token,
            "owner_pid": os.getpid(), "owner_pgid": os.getpgrp(),
            "owner_started_at": process_info.start_time(os.getpid()), **fields()})


def _check_parent(parent):
    token = os.environ.get(PARENT_TOKEN_ENV)
    if not token or token != _read(parent["run_dir"], "dispatch-owner.json").get("run_token"):
        raise ValueError("nested parent run token is missing or does not match")
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
    _check_parent(parent)
    parent_dir = Path(parent["run_dir"])
    with journal_custody(parent_dir, parent["task_id"], parent["attempt"]):
        _check_parent(parent)
        receipt = _read(run_dir, "RUN_RECEIPT.json")
        receipt["parent"] = parent
        _write(run_dir, "RUN_RECEIPT.json", receipt)
        relative = _journal(parent["task_id"], parent["attempt"])
        key = hashlib.sha256(str(run_dir).encode()).hexdigest()
        owner = _read(run_dir, "dispatch-owner.json")
        _write(parent_dir, relative / f"{key}.json", {
            "run_id": run_id, "run_dir": str(run_dir), "run_token": owner.get("run_token")})


def register_external(result):
    parent = parent_attempt()
    if parent is None:
        return
    _check_parent(parent)
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


def _runs_root(run_dir):
    for ancestor in Path(run_dir).resolve().parents:
        if ancestor.name == ".agent-run":
            return ancestor / "runs"
    raise ValueError("parent run has no runs root")


def _verified_child(run_dir, task_id, number, child):
    """Never let a provider-written path select evidence outside the runs root."""
    root = _runs_root(run_dir)
    directory = Path(child.get("run_dir", ""))
    if (not directory.is_absolute() or directory.parent != root
            or directory.resolve(strict=True) != directory):
        raise ValueError("invalid child run directory")
    owner = _read(root, Path(directory.name) / "dispatch-owner.json")
    if (owner.get("schema_version") != 1 or owner.get("kind") not in {"dispatch", "batch"}
            or owner.get("run_dir") != str(directory)
            or not child.get("run_token") or child["run_token"] != owner.get("run_token")):
        raise ValueError("unverified child owner")
    receipt = _read(root, Path(directory.name) / "RUN_RECEIPT.json")
    parent = receipt.get("parent") or {}
    if ((parent.get("run_dir"), parent.get("task_id"), parent.get("attempt"))
            != (str(run_dir), task_id, number)):
        raise ValueError("unverified child parent")
    return {"run_id": child["run_id"], "run_dir": str(directory)}


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
        info = process_info.process(pid)
        if info is not None and "Z" in info.stat:
            return True
        started = process_info.start_time(pid)
        return bool(started and record.get("owner_started_at") and started != record["owner_started_at"])
    except (OSError, ValueError):
        return False


def _child_attempts(child, runs_root):
    child_dir = Path(child["run_dir"])
    receipt = _read(runs_root, Path(child_dir.name) / "RUN_RECEIPT.json")
    latest = {}
    for attempt in sorted((child_dir / "tasks").glob("*/attempt-*/attempt.json")):
        row = _read(runs_root, attempt.relative_to(runs_root))
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
            print(f"ignored unverified lineage journal entry {path.name}", file=sys.stderr)
            continue
        if child.get("remote"):
            rows.append(child)
            continue
        try:
            child = _verified_child(run_dir, task_id, number, child)
        except (OSError, ValueError, TypeError, AttributeError, KeyError):
            print(f"ignored unverified lineage journal entry {path.name}", file=sys.stderr)
            continue
        try:
            rows.extend(_child_attempts(child, _runs_root(run_dir)))
        except (OSError, ValueError, TypeError, AttributeError, KeyError):
            rows.append({**child, "status": "lost", "error": "completion evidence unavailable"})
    return rows


def owner_token(run_dir):
    return _read(run_dir, "dispatch-owner.json").get("run_token")


def cancel_children(run_dir, task_id, number, *, timeout_seconds=5):
    """Cancel verified local children together, then bound uncooperative cleanup."""
    import provider_exec

    class RecordedOwner:
        def __init__(self, record, identity):
            self.pid, self.identity = record["owner_pid"], identity

        def poll(self):
            row = process_info._probe_process_row(self.pid)
            return None if row and row.identity == self.identity and not row.zombie else 0

        def wait(self, timeout):
            if self.poll() is None:
                raise subprocess.TimeoutExpired("nested owner", timeout)
            return 0

    pending = []
    seen = set()
    for child in children(run_dir, task_id, number):
        if child.get("remote") or child.get("run_dir") in seen:
            continue
        directory = Path(child["run_dir"])
        seen.add(str(directory))
        try:
            record = _read(directory, "dispatch-owner.json")
            pid = record.get("owner_pid")
            info = process_info.process(pid) if type(pid) is int and pid > 1 else None
            if (info is None or "Z" in info.stat or pid == os.getpid()
                    or info.pgid != pid or record.get("owner_pgid") != pid
                    or not record.get("owner_started_at")
                    or process_info.start_time(pid) != record["owner_started_at"]):
                continue
            row = process_info._probe_process_row(pid)
            if row is None or row.zombie:
                continue
            process = RecordedOwner(record, row.identity)
            tree = provider_exec._Descendants(process, None)
            tree.sample()
            os.kill(pid, signal.SIGTERM)
            pending.append((process, tree))
        except (OSError, ValueError, TypeError):
            print("nested cancellation owner unavailable", file=sys.stderr)
    deadline = time.monotonic() + timeout_seconds
    while pending and time.monotonic() < deadline:
        if all(process.poll() is not None for process, _ in pending):
            break
        time.sleep(.05)
    for process, tree in pending:
        rows = tree.sample(include_reparented=True)
        if process.poll() is None or tree.live(rows):
            tree.stop(root_grace=.2, descendant_grace=.05)


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
