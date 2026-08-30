#!/usr/bin/env python3
"""Install and verify one identity-bound dispatch output."""

from __future__ import annotations

import argparse
import hashlib
import os
import secrets
import stat
import sys


class CustodyError(ValueError):
    """The requested output path cannot retain the installed file identity."""


def open_parent(path: str) -> tuple[int, str]:
    """Open the lexical parent without following user-controlled symlinks."""
    absolute = os.path.isabs(path)
    parts = path.split(os.sep)
    if absolute:
        current = os.open(os.sep, os.O_RDONLY | os.O_DIRECTORY)
        parts = parts[1:]
    else:
        current = os.open(".", os.O_RDONLY | os.O_DIRECTORY)
    if not parts or not parts[-1] or parts[-1] in {".", ".."}:
        os.close(current)
        raise CustodyError("output destination has no file name")
    leaf = parts.pop()
    try:
        for component in parts:
            if component in {"", "."}:
                continue
            if component == "..":
                raise CustodyError("output destination may not traverse a parent directory")
            entry = os.stat(component, dir_fd=current, follow_symlinks=False)
            flags = os.O_RDONLY | os.O_DIRECTORY
            if stat.S_ISLNK(entry.st_mode):
                # Darwin exposes /var and /tmp as root-owned aliases. Follow an
                # alias only when neither it nor its containing directory is
                # user-controlled.
                container = os.fstat(current)
                if (
                    entry.st_uid != 0
                    or container.st_uid != 0
                    or container.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
                ):
                    raise CustodyError("output destination parent is a symlink")
            else:
                flags |= os.O_NOFOLLOW
            next_fd = os.open(component, flags, dir_fd=current)
            os.close(current)
            current = next_fd
        return current, leaf
    except BaseException:
        os.close(current)
        raise


def hash_fd(fd: int) -> str:
    os.lseek(fd, 0, os.SEEK_SET)
    hasher = hashlib.sha256()
    while chunk := os.read(fd, 1024 * 1024):
        hasher.update(chunk)
    return "sha256:" + hasher.hexdigest()


def validate_existing_leaf(parent_fd: int, leaf: str) -> None:
    try:
        existing = os.stat(leaf, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    if stat.S_ISDIR(existing.st_mode):
        raise CustodyError("output destination is a directory")
    if stat.S_ISLNK(existing.st_mode):
        try:
            target = os.stat(leaf, dir_fd=parent_fd, follow_symlinks=True)
        except FileNotFoundError:
            return
        if not stat.S_ISREG(target.st_mode):
            raise CustodyError("output destination symlink does not target a regular file")
    elif not stat.S_ISREG(existing.st_mode):
        raise CustodyError("output destination is not a regular file")


def install(source: str, destination: str) -> tuple[str, int, int]:
    parent_fd = source_fd = temporary_fd = installed_fd = None
    temporary_name = ""
    try:
        parent_fd, leaf = open_parent(destination)
        validate_existing_leaf(parent_fd, leaf)
        source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
        if not stat.S_ISREG(os.fstat(source_fd).st_mode):
            raise CustodyError("dispatch output source is not a regular file")

        for _ in range(32):
            temporary_name = f".cf-dispatch-output.{secrets.token_hex(12)}"
            try:
                temporary_fd = os.open(
                    temporary_name,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                    0o600,
                    dir_fd=parent_fd,
                )
                break
            except FileExistsError:
                temporary_name = ""
        if temporary_fd is None:
            raise CustodyError("could not allocate an output temporary file")

        source_hash = hashlib.sha256()
        while chunk := os.read(source_fd, 1024 * 1024):
            source_hash.update(chunk)
            view = memoryview(chunk)
            while view:
                view = view[os.write(temporary_fd, view):]
        os.fsync(temporary_fd)
        os.close(temporary_fd)
        temporary_fd = None
        os.replace(temporary_name, leaf, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)
        temporary_name = ""
        os.fsync(parent_fd)

        installed_fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent_fd)
        metadata = os.fstat(installed_fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise CustodyError("installed output is not a retained regular file")
        digest = "sha256:" + source_hash.hexdigest()
        if hash_fd(installed_fd) != digest:
            raise CustodyError("installed output identity changed")
        return digest, metadata.st_dev, metadata.st_ino
    finally:
        if temporary_name and parent_fd is not None:
            try:
                os.unlink(temporary_name, dir_fd=parent_fd)
            except OSError:
                pass
        for descriptor in (installed_fd, temporary_fd, source_fd, parent_fd):
            if descriptor is not None:
                try:
                    os.close(descriptor)
                except OSError:
                    pass


def verify(destination: str, digest: str, device: int, inode: int) -> None:
    """Verify bytes and inode, then re-open the lexical path after hashing."""
    parent_fd = output_fd = None
    try:
        parent_fd, leaf = open_parent(destination)
        output_fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent_fd)
        before = os.fstat(output_fd)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_nlink != 1
            or (before.st_dev, before.st_ino) != (device, inode)
            or hash_fd(output_fd) != digest
        ):
            raise CustodyError("output path no longer names the installed identity")
        after = os.fstat(output_fd)
        if (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns) != (
            before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns
        ):
            raise CustodyError("output identity changed while being verified")
    finally:
        for descriptor in (output_fd, parent_fd):
            if descriptor is not None:
                os.close(descriptor)

    # The potentially long hash is complete. Re-open once more so a parent
    # rename/symlink substitution during that window cannot certify.
    parent_fd = output_fd = None
    try:
        parent_fd, leaf = open_parent(destination)
        output_fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent_fd)
        final = os.fstat(output_fd)
        if (
            not stat.S_ISREG(final.st_mode)
            or final.st_nlink != 1
            or (final.st_dev, final.st_ino) != (device, inode)
        ):
            raise CustodyError("output path no longer names the installed identity")
    finally:
        for descriptor in (output_fd, parent_fd):
            if descriptor is not None:
                os.close(descriptor)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    install_command = commands.add_parser("install")
    install_command.add_argument("--source", required=True)
    install_command.add_argument("--destination", required=True)
    verify_command = commands.add_parser("verify")
    verify_command.add_argument("--destination", required=True)
    verify_command.add_argument("--digest", required=True)
    verify_command.add_argument("--device", type=int, required=True)
    verify_command.add_argument("--inode", type=int, required=True)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "install":
            digest, device, inode = install(args.source, args.destination)
            print(digest, device, inode)
        else:
            verify(args.destination, args.digest, args.device, args.inode)
    except (CustodyError, OSError) as exc:
        print(f"output custody failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
