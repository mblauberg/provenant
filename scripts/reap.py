#!/usr/bin/env python3
"""Stop explicit PIDs: TERM, then KILL after a grace period.

Only the PIDs named (and, with --tree, their descendants) are signalled, never a
process group or a pattern. Each process is described before signalling, and one
that started after --started-before is refused, so a reused PID cannot take
down another session's process.
"""

from __future__ import annotations

import argparse
from datetime import datetime
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

PS_ENV = {**os.environ, "LC_ALL": "C"}


class ReapError(ValueError):
    pass


def _ps(*arguments: str) -> str:
    result = subprocess.run(
        ["ps", *arguments], capture_output=True, text=True, env=PS_ENV, check=False
    )
    return result.stdout if result.returncode == 0 else ""


def process_info(pid: int) -> dict[str, object] | None:
    """Parent, start time, state, working directory and command, or None if gone."""
    row = _ps("-o", "ppid=,stat=,lstart=,command=", "-p", str(pid)).strip()
    if not row:
        return None
    fields = row.split(None, 7)
    if len(fields) < 8:
        return None
    started = time.mktime(time.strptime(" ".join(fields[2:7]), "%a %b %d %H:%M:%S %Y"))
    return {
        "pid": pid,
        "ppid": int(fields[0]),
        "state": fields[1],
        "started": started,
        "command": fields[7],
        "cwd": working_directory(pid),
    }


def working_directory(pid: int) -> str:
    proc = Path(f"/proc/{pid}/cwd")
    try:
        return os.readlink(proc)
    except OSError:
        pass
    try:
        out = subprocess.run(
            ["lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"],
            capture_output=True, text=True, check=False, timeout=10,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return "unknown"
    for line in out.splitlines():
        if line.startswith("n"):
            return line[1:]
    return "unknown"


def is_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    info = process_info(pid)
    return info is not None and not str(info["state"]).startswith("Z")


def descendants(pid: int) -> list[int]:
    """Descendants of pid, deepest first."""
    children: dict[int, list[int]] = {}
    for line in _ps("-A", "-o", "pid=,ppid=").splitlines():
        parts = line.split()
        if len(parts) == 2 and all(part.isdigit() for part in parts):
            children.setdefault(int(parts[1]), []).append(int(parts[0]))
    ordered: list[int] = []
    queue = [pid]
    while queue:
        for child in children.get(queue.pop(0), []):
            if child not in ordered:
                ordered.append(child)
                queue.append(child)
    return ordered[::-1]


def format_age(seconds: float) -> str:
    seconds = max(int(seconds), 0)
    days, rest = divmod(seconds, 86400)
    hours, rest = divmod(rest, 3600)
    minutes, secs = divmod(rest, 60)
    if days:
        return f"{days}d{hours:02d}h"
    if hours:
        return f"{hours}h{minutes:02d}m"
    return f"{minutes}m{secs:02d}s"


def describe(info: dict[str, object], now: float) -> str:
    return (
        f"pid {info['pid']} age {format_age(now - float(info['started']))} "
        f"parent {info['ppid']} cwd {info['cwd']} cmd {info['command']}"
    )


def parse_started_before(value: str) -> float:
    try:
        return float(value)
    except ValueError:
        pass
    try:
        return datetime.fromisoformat(value).timestamp()
    except ValueError:
        raise ReapError(
            f"--started-before needs epoch seconds or an ISO time, got {value!r}"
        ) from None


def protected_pids() -> set[int]:
    """This process and its ancestors: stopping them would end the caller."""
    protected, pid = {0, 1}, os.getpid()
    while pid > 1 and pid not in protected:
        protected.add(pid)
        info = process_info(pid)
        if info is None:
            break
        pid = int(info["ppid"])
    return protected


def reap(
    pids: list[int],
    *,
    tree: bool = False,
    grace: float = 5.0,
    started_before: float | None = None,
    out=None,
) -> int:
    """Return 0 when every PID is stopped or already gone, 1 otherwise."""
    out = out or sys.stdout
    protected = protected_pids()
    status = 0
    victims: list[int] = []
    for pid in dict.fromkeys(pids):
        if pid in protected:
            print(f"refused pid {pid}: pid 1, the caller or one of its ancestors", file=out)
            status = 1
            continue
        info = process_info(pid)
        if info is None or str(info["state"]).startswith("Z"):
            print(f"pid {pid} already gone", file=out)
            continue
        print(describe(info, time.time()), file=out)
        if started_before is not None and float(info["started"]) > started_before:
            print(f"refused pid {pid}: started after --started-before; PID may be reused", file=out)
            status = 1
            continue
        if tree:
            for child in descendants(pid):
                if child not in protected:
                    child_info = process_info(child)
                    if child_info is not None:
                        print("  " + describe(child_info, time.time()), file=out)
                        victims.append(child)
        victims.append(pid)
    victims = list(dict.fromkeys(victims))
    for pid in victims:
        _signal(pid, signal.SIGTERM)
    deadline = time.monotonic() + max(grace, 0.0)
    live = [pid for pid in victims if is_alive(pid)]
    while live and time.monotonic() < deadline:
        time.sleep(0.1)
        live = [pid for pid in live if is_alive(pid)]
    for pid in live:
        _signal(pid, signal.SIGKILL)
    if live:
        time.sleep(0.2)
    for pid in victims:
        if is_alive(pid):
            print(f"pid {pid} still running", file=out)
            status = 1
        else:
            print(f"stopped pid {pid}" + (" (KILL)" if pid in live else ""), file=out)
    return status


def _signal(pid: int, number: signal.Signals) -> None:
    try:
        os.kill(pid, number)
    except (ProcessLookupError, PermissionError):
        pass


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="provenant reap",
        description="Stop explicit PIDs: TERM, then KILL after the grace period.",
    )
    parser.add_argument("pids", nargs="+", help="numeric PIDs; patterns and groups are refused")
    parser.add_argument("--tree", action="store_true", help="also stop descendants")
    parser.add_argument("--grace", type=float, default=5.0, metavar="S",
                        help="seconds between TERM and KILL (default 5)")
    parser.add_argument("--started-before", metavar="TIME",
                        help="refuse a PID whose process started after this epoch or ISO time")
    options = parser.parse_args(arguments)
    try:
        pids = []
        for value in options.pids:
            if not value.isascii() or not value.isdigit():
                raise ReapError(f"{value!r} is not a literal PID")
            pids.append(int(value))
        cutoff = (
            parse_started_before(options.started_before)
            if options.started_before is not None else None
        )
        if options.grace < 0:
            raise ReapError("--grace must not be negative")
    except ReapError as exc:
        print(f"reap: {exc}", file=sys.stderr)
        return 2
    return reap(pids, tree=options.tree, grace=options.grace, started_before=cutoff)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
