"""Check the pointer-owned installed Provenant stub against its template."""

from __future__ import annotations

import os
from pathlib import Path
import sys

from lib.product_root_resolver import load_pointer_file


ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "scripts/provenant.template"


def main() -> int:
    instance_value = os.environ.get("AGENT_FABRIC_INSTANCE_ROOT")
    instance_root = (
        Path(instance_value).expanduser()
        if instance_value is not None
        else Path.home() / ".agents"
    )
    if not instance_root.is_absolute():
        print("FAIL: Agent Fabric instance root must be absolute", file=sys.stderr)
        return 1
    pointed_product = load_pointer_file(instance_root)
    if pointed_product is None or pointed_product.resolve() != ROOT.resolve():
        print("provenant installed stub=not-owned-by-this-checkout")
        return 0

    bin_value = os.environ.get("PROVENANT_BIN_DIR")
    bin_directory = (
        Path(bin_value).expanduser()
        if bin_value is not None
        else Path.home() / ".local/bin"
    )
    if not bin_directory.is_absolute():
        print("FAIL: PROVENANT_BIN_DIR must be absolute", file=sys.stderr)
        return 1
    command = bin_directory / "provenant"
    if command.is_symlink() or not command.is_file():
        print(
            f"FAIL: {command} must be a regular managed copy; re-run install-harness",
            file=sys.stderr,
        )
        return 1
    if not os.access(command, os.X_OK):
        print(
            f"FAIL: {command} is not executable; re-run install-harness",
            file=sys.stderr,
        )
        return 1
    if command.read_bytes() != TEMPLATE.read_bytes():
        print(
            "FAIL: installed stub differs from scripts/provenant.template; "
            "re-run install-harness",
            file=sys.stderr,
        )
        return 1
    print(f"provenant installed stub=ok path={command}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
