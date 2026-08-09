#!/usr/bin/env python3
"""Capture a normalized, model-specific Codex runtime capability snapshot."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import sys
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from _shared.bounded_process import run_bounded


def load_json(raw: str) -> Any:
    def reject_duplicate_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON member: {key}")
            result[key] = value
        return result

    return json.loads(raw, object_pairs_hook=reject_duplicate_members)


def normalize(raw: Any) -> dict[str, Any]:
    if not isinstance(raw, dict) or not isinstance(raw.get("models"), list):
        raise ValueError("catalogue root must contain a models list")
    models: dict[str, Any] = {}
    for item in raw["models"]:
        if not isinstance(item, dict):
            raise ValueError("catalogue model entry must be an object")
        slug = item.get("slug")
        if not isinstance(slug, str) or not slug.strip():
            raise ValueError("catalogue model slug must be a non-empty string")
        levels = item.get("supported_reasoning_levels")
        if not isinstance(levels, list):
            raise ValueError("catalogue reasoning levels must be a list")
        if not levels:
            raise ValueError("catalogue reasoning levels must not be empty")
        efforts = []
        for level in levels:
            if not isinstance(level, dict):
                raise ValueError("catalogue reasoning-level entry must be an object")
            effort = level.get("effort")
            if not isinstance(effort, str) or not effort.strip():
                raise ValueError("catalogue reasoning effort must be a non-empty string")
            efforts.append(effort.lower())
        normalized_slug = slug.casefold()
        if normalized_slug in models:
            raise ValueError("catalogue model slugs must be unique when case-folded")
        models[normalized_slug] = {
            "resolved_model": slug,
            "supported_efforts": efforts,
        }
    if not models:
        raise ValueError("catalogue contains no usable model entries")
    return {
        "schema_version": 1,
        "source": "codex debug models",
        "observed_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "models": models,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--codex-bin", default="codex")
    args = parser.parse_args(argv)
    try:
        result = run_bounded(
            [args.codex_bin, "debug", "models"],
            cwd=Path.cwd(),
            timeout_seconds=10,
            output_limit_bytes=1_048_576,
            merge_stderr=False,
        )
        stderr = (result.stderr or "").strip()
        if result.timed_out:
            detail = f": {stderr}" if stderr else ""
            raise ValueError(f"codex debug models timed out{detail}")
        if result.returncode != 0:
            detail = f": {stderr}" if stderr else ""
            raise ValueError(
                f"codex debug models exited {result.returncode}{detail}"
            )
        snapshot = normalize(load_json(result.stdout))
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"capability discovery failed: {exc}", file=sys.stderr)
        return 1
    encoded = json.dumps(snapshot, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.write_text(encoded)
    else:
        print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
