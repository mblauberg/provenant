#!/usr/bin/env python3
"""Provision missing npm dependencies only when running in a linked worktree."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys

from worktree import GIT_REDIRECT_ENVIRONMENT_KEYS, cow_clone, node_modules_preflight_passes


def discovery_environment() -> dict[str, str]:
    """A hook's GIT_DIR would point discovery at the primary; find this checkout from cwd."""
    environment = os.environ.copy()
    for key in GIT_REDIRECT_ENVIRONMENT_KEYS:
        environment.pop(key, None)
    return environment


def checkout_root() -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], check=True, env=discovery_environment(),
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True,
    )
    return Path(result.stdout.strip()).resolve()


def primary_checkout(root: Path) -> Path:
    result = subprocess.run(
        ["git", "worktree", "list", "--porcelain"], cwd=root, check=True, env=discovery_environment(),
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True,
    )
    first = result.stdout.splitlines()[0]
    if not first.startswith("worktree "):
        raise RuntimeError("git did not report the primary checkout")
    return Path(first.removeprefix("worktree ")).resolve()


def dependency_targets(root: Path) -> list[Path]:
    return [root / "node_modules", *sorted((root / "runtime").glob("*/node_modules"))]


def drop_borrowed_links(worktree: Path) -> None:
    """A linked node_modules belongs to another checkout; never write through it."""
    for target in dependency_targets(worktree):
        if target.is_symlink():
            target.unlink()


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
    stale: list[tuple[Path, Path]] = []
    try:
        for source in dependency_targets(primary):
            if source.is_symlink() or not source.is_dir():
                continue
            target = worktree / source.relative_to(primary)
            if target.exists():
                # Aside under .agent-run (ignored, generated), so an interrupted run leaves no residue.
                # A linked .agent-run would carry the tree out of the worktree: remove it instead.
                parking = worktree / ".agent-run"
                if parking.is_symlink():
                    shutil.rmtree(target)
                else:
                    parking.mkdir(exist_ok=True)
                    aside = parking / f"{'-'.join(target.relative_to(worktree).parts)}.stale-{os.getpid()}"
                    target.rename(aside)
                    stale.append((target, aside))
            target.parent.mkdir(parents=True, exist_ok=True)
            cloned.append(target)
            cow_clone(source, target)
    except OSError:
        for path in reversed(cloned):
            if path.is_symlink():
                path.unlink()
            elif path.is_dir():
                shutil.rmtree(path)
        for target, aside in stale:
            aside.rename(target)
        return False
    for _, aside in stale:
        shutil.rmtree(aside, ignore_errors=True)
    return True


def main() -> int:
    try:
        worktree = checkout_root()
        primary = primary_checkout(worktree)
    except (OSError, subprocess.CalledProcessError, RuntimeError) as exc:
        # Not a git checkout (an exported tree): nothing to provision from; the
        # preflight that follows still reports missing dependencies.
        print(f"node-workspace-provision: skipped, cannot identify checkout: {exc}", file=sys.stderr)
        return 0

    if worktree == primary or node_modules_preflight_passes(worktree):
        return 0
    drop_borrowed_links(worktree)
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
