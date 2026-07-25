#!/usr/bin/env python3
"""Probe one Claude subscription route and emit scrubbed runtime capability evidence."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import sys
from typing import Any


EFFORTS = {"low", "medium", "high", "xhigh", "max"}


def load_json(raw: str) -> Any:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON member: {key}")
            value[key] = item
        return value

    return json.loads(raw, object_pairs_hook=reject_duplicates)


def run_json(command: list[str], timeout: int) -> Any:
    result = subprocess.run(
        command,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if result.returncode != 0:
        raise ValueError(f"command exited {result.returncode}")
    return load_json(result.stdout)


def discover(claude_bin: str, alias: str, effort: str) -> dict[str, Any]:
    auth = run_json([claude_bin, "auth", "status"], 5)
    if (
        not isinstance(auth, dict)
        or auth.get("loggedIn") is not True
        or auth.get("authMethod") != "claude.ai"
        or not isinstance(auth.get("subscriptionType"), str)
        or not auth["subscriptionType"]
    ):
        raise ValueError("Claude subscription authentication is unavailable")

    result = run_json([
        claude_bin,
        "-p",
        "--safe-mode",
        "--no-session-persistence",
        "--permission-mode", "plan",
        "--tools", "",
        "--model", alias,
        "--effort", effort,
        "--output-format", "json",
        "Reply exactly OK.",
    ], 30)
    usage = result.get("modelUsage") if isinstance(result, dict) else None
    if (
        not isinstance(result, dict)
        or result.get("type") != "result"
        or result.get("subtype") != "success"
        or result.get("is_error") is not False
        or result.get("result") != "OK"
        or not isinstance(usage, dict)
    ):
        raise ValueError("Claude canary returned an ambiguous or unsuccessful result")
    alias_token = alias.casefold()
    matching_models = [
        model for model in usage
        if isinstance(model, str)
        and model.casefold().startswith("claude-")
        and alias_token in model.casefold().split("-")
    ]
    if len(matching_models) != 1:
        raise ValueError("Claude canary did not identify one primary runtime model")
    resolved_model = matching_models[0]

    return {
        "schema_version": 1,
        "source": "claude subscription canary",
        "observed_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "provenance": {
            "kind": "subscription_runtime_canary",
            "auth_method": "claude.ai",
            "subscription_type": auth["subscriptionType"],
        },
        "models": {
            alias.casefold(): {
                "resolved_model": resolved_model,
                "supported_efforts": [effort],
            },
        },
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--claude-bin", default="claude")
    parser.add_argument("--alias", required=True)
    parser.add_argument("--effort", choices=sorted(EFFORTS), required=True)
    args = parser.parse_args(argv)
    if not args.alias.strip():
        parser.error("--alias must be non-empty")
    try:
        snapshot = discover(args.claude_bin, args.alias, args.effort)
    except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError, ValueError) as exc:
        print(f"capability discovery failed: {exc}", file=sys.stderr)
        return 1
    args.out.write_text(json.dumps(snapshot, indent=2, sort_keys=True) + "\n")
    args.out.chmod(0o600)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
