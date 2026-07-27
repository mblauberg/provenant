#!/usr/bin/env python3
"""Atomically update and verify a canonical delivery-run recovery checkpoint."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import tempfile
from typing import Any


def fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def update(path: Path, current_slice: str, next_action: str, in_flight: list[Any], artifacts: list[Any]) -> dict[str, Any]:
    path = path.resolve()
    root = path.parent
    run = json.loads(path.read_text())
    if not isinstance(run, dict) or run.get("contract") != "delivery-run" or run.get("schema_version") != 1:
        raise ValueError("RUN.json must be a canonical delivery-run v1 receipt")
    checkpoint = run.get("checkpoint")
    if not isinstance(checkpoint, dict):
        raise ValueError("RUN.json checkpoint must be an object")
    generation = checkpoint.get("generation")
    if not isinstance(generation, int) or isinstance(generation, bool) or generation < 0:
        raise ValueError("checkpoint.generation must be a non-negative integer")
    if not all(isinstance(item, str) and item for item in in_flight + artifacts):
        raise ValueError("in-flight IDs and artifact paths must be non-empty strings")
    existing_artifacts = checkpoint.get("artifact_paths")
    if not isinstance(existing_artifacts, list):
        raise ValueError("checkpoint.artifact_paths must be a list")
    merged = list(dict.fromkeys([*existing_artifacts, *artifacts]))
    for value in merged:
        target = Path(value)
        target = target if target.is_absolute() else root / target
        try:
            target.resolve().relative_to(root)
        except ValueError as exc:
            raise ValueError(f"artifact path escapes run directory: {value}") from exc
        if not target.is_file():
            raise ValueError(f"artifact path does not exist: {value}")
    checkpoint.update({
        "generation": generation + 1,
        "current_slice": current_slice,
        "next_action": next_action,
        "in_flight": in_flight,
        "artifact_paths": merged,
    })
    run["updated_at"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    temp_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile("w", dir=root, prefix=".RUN.", suffix=".tmp", delete=False) as handle:
            temp_path = Path(handle.name)
            json.dump(run, handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_path, path)
        # The rename itself needs an fsync on the directory, or a crash can
        # leave the checkpoint the recovery path depends on unreachable.
        fsync_directory(root)
    finally:
        if temp_path is not None and temp_path.exists():
            temp_path.unlink()
    reread = json.loads(path.read_text())
    actual = reread["checkpoint"]
    verified = all((
        actual.get("generation") == generation + 1,
        actual.get("current_slice") == current_slice,
        actual.get("next_action") == next_action,
        actual.get("in_flight") == in_flight,
        actual.get("artifact_paths") == merged,
    ))
    return {"path": str(path), "generation": generation + 1, "verified": verified}


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
