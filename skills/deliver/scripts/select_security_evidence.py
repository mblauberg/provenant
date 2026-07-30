#!/usr/bin/env python3
"""Select deterministic security evidence from declared changed surfaces."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys
from typing import Any


ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[3])
).expanduser()


class SelectionError(ValueError):
    pass


def select(surfaces: list[str], root: Path = ROOT, *, profile: str = "software") -> dict[str, Any]:
    try:
        policy = json.loads((root / "config" / "security-evidence.json").read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise SelectionError(f"security evidence policy is unreadable: {exc}") from exc
    if policy.get("schema_version") != 1 or not isinstance(policy.get("surfaces"), dict) or not isinstance(policy.get("checks"), dict):
        raise SelectionError("security evidence policy is invalid")
    unknown = sorted(set(surfaces) - set(policy["surfaces"]))
    if unknown:
        raise SelectionError("unknown changed surface: " + ", ".join(unknown))
    selected = sorted({check for surface in surfaces for check in policy["surfaces"][surface]})
    checks = []
    for check in selected:
        definition = policy["checks"].get(check)
        if not isinstance(definition, dict) or definition.get("kind") != "deterministic":
            raise SelectionError(f"check {check} lacks a deterministic definition")
        checks.append({"check": check, "kind": "deterministic", "status": "required"})
    return {
        "schema_version": 1,
        "profile": profile,
        "surfaces": sorted(set(surfaces)),
        "checks": checks,
        "agentic_risks": policy["agentic_risks"] if profile == "agent-product" else [],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--surface", action="append", required=True, dest="surfaces")
    parser.add_argument("--profile", choices=("software", "agent-product"), default="software")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    try:
        result = select(args.surfaces, args.root.resolve(), profile=args.profile)
    except SelectionError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
