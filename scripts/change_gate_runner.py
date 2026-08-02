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
import time
from pathlib import Path

try:
    from .change_gate_reports import (
        FailureClass,
        classify_structured_report_with_evidence,
    )
except ImportError:  # pragma: no cover - direct script execution fallback
    from change_gate_reports import FailureClass, classify_structured_report_with_evidence


class Runner(str, Enum):
    PYTEST = "pytest"
    VITEST = "vitest"


@dataclass(frozen=True)
class CommandResult:
    command: str
    returncode: int
    output: str
    classification: FailureClass
    unresolved_module: str | None = None
    structured_import_evidence: bool = False
    elapsed_seconds: float = 0.0
    timed_out: bool = False


# A cap on a target run, so a genuine hang cannot stall the gate forever. It has
# to clear the slowest legitimate target rather than the average one: at 30s a
# hang and a merely slow suite were indistinguishable, and any change touching
# skills/orchestrate/evals/test_cf_dispatch.py was permanently unlandable because
# the reverted run was killed part way and came back as an unclassified SIGTERM
# rather than an assertion. That file takes 76s on macOS but over 300s at its own
# merge base on a Linux runner, so the cap is set well clear of the slowest known
# target rather than just above it. A hang is unbounded and is still caught.
DIRECT_PROCESS_TIMEOUT = 1800.0
PIPE_DRAIN_TIMEOUT = 5.0

_PYTEST_IMPORT_PLUGIN = """import json
import os
from pathlib import Path


_SCHEMA = "provenant.pytest-import-evidence.v1"
_modules = set()


def pytest_exception_interact(node, call, report):
    del node
    if report.when != "collect":
        return
    cause = call.excinfo.value.__cause__
    if not isinstance(cause, ImportError) or not isinstance(cause.name, str):
        return
    if cause.name.strip() == cause.name and cause.name:
        _modules.add(cause.name)


def pytest_sessionfinish(session, exitstatus):
    del session, exitstatus
    sidecar = os.environ.get("PROVENANT_PYTEST_IMPORT_SIDECAR")
    if sidecar is None:
        return
    try:
        Path(sidecar).write_text(
            json.dumps({"schema": _SCHEMA, "modules": sorted(_modules)}),
            encoding="utf-8",
        )
    except OSError:
        return
"""


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
    "fixture '",
    "error at setup of ",
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


def _run_structured(
    arguments: list[str], cwd: Path, rendered: str, runner: Runner, report_path: Path
) -> CommandResult:
    sidecar_path = report_path.with_name("pytest-import.json")
    environment = os.environ.copy()
    if runner is Runner.PYTEST:
        plugin_path = report_path.with_name("_provenant_pytest_import_sidecar.py")
        plugin_path.write_text(_PYTEST_IMPORT_PLUGIN, encoding="utf-8")
        pythonpath = environment.get("PYTHONPATH")
        environment["PYTHONPATH"] = os.pathsep.join(
            part for part in (str(plugin_path.parent), pythonpath) if part
        )
        environment["PROVENANT_PYTEST_IMPORT_SIDECAR"] = str(sidecar_path)
        arguments.extend(["-p", plugin_path.stem])
        rendered = shlex.join(arguments)
    with tempfile.TemporaryFile(mode="w+b") as output_file:
        process = subprocess.Popen(
            arguments,
            cwd=cwd,
            text=True,
            stdout=output_file,
            stderr=subprocess.STDOUT,
            env=environment,
            start_new_session=True,
        )
        started = time.monotonic()
        direct_timed_out = False
        group_closed: bool
        output = ""
        output_read = False
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
            output_file.flush()
            output_file.seek(0)
            output = _text_output(output_file.read())
            output_read = True
        finally:
            if process.poll() is None:
                group_closed = _terminate_process_group(process.pid) and group_closed
                process.kill()
                process.wait(timeout=PIPE_DRAIN_TIMEOUT)

    elapsed_seconds = time.monotonic() - started
    returncode = process.returncode
    if returncode is None:
        returncode = -signal.SIGKILL
    classification, unresolved_module = classify_structured_report_with_evidence(
        runner,
        report_path,
        returncode,
        pytest_sidecar=sidecar_path if runner is Runner.PYTEST else None,
    )
    if direct_timed_out or not group_closed or not output_read:
        classification = FailureClass.UNKNOWN
        unresolved_module = None
    structured_import_evidence = (
        classification in {FailureClass.IMPORT, FailureClass.COLLECTION}
        and unresolved_module is not None
    )
    return CommandResult(
        command=rendered,
        returncode=returncode,
        output=output,
        classification=classification,
        unresolved_module=unresolved_module,
        structured_import_evidence=structured_import_evidence,
        elapsed_seconds=elapsed_seconds,
        timed_out=direct_timed_out,
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
