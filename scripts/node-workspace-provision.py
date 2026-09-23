#!/usr/bin/env python3
"""Provision missing npm dependencies only when running in a linked worktree."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys

from worktree import cow_clone, node_modules_preflight_passes


def checkout_root() -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], check=True,
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True,
    )
    return Path(result.stdout.strip()).resolve()


def primary_checkout(root: Path) -> Path:
    result = subprocess.run(
        ["git", "worktree", "list", "--porcelain"], cwd=root, check=True,
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True,
    )
    first = result.stdout.splitlines()[0]
    if not first.startswith("worktree "):
        raise RuntimeError("git did not report the primary checkout")
    return Path(first.removeprefix("worktree ")).resolve()


def clone_dependencies(primary: Path, worktree: Path) -> bool:
    primary_lock = primary / "package-lock.json"
    worktree_lock = worktree / "package-lock.json"
    if (
        (primary / "node_modules").is_symlink()
        or not (primary / "node_modules").is_dir()
        or not node_modules_preflight_passes(primary)
        or not primary_lock.is_file()
        or not worktree_lock.is_file()
        or primary_lock.read_bytes() != worktree_lock.read_bytes()
    ):
        return False

    cloned: list[Path] = []
    sources = [primary / "node_modules", *sorted((primary / "runtime").glob("*/node_modules"))]
    try:
        for source in sources:
            if source.is_symlink() or not source.is_dir():
                continue
            target = worktree / source.relative_to(primary)
            target.parent.mkdir(parents=True, exist_ok=True)
            cloned.append(target)
            cow_clone(source, target)
    except OSError:
        for path in reversed(cloned):
            if path.is_symlink():
                path.unlink()
            elif path.is_dir():
                shutil.rmtree(path)
        return False
    return True


def main() -> int:
    try:
        worktree = checkout_root()
        primary = primary_checkout(worktree)
    except (OSError, subprocess.CalledProcessError, RuntimeError) as exc:
        print(f"node-workspace-provision: cannot identify checkout: {exc}", file=sys.stderr)
        return 2

    if worktree == primary or node_modules_preflight_passes(worktree):
        return 0
    if clone_dependencies(primary, worktree):
        print("node-workspace-provision: cloned node_modules from primary checkout")
        return 0

    result = subprocess.run(
        ["npm", "ci", "--prefer-offline", "--no-audit", "--no-fund"],
        cwd=worktree, env=os.environ.copy(), check=False,
    )
    if result.returncode != 0:
        return result.returncode
    print("node-workspace-provision: ran npm ci in linked worktree")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
