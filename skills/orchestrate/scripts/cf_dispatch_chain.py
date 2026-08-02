#!/usr/bin/env python3
"""Build one immutable dispatch record for a fallback chain."""

from __future__ import annotations

import json
from pathlib import Path
import sys


def attempt_summary(item: dict[str, object]) -> dict[str, object]:
    record = item["record"]
    assert isinstance(record, dict)
    return {
        "attempt_id": record.get("attempt_id", ""),
        "status": record.get("status", ""),
        "exit": record.get("exit"),
        "output_path": record.get("output_path", ""),
        "output_sha256": record.get("output_sha256", ""),
        "terminal_artifact_path": record.get("terminal_artifact_path", ""),
        "terminal_artifact_sha256": record.get("terminal_artifact_sha256", ""),
        "receipt_path": item["receipt_path"],
    }


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: cf_dispatch_chain.py ATTEMPTS SELECTED_JSON_OR_EMPTY SUMMARY_PATH", file=sys.stderr)
        return 2
    items = [
        json.loads(line)
        for line in Path(argv[0]).read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    attempts = [attempt_summary(item) for item in items]
    if argv[1]:
        selected = json.loads(argv[1])
        selected_attempt = attempts[-1]
        selected["chain"] = {
            "attempts": attempts,
            "selected_success": {
                "attempt_id": selected_attempt["attempt_id"],
                "receipt_path": selected_attempt["receipt_path"],
            },
            "summary_path": argv[2],
        }
    else:
        selected = dict(items[-1]["record"] if items else {})
        selected.update({
            "tool": "chain",
            "adapter": "chain",
            "status": "all_failed",
            "exit": 1,
            "terminal_observed": False,
            "read_only_guarantee": "none",
            "cross_family": False,
            "certification_eligible": False,
            "chain": {
                "attempts": attempts,
                "selected_success": None,
                "summary_path": argv[2],
            },
        })
    print(json.dumps(selected, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
