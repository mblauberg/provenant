#!/usr/bin/env python3
"""Report live ``codex exec`` workers and non-mutating liveness signals."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
import json
from pathlib import Path
import shlex
import subprocess
import sys
from typing import Iterable


# Canonical advisory rule. This label never triggers process control.
STALL_WINDOW_SECONDS = 60.0
TERMINAL_CLASSIFICATIONS = frozenset(
    {"blocked", "question", "unavailable", "failed", "complete"}
)


class TerminalReportError(ValueError):
    """A terminal report is missing required worker-exit evidence."""


class LiveWriterError(RuntimeError):
    """An operation would race an owned worker that is still writing."""


@dataclass(frozen=True)
class WorkerExit:
    pid: int
    exit_status: int


@dataclass(frozen=True)
class TerminalReport:
    classification: str
    pid: int
    observed_exit: bool
    exit_status: int


@dataclass(frozen=True)
class Process:
    pid: int
    ppid: int
    elapsed: str
    cpu: str
    command: str


@dataclass(frozen=True)
class Worker:
    pid: int
    elapsed: str
    elapsed_seconds: float
    cpu: str
    cpu_seconds: float
    cwd: str
    session_log: str
    session_log_mtime: str
    session_log_age_seconds: float | None
    output_size: int | None
    worktree: str
    stalled: bool


def require_worker_quiescent(*, pid: int, process_live: bool, action: str) -> None:
    """Fence worktree actions while the owned worker PID remains live."""
    if process_live:
        raise LiveWriterError(
            f"cannot {action} while owned worker PID {pid} remains live"
        )


def terminal_report(
    classification: str,
    *,
    pid: int,
    process_live: bool,
    exit_observation: WorkerExit | None,
) -> TerminalReport:
    """Build a terminal report only after the owned process has exited."""
    if classification not in TERMINAL_CLASSIFICATIONS:
        raise TerminalReportError(f"unknown terminal classification: {classification}")
    require_worker_quiescent(pid=pid, process_live=process_live, action="terminal-report")
    if exit_observation is None or exit_observation.pid != pid:
        raise TerminalReportError(
            "terminal report requires observed process exit for the owned PID"
        )
    return TerminalReport(
        classification, pid, observed_exit=True, exit_status=exit_observation.exit_status
    )


def output_grew(previous_output_size: int, current_output_size: int) -> bool:
    """Return the positive liveness signal for an owned worker."""
    return current_output_size > previous_output_size


def liveness_signal(
    *,
    process_live: bool,
    previous_output_size: int,
    current_output_size: int,
) -> str:
    if not process_live:
        return "exited"
    return (
        "progress"
        if output_grew(previous_output_size, current_output_size)
        else "live-no-growth"
    )


def rearm_poll_deadline(
    *, now: float, budget_seconds: float, process_live: bool
) -> float | None:
    """Re-arm a bounded wait while the owned PID remains live."""
    if budget_seconds <= 0:
        raise ValueError("poll budget must be positive")
    return now + budget_seconds if process_live else None


def parse_duration(value: str) -> float:
    parts = value.strip().split(":")
    try:
        numbers = [float(part) for part in parts]
    except ValueError:
        return 0.0
    if len(numbers) == 3:
        return numbers[0] * 3600 + numbers[1] * 60 + numbers[2]
    if len(numbers) == 2:
        return numbers[0] * 60 + numbers[1]
    return numbers[0] if len(numbers) == 1 else 0.0


def parse_elapsed(value: str) -> float:
    value = value.strip()
    days = 0.0
    if "-" in value:
        day_part, value = value.split("-", 1)
        days = float(day_part) * 86400
    return days + parse_duration(value)


def canonical_cwd(value: str) -> str:
    if value == "?":
        return value
    try:
        return str(Path(value).expanduser().resolve())
    except OSError:
        return value


def process_cwd(pid: int) -> str:
    if sys.platform.startswith("linux"):
        proc_link = Path("/proc") / str(pid) / "cwd"
        if proc_link.exists():
            try:
                return str(proc_link.resolve())
            except OSError:
                pass
    result = subprocess.run(
        ["lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"],
        capture_output=True, text=True, check=False,
    )
    for line in result.stdout.splitlines():
        if line.startswith("n"):
            return line[1:]
    return "?"


def command_cwd(command: str) -> str | None:
    try:
        arguments = shlex.split(command)
    except ValueError:
        return None
    for index, argument in enumerate(arguments):
        if argument in {"--cd", "-C"} and index + 1 < len(arguments):
            return arguments[index + 1]
        if argument.startswith("--cd="):
            return argument.split("=", 1)[1]
        if argument.startswith("-C") and len(argument) > 2:
            return argument[2:]
    return None


def worker_cwd(process: Process) -> str:
    launch_cwd = canonical_cwd(process_cwd(process.pid))
    target = command_cwd(process.command)
    if target is None:
        return launch_cwd
    path = Path(target).expanduser()
    if not path.is_absolute():
        if launch_cwd == "?":
            return target
        path = Path(launch_cwd) / path
    return canonical_cwd(str(path))


def live_processes() -> Iterable[Process]:
    try:
        result = subprocess.run(
            ["ps", "-axo", "pid=,ppid=,etime=,time=,command="],
            capture_output=True, text=True, check=False,
        )
    except OSError as error:
        raise RuntimeError(f"cannot inspect processes: {error}") from error
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "ps failed")
    processes: list[Process] = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 4)
        if len(fields) != 5:
            continue
        pid_text, ppid_text, elapsed, cpu, command = fields
        try:
            processes.append(Process(int(pid_text), int(ppid_text), elapsed, cpu, command))
        except ValueError:
            continue

    by_pid = {process.pid: process for process in processes}
    candidates = [
        process for process in processes
        if "codex exec" in process.command and "worker_liveness.py" not in process.command
    ]
    candidate_pids = {process.pid for process in candidates}
    wrapper_pids: set[int] = set()
    for process in candidates:
        ancestor_pid = process.ppid
        seen: set[int] = set()
        while ancestor_pid in by_pid and ancestor_pid not in seen:
            seen.add(ancestor_pid)
            if ancestor_pid in candidate_pids:
                wrapper_pids.add(ancestor_pid)
            ancestor_pid = by_pid[ancestor_pid].ppid
    yield from (process for process in candidates if process.pid not in wrapper_pids)


def session_logs(root: Path) -> dict[str, Path]:
    result: dict[str, Path] = {}
    if not root.is_dir():
        return result
    for path in root.glob("**/rollout-*.jsonl"):
        try:
            record = json.loads(path.open(encoding="utf-8").readline())
            cwd = record.get("payload", {}).get("cwd")
            if isinstance(cwd, str):
                cwd = canonical_cwd(cwd)
                current = result.get(cwd)
                if current is None or path.stat().st_mtime > current.stat().st_mtime:
                    result[cwd] = path
        except (OSError, json.JSONDecodeError):
            continue
    return result


def worktree_state(
    cwd: str,
    *,
    owned_pid: int | None = None,
    live_pids: set[int] | None = None,
) -> str:
    if owned_pid is not None:
        if live_pids is None:
            raise LiveWriterError(
                "cannot inspection without an observed PID set"
            )
        require_worker_quiescent(
            pid=owned_pid,
            process_live=owned_pid in live_pids,
            action="inspection",
        )
    if cwd == "?":
        return "unknown"
    result = subprocess.run(
        [
            "git", "--no-optional-locks", "-C", cwd,
            "status", "--porcelain=v1", "--untracked-files=all",
        ],
        capture_output=True, text=True, check=False,
    )
    if result.returncode:
        return "not-a-worktree"
    return "dirty" if result.stdout else "clean"


def repository_id(cwd: str | Path) -> str | None:
    if str(cwd) == "?":
        return None
    result = subprocess.run(
        [
            "git", "--no-optional-locks", "-C", str(cwd), "rev-parse",
            "--path-format=absolute", "--git-common-dir",
        ],
        capture_output=True, text=True, check=False,
    )
    if result.returncode or not result.stdout.strip():
        return None
    return canonical_cwd(result.stdout.strip())


def log_metadata(path: Path | None) -> tuple[str, float | None]:
    if path is None:
        return "-", None
    try:
        mtime = path.stat().st_mtime
    except OSError:
        return "-", None
    age = max(0.0, datetime.now(timezone.utc).timestamp() - mtime)
    return datetime.fromtimestamp(mtime, timezone.utc).isoformat(), age


def output_size(path: Path | None) -> int | None:
    if path is None:
        return None
    try:
        return path.stat().st_size
    except OSError:
        return None


def collect(
    sessions_root: Path,
    repo_scope: Path | None = None,
    previous_output_sizes: dict[int, int] | None = None,
) -> list[Worker]:
    logs = session_logs(sessions_root)
    scope_id = repository_id(repo_scope) if repo_scope is not None else None
    if repo_scope is not None and scope_id is None:
        raise RuntimeError(f"not a git repository: {repo_scope}")
    processes = list(live_processes())
    located_processes: list[tuple[Process, str]] = []
    for process in processes:
        cwd = worker_cwd(process)
        if scope_id is None or repository_id(cwd) == scope_id:
            located_processes.append((process, cwd))
    cwd_counts: dict[str, int] = {}
    for _process, cwd in located_processes:
        cwd_counts[cwd] = cwd_counts.get(cwd, 0) + 1
    workers: list[Worker] = []
    for process, cwd in located_processes:
        pid, elapsed, cpu = process.pid, process.elapsed, process.cpu
        elapsed_seconds = parse_elapsed(elapsed)
        cpu_seconds = parse_duration(cpu)
        log = logs.get(cwd)
        log_mtime, log_age = log_metadata(log)
        output_is_ambiguous = cwd_counts[cwd] > 1
        current_output_size = None if output_is_ambiguous else output_size(log)
        previous_output_size = (
            previous_output_sizes.get(pid) if previous_output_sizes is not None else None
        )
        stalled = (
            elapsed_seconds >= STALL_WINDOW_SECONDS
            and previous_output_size is not None
            and current_output_size is not None
            and not output_grew(previous_output_size, current_output_size)
        )
        workers.append(Worker(
            pid, elapsed, elapsed_seconds, cpu, cpu_seconds, cwd,
            str(log) if log else "-", log_mtime, log_age,
            current_output_size,
            "writer-live", stalled,
        ))
    return workers


def render(workers: list[Worker]) -> str:
    columns = (
        "PID", "ELAPSED", "CPU", "OUTPUT SIZE", "SESSION LOG MTIME",
        "WORKTREE", "STATUS", "CWD",
    )
    rows = [columns]
    for worker in workers:
        rows.append((
            str(worker.pid), worker.elapsed, worker.cpu,
            str(worker.output_size) if worker.output_size is not None else "-",
            worker.session_log_mtime, worker.worktree,
            "STALLED?" if worker.stalled else "active", worker.cwd,
        ))
    widths = [max(len(row[index]) for row in rows) for index in range(len(columns))]
    return "\n".join(
        "  ".join(value.ljust(widths[index]) for index, value in enumerate(row)).rstrip()
        for row in rows
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sessions-root", type=Path, default=Path.home() / ".codex" / "sessions")
    parser.add_argument(
        "--repo", type=Path, default=Path.cwd(),
        help="show workers from this repository (default: current repository)",
    )
    parser.add_argument("--json", action="store_true", help="emit machine-readable rows")
    args = parser.parse_args(argv)
    try:
        workers = collect(args.sessions_root, args.repo)
    except RuntimeError as error:
        print(f"worker-liveness: {error}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps([asdict(worker) for worker in workers], indent=2))
    else:
        print(render(workers))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
