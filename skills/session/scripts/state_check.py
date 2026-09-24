#!/usr/bin/env python3
"""Check compact chair-session state files."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


MAX_BYTES = 6144
REQUIRED_HEADINGS = (
    "Goal and authority",
    "Stage and blockers",
    "Active lanes",
    "Queue",
    "Last checkpoint",
    "Next actions",
    "Links",
)
HEADING = re.compile(r"^##[ \t]+(.+?)\s*#*\s*$")
LIST_ITEM = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s+")


def project_root(cwd: Path) -> Path:
    try:
        result = subprocess.run(
            ["git", "-C", str(cwd), "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=False,
            timeout=0.25,
        )
    except (OSError, subprocess.TimeoutExpired):
        return cwd
    if result.returncode == 0 and result.stdout.strip():
        return Path(result.stdout.strip()).resolve()
    return cwd


def hook_cwd(cwd: Path) -> Path:
    if sys.stdin.isatty():
        return cwd
    try:
        payload = json.loads(sys.stdin.read())
    except (OSError, json.JSONDecodeError):
        return cwd
    if isinstance(payload, dict) and isinstance(payload.get("cwd"), str) and payload["cwd"]:
        candidate = Path(payload["cwd"])
        if not candidate.is_absolute():
            candidate = cwd / candidate
        return candidate.resolve()
    return cwd


def display_path(path: Path, root: Path, cwd: Path) -> str:
    for base in (root, cwd):
        try:
            return path.resolve().relative_to(base.resolve()).as_posix()
        except ValueError:
            continue
    return path.name


def check_file(path: Path, relative: str, *, max_age_minutes: float, now: float) -> list[str]:
    try:
        info = path.stat()
        content = path.read_bytes()
    except OSError as exc:
        message = exc.strerror or "I/O error"
        return [
            f"state_check: {relative}: cannot read file ({message}); make the state file readable"
        ]

    findings: list[str] = []
    if len(content) > MAX_BYTES:
        findings.append(
            f"state_check: {relative}: {len(content)} bytes exceeds {MAX_BYTES}; trim the state file below 6 KiB"
        )

    text = content.decode("utf-8", errors="replace")
    headings: list[tuple[str, int]] = []
    for index, line in enumerate(text.splitlines()):
        match = HEADING.match(line)
        if match:
            headings.append((match.group(1).strip(), index))

    for required in REQUIRED_HEADINGS:
        if not any(title.casefold().startswith(required.casefold()) for title, _ in headings):
            findings.append(
                f"state_check: {relative}: missing level-2 heading '{required}'; add the required heading"
            )

    next_heading = next(
        (
            (title, index)
            for title, index in headings
            if title.casefold().startswith("next actions")
        ),
        None,
    )
    if next_heading:
        _, start = next_heading
        end = next((index for _title, index in headings if index > start), len(text.splitlines()))
        lines = text.splitlines()
        item_count = sum(1 for line in lines[start + 1:end] if LIST_ITEM.match(line))
        if not 1 <= item_count <= 3:
            findings.append(
                f"state_check: {relative}: Next actions has {item_count} list items (expected 1 to 3); keep 1 to 3 next actions"
            )

    age_minutes = (now - info.st_mtime) / 60
    if age_minutes > max_age_minutes:
        findings.append(
            f"state_check: {relative}: file is older than {max_age_minutes:g} minutes; refresh the checkpoint"
        )
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", type=Path, help="state files to check")
    parser.add_argument("--max-age-minutes", type=float, default=30)
    parser.add_argument("--hook", action="store_true", help="run as a non-blocking PreCompact hook")
    args = parser.parse_args(argv)

    cwd = Path.cwd().resolve()
    if args.hook:
        cwd = hook_cwd(cwd)
    root = project_root(cwd)
    paths = [path if path.is_absolute() else cwd / path for path in args.paths]
    if not paths:
        paths = sorted((root / ".agent-run" / "sessions").glob("*/STATE.md"))

    findings = []
    now = time.time()
    for path in paths:
        findings.extend(
            check_file(
                path,
                display_path(path, root, cwd),
                max_age_minutes=args.max_age_minutes,
                now=now,
            )
        )
    prefix = "Checkpoint check: " if args.hook else ""
    for finding in findings:
        print(prefix + finding)
    return 0 if args.hook or not findings else 1


if __name__ == "__main__":
    raise SystemExit(main())
