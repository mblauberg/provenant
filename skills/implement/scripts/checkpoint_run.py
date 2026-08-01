#!/usr/bin/env python3
"""Update a delivery checkpoint through the canonical receipt producer."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from types import SimpleNamespace
from typing import Any


DELIVERY_SCRIPTS = Path(__file__).resolve().parents[2] / "deliver" / "scripts"
sys.path.insert(0, str(DELIVERY_SCRIPTS))
import delivery_receipt as producer


def update(
    path: Path,
    current_slice: str,
    next_action: str,
    in_flight: list[Any],
    artifacts: list[Any],
) -> dict[str, Any]:
    request = SimpleNamespace(
        run_dir=path,
        current_slice=current_slice,
        next_action=next_action,
        in_flight=in_flight,
        artifacts=artifacts,
        compatibility=True,
    )
    return producer.lifecycle.command_checkpoint_set(request, producer._api())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--current-slice", required=True)
    parser.add_argument("--next-action", required=True)
    parser.add_argument("--in-flight-json", default="[]")
    parser.add_argument("--artifact-paths-json", default="[]")
    args = parser.parse_args(argv)
    try:
        in_flight = json.loads(args.in_flight_json)
        artifacts = json.loads(args.artifact_paths_json)
        if not isinstance(in_flight, list) or not isinstance(artifacts, list):
            raise ValueError("JSON arguments must be arrays")
        result = update(args.run, args.current_slice, args.next_action, in_flight, artifacts)
    except (OSError, ValueError, json.JSONDecodeError, KeyError) as exc:
        print(json.dumps({"verified": False, "error": str(exc)}))
        return 1
    print(json.dumps(result))
    return 0 if result["verified"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
