from pathlib import Path
import json
import subprocess
import sys

import pytest

from scripts import change_gates
from scripts.change_gate_reports import FailureClass, classify_structured_report
from scripts.change_gate_runner import CommandResult, Runner, run_command
from scripts.change_gates import (
    ChangeMode,
    DiffHunk,
    GateError,
    Mutant,
    added_module_names,
    gate_changed_line_mutation,
    gate_right_reason_red,
    gate_revert_probe,
    parse_diff,
    right_reason_red_evidence,
)


def _commit(path: Path) -> None:
    subprocess.run(["git", "-C", str(path), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(path), "add", "-A"], check=True)
    subprocess.run(
        [
            "git", "-C", str(path), "-c", "user.name=Gate Test",
            "-c", "user.email=gate-test@example.invalid", "commit", "--quiet", "-m", "base",
        ],
        check=True,
    )


def _hunk(path: str, before: str, after: str) -> DiffHunk:
    return DiffHunk(
        path,
        (),
        "@@ -1,1 +1,1 @@",
        (f"-{before}", f"+{after}"),
        1,
        1,
        1,
        1,
        (before,),
        (after,),
    )


def _main_result(arguments: list[str]) -> int:
    try:
        return change_gates.main(arguments)
    except GateError as error:
        raise AssertionError(f"gate raised unexpectedly: {error}") from error


def test_structured_reports_and_runner_preserve_exact_import_evidence(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="1" failures="0" errors="1">
        <testcase name="one"><error type="ModuleNotFoundError"
          message="No module named 'scripts.count_skill_words'" /></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )
    classification, module = classify_structured_report(
        "pytest", report, 1, include_evidence=True
    )
    assert (classification, module) == (FailureClass.IMPORT, "scripts.count_skill_words")
    assert classify_structured_report("pytest", report, 1) is FailureClass.IMPORT

    test_file = tmp_path / "test_import.py"
    test_file.write_text(
        "import module_that_does_not_exist_for_gate\n"
        "def test_never_runs():\n    assert True\n",
        encoding="utf-8",
    )
    result = run_command("pytest {test} -q", tmp_path, str(test_file), runner=Runner.PYTEST)
    assert (result.classification, result.unresolved_module) == (
        FailureClass.IMPORT,
        "module_that_does_not_exist_for_gate",
    )

    report.write_text(
        """<testsuites><testsuite tests="2" failures="0" errors="2">
        <testcase name="one"><error type="ModuleNotFoundError"
          message="No module named 'one'" /></testcase>
        <testcase name="two"><error type="ModuleNotFoundError"
          message="No module named 'two'" /></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )
    classification, module = classify_structured_report(
        "pytest", report, 1, include_evidence=True
    )
    assert classification is FailureClass.IMPORT
    assert module is None

    vitest_report = tmp_path / "vitest.json"
    vitest_report.write_text(
        json.dumps(
            {
                "success": False,
                "numTotalTests": 0,
                "numPassedTests": 0,
                "numFailedTests": 0,
                "numPendingTests": 0,
                "numTodoTests": 0,
                "numTotalTestSuites": 1,
                "numPassedTestSuites": 0,
                "numFailedTestSuites": 1,
                "numPendingTestSuites": 0,
                "testResults": [
                    {
                        "name": "tests/import.test.ts",
                        "status": "failed",
                        "assertionResults": [],
                        "message": "Cannot find module 'scripts/count_skill_words'",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    classification, module = classify_structured_report(
        "vitest", vitest_report, 1, include_evidence=True
    )
    assert (classification, module) == (FailureClass.IMPORT, "scripts/count_skill_words")


def test_added_module_names_are_exact_and_only_for_new_source_files():
    hunks = parse_diff(
        """diff --git a/scripts/count_skill_words.py b/scripts/count_skill_words.py
new file mode 100644
--- /dev/null
+++ b/scripts/count_skill_words.py
@@ -0,0 +1 @@
+def count_skill_words(text):
    return len(text.split())
diff --git a/scripts/old.py b/scripts/old.py
--- a/scripts/old.py
+++ b/scripts/old.py
@@ -1,1 +1,1 @@
-old
+new
"""
    )
    assert added_module_names(hunks) == frozenset(
        {
            "scripts/count_skill_words",
            "./scripts/count_skill_words",
            "scripts.count_skill_words",
        }
    )


def test_live_shared_helper_is_accepted_but_foreign_import_is_rejected(tmp_path):
    source = tmp_path / "source"
    (source / "scripts").mkdir(parents=True)
    (source / "tests").mkdir()
    (source / "scripts" / "__init__.py").write_text("", encoding="utf-8")
    (source / "tests" / "test_context_budgets.py").write_text(
        "def test_budget():\n    assert True\n", encoding="utf-8"
    )
    (source / "tests" / "test_context_budgets_extra.py").write_text(
        "def test_budget_extra():\n    assert True\n", encoding="utf-8"
    )
    _commit(source)
    (source / "scripts" / "count_skill_words.py").write_text(
        "def count_skill_words(text):\n    return len(text.split())\n", encoding="utf-8"
    )
    test = source / "tests" / "test_context_budgets.py"
    test.write_text(
        "from scripts.foreign_counter import count_skill_words\n\n"
        "def test_budget():\n    assert count_skill_words('x') == 1\n",
        encoding="utf-8",
    )
    extra_test = source / "tests" / "test_context_budgets_extra.py"
    extra_test.write_text(
        "from scripts.foreign_counter import count_skill_words\n\n"
        "def test_budget_extra():\n    assert count_skill_words('x') == 1\n",
        encoding="utf-8",
    )
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    arguments = [
        "right-reason-red", "--base", "HEAD", "--source-root", str(source),
        "--scratch-root", str(tmp_path / "foreign"),
        "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
        "--test-command-ts", "npm exec vitest run {test}",
    ]
    diff = change_gates.git_diff(source, "HEAD")
    assert "count_skill_words.py" in diff
    assert "scripts.count_skill_words" in change_gates.added_module_names(change_gates.parse_diff(diff))
    assert _main_result(arguments) == 1
    test.write_text(
        "from scripts.count_skill_words import count_skill_words\n\n"
        "def test_budget():\n    assert count_skill_words('x') == 1\n",
        encoding="utf-8",
    )
    extra_test.write_text(
        "from scripts.count_skill_words import count_skill_words\n\n"
        "def test_budget_extra():\n    assert count_skill_words('x') == 1\n",
        encoding="utf-8",
    )
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    arguments[6] = str(tmp_path / "shared-helper")
    assert _main_result(arguments) == 0


def test_default_and_refactor_reason_modes_are_closed():
    green = CommandResult("test", 0, "", FailureClass.PASS)
    assertion = CommandResult("test", 1, "", FailureClass.ASSERTION)
    import_result = CommandResult(
        "test", 1, "", FailureClass.IMPORT, unresolved_module="scripts.count_skill_words"
    )

    assert right_reason_red_evidence(green, True) is None
    assert right_reason_red_evidence(assertion, True) == "assertion"
    assert right_reason_red_evidence(green, False, mode=ChangeMode.REFACTOR) is None
    assert right_reason_red_evidence(green, True, mode=ChangeMode.REFACTOR) == "refactor"
    assert right_reason_red_evidence(assertion, True, mode=ChangeMode.REFACTOR) is None
    assert right_reason_red_evidence(green, True, mode=ChangeMode.TYPE_ONLY) == "type-only"
    assert right_reason_red_evidence(
        import_result,
        True,
        added_modules={"scripts.count_skill_words"},
    ) == "added-module"
    assert right_reason_red_evidence(
        import_result,
        True,
        added_modules={"scripts.foreign_counter"},
    ) is None
    assert {mode.value for mode in ChangeMode} == {"behaviour", "refactor", "type-only"}


def test_refactor_mode_does_not_bypass_revert_or_mutation(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("value = 'old'\n", encoding="utf-8")
    (source / "tests").mkdir()
    (source / "tests" / "test_refactor.py").write_text("def test_refactor():\n    assert True\n", encoding="utf-8")
    _commit(source)
    (source / "production.py").write_text("value = 'new'\n", encoding="utf-8")
    hunk = _hunk("production.py", "value = 'old'", "value = 'new'")

    assert gate_right_reason_red(
        source,
        "HEAD",
        {"py": "python3 -c 'pass' {test}", "ts": "npm exec vitest run {test}"},
        ["tests/test_refactor.py"],
        tmp_path / "right-reason",
        ChangeMode.REFACTOR,
    ) == 0
    assert gate_revert_probe(
        source, [hunk],
        ["python3 -c 'from pathlib import Path; assert Path(\"production.py\").read_text().startswith(\"value = \")'"],
        [], tmp_path / "revert",
    ) == 1
    assert gate_changed_line_mutation(
        source,
        [Mutant("production.py", 1, "value = 'new'", "value = 'old'", "survivor")],
        ["python3 -c 'pass'"], [], tmp_path / "mutation", "crucial",
    ) == 1


def test_type_only_is_owned_by_type_gate_and_mixed_fails_closed():
    type_hunk = _hunk(
        "production.ts", "export type Value = Old", "export type Value = New"
    )
    mixed_hunk = DiffHunk(
        "production.ts", (), "@@ -1,1 +1,2 @@",
        ("-export type Value = Old", "+export type Value = New", "+export const value = true;"),
        1, 1, 1, 2, ("export type Value = Old",),
        ("export type Value = New", "export const value = true;"),
    )
    same_line_mixed_hunk = _hunk(
        "production.ts",
        "import { type Old } from './types'; sideEffect()",
        "import { type New } from './types'; sideEffect()",
    )
    assert change_gates.hunk_mode(type_hunk) is ChangeMode.TYPE_ONLY
    assert change_gates.hunk_mode(
        _hunk(
            "production.ts",
            "import { type Old } from './types'",
            "import { type New } from './types'",
        )
    ) is ChangeMode.TYPE_ONLY
    assert change_gates.hunk_mode(
        _hunk("production.ts", "declare const old: string", "declare const newValue: string")
    ) is ChangeMode.TYPE_ONLY
    assert change_gates.hunk_mode(mixed_hunk) is None
    assert change_gates.hunk_mode(same_line_mixed_hunk) is None
    try:
        change_gates.validate_change_mode([type_hunk], ChangeMode.BEHAVIOUR)
    except GateError as error:
        assert False, f"pure type-only hunk was rejected: {error}"
    with pytest.raises(GateError, match="mixed type/runtime"):
        change_gates.validate_change_mode([mixed_hunk], ChangeMode.BEHAVIOUR)


def test_type_only_mode_routes_all_three_gates_to_the_type_gate(tmp_path, capsys):
    source = tmp_path / "source"
    source.mkdir()
    (source / "tests").mkdir()
    (source / "tests" / "test_type_only.py").write_text(
        "def test_type_only():\n    assert True\n", encoding="utf-8"
    )
    (source / "production.ts").write_text("export type Value = Old\n", encoding="utf-8")
    _commit(source)
    (source / "production.ts").write_text("export type Value = New\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    default_common = [
        "--base", "HEAD", "--source-root", str(source),
        "--scratch-root", str(tmp_path / "default-scratch"),
        "--test-command-py", "python3 -m pytest {test} -q",
        "--test-command-ts", "npm exec vitest run {test}",
        "--test", "tests/test_type_only.py",
    ]
    try:
        default_result = change_gates.main(["right-reason-red", *default_common])
    except GateError as error:
        assert False, f"default type-only routing failed: {error}"
    assert default_result == 0
    assert "RIGHT_REASON_RED: TYPE_ONLY owner=type-gate" in capsys.readouterr().out
    common = [
        "--base", "HEAD", "--source-root", str(source),
        "--scratch-root", str(tmp_path / "scratch"),
        "--test-command-py", "python3 -m pytest {test} -q",
        "--test-command-ts", "npm exec vitest run {test}",
        "--mode", "type-only",
    ]
    for gate in ("right-reason-red", "revert-probe", "changed-lines-only"):
        assert change_gates.main([gate, *common]) == 0
        output = capsys.readouterr().out
        assert "owner=type-gate" in output
        assert f"{gate.upper().replace('-', '_')}:" in output or (
            gate == "changed-lines-only" and "CHANGED_LINES_MUTATION:" in output
        )
        if gate == "right-reason-red":
            assert "tests=0" in output
        elif gate == "revert-probe":
            assert "PASS owner=type-gate" in output and "killed=0" in output
        else:
            assert "TYPE_ONLY owner=type-gate" in output
            assert "survivors=0" in output and "inconclusive=0" in output
