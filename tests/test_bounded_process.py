import io
import os
import signal
import sys
import time

import pytest

from skills._shared import bounded_process
from skills._shared.bounded_process import run_bounded


def _assert_process_gone(process_id: int) -> None:
    deadline = time.monotonic() + 1
    while time.monotonic() < deadline:
        try:
            os.kill(process_id, 0)
        except ProcessLookupError:
            break
        time.sleep(0.01)
    with pytest.raises(ProcessLookupError):
        os.kill(process_id, 0)


def test_large_output_is_captured_without_blocking_and_keeps_final_marker(tmp_path):
    marker = "FINAL-BOUNDED-MARKER"
    code = f"import sys; sys.stdout.write('x' * (256 * 1024) + {marker!r})"

    result = run_bounded(
        [sys.executable, "-c", code],
        cwd=tmp_path,
        timeout_seconds=2,
    )

    assert result.timed_out is False
    assert result.returncode == 0
    assert result.output_bytes >= 256 * 1024
    assert marker in result.output


def test_timeout_returns_partial_output_and_a_negative_status(tmp_path):
    pid_path = tmp_path / "timeout.pid"
    code = (
        "import os, sys, time; "
        f"open({str(pid_path)!r}, 'w').write(str(os.getpid())); "
        "print('TIMEOUT-STARTED', flush=True); time.sleep(60)"
    )

    started = time.monotonic()
    try:
        result = run_bounded(
            [sys.executable, "-c", code],
            cwd=tmp_path,
            timeout_seconds=0.1,
        )
    finally:
        if pid_path.is_file():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass

    assert result.timed_out is True
    assert result.elapsed_seconds < 2
    assert time.monotonic() - started < 2
    assert "TIMEOUT-STARTED" in result.output
    assert result.returncode < 0


def test_timeout_kills_same_group_grandchild_holding_output_descriptor(tmp_path):
    grandchild_pid_path = tmp_path / "grandchild.pid"
    grandchild_ready_path = tmp_path / "grandchild.ready"
    grandchild_code = (
        "from pathlib import Path; import signal, time; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        f"Path({str(grandchild_ready_path)!r}).write_text('ready'); "
        "time.sleep(60)"
    )
    parent_code = (
        "import subprocess, sys, time; from pathlib import Path; "
        f"child = subprocess.Popen([sys.executable, '-c', {grandchild_code!r}]); "
        f"Path({str(grandchild_pid_path)!r}).write_text(str(child.pid)); "
        f"ready = Path({str(grandchild_ready_path)!r}); "
        "deadline = time.monotonic() + 2; "
        "exec(\"while not ready.is_file() and time.monotonic() < deadline:\\n    time.sleep(0.005)\"); "
        "assert ready.is_file(); "
        "print('GRANDCHILD-SPAWNED', flush=True); time.sleep(60)"
    )

    try:
        result = run_bounded(
            [sys.executable, "-c", parent_code],
            cwd=tmp_path,
            timeout_seconds=0.1,
        )

        assert result.timed_out is True
        assert "GRANDCHILD-SPAWNED" in result.output
        assert grandchild_pid_path.is_file()
        _assert_process_gone(int(grandchild_pid_path.read_text()))
    finally:
        if grandchild_pid_path.is_file():
            try:
                os.kill(int(grandchild_pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_clean_exit_still_kills_same_group_grandchild(tmp_path):
    grandchild_pid_path = tmp_path / "clean-grandchild.pid"
    grandchild_code = "import time; time.sleep(60)"
    parent_code = (
        "import subprocess, sys; from pathlib import Path; "
        f"child = subprocess.Popen([sys.executable, '-c', {grandchild_code!r}]); "
        f"Path({str(grandchild_pid_path)!r}).write_text(str(child.pid))"
    )

    try:
        result = run_bounded(
            [sys.executable, "-c", parent_code],
            cwd=tmp_path,
            timeout_seconds=2,
        )

        assert result.timed_out is False
        assert result.returncode == 0
        assert grandchild_pid_path.is_file()
        _assert_process_gone(int(grandchild_pid_path.read_text()))
    finally:
        if grandchild_pid_path.is_file():
            try:
                os.kill(int(grandchild_pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_natural_signal_exit_is_not_a_timeout(tmp_path):
    code = "import os, signal; os.kill(os.getpid(), signal.SIGTERM)"

    result = run_bounded(
        [sys.executable, "-c", code],
        cwd=tmp_path,
        timeout_seconds=2,
    )

    assert result.timed_out is False
    assert result.returncode == -signal.SIGTERM
    assert result.terminating_signal == 15


def test_output_limit_retains_head_and_tail(tmp_path):
    code = "import sys; sys.stdout.write('HEAD-' + 'x' * 4096 + '-TAIL')"

    result = run_bounded(
        [sys.executable, "-c", code],
        cwd=tmp_path,
        timeout_seconds=2,
        output_limit_bytes=128,
    )

    assert result.output_bytes > 128
    assert result.output_truncated is True
    assert len(result.output.encode()) <= 128
    assert result.output.startswith("HEAD-")
    assert result.output.endswith("-TAIL")


def test_output_limit_does_not_read_the_whole_spool_into_memory():
    class GuardedSpool(io.BytesIO):
        def read(self, size=-1):
            assert 0 <= size <= 64
            return super().read(size)

    output, output_bytes, truncated = bounded_process._read_bounded_output(
        GuardedSpool(b"HEAD" + b"x" * 4096 + b"TAIL"), 64
    )

    assert output_bytes == 4104
    assert truncated is True
    assert output.startswith("HEAD")
    assert output.endswith("TAIL")
