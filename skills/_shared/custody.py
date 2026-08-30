"""Small, fail-closed helpers for run-owned regular files."""

from __future__ import annotations

import os
import stat
from pathlib import Path


class OwnedFileError(ValueError):
    """A run-owned file is missing, redirected, or not a single-link regular file."""


class OwnedLinkError(OwnedFileError):
    """An owned path resolves to an inode with more than one hard link."""


def contained_regular_path(root: Path, value: str | Path, label: str = "file") -> tuple[str, Path]:
    """Validate and return a run-relative regular single-link file path."""
    root = root.resolve()
    raw = Path(value)
    if raw.is_absolute() or ".." in raw.parts or not raw.parts:
        raise OwnedFileError(f"{label} must be a run-relative path")
    target = root.joinpath(*raw.parts)
    current = root
    try:
        root_meta = root.lstat()
    except (OSError, OwnedFileError) as exc:
        raise OwnedFileError(f"{label} run root is unavailable") from exc
    if stat.S_ISLNK(root_meta.st_mode) or not stat.S_ISDIR(root_meta.st_mode):
        raise OwnedFileError(f"{label} run root is not a real directory")
    for part in raw.parts:
        current /= part
        try:
            metadata = current.lstat()
        except OSError as exc:
            raise OwnedFileError(f"{label} is unavailable: {raw.as_posix()}") from exc
        if stat.S_ISLNK(metadata.st_mode):
            raise OwnedFileError(f"{label} must not use a symlink: {raw.as_posix()}")
    try:
        metadata = target.lstat()
        target.resolve(strict=True).relative_to(root)
    except (OSError, ValueError) as exc:
        raise OwnedFileError(f"{label} is outside the run: {raw.as_posix()}") from exc
    if not stat.S_ISREG(metadata.st_mode):
        raise OwnedFileError(f"{label} must be a regular single-link file: {raw.as_posix()}")
    if metadata.st_nlink != 1:
        raise OwnedLinkError(f"{label} must be a regular single-link file: {raw.as_posix()}")
    return raw.as_posix(), target


def open_contained_regular(
    root: Path, value: str | Path, flags: int, *, mode: int = 0o600, label: str = "file"
) -> tuple[int, str, Path]:
    """Open an owned file once and bind the returned descriptor to its inode."""
    root = root.resolve()
    raw = Path(value)
    if raw.is_absolute() or ".." in raw.parts or not raw.parts:
        raise OwnedFileError(f"{label} must be a run-relative path")
    relative = raw.as_posix()
    target = root.joinpath(*raw.parts)
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    directory = getattr(os, "O_DIRECTORY", 0)
    root_fd = directory_fd = fd = -1
    try:
        root_fd = os.open(root, os.O_RDONLY | nofollow | directory)
        directory_fd = root_fd
        for part in raw.parts[:-1]:
            next_fd = os.open(part, os.O_RDONLY | nofollow | directory, dir_fd=directory_fd)
            if not stat.S_ISDIR(os.fstat(next_fd).st_mode):
                os.close(next_fd)
                raise OwnedFileError(f"{label} path contains a non-directory component: {relative}")
            if directory_fd != root_fd:
                os.close(directory_fd)
            directory_fd = next_fd
        fd = os.open(raw.parts[-1], flags | nofollow, mode, dir_fd=directory_fd)
        bound = os.fstat(fd)
    except (OSError, OwnedFileError) as exc:
        if fd >= 0:
            os.close(fd)
        if directory_fd >= 0 and directory_fd != root_fd:
            os.close(directory_fd)
        if root_fd >= 0:
            os.close(root_fd)
        if isinstance(exc, OwnedFileError):
            raise
        raise OwnedFileError(f"{label} cannot be opened safely: {relative}") from exc
    if not stat.S_ISREG(bound.st_mode):
        os.close(fd)
        if directory_fd >= 0 and directory_fd != root_fd:
            os.close(directory_fd)
        if root_fd >= 0:
            os.close(root_fd)
        raise OwnedFileError(f"{label} changed while being opened: {relative}")
    if bound.st_nlink != 1:
        os.close(fd)
        if directory_fd >= 0 and directory_fd != root_fd:
            os.close(directory_fd)
        if root_fd >= 0:
            os.close(root_fd)
        raise OwnedLinkError(f"{label} must be a regular single-link file: {relative}")
    if directory_fd >= 0 and directory_fd != root_fd:
        os.close(directory_fd)
    if root_fd >= 0:
        os.close(root_fd)
    return fd, relative, target


def ensure_contained_directory(
    root: Path,
    value: str | Path,
    *,
    mode: int = 0o700,
    label: str = "directory",
    final_must_be_new: bool = False,
) -> None:
    """Create or open each directory component relative to a bound parent fd."""
    root = root.resolve()
    raw = Path(value)
    if raw.is_absolute() or ".." in raw.parts or not raw.parts:
        raise OwnedFileError(f"{label} must be a run-relative path")
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    directory = getattr(os, "O_DIRECTORY", 0)
    root_fd = directory_fd = -1
    try:
        root_fd = os.open(root, os.O_RDONLY | nofollow | directory)
        directory_fd = root_fd
        for index, part in enumerate(raw.parts):
            try:
                os.mkdir(part, mode, dir_fd=directory_fd)
            except FileExistsError:
                if final_must_be_new and index == len(raw.parts) - 1:
                    raise OwnedFileError(f"{label} already exists: {raw.as_posix()}")
            next_fd = os.open(part, os.O_RDONLY | nofollow | directory, dir_fd=directory_fd)
            if not stat.S_ISDIR(os.fstat(next_fd).st_mode):
                os.close(next_fd)
                raise OwnedFileError(f"{label} path contains a non-directory component: {raw.as_posix()}")
            if directory_fd != root_fd:
                os.close(directory_fd)
            directory_fd = next_fd
    except (OSError, OwnedFileError) as exc:
        if directory_fd >= 0 and directory_fd != root_fd:
            os.close(directory_fd)
        if root_fd >= 0:
            os.close(root_fd)
        if isinstance(exc, OwnedFileError):
            raise
        raise OwnedFileError(f"{label} cannot be created safely: {raw.as_posix()}") from exc
    if directory_fd >= 0 and directory_fd != root_fd:
        os.close(directory_fd)
    if root_fd >= 0:
        os.close(root_fd)


def create_contained_directory(root: Path, value: str | Path, *, mode: int = 0o700, label: str = "directory") -> None:
    """Create a new contained directory, binding its parent by descriptors."""
    ensure_contained_directory(root, value, mode=mode, label=label, final_must_be_new=True)


def read_bound_bytes(root: Path, value: str | Path, *, label: str = "file") -> bytes:
    """Read one checked inode without a second pathname open."""
    return read_contained_regular(root, value, label=label)[2]


def read_contained_regular(
    root: Path, value: str | Path, *, label: str = "file", max_bytes: int | None = None
) -> tuple[str, Path, bytes]:
    """Read one contained regular file from its already-bound descriptor."""
    fd, relative, target = open_contained_regular(root, value, os.O_RDONLY, label=label)
    try:
        chunks: list[bytes] = []
        total = 0
        while True:
            read_size = 1024 * 1024
            if max_bytes is not None:
                read_size = min(read_size, max_bytes + 1 - total)
                if read_size <= 0:
                    raise OwnedFileError(f"{label} exceeds the {max_bytes}-byte limit: {relative}")
            chunk = os.read(fd, read_size)
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if max_bytes is not None and total > max_bytes:
                raise OwnedFileError(f"{label} exceeds the {max_bytes}-byte limit: {relative}")
        return relative, target, b"".join(chunks)
    finally:
        os.close(fd)
