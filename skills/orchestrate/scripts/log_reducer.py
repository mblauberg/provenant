#!/usr/bin/env python3
"""Opt-in, evidence-preserving reduction for large CI and test logs."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

ALLOWED_COMMANDS = frozenset({"pytest", "vitest", "gh run view --log-failed", "tsc"})
MIN_BYTES = 4096
_EVIDENCE = re.compile(
    r"(?:\bFAIL(?:ED|URE|URES|ING)?\b|\bERROR\b|\b[A-Za-z_]*Error\b|\bEXCEPTION\b|\bTRACEBACK\b|"
    r"\bASSERT(?:ION)?\b|\bPANIC\b|\b\d+\s+(?:passed|failed|skipped|errors?)\b|"
    r"\b\d+\s+tests?\b|\b(?:total|count|tests?)\s*[:=]\s*\d+)",
    re.IGNORECASE,
)
_SECRET = re.compile(
    r"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|"
    r"AKIA[0-9A-Z]{16}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|"
    r"(?i:api[_-]?key|token|secret|password)\s*[:=]\s*['\"]?[^\s'\"]{12,})"
)


def reduce_text(
    source: str, *, command: str, exit_status: int, enabled: bool = True
) -> dict[str, object]:
    """Return a compact receipt and text, falling back to the source on any doubt."""
    source_bytes = source.encode("utf-8")
    digest = hashlib.sha256(source_bytes).hexdigest()
    receipt: dict[str, object] = {
        "schema": "provenant-log-reduction.v1",
        "command": command,
        "source_sha256": digest,
        "exit_status": exit_status,
        "source_bytes": len(source_bytes),
        "reduced_bytes": len(source_bytes),
        "quoted_lines": [],
        "dropped_spans": [],
        "reduced": False,
        "text": source,
    }
    if (
        not enabled
        or command not in ALLOWED_COMMANDS
        or len(source_bytes) < MIN_BYTES
        or _SECRET.search(source)
    ):
        return receipt

    lines = source.splitlines(keepends=True)
    evidence = {index for index, line in enumerate(lines) if _EVIDENCE.search(line)}
    nonempty = [index for index, line in enumerate(lines) if line.strip()]
    if nonempty:
        evidence.add(nonempty[-1])
    if not evidence:
        return receipt

    output: list[str] = []
    spans: list[dict[str, int]] = []
    quotes: list[str] = []
    index = 0
    while index < len(lines):
        if index in evidence:
            output.append(lines[index])
            quotes.append(lines[index].rstrip("\r\n"))
            index += 1
            continue
        start = index
        while index < len(lines) and index not in evidence:
            index += 1
        count = index - start
        marker = f"[reducer: dropped {count} lines, source lines {start + 1}-{index}]\n"
        output.append(marker)
        spans.append({"start_line": start + 1, "end_line": index, "dropped_lines": count})

    reduced = "".join(output)
    reduced_bytes = len(reduced.encode("utf-8"))
    if reduced_bytes >= len(source_bytes):
        return receipt
    # The retained quotes are exact source lines; verify them before returning a reduction.
    if any(quote not in source for quote in quotes):
        return receipt
    receipt.update({
        "reduced_bytes": reduced_bytes,
        "quoted_lines": quotes,
        "dropped_spans": spans,
        "reduced": True,
        "text": reduced,
    })
    return receipt


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--command", required=True, choices=sorted(ALLOWED_COMMANDS))
    parser.add_argument("--exit-status", required=True, type=int)
    parser.add_argument(
        "--archive", required=True, type=Path, help="path to preserve the full source log"
    )
    parser.add_argument("--receipt", type=Path, help="optional JSON receipt path")
    parser.add_argument("--pilot", action="store_true", help="enable the reduction pilot")
    args = parser.parse_args(argv)

    source_bytes = sys.stdin.buffer.read()
    args.archive.parent.mkdir(parents=True, exist_ok=True)
    args.archive.write_bytes(source_bytes)
    try:
        source = source_bytes.decode("utf-8")
    except UnicodeDecodeError:
        sys.stdout.buffer.write(source_bytes)
        return 0
    result = reduce_text(
        source,
        command=args.command,
        exit_status=args.exit_status,
        enabled=args.pilot,
    )
    if args.receipt:
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        metadata = {key: value for key, value in result.items() if key != "text"}
        args.receipt.write_text(
            json.dumps(metadata, indent=2) + "\n", encoding="utf-8"
        )
    sys.stdout.buffer.write(str(result["text"]).encode("utf-8"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
