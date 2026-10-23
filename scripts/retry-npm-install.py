#!/usr/bin/env python3
"""Retry npm installs that fail with transient registry errors."""

from __future__ import annotations

from collections import deque
import re
import subprocess
import sys
import threading
import time


RETRYABLE = re.compile(
    r"(?:npm\s+(?:ERR!|error)\s+code\s+ECONNRESET"
    r"|npm\s+(?:ERR!|error)[^\n]*(?:503 Service Unavailable|\bE503\b|status 503)"
    r"|npm\s+http\s+fetch[^\n]*(?:503 Service Unavailable|\bE503\b|status 503|\bGET\s+503\b))",
    re.IGNORECASE,
)
DELAYS = (2, 4)
MAX_OUTPUT_TAIL = 64 * 1024


def _run_with_live_output(command: list[str]) -> tuple[int, str]:
    process = subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, errors="replace", bufsize=1,
    )
    tail: deque[str] = deque()
    tail_size = 0
    tail_lock = threading.Lock()

    def forward(stream, destination) -> None:
        nonlocal tail_size
        for chunk in iter(stream.readline, ""):
            destination.write(chunk)
            destination.flush()
            with tail_lock:
                tail.append(chunk)
                tail_size += len(chunk)
                while tail_size > MAX_OUTPUT_TAIL and tail:
                    excess = tail_size - MAX_OUTPUT_TAIL
                    first = tail.popleft()
                    if len(first) > excess:
                        tail.appendleft(first[excess:])
                        tail_size -= excess
                    else:
                        tail_size -= len(first)

    stdout_reader = threading.Thread(
        target=forward, args=(process.stdout, sys.stdout),
    )
    stderr_reader = threading.Thread(
        target=forward, args=(process.stderr, sys.stderr),
    )
    stdout_reader.start()
    stderr_reader.start()
    returncode = process.wait()
    stdout_reader.join()
    stderr_reader.join()
    with tail_lock:
        output_tail = "".join(tail)
    return returncode, output_tail


def main(argv: list[str] | None = None) -> int:
    command = list(sys.argv[1:] if argv is None else argv)
    if command[:1] == ["--"]:
        command.pop(0)
    if not command:
        print("usage: retry-npm-install.py -- <npm install command>", file=sys.stderr)
        return 2

    for attempt in range(len(DELAYS) + 1):
        returncode, output_tail = _run_with_live_output(command)
        if returncode == 0:
            return 0
        if attempt == len(DELAYS) or not RETRYABLE.search(output_tail):
            return returncode
        delay = DELAYS[attempt]
        print(f"npm install hit a transient registry error; retrying in {delay}s", file=sys.stderr)
        time.sleep(delay)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
