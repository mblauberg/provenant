"""Throwaway git repositories with a fixed identity."""

from __future__ import annotations

import os
import subprocess
from collections.abc import Mapping
from pathlib import Path

IDENTITY = (
    "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false",
)


def git(cwd: Path, *args: str, check: bool = True, env: Mapping[str, str] | None = None) -> str:
    """Run git in ``cwd`` and return stripped stdout.

    The default environment drops inherited ``GIT_*`` variables.
    """
    if env is None:
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    result = subprocess.run(
        ["git", *IDENTITY, *args],
        cwd=cwd,
        env=dict(env),
        text=True,
        capture_output=True,
        check=False,
    )
    if check and result.returncode:
        raise subprocess.CalledProcessError(
            result.returncode, result.args, result.stdout, result.stderr
        )
    return result.stdout.strip()


def git_repo(
    path: Path,
    *,
    branch: str = "main",
    files: Mapping[str, str] | None = None,
    message: str = "initial",
    commit: bool = True,
) -> Path:
    """Create a repository with one commit (empty when ``files`` is None).

    ``commit=False`` leaves the files unstaged and uncommitted. Returns the
    resolved path.
    """
    path.mkdir(parents=True, exist_ok=True)
    git(path, "init", "-q", "-b", branch)
    git(path, "config", "user.name", "Test")
    git(path, "config", "user.email", "test@example.invalid")
    for name, text in (files or {}).items():
        target = path / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    if commit:
        git(path, "add", "-A")
        git(path, "commit", "-q", "--allow-empty", "-m", message)
    return path.resolve()


def linked_worktree(
    repo: Path, name: str, *, branch: str | None = None, path: Path | None = None
) -> Path:
    """Add a linked worktree on a new ``branch`` (default ``name``).

    It lives at ``path``, or beside ``repo`` as ``name``. Returns the resolved path.
    """
    worktree = path or repo.parent / name
    git(repo, "worktree", "add", "-q", "-b", branch or name, str(worktree))
    return worktree.resolve()
