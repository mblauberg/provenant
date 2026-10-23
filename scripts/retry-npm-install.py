#!/usr/bin/env python3
"""Retry npm installs that fail with transient registry errors."""

from __future__ import annotations

import subprocess
import sys
import time
import re


RETRYABLE = re.compile(
    r"(?:npm\s+(?:ERR!|error)\s+code\s+ECONNRESET"
    r"|npm\s+(?:ERR!|error)[^\n]*\b503\b"
    r"|npm\s+http\s+fetch[^\n]*\b503\b)",
    re.IGNORECASE,
)
DELAYS = (2, 4)


def main(argv: list[str] | None = None) -> int:
    command = list(sys.argv[1:] if argv is None else argv)
    if command[:1] == ["--"]:
        command.pop(0)
    if not command:
        print("usage: retry-npm-install.py -- <npm install command>", file=sys.stderr)
        return 2

    for attempt in range(len(DELAYS) + 1):
        result = subprocess.run(command, text=True, capture_output=True, check=False)
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
        if result.returncode == 0:
            return 0
        output = result.stdout + result.stderr
        if attempt == len(DELAYS) or not RETRYABLE.search(output):
            return result.returncode
        delay = DELAYS[attempt]
        print(f"npm install hit a transient registry error; retrying in {delay}s", file=sys.stderr)
        time.sleep(delay)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
