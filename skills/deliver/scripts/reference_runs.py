#!/usr/bin/env python3
"""Compatibility entry point for producer-owned reference receipts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))
from delivery_receipt import PRODUCT_ROOT, build_reference_run

ROOT = PRODUCT_ROOT


def make_reference_run(profile_name: str, root: Path = ROOT, *, high_stakes: bool = False):
    return build_reference_run(profile_name, root, high_stakes=high_stakes)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    registry = json.loads((args.root / "config" / "delivery-profiles.json").read_text())
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for profile in registry["profiles"]:
        (args.output_dir / f"{profile}.json").write_text(
            json.dumps(build_reference_run(profile, args.root), indent=2) + "\n"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
