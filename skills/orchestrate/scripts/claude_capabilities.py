#!/usr/bin/env python3
"""Probe one Claude subscription route and emit scrubbed runtime capability evidence."""

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


EFFORTS = {"low", "medium", "high", "xhigh", "max"}


def scrubbed_failure_detail(stdout: str, stderr: str) -> str:
    combined = "\n".join(part for part in (stderr.strip(), stdout.strip()) if part)
    if re.search(
        r"organi[sz]ation.*disabled.*subscription access|subscription access.*disabled",
        combined,
        re.IGNORECASE | re.DOTALL,
    ):
        return "provider access denied; subscription access disabled by organisation policy"
    if stderr.strip():
        return stderr.strip()
    status = re.search(r"\b(?:HTTP\s*)?(401|403|429)\b", stdout, re.IGNORECASE)
    if status:
        return f"provider returned HTTP {status.group(1)}"
    return ""


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
    result = run_bounded(
        command,
        cwd=Path.cwd(),
        timeout_seconds=timeout,
        output_limit_bytes=1_048_576,
        merge_stderr=False,
    )
    stderr = (result.stderr or "").strip()
    if result.timed_out:
        detail = f": {stderr}" if stderr else ""
        raise ValueError(f"command timed out after {timeout} seconds{detail}")
    if result.returncode != 0:
        reason = scrubbed_failure_detail(result.stdout or "", stderr)
        detail = f": {reason}" if reason else ""
        raise ValueError(f"command exited {result.returncode}{detail}")
    if "Warning: Unknown --effort value" in stderr:
        raise ValueError("Claude CLI rejected the requested effort")
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
                "requested_effort": effort,
                "effort_verified": False,
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
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"capability discovery failed: {exc}", file=sys.stderr)
        return 1
    args.out.write_text(json.dumps(snapshot, indent=2, sort_keys=True) + "\n")
    args.out.chmod(0o600)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
