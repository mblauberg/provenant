"""Bounded POSIX process-group execution for deterministic evidence.

The command is the direct child and starts a dedicated session/process group.
Cleanup signals that group, reaps the direct child unconditionally, and does
not claim that descendants outside the group were contained. Commands that
deliberately daemonise or call ``setsid`` are outside this evidence contract.
"""

from __future__ import annotations

import base64
import hashlib
import os
import selectors
import signal
import subprocess
import time
from pathlib import Path
from typing import Any


POST_EXIT_DRAIN_SECONDS = 0.1
PROCESS_GROUP_GRACE_SECONDS = 0.1
POLL_SECONDS = 0.02
CUSTODY_STATUS = "posix-process-group-cleanup"
UNSUPPORTED_COMMANDS = "Commands that daemonise or call setsid are unsupported."


def _empty_cleanup() -> dict[str, Any]:
    return {
        "strategy": CUSTODY_STATUS,
        "term_sent": False,
        "kill_sent": False,
        "grace_seconds": PROCESS_GROUP_GRACE_SECONDS,
    }


def _signal_group(group_id: int, sig: signal.Signals) -> bool:
    try:
        os.killpg(group_id, sig)
    except ProcessLookupError:
        return False
    return True


def _reap(process: subprocess.Popen[Any]) -> int:
    """Wait for the direct child and reject a non-integer observed status."""
    exit_code = process.wait()
    if isinstance(exit_code, bool) or not isinstance(exit_code, int):
        raise RuntimeError("spawned process was not reaped with an integer exit code")
    return exit_code


def _cleanup_process_group(
    process: subprocess.Popen[Any], *, grace_seconds: float = PROCESS_GROUP_GRACE_SECONDS,
) -> tuple[int, dict[str, Any]]:
    """TERM then KILL the dedicated group, always reaping the direct child."""
    group_id = process.pid
    term_sent = _signal_group(group_id, signal.SIGTERM)
    deadline = time.monotonic() + grace_seconds
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        if process.poll() is None:
            try:
                process.wait(timeout=min(POLL_SECONDS, remaining))
            except subprocess.TimeoutExpired:
                pass
        else:
            time.sleep(min(POLL_SECONDS, remaining))
    kill_sent = _signal_group(group_id, signal.SIGKILL)
    exit_code = _reap(process)
    return exit_code, {
        "strategy": CUSTODY_STATUS,
        "term_sent": term_sent,
        "kill_sent": kill_sent,
        "grace_seconds": grace_seconds,
    }


def _drain(
    selector: selectors.BaseSelector,
    buffers: list[bytearray],
    full_buffers: list[bytearray],
    counts: list[int],
    digests: list[Any],
    truncated: list[bool],
    complete: list[bool],
    max_log_bytes: int,
    timeout: float,
) -> None:
    for key, _ in selector.select(timeout):
        index = key.data
        try:
            chunk = os.read(key.fileobj.fileno(), 8192)
        except OSError:
            chunk = b""
            complete[index] = False
        if not chunk:
            selector.unregister(key.fileobj)
            if complete[index]:
                complete[index] = True
            continue
        counts[index] += len(chunk)
        digests[index].update(chunk)
        full_buffers[index].extend(chunk)
        buffers[index].extend(chunk)
        if len(buffers[index]) > max_log_bytes:
            del buffers[index][:-max_log_bytes]
            truncated[index] = True


def _close_selector(selector: selectors.BaseSelector) -> None:
    for key in list(selector.get_map().values()):
        selector.unregister(key.fileobj)


def execute_bounded(
    command: list[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    max_log_bytes: int,
    error_type: type[ValueError],
) -> dict[str, Any]:
    if not command:
        raise error_type("evidence run requires a command after --")
    if os.name != "posix" or not hasattr(os, "killpg"):
        raise error_type("evidence process-group cleanup requires POSIX")

    process: subprocess.Popen[Any] | None = None
    selector: selectors.BaseSelector | None = None
    cleanup: dict[str, Any] | None = None
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            close_fds=True,
            start_new_session=True,
        )
        assert process.stdout is not None and process.stderr is not None
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ, 0)
        selector.register(process.stderr, selectors.EVENT_READ, 1)

        buffers = [bytearray(), bytearray()]
        full_buffers = [bytearray(), bytearray()]
        counts = [0, 0]
        digests = [hashlib.sha256(), hashlib.sha256()]
        truncated = [False, False]
        complete = [True, True]
        started = time.monotonic()
        timed_out = False
        child_exited_at: float | None = None
        child_exit: int | None = None

        while selector.get_map() or process.poll() is None:
            now = time.monotonic()
            if process.poll() is None:
                remaining = timeout_seconds - (now - started)
                if remaining <= 0:
                    timed_out = True
                    child_exit, cleanup = _cleanup_process_group(process)
                    break
                wait_timeout = min(POLL_SECONDS, remaining)
            else:
                if child_exited_at is None:
                    child_exited_at = now
                    child_exit = _reap(process)
                if not selector.get_map():
                    break
                remaining = POST_EXIT_DRAIN_SECONDS - (now - child_exited_at)
                if remaining <= 0:
                    child_exit, cleanup = _cleanup_process_group(process)
                    break
                wait_timeout = min(POLL_SECONDS, remaining)

            if selector.get_map():
                _drain(
                    selector, buffers, full_buffers, counts, digests, truncated,
                    complete, max_log_bytes, wait_timeout,
                )
            else:
                try:
                    process.wait(timeout=wait_timeout)
                except subprocess.TimeoutExpired:
                    pass

        if child_exit is None:
            child_exit = _reap(process)

        drain_deadline = time.monotonic() + POST_EXIT_DRAIN_SECONDS
        while selector.get_map():
            remaining = drain_deadline - time.monotonic()
            if remaining <= 0:
                if cleanup is None:
                    _ignored_exit, cleanup = _cleanup_process_group(process)
                for key in list(selector.get_map().values()):
                    complete[key.data] = False
                break
            _drain(
                selector, buffers, full_buffers, counts, digests, truncated,
                complete, max_log_bytes, min(POLL_SECONDS, remaining),
            )

        if cleanup is None:
            cleanup = _empty_cleanup()
        if not isinstance(child_exit, int) or isinstance(child_exit, bool):
            raise RuntimeError("bounded process result requires an integer child exit")

        output = []
        for index in range(2):
            output.append({
                "digest": "sha256:" + digests[index].hexdigest(),
                "bytes": counts[index],
                "retained_bytes": len(buffers[index]),
                "truncated": truncated[index],
                "complete": complete[index],
                "captured_b64": base64.b64encode(bytes(full_buffers[index])).decode("ascii"),
                "retained_b64": base64.b64encode(bytes(buffers[index])).decode("ascii"),
            })
        return {
            "exit_code": child_exit,
            "signal": -child_exit if child_exit < 0 else None,
            "timed_out": timed_out,
            "stdout": output[0],
            "stderr": output[1],
            "custody": {
                "status": CUSTODY_STATUS,
                "cleanup": cleanup,
                "unsupported": UNSUPPORTED_COMMANDS,
            },
            "retained_stdout": bytes(buffers[0]).decode("utf-8", errors="replace"),
            "retained_stderr": bytes(buffers[1]).decode("utf-8", errors="replace"),
        }
    except error_type:
        raise
    except Exception as exc:
        if process is not None:
            try:
                _cleanup_process_group(process)
            except Exception:
                try:
                    _reap(process)
                except Exception:
                    pass
        raise error_type(f"bounded process execution failed: {exc}") from exc
    finally:
        if selector is not None:
            _close_selector(selector)
            selector.close()
        if process is not None:
            if process.stdout is not None:
                process.stdout.close()
            if process.stderr is not None:
                process.stderr.close()
            # This is intentionally unconditional: the direct child is the
            # process owner and must be reaped on every terminal path.
            if process.poll() is None:
                try:
                    _cleanup_process_group(process)
                except Exception:
                    try:
                        _reap(process)
                    except Exception:
                        pass
