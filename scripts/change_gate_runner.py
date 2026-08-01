"""Command execution seams for structured and legacy change-gate runners."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import os
import re
import shlex
import signal
import subprocess
import tempfile
from pathlib import Path

try:
    from .change_gate_reports import FailureClass, classify_structured_report
except ImportError:  # pragma: no cover - direct script execution fallback
    from change_gate_reports import FailureClass, classify_structured_report


class Runner(str, Enum):
    PYTEST = "pytest"
    VITEST = "vitest"


@dataclass(frozen=True)
class CommandResult:
    command: str
    returncode: int
    output: str
    classification: FailureClass


DIRECT_PROCESS_TIMEOUT = 30.0
PIPE_DRAIN_TIMEOUT = 5.0


_IMPORT_MARKERS = (
    "importerror",
    "modulenotfounderror",
    "cannot find module",
    "failed to load url",
    "failed to resolve import",
    "err_module_not_found",
    "does not provide an export named",
)
_COLLECTION_MARKERS = (
    "collection error",
    "error during collection",
    "errors during collection",
    "no test files found",
    "no tests collected",
    "test file not found",
)
_SETUP_MARKERS = (
    "fixture ",
    "setup failed",
    "teardown failed",
    "beforeall",
    "before_all",
    "beforeeach",
    "afterall",
    "aftereach",
)
_RUNTIME_ERROR_MARKERS = (
    "typeerror:",
    "referenceerror:",
    "syntaxerror:",
    " is not a function",
    "cannot read properties of",
)
_ASSERTION_RE = re.compile(
    r"(?:assertionerror|assert(?:ion)?\s+(?:error|failed)|expected .+\s+to\s+|\bassert\s+.+\s+failed)",
    re.IGNORECASE,
)
_PYTEST_ASSERT_RE = re.compile(r"(?m)^\s*(?:E\s+)?assert\b")


def classify_failure(returncode: int, output: str) -> FailureClass:
    """Preserve current-main text classification for legacy commands."""

    if returncode == 0:
        return FailureClass.PASS
    folded = output.casefold()
    if any(marker in folded for marker in _IMPORT_MARKERS):
        return FailureClass.IMPORT
    if any(marker in folded for marker in _COLLECTION_MARKERS):
        return FailureClass.COLLECTION
    if any(marker in folded for marker in _SETUP_MARKERS):
        return FailureClass.SETUP
    if any(marker in folded for marker in _RUNTIME_ERROR_MARKERS):
        return FailureClass.UNKNOWN
    if _ASSERTION_RE.search(output) or _PYTEST_ASSERT_RE.search(output):
        return FailureClass.ASSERTION
    return FailureClass.UNKNOWN


def runner_for_command(command: str) -> Runner | None:
    """Recognise only commands whose report contract this module owns."""

    try:
        arguments = shlex.split(command)
    except ValueError:
        return None
    for index, argument in enumerate(arguments):
        if argument == "-m":
            if index + 1 < len(arguments) and arguments[index + 1] == Runner.PYTEST.value:
                return Runner.PYTEST
            continue
        if index and arguments[index - 1] == "-m":
            continue
        if Path(argument).name == Runner.PYTEST.value:
            return Runner.PYTEST
        if Path(argument).name == Runner.VITEST.value:
            return Runner.VITEST
    return None


def _terminate_process_group(process_id: int) -> bool:
    """Close descendants inherited by a gate-owned structured command."""

    try:
        os.killpg(process_id, signal.SIGTERM)
    except ProcessLookupError:
        return True
    except OSError:
        return False
    try:
        os.killpg(process_id, signal.SIGKILL)
    except ProcessLookupError:
        return True
    except OSError:
        return False
    return True


def _text_output(value: object) -> str:
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return value if isinstance(value, str) else ""


def _run_legacy(arguments: list[str], cwd: Path, rendered: str) -> CommandResult:
    """Keep the current-main subprocess.run path for unstructured commands."""

    completed = subprocess.run(
        arguments,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=os.environ.copy(),
        check=False,
    )
    output = completed.stdout or ""
    return CommandResult(
        command=rendered,
        returncode=completed.returncode,
        output=output,
        classification=classify_failure(completed.returncode, output),
    )


def _drain_output(process: subprocess.Popen[str]) -> tuple[str, bool]:
    try:
        output, _ = process.communicate(timeout=PIPE_DRAIN_TIMEOUT)
        return output or "", True
    except subprocess.TimeoutExpired as exc:
        partial = _text_output(exc.output)
        if process.stdout is not None:
            process.stdout.close()
        return partial, False


def _run_structured(
    arguments: list[str], cwd: Path, rendered: str, runner: Runner, report_path: Path
) -> CommandResult:
    process = subprocess.Popen(
        arguments,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=os.environ.copy(),
        start_new_session=True,
    )
    direct_timed_out = False
    group_closed: bool
    output = ""
    try:
        try:
            process.wait(timeout=DIRECT_PROCESS_TIMEOUT)
        except subprocess.TimeoutExpired:
            direct_timed_out = True
            group_closed = _terminate_process_group(process.pid)
            try:
                process.wait(timeout=PIPE_DRAIN_TIMEOUT)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=PIPE_DRAIN_TIMEOUT)
        else:
            group_closed = _terminate_process_group(process.pid)
        output, pipes_drained = _drain_output(process)
    finally:
        if process.poll() is None:
            group_closed = _terminate_process_group(process.pid) and group_closed
            process.kill()
            process.wait(timeout=PIPE_DRAIN_TIMEOUT)
        if process.stdout is not None and not process.stdout.closed:
            process.stdout.close()

    returncode = process.returncode
    if returncode is None:
        returncode = -signal.SIGKILL
    classification = classify_structured_report(runner, report_path, returncode)
    if direct_timed_out or not group_closed or not pipes_drained:
        classification = FailureClass.UNKNOWN
    return CommandResult(
        command=rendered,
        returncode=returncode,
        output=output,
        classification=classification,
    )


def run_command(
    command: str,
    cwd: Path,
    test_path: str | None = None,
    *,
    runner: Runner | str | None = None,
) -> CommandResult:
    arguments = shlex.split(command)
    if test_path:
        arguments = [test_path if argument == "{test}" else argument for argument in arguments]
    resolved_runner: Runner | None = None
    if runner is not None:
        try:
            resolved_runner = runner if isinstance(runner, Runner) else Runner(runner)
        except ValueError:
            rendered = shlex.join(arguments)
            return CommandResult(rendered, 1, "", FailureClass.UNKNOWN_RUNNER)

    rendered = shlex.join(arguments)
    if resolved_runner is None:
        return _run_legacy(arguments, cwd, rendered)

    with tempfile.TemporaryDirectory(prefix="report-", dir=cwd) as report_directory:
        report_path = Path(report_directory) / (
            "pytest.xml" if resolved_runner is Runner.PYTEST else "vitest.json"
        )
        if resolved_runner is Runner.PYTEST:
            arguments.extend([f"--junitxml={report_path}"])
        elif arguments[:3] == ["npm", "exec", "vitest"]:
            arguments.insert(3, "--")
            arguments.extend(["--reporter=json", f"--outputFile={report_path}"])
        elif resolved_runner is Runner.VITEST:
            arguments.extend(["--reporter=json", f"--outputFile={report_path}"])
        rendered = shlex.join(arguments)
        return _run_structured(arguments, cwd, rendered, resolved_runner, report_path)
