import pytest
import json
from pathlib import Path
import subprocess
import sys

from _change_gate_helpers import PYTEST_COMMAND
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
    hunk_mode,
    module_is_added,
    parse_diff,
    right_reason_red_evidence,
    validate_change_mode,
)


def test_default_mode_keeps_assertion_and_new_target_rules_and_import_helper_is_explicit():
    assertion = CommandResult("test", 1, "", FailureClass.ASSERTION)
    new_target = CommandResult("test", 1, "", FailureClass.COLLECTION)
    helper_import = CommandResult(
        "test",
        1,
        "",
        FailureClass.IMPORT,
        unresolved_module="scripts.new_helper",
        structured_import_evidence=True,
    )

    assert right_reason_red_evidence(assertion, True) == "assertion"
    assert right_reason_red_evidence(new_target, False) == "new-target"
    assert right_reason_red_evidence(helper_import, True) is None
    assert right_reason_red_evidence(
        helper_import,
        True,
        mode=ChangeMode.IMPORT_HELPER,
        added_modules={"scripts.new_helper"},
    ) == "added-module"
    assert right_reason_red_evidence(
        CommandResult("test", 0, "", FailureClass.PASS),
        True,
        mode=ChangeMode.TYPE_ONLY,
    ) == "type-only"
    assert {mode.value for mode in ChangeMode} == {
        "behaviour", "import-helper", "refactor", "type-only"
    }


def test_junit_test_body_module_error_has_no_import_evidence(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="1" failures="1" errors="0">
          <testcase name="body"><failure type="ModuleNotFoundError"
            message="No module named 'new_helper'">forged</failure></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    classification, module = classify_structured_report(
        "pytest", report, 1, include_evidence=True
    )

    assert classification is FailureClass.IMPORT
    assert module is None


def test_legacy_structured_classifier_still_returns_only_its_failure_class(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        '<testsuites><testsuite tests="1" failures="0" errors="0">'
        '<testcase name="pass" /></testsuite></testsuites>',
        encoding="utf-8",
    )

    assert classify_structured_report("pytest", report, 0) is FailureClass.PASS


def test_junit_collection_error_has_one_import_identity(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="0" failures="0" errors="1">
          <error type="ModuleNotFoundError" message="No module named 'new_helper'" />
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    assert classify_structured_report("pytest", report, 1, include_evidence=True) == (
        FailureClass.COLLECTION,
        "new_helper",
    )


def test_junit_collection_testcase_uses_its_parent_suite_for_import_identity(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="0" failures="0" errors="0">
          <testcase name="collected"><error type="ModuleNotFoundError"
            message="No module named 'new_helper'" /></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    from scripts.change_gate_reports import _junit_import_values

    assert len(_junit_import_values(report)) == 2


def test_junit_testcase_error_without_native_collection_traceback_has_no_identity(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites><testsuite tests="1" failures="0" errors="1">
          <testcase name="body"><error type="ModuleNotFoundError"
            message="collection failure">No module named 'new_helper'</error></testcase>
        </testsuite></testsuites>""",
        encoding="utf-8",
    )

    classification, module = classify_structured_report(
        "pytest", report, 1, include_evidence=True
    )

    assert classification is FailureClass.IMPORT
    assert module is None


def test_multiple_collection_import_identities_are_not_accepted(tmp_path):
    report = tmp_path / "pytest.xml"
    report.write_text(
        """<testsuites>
          <testsuite tests="0" failures="0" errors="1">
            <error type="ModuleNotFoundError" message="No module named 'one'" />
          </testsuite>
          <testsuite tests="0" failures="0" errors="1">
            <error type="ModuleNotFoundError" message="No module named 'two'" />
          </testsuite>
        </testsuites>""",
        encoding="utf-8",
    )

    classification, module = classify_structured_report(
        "pytest", report, 1, include_evidence=True
    )

    assert classification is FailureClass.COLLECTION
    assert module is None


def test_vitest_body_error_is_not_file_import_evidence(tmp_path):
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
                "testResults": [{
                    "name": "tests/body.test.ts",
                    "status": "failed",
                    "assertionResults": [{
                        "status": "failed",
                        "ancestorTitles": [],
                        "title": "forged",
                        "failureMessages": ["ModuleNotFoundError: No module named 'new_helper'"],
                    }],
                }],
            }
        ),
        encoding="utf-8",
    )

    assert classify_structured_report("vitest", report, 1, include_evidence=True) == (
        FailureClass.UNKNOWN,
        None,
    )


def test_vitest_file_error_has_one_import_identity(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps({
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
            "testResults": [{
                "name": "runtime/tests/import.test.ts",
                "status": "failed",
                "error": {
                    "name": "ModuleNotFoundError",
                    "message": "Cannot find module '../new_helper.js'",
                },
                "assertionResults": [],
            }],
        }),
        encoding="utf-8",
    )

    assert classify_structured_report("vitest", report, 1, include_evidence=True) == (
        FailureClass.IMPORT,
        "../new_helper.js",
    )


def test_vitest_error_field_with_assertions_is_not_file_import_evidence(tmp_path):
    report = tmp_path / "vitest.json"
    report.write_text(
        json.dumps({
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
            "testResults": [{
                "name": "tests/body.test.ts",
                "status": "passed",
                "error": {
                    "name": "ModuleNotFoundError",
                    "message": "Cannot find module '../new_helper.js'",
                },
                "assertionResults": [{
                    "status": "passed",
                    "ancestorTitles": [],
                    "title": "body",
                    "failureMessages": [],
                }],
            }],
        }),
        encoding="utf-8",
    )

    assert classify_structured_report("vitest", report, 0, include_evidence=True) == (
        FailureClass.PASS,
        None,
    )


def test_structured_runner_does_not_promote_test_body_module_error(tmp_path):
    test_file = tmp_path / "test_body_module_error.py"
    test_file.write_text(
        "def test_body_module_error():\n"
        "    raise ModuleNotFoundError(\"No module named 'new_helper'\")\n",
        encoding="utf-8",
    )

    result = run_command(
        PYTEST_COMMAND,
        tmp_path,
        str(test_file),
        runner=Runner.PYTEST,
        timeout_seconds=10.0,
    )

    assert result.classification is FailureClass.IMPORT
    assert result.unresolved_module is None


def test_structured_runner_preserves_one_real_collection_module(tmp_path):
    test_file = tmp_path / "test_import.py"
    test_file.write_text(
        "from new_helper import value\n\n"
        "def test_import():\n    assert value == 1\n",
        encoding="utf-8",
    )

    result = run_command(
        PYTEST_COMMAND,
        tmp_path,
        str(test_file),
        runner=Runner.PYTEST,
        timeout_seconds=10.0,
    )

    assert result.classification is FailureClass.IMPORT
    assert result.unresolved_module == "new_helper"


def test_added_module_names_canonicalise_python_packages_and_typescript_files():
    hunks = parse_diff(
        """diff --git a/scripts/new_helper/__init__.py b/scripts/new_helper/__init__.py
new file mode 100644
--- /dev/null
+++ b/scripts/new_helper/__init__.py
@@ -0,0 +1 @@
+VALUE = 1
diff --git a/runtime/new_helper.ts b/runtime/new_helper.ts
new file mode 100644
--- /dev/null
+++ b/runtime/new_helper.ts
@@ -0,0 +1 @@
+export const value = 1
"""
    )

    names = added_module_names(hunks)

    assert "scripts/new_helper" in names
    assert "scripts.new_helper" in names
    assert "scripts/new_helper/__init__" not in names
    assert "runtime/new_helper" in names


def test_import_helper_resolves_relative_typescript_import_from_importer_path():
    result = CommandResult(
        "test",
        1,
        "",
        FailureClass.IMPORT,
        unresolved_module="../new_helper.js",
        structured_import_evidence=True,
    )

    assert right_reason_red_evidence(
        result,
        True,
        mode=ChangeMode.IMPORT_HELPER,
        added_modules={"runtime/new_helper"},
        importer="runtime/tests/import.test.ts",
    ) == "added-module"
    assert module_is_added("scripts.new_helper", {"scripts.new_helper"})
    assert not module_is_added("", {"scripts.new_helper"})


def test_type_only_requires_every_changed_typescript_line_to_be_type_only():
    pure = DiffHunk(
        "production.ts", (), "@@ -1,1 +1,1 @@",
        ("-export type Value = Old;", "+export type Value = New;"),
        1, 1, 1, 1,
        ("export type Value = Old;",), ("export type Value = New;",),
    )
    runtime_suffix = DiffHunk(
        "production.ts", (), "@@ -1,1 +1,1 @@",
        ("-export type Value = Old;", "+export type Value = New; console.log(Value);"),
        1, 1, 1, 1,
        ("export type Value = Old;",), ("export type Value = New; console.log(Value);",),
    )
    mixed_named_import = DiffHunk(
        "production.ts", (), "@@ -1,1 +1,1 @@",
        ("-import { type Foo } from './m';", "+import { type Foo, runtime } from './m';"),
        1, 1, 1, 1,
        ("import { type Foo } from './m';",),
        ("import { type Foo, runtime } from './m';",),
    )

    assert hunk_mode(pure) is ChangeMode.TYPE_ONLY
    assert hunk_mode(runtime_suffix) is not ChangeMode.TYPE_ONLY
    assert hunk_mode(mixed_named_import) is not ChangeMode.TYPE_ONLY
    with pytest.raises(GateError, match="mixed type/runtime"):
        validate_change_mode([runtime_suffix], ChangeMode.TYPE_ONLY)
    with pytest.raises(GateError, match="every production hunk"):
        validate_change_mode([], ChangeMode.TYPE_ONLY)

    for mode in (ChangeMode.TYPE_ONLY, ChangeMode.BEHAVIOUR):
        try:
            validate_change_mode([pure], mode)
        except GateError as error:
            raise AssertionError(f"pure type-only hunk was rejected in {mode}") from error


def test_change_gate_help_exposes_string_mode_values():
    completed = subprocess.run(
        [sys.executable, "scripts/change_gates.py", "--help"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )

    assert completed.returncode == 0
    assert "behaviour" in completed.stdout
    assert "import-helper" in completed.stdout
    assert "refactor" in completed.stdout
    assert "type-only" in completed.stdout
    assert "ChangeMode." not in completed.stdout


def test_refactor_mode_accepts_base_green_only_at_the_right_reason_gate(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("value = 'old'\n", encoding="utf-8")
    (source / "tests").mkdir()
    (source / "tests" / "test_refactor.py").write_text(
        "def test_refactor():\n    assert True\n", encoding="utf-8"
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
    (source / "production.py").write_text("value = 'new'\n", encoding="utf-8")

    assert gate_right_reason_red(
        source,
        "HEAD",
        {"py": "python3 -c 'pass' {test}", "ts": "npm exec vitest run {test}"},
        ["tests/test_refactor.py"],
        tmp_path / "scratch",
        mode=ChangeMode.REFACTOR,
    ) == 0


def test_import_helper_mode_accepts_the_live_shared_helper_collection_case(tmp_path):
    source = tmp_path / "source"
    (source / "scripts").mkdir(parents=True)
    (source / "tests").mkdir()
    (source / "scripts" / "__init__.py").write_text("", encoding="utf-8")
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
        "# shared-helper extraction\n"
        "from scripts.new_helper import value\n\n"
        "def test_budget():\n    assert value == 1\n",
        encoding="utf-8",
    )
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)

    common = [
        "right-reason-red",
        "--base", "HEAD",
        "--source-root", str(source),
        "--scratch-root", str(tmp_path / "scratch"),
        "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
        "--test-command-ts", "npm exec vitest run {test}",
        "--mode", "import-helper",
    ]

    assert __import__("scripts.change_gates", fromlist=["main"]).main(common) == 0


def test_import_helper_mode_rejects_foreign_and_forged_module_names():
    trusted = CommandResult(
        "test",
        1,
        "",
        FailureClass.IMPORT,
        unresolved_module="scripts.new_helper",
        structured_import_evidence=True,
    )
    forged = CommandResult(
        "test", 1, "", FailureClass.IMPORT, unresolved_module="scripts.new_helper"
    )
    foreign = CommandResult(
        "test", 1, "", FailureClass.IMPORT, unresolved_module="scripts.foreign"
    )

    assert right_reason_red_evidence(
        trusted,
        True,
        mode=ChangeMode.IMPORT_HELPER,
        added_modules={"scripts.new_helper"},
    ) == "added-module"
    assert right_reason_red_evidence(
        forged,
        True,
        mode=ChangeMode.IMPORT_HELPER,
        added_modules={"scripts.new_helper"},
    ) is None
    assert right_reason_red_evidence(
        foreign,
        True,
        mode=ChangeMode.IMPORT_HELPER,
        added_modules={"scripts.new_helper"},
    ) is None


def test_pure_type_only_change_is_owned_by_type_gate(tmp_path, capsys):
    source = tmp_path / "source"
    (source / "tests").mkdir(parents=True)
    (source / "production.ts").write_text("export type Value = Old;\n", encoding="utf-8")
    (source / "tests" / "test_type_only.py").write_text(
        "def test_type_only():\n    assert True\n", encoding="utf-8"
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
    (source / "production.ts").write_text("export type Value = New;\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)

    from scripts import change_gates

    result = change_gates.main(
        [
            "right-reason-red", "--base", "HEAD", "--source-root", str(source),
            "--scratch-root", str(tmp_path / "scratch"),
            "--test-command-py", PYTEST_COMMAND,
            "--test-command-ts", "npm exec vitest run {test}",
        ]
    )

    assert result == 0
    output = capsys.readouterr().out
    assert "RIGHT_REASON_RED: TYPE_ONLY" in output
    assert "owner=type-gate" in output

    revert_result = change_gates.main(
        [
            "revert-probe", "--base", "HEAD", "--source-root", str(source),
            "--scratch-root", str(tmp_path / "revert-scratch"),
            "--test-command-py", PYTEST_COMMAND,
            "--test-command-ts", "npm exec vitest run {test}",
        ]
    )
    assert revert_result == 0
    output = capsys.readouterr().out
    assert "REVERT_PROBE: PASS owner=type-gate" in output
    assert "hunks=1 killed=0 survivors=0 inconclusive=0" in output

    mutation_result = change_gates.main(
        [
            "changed-lines-only", "--base", "HEAD", "--source-root", str(source),
            "--scratch-root", str(tmp_path / "mutation-scratch"),
            "--test-command-py", PYTEST_COMMAND,
            "--test-command-ts", "npm exec vitest run {test}",
        ]
    )
    assert mutation_result == 0
    assert "CHANGED_LINES_MUTATION: PASS owner=type-gate" in capsys.readouterr().out


def test_live_ci_consumes_the_tracked_change_gate_mode_declaration():
    workflow = Path(".github/workflows/ci.yml").read_text(encoding="utf-8")
    declaration = Path(".github/change-gate-mode").read_text(encoding="utf-8").strip()

    assert declaration in {mode.value for mode in ChangeMode}
    assert workflow.count('--mode "$change_gate_mode"') == 2
    assert "change_gate_mode=$(tr -d" in workflow


def test_mutation_verdict_reports_zero_inconclusive_explicitly():
    source = Path("scripts/change_gates.py").read_text(encoding="utf-8")

    assert 'f"inconclusive={len(inconclusive)} wall_seconds={elapsed:.3f}"' in source


def test_mutation_verdict_rejects_inconclusive_evidence_without_calling_it_invalid(
    tmp_path, monkeypatch, capsys
):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("enabled = True\n", encoding="utf-8")
    mutant = Mutant(
        path="production.py",
        line=1,
        before="enabled = True",
        after="enabled = False",
        description="inconclusive evidence",
    )

    def run_suite(commands, cwd, tests, *, fail_fast=False, budget=None):
        del commands, cwd, tests, budget
        if fail_fast:
            return [CommandResult("test", 1, "", FailureClass.PASS)]
        return [CommandResult("test", 0, "", FailureClass.PASS)]

    monkeypatch.setattr(change_gates, "_run_suite", run_suite)

    assert gate_changed_line_mutation(
        source,
        [mutant],
        ["python3 -c 'pass'"],
        [],
        tmp_path / "scratch",
        "crucial",
    ) == 1
    output = capsys.readouterr().out
    assert "status=INCONCLUSIVE" in output
    assert "status=INVALID" not in output
    assert "survivors=0 inconclusive=1" in output


def test_mutation_verdict_keeps_green_mutants_as_survivors(tmp_path, monkeypatch, capsys):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("enabled = True\n", encoding="utf-8")
    mutant = Mutant(
        path="production.py",
        line=1,
        before="enabled = True",
        after="enabled = False",
        description="surviving evidence",
    )

    def run_suite(commands, cwd, tests, *, fail_fast=False, budget=None):
        del commands, cwd, tests, fail_fast, budget
        return [CommandResult("test", 0, "", FailureClass.PASS)]

    monkeypatch.setattr(change_gates, "_run_suite", run_suite)

    assert gate_changed_line_mutation(
        source,
        [mutant],
        ["python3 -c 'pass'"],
        [],
        tmp_path / "scratch",
        "crucial",
    ) == 1
    output = capsys.readouterr().out
    assert "status=SURVIVED" in output
    assert "survivors=1 inconclusive=0" in output


def test_refactor_mode_is_reachable_for_a_changed_test_without_production_hunks(tmp_path, capsys):
    source = tmp_path / "source"
    (source / "tests").mkdir(parents=True)
    test_file = source / "tests" / "test_only.py"
    test_file.write_text("def test_only():\n    assert True\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git", "-C", str(source), "-c", "user.name=Gate Test",
            "-c", "user.email=gate-test@example.invalid", "commit", "--quiet", "-m", "base",
        ],
        check=True,
    )
    test_file.write_text(
        "# refactor-only test surface\ndef test_only():\n    assert True\n",
        encoding="utf-8",
    )

    from scripts import change_gates

    result = change_gates.main(
        [
            "right-reason-red", "--base", "HEAD", "--source-root", str(source),
            "--scratch-root", str(tmp_path / "scratch"),
            "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
            "--test-command-ts", "npm exec vitest run {test}",
            "--mode", "refactor",
        ]
    )

    assert result == 0
    assert "RIGHT_REASON_RED: PASS" in capsys.readouterr().out

    revert_result = change_gates.main(
        [
            "revert-probe", "--base", "HEAD", "--source-root", str(source),
            "--scratch-root", str(tmp_path / "revert-scratch"),
            "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
            "--test-command-ts", "npm exec vitest run {test}",
            "--mode", "refactor",
        ]
    )
    assert revert_result == 0
    assert "REVERT_PROBE: PASS mode=refactor" in capsys.readouterr().out

    mutation_result = change_gates.main(
        [
            "changed-lines-only", "--base", "HEAD", "--source-root", str(source),
            "--scratch-root", str(tmp_path / "mutation-scratch"),
            "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
            "--test-command-ts", "npm exec vitest run {test}",
            "--mode", "refactor",
        ]
    )
    assert mutation_result == 0
    assert "CHANGED_LINES_MUTATION: PASS mode=refactor" in capsys.readouterr().out

    try:
        default_result = change_gates.main(
            [
                "changed-lines-only", "--base", "HEAD", "--source-root", str(source),
                "--scratch-root", str(tmp_path / "default-scratch"),
                "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
                "--test-command-ts", "npm exec vitest run {test}",
            ]
        )
    except GateError as error:
        raise AssertionError("default test-only mutation gate must retain its skip rule") from error
    assert default_result == 0
    assert "SKIP production_hunks=0" in capsys.readouterr().out


def test_behaviour_mode_skips_revert_for_a_test_only_change(tmp_path, capsys):
    source = tmp_path / "source"
    (source / "tests").mkdir(parents=True)
    test_file = source / "tests" / "test_only.py"
    test_file.write_text("def test_only():\n    assert True\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git", "-C", str(source), "-c", "user.name=Gate Test",
            "-c", "user.email=gate-test@example.invalid", "commit", "--quiet", "-m", "base",
        ],
        check=True,
    )
    test_file.write_text("# test-only change\ndef test_only():\n    assert True\n", encoding="utf-8")

    try:
        result = change_gates.main(
            [
                "revert-probe", "--base", "HEAD", "--source-root", str(source),
                "--scratch-root", str(tmp_path / "scratch"),
                "--test-command-py", f"{sys.executable} -m pytest {{test}} -q",
                "--test-command-ts", "npm exec vitest run {test}",
            ]
        )
    except GateError as error:
        pytest.fail("behaviour-mode test-only revert must retain its skip rule", pytrace=False)
    assert result == 0
    assert "REVERT_PROBE: SKIP production_hunks=0" in capsys.readouterr().out
