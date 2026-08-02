#!/usr/bin/env python3
"""Small no-follow publisher/validator for cf_dispatch evidence files."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import stat
import sys
import tempfile
import time


class PublicationError(RuntimeError):
    """The named evidence file could not be published or verified."""


def _identity(value: os.stat_result) -> tuple[int, int]:
    return value.st_dev, value.st_ino


def _open_flags() -> int:
    return os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)


def _read_fd(fd: int, label: str) -> tuple[bytes, tuple[int, int]]:
    before = os.fstat(fd)
    if not stat.S_ISREG(before.st_mode):
        raise PublicationError(f"{label} is not a regular file")
    if before.st_nlink != 1:
        raise PublicationError(f"{label} is hardlinked")
    chunks: list[bytes] = []
    while True:
        chunk = os.read(fd, 1024 * 1024)
        if not chunk:
            break
        chunks.append(chunk)
    after = os.fstat(fd)
    if _identity(before) != _identity(after) or before.st_nlink != after.st_nlink:
        raise PublicationError(f"{label} changed while it was read")
    return b"".join(chunks), _identity(before)


def _open_and_read(path: Path, label: str) -> tuple[int, bytes, tuple[int, int]]:
    try:
        fd = os.open(path, _open_flags())
    except OSError as exc:
        raise PublicationError(f"{label} cannot be opened without following links: {exc}") from exc
    try:
        data, identity = _read_fd(fd, label)
        return fd, data, identity
    except Exception:
        os.close(fd)
        raise


def _path_matches_fd(path: Path, fd: int, identity: tuple[int, int], label: str) -> None:
    try:
        current = os.stat(path, follow_symlinks=False)
    except OSError as exc:
        raise PublicationError(f"{label} disappeared after publication: {exc}") from exc
    if stat.S_ISLNK(current.st_mode):
        raise PublicationError(f"{label} became a symlink")
    if not stat.S_ISREG(current.st_mode):
        raise PublicationError(f"{label} is not a regular file after publication")
    if current.st_nlink != 1:
        raise PublicationError(f"{label} became hardlinked")
    if _identity(current) != identity or _identity(os.fstat(fd)) != identity:
        raise PublicationError(f"{label} inode changed after publication")


def _barrier(path: Path, phase: str) -> None:
    """Optional deterministic test barrier; inactive for normal dispatches."""

    barrier_dir = os.environ.get("CF_DISPATCH_TEST_BARRIER_DIR")
    match = os.environ.get("CF_DISPATCH_TEST_BARRIER_MATCH")
    if not barrier_dir or (match and os.path.abspath(path) != os.path.abspath(match)):
        return
    root = Path(barrier_dir)
    root.mkdir(parents=True, exist_ok=True)
    key = hashlib.sha256(os.fsencode(os.path.abspath(path))).hexdigest()
    ready = root / f"{key}.{phase}.ready"
    release = root / f"{key}.{phase}.release"
    ready.write_text(str(path), encoding="utf-8")
    deadline = time.monotonic() + 15
    while not release.exists():
        if time.monotonic() >= deadline:
            raise PublicationError(f"test barrier timed out for {path}")
        time.sleep(0.001)


def _digest(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def publish(target: Path, source: Path) -> None:
    source_fd, source_data, _source_identity = _open_and_read(source, "publication source")
    os.close(source_fd)
    parent = target.parent
    temporary_name: str | None = None
    try:
        fd, temporary_name = tempfile.mkstemp(prefix=f".{target.name}.", dir=str(parent))
        try:
            with os.fdopen(fd, "wb") as temporary:
                temporary.write(source_data)
                temporary.flush()
                os.fsync(temporary.fileno())
        except Exception:
            raise
        os.replace(temporary_name, target)
        temporary_name = None
        target_fd, target_data, target_identity = _open_and_read(target, "published evidence")
        try:
            if _digest(target_data) != _digest(source_data):
                raise PublicationError("published evidence digest changed")
            _barrier(target, "publish")
            _path_matches_fd(target, target_fd, target_identity, "published evidence")
        finally:
            os.close(target_fd)
    except PublicationError:
        raise
    except OSError as exc:
        raise PublicationError(f"cannot atomically publish {target}: {exc}") from exc
    finally:
        if temporary_name:
            try:
                os.unlink(temporary_name)
            except FileNotFoundError:
                pass


def verify(entries: list[tuple[Path, str]]) -> None:
    opened: list[tuple[Path, str, int, bytes, tuple[int, int]]] = []
    try:
        for path, expected in entries:
            fd, data, identity = _open_and_read(path, path.name or "evidence")
            if _digest(data) != expected:
                raise PublicationError(f"{path} digest does not match")
            opened.append((path, expected, fd, data, identity))
        for path, _expected, _fd, _data, _identity_value in opened:
            _barrier(path, "verify")
        identities: set[tuple[int, int]] = set()
        canonical: set[Path] = set()
        for path, _expected, fd, _data, identity in opened:
            resolved = path.resolve(strict=False)
            if resolved in canonical:
                raise PublicationError("evidence paths resolve to the same file")
            canonical.add(resolved)
            _path_matches_fd(path, fd, identity, path.name or "evidence")
            if identity in identities:
                raise PublicationError("evidence paths use the same inode")
            identities.add(identity)
    finally:
        for _path, _expected, fd, _data, _identity_value in opened:
            os.close(fd)


def identity(paths: list[Path]) -> None:
    canonical: set[Path] = set()
    identities: set[tuple[int, int]] = set()
    for path in paths:
        resolved = path.resolve(strict=False)
        if resolved in canonical:
            raise PublicationError("evidence paths resolve to the same file")
        canonical.add(resolved)
        try:
            current = os.stat(path, follow_symlinks=False)
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise PublicationError(f"cannot inspect {path}: {exc}") from exc
        if stat.S_ISLNK(current.st_mode):
            raise PublicationError(f"{path} is a symlink")
        if current.st_nlink != 1:
            raise PublicationError(f"{path} is hardlinked")
        current_identity = _identity(current)
        if current_identity in identities:
            raise PublicationError("evidence paths use the same inode")
        identities.add(current_identity)


def main(argv: list[str]) -> int:
    try:
        command = argv[0] if argv else ""
        if command == "publish" and len(argv) == 3:
            publish(Path(argv[1]), Path(argv[2]))
        elif command == "identity" and len(argv) >= 3:
            identity([Path(value) for value in argv[1:]])
        elif command == "verify" and len(argv) >= 3 and len(argv[1:]) % 2 == 0:
            verify([(Path(argv[index]), argv[index + 1]) for index in range(1, len(argv), 2)])
        elif command == "digest" and len(argv) == 2:
            fd, data, _identity_value = _open_and_read(Path(argv[1]), "digest target")
            os.close(fd)
            print(_digest(data))
        else:
            raise PublicationError("usage: publish TARGET SOURCE | identity PATH... | verify PATH DIGEST... | digest PATH")
    except (OSError, PublicationError, ValueError) as exc:
        print(str(exc), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
