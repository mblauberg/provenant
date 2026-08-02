import ast
from contextlib import redirect_stdout
import os
from pathlib import Path
import io
import subprocess
import sys

import pytest

from _change_gate_helpers import PYTEST_COMMAND
from scripts import change_gate_runner, change_gates
from scripts.change_gates import (
    DiffHunk,
    GateError,
    Mutant,
    CommandResult,
    FailureClass,
    _apply_reverse_hunk,
    _materialise_base,
    _targets,
    gate_changed_line_mutation,
    gate_right_reason_red,
    gate_revert_probe,
    mutations_for_lines,
    parse_diff,
    right_reason_red_evidence,
    target_existed_at_base,
    _temporary_tree,
)


ROOT = Path(__file__).resolve().parents[1]
GATE = ROOT / "scripts" / "check-test-gates"


def classify_failure(returncode, output):
    implementation = getattr(change_gates, "classify_failure", None)
    assert implementation is not None, "structured classifier is absent at this revert point"
    return implementation(returncode, output)


def _implementation():
    assert GATE.is_file(), "change-gate implementation is absent at the merge base"
    assert all(
        value is not None
        for value in (
            classify_failure,
            DiffHunk,
            _apply_reverse_hunk,
            mutations_for_lines,
            parse_diff,
            _temporary_tree,
        )
    )
    return classify_failure, FailureClass


def _missing_failure_class():
    value = getattr(FailureClass, "MISSING", None)
    assert value is not None, "structured failure classes are absent at the merge base"
    return value


def test_change_gates_uses_the_single_change_gate_runner():
    assert change_gates.run_command is change_gate_runner.run_command


def test_local_subprocess_run_calls_all_state_finite_timeouts():
    tree = ast.parse(Path(change_gates.__file__).read_text(encoding="utf-8"))
    calls = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "subprocess"
        and node.func.attr == "run"
    ]

    assert calls
    assert all(any(keyword.arg == "timeout" for keyword in call.keywords) for call in calls)


def test_right_reason_red_rejects_collection_errors_as_assertion_reds():
    assert classify_failure(1, "ERROR during collection") is not FailureClass.ASSERTION


def test_text_marker_tables_are_not_retained_as_a_classification_fallback():
    assert all(
        not hasattr(change_gates, name)
        for name in (
            "_IMPORT_MARKERS",
            "_COLLECTION_MARKERS",
            "_SETUP_MARKERS",
            "_RUNTIME_ERROR_MARKERS",
            "_ASSERTION_RE",
            "_PYTEST_ASSERT_RE",
        )
    )


def test_existing_target_collection_error_is_rejected():
    output = "ERROR during collection"
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=True) is None


def test_new_target_collection_error_is_accepted():
    output = "ERROR during collection"
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=False) == "new-target"


def test_existing_target_assertion_failure_is_accepted():
    output = "AssertionError: expected value"
    result = CommandResult("test", 1, output, FailureClass.ASSERTION)

    assert right_reason_red_evidence(result, target_existed=True) == "assertion"


def test_existing_target_import_typo_is_rejected():
    # Written plainly. This text is exactly what a NEW target also produces, so
    # the whole point of the criterion is that it never reads this string: a
    # target that existed at the base and cannot be imported is a typo, and a
    # target that did not exist is ordinary new-file creation.
    output = "ModuleNotFoundError: No module named typo"
    result = CommandResult("test", 1, output, FailureClass.IMPORT)

    assert right_reason_red_evidence(result, target_existed=True) is None


def test_right_reason_red_accepts_collection_error_for_target_new_at_base(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "tracked.py").write_text("base\n", encoding="utf-8")
    (source / "tests").mkdir()
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(source),
            "-c",
            "user.name=Gate Test",
            "-c",
            "user.email=gate-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
        check=True,
    )
    (source / "tests" / "new_test.py").write_text("new current test\n", encoding="utf-8")

    result = gate_right_reason_red(
        source,
        "HEAD",
        [f'{sys.executable} -c "print(\\"ERROR during collection\\"); raise SystemExit(1)" {{test}}'],
        ["tests/new_test.py"],
        tmp_path / "scratch",
    )

    assert result == 0


def test_revert_probe_fails_when_a_reverted_hunk_survives(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("new\n", encoding="utf-8")
    hunk = DiffHunk(
        path="production.py",
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-old", "+new"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("old",),
        new_lines=("new",),
    )
    command = "python3 -c 'from pathlib import Path; assert Path(\"production.py\").read_text() == \"old\\n\"'"

    assert gate_revert_probe(source, [hunk], [command], [], tmp_path / "scratch") == 1


def _hunk(path="production.py"):
    return DiffHunk(
        path=path,
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-old", "+new"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("old",),
        new_lines=("new",),
    )


def test_revert_probe_does_not_count_a_non_assertion_red_as_a_survivor(tmp_path):
    """A suite that cannot run is not evidence that nothing constrains the hunk.

    Reverting a hunk can break the very import the tests need, which reds the
    suite for a mechanical reason. That is an inability to measure, and counting
    it as a survivor reports "this change is unconstrained" on evidence that says
    only "I could not tell". A survivor is a suite that stayed GREEN.
    """
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("new\n", encoding="utf-8")
    # Exits non-zero with a collection error, never an assertion failure.
    command = "python3 -c 'import no_such_module_at_all'"

    assert gate_revert_probe(source, [_hunk()], [command], [], tmp_path / "scratch") == 1


def test_revert_probe_does_not_count_missing_report_as_a_survivor(tmp_path, monkeypatch, capsys):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("new\n", encoding="utf-8")

    monkeypatch.setattr(
        change_gates,
        "_run_suite",
        lambda commands, cwd, tests: [CommandResult("test", 0, "", _missing_failure_class())],
    )

    assert gate_revert_probe(source, [_hunk()], ["unused"], [], tmp_path / "scratch") == 1
    assert "INCONCLUSIVE unusable evidence" in capsys.readouterr().out


def test_revert_probe_uses_the_cohesive_test_module_for_each_source_hunk(tmp_path, monkeypatch):
    source = tmp_path / "source"
    (source / "scripts").mkdir(parents=True)
    (source / "scripts" / "change_gates.py").write_text("new\n", encoding="utf-8")
    hunk = DiffHunk(
        path="scripts/change_gates.py",
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-old", "+new"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("old",),
        new_lines=("new",),
    )
    selected = []

    def run_selected(commands, cwd, tests):
        del commands, cwd
        selected.append(tests)
        return [CommandResult("test", 1, "", FailureClass.ASSERTION)]

    monkeypatch.setattr(change_gates, "_run_suite", run_selected)

    assert (
        gate_revert_probe(
            source,
            [hunk],
            {"py": "pytest {test}"},
            ["tests/test_change_gate_reports.py", "tests/test_change_gates.py"],
            tmp_path / "scratch",
        )
        == 0
    )
    assert selected == [["tests/test_change_gates.py"]]


def test_changed_lines_mutation_fails_for_a_surviving_crucial_mutant(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("enabled = True\n", encoding="utf-8")
    mutant = Mutant(
        path="production.py",
        line=1,
        before="enabled = True",
        after="enabled = 1 == 1",
        description="test survivor",
    )

    assert gate_changed_line_mutation(
        source,
        [mutant],
        ["python3 -c 'pass'"],
        [],
        tmp_path / "scratch",
        "crucial",
    ) == 1


def test_custom_commands_use_shared_process_semantics_for_known_file_suffixes(tmp_path):
    commands = {
        "py": f'{sys.executable} -c "import sys; sys.exit(0)" {{test}}',
        "ts": f'{sys.executable} -c "import sys; sys.exit(0)" {{test}}',
    }

    results = change_gates._run_suite(
        commands,
        tmp_path,
        ["tests/custom.py", "tests/custom.ts"],
    )

    assert [result.returncode for result in results] == [0, 0]
    assert [result.classification for result in results] == [FailureClass.PASS, FailureClass.PASS]


def test_run_suite_does_not_enable_fail_fast_by_default(tmp_path, monkeypatch):
    commands_seen = []

    def observe(command, cwd, target, *, runner=None):
        del cwd, target, runner
        commands_seen.append(command)
        return CommandResult(command, 0, "", FailureClass.PASS)

    monkeypatch.setattr(change_gates, "run_command", observe)
    change_gates._run_suite(
        {"py": "pytest {test}", "ts": "npm exec vitest run {test}"},
        tmp_path,
        ["tests/custom.py", "tests/custom.ts"],
    )

    assert all("--maxfail" not in command and "--bail" not in command for command in commands_seen)


def test_structured_runner_dispatch_is_explicit_for_known_file_suffixes(tmp_path, monkeypatch):
    runners = []

    def observe(command, cwd, target, *, runner=None):
        del command, cwd, target
        runners.append(runner)
        return CommandResult("test", 0, "", FailureClass.PASS)

    monkeypatch.setattr(change_gates, "run_command", observe)

    change_gates._run_suite(
        {"py": "pytest {test}", "ts": "npm exec vitest run {test}"},
        tmp_path,
        ["tests/custom.py", "tests/custom.ts"],
    )

    assert [getattr(runner, "value", None) for runner in runners] == ["pytest", "vitest"]


def test_changed_behaviour_doctrine_invokes_tdd_and_has_no_retrospective_red_clause():
    corpus = "\n".join(
        path.read_text(encoding="utf-8")
        for path in (
            ROOT / "HARNESS.md",
            ROOT / "skills" / "implement" / "SKILL.md",
            ROOT / "skills" / "orchestrate" / "SKILL.md",
            ROOT / "skills" / "orchestrate" / "references" / "orchestration-contract.md",
        )
    ).casefold()
    assert "invoke `tdd`" in corpus
    assert "would fail " + "against the " + "pre-" + "change code" not in corpus
    assert "would fail " + "pre-" + "change" not in corpus


def test_right_reason_red_accepts_only_a_real_assertion_failure():
    classify, failure_class = _implementation()
    assert classify(1, "E AssertionError: expected 1 == 2") is failure_class.ASSERTION
    assert classify(1, "E       assert 1 == 2\n1 failed") is failure_class.ASSERTION
    import_error = "E ModuleNotFound" + "Error: No module named 'new_code'"
    assert classify(1, import_error) is failure_class.IMPORT
    assert classify(1, "Error during collection") is failure_class.COLLECTION
    assert classify(1, "TypeError: missing API\nAssertionError: unrelated") is failure_class.UNKNOWN


def test_materialised_base_is_a_git_repository(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "tracked.txt").write_text("base\n", encoding="utf-8")
    (source / "tests").mkdir()
    (source / "tests" / "changed.py").write_text("base test\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(source),
            "-c",
            "user.name=Gate Test",
            "-c",
            "user.email=gate-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
        check=True,
    )
    (source / "tests" / "changed.py").write_text("current test\n", encoding="utf-8")
    (source / "current_only.py").write_text("must not enter base\n", encoding="utf-8")
    destination = tmp_path / "destination"
    destination.mkdir()

    _materialise_base(source, "HEAD", destination, ["tests/changed.py"])

    listed = subprocess.run(
        ["git", "-C", str(destination), "ls-files"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    origin = subprocess.run(
        ["git", "-C", str(destination), "rev-parse", "origin/main"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    assert listed.returncode == 0, listed.stdout
    assert listed.stdout.splitlines() == ["tests/changed.py", "tracked.txt"]
    assert origin.returncode == 0, origin.stdout
    assert (destination / "tests" / "changed.py").read_text(encoding="utf-8") == "current test\n"
    assert (destination / "current_only.py").exists() is False
    base_test = subprocess.run(
        ["git", "-C", str(destination), "show", "origin/main:tests/changed.py"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    assert base_test.returncode == 0, base_test.stdout
    assert base_test.stdout == "base test\n"


def test_test_targets_require_a_test_placeholder():
    with pytest.raises(GateError, match=r"--test targets require.*\{test\}"):
        _targets(["python3 -m pytest tests/ -q"], ["tests/a.py", "tests/b.py"])


def test_diff_parser_retains_individual_hunks_for_single_hunk_probes():
    _implementation()
    hunks = parse_diff(
        """diff --git a/src/example.py b/src/example.py
index 1111111..2222222 100644
--- a/src/example.py
+++ b/src/example.py
@@ -1,1 +1,1 @@
-return False
+return True
@@ -4,1 +4,1 @@
-return 1
+return 2
"""
    )
    assert [(hunk.path, hunk.old_start, hunk.new_start) for hunk in hunks] == [
        ("src/example.py", 1, 1),
        ("src/example.py", 4, 4),
    ]


def test_revert_probe_replaces_only_one_hunk_in_the_probe_copy(tmp_path):
    _implementation()
    target = tmp_path / "src" / "example.py"
    target.parent.mkdir()
    target.write_text("return True\nkeep\nreturn 2\n", encoding="utf-8")
    hunk = DiffHunk(
        path="src/example.py",
        header=("diff --git a/src/example.py b/src/example.py", "--- a/src/example.py", "+++ b/src/example.py"),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-return False", "+return True"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("return False",),
        new_lines=("return True",),
    )
    _apply_reverse_hunk(tmp_path, hunk)
    assert target.read_text(encoding="utf-8") == "return False\nkeep\nreturn 2\n"


def test_revert_probe_rejects_mixed_test_evidence(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("new\n", encoding="utf-8")
    hunk = DiffHunk(
        path="production.py",
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-old", "+new"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("old",),
        new_lines=("new",),
    )
    (source / "tests").mkdir()
    (source / "tests" / "test_fail.py").write_text(
        "def test_fail():\n    assert False, 'one target failed'\n", encoding="utf-8"
    )
    (source / "tests" / "test_pass.py").write_text(
        "def test_pass():\n    assert True\n", encoding="utf-8"
    )
    commands = {"py": PYTEST_COMMAND, "ts": "npm exec vitest run {test}"}

    assert (
        gate_revert_probe(
            source,
            [hunk],
            commands,
            ["tests/test_fail.py", "tests/test_pass.py"],
            tmp_path / "scratch",
        )
        == 1
    )


def test_revert_probe_rejects_result_target_count_mismatch(tmp_path, monkeypatch):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("new\n", encoding="utf-8")
    monkeypatch.setattr(
        change_gates,
        "_run_suite",
        lambda commands, cwd, tests: [
            CommandResult("one", 1, "", FailureClass.ASSERTION),
            CommandResult("two", 1, "", FailureClass.ASSERTION),
        ],
    )

    try:
        gate_revert_probe(source, [_hunk()], ["unused"], [], tmp_path / "scratch")
    except ValueError:
        pass
    else:
        assert False, "revert probe accepted mismatched evidence cardinality"


def test_probe_scratch_tree_contains_the_current_source(tmp_path):
    _implementation()
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("return True\n", encoding="utf-8")
    with _temporary_tree(source, tmp_path / "scratch") as probe:
        assert (probe / "production.py").read_text(encoding="utf-8") == "return True\n"


def test_mutation_candidates_are_restricted_to_changed_lines():
    _implementation()
    mutants = mutations_for_lines(
        "src/example.ts",
        ["const unchanged = true;", "return enabled && ready;"],
        [2],
    )
    assert mutants
    assert all(mutant.line == 2 for mutant in mutants)
    assert all(mutant.before == "return enabled && ready;" for mutant in mutants)
    assert any("||" in mutant.after for mutant in mutants)


def test_changed_line_mutation_rejects_mixed_test_evidence(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("enabled = True\n", encoding="utf-8")
    mutant = Mutant(
        path="production.py",
        line=1,
        before="enabled = True",
        after="enabled = False",
        description="mixed evidence",
    )
    commands = [
        "python3 -c 'from pathlib import Path; assert Path(\"production.py\").read_text() == \"enabled = True\\n\"'",
        "python3 -c 'from pathlib import Path; raise RuntimeError(\"invalid mutation evidence\") if Path(\"production.py\").read_text() != \"enabled = True\\n\" else None'",
    ]

    assert gate_changed_line_mutation(
        source,
        [mutant],
        commands,
        [],
        tmp_path / "scratch",
        "crucial",
    ) == 1


def test_changed_line_mutation_uses_the_cohesive_test_module_and_fail_fast(
    tmp_path, monkeypatch
):
    source = tmp_path / "source"
    (source / "scripts").mkdir(parents=True)
    (source / "scripts" / "change_gates.py").write_text("enabled = True\n", encoding="utf-8")
    observed = []

    def observe(commands, cwd, selected_tests, *, fail_fast=False):
        del commands, cwd
        observed.append((selected_tests, fail_fast))
        classification = FailureClass.ASSERTION if fail_fast else FailureClass.PASS
        return [CommandResult("test", 1 if fail_fast else 0, "", classification)]

    monkeypatch.setattr(change_gates, "_run_suite", observe)
    mutant = Mutant(
        path="scripts/change_gates.py",
        line=1,
        before="enabled = True",
        after="enabled = False",
        description="targeted mutation",
    )

    assert (
        gate_changed_line_mutation(
            source,
            [mutant],
            {"py": "pytest {test}"},
            ["tests/test_change_gate_reports.py", "tests/test_change_gates.py"],
            tmp_path / "scratch",
            "crucial",
        )
        == 0
    )
    assert observed == [
        (["tests/test_change_gate_reports.py", "tests/test_change_gates.py"], False),
        (["tests/test_change_gates.py"], True),
    ]


def test_changed_line_baseline_runs_in_a_scratch_tree(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("enabled = True\n", encoding="utf-8")
    mutant = Mutant(
        path="production.py",
        line=1,
        before="enabled = True",
        after="enabled = False",
        description="scratch baseline",
    )

    assert gate_changed_line_mutation(
        source,
        [mutant],
        ["python3 -c 'from pathlib import Path; Path(\"sentinel\").write_text(\"x\")'"],
        [],
        tmp_path / "scratch",
        "routine",
    ) == 0
    assert not (source / "sentinel").exists()


def test_mutation_candidates_do_not_delete_structural_lines():
    mutants = mutations_for_lines(
        "scripts/change_gate_reports.py",
        ["return enabled && ready;", "from pathlib import Path"],
        [1, 2],
    )

    assert mutants
    assert all(mutant.description != "changed line deleted" for mutant in mutants)


def test_right_reason_red_names_the_target_it_rejected(tmp_path):
    """A verdict of "rejected=1" with no named target is undiagnosable.

    The gate blocks the merge on this line, so a reader has to be able to tell
    which target was rejected and on what classification. Without that, the only
    way to find out is to rerun the whole gate by hand and bisect the targets.
    """
    source = tmp_path / "source"
    source.mkdir()
    (source / "tracked.py").write_text("base\n", encoding="utf-8")
    (source / "tests").mkdir()
    (source / "tests" / "existing.py").write_text("base test\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git", "-C", str(source),
            "-c", "user.name=Gate Test",
            "-c", "user.email=gate-test@example.invalid",
            "commit", "--quiet", "-m", "base",
        ],
        check=True,
    )

    # The target existed at the base and cannot be imported, so its red is a
    # typo rather than missing behaviour. That is the rejected case.
    # Split so this line does not itself read as a collection marker. pytest
    # echoes the source of a failing test, `classify_failure` substring-matches
    # the whole capture, and this file has to fail at the merge base by design.
    # The same idiom is already used above for the import marker. See #622.
    marker = "ERROR during " + "collection"
    command = f'{sys.executable} -c "print(\\"{marker}\\"); raise SystemExit(1)" {{test}}'
    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_right_reason_red(source, "HEAD", [command], ["tests/existing.py"], tmp_path / "scratch")

    assert result == 1
    # Assert against the TARGET lines alone rather than the whole capture.
    # `classify_failure` substring-matches the output, the command above emits a
    # marker string on purpose, and pytest echoes the entire asserted value when
    # an assertion fails. Asserting on the whole capture would replay that marker
    # into this file's own failure text and misclassify it. See #622.
    target_lines = [line for line in capture.getvalue().splitlines() if line.startswith("TARGET ")]
    assert target_lines, "gate printed no per-target line"
    assert "tests/existing.py" in target_lines[0]
    assert "REJECTED" in target_lines[0]


def test_right_reason_red_survives_a_scratch_tree_that_will_not_delete(tmp_path, monkeypatch):
    """A scratch tree that will not delete must not destroy the verdict.

    CI hit this for real: git left `.git` busy in the copied tree, cleanup raised
    `OSError: [Errno 39] Directory not empty: '.git'`, and the exception escaped
    the gate before it had printed anything. The run reported a crash with no
    verdict at all, which is worse than either a pass or a fail because there is
    nothing to act on and nothing to rerun against.
    """
    source = tmp_path / "source"
    source.mkdir()
    (source / "tests").mkdir()
    (source / "tests" / "existing.py").write_text("base test\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git", "-C", str(source),
            "-c", "user.name=Gate Test",
            "-c", "user.email=gate-test@example.invalid",
            "commit", "--quiet", "-m", "base",
        ],
        check=True,
    )

    # Only the scratch tree's own cleanup refuses. `_materialise_base` uses
    # rmtree too and must keep working, so a blanket patch would test nothing.
    real_rmtree = change_gates.shutil.rmtree

    def refuse_to_remove_the_scratch_tree(path, *args, **kwargs):
        if Path(path).name.startswith("gate-"):
            raise OSError(39, "Directory not empty", ".git")
        return real_rmtree(path, *args, **kwargs)

    monkeypatch.setattr(change_gates.shutil, "rmtree", refuse_to_remove_the_scratch_tree)

    command = f'{sys.executable} -c "raise SystemExit(0)" {{test}}'
    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_right_reason_red(source, "HEAD", [command], ["tests/existing.py"], tmp_path / "scratch")

    # The verdict itself is not the point here; reaching one at all is. Before
    # the fix the OSError escaped and no verdict line was printed.
    assert result in (0, 1)
    output = capture.getvalue()
    assert "RIGHT_REASON_RED:" in output
    assert "left behind" in output


def test_right_reason_red_and_revert_probe_close_structured_runner_children(tmp_path):
    source = tmp_path / "right-reason-source"
    source.mkdir()
    (source / "tests").mkdir()
    target = source / "tests" / "test_target.py"
    target.write_text("def test_target():\n    assert True\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(source), "add", "-A"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(source),
            "-c",
            "user.name=Gate Test",
            "-c",
            "user.email=gate-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
        check=True,
    )
    target.write_text("def test_target():\n    assert False\n", encoding="utf-8")
    commands = {"py": PYTEST_COMMAND, "ts": "npm exec vitest run {test}"}
    right_reason_scratch = tmp_path / "right-reason-scratch"

    assert (
        gate_right_reason_red(
            source,
            "HEAD",
            commands,
            ["tests/test_target.py"],
            right_reason_scratch,
        )
        == 0
    )
    assert not list(right_reason_scratch.glob("gate-*"))

    probe_source = tmp_path / "revert-source"
    probe_source.mkdir()
    (probe_source / "production.py").write_text("value = 'new'\n", encoding="utf-8")
    (probe_source / "tests").mkdir()
    (probe_source / "tests" / "test_behavior.py").write_text(
        "import sys\n"
        "from pathlib import Path\n"
        "sys.path.insert(0, str(Path(__file__).parents[1]))\n"
        "from production import value\n\n"
        "def test_behavior():\n"
        "    assert value == 'new'\n",
        encoding="utf-8",
    )
    hunk = DiffHunk(
        path="production.py",
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-value = 'old'", "+value = 'new'"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("value = 'old'",),
        new_lines=("value = 'new'",),
    )
    revert_scratch = tmp_path / "revert-scratch"

    assert (
        gate_revert_probe(
            probe_source,
            [hunk],
            commands,
            ["tests/test_behavior.py"],
            revert_scratch,
        )
        == 0
    )
    assert not list(revert_scratch.glob("gate-*"))
