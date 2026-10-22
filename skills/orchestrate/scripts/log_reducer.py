#!/usr/bin/env python3
"""Opt-in, evidence-preserving reduction for large CI and test logs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import time
from pathlib import Path

ALLOWED_COMMANDS = frozenset({"pytest", "vitest", "gh run view --log-failed", "tsc"})
MIN_BYTES = 4096
_EVIDENCE = re.compile(
    r"(?:\bFAIL(?:ED|URE|URES|ING)?\b|\bERROR\b|\b[A-Za-z_]*Error\b|\bEXCEPTION\b|\bTRACEBACK\b|"
    r"\bASSERT(?:ION)?\b|\bPANIC\b)",
    re.IGNORECASE,
)
# Counts and totals are kept as single lines; they never open a block.
_COUNT = re.compile(
    r"(?:\b\d+\s+(?:passed|failed|skipped|errors?)\b|"
    r"\b\d+\s+tests?\b|\b(?:total|count|tests?)\s*[:=]\s*\d+)",
    re.IGNORECASE,
)
_ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
_TEST_HEADER = re.compile(r"^\s*_{3,}.*_{3,}\s*$")
# A pytest failure section ends at a section rule, a captured-output rule, or
# after its final location line (`path:N: ErrorName`, not `path:N: in func`).
_SECTION_END = re.compile(r"^\s*(?:={3,}.*={3,}|-{3,}\s*Captured\b.*)\s*$")
_SECTION_LAST = re.compile(r"^\S+:\d+: (?!in )\S+")


def _is_continuation(line: str) -> bool:
    """Indented detail or a diff, source or location marker line."""
    return line[:1].isspace() or line.startswith(("E ", ">", "-", "+", "❯"))


def _quotes_are_ordered_source_lines(source: str, quotes: list[str]) -> bool:
    """Check exact source lines, in order, for all retained quotations."""
    source_lines = iter(source.splitlines())
    return all(any(line == quote for line in source_lines) for quote in quotes)


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
    ):
        return receipt

    lines = source.splitlines(keepends=True)
    clean_lines = [_ANSI.sub("", line) for line in lines]
    starts = {
        index for index, line in enumerate(clean_lines)
        if _EVIDENCE.search(line) or _TEST_HEADER.match(line)
    }
    counts = {index for index, line in enumerate(clean_lines) if _COUNT.search(line)}
    if not starts and not counts:
        return receipt

    evidence: set[int] = set(counts)
    for start in sorted(starts):
        evidence.add(start)
        if _TEST_HEADER.match(clean_lines[start]):
            index = start + 1
            while index < len(lines) and not (
                _TEST_HEADER.match(clean_lines[index])
                or _SECTION_END.match(clean_lines[index])
            ):
                evidence.add(index)
                if _SECTION_LAST.match(clean_lines[index]):
                    break
                index += 1
            continue
        # Other blocks run on through continuation lines, bridging blank lines.
        index = start + 1
        while index < len(lines) and index not in starts:
            next_index = index
            while next_index < len(lines) and not lines[next_index].strip():
                next_index += 1
            if next_index == len(lines) or not _is_continuation(clean_lines[next_index]):
                break
            evidence.update(range(index, next_index + 1))
            index = next_index + 1

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
    if not _quotes_are_ordered_source_lines(source, quotes):
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
        "--archive", type=Path, help="path to preserve the full source log"
    )
    parser.add_argument("--receipt", type=Path, help="optional JSON receipt path")
    parser.add_argument("--pilot", action="store_true", help="enable the reduction pilot")
    args = parser.parse_args(argv)

    source_bytes = sys.stdin.buffer.read()
    import secret_scan

    findings = secret_scan.scan_bytes(source_bytes, "stdin")
    if findings:
        for finding in findings:
            print(f"refusing to archive log: secret scan found {finding.name} at line {finding.line}", file=sys.stderr)
        return 2
    archive = args.archive
    if archive is None:
        instance_root = Path(
            os.environ.get("AGENT_FABRIC_INSTANCE_ROOT") or Path.home() / ".agents"
        ).expanduser()
        archive_dir = instance_root / "state" / "log-reducer"
        archive = archive_dir / f"log-{time.time_ns()}.log"
    try:
        archive.parent.mkdir(parents=True, exist_ok=True)
        archive.write_bytes(source_bytes)
    except OSError as exc:
        print(f"cannot archive full source log: {exc}", file=sys.stderr)
        return 2
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
