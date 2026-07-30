#!/usr/bin/env python3
"""Install the machine-local Provenant stub without clobbering foreign paths."""

from __future__ import annotations

import argparse
import ctypes
import errno
import os
from pathlib import Path
import stat
import sys
import tempfile


MANAGED_MARKER = (
    b"# Managed by scripts/install-harness; "
    b"local edits to an installed copy are not preserved."
)
OWNED_STATES = {"absent", "existing", "managed-file", "legacy-link"}


class Collision(RuntimeError):
    pass


def _normalised_link(destination: Path) -> Path:
    value = Path(os.readlink(destination))
    if not value.is_absolute():
        value = destination.parent / value
    return Path(os.path.abspath(value))


def classify(source: Path, destination: Path, legacy_target: Path) -> str:
    try:
        mode = destination.lstat().st_mode
    except FileNotFoundError:
        return "absent"
    if stat.S_ISLNK(mode):
        if _normalised_link(destination) == legacy_target:
            return "legacy-link"
        raise Collision(f"provenant command collision={destination}")
    if not stat.S_ISREG(mode):
        raise Collision(f"provenant command collision={destination}")
    content = destination.read_bytes()
    if content == source.read_bytes():
        return "existing"
    if MANAGED_MARKER in content.splitlines():
        return "managed-file"
    raise Collision(f"provenant command collision={destination}")


def _exchange(first: Path, second: Path) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    first_bytes = os.fsencode(first)
    second_bytes = os.fsencode(second)
    if sys.platform == "darwin":
        rename = libc.renamex_np
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        result = rename(first_bytes, second_bytes, 0x00000002)
    elif hasattr(libc, "renameat2"):
        rename = libc.renameat2
        rename.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        rename.restype = ctypes.c_int
        result = rename(-100, first_bytes, -100, second_bytes, 0x00000002)
    else:
        raise OSError(errno.ENOTSUP, "atomic path exchange is unavailable")
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def _sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def publish(
    source: Path,
    destination: Path,
    legacy_target: Path,
    expected: str,
) -> str:
    if expected not in OWNED_STATES:
        raise Collision(f"invalid expected Provenant command state: {expected}")
    current = classify(source, destination, legacy_target)
    if current != expected:
        raise Collision(
            f"provenant command collision={destination}; "
            f"changed after preflight ({expected} -> {current})"
        )
    if current == "existing":
        return "existing"

    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=".provenant.",
        dir=destination.parent,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(source.read_bytes())
            stream.flush()
            os.fsync(stream.fileno())
        temporary.chmod(0o755)
        if current == "absent":
            try:
                os.link(temporary, destination)
            except FileExistsError as exc:
                raise Collision(
                    f"provenant command collision={destination}; "
                    "created after preflight"
                ) from exc
        else:
            _exchange(temporary, destination)
            try:
                displaced = classify(source, temporary, legacy_target)
            except Collision:
                _exchange(temporary, destination)
                raise Collision(
                    f"provenant command collision={destination}; "
                    "changed during atomic publication"
                ) from None
            if displaced != current:
                _exchange(temporary, destination)
                raise Collision(
                    f"provenant command collision={destination}; "
                    "changed during atomic publication"
                )
        _sync_directory(destination.parent)
        return "installed" if current == "absent" else "updated"
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    for name in ("classify", "publish"):
        command = commands.add_parser(name)
        command.add_argument("--source", required=True, type=Path)
        command.add_argument("--destination", required=True, type=Path)
        command.add_argument("--legacy-target", required=True, type=Path)
        if name == "publish":
            command.add_argument("--expected", required=True)
    return root


def main(arguments: list[str] | None = None) -> int:
    args = parser().parse_args(arguments)
    for field in ("source", "destination", "legacy_target"):
        value = getattr(args, field)
        if not value.is_absolute():
            print(f"{field.replace('_', '-')} must be absolute", file=sys.stderr)
            return 3
    try:
        if args.command == "classify":
            result = classify(args.source, args.destination, args.legacy_target)
        else:
            result = publish(
                args.source,
                args.destination,
                args.legacy_target,
                args.expected,
            )
    except (Collision, OSError) as exc:
        print(exc, file=sys.stderr)
        return 3
    print(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
