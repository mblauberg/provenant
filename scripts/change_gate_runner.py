"""Command execution seams for structured and legacy change-gate runners."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import os
import re
import shlex
import tempfile
from pathlib import Path

try:
    from .change_gate_reports import (
        FailureClass,
        classify_structured_report_with_evidence,
    )
    from .bounded_process import run_bounded
except ImportError:  # pragma: no cover - direct script execution fallback
    from bounded_process import run_bounded
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
    timeout_seconds: float = 0.0


# The slowest measured target is 79 seconds. A 180-second cap leaves more than
# 2x headroom for runner variance while turning a genuine hang into a prompt,
# typed gate failure.
DIRECT_PROCESS_TIMEOUT = 180.0

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


def _run_captured(
    arguments: list[str],
    cwd: Path,
    rendered: str,
    timeout_seconds: float,
    runner: Runner | None = None,
    report_path: Path | None = None,
) -> CommandResult:
    sidecar_path = report_path.with_name("pytest-import.json") if report_path else None
    environment = os.environ.copy()
    if runner is Runner.PYTEST:
        assert report_path is not None
        plugin_path = report_path.with_name("_provenant_pytest_import_sidecar.py")
        plugin_path.write_text(_PYTEST_IMPORT_PLUGIN, encoding="utf-8")
        pythonpath = environment.get("PYTHONPATH")
        environment["PYTHONPATH"] = os.pathsep.join(
            part for part in (str(plugin_path.parent), pythonpath) if part
        )
        environment["PROVENANT_PYTEST_IMPORT_SIDECAR"] = str(sidecar_path)
        arguments.extend(["-p", plugin_path.stem])
        rendered = shlex.join(arguments)
    result = run_bounded(
        arguments,
        cwd=cwd,
        timeout_seconds=timeout_seconds,
        env=environment,
    )
    if runner is None:
        classification = (
            FailureClass.TIMEOUT
            if result.timed_out
            else classify_failure(result.returncode, result.output)
        )
        return CommandResult(
            command=rendered,
            returncode=result.returncode,
            output=result.output,
            classification=classification,
            elapsed_seconds=result.elapsed_seconds,
            timed_out=result.timed_out,
            timeout_seconds=timeout_seconds,
        )

    assert report_path is not None
    classification, unresolved_module = classify_structured_report_with_evidence(
        runner,
        report_path,
        result.returncode,
        pytest_sidecar=sidecar_path if runner is Runner.PYTEST else None,
    )
    if result.timed_out:
        classification = FailureClass.TIMEOUT
        unresolved_module = None
    structured_import_evidence = (
        classification in {FailureClass.IMPORT, FailureClass.COLLECTION}
        and unresolved_module is not None
    )
    return CommandResult(
        command=rendered,
        returncode=result.returncode,
        output=result.output,
        classification=classification,
        unresolved_module=unresolved_module,
        structured_import_evidence=structured_import_evidence,
        elapsed_seconds=result.elapsed_seconds,
        timed_out=result.timed_out,
        timeout_seconds=timeout_seconds,
    )


def run_command(
    command: str,
    cwd: Path,
    test_path: str | None = None,
    *,
    runner: Runner | str | None = None,
    timeout_seconds: float,
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
        return _run_captured(arguments, cwd, rendered, timeout_seconds)

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
        return _run_captured(
            arguments,
            cwd,
            rendered,
            timeout_seconds,
            resolved_runner,
            report_path,
        )
