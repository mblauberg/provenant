"""Host memory admission for new provider attempts."""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Callable

POLL_SECONDS = 15


def parse_vm_stat(text: str) -> int:
    header = re.search(r"page size of (\d+) bytes", text)
    if header is None:
        raise ValueError("vm_stat page size missing")
    pages = {}
    for name, count in re.findall(r"^Pages (free|inactive|speculative):\s*([\d,.]+)", text, re.M):
        pages[name] = int(count.replace(".", "").replace(",", ""))
    if len(pages) != 3:
        raise ValueError("vm_stat available page counts missing")
    return sum(pages.values()) * int(header.group(1)) // (1024 * 1024)


def parse_meminfo(text: str) -> int:
    match = re.search(r"^MemAvailable:\s*(\d+)\s+kB\s*$", text, re.M)
    if match is None:
        raise ValueError("MemAvailable missing")
    return int(match.group(1)) // 1024


def available_mb() -> int:
    if sys.platform == "darwin":
        output = subprocess.run(["/usr/bin/vm_stat"], capture_output=True, text=True,
                                timeout=5, check=True)
        return parse_vm_stat(output.stdout)
    if sys.platform.startswith("linux"):
        return parse_meminfo(Path("/proc/meminfo").read_text(encoding="ascii"))
    raise OSError(f"memory probe unsupported on {sys.platform}")


def floor_mb() -> int:
    raw = os.environ.get("FABRIC_MEMORY_FLOOR_MB", "1024")
    try:
        floor = int(raw)
        if floor < 0:
            raise ValueError
    except ValueError as exc:
        raise ValueError("FABRIC_MEMORY_FLOOR_MB must be a non-negative integer") from exc
    return floor


def admit(
    on_wait: Callable[[str], None], cancelled: Callable[[], bool],
    on_warning: Callable[[str], None], probe: Callable[[], int] = available_mb,
    pause: Callable[[float], None] = time.sleep,
) -> bool:
    """Wait for available memory in this attempt owner; false means cancelled."""
    floor = floor_mb()
    while True:
        if cancelled():
            return False
        if floor == 0:
            return True
        try:
            available = probe()
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            on_warning(f"memory probe failed: {exc}")
            return True
        if available >= floor:
            return True
        on_wait(f"waiting for memory: {available} MB available, floor {floor} MB")
        deadline = time.monotonic() + POLL_SECONDS
        while time.monotonic() < deadline:
            if cancelled():
                return False
            pause(min(0.1, deadline - time.monotonic()))
