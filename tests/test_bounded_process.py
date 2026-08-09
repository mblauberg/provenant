import io
import os
import signal
import sys
import time

import pytest

from skills._shared import bounded_process
from skills._shared.bounded_process import run_bounded


# Timeout tests have to outlive interpreter startup. A deadline tuned to an idle
# machine turns into a spawn race on a busy one, and then the test reports a
# missing marker rather than the timeout behaviour it exists to check. Every
# child here sleeps for 60s, so a deadline of a few seconds still proves the
# bound while leaving room for a loaded machine to get the process running.
SPAWN_SAFE_TIMEOUT = 5.0


def test_separated_mode_keeps_stdout_and_stderr_apart(tmp_path):
    code = "import sys; sys.stdout.write('STDOUT'); sys.stderr.write('STDERR')"

    result = run_bounded(
        [sys.executable, "-c", code],
        cwd=tmp_path,
        timeout_seconds=2,
        merge_stderr=False,
    )

    assert result.output == "STDOUT"
    assert result.stdout == "STDOUT"
    assert result.stderr == "STDERR"
    assert result.stdout_bytes == len("STDOUT")
    assert result.stderr_bytes == len("STDERR")
    assert result.stdout_truncated is False
    assert result.stderr_truncated is False


def test_merged_mode_retains_operating_system_write_order(tmp_path):
    code = "import os; os.write(1, b'OUT'); os.write(2, b'ERR')"

    result = run_bounded(
        [sys.executable, "-c", code],
        cwd=tmp_path,
        timeout_seconds=2,
    )

    assert result.output == "OUTERR"
    assert result.output_bytes == len("OUTERR")
    assert result.output_truncated is False
    assert result.stdout is None
    assert result.stderr is None


def test_separated_mode_bounds_each_stream_independently(tmp_path):
    code = (
        "import os; "
        "os.write(1, b'STDOUT-HEAD-' + b'x' * 4096 + b'-STDOUT-TAIL'); "
        "os.write(2, b'STDERR-HEAD-' + b'y' * 4096 + b'-STDERR-TAIL')"
    )

    result = run_bounded(
        [sys.executable, "-c", code],
        cwd=tmp_path,
        timeout_seconds=2,
        output_limit_bytes=128,
        merge_stderr=False,
    )

    assert result.stdout_bytes > 128
    assert result.stderr_bytes > 128
    assert result.stdout_truncated is True
    assert result.stderr_truncated is True
    assert len(result.stdout.encode()) <= 128
    assert len(result.stderr.encode()) <= 128
    assert result.stdout.startswith("STDOUT-HEAD-")
    assert result.stdout.endswith("-STDOUT-TAIL")
    assert result.stderr.startswith("STDERR-HEAD-")
    assert result.stderr.endswith("-STDERR-TAIL")


def test_separated_mode_handles_child_that_writes_only_stderr(tmp_path):
    code = "import os; os.write(2, b'STDERR-ONLY')"

    result = run_bounded(
        [sys.executable, "-c", code],
        cwd=tmp_path,
        timeout_seconds=2,
        merge_stderr=False,
    )

    assert result.output == ""
    assert result.stdout == ""
    assert result.stderr == "STDERR-ONLY"
    assert result.stdout_bytes == 0
    assert result.stderr_bytes == len("STDERR-ONLY")


def _assert_process_gone(process_id: int) -> None:
    deadline = time.monotonic() + 5
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

    # The deadline has to clear interpreter startup, or a loaded machine kills
    # the child before it prints its marker and the test fails for reasons that
    # have nothing to do with the timeout behaviour under test. The child sleeps
    # for 60s, so any deadline well under that still proves the bound.
    started = time.monotonic()
    try:
        result = run_bounded(
            [sys.executable, "-c", code],
            cwd=tmp_path,
            timeout_seconds=SPAWN_SAFE_TIMEOUT,
        )
    finally:
        if pid_path.is_file():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass

    assert result.timed_out is True
    assert result.elapsed_seconds < SPAWN_SAFE_TIMEOUT * 4
    assert time.monotonic() - started < SPAWN_SAFE_TIMEOUT * 4
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
        "deadline = time.monotonic() + 30; "
        "exec(\"while not ready.is_file() and time.monotonic() < deadline:\\n    time.sleep(0.005)\"); "
        "assert ready.is_file(); "
        "print('GRANDCHILD-SPAWNED', flush=True); time.sleep(60)"
    )

    # Two interpreter spawns have to complete before the parent prints its
    # marker, so this deadline needs the most headroom of any test here.
    try:
        result = run_bounded(
            [sys.executable, "-c", parent_code],
            cwd=tmp_path,
            timeout_seconds=SPAWN_SAFE_TIMEOUT,
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
