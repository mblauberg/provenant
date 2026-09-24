"""Host memory admission for new provider attempts."""

from __future__ import annotations

import os
import fcntl
import json
import math
import re
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Callable

POLL_SECONDS = 15
SETTLE_SECONDS = 20


class MemoryUnavailableError(Exception):
    code = "memory_unavailable"


class AdmissionLease:
    def __init__(self, fd: int | None = None) -> None:
        self.fd = fd
        self.guard = threading.Lock()
        self.timer: threading.Timer | None = None

    def started(self) -> None:
        with self.guard:
            if self.fd is not None:
                if self.timer is not None:
                    self.timer.cancel()
                self.timer = threading.Timer(SETTLE_SECONDS, self.close)
                self.timer.daemon = True
                self.timer.start()

    def close(self) -> None:
        with self.guard:
            if self.fd is not None:
                fcntl.flock(self.fd, fcntl.LOCK_UN)
                os.close(self.fd)
                self.fd = None
            if self.timer is not None:
                self.timer.cancel()
                self.timer = None


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


def available_memory_mb() -> tuple[int, int]:
    if sys.platform == "darwin":
        output = subprocess.run(["/usr/bin/vm_stat"], capture_output=True, text=True,
                                timeout=5, check=True)
        total = subprocess.run(["/usr/sbin/sysctl", "-n", "hw.memsize"],
                               capture_output=True, text=True, timeout=5, check=True)
        return parse_vm_stat(output.stdout), int(total.stdout.strip()) // (1024 * 1024)
    if sys.platform.startswith("linux"):
        meminfo = Path("/proc/meminfo").read_text(encoding="ascii")
        total = re.search(r"^MemTotal:\s*(\d+)\s+kB\s*$", meminfo, re.M)
        if total is None:
            raise ValueError("MemTotal missing")
        return parse_meminfo(meminfo), int(total.group(1)) // 1024
    raise OSError(f"memory probe unsupported on {sys.platform}")


def floor_percent(workspace_root: Path, mode: str) -> int | float:
    defaults = {"worktree_write": 10, "read_only": 5}
    path = workspace_root / ".agents/fabric-policy.json"
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return defaults[mode]
    except (OSError, UnicodeError, ValueError) as exc:
        raise ValueError("Set .agents/fabric-policy.json memory_floor_percent to numbers from 0 to 100.") from exc
    floors = policy.get("memory_floor_percent", {}) if isinstance(policy, dict) else None
    if not isinstance(floors, dict) or any(
        key not in defaults or type(value) not in (int, float) or not math.isfinite(value)
        or not 0 <= value <= 100 for key, value in floors.items()
    ):
        raise ValueError("Set .agents/fabric-policy.json memory_floor_percent to numbers from 0 to 100.")
    return floors.get(mode, defaults[mode])


def _duration(seconds: float) -> str:
    return f"{int(seconds // 60)}m" if seconds >= 60 else f"{int(seconds)}s"


def wait_seconds() -> float:
    raw = os.environ.get("FABRIC_MEMORY_WAIT_SECONDS", "1800")
    try:
        value = float(raw)
        if not math.isfinite(value) or value < 0:
            raise ValueError
    except ValueError as exc:
        raise ValueError("FABRIC_MEMORY_WAIT_SECONDS must be a non-negative number") from exc
    return value


def lock_path() -> Path:
    state_home = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state")
    if not state_home.is_absolute():
        state_home = Path.home() / ".local/state"
    return state_home / "provenant/admission.lock"


def _lock_file(on_warning: Callable[[str], None]) -> int | None:
    path = lock_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        return os.open(path, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    except OSError as exc:
        on_warning(f"memory admission lock unavailable: {exc}")
        return None


def admit(
    on_wait: Callable[[str], None], cancelled: Callable[[], bool],
    on_warning: Callable[[str], None], probe: Callable[[], tuple[int, int]] | None = None,
    pause: Callable[[float], None] = time.sleep,
    waited_seconds: float = 0.0,
    workspace_root: Path | None = None,
    mode: str = "read_only",
) -> AdmissionLease | None:
    """Reserve host admission until the caller starts or ends the attempt."""
    floor = floor_percent(workspace_root or Path.cwd(), mode)
    probe = probe or available_memory_mb
    started = time.monotonic()
    budget = wait_seconds()
    deadline = started + max(0.0, budget - waited_seconds)
    waited_below_floor = False
    last_lock_report = 0.0
    while True:
        if cancelled():
            return None
        if waited_below_floor and time.monotonic() >= deadline:
            raise MemoryUnavailableError("memory admission wait expired")
        if floor == 0:
            return AdmissionLease()
        fd = _lock_file(on_warning)
        if fd is None:
            return AdmissionLease()
        contended = False
        while True:
            if cancelled():
                os.close(fd)
                return None
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                contended = True
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    os.close(fd)
                    raise MemoryUnavailableError("memory admission wait expired")
                if time.monotonic() - last_lock_report >= 1:
                    on_wait(f"waiting for memory admission lock: {waited_seconds + time.monotonic() - started:.1f}s elapsed, {remaining:.1f}s remaining")
                    last_lock_report = time.monotonic()
                pause(min(0.1, remaining))
            except OSError as exc:
                os.close(fd)
                on_warning(f"memory admission lock unavailable: {exc}")
                return AdmissionLease()
        if (contended or waited_below_floor or waited_seconds > 0) and time.monotonic() >= deadline:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)
            raise MemoryUnavailableError("memory admission wait expired")
        try:
            available, total = probe()
            if total <= 0 or available < 0:
                raise ValueError("invalid memory reading")
        except (OSError, ValueError, TypeError, subprocess.SubprocessError) as exc:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)
            waited_below_floor = True
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise MemoryUnavailableError("memory admission wait expired") from exc
            elapsed = waited_seconds + time.monotonic() - started
            on_wait(f"memory probe failed: {exc}; holding; {_duration(elapsed)} of {_duration(budget)}")
            poll_end = min(deadline, time.monotonic() + POLL_SECONDS)
            while time.monotonic() < poll_end:
                if cancelled():
                    return None
                pause(min(0.1, poll_end - time.monotonic()))
            continue
        if (waited_below_floor or contended or waited_seconds > 0) and time.monotonic() >= deadline:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)
            raise MemoryUnavailableError("memory admission wait expired")
        percent = available / total * 100
        if percent >= floor:
            return AdmissionLease(fd)
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)
        waited_below_floor = True
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise MemoryUnavailableError("memory admission wait expired")
        elapsed = waited_seconds + time.monotonic() - started
        on_wait(f"waiting for memory: {percent:.1f}% available ({available / 1024:.2f} GB), "
                f"floor {floor:g}% for {mode}; {_duration(elapsed)} of {_duration(budget)}")
        poll_end = min(deadline, time.monotonic() + POLL_SECONDS)
        while time.monotonic() < poll_end:
            if cancelled():
                return None
            pause(min(0.1, poll_end - time.monotonic()))
