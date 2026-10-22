#!/usr/bin/env python3
"""Select the checker for `provenant check`, for front doors that are not Python.

The compiled front door keeps registered-checkout custody and the stale-checkout
advisory in the existing Python dispatcher (ADR 0027). This helper loads that
dispatcher unchanged and calls its `check_target()` and
`warn_if_stale_checkout()`, so no custody logic lives here.

Contract: working directory and environment are the caller's. On success the
helper writes `<checker path>\\0<selected root>\\0` to stdout and exits 0; the
advisory warnings go to stderr. On refusal it prints `provenant: <message>` to
stderr and exits 3 with empty stdout.
"""

from __future__ import annotations

from importlib.machinery import SourceFileLoader
from importlib.util import module_from_spec, spec_from_loader
import os
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
# The Python dispatcher. In a checkout it is `scripts/provenant`; a tree whose
# `scripts/provenant` is a compiled binary keeps the source beside it as
# `scripts/provenant.py`.
DISPATCHER_CANDIDATES = ("provenant", "provenant.py")


def _is_python_source(path: Path) -> bool:
    try:
        with path.open("rb") as stream:
            first = stream.readline(256)
    except OSError:
        return False
    return first.startswith(b"#!") and b"python" in first


def load_dispatcher():
    for name in DISPATCHER_CANDIDATES:
        path = HERE / name
        if path.is_file() and _is_python_source(path):
            break
    else:
        raise ValueError("Python dispatcher is unavailable beside check_target.py")
    sys.path.insert(0, str(HERE))
    loader = SourceFileLoader("provenant_dispatcher", str(path))
    module = module_from_spec(spec_from_loader(loader.name, loader))
    loader.exec_module(module)
    return module


def main() -> int:
    try:
        dispatcher = load_dispatcher()
        target, selected = dispatcher.check_target()
    except (OSError, ValueError) as exc:
        print(f"provenant: {exc}", file=sys.stderr)
        return 3
    try:
        dispatcher.warn_if_stale_checkout(dispatcher.resolve_product_root())
    except Exception as exc:  # advisory only; PROVENANT_DEBUG shows why it failed
        if os.environ.get("PROVENANT_DEBUG"):
            print(f"provenant: stale-checkout check failed: {exc!r}", file=sys.stderr)
    sys.stdout.buffer.write(os.fsencode(target) + b"\0" + os.fsencode(selected) + b"\0")
    sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
