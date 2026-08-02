import os
from dataclasses import FrozenInstanceError
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time

import pytest

from _change_gate_helpers import PYTEST_COMMAND
from scripts import change_gate_runner
from scripts.change_gate_runner import (
    FailureClass,
    Runner,
    runner_for_command,
    run_command,
    _drain_output,
    _terminate_process_group,
)


ROOT = Path(__file__).resolve().parents[1]


def _runner(name):
    return getattr(Runner, name)


def _failure(name):
    return getattr(FailureClass, name)


def _run_with_runner(command, cwd, test_path=None, runner=None):
    return run_command(command, cwd, test_path, runner=runner)


def test_run_command_does_not_interpret_shell_metacharacters(tmp_path):
    marker = tmp_path / "shell-was-used"
    command = f'{sys.executable} -c "import sys; sys.exit(0)" ; touch {marker}'

    result = run_command(command, tmp_path)

    assert result.returncode == 0
    assert marker.exists() is False


def test_known_runner_missing_report_and_unknown_runner_fail_closed(tmp_path):
    missing = _run_with_runner(
        f'{sys.executable} -c "raise SystemExit(1)"',
        tmp_path,
        runner=_runner("PYTEST"),
    )
    unknown = _run_with_runner(
        f'{sys.executable} -c "raise SystemExit(1)"',
        tmp_path,
        runner="custom-runner",
    )

    assert missing.classification is _failure("MISSING")
    assert unknown.classification is _failure("UNKNOWN_RUNNER")


def test_command_result_is_frozen():
    result = change_gate_runner.CommandResult("test", 0, "", FailureClass.PASS)

    try:
        result.returncode = 1
    except FrozenInstanceError:
        pass
    else:
        assert False, "CommandResult accepted mutation"


def test_structured_runner_uses_file_output_and_private_process_group(tmp_path, monkeypatch):
    observed = {}

    class Process:
        pid = 1234
        returncode = 0
        stdout = None

        def wait(self, timeout):
            del timeout

        def poll(self):
            return self.returncode

        def communicate(self, timeout):
            del timeout
            return "", None

    def start(arguments, **kwargs):
        del arguments
        observed.update(kwargs)
        return Process()

    monkeypatch.setattr(change_gate_runner.subprocess, "Popen", start)
    monkeypatch.setattr(change_gate_runner, "_terminate_process_group", lambda _: True)

    change_gate_runner._run_structured(
        ["pytest", "tests/example.py"],
        tmp_path,
        "pytest tests/example.py",
        Runner.PYTEST,
        tmp_path / "missing.xml",
    )

    assert observed["text"] is True
    assert observed["start_new_session"] is True
    assert observed["stdout"] is not subprocess.PIPE


def test_legacy_success_is_classified_as_pass(tmp_path):
    assert run_command(f"{sys.executable} -c 'print(42)'", tmp_path).classification is FailureClass.PASS


def test_legacy_nonzero_exit_is_returned_without_check_true_raising(tmp_path):
    try:
        result = run_command(f"{sys.executable} -c 'raise SystemExit(3)'", tmp_path)
    except Exception as error:  # pragma: no cover - mutation contract
        assert False, f"legacy runner raised instead of returning a result: {error}"

    assert result.returncode == 3


def test_legacy_pytest_assertion_with_fixture_repr_is_assertion(tmp_path):
    test_file = tmp_path / "test_fixture_assertion.py"
    test_file.write_text(
        "def test_fixture_assertion(capsys):\n"
        "    assert False\n",
        encoding="utf-8",
    )

    result = run_command(PYTEST_COMMAND, tmp_path, str(test_file))

    assert "CaptureFixture" in result.output
    assert result.classification is FailureClass.ASSERTION


def test_legacy_pytest_missing_fixture_is_setup(tmp_path):
    test_file = tmp_path / "test_missing_fixture.py"
    test_file.write_text(
        "def test_missing_fixture(missing_fixture):\n"
        "    assert missing_fixture\n",
        encoding="utf-8",
    )

    result = run_command(PYTEST_COMMAND, tmp_path, str(test_file))

    assert "fixture 'missing_fixture' not found" in result.output
    assert result.classification is FailureClass.SETUP


def test_legacy_pytest_fixture_setup_failure_is_setup(tmp_path):
    test_file = tmp_path / "test_fixture_setup.py"
    test_file.write_text(
        "import pytest\n"
        "\n"
        "@pytest.fixture\n"
        "def broken_fixture():\n"
        "    raise RuntimeError('boom')\n"
        "\n"
        "def test_fixture_setup(broken_fixture):\n"
        "    assert broken_fixture\n",
        encoding="utf-8",
    )

    result = run_command(PYTEST_COMMAND, tmp_path, str(test_file))

    assert "ERROR at setup of test_fixture_setup" in result.output
    assert result.classification is FailureClass.SETUP


def test_run_command_replaces_only_the_test_placeholder(tmp_path):
    target = sys.executable
    code = f"import sys; assert sys.argv[1] == {target!r}"
    command = f"{sys.executable} -c {shlex.quote(code)} {{test}}"

    result = run_command(command, tmp_path, str(target))

    assert result.returncode == 0


def test_run_command_uses_pytest_report_not_process_output(tmp_path):
    test_file = tmp_path / "test_marker_payload.py"
    test_file.write_text(
        "def test_marker_payload():\n"
        "    assert False, 'ERROR during collection ModuleNotFoundError AssertionError'\n",
        encoding="utf-8",
    )

    result = _run_with_runner(
        PYTEST_COMMAND,
        tmp_path,
        str(test_file),
        runner=_runner("PYTEST"),
    )

    assert result.classification is _failure("MIXED")


def test_run_command_uses_vitest_json_report(tmp_path):
    del tmp_path
    result = _run_with_runner(
        "npm exec vitest run {test} -t 'preserves capability failure classes'",
        ROOT,
        "runtime/agent-fabric-console/tests/controller.test.ts",
        runner=_runner("VITEST"),
    )

    assert result.returncode == 0
    assert result.classification is FailureClass.PASS
    assert "--reporter=json" in result.command
    assert "npm exec vitest -- run" in result.command


def test_structured_runner_pass_output_is_text(tmp_path):
    test_file = tmp_path / "test_pass.py"
    test_file.write_text("def test_pass():\n    assert True\n", encoding="utf-8")

    result = run_command(PYTEST_COMMAND, tmp_path, str(test_file), runner=Runner.PYTEST)

    assert result.classification is FailureClass.PASS
    assert isinstance(result.output, str)


def test_run_structured_does_not_deadlock_on_large_output(tmp_path, monkeypatch):
    monkeypatch.setattr(change_gate_runner, "DIRECT_PROCESS_TIMEOUT", 0.1)
    code = "import sys; sys.stdout.write('x' * 131072 + '\\nFINAL-LINE\\n')"

    result = run_command(
        f"{sys.executable} -c {shlex.quote(code)}",
        tmp_path,
        runner=Runner.PYTEST,
    )

    assert result.returncode == 0
    assert result.timed_out is False
    assert len(result.output) > 64 * 1024
    assert result.output.endswith("FINAL-LINE\n")


def test_structured_runner_closes_descendants_that_hold_output_pipes(tmp_path):
    child_pid = tmp_path / "child.pid"
    child_code = (
        "from pathlib import Path; import os, time; "
        f"Path({str(child_pid)!r}).write_text(str(os.getpid())); time.sleep(60)"
    )
    parent_code = (
        "import subprocess, sys, time\n"
        "from pathlib import Path\n"
        f"subprocess.Popen([sys.executable, '-c', {child_code!r}])\n"
        "deadline = time.monotonic() + 2\n"
        f"while not Path({str(child_pid)!r}).is_file() and time.monotonic() < deadline:\n"
        "    time.sleep(0.01)\n"
    )

    started = time.monotonic()
    result = run_command(
        f"{sys.executable} -c {shlex.quote(parent_code)}",
        tmp_path,
        runner=Runner.PYTEST,
    )
    elapsed = time.monotonic() - started

    assert elapsed < 3
    assert result.classification is not FailureClass.ASSERTION
    for _ in range(100):
        if child_pid.is_file():
            break
        time.sleep(0.01)
    assert child_pid.is_file()
    child_process_id = int(child_pid.read_text(encoding="utf-8"))
    for _ in range(100):
        try:
            os.kill(child_process_id, 0)
        except ProcessLookupError:
            break
        time.sleep(0.01)
    try:
        os.kill(child_process_id, 0)
    except ProcessLookupError:
        child_is_alive = False
    else:
        child_is_alive = True
    assert child_is_alive is False


def test_structured_runner_bounds_a_direct_process_that_does_not_exit(tmp_path, monkeypatch):
    monkeypatch.setattr(change_gate_runner, "DIRECT_PROCESS_TIMEOUT", 0.1)

    started = time.monotonic()
    result = run_command(
        f"{sys.executable} -c {shlex.quote('import time; time.sleep(60)')}",
        tmp_path,
        runner=Runner.PYTEST,
    )

    assert time.monotonic() - started < 3
    assert result.classification is FailureClass.UNKNOWN


@pytest.mark.parametrize(
    ("side_effect", "expected"),
    [
        (ProcessLookupError(), True),
        (OSError(), False),
    ],
)
def test_terminate_process_group_reports_term_signal_failures(monkeypatch, side_effect, expected):
    def killpg(process_id, signal_number):
        del process_id, signal_number
        raise side_effect

    monkeypatch.setattr(change_gate_runner.os, "killpg", killpg)

    assert _terminate_process_group(1234) is expected


@pytest.mark.parametrize(
    ("side_effect", "expected"),
    [
        (ProcessLookupError(), True),
        (OSError(), False),
    ],
)
def test_terminate_process_group_reports_kill_signal_failures(monkeypatch, side_effect, expected):
    calls = 0

    def killpg(process_id, signal_number):
        nonlocal calls
        del process_id, signal_number
        calls += 1
        if calls == 2:
            raise side_effect

    monkeypatch.setattr(change_gate_runner.os, "killpg", killpg)

    assert _terminate_process_group(1234) is expected


def test_terminate_process_group_reports_success(monkeypatch):
    monkeypatch.setattr(change_gate_runner.os, "killpg", lambda *_: None)

    assert _terminate_process_group(1234) is True


def test_drain_output_reports_a_successful_pipe_drain():
    class Process:
        stdout = None

        def communicate(self, timeout):
            assert timeout == change_gate_runner.PIPE_DRAIN_TIMEOUT
            return "output", None

    assert _drain_output(Process()) == ("output", True)


def test_drain_output_reports_a_timed_out_pipe_drain():
    class Stdout:
        closed = False

        def close(self):
            self.closed = True

    class Process:
        stdout = Stdout()

        def communicate(self, timeout):
            del timeout
            raise subprocess.TimeoutExpired("command", 1, output=b"partial")

    process = Process()
    assert _drain_output(process) == ("partial", False)
    assert process.stdout.closed is True


def test_legacy_runner_keeps_the_subprocess_run_path(tmp_path, monkeypatch):
    called = False

    def fail_if_structured(*args, **kwargs):
        nonlocal called
        called = True
        raise AssertionError("legacy command used structured custody")

    monkeypatch.setattr(change_gate_runner, "_run_structured", fail_if_structured)
    result = run_command(f"{sys.executable} -c 'print(42)'", tmp_path)

    assert called is False
    assert result.returncode == 0
    assert result.output.strip() == "42"


@pytest.mark.parametrize(
    ("command", "expected"),
    [
        ("pytest {test}", Runner.PYTEST),
        (f"{sys.executable} -m pytest {{test}}", Runner.PYTEST),
        ("npm exec vitest run {test}", Runner.VITEST),
        ("python -c 'print(\"pytest\")' {test}", None),
        ("python -c 'print(\"vitest\")' {test}", None),
    ],
)
def test_runner_detection_is_bound_to_the_configured_command(command, expected):
    assert runner_for_command(command) is expected
