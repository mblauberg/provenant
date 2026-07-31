"""Bounded subprocess execution for delivery receipt evidence."""

from __future__ import annotations

import os
import selectors
import signal
import subprocess
import time
from pathlib import Path

POST_EXIT_DRAIN_SECONDS = 0.1


def terminate_process_group(
    process: subprocess.Popen[bytes],
    *,
    timeout_seconds: float,
    error_type: type[ValueError],
) -> None:
    deadline = time.monotonic() + 0.1
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait()
        return
    try:
        process.wait(timeout=0.1)
    except subprocess.TimeoutExpired:
        pass
    remaining = deadline - time.monotonic()
    if remaining > 0:
        time.sleep(remaining)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        raise error_type(
            "evidence process did not exit after SIGKILL within "
            f"{timeout_seconds:g} seconds"
        ) from None


def execute_bounded(
    command: list[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    max_log_bytes: int,
    error_type: type[ValueError],
) -> tuple[int, str, str]:
    if not command:
        raise error_type("evidence run requires a command after --")
    process = subprocess.Popen(
        command,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    assert process.stdout is not None and process.stderr is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, 0)
    selector.register(process.stderr, selectors.EVENT_READ, 1)
    buffers = [bytearray(), bytearray()]
    deadline = time.monotonic() + timeout_seconds
    group_terminated = False
    exit_code: int | None = None
    drain_deadline: float | None = None
    try:
        while selector.get_map():
            if process.poll() is not None and not group_terminated:
                exit_code = process.returncode
                group_terminated = True
                terminate_process_group(
                    process, timeout_seconds=timeout_seconds, error_type=error_type,
                )
                drain_deadline = time.monotonic() + POST_EXIT_DRAIN_SECONDS
            active_deadline = drain_deadline if drain_deadline is not None else deadline
            remaining = active_deadline - time.monotonic()
            if remaining <= 0 and exit_code is None:
                raise error_type(
                    f"evidence command timed out after {timeout_seconds:g} seconds"
                )
            if remaining <= 0:
                break
            for key, _mask in selector.select(min(0.1, remaining)):
                chunk = os.read(key.fileobj.fileno(), 8192)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                buffer = buffers[key.data]
                buffer.extend(chunk)
                if len(buffer) > max_log_bytes:
                    del buffer[:-max_log_bytes]
        if exit_code is None:
            try:
                exit_code = process.wait(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                raise error_type(
                    f"evidence command timed out after {timeout_seconds:g} seconds"
                ) from None
    finally:
        try:
            if not group_terminated:
                group_terminated = True
                terminate_process_group(
                    process, timeout_seconds=timeout_seconds, error_type=error_type,
                )
        finally:
            for key in list(selector.get_map().values()):
                selector.unregister(key.fileobj)
            process.stdout.close()
            process.stderr.close()
            selector.close()
    return (
        exit_code,
        buffers[0].decode("utf-8", errors="replace"),
        buffers[1].decode("utf-8", errors="replace"),
    )
