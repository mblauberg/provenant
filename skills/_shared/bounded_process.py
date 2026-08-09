"""Bounded POSIX child-process execution with file-backed output in two modes.

The direct child owns a new session and process group. Descendants that leave
that group with ``setsid()`` are outside this helper's containment contract.
Merged output is spooled in operating-system write order before a bounded
head-and-tail view is retained in memory. Separated mode spools stdout and
stderr independently, so their cross-stream order is not available.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from contextlib import ExitStack
from dataclasses import dataclass
import os
import signal
import subprocess
import tempfile
import time
from pathlib import Path
from typing import BinaryIO


_TRUNCATION_MARKER = b"\n...[output truncated]...\n"
_TERMINATION_GRACE_SECONDS = 0.25
_POLL_SECONDS = 0.01


@dataclass(frozen=True, slots=True)
class BoundedProcessResult:
    returncode: int
    output: str
    output_bytes: int
    output_truncated: bool
    timed_out: bool
    elapsed_seconds: float
    stdout: str | None = None
    stderr: str | None = None
    stdout_bytes: int | None = None
    stderr_bytes: int | None = None
    stdout_truncated: bool | None = None
    stderr_truncated: bool | None = None

    @property
    def terminating_signal(self) -> int | None:
        return -self.returncode if self.returncode < 0 else None


def _read_bounded_output(output_file: BinaryIO, limit: int) -> tuple[str, int, bool]:
    output_file.seek(0, os.SEEK_END)
    output_bytes = output_file.tell()
    limit = max(0, limit)
    if output_bytes <= limit:
        output_file.seek(0)
        retained = output_file.read(output_bytes)
        truncated = False
    elif limit <= len(_TRUNCATION_MARKER):
        head = limit // 2
        tail = limit - head
        output_file.seek(0)
        retained = output_file.read(head)
        if tail:
            output_file.seek(-tail, os.SEEK_END)
            retained += output_file.read(tail)
        truncated = True
    else:
        available = limit - len(_TRUNCATION_MARKER)
        head = available // 2
        tail = available - head
        output_file.seek(0)
        retained = output_file.read(head) + _TRUNCATION_MARKER
        output_file.seek(-tail, os.SEEK_END)
        retained += output_file.read(tail)
        truncated = True
    return retained.decode("utf-8", errors="replace"), output_bytes, truncated


def _signal_group(process_id: int, sig: signal.Signals) -> bool:
    try:
        os.killpg(process_id, sig)
    except (ProcessLookupError, OSError):
        return False
    return True


def _group_exists(process_id: int) -> bool:
    try:
        os.killpg(process_id, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def _stop_process_group(process: subprocess.Popen[bytes]) -> None:
    group_signalled = _signal_group(process.pid, signal.SIGTERM)
    if not group_signalled and process.poll() is None:
        process.kill()
    deadline = time.monotonic() + _TERMINATION_GRACE_SECONDS
    while _group_exists(process.pid) and time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        if process.poll() is None:
            try:
                process.wait(timeout=min(_POLL_SECONDS, remaining))
            except subprocess.TimeoutExpired:
                pass
        else:
            time.sleep(min(_POLL_SECONDS, remaining))
    if _group_exists(process.pid):
        group_killed = _signal_group(process.pid, signal.SIGKILL)
        if not group_killed and process.poll() is None:
            process.kill()
    try:
        process.wait(timeout=_TERMINATION_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=_TERMINATION_GRACE_SECONDS)


def run_bounded(
    command: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    env: Mapping[str, str] | None = None,
    output_limit_bytes: int = 1_048_576,
    merge_stderr: bool = True,
) -> BoundedProcessResult:
    """Run one process group without pipe deadlocks.

    By default, ``output`` is the merged stdout and stderr view in operating-
    system write order, as captured by the existing file-backed mode. With
    ``merge_stderr=False``, ``stdout`` and ``stderr`` are independent bounded
    views, and ``output`` is an alias for ``stdout`` because merge order cannot
    be reconstructed from the two spools. The separated per-stream byte and
    truncation fields are populated only in that mode.
    """

    started = time.monotonic()
    with ExitStack() as stack:
        stdout_file = stack.enter_context(tempfile.TemporaryFile(mode="w+b"))
        stderr_file = (
            None
            if merge_stderr
            else stack.enter_context(tempfile.TemporaryFile(mode="w+b"))
        )
        process = subprocess.Popen(
            list(command),
            cwd=cwd,
            stdout=stdout_file,
            stderr=subprocess.STDOUT if merge_stderr else stderr_file,
            env=dict(env) if env is not None else None,
            start_new_session=True,
        )
        timed_out = False
        try:
            try:
                process.wait(timeout=timeout_seconds)
            except subprocess.TimeoutExpired:
                timed_out = True
        finally:
            _stop_process_group(process)
        output, output_bytes, truncated = _read_bounded_output(stdout_file, output_limit_bytes)
        if merge_stderr:
            stdout = stderr = None
            stdout_bytes = stderr_bytes = None
            stdout_truncated = stderr_truncated = None
        else:
            if stderr_file is None:  # pragma: no cover - branch follows merge_stderr
                raise RuntimeError("separated bounded process has no stderr spool")
            stdout = output
            stdout_bytes = output_bytes
            stdout_truncated = truncated
            stderr, stderr_bytes, stderr_truncated = _read_bounded_output(
                stderr_file, output_limit_bytes
            )

    if process.returncode is None:  # pragma: no cover - wait contract guard
        raise RuntimeError("bounded process was not reaped")
    return BoundedProcessResult(
        returncode=process.returncode,
        output=output,
        output_bytes=output_bytes,
        output_truncated=truncated,
        timed_out=timed_out,
        elapsed_seconds=time.monotonic() - started,
        stdout=stdout,
        stderr=stderr,
        stdout_bytes=stdout_bytes,
        stderr_bytes=stderr_bytes,
        stdout_truncated=stdout_truncated,
        stderr_truncated=stderr_truncated,
    )
