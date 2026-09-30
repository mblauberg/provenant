"""Bulky per-attempt storage: honest size accounting and private-tmp pruning."""

from __future__ import annotations

import os
from pathlib import Path
import stat

# A failed attempt keeps a small private tmp for diagnosis; anything larger is pruned.
KEEP_FAILED_TMP_BYTES = 50 * 1024 * 1024
KEEP_ENV = "PROVENANT_KEEP_ATTEMPT_TMP"


def tree_bytes(path: Path) -> int:
    """Disk usage of a tree: allocated blocks, hard links once, links never followed."""
    seen: set[tuple[int, int]] = set()

    def one(item: Path) -> int:
        try:
            info = item.lstat()
        except OSError:
            return 0
        if os.path.islink(item) or (info.st_nlink > 1 and not os.path.isdir(item)
                                    and (info.st_dev, info.st_ino) in seen):
            return 0
        seen.add((info.st_dev, info.st_ino))
        return info.st_blocks * 512 if hasattr(info, "st_blocks") else info.st_size

    if os.path.islink(path) or not os.path.isdir(path):
        return one(path)
    total = one(path)
    for base, directories, files in os.walk(path, followlinks=False):
        directories[:] = [name for name in directories if not os.path.islink(os.path.join(base, name))]
        for name in (*directories, *files):
            total += one(Path(base) / name)
    return total


def _open_dir(name: str, parent_fd: int, dev: int) -> int:
    """Open a real directory on ``dev`` and prove it is the one just examined."""
    before = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent_fd)
    try:
        after = os.fstat(fd)
        if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino) or after.st_dev != dev:
            raise OSError(f"directory changed or crosses a device: {name}")
    except BaseException:
        os.close(fd)
        raise
    return fd


def _rm_at(parent_fd: int, name: str, dev: int, rel: str, left: list[str]) -> None:
    """Delete relative to an open directory: links are unlinked, never entered."""
    info = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    if not stat.S_ISDIR(info.st_mode):
        os.unlink(name, dir_fd=parent_fd)
        return
    if info.st_dev != dev:  # a mount point: leave it and everything under it
        left.append(rel)
        return
    fd = _open_dir(name, parent_fd, dev)
    try:
        try:
            os.fchmod(fd, 0o700)  # tools such as pytest leave read-only directories
        except OSError:
            pass
        for entry in os.listdir(fd):
            try:
                _rm_at(fd, entry, dev, f"{rel}/{entry}", left)
            except OSError:
                left.append(f"{rel}/{entry}")
    finally:
        os.close(fd)
    if not any(item == rel or item.startswith(rel + "/") for item in left):
        os.rmdir(name, dir_fd=parent_fd)


def remove_tree(anchor: Path, path: Path) -> list[str]:
    """Remove ``path`` (below the trusted ``anchor``) without following any link.

    Every component is opened with O_NOFOLLOW from the anchor, so a swapped
    ancestor cannot redirect the delete. Returns the paths left in place (mount
    points, refusals, errors); an empty list means the tree is gone. Every
    component must be on the anchor's device. Unsupported layout: a same-device
    bind mount is indistinguishable from a directory and is not detected.
    """
    try:
        parts = Path(path).relative_to(anchor).parts
    except ValueError:
        return [str(path)]
    if not parts or os.rmdir not in os.supports_dir_fd or os.listdir not in os.supports_fd:
        return [str(path)]
    left: list[str] = []
    fds: list[int] = []
    try:
        fds.append(os.open(anchor, os.O_RDONLY | os.O_DIRECTORY))
        dev = os.fstat(fds[0]).st_dev
        for part in parts[:-1]:
            fds.append(_open_dir(part, fds[-1], dev))
        if not stat.S_ISDIR(os.stat(parts[-1], dir_fd=fds[-1], follow_symlinks=False).st_mode):
            return [str(path)]
        _rm_at(fds[-1], parts[-1], dev, str(path), left)
    except FileNotFoundError:
        pass
    except OSError:
        left.append(str(path))
    finally:
        for fd in fds:
            os.close(fd)
    return left


def prune_private_tmp(run_dir: Path, attempt_dir: Path, status: str) -> tuple[int, list[str]]:
    """Drop a terminal attempt's private ``tmp/``; return (bytes freed, paths left).

    Kept when explicitly requested, when the attempt awaits input (resume may
    reuse it), or when a failed attempt's tmp is small enough to inspect.
    """
    tmp = Path(attempt_dir) / "tmp"
    if os.environ.get(KEEP_ENV) or status == "input_required" or os.path.islink(tmp) or not os.path.isdir(tmp):
        return 0, []
    size = tree_bytes(tmp)
    if status != "ok" and size <= KEEP_FAILED_TMP_BYTES:
        return 0, []
    left = remove_tree(run_dir, tmp)
    return (0 if left else size), left
