"""Component-wise no-follow filesystem operations for bounded evidence paths."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import stat
from typing import Callable


class NoFollowError(OSError):
    """A path could not be safely traversed without following a link."""


def _lexical_absolute(path: Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def _reject_parent_components(path: Path) -> None:
    if ".." in path.parts:
        raise NoFollowError(f"path contains a parent traversal: {path}")


def relative_path(root: Path, value: Path | str, *, allow_absolute: bool = False) -> Path:
    """Return a lexical path relative to root without resolving any symlink."""
    root_absolute = _lexical_absolute(root)
    candidate = Path(value)
    _reject_parent_components(candidate)
    if candidate.is_absolute():
        if not allow_absolute:
            raise NoFollowError("absolute path is not allowed")
        candidate_absolute = _lexical_absolute(candidate)
        try:
            return candidate_absolute.relative_to(root_absolute)
        except ValueError as exc:
            raise NoFollowError("path escapes the run directory") from exc
    return candidate


def bound_path(root: Path, value: Path | str, *, allow_absolute: bool = False) -> Path:
    return _lexical_absolute(root) / relative_path(root, value, allow_absolute=allow_absolute)


def lexical_path(path: Path) -> Path:
    """Normalise only dot segments; never resolve a symlink."""
    return _lexical_absolute(path)


def _open_flags(flags: int) -> int:
    return (
        flags
        | getattr(os, "O_NONBLOCK", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )


def _directory_flags() -> int:
    return (
        os.O_RDONLY
        | getattr(os, "O_NONBLOCK", 0)
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )


def _open_directory(path: Path) -> int:
    """Bind a trusted directory after canonicalising its host path once."""

    canonical = Path(os.path.realpath(os.fspath(path)))
    components = list(canonical.parts[1:])
    fd = os.open(canonical.anchor or os.sep, _directory_flags())
    try:
        for component in components:
            next_fd = os.open(component, _directory_flags(), dir_fd=fd)
            os.close(fd)
            fd = next_fd
        return fd
    except Exception:
        os.close(fd)
        raise


def _walk_from(root_fd: int, relative: Path) -> int:
    _reject_parent_components(relative)
    fd = os.dup(root_fd)
    try:
        for component in relative.parts:
            next_fd = os.open(component, _directory_flags(), dir_fd=fd)
            os.close(fd)
            fd = next_fd
        return fd
    except Exception:
        os.close(fd)
        raise


@dataclass
class OpenedFile:
    fd: int
    parent_fd: int
    name: str
    path: Path

    def close(self) -> None:
        os.close(self.fd)
        os.close(self.parent_fd)


def _path_parts(path: Path, root: Path | None, allow_absolute: bool) -> tuple[Path, Path, Path]:
    if root is not None:
        relative = relative_path(root, path, allow_absolute=allow_absolute)
        return root, relative, bound_path(root, path, allow_absolute=allow_absolute)
    absolute = _lexical_absolute(path)
    _reject_parent_components(Path(path))
    if not absolute.name:
        raise NoFollowError(f"path has no file component: {path}")
    return absolute.parent, Path(absolute.name), absolute


def _open_relative(
    root_fd: int,
    relative: Path,
    logical_path: Path,
    *,
    flags: int = os.O_RDONLY,
) -> OpenedFile:
    if not relative.parts:
        raise NoFollowError(f"path has no file component: {logical_path}")
    parent_fd = _walk_from(root_fd, Path(*relative.parts[:-1]))
    name = relative.parts[-1]
    try:
        fd = os.open(name, _open_flags(flags), dir_fd=parent_fd)
    except Exception:
        os.close(parent_fd)
        raise
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        os.close(parent_fd)
        raise NoFollowError(f"{logical_path} is not a regular file")
    return OpenedFile(fd, parent_fd, name, logical_path)


def open_existing(
    path: Path | str,
    *,
    root: Path | None = None,
    allow_absolute: bool = False,
    flags: int = os.O_RDONLY,
) -> OpenedFile:
    """Open a regular evidence path after component-wise no-follow traversal."""
    anchor, relative, logical_path = _path_parts(Path(path), root, allow_absolute)
    bound_fd = _open_directory(anchor)
    try:
        return _open_relative(bound_fd, relative, logical_path, flags=flags)
    finally:
        os.close(bound_fd)


def open_parent(
    path: Path | str,
    *,
    root: Path | None = None,
    allow_absolute: bool = True,
) -> tuple[int, str, Path]:
    """Retain the safely traversed parent directory for an atomic operation."""
    anchor, relative, logical_path = _path_parts(Path(path), root, allow_absolute)
    if not relative.parts:
        raise NoFollowError(f"path has no file component: {path}")
    bound_fd = _open_directory(anchor)
    try:
        parent_fd = _walk_from(bound_fd, Path(*relative.parts[:-1]))
    finally:
        os.close(bound_fd)
    return parent_fd, relative.parts[-1], logical_path


def read_file(
    path: Path | str,
    label: str,
    *,
    root: Path | None = None,
    allow_absolute: bool = True,
    after_read: Callable[[Path, str], None] | None = None,
) -> tuple[bytes, tuple[int, int], Path]:
    """Read through one retained descriptor and re-open the path component-wise."""
    anchor, relative, logical_path = _path_parts(Path(path), root, allow_absolute)
    bound_fd = _open_directory(anchor)
    opened: OpenedFile | None = None
    try:
        opened = _open_relative(bound_fd, relative, logical_path)
        before = os.fstat(opened.fd)
        if not stat.S_ISREG(before.st_mode):
            raise NoFollowError(f"{label} is not a regular file")
        if before.st_nlink != 1:
            raise NoFollowError(f"{label} is hardlinked")
        chunks: list[bytes] = []
        while True:
            chunk = os.read(opened.fd, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
        after = os.fstat(opened.fd)
        identity = (before.st_dev, before.st_ino)
        if identity != (after.st_dev, after.st_ino) or before.st_nlink != after.st_nlink:
            raise NoFollowError(f"{label} changed while it was read")
        data = b"".join(chunks)
        if after_read is not None:
            after_read(opened.path, label)
        if root is None:
            fresh = open_existing(logical_path, allow_absolute=True)
        else:
            fresh = open_existing(logical_path, root=root, allow_absolute=True)
        try:
            current = os.fstat(fresh.fd)
            if not stat.S_ISREG(current.st_mode):
                raise NoFollowError(f"{label} is not a regular file after it was read")
            if current.st_nlink != 1:
                raise NoFollowError(f"{label} became hardlinked")
            if (current.st_dev, current.st_ino) != identity:
                raise NoFollowError(f"{label} inode changed after it was read")
        finally:
            fresh.close()
        return data, identity, opened.path
    finally:
        if opened is not None:
            opened.close()
        os.close(bound_fd)


def identity(
    path: Path | str,
    *,
    root: Path | None = None,
    allow_missing: bool = False,
) -> tuple[int, int] | None:
    opened: OpenedFile | None = None
    try:
        opened = open_existing(path, root=root, allow_absolute=True)
        value = os.fstat(opened.fd)
        if not stat.S_ISREG(value.st_mode):
            raise NoFollowError(f"{path} is not a regular file")
        if value.st_nlink != 1:
            raise NoFollowError(f"{path} is hardlinked")
        return value.st_dev, value.st_ino
    except FileNotFoundError:
        if allow_missing:
            return None
        raise
    finally:
        if opened is not None:
            opened.close()
