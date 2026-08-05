#!/usr/bin/env python3
"""Capture a normalized, model-specific Agy runtime capability snapshot.

`agy models` prints one dispatchable model id per line with the reasoning
effort already baked into the suffix, for example `gemini-3.1-pro-high`. The
bare family id is not dispatchable on its own: `agy --model gemini-3.1-pro`
exits 1 with "requires --effort (available: low, high)".

Efforts are per model rather than global. Measured on agy 1.1.10, gemini-3.1-pro
offers only low and high, while the flash families offer low, medium and high.
The CLI's own --help advertises a blanket low|medium|high and is wrong, so the
runtime list is the only trustworthy source.

This snapshot therefore keys on the family id and records the efforts that
family actually offers, which is the shape the route resolver consumes for
Codex.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sys
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from _shared.bounded_process import run_bounded

EFFORT_SUFFIX = re.compile(r"^(?P<family>.+?)-(?P<effort>low|medium|high)$")
# Agy also fronts models from other vendors. They are recorded so a caller can
# see them, but they must never satisfy a google-family route, and they must
# never be used to claim a cross-family opinion against the family they
# actually belong to.
FOREIGN_FAMILY = re.compile(r"claude|gpt|llama|mistral|qwen|oss", re.IGNORECASE)


def normalize(raw: str) -> dict[str, Any]:
    ids = [line.strip() for line in raw.splitlines() if line.strip()]
    if not ids:
        raise ValueError("agy models returned no model ids")

    models: dict[str, Any] = {}
    for model_id in ids:
        match = EFFORT_SUFFIX.match(model_id)
        family = match.group("family") if match else model_id
        entry = models.setdefault(
            family,
            {"resolved_model": family, "supported_efforts": [], "dispatchable_ids": []},
        )
        entry["dispatchable_ids"].append(model_id)
        if match:
            effort = match.group("effort")
            if effort not in entry["supported_efforts"]:
                entry["supported_efforts"].append(effort)

    for entry in models.values():
        entry["supported_efforts"].sort(key=["low", "medium", "high"].index)
        entry["dispatchable_ids"].sort()

    # The route resolver requires every entry in `models` to carry at least one
    # selectable effort. Agy lists some ids with no effort suffix at all, such
    # as claude-sonnet-4-6, which are dispatchable but not effort-controllable.
    # Routing one of those through this adapter would also break family
    # distinctness, since they are not Gemini. They stay visible under
    # `effortless_models` and out of the routable set.
    routable = {k: v for k, v in models.items() if v["supported_efforts"]}
    effortless = sorted(k for k, v in models.items() if not v["supported_efforts"])
    if not routable:
        raise ValueError("agy models listed no effort-controllable model")

    return {
        "schema_version": 1,
        "source": "agy models",
        "observed_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "models": routable,
        "effortless_models": effortless,
        "google_models": sorted(m for m in routable if m.startswith("gemini")),
        "foreign_models": sorted(m for m in models if FOREIGN_FAMILY.search(m)),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--agy-bin", default="agy")
    args = parser.parse_args(argv)
    try:
        result = run_bounded(
            [args.agy_bin, "models"],
            cwd=Path.cwd(),
            timeout_seconds=60,
            output_limit_bytes=1_048_576,
        )
        if result.timed_out:
            raise ValueError("agy models timed out")
        if result.returncode != 0:
            raise ValueError(
                f"agy models exited {result.returncode}: {result.output.strip()}"
            )
        snapshot = normalize(result.output)
    except (OSError, ValueError) as exc:
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
