#!/usr/bin/env python3
"""Build, identify and choose the installed Provenant front door (ADR 0027).

Standard library only, so it runs before and without the harness virtualenv.

The front door is a binary built from `tools/front-door/`. A built binary
embeds the managed-file marker and the line
`provenant-front-door source-sha256=<hex>` as lines of their own, so the
installer's marker rule and the install check both work on it unchanged. The
hash covers the workspace manifest, lockfile and toolchain, plus the front
door's and every `crates/*` member's manifest, `src/**` and `build.rs`.

Commands (stdout carries only the answer; progress and reasons go to stderr):

    digest   CRATE                       print the source digest
    embedded BINARY                      print the digest a binary embeds
    smoke    BINARY                      run `BINARY help`; exit 0 when healthy
    build    CRATE TARGET_DIR            build, print the binary path
    select   --product-root R --mode M --build-dir D [--prebuilt F]
                                         print `<kind>\\t<source path>`
"""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

CRATE_RELATIVE = "tools/front-door"
HASHED_FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml")
MODES = ("rust",)
RUSTUP_FIX = "fix: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
DEPLOYMENT_TARGET = "15.0"
IDENTITY = re.compile(rb"(?m)^provenant-front-door source-sha256=([0-9a-f]{64}|unknown)$")
SMOKE_TIMEOUT_SECONDS = 30


class FrontDoorError(RuntimeError):
    """The Rust front door cannot be produced; the message says why."""


class CargoUnavailable(FrontDoorError):
    """Install rustup before building the front door."""


def source_digest(crate: Path) -> str:
    """Hash the R2 workspace layout without requiring Cargo.

    Start with HASHED_FILES in their listed order (when present), then all
    remaining inputs sorted by workspace-relative POSIX path. Each input is
    framed as UTF-8 `path\0byte-length\0` followed by its unmodified bytes.
    """
    manifest = crate / "Cargo.toml"
    sources = crate / "src"
    if not manifest.is_file() or not sources.is_dir():
        raise FileNotFoundError(f"no Rust front door sources under {crate}")
    names = [name for name in HASHED_FILES if (crate / name).is_file()]
    # PORTING.md R2 fixes workspace members at `.` and `crates/*`. Include
    # every member, even before it becomes a dependency of the binary.
    packages = [crate, *(path.parent for path in crate.glob("crates/*/Cargo.toml"))]
    inputs = set()
    for package in packages:
        inputs.update(
            path.relative_to(crate).as_posix()
            for path in (package / "Cargo.toml", package / "build.rs")
            if path.is_file()
        )
        inputs.update(
            path.relative_to(crate).as_posix()
            for path in (package / "src").rglob("*") if path.is_file()
        )
    names += sorted(inputs.difference(names))
    digest = hashlib.sha256()
    for name in names:
        content = (crate / name).read_bytes()
        digest.update(f"{name}\0{len(content)}\0".encode())
        digest.update(content)
    return digest.hexdigest()


def embedded_digest(binary: Path) -> str | None:
    """The source digest a binary embeds, `unknown` if unstamped, None if absent."""
    match = IDENTITY.search(binary.read_bytes())
    return match.group(1).decode() if match else None


def smoke(binary: Path) -> None:
    """A binary that cannot print its help is not installed."""
    environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": os.environ.get("HOME", "/")}
    try:
        done = subprocess.run(
            [str(binary), "help"],
            env=environment,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=SMOKE_TIMEOUT_SECONDS,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise FrontDoorError(f"smoke run of {binary} failed: {exc}") from None
    if done.returncode != 0 or not done.stdout.startswith(b"usage: provenant"):
        raise FrontDoorError(f"smoke run of {binary} failed: `help` exited {done.returncode}")


def build(crate: Path, target_dir: Path, digest: str) -> Path:
    """`cargo build --release --locked` into `target_dir`; returns the binary."""
    crate, target_dir = crate.resolve(), target_dir.resolve()  # cwd changes below
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(target_dir)
    environment["PROVENANT_FD_SOURCE_SHA256"] = digest
    if sys.platform == "darwin":
        environment["MACOSX_DEPLOYMENT_TARGET"] = DEPLOYMENT_TARGET
    cargo = shutil.which("cargo")
    if cargo is None:
        fallback = Path.home() / ".cargo/bin/cargo"
        cargo = str(fallback) if fallback.is_file() else None
    if cargo is None:
        raise CargoUnavailable("cargo is not on PATH (looked in ~/.cargo/bin too)")
    command = [cargo, "build", "--release", "--locked", "--manifest-path", str(crate / "Cargo.toml")]
    try:
        # Cargo's own output is progress, not an answer: keep it off stdout.
        done = subprocess.run(
            command,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=sys.stderr,
            check=False,
            # The crate's rustup toolchain pin applies to the working directory.
            cwd=crate,
        )
    except OSError as exc:
        raise FrontDoorError(f"cannot run cargo: {exc}") from None
    binary = target_dir / "release" / "provenant"
    if done.returncode != 0 or not binary.is_file():
        raise FrontDoorError(f"cargo build failed (exit {done.returncode})")
    return binary


def select(
    product_root: Path,
    mode: str,
    build_dir: Path,
    prebuilt: Path | None = None,
    *,
    cargo_available: bool | None = None,
) -> tuple[str, Path, str]:
    """Produce the Rust artefact to install: `(kind, source path, note)`."""
    if mode not in MODES:
        raise ValueError(f"PROVENANT_FRONT_DOOR must be rust, got {mode!r}")
    crate = product_root / CRATE_RELATIVE

    if not (crate / "Cargo.toml").is_file():
        raise FrontDoorError(f"no {CRATE_RELATIVE} crate in {product_root}")
    digest = source_digest(crate)
    if prebuilt is not None:
        if not prebuilt.is_absolute() or prebuilt.is_symlink() or not prebuilt.is_file():
            raise FrontDoorError(
                f"PROVENANT_FRONT_DOOR_BINARY must be an absolute regular file: {prebuilt}"
            )
        try:
            stamped = embedded_digest(prebuilt)
        except OSError as exc:
            raise FrontDoorError(f"cannot read prebuilt front door {prebuilt}: {exc}") from None
        if stamped != digest:
            raise FrontDoorError(
                f"prebuilt front door {prebuilt} was built from different sources "
                f"(embeds {stamped}, checkout is {digest}); rebuild it from this checkout"
            )
        smoke(prebuilt)
        return "rust", prebuilt, "prebuilt"
    if cargo_available is None:
        cargo_available = shutil.which("cargo") is not None or (Path.home() / ".cargo/bin/cargo").is_file()
    if not cargo_available:
        raise CargoUnavailable("cargo is not on PATH (looked in ~/.cargo/bin too)")
    binary = build(crate, build_dir, digest)
    smoke(binary)
    return "rust", binary, "built"


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    commands = root.add_subparsers(dest="command", required=True)
    commands.add_parser("digest").add_argument("crate", type=Path)
    commands.add_parser("embedded").add_argument("binary", type=Path)
    commands.add_parser("smoke").add_argument("binary", type=Path)
    built = commands.add_parser("build")
    built.add_argument("crate", type=Path)
    built.add_argument("target_dir", type=Path)
    chosen = commands.add_parser("select")
    chosen.add_argument("--product-root", required=True, type=Path)
    chosen.add_argument("--mode", required=True)
    chosen.add_argument("--build-dir", required=True, type=Path)
    chosen.add_argument("--prebuilt", type=Path, default=None)
    return root


def main(arguments: list[str] | None = None) -> int:
    args = parser().parse_args(arguments)
    try:
        if args.command == "digest":
            print(source_digest(args.crate))
        elif args.command == "embedded":
            value = embedded_digest(args.binary)
            if value is None:
                return 1
            print(value)
        elif args.command == "smoke":
            smoke(args.binary)
        elif args.command == "build":
            print(build(args.crate, args.target_dir, source_digest(args.crate)))
        else:
            kind, source, note = select(args.product_root, args.mode, args.build_dir, args.prebuilt)
            print(f"{kind}\t{source}")
    except CargoUnavailable as exc:
        print(f"front door: {exc}", file=sys.stderr)
        print(RUSTUP_FIX, file=sys.stderr)
        return 3
    except FrontDoorError as exc:
        print(f"front door: {exc}", file=sys.stderr)
        return 3
    except (OSError, ValueError) as exc:
        print(f"front door: {exc}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
