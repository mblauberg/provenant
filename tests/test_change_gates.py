from enum import Enum
from dataclasses import dataclass
from pathlib import Path
import subprocess
import sys

import pytest

try:
    from scripts.change_gates import (
        DiffHunk,
        GateError,
        Mutant,
        FailureClass,
        _apply_reverse_hunk,
        _materialise_base,
        _targets,
        gate_changed_line_mutation,
        gate_revert_probe,
        classify_failure,
        mutations_for_lines,
        parse_diff,
        run_command,
        _temporary_tree,
    )
except ModuleNotFoundError:  # The merge-base red must remain an assertion failure.
    @dataclass(frozen=True)
    class DiffHunk:
        path: str
        header: tuple[str, ...]
        hunk_header: str
        body: tuple[str, ...]
        old_start: int
        old_count: int
        new_start: int
        new_count: int
        old_lines: tuple[str, ...]
        new_lines: tuple[str, ...]

    @dataclass(frozen=True)
    class Mutant:
        path: str
        line: int
        before: str
        after: str | None
        description: str

    class _FallbackFailureClass(str, Enum):
        ASSERTION = "assertion-failure"

    FailureClass = _FallbackFailureClass
    classify_failure = lambda _returncode, _output: FailureClass.ASSERTION
    gate_revert_probe = lambda *_args, **_kwargs: 0
    gate_changed_line_mutation = lambda *_args, **_kwargs: 0
    GateError = _apply_reverse_hunk = _materialise_base = None
    _targets = mutations_for_lines = parse_diff = run_command = _temporary_tree = None


ROOT = Path(__file__).resolve().parents[1]
GATE = ROOT / "scripts" / "check-test-gates"


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


def test_run_command_does_not_interpret_shell_metacharacters(tmp_path):
    marker = tmp_path / "shell-was-used"
    command = f'{sys.executable} -c "import sys; sys.exit(0)" ; touch {marker}'

    result = run_command(command, tmp_path)

    assert result.returncode == 0
    assert marker.exists() is False


def test_right_reason_red_rejects_collection_errors_as_assertion_reds():
    assert classify_failure(1, "ERROR during collection") is not FailureClass.ASSERTION


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
    assert classify(1, "E ModuleNotFoundError: No module named 'new_code'") is failure_class.IMPORT
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
    commands = ["python3 -c 'assert False, \"one target failed\"'", "python3 -c 'pass'"]

    assert gate_revert_probe(source, [hunk], commands, [], tmp_path / "scratch") == 1


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
