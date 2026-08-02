import json
from pathlib import Path
import subprocess
import sys
import pytest

from scripts.change_gate_reports import (
    FailureClass,
    classify_structured_report,
    parse_junit_report,
    parse_vitest_report,
)


def _failure(name):
    return getattr(FailureClass, name)


def _structured_parser(name):
    return {
        "parse_junit_report": parse_junit_report,
        "parse_vitest_report": parse_vitest_report,
    }[name]


def _structured_classifier(*args):
    return classify_structured_report(*args)


def test_junit_failure_record_is_assertion_even_with_collection_marker(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="1" failures="1" errors="0">
        <testcase classname="test_example" name="test_failure">
          <failure message="AssertionError">ERROR during collection: expected value</failure>
        </testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.ASSERTION


def test_junit_runtime_failure_record_is_not_assertion(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="1" failures="1" errors="0">
        <testcase name="test_runtime"><failure type="NameError" message="NameError">AssertionError marker payload</failure></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.UNKNOWN


def test_junit_assertion_traceback_does_not_treat_capture_fixture_as_setup(tmp_path):
    report = tmp_path / "report.xml"
    report.write_text(
        """<testsuite tests="1" errors="0" failures="1">
          <testcase name="test_assertion">
            <failure message="AssertionError: assert value in CaptureFixture output" />
          </testcase>
        </testsuite>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.ASSERTION


def test_junit_non_assertion_type_is_authoritative_even_when_name_contains_assert(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="1" failures="1" errors="0">
        <testcase name="test_runtime"><failure type="NotAssertionError">AssertionError marker payload</failure></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.UNKNOWN


def test_junit_count_mismatch_and_empty_evidence_are_malformed(tmp_path):
    mismatch = tmp_path / "mismatch.xml"
    mismatch.write_text(
        """<testsuites><testsuite tests="2" failures="1" errors="0">
        <testcase><failure type="AssertionError">one</failure></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )
    empty = tmp_path / "empty.xml"
    empty.write_text(
        '<testsuites><testsuite tests="0" failures="0" errors="0" /></testsuites>',
        encoding="utf-8",
    )
    root_mismatch = tmp_path / "root-mismatch.xml"
    root_mismatch.write_text(
        """<testsuites tests="2" failures="0" errors="0">
        <testsuite tests="1" failures="0" errors="0">
          <testcase name="one" />
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    parser = _structured_parser("parse_junit_report")
    assert parser(mismatch) is _failure("MALFORMED")
    assert parser(empty) is _failure("MALFORMED")
    assert parser(root_mismatch) is _failure("MALFORMED")


@pytest.mark.parametrize(
    "payload",
    [
        "ERROR during collection",
        "ModuleNotFoundError: missing",
        "AssertionError: expected value",
    ],
)
def test_junit_failure_payload_markers_do_not_change_assertion_class(payload, tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        f"""<testsuites><testsuite tests="1" failures="1" errors="0">
        <testcase><failure type="AssertionError">{payload}</failure>
        </testcase></testsuite></testsuites>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.ASSERTION


@pytest.mark.parametrize(
    ("error_type", "expected"),
    [
        ("ModuleNotFoundError", FailureClass.IMPORT),
        ("CollectError", FailureClass.COLLECTION),
        ("FixtureLookupError", FailureClass.SETUP),
    ],
)
def test_junit_error_records_classify_by_structured_type(error_type, expected, tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        f"""<testsuites><testsuite tests="1" failures="0" errors="1">
        <testcase><error type="{error_type}">AssertionError and ERROR during collection</error>
        </testcase></testsuite></testsuites>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is expected


def test_junit_suite_error_without_testcase_is_collection(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        '<testsuites><testsuite tests="0" failures="0" errors="1" /></testsuites>',
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.COLLECTION


def test_junit_error_body_is_structured_evidence_without_a_type(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuite tests="1" failures="0" errors="1">
          <testcase name="test_collection"><error>ERROR during collection</error></testcase>
        </testsuite>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.COLLECTION


def test_junit_root_count_mismatch_is_not_hidden_by_equal_other_counts(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites tests="2">
          <testsuite tests="1" failures="0" errors="0">
            <testcase name="one" />
          </testsuite>
        </testsuites>""",
        encoding="utf-8",
    )

    assert _structured_parser("parse_junit_report")(report) is FailureClass.MALFORMED


@pytest.mark.parametrize(
    ("source", "expected"),
    [
        (
            "import module_that_does_not_exist_for_gate",
            FailureClass.IMPORT,
        ),
        (
            "import pytest\n"
            "@pytest.fixture\n"
            "def broken_fixture():\n"
            "    raise RuntimeError('setup')\n"
            "def test_setup(broken_fixture):\n"
            "    assert True\n",
            FailureClass.SETUP,
        ),
        (
            "def test_collection(:\n"
            "    assert True\n",
            FailureClass.COLLECTION,
        ),
    ],
)
def test_pytest_reports_classify_setup_collection_and_import(source, expected, tmp_path):
    test_file = tmp_path / "test_structured_failure.py"
    report = tmp_path / "pytest.xml"
    test_file.write_text(source, encoding="utf-8")
    subprocess.run(
        [sys.executable, "-m", "pytest", str(test_file), "--junitxml", str(report), "-q"],
        cwd=tmp_path,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    assert _structured_parser("parse_junit_report")(report) is expected


def test_vitest_assertion_record_ignores_failure_message_markers(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 1,
                "numFailedTests": 1,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                        {
                            "name": "test/example.test.ts",
                            "status": "failed",
                            "message": "Cannot find package runtime failure",
                            "assertionResults": [
                            {
                                "status": "failed",
                                "ancestorTitles": [],
                                "title": "marker payload",
                                "failureMessages": [
                                    "ERROR during collection ModuleNotFoundError AssertionError"
                                ],
                            }
                        ],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is FailureClass.MIXED


def test_vitest_file_message_cannot_establish_assertion_without_assertion_record(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 0,
                "numFailedTests": 0,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                    {
                        "name": "test/runtime.test.ts",
                        "status": "failed",
                        "message": "AssertionError: runtime payload",
                        "assertionResults": [],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is FailureClass.UNKNOWN


def test_vitest_reporter_hook_stack_precedes_failed_assertion_status(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 1,
                "numFailedTests": 1,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                    {
                        "name": "test/example.test.ts",
                        "status": "failed",
                        "message": "",
                        "assertionResults": [
                            {
                                "status": "failed",
                                "ancestorTitles": [],
                                "title": "hooked test",
                                "failureMessages": [
                                    "Error: hook failure\n"
                                    "    at runHook (node_modules/@vitest/runner/index.js:1:1)"
                                ],
                            }
                        ],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is _failure("HOOK")


def test_vitest_assertion_message_with_hook_word_is_not_hook_evidence(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 1,
                "numFailedTests": 1,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                    {
                        "name": "test/example.test.ts",
                        "status": "failed",
                        "assertionResults": [
                            {
                                "status": "failed",
                                "ancestorTitles": [],
                                "failureMessages": [
                                    "AssertionError: expected beforeEach marker to be preserved"
                                ],
                            }
                        ],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is FailureClass.ASSERTION


def test_vitest_generic_runtime_failure_is_not_assertion(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 1,
                "numFailedTests": 1,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                    {
                        "name": "test/example.test.ts",
                        "status": "failed",
                        "assertionResults": [
                            {
                                "status": "failed",
                                "ancestorTitles": [],
                                "failureMessages": [
                                    "Error: boom\\n    at test (test/example.test.ts:1:1)"
                                ],
                            }
                        ],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is FailureClass.UNKNOWN


def test_vitest_passing_record_is_not_reclassified_as_a_hook(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": True,
                "numTotalTests": 1,
                "numFailedTests": 0,
                "numFailedTestSuites": 0,
                "numPassedTests": 1,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 1,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                    {
                        "name": "test/passing.test.ts",
                        "status": "passed",
                        "assertionResults": [
                            {"status": "passed", "ancestorTitles": []}
                        ],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is FailureClass.PASS


def test_vitest_empty_failed_suite_is_collection_evidence(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 0,
                "numFailedTests": 0,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                    {
                        "name": "test/empty.test.ts",
                        "status": "failed",
                        "assertionResults": [],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is FailureClass.COLLECTION


@pytest.mark.parametrize(
    ("error_type", "expected"),
    [
        ("HookError", getattr(FailureClass, "HOOK", FailureClass.UNKNOWN)),
        ("ModuleNotFoundError", FailureClass.IMPORT),
        ("Error", FailureClass.UNKNOWN),
    ],
)
def test_vitest_non_assertion_records_fail_closed(error_type, expected, tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 0,
                "numFailedTests": 0,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [
                    {
                        "name": "test/example.test.ts",
                        "status": "failed",
                        "errorType": error_type,
                        "assertionResults": [],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is expected


@pytest.mark.parametrize(
    ("result", "expected"),
    [
        (
            {
                "status": "failed",
                "message": "hook failure",
                "assertionResults": [{"status": "skipped", "ancestorTitles": []}],
            },
            "HOOK",
        ),
        (
            {
                "status": "failed",
                "message": "Cannot find package 'missing-module' imported from test.ts",
                "assertionResults": [],
            },
            "IMPORT",
        ),
        (
            {"status": "failed", "message": "Error: generic failure", "assertionResults": []},
            "UNKNOWN",
        ),
    ],
)
def test_vitest_actual_hook_import_and_generic_records(result, expected, tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 0 if not result["assertionResults"] else 1,
                "numFailedTests": 0,
                "numFailedTestSuites": 1,
                "numPassedTests": 0,
                "numPendingTests": 1 if result["assertionResults"] else 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 1,
                "testResults": [{"name": "test/example.test.ts", **result}],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is _failure(expected)


def test_structured_results_classify_mixed_and_unusable_evidence(tmp_path):
    mixed = tmp_path / "mixed.xml"
    mixed.write_text(
        """<testsuites><testsuite tests="2" failures="1" errors="1">
        <testcase><failure type="AssertionError">one</failure></testcase>
        <testcase><error type="FixtureLookupError">two</error></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )
    malformed = tmp_path / "malformed.json"
    malformed.write_text("{not-json", encoding="utf-8")

    junit_parser = _structured_parser("parse_junit_report")
    vitest_parser = _structured_parser("parse_vitest_report")
    assert junit_parser(mixed) is _failure("MIXED")
    assert vitest_parser(malformed) is _failure("MALFORMED")
    assert vitest_parser(tmp_path / "missing.json") is _failure("MISSING")
    empty = tmp_path / "empty.json"
    empty.write_text(json.dumps({"success": True, "testResults": []}), encoding="utf-8")
    missing_status = tmp_path / "missing-status.json"
    missing_status.write_text(
        json.dumps(
            {
                "success": True,
                "testResults": [{"name": "test.ts", "assertionResults": []}],
            }
        ),
        encoding="utf-8",
    )
    assert vitest_parser(empty) is _failure("MALFORMED")
    assert vitest_parser(missing_status) is _failure("MALFORMED")
    inconsistent_counts = tmp_path / "inconsistent-counts.json"
    inconsistent_counts.write_text(
        json.dumps(
            {
                "success": True,
                "numTotalTests": 1,
                "numFailedTests": 0,
                "numFailedTestSuites": 0,
                "numPassedTests": 1,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 0,
                "testResults": [
                    {
                        "name": "test.ts",
                        "status": "passed",
                        "assertionResults":[{"status": "passed", "ancestorTitles": []}],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    assert vitest_parser(inconsistent_counts) is _failure("MALFORMED")
    assert (
        _structured_classifier("unknown", mixed, 1)
        is _failure("UNKNOWN_RUNNER")
    )


def test_structured_classifier_rejects_a_nonzero_pass_report(tmp_path):
    report = tmp_path / "pass.xml"
    report.write_text(
        """<testsuite tests="1" failures="0" errors="0">
          <testcase name="one" />
        </testsuite>""",
        encoding="utf-8",
    )

    assert classify_structured_report("pytest", report, 1) is FailureClass.UNKNOWN


def test_vitest_assertion_and_hook_records_are_mixed(tmp_path):
    report = tmp_path / "mixed.json"
    report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 1,
                "numFailedTests": 1,
                "numFailedTestSuites": 2,
                "numPassedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numPassedTestSuites": 0,
                "numPendingTestSuites": 0,
                "numTotalTestSuites": 2,
                "testResults": [
                    {
                        "name": "assertion.test.ts",
                        "status": "failed",
                        "assertionResults": [
                            {
                                "status": "failed",
                                "ancestorTitles": [],
                                "title": "assertion",
                                "failureMessages": ["AssertionError: assertion"],
                            }
                        ],
                    },
                    {
                        "name": "hook.test.ts",
                        "status": "failed",
                        "errorType": "HookError",
                        "assertionResults": [],
                    },
                ],
            }
        ),
        encoding="utf-8",
    )

    assert _structured_parser("parse_vitest_report")(report) is _failure("MIXED")


def test_pytest_runtime_exception_message_does_not_become_assertion(tmp_path):
    test_file = tmp_path / "test_runtime.py"
    report = tmp_path / "pytest.xml"
    test_file.write_text(
        "def test_runtime():\n"
        "    raise ValueError('AssertionError: payload only')\n",
        encoding="utf-8",
    )
    subprocess.run(
        [sys.executable, "-m", "pytest", str(test_file), "--junitxml", str(report), "-q"],
        cwd=tmp_path,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    assert parse_junit_report(report) is FailureClass.MIXED
