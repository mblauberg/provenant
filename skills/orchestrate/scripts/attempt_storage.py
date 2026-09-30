"""Bulky per-attempt storage: honest size accounting and private-tmp pruning."""

from __future__ import annotations

import os
from pathlib import Path
import shutil

# A failed attempt keeps a small private tmp for diagnosis; anything larger is pruned.
KEEP_FAILED_TMP_BYTES = 50 * 1024 * 1024
KEEP_ENV = "PROVENANT_KEEP_ATTEMPT_TMP"


def tree_bytes(path: Path) -> int:
    """Disk usage of a tree: allocated blocks, hard links once, links never followed."""
    seen: set[tuple[int, int]] = set()

    def one(item: Path) -> int:
        try:
            stat = item.lstat()
        except OSError:
            return 0
        if os.path.islink(item) or (stat.st_nlink > 1 and not os.path.isdir(item)
                                    and (stat.st_dev, stat.st_ino) in seen):
            return 0
        seen.add((stat.st_dev, stat.st_ino))
        return stat.st_blocks * 512

    if os.path.islink(path) or not os.path.isdir(path):
        return one(path)
    total = one(path)
    for base, directories, files in os.walk(path, followlinks=False):
        directories[:] = [name for name in directories if not os.path.islink(os.path.join(base, name))]
        for name in (*directories, *files):
            total += one(Path(base) / name)
    return total


def remove_tree(path: Path) -> bool:
    """Remove a real directory tree without following links; False when it is not one."""
    if os.path.islink(path) or not os.path.isdir(path):
        return False
    for base, directories, _files in os.walk(path, followlinks=False):
        for name in directories:
            child = os.path.join(base, name)
            if not os.path.islink(child):
                try:
                    os.chmod(child, 0o700)  # tools such as pytest leave read-only directories
                except OSError:
                    pass
    try:
        os.chmod(path, 0o700)
    except OSError:
        pass
    shutil.rmtree(path, ignore_errors=True)
    return not os.path.lexists(path)


def prune_private_tmp(attempt_dir: Path, status: str) -> int:
    """Drop a terminal attempt's private ``tmp/``; return bytes freed (0 when kept).

    Kept when explicitly requested, when the attempt awaits input (resume may
    reuse it), or when a failed attempt's tmp is small enough to inspect.
    """
    tmp = Path(attempt_dir) / "tmp"
    if os.environ.get(KEEP_ENV) or status == "input_required" or os.path.islink(tmp) or not os.path.isdir(tmp):
        return 0
    size = tree_bytes(tmp)
    if status != "ok" and size <= KEEP_FAILED_TMP_BYTES:
        return 0
    return size if remove_tree(tmp) else 0
