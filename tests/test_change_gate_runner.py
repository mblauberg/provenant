import os
from dataclasses import FrozenInstanceError
from pathlib import Path
import shlex
import sys
import time

import pytest

from _change_gate_helpers import PYTEST_COMMAND
from scripts import change_gate_runner
from scripts.bounded_process import BoundedProcessResult
from scripts.change_gate_runner import (
    FailureClass,
    Runner,
    runner_for_command,
    run_command,
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


def test_structured_runner_uses_the_bounded_process_helper(tmp_path, monkeypatch):
    observed = {}

    def bounded(command, *, cwd, timeout_seconds, env=None, output_limit_bytes=1_048_576):
        observed.update(
            command=command,
            cwd=cwd,
            timeout_seconds=timeout_seconds,
            env=env,
            output_limit_bytes=output_limit_bytes,
        )
        return BoundedProcessResult(1, "captured output", 15, False, False, 0.1)

    monkeypatch.setattr(change_gate_runner, "run_bounded", bounded)
    run_command("pytest tests/example.py", tmp_path, runner=Runner.PYTEST)

    assert observed["cwd"] == tmp_path
    assert observed["timeout_seconds"] == change_gate_runner.DIRECT_PROCESS_TIMEOUT
    assert sum(argument.startswith("--junitxml=") for argument in observed["command"]) == 1
    assert observed["env"]["PROVENANT_PYTEST_IMPORT_SIDECAR"].endswith("pytest-import.json")


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


def test_unstructured_runner_uses_the_bounded_process_helper(tmp_path, monkeypatch):
    observed = {}

    def bounded(command, *, cwd, timeout_seconds, env=None, output_limit_bytes=1_048_576):
        observed.update(
            command=command,
            cwd=cwd,
            timeout_seconds=timeout_seconds,
            env=env,
            output_limit_bytes=output_limit_bytes,
        )
        return BoundedProcessResult(0, "42\n", 3, False, False, 0.1)

    monkeypatch.setattr(change_gate_runner, "run_bounded", bounded)
    result = run_command(f"{sys.executable} -c 'print(42)'", tmp_path)

    assert observed["command"] == [sys.executable, "-c", "print(42)"]
    assert observed["cwd"] == tmp_path
    assert observed["timeout_seconds"] == change_gate_runner.DIRECT_PROCESS_TIMEOUT
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
