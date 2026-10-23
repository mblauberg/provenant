"""Writer worktree identity, reservations and exclusive leases."""

from __future__ import annotations

import fcntl
import json
import os
import re
import sqlite3
import subprocess
import time
from pathlib import Path
from typing import Any

BRANCH_TYPES = "feat|fix|refactor|perf|test|docs|chore|ci|build"
BRANCH_PATTERN = re.compile(rf"^(?:(?:{BRANCH_TYPES}|proto)/[a-z][a-z0-9]*-[a-z0-9]+(?:-[a-z0-9]+){{1,4}}|land/[0-9]{{8}}-[a-z0-9]+(?:-[a-z0-9]+)*)$")

def workspace_identity(workspace: Path, provider_cwd: Path | None = None) -> dict[str, Any]:
    identity: dict[str, Any] = {
        "cwd": str((provider_cwd or workspace).resolve()),
        "root": str(workspace.resolve()),
        "base_revision": None,
        "working_tree": "unavailable",
    }
    try:
        base = subprocess.run(
            ["git", "rev-parse", "--show-toplevel", "HEAD"],
            cwd=workspace,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
            check=True,
        ).stdout.splitlines()
        if len(base) != 2 or not re.fullmatch(r"[0-9a-fA-F]{40,64}", base[1]):
            return identity
        dirty = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=normal"],
            cwd=workspace,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
            check=True,
        ).stdout
        identity.update(
            base_revision=base[1].lower(),
            working_tree="dirty" if dirty else "clean",
        )
    except (OSError, subprocess.SubprocessError):
        pass
    return identity

# Directories a launch itself churns, or Git owns; edits under them are not writer work.
STARTUP_WALK_SKIP = {".git", ".agent-run", "node_modules", ".venv", "__pycache__", ".pytest_cache", "dist", "build"}
STARTUP_WALK_MAX_ENTRIES = 200_000
STARTUP_WALK_MAX_SECONDS = 5.0

def writer_worktree_state(cwd: str) -> tuple[str, bytes] | None:
    """HEAD and porcelain status of a writer's worktree, or None when Git cannot say."""
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    try:
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=cwd, env=env, capture_output=True,
                              text=True, timeout=10, check=True).stdout.strip()
        status = subprocess.run(["git", "status", "--porcelain=v1", "-z", "--untracked-files=normal"],
                                cwd=cwd, env=env, capture_output=True, timeout=10, check=True).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    return head, status

def worktree_unchanged(start, cwd: str) -> bool | None:
    """Whether a clean starting worktree still has the same HEAD and porcelain; unknown from a dirty start."""
    if start is None or start[1]:
        return None
    current = writer_worktree_state(cwd)
    return None if current is None else current == start

def modified_since(cwd: str, since: float) -> bool:
    """Whether any entry under cwd, ignored files included, changed at or after since.

    Hitting the entry or time cap, or an unreadable directory, counts as changed.
    """
    deadline = time.monotonic() + STARTUP_WALK_MAX_SECONDS
    visited = 0
    pending = [cwd]
    while pending:
        directory = pending.pop()
        try:
            metadata = os.lstat(directory)
            if max(metadata.st_mtime, metadata.st_ctime) >= since:
                return True  # a created, renamed or deleted child
            with os.scandir(directory) as entries:
                for entry in entries:
                    visited += 1
                    if visited > STARTUP_WALK_MAX_ENTRIES or time.monotonic() > deadline:
                        return True
                    if entry.is_dir(follow_symlinks=False):
                        if entry.name not in STARTUP_WALK_SKIP:
                            pending.append(entry.path)
                        continue
                    metadata = entry.stat(follow_symlinks=False)
                    if max(metadata.st_mtime, metadata.st_ctime) >= since:
                        return True
        except OSError:
            return True
    return False

def guard_writer_retry(record: dict[str, Any], start: tuple[Any, ...] | None, cwd: str) -> None:
    """Retry a silent or rate-limited writer only over a provably untouched worktree."""
    if record.get("status") not in {"startup_timeout", "rate_limited"}:
        return
    state, launched = start or (None, 0.0)
    if (state is not None and not state[1] and writer_worktree_state(cwd) == state
            and not modified_since(cwd, launched - 1.0)):
        return
    record["retryable"] = False
    record["fix"] = ("inspect the worktree: it changed or was dirty when the writer stopped; "
                     "then dispatch again on another model or adapter")

WORKTREE_WRITER_LOCK = "provenant-dispatch-writer.lock"
WORKTREE_WRITE_ADAPTERS = ("claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot")

class WorktreeLeaseError(ValueError):
    """The requested writer worktree cannot be owned exclusively."""

def _git_path(worktree: Path, which: str) -> Path | None:
    """Resolve one absolute Git path for a worktree, or None when it is not one."""
    try:
        completed = subprocess.run(
            ["git", "-C", str(worktree), "rev-parse", "--path-format=absolute", which],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    value = completed.stdout.strip()
    return Path(value) if value else None

def resolve_writer_worktree(worktree: Path) -> Path:
    """Return the resolved root of a Git worktree the caller may write inside."""
    try:
        resolved = worktree.expanduser().resolve(strict=True)
    except OSError as exc:
        raise WorktreeLeaseError(f"worktree is not a readable directory: {worktree}") from exc
    if not resolved.is_dir():
        raise WorktreeLeaseError(f"worktree is not a directory: {resolved}")
    top = _git_path(resolved, "--show-toplevel")
    if top is None:
        raise WorktreeLeaseError(f"worktree is not inside a Git repository: {resolved}")
    try:
        if top.resolve() != resolved:
            raise WorktreeLeaseError(f"worktree must be the root of a Git worktree: {resolved}")
    except OSError as exc:
        raise WorktreeLeaseError(f"worktree root could not be resolved: {resolved}") from exc
    try:
        listing = subprocess.run(["git", "-C", str(resolved), "worktree", "list", "--porcelain", "-z"],
                                 env={key: value for key, value in os.environ.items() if not key.startswith("GIT_")}, capture_output=True, timeout=3, check=True)
        roots = [Path(os.fsdecode(field[9:])).resolve() for field in listing.stdout.split(b"\0") if field.startswith(b"worktree ")]
        if resolved not in roots:
            raise WorktreeLeaseError("worktree must be registered with Git")
    except (OSError, subprocess.SubprocessError) as exc:
        raise WorktreeLeaseError("cannot verify registered Git worktree") from exc
    git_dir = _git_path(resolved, "--absolute-git-dir")
    common_dir = _git_path(resolved, "--git-common-dir")
    if git_dir is None or common_dir is None or git_dir.resolve() == common_dir.resolve():
        raise WorktreeLeaseError("worktree_write requires a linked worktree; fix: create a linked worktree")
    return resolved

def branch_name_warning(worktree: Path) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(worktree), "symbolic-ref", "--quiet", "--short", "HEAD"],
        env={key: value for key, value in os.environ.items() if not key.startswith("GIT_")},
        capture_output=True, text=True, timeout=3,
    )
    branch = result.stdout.strip() if result.returncode == 0 else "(detached HEAD)"
    if len(branch) <= 48 and BRANCH_PATTERN.fullmatch(branch):
        return None
    return f"branch {branch} is outside <type>/<area>-<slug> (or land/, proto/); rename for repository convention"

def acquire_worktree_lease(worktree: Path):
    """Hold the one-writer lease for a worktree, or refuse a concurrent writer."""
    git_dir = _git_path(worktree, "--git-dir")
    if git_dir is None or not git_dir.is_dir():
        raise WorktreeLeaseError(f"worktree has no usable Git directory: {worktree}")
    lock_path = git_dir / WORKTREE_WRITER_LOCK
    try:
        handle = lock_path.open("a+")
    except OSError as exc:
        raise WorktreeLeaseError(f"cannot open the worktree writer lease: {exc}") from exc
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as exc:
        handle.close()
        raise WorktreeLeaseError(f"another writer already owns this worktree: {worktree}") from exc
    return handle

GRAPH_RESERVATION_FILE = "provenant-graph-reservation.json"

def graph_reservation_refusal(worktree: Path) -> str | None:
    """Refuse a writer into a worktree that a running Fabric graph has reserved.

    The graph owner writes the reservation file and passes its token to its own
    writer lanes. Any other writer is refused while the reserving graph is
    running; a file whose graph has ended is stale. Unreadable state refuses.
    """
    git_dir = _git_path(worktree, "--absolute-git-dir")
    if git_dir is None:
        return None
    path = git_dir / GRAPH_RESERVATION_FILE
    try:
        reservation = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, ValueError):
        return f"the graph reservation {path} is unreadable; fix: check it, or remove it if no graph uses this worktree"
    token = reservation.get("token") if isinstance(reservation, dict) else None
    if not isinstance(token, str) or not token:
        return f"the graph reservation {path} names no token; fix: check it, or remove it if no graph uses this worktree"
    if os.environ.get("PROVENANT_GRAPH_RESERVATION") == token:
        return None
    graph, node = reservation.get("graph_id", "?"), reservation.get("node_id", "?")
    state = Path(os.environ.get("AGENT_FABRIC_STATE_DIRECTORY", str(Path.home() / ".local/state/agent-harness/fabric")))
    try:
        connection = sqlite3.connect(f"{(state / 'fabric.sqlite3').resolve().as_uri()}?mode=ro", uri=True, timeout=5)
        try:
            row = connection.execute(
                "SELECT g.state FROM graph_worktree_reservations r JOIN graphs g"
                " ON g.project = r.project AND g.graph_id = r.graph_id WHERE r.token = ?", (token,)).fetchone()
        finally:
            connection.close()
    except sqlite3.Error:
        return (f"worktree is reserved by graph {graph} node {node} and the Fabric store cannot confirm it ended;"
                f" fix: run runtime/fabric/bin/fabric graph status {graph}")
    if row is None or row[0] != "running":
        return None
    return f"worktree is reserved by graph {graph} node {node}; fix: wait for it, or run runtime/fabric/bin/fabric graph cancel {graph}"

def release_worktree_lease(handle) -> None:
    if handle is None:
        return
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    finally:
        handle.close()
