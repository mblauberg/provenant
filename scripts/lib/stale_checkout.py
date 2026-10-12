"""Warn (never block, never fetch) when the product checkout is stale.

Callers pass the product root. Set PROVENANT_STALE_CHECKED=1 to opt out.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import time

FETCH_MAX_AGE_SECONDS = 3 * 86400


def _git(root: Path, *args: str) -> list[str] | None:
    """Stdout lines of a local git query, or None on any failure."""
    try:
        from worktree import git_environment  # trusted PATH, no inherited GIT_DIR redirects

        env = git_environment()
        done = subprocess.run(
            ["git", "-C", str(root), *args], text=True, capture_output=True,
            stdin=subprocess.DEVNULL, timeout=1, check=False, env=env,
        )
    except (ImportError, OSError, subprocess.SubprocessError):
        return None
    return done.stdout.splitlines() if done.returncode == 0 else None


def check_stale_checkout(root: Path, *, now: float | None = None) -> list[str]:
    """Warning lines for a stale checkout at root; empty when fine or unknowable."""
    paths = _git(root, "rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir", "--git-path", "FETCH_HEAD")
    if paths is None or len(paths) != 3 or paths[0] != paths[1]:
        return []  # not a repo, git missing or failing, or a linked worktree
    lines = []
    try:
        age = (time.time() if now is None else now) - Path(paths[2]).stat().st_mtime
        if age > FETCH_MAX_AGE_SECONDS:
            lines.append(f"warning: origin last fetched {int(age // 86400)}d ago; run: git -C {root} fetch")
    except OSError:
        pass  # never fetched is fine
    head = _git(root, "symbolic-ref", "--short", "refs/remotes/origin/HEAD")
    default = head[0].removeprefix("origin/") if head else "main"
    counts = _git(root, "rev-list", "--left-right", "--count", f"HEAD...origin/{default}")
    try:
        ahead, behind = map(int, counts[0].split()) if counts else (0, 0)
    except ValueError:
        return lines
    if behind and ahead:
        lines.append(
            f"warning: product checkout diverged from origin/{default} ({ahead} ahead, {behind} behind); "
            f"run: git -C {root} rebase origin/{default} (or merge)"
        )
    elif behind:
        lines.append(
            f"warning: product checkout is {behind} commits behind origin/{default}; "
            f"run: git -C {root} pull --ff-only"
        )
    return lines


def warn_if_stale_checkout(root: Path, *, now: float | None = None) -> list[str]:
    """Print warnings to stderr once per process tree (PROVENANT_STALE_CHECKED) and return them."""
    if os.environ.get("PROVENANT_STALE_CHECKED") == "1":
        return []
    os.environ["PROVENANT_STALE_CHECKED"] = "1"
    lines = check_stale_checkout(root, now=now)
    for line in lines:
        print(line, file=sys.stderr)
    return lines
