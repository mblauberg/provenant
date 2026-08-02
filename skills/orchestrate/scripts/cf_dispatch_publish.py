#!/usr/bin/env python3
"""Create-only publisher and verifier for cf_dispatch evidence files."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import sys
import time
import uuid


sys.path.insert(0, str(Path(__file__).absolute().parents[2]))
from _shared.no_follow import (  # noqa: E402
    NoFollowError,
    identity as safe_identity,
    lexical_path,
    open_parent,
    read_file,
)


class PublicationError(RuntimeError):
    """The named evidence file could not be published or verified."""


def _digest(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def _validate_evidence_path(path: Path, evidence_root: Path) -> None:
    root = lexical_path(evidence_root)
    candidate = lexical_path(path if path.is_absolute() else evidence_root / path)
    try:
        candidate.relative_to(root)
    except ValueError as exc:
        raise PublicationError(f"evidence path escapes evidence root: {path}") from exc


def _barrier(path: Path, phase: str) -> None:
    """Optional deterministic test barrier; inactive for normal dispatches."""
    barrier_dir = os.environ.get("CF_DISPATCH_TEST_BARRIER_DIR")
    match = os.environ.get("CF_DISPATCH_TEST_BARRIER_MATCH")
    if not barrier_dir or (match and lexical_path(path) != lexical_path(Path(match))):
        return
    root = Path(barrier_dir)
    root.mkdir(parents=True, exist_ok=True)
    key = hashlib.sha256(os.fsencode(str(lexical_path(path)))).hexdigest()
    ready = root / f"{key}.{phase}.ready"
    release = root / f"{key}.{phase}.release"
    ready.write_text(str(path), encoding="utf-8")
    deadline = time.monotonic() + 15
    while not release.exists():
        if time.monotonic() >= deadline:
            raise PublicationError(f"test barrier timed out for {path}")
        time.sleep(0.001)


def _read(path: Path, label: str, root: Path) -> tuple[bytes, tuple[int, int]]:
    try:
        data, identity, _logical = read_file(path, label, root=root, allow_absolute=True)
    except (NoFollowError, OSError) as exc:
        raise PublicationError(str(exc)) from exc
    return data, identity


def _target_identity(path: Path, expected: tuple[int, int], label: str, root: Path) -> None:
    try:
        current = safe_identity(path, root=root)
    except (NoFollowError, OSError) as exc:
        raise PublicationError(f"{label} changed after publication: {exc}") from exc
    if current != expected:
        raise PublicationError(f"{label} inode changed after publication")


def _publish_bytes(target: Path, data: bytes, root: Path) -> None:
    try:
        parent_fd, name, _logical = open_parent(target, root=root, allow_absolute=True)
    except (NoFollowError, OSError) as exc:
        raise PublicationError(f"cannot safely open publication parent {target.parent}: {exc}") from exc
    temporary_name = f".{name}.{os.getpid()}.{uuid.uuid4().hex}.tmp"
    temporary_fd: int | None = None
    temporary_present = False
    try:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
        try:
            temporary_fd = os.open(temporary_name, flags, 0o600, dir_fd=parent_fd)
        except OSError as exc:
            raise PublicationError(f"cannot create publication temporary file: {exc}") from exc
        temporary_present = True
        offset = 0
        while offset < len(data):
            offset += os.write(temporary_fd, data[offset:])
        os.fsync(temporary_fd)
        os.close(temporary_fd)
        temporary_fd = None
        try:
            os.link(
                temporary_name,
                name,
                src_dir_fd=parent_fd,
                dst_dir_fd=parent_fd,
                follow_symlinks=False,
            )
        except FileExistsError as exc:
            raise PublicationError(f"refusing to replace existing evidence: {target}") from exc
        except OSError as exc:
            raise PublicationError(f"cannot atomically publish {target}: {exc}") from exc
        os.unlink(temporary_name, dir_fd=parent_fd)
        temporary_present = False
        final_fd = os.open(
            name,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
            dir_fd=parent_fd,
        )
        try:
            final_stat = os.fstat(final_fd)
            if final_stat.st_nlink != 1:
                raise PublicationError(f"published evidence is hardlinked: {target}")
            final_identity = (final_stat.st_dev, final_stat.st_ino)
            chunks: list[bytes] = []
            while True:
                chunk = os.read(final_fd, 1024 * 1024)
                if not chunk:
                    break
                chunks.append(chunk)
            final_data = b"".join(chunks)
            if final_data != data:
                raise PublicationError(f"published evidence digest changed: {target}")
            _barrier(target, "publish")
            _target_identity(target, final_identity, "published evidence", root)
        finally:
            os.close(final_fd)
    finally:
        if temporary_fd is not None:
            os.close(temporary_fd)
        if temporary_present:
            try:
                os.unlink(temporary_name, dir_fd=parent_fd)
            except FileNotFoundError:
                pass
        os.close(parent_fd)


def publish(target: Path, source: Path, root: Path) -> None:
    _validate_evidence_path(target, root)
    source_data, _source_identity = _read(source, "publication source", source.parent)
    _publish_bytes(target, source_data, root)


def verify(entries: list[tuple[Path, str]], root: Path) -> None:
    for path, _expected in entries:
        _validate_evidence_path(path, root)
    opened: list[tuple[Path, str, tuple[int, int]]] = []
    try:
        for path, expected in entries:
            data, identity = _read(path, path.name or "evidence", root)
            if _digest(data) != expected:
                raise PublicationError(f"{path} digest does not match")
            opened.append((path, expected, identity))
        for path, _expected, _identity_value in opened:
            _barrier(path, "verify")
        identities: set[tuple[int, int]] = set()
        paths: set[Path] = set()
        for path, _expected, identity in opened:
            normal = lexical_path(path)
            if normal in paths:
                raise PublicationError("evidence paths resolve to the same file")
            paths.add(normal)
            _target_identity(path, identity, path.name or "evidence", root)
            if identity in identities:
                raise PublicationError("evidence paths use the same inode")
            identities.add(identity)
    finally:
        del opened


def identity(paths: list[Path], root: Path) -> None:
    for path in paths:
        _validate_evidence_path(path, root)
    paths_seen: set[Path] = set()
    identities: set[tuple[int, int]] = set()
    for path in paths:
        normal = lexical_path(path)
        if normal in paths_seen:
            raise PublicationError("evidence paths resolve to the same file")
        paths_seen.add(normal)
        try:
            current = safe_identity(path, root=root, allow_missing=True)
        except (NoFollowError, OSError) as exc:
            raise PublicationError(f"cannot inspect {path}: {exc}") from exc
        if current is None:
            continue
        if current in identities:
            raise PublicationError("evidence paths use the same inode")
        identities.add(current)


def main(argv: list[str]) -> int:
    try:
        root = Path.cwd()
        if len(argv) >= 2 and argv[0] == "--root":
            root = Path(argv[1])
            argv = argv[2:]
        command = argv[0] if argv else ""
        if command == "publish" and len(argv) == 3:
            publish(Path(argv[1]), Path(argv[2]), root)
        elif command == "identity" and len(argv) >= 3:
            identity([Path(value) for value in argv[1:]], root)
        elif command == "verify" and len(argv) >= 3 and len(argv[1:]) % 2 == 0:
            verify([(Path(argv[index]), argv[index + 1]) for index in range(1, len(argv), 2)], root)
        elif command == "digest" and len(argv) == 2:
            target = Path(argv[1])
            _validate_evidence_path(target, root)
            data, _identity_value = _read(target, "digest target", root)
            print(_digest(data))
        else:
            raise PublicationError(
                "usage: [--root ROOT] publish TARGET SOURCE | identity PATH... | verify PATH DIGEST... | digest PATH"
            )
    except (OSError, PublicationError, ValueError) as exc:
        print(str(exc), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
