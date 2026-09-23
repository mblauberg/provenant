"""Repository-local ignore rules for generated run and worktree directories."""

from __future__ import annotations

from pathlib import Path


def write_exclude_rules(common_git_dir: Path, *rules: str) -> None:
    """Append missing anchored rules without changing tracked project files."""
    if not common_git_dir.is_dir() or common_git_dir.is_symlink():
        raise ValueError("expected a real common Git directory")
    exclude = common_git_dir / "info" / "exclude"
    if exclude.parent.is_symlink() or exclude.is_symlink():
        raise ValueError("Git exclude path must not be a symlink")
    exclude.parent.mkdir(parents=True, exist_ok=True)
    existing = exclude.read_text(errors="replace").splitlines() if exclude.exists() else []
    missing = [rule for rule in rules if rule not in existing]
    if not missing:
        return
    with exclude.open("a") as stream:
        if exclude.exists() and exclude.stat().st_size and not exclude.read_bytes().endswith(b"\n"):
            stream.write("\n")
        for rule in missing:
            stream.write(rule + "\n")
