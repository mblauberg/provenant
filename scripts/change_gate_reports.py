"""Structured report parsing for the change gates.

Only typed JUnit and Vitest records can establish an assertion red.  Missing,
malformed, conflicting, or otherwise non-assertion evidence stays outside that
class so callers can fail closed.
"""

from __future__ import annotations

import json
from enum import Enum
import re
import xml.etree.ElementTree as ET
from pathlib import Path


class FailureClass(str, Enum):
    PASS = "pass"
    ASSERTION = "assertion-failure"
    IMPORT = "import-error"
    COLLECTION = "collection-error"
    SETUP = "fixture-or-setup-error"
    HOOK = "hook-error"
    MIXED = "mixed-results"
    MALFORMED = "malformed-report"
    MISSING = "missing-report"
    UNKNOWN_RUNNER = "unknown-runner"
    UNKNOWN = "unclassified-failure"


_ASSERTION_RE = re.compile(
    r"(?:assertionerror|assert(?:ion)?\s+(?:error|failed)|expected .+\s+to\s+|\bassert\s+.+\s+failed)",
    re.IGNORECASE,
)


def _combine_failure_classes(classes: list[FailureClass]) -> FailureClass:
    if not classes:
        return FailureClass.PASS
    distinct = set(classes)
    if len(distinct) > 1:
        return FailureClass.MIXED
    return classes[0]


def _structured_error_class(error_type: object, default: FailureClass) -> FailureClass:
    if not isinstance(error_type, str):
        return default
    folded = error_type.casefold()
    if "module" in folded or "import" in folded:
        return FailureClass.IMPORT
    if "collect" in folded:
        return FailureClass.COLLECTION
    if folded.startswith("fixture") or re.search(r"\b(?:setup|teardown)\b", folded):
        return FailureClass.SETUP
    if "hook" in folded:
        return FailureClass.HOOK
    return default


def _structured_message_class(message: object, *, allow_assertion: bool = True) -> FailureClass:
    if not isinstance(message, str) or not message.strip():
        return FailureClass.UNKNOWN
    folded = message.casefold()
    classes: list[FailureClass] = []
    if any(marker in folded for marker in ("cannot find package", "cannot find module", "failed to resolve", "modulenotfounderror", "importerror")):
        classes.append(FailureClass.IMPORT)
    if any(
        marker in folded
        for marker in (
            "collection error",
            "error during collection",
            "errors during collection",
            "no test files found",
            "no tests collected",
        )
    ):
        classes.append(FailureClass.COLLECTION)
    if re.search(r"\bfixture\b", folded) or any(
        marker in folded for marker in ("setup failed", "teardown failed")
    ):
        classes.append(FailureClass.SETUP)
    if any(
        marker in folded
        for marker in ("hook failure", "beforeall", "beforeeach", "afterall", "aftereach")
    ):
        classes.append(FailureClass.HOOK)
    if re.search(r"\b(?:valueerror|typeerror|nameerror|syntaxerror|referenceerror|runtimeerror|keyerror|indexerror|attributeerror)\s*:", folded):
        classes.append(FailureClass.UNKNOWN)
    if allow_assertion and (folded.lstrip().startswith("assert ") or _ASSERTION_RE.search(message)):
        classes.append(FailureClass.ASSERTION)
    return _combine_failure_classes(classes) if classes else FailureClass.UNKNOWN


def _junit_failure_class(record: ET.Element, *, include_text: bool = False) -> FailureClass:
    """Classify one JUnit failure/error with ``type`` as the authority."""

    error_type = record.attrib.get("type")
    if isinstance(error_type, str) and error_type.strip():
        typed = _structured_error_class(error_type, FailureClass.UNKNOWN)
        folded_type = error_type.casefold().strip()
        if folded_type in {"assertion", "assertionerror", "assertionfailure"} or folded_type.endswith(
            ".assertionerror"
        ):
            return FailureClass.ASSERTION
        return typed

    message_attribute = record.attrib.get("message")
    if include_text and isinstance(message_attribute, str) and "collection" in message_attribute.casefold():
        detail = " ".join((message_attribute, "".join(record.itertext()))).casefold()
        if "importerror" in detail or "modulenotfounderror" in detail:
            return FailureClass.IMPORT
        return FailureClass.COLLECTION
    if include_text and isinstance(message_attribute, str) and any(
        marker in message_attribute.casefold() for marker in ("setup", "teardown", "fixture")
    ):
        return FailureClass.SETUP

    message_values = [message_attribute]
    if include_text:
        message_values.append("".join(record.itertext()))
    message = " ".join(
        value
        for value in message_values
        if isinstance(value, str) and value.strip()
    )
    if not include_text and "capturefixture" in message.casefold() and _ASSERTION_RE.search(message):
        return FailureClass.ASSERTION
    return _structured_message_class(message)


def _xml_count(element: ET.Element, field: str) -> int | None:
    try:
        value = int(element.attrib.get(field, "0"))
    except ValueError:
        return None
    return value if value >= 0 else None


def parse_junit_report(report: Path) -> FailureClass:
    """Classify pytest's JUnit records without inspecting process output."""

    if not report.is_file():
        return FailureClass.MISSING
    try:
        root = ET.parse(report).getroot()
    except (ET.ParseError, OSError):
        return FailureClass.MALFORMED

    def tag(node: ET.Element) -> str:
        return node.tag.rsplit("}", 1)[-1]

    if tag(root) not in {"testsuites", "testsuite"}:
        return FailureClass.MALFORMED
    suites = [root] if tag(root) == "testsuite" else [
        node for node in root.iter() if tag(node) == "testsuite"
    ]
    if not suites:
        return FailureClass.MALFORMED

    classes: list[FailureClass] = []
    reported = {field: 0 for field in ("tests", "errors", "failures")}
    testcase_count = 0
    for suite in suites:
        counts = {field: _xml_count(suite, field) for field in reported}
        if any(value is None for value in counts.values()):
            return FailureClass.MALFORMED
        reported = {field: reported[field] + counts[field] for field in reported}
        suite_testcase_count = 0
        suite_error_count = 0
        suite_failure_count = 0
        for testcase in suite:
            if tag(testcase) != "testcase":
                continue
            suite_testcase_count += 1
            for record in testcase:
                record_tag = tag(record)
                if record_tag == "failure":
                    suite_failure_count += 1
                    classes.append(_junit_failure_class(record))
                elif record_tag == "error":
                    suite_error_count += 1
                    classes.append(_junit_failure_class(record, include_text=True))
        suite_level_error = (
            suite_testcase_count == 0
            and counts["tests"] == 0
            and counts["failures"] == 0
            and counts["errors"] == 1
            and suite_error_count == 0
        )
        if (
            suite_testcase_count != counts["tests"]
            or suite_failure_count != counts["failures"]
            or (suite_error_count != counts["errors"] and not suite_level_error)
        ):
            return FailureClass.MALFORMED
        testcase_count += suite_testcase_count

    if tag(root) == "testsuites":
        for field, total in reported.items():
            if field in root.attrib:
                root_total = _xml_count(root, field)
                if root_total is None or root_total != total:
                    return FailureClass.MALFORMED

    if classes:
        return _combine_failure_classes(classes)
    if not testcase_count and not reported["errors"] and not reported["failures"]:
        return FailureClass.MALFORMED
    if reported["failures"]:
        return FailureClass.MALFORMED
    if reported["errors"]:
        return FailureClass.COLLECTION if reported["tests"] == 0 else FailureClass.UNKNOWN
    return FailureClass.PASS


def _vitest_file_error_classes(result: dict[str, object]) -> list[FailureClass]:
    """Return file-level error evidence independently of assertion records."""

    error = result.get("error")
    error_type: object = result.get("errorType") or result.get("type")
    error_message: object = result.get("message")
    if isinstance(error, dict):
        error_type = error_type or error.get("name") or error.get("type")
        error_message = error_message or error.get("message")

    if isinstance(error_type, str) and error_type.strip():
        return [_structured_error_class(error_type, FailureClass.UNKNOWN)]
    if isinstance(error_message, str) and error_message.strip():
        return [_structured_message_class(error_message, allow_assertion=False)]
    return []


def parse_vitest_report(report: Path) -> FailureClass:
    """Classify Vitest's JSON records without inspecting process output."""

    if not report.is_file():
        return FailureClass.MISSING
    try:
        document = json.loads(report.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return FailureClass.MALFORMED
    if not isinstance(document, dict):
        return FailureClass.MALFORMED
    if not isinstance(document.get("success"), bool) or not isinstance(
        document.get("testResults"), list
    ):
        return FailureClass.MALFORMED
    required_counts = (
        "numTotalTests",
        "numPassedTests",
        "numFailedTests",
        "numPendingTests",
        "numTodoTests",
        "numTotalTestSuites",
        "numPassedTestSuites",
        "numFailedTestSuites",
        "numPendingTestSuites",
    )
    for key in required_counts:
        value = document.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            return FailureClass.MALFORMED

    test_results = document["testResults"]
    if not test_results:
        return FailureClass.MALFORMED
    classes: list[FailureClass] = []
    assertion_count = 0
    assertion_counts = {status: 0 for status in ("passed", "failed", "skipped", "pending", "todo")}
    suite_states: dict[tuple[str, ...], set[str]] = {}
    for result in test_results:
        if (
            not isinstance(result, dict)
            or not isinstance(result.get("name"), str)
            or not result["name"]
            or not isinstance(result.get("assertionResults"), list)
        ):
            return FailureClass.MALFORMED
        record_status = result.get("status")
        if record_status not in {"passed", "failed", "skipped", "pending"}:
            return FailureClass.MALFORMED
        assertions = result["assertionResults"]
        assertion_count += len(assertions)
        failed_in_result = 0
        suite_paths: set[tuple[str, ...]] = {(result["name"],)}
        classes.extend(_vitest_file_error_classes(result))
        for assertion in assertions:
            if not isinstance(assertion, dict) or assertion.get("status") not in {
                "passed",
                "failed",
                "skipped",
                "pending",
                "todo",
            }:
                return FailureClass.MALFORMED
            ancestor_titles = assertion.get("ancestorTitles")
            if not isinstance(ancestor_titles, list) or any(
                not isinstance(title, str) for title in ancestor_titles
            ):
                return FailureClass.MALFORMED
            for depth in range(1, len(ancestor_titles) + 1):
                suite_paths.add((result["name"], *ancestor_titles[:depth]))
            assertion_counts[assertion["status"]] += 1
            if assertion["status"] == "failed":
                failed_in_result += 1
                failure_messages = assertion.get("failureMessages", [])
                if failure_messages is None:
                    failure_messages = []
                if not isinstance(failure_messages, list) or any(
                    not isinstance(message, str) for message in failure_messages
                ):
                    return FailureClass.MALFORMED
                if any(
                    re.search(
                        r"(?m)^\s*at\s+(?:runhook|beforeeach|aftereach|beforeall|afterall)(?:\s|\()",
                        message.casefold(),
                    )
                    for message in failure_messages
                ):
                    classes.append(FailureClass.HOOK)
                elif any("assertionerror" in message.casefold() for message in failure_messages):
                    classes.append(FailureClass.ASSERTION)
                else:
                    classes.append(FailureClass.UNKNOWN)
        suite_state = (
            "failed"
            if record_status == "failed" or failed_in_result
            else "pending"
            if record_status in {"skipped", "pending"}
            else "passed"
        )
        for suite_path in suite_paths:
            suite_states.setdefault(suite_path, set()).add(suite_state)
        if failed_in_result and record_status != "failed":
            return FailureClass.MALFORMED
        if record_status == "failed" and not any(
            assertion["status"] == "failed" for assertion in assertions
        ) and not _vitest_file_error_classes(result):
            if assertions:
                classes.append(FailureClass.HOOK)

    expected_counts = {
        "numTotalTests": assertion_count,
        "numPassedTests": assertion_counts["passed"],
        "numFailedTests": assertion_counts["failed"],
        "numPendingTests": assertion_counts["skipped"] + assertion_counts["pending"],
        "numTodoTests": assertion_counts["todo"],
        "numTotalTestSuites": len(suite_states),
        "numPassedTestSuites": sum(1 for states in suite_states.values() if states == {"passed"}),
        "numFailedTestSuites": sum(1 for states in suite_states.values() if "failed" in states),
        "numPendingTestSuites": sum(1 for states in suite_states.values() if states == {"pending"}),
    }
    if any(document[key] != expected for key, expected in expected_counts.items()):
        return FailureClass.MALFORMED
    if document["success"] and (assertion_counts["failed"] or expected_counts["numFailedTestSuites"]):
        return FailureClass.MALFORMED
    if classes:
        return _combine_failure_classes(classes)
    if document["success"]:
        return FailureClass.PASS
    return FailureClass.COLLECTION if document["numTotalTests"] == 0 and document["numFailedTestSuites"] else FailureClass.UNKNOWN


def classify_structured_report(runner: object, report: Path, returncode: int) -> FailureClass:
    """Classify one known runner's report, failing closed on unusable evidence."""

    name = getattr(runner, "value", runner)
    if name == "pytest":
        result = parse_junit_report(report)
    elif name == "vitest":
        result = parse_vitest_report(report)
    else:
        return FailureClass.UNKNOWN_RUNNER
    if returncode != 0 and result is FailureClass.PASS:
        return FailureClass.UNKNOWN
    return result
