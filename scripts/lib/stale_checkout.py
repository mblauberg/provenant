"""Warn when the Provenant product checkout is not on main or is behind upstream."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from typing import TextIO

THREE_DAYS_SECONDS = 3 * 86400


def _fetch_head_path(product_root: Path) -> Path | None:
    """Locate FETCH_HEAD in the checkout or its common directory, if present."""
    git_entry = product_root / ".git"
    if git_entry.is_dir():
        candidate = git_entry / "FETCH_HEAD"
        return candidate if candidate.is_file() else None
    if git_entry.is_file():
        try:
            content = git_entry.read_text(encoding="utf-8").strip()
            if content.startswith("gitdir:"):
                git_dir_raw = content[len("gitdir:"):].strip()
                git_dir = Path(git_dir_raw)
                if not git_dir.is_absolute():
                    git_dir = (product_root / git_dir).resolve()
                candidate = git_dir / "FETCH_HEAD"
                if candidate.is_file():
                    return candidate
                commondir_file = git_dir / "commondir"
                if commondir_file.is_file():
                    common_raw = commondir_file.read_text(encoding="utf-8").strip()
                    common_dir = Path(common_raw)
                    if not common_dir.is_absolute():
                        common_dir = (git_dir / common_dir).resolve()
                    candidate = common_dir / "FETCH_HEAD"
                    if candidate.is_file():
                        return candidate
        except OSError:
            pass
    return None


TRUSTED_TOOL_DIRECTORIES = (
    "/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin",
    "/usr/local/sbin", "/opt/local/bin", "/usr/bin", "/bin",
    "/usr/sbin", "/sbin",
)


def _trusted_git_bin() -> str:
    configured = os.environ.get("PROVENANT_GIT_BIN")
    if configured and os.path.isfile(configured) and os.access(configured, os.X_OK):
        return configured
    for candidate_dir in TRUSTED_TOOL_DIRECTORIES:
        candidate = Path(candidate_dir) / "git"
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    return shutil.which("git") or "git"


def _run_git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run a local, non-interactive git query without redirects using trusted Git."""
    env = os.environ.copy()
    for key in (
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ):
        env.pop(key, None)
    env["PATH"] = os.pathsep.join(TRUSTED_TOOL_DIRECTORIES)
    env["GIT_TERMINAL_PROMPT"] = "0"
    git_bin = _trusted_git_bin()
    try:
        return subprocess.run(
            [git_bin, "-C", str(root), *args],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
            timeout=3,
            check=False,
            env=env,
        )
    except (OSError, subprocess.TimeoutExpired):
        return subprocess.CompletedProcess(
            args=[git_bin, "-C", str(root), *args],
            returncode=1,
            stdout="",
            stderr="git query failed",
        )


def resolve_product_checkout(root: Path | None = None) -> Path | None:
    """Resolve the product checkout directory (matching `provenant root`)."""
    if root is not None:
        candidate = root.expanduser()
        return candidate.resolve() if candidate.is_dir() else candidate

    configured = os.environ.get("AGENT_FABRIC_PRODUCT_ROOT")
    if configured:
        p = Path(configured).expanduser()
        if p.is_dir():
            return p.resolve()

    instance_val = os.environ.get("AGENT_FABRIC_INSTANCE_ROOT")
    instance_root = Path(instance_val).expanduser() if instance_val else Path.home() / ".agents"
    try:
        pointer_file = instance_root / ".agent-fabric/product-root.json"
        if pointer_file.is_file():
            data = json.loads(pointer_file.read_text(encoding="utf-8"))
            if isinstance(data, dict) and isinstance(data.get("product_root"), str):
                target = Path(data["product_root"]).expanduser()
                if target.is_dir():
                    return target.resolve()
    except Exception:
        pass

    agents_home = os.environ.get("AGENTS_HOME")
    if agents_home:
        p = Path(agents_home).expanduser()
        if p.is_dir():
            return p.resolve()

    default_root = Path(__file__).resolve().parents[2]
    if default_root.is_dir():
        return default_root.resolve()
    return None


def check_stale_checkout(product_root: Path | None = None, *, now: float | None = None) -> str | None:
    """Return a single warning line if the product checkout is not on main or behind upstream."""
    target_root = resolve_product_checkout(product_root)
    if target_root is None or not target_root.is_dir():
        return None

    # Verify target is inside a git repository
    is_git = _run_git(target_root, "rev-parse", "--is-inside-work-tree")
    if is_git.returncode != 0:
        return None

    # Check branch
    branch_query = _run_git(target_root, "symbolic-ref", "--quiet", "--short", "HEAD")
    if branch_query.returncode != 0:
        # Detached HEAD or unborn branch
        head_query = _run_git(target_root, "rev-parse", "--verify", "HEAD")
        if head_query.returncode == 0:
            return f"warning: product checkout is not on main (detached HEAD); run: git -C {target_root} checkout main"
        return None

    branch = branch_query.stdout.strip()
    if branch != "main":
        return f"warning: product checkout is on branch {branch} (expected main); run: git -C {target_root} checkout main"

    # Branch is main; check if behind upstream
    rev_list = _run_git(target_root, "rev-list", "--count", "HEAD..@{u}")
    if rev_list.returncode != 0:
        return None

    try:
        count = int(rev_list.stdout.strip())
    except ValueError:
        return None

    if count <= 0:
        return None

    # Resolve upstream ref name
    upstream_query = _run_git(target_root, "rev-parse", "--abbrev-ref", "@{u}")
    upstream = (
        upstream_query.stdout.strip()
        if upstream_query.returncode == 0 and upstream_query.stdout.strip()
        else "origin/main"
    )

    # Check FETCH_HEAD age
    fetch_info = ""
    fetch_head = _fetch_head_path(target_root)
    if fetch_head is not None:
        try:
            mtime = fetch_head.stat().st_mtime
            current_time = time.time() if now is None else now
            age_seconds = current_time - mtime
            if age_seconds > THREE_DAYS_SECONDS:
                days = int(age_seconds // 86400)
                fetch_info = f" (last fetched {days}d ago)"
        except OSError:
            pass

    commit_word = "commits" if count != 1 else "commit"
    return (
        f"warning: product checkout is {count} {commit_word} behind {upstream}{fetch_info}; "
        f"run: git -C {target_root} pull --ff-only"
    )


def warn_if_stale_checkout(
    product_root: Path | None = None,
    *,
    now: float | None = None,
    stream: TextIO | None = None,
) -> str | None:
    """Print the warning line to stream (stderr by default) if stale, returning the line."""
    warning = check_stale_checkout(product_root, now=now)
    if warning:
        target_stream = sys.stderr if stream is None else stream
        print(warning, file=target_stream)
    return warning


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check for stale product checkout.")
    parser.add_argument("product_root", nargs="?", type=Path, help="product checkout directory")
    args = parser.parse_args(argv)
    warn_if_stale_checkout(args.product_root)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
