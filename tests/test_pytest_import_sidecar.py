import json
from pathlib import Path
import subprocess
import sys

from scripts.change_gate_reports import FailureClass, classify_structured_report
from scripts.change_gate_runner import Runner, run_command


def test_body_stdout_stderr_and_module_error_are_not_structured_evidence(tmp_path):
    test_file = tmp_path / "test_body_module_error.py"
    test_file.write_text(
        "import sys\n"
        "def test_body_module_error():\n"
        "    print('ERROR during collection No module named \\\"new_helper\\\"')\n"
        "    print('ModuleNotFoundError: No module named \\\"new_helper\\\"', file=sys.stderr)\n"
        "    raise ModuleNotFoundError(\"No module named 'new_helper'\")\n",
        encoding="utf-8",
    )

    result = run_command(
        f"{sys.executable} -m pytest {{test}} -q",
        tmp_path,
        str(test_file),
        runner=Runner.PYTEST,
    )

    assert result.classification is FailureClass.IMPORT
    assert result.unresolved_module is None
    assert result.structured_import_evidence is False


def test_real_collection_import_uses_sidecar_when_junit_omits_identity(tmp_path):
    (tmp_path / "conftest.py").write_text(
        "import pytest\n"
        "@pytest.hookimpl(tryfirst=True)\n"
        "def pytest_collectreport(report):\n"
        "    if report.failed:\n"
        "        report.longrepr = 'collection failure'\n",
        encoding="utf-8",
    )
    test_file = tmp_path / "test_import.py"
    test_file.write_text(
        "from new_helper import value\n\n"
        "def test_import():\n    assert value == 1\n",
        encoding="utf-8",
    )

    result = run_command(
        f"{sys.executable} -m pytest {{test}} -q",
        tmp_path,
        str(test_file),
        runner=Runner.PYTEST,
    )

    assert result.classification is FailureClass.COLLECTION
    assert result.unresolved_module == "new_helper"
    assert result.structured_import_evidence is True


def test_pytest_sidecar_rejects_multiple_collection_modules(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        '<testsuites><testsuite tests="1" failures="0" errors="1">'
        '<testcase name="collection"><error message="collection failure" />'
        '</testcase></testsuite></testsuites>',
        encoding="utf-8",
    )
    sidecar = tmp_path / "pytest-import.json"
    sidecar.write_text(
        json.dumps({
            "schema": "provenant.pytest-import-evidence.v1",
            "modules": ["one", "two"],
        }),
        encoding="utf-8",
    )

    assert classify_structured_report(
        "pytest", report, 1, include_evidence=True, pytest_sidecar=sidecar
    ) == (FailureClass.COLLECTION, None)


def test_pytest_sidecar_outside_report_directory_is_not_evidence(tmp_path):
    report_directory = tmp_path / "report"
    report_directory.mkdir()
    report = report_directory / "pytest.xml"
    report.write_text(
        '<testsuites><testsuite tests="1" failures="0" errors="1">'
        '<testcase name="collection"><error message="collection failure" />'
        '</testcase></testsuite></testsuites>',
        encoding="utf-8",
    )
    sidecar = tmp_path / "pytest-import.json"
    sidecar.write_text(
        json.dumps({
            "schema": "provenant.pytest-import-evidence.v1",
            "modules": ["new_helper"],
        }),
        encoding="utf-8",
    )

    assert classify_structured_report(
        "pytest", report, 1, include_evidence=True, pytest_sidecar=sidecar
    ) == (FailureClass.COLLECTION, None)


def test_malformed_pytest_sidecar_is_not_evidence(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        '<testsuites><testsuite tests="1" failures="0" errors="1">'
        '<testcase name="collection"><error message="collection failure" />'
        '</testcase></testsuite></testsuites>',
        encoding="utf-8",
    )
    sidecar = tmp_path / "pytest-import.json"
    sidecar.write_text('{"schema":"wrong","modules":["new_helper"]}', encoding="utf-8")

    assert classify_structured_report(
        "pytest", report, 1, include_evidence=True, pytest_sidecar=sidecar
    ) == (FailureClass.COLLECTION, None)


def test_import_helper_right_reason_red_accepts_a_new_helper_without_junit_identity(
    tmp_path, capsys
):
    source = tmp_path / "source"
    (source / "scripts").mkdir(parents=True)
    (source / "tests").mkdir()
    (source / "scripts" / "__init__.py").write_text("", encoding="utf-8")
    (source / "conftest.py").write_text(
        "import pytest\n"
        "@pytest.hookimpl(tryfirst=True)\n"
        "def pytest_collectreport(report):\n"
        "    if report.failed:\n"
        "        report.longrepr = 'collection failure'\n",
        encoding="utf-8",
    )
    test_file = source / "tests" / "test_context_budgets.py"
    test_file.write_text(
        "from scripts.new_helper import value\n\n"
        "def test_budget():\n    assert value == 1\n",
        encoding="utf-8",
    )
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git", "-C", str(source), "-c", "user.name=Gate Test",
            "-c", "user.email=gate-test@example.invalid", "commit", "--quiet", "-m", "base",
        ],
        check=True,
    )
    (source / "scripts" / "new_helper.py").write_text("value = 1\n", encoding="utf-8")
    test_file.write_text(
        "# helper import remains collection-scoped\n"
        "from scripts.new_helper import value\n\n"
        "def test_budget():\n    assert value == 1\n",
        encoding="utf-8",
    )
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)

    result = __import__("scripts.change_gates", fromlist=["main"]).main(
        [
            "right-reason-red",
            "--base", "HEAD",
            "--source-root", str(source),
            "--scratch-root", str(tmp_path / "scratch"),
            "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
            "--test-command-ts", "npm exec vitest run {test}",
            "--mode", "import-helper",
        ]
    )

    assert result == 0
    output = capsys.readouterr().out
    assert "status=ADDED-MODULE" in output
    assert "RIGHT_REASON_RED: PASS" in output
