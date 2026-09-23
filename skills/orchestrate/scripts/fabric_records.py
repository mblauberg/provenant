"""Fabric v1 rows, terse digests and locked durable terminal indexes."""

from contextlib import contextmanager
from datetime import UTC, datetime, timedelta
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets

try:
    from .layout import run_root
    from .output_custody import open_parent
except ImportError:
    from layout import run_root
    from output_custody import open_parent

TERMINAL_STATUSES = {
    "ok",
    "partial",
    "failed",
    "usage_limited",
    "rate_limited",
    "auth_required",
    "model_unavailable",
    "permission_blocked",
    "stalled",
    "timed_out",
    "cancelled",
    "interrupted",
    "rejected",
    "tool_missing",
    "input_required",
}


def timestamp(value):
    return value.astimezone(UTC).isoformat().replace("+00:00", "Z")


def parse_time(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def render_digest(row):
    run_id = row.get("run_id", "")
    state = row.get("state", "terminal")
    status = row.get("status") or state
    prov = row.get("provenance") or {}
    requested = prov.get("requested") or {}
    route = (
        requested.get("adapter", "")
        + "/"
        + (prov.get("observed_model") or prov.get("resolved_model") or "?")
    )
    effort = prov.get("effort_applied")
    if effort:
        route += "@" + effort
    if status == "rejected":
        return (
            "rejected "
            + row.get("error", "preflight")
            + " · fix: "
            + (row.get("fix") or "inspect receipt")
        )
    if status == "input_required":
        question = row.get("question") or ""
        if isinstance(question, dict):
            question = question.get("prompt", "")
        return (
            f"input_required {run_id} "
            + json.dumps(" ".join(question.split())[:200], ensure_ascii=False)
            + f' · reply: fabric_dispatch{{resume:"{run_id}",prompt:"…"}}'
        )
    if state != "terminal":
        fallback = prov.get("fallback_from")
        if fallback:
            previous = fallback.get("route", "")
            return f"running {run_id} attempt {row['attempt']}: {previous} {fallback['status']} → {route}"
        access = (
            ("write " + str(row.get("worktree") or row.get("cwd")))
            if row.get("mode") == "worktree_write"
            else "read"
        )
        return f'running {run_id} {route} {access} · fabric_status{{ids:["{run_id}"],wait_seconds:55}}'
    try:
        duration = max(
            0,
            round(
                (
                    parse_time(row["ended_at"]) - parse_time(row["started_at"])
                ).total_seconds()
            ),
        )
    except (KeyError, TypeError, ValueError):
        duration = 0
    result = row.get("paths", {}).get("result")
    detail = " · result " + result if result else ""
    if status not in {"ok", "partial"}:
        detail = " · " + (
            row.get("fix")
            or row.get("evidence", {}).get("signature")
            or "inspect stderr"
        )
        if row.get("reset_at"):
            detail += " (resets " + row["reset_at"] + ")"
    text = f"{status} {run_id} {route} {duration}s" + detail
    if prov.get("line"):
        text += "\n  " + prov["line"]
    return text


def cooldown_path():
    return Path(
        os.environ.get("FABRIC_COOLDOWNS_PATH")
        or Path.home() / ".local/state/agent-harness/fabric/cooldowns.json"
    )


def store_parent(path):
    """Create missing parents through directory descriptors; refuse redirected stores."""
    try:
        return open_parent(str(path))
    except FileNotFoundError:
        parent, leaf = store_parent(Path(path).parent)
        try:
            try:
                os.mkdir(leaf, 0o700, dir_fd=parent)
            except FileExistsError:
                pass
        finally:
            os.close(parent)
        return open_parent(str(path))


def store_open(path, flags, create_parents=False):
    parent, leaf = store_parent(path) if create_parents else open_parent(str(path))
    try:
        return os.open(leaf, flags | getattr(os, "O_NOFOLLOW", 0), 0o600, dir_fd=parent)
    finally:
        os.close(parent)


@contextmanager
def locked(path):
    path = Path(path)
    fd = store_open(path, os.O_RDWR | os.O_CREAT, create_parents=True)
    try:
        if os.fstat(fd).st_nlink != 1:
            raise ValueError("lock must not be hard-linked")
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def read_cooldowns(path=None, at=None):
    at = at or datetime.now(UTC)
    path = Path(path or cooldown_path())
    try:
        if path.is_symlink():
            return {}
        with os.fdopen(store_open(path, os.O_RDONLY)) as stream:
            document = json.load(stream)
        entries = (
            document.get("cooldowns", {})
            if document.get("schema") == "fabric.cooldowns.v1"
            else {}
        )
        return {
            key: value
            for key, value in entries.items()
            if isinstance(value, dict) and parse_time(value["cooling_until"]) > at
        }
    except (OSError, ValueError, TypeError, KeyError, AttributeError):
        return {}


def write_cooldown(row, *, path=None, at=None):
    if row["status"] not in {"usage_limited", "rate_limited"}:
        return
    path = Path(path or cooldown_path())
    at = at or datetime.now(UTC)
    prov = row["provenance"]
    adapter = prov["requested"]["adapter"]
    model = prov["resolved_model"] or "*"
    reset = row.get("reset_at")
    try:
        until = parse_time(reset) if reset else None
    except (ValueError, TypeError):
        until = None
    if until is None or until <= at:
        delay = row.get("retry_after")
        if not isinstance(delay, (int, float)) or delay <= 0:
            delay = 900 if row["status"] == "rate_limited" else 3600
        until = at + timedelta(seconds=delay)
    with locked(path.with_name("cooldowns.lock")):
        values = read_cooldowns(path, at)
        values[adapter + "/" + model] = {
            "adapter": adapter,
            "model": model,
            "account": None,
            "status": row["status"],
            "cooling_until": timestamp(until),
            "source_run": row["run_id"],
            "recorded_at": timestamp(at),
            "signature": row["evidence"]["signature"],
        }
        parent, leaf = store_parent(path)
        temp = ".cooldowns-" + secrets.token_hex(12)
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=parent)
        try:
            with os.fdopen(fd, "w") as stream:
                json.dump(
                    {"schema": "fabric.cooldowns.v1", "cooldowns": values},
                    stream,
                    indent=2,
                )
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temp, leaf, src_dir_fd=parent, dst_dir_fd=parent)
            os.fsync(parent)
        finally:
            try:
                os.unlink(temp, dir_fd=parent)
            except FileNotFoundError:
                pass
            os.close(parent)


def append_index(row, run_dir, *, root=None):
    root = Path(root) if root is not None else run_root(row["cwd"]) / ".agent-run"
    path = root / "runs/index.jsonl"
    key = (row["run_id"], row["task_id"], row["attempt"])
    result = row.get("paths", {}).get("result")
    result_sha = row.get("result_sha256")
    if result and result_sha is None:
        candidate = Path(result)
        if not candidate.is_absolute():
            candidate = Path(run_dir) / candidate
        # Artifact custody was verified by the owner; index only run-local files.
        if (
            candidate.resolve().is_relative_to(Path(run_dir).resolve())
            and candidate.is_file()
            and not candidate.is_symlink()
        ):
            result_sha = "sha256:" + hashlib.sha256(candidate.read_bytes()).hexdigest()
    entry = {
        "run_id": row["run_id"],
        "task_id": row["task_id"],
        "attempt": row["attempt"],
        "dir": str(run_dir),
        "status": row["status"],
        "line": row["provenance"].get("line", ""),
        "result_sha256": result_sha,
        "ended_at": row["ended_at"],
    }
    with locked(path.with_suffix(".lock")):
        fd = store_open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND, create_parents=True)
        with os.fdopen(fd, "a+") as stream:
            if os.fstat(stream.fileno()).st_nlink != 1:
                raise ValueError("index must not be hard-linked")
            stream.seek(0)
            for line in stream:
                try:
                    old = json.loads(line)
                except ValueError:
                    continue
                if (old.get("run_id"), old.get("task_id"), old.get("attempt")) == key:
                    return
            stream.seek(0, os.SEEK_END)
            stream.write(json.dumps(entry, separators=(",", ":")) + "\n")
            stream.flush()
            os.fsync(stream.fileno())
