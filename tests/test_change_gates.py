from contextlib import redirect_stdout
from pathlib import Path
import io
import subprocess
import sys
import tarfile

import pytest

from scripts import change_gates
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
    classify_failure,
    mutations_for_lines,
    parse_diff,
    run_command,
    right_reason_red_evidence,
    target_existed_at_base,
    _temporary_tree,
)


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


@pytest.mark.parametrize(
    "output",
    [
        "AssertionError: expected value containing ERROR during collection",
        "AssertionError: expected fixture 'missing' not found",
        "AssertionError: expected ModuleNotFoundError: missing dependency",
        "AssertionError: expected TypeError: marker text only",
        "AssertionError: expected foo is not a function",
    ],
)
def test_assertion_message_markers_do_not_override_assertion_classification(output):
    assert classify_failure(1, output) is FailureClass.ASSERTION


def test_unstructured_runner_marker_does_not_classify_as_mechanical_failure():
    output = "the assertion message mentioned ModuleNotFoundError but the runner gave no diagnostic"

    assert classify_failure(1, output) is FailureClass.UNKNOWN


def test_assertion_with_pytest_fixture_dump_stays_an_assertion():
    fixture_dump = "capsys = <Capture" + "Fixture object>"

    assert classify_failure(1, "AssertionError: expected value\n" + fixture_dump) is FailureClass.ASSERTION


def test_existing_target_collection_error_is_rejected():
    output = "ERROR during collection"
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=True) is None


def test_new_target_collection_error_is_rejected_without_an_explicit_policy():
    output = "ERROR during collection"
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=False) is None


@pytest.mark.parametrize(
    "output",
    [
        "SyntaxError: invalid syntax",
        "fixture 'missing' not found",
        "ModuleNotFoundError: No module named 'missing'",
        "runner exited with status 1",
    ],
)
def test_new_target_mechanical_failures_are_rejected(output):
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=False) is None


def test_new_target_assertion_failure_is_accepted_as_assertion_evidence():
    output = "AssertionError: expected new behaviour"
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=False) == "assertion"


def test_existing_target_assertion_failure_is_accepted():
    output = "AssertionError: expected value"
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=True) == "assertion"


def test_existing_target_import_typo_is_rejected():
    # Written plainly. This text is exactly what a NEW target also produces, so
    # the whole point of the criterion is that it never reads this string: a
    # target that existed at the base and cannot be imported is a typo, and a
    # target that did not exist is ordinary new-file creation.
    output = "ModuleNotFoundError: No module named typo"
    result = CommandResult("test", 1, output, classify_failure(1, output))

    assert right_reason_red_evidence(result, target_existed=True) is None


def test_right_reason_red_rejects_collection_error_for_target_new_at_base(tmp_path):
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

    assert result == 1


def test_ts_right_reason_red_provisions_protocol_build_in_materialised_tree(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "scripts").mkdir()
    verifier = source / "runtime" / "agent-fabric" / "scripts" / "lib" / "npm-install-attestation.mjs"
    verifier.parent.mkdir(parents=True)
    verifier.write_text("base verifier\n", encoding="utf-8")
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "printf '%s\\n' \"$AGENTS_HOME\" > \"$AGENTS_HOME/protocol-build-agents-home\"\n"
        "cat \"$AGENTS_HOME/runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs\" "
        "> \"$AGENTS_HOME/protocol-verifier-content\"\n"
        ": > \"$AGENTS_HOME/protocol-ready\"\n",
        encoding="utf-8",
    )
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "ts_gate.py").write_text(
        "from pathlib import Path\n"
        "import sys\n"
        "if not Path('protocol-ready').is_file():\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: local @local/agent-fabric-protocol dist is missing')\n"
        "    raise SystemExit(1)\n"
        "assert Path('protocol-build-agents-home').read_text().strip() == str(Path.cwd())\n"
        "if Path('protocol-verifier-content').read_text() != 'current verifier\\n':\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: current attestation verifier was not used')\n"
        "    raise SystemExit(1)\n"
        "assert Path('runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs').read_text() == 'base verifier\\n'\n"
        "raise AssertionError('expected right-reason red after protocol build')\n",
        encoding="utf-8",
    )
    target = source / "tests" / "protocol.test.ts"
    target.write_text("base test\n", encoding="utf-8")
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
    verifier.write_text("current verifier\n", encoding="utf-8")
    target.write_text("current test\n", encoding="utf-8")

    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_right_reason_red(
            source,
            "HEAD",
            {
                "py": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
                "ts": f'{sys.executable} tests/ts_gate.py {{test}}',
            },
            ["tests/protocol.test.ts"],
            tmp_path / "scratch",
        )
    output = capture.getvalue()

    assert result == 0, f"expected TS gate to accept the assertion red, got {result}:\n{output}"
    assert "classification=assertion-failure" in output
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" not in output


def test_ts_right_reason_red_reprovisions_untracked_install_artifacts(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "scripts").mkdir()
    (source / "runtime" / "agent-fabric").mkdir(parents=True)
    (source / "node_modules").mkdir()
    (source / "node_modules" / "installed-marker").write_text("installed\n", encoding="utf-8")
    (source / ".gitignore").write_text("node_modules/\n", encoding="utf-8")
    (source / "runtime" / "agent-fabric" / ".gitignore").write_text(
        ".npm-ci-attestation\n", encoding="utf-8"
    )
    (source / "runtime" / "agent-fabric" / ".npm-ci-attestation").write_text(
        "attested\n", encoding="utf-8"
    )
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "import { writeFile } from 'node:fs/promises';\n"
        "import { join } from 'node:path';\n"
        "await writeFile(join(process.argv[2], 'runtime/agent-fabric/.npm-ci-attestation'), 'refreshed\\n');\n",
        encoding="utf-8",
    )
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "if test -d \"$AGENTS_HOME/node_modules\" && "
        "test ! -L \"$AGENTS_HOME/node_modules\" && "
        "test -f \"$AGENTS_HOME/node_modules/installed-marker\" && "
        "test \"$(cat \"$AGENTS_HOME/runtime/agent-fabric/.npm-ci-attestation\")\" = refreshed; then\n"
        "    : > \"$AGENTS_HOME/install-artifacts-provisioned\"\n"
        "fi\n",
        encoding="utf-8",
    )
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "provisioning.test.ts").write_text("base test\n", encoding="utf-8")
    (source / "tests" / "provisioning_gate.py").write_text(
        "from pathlib import Path\n"
        "if not Path('install-artifacts-provisioned').is_file():\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: install artifacts were not provisioned')\n"
        "    raise SystemExit(1)\n"
        "raise AssertionError('expected right-reason red after install provisioning')\n",
        encoding="utf-8",
    )
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
    (source / "tests" / "provisioning.test.ts").write_text("current test\n", encoding="utf-8")

    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_right_reason_red(
            source,
            "HEAD",
            {
                "py": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
                "ts": f'{sys.executable} tests/provisioning_gate.py {{test}}',
            },
            ["tests/provisioning.test.ts"],
            tmp_path / "scratch",
        )
    output = capture.getvalue()

    assert result == 0, f"expected install artifacts to be provisioned, got {result}:\n{output}"
    assert "classification=assertion-failure" in output
    assert "AGENT_FABRIC_PROTOCOL_BUILD_STALE" not in output


def test_materialised_tests_remap_workspace_links_into_the_scratch_tree(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / ".gitignore").write_text("node_modules/\n", encoding="utf-8")
    package = source / "runtime" / "workspace-package"
    package.mkdir(parents=True)
    (package / "sentinel.txt").write_text("base\n", encoding="utf-8")
    (source / "node_modules" / "@local").mkdir(parents=True)
    (source / "node_modules" / "@local" / "workspace").symlink_to(
        "../../runtime/workspace-package", target_is_directory=True
    )
    (source / "tests").mkdir()
    (source / "tests" / "workspace.test.ts").write_text("base test\n", encoding="utf-8")
    (source / "check_workspace.py").write_text(
        "from pathlib import Path\n"
        "value = Path('node_modules/@local/workspace/sentinel.txt').read_text()\n"
        "if value == 'current\\n':\n"
        "    print('SOURCE_WORKSPACE_LINK_LEAK')\n"
        "    raise SystemExit(1)\n"
        "raise AssertionError('expected isolated materialised workspace link')\n",
        encoding="utf-8",
    )
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
    (package / "sentinel.txt").write_text("current\n", encoding="utf-8")
    (source / "tests" / "workspace.test.ts").write_text("current test\n", encoding="utf-8")

    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_right_reason_red(
            source,
            "HEAD",
            [f"{sys.executable} check_workspace.py {{test}}"],
            ["tests/workspace.test.ts"],
            tmp_path / "scratch",
        )

    output = capture.getvalue()
    assert result == 0, f"materialised workspace link leaked to source:\n{output}"
    assert "SOURCE_WORKSPACE_LINK_LEAK" not in output
    assert "classification=assertion-failure" in output


def test_materialised_install_provisioning_rejects_symlinked_attestation_parents(tmp_path):
    source = tmp_path / "source"
    source_attestation = source / "runtime" / "agent-fabric" / ".npm-ci-attestation"
    source_attestation.parent.mkdir(parents=True)
    source_attestation.write_text("attested\n", encoding="utf-8")
    outside = tmp_path / "outside"
    outside.mkdir()

    source_attestation.unlink()
    (source / "runtime" / "agent-fabric").rmdir()
    (source / "runtime" / "agent-fabric").symlink_to(outside, target_is_directory=True)
    (source / "tests").mkdir()
    (source / "tests" / "symlink.test.ts").write_text("base test\n", encoding="utf-8")
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
    (source / "runtime" / "agent-fabric").unlink()
    (source / "runtime" / "agent-fabric").mkdir()
    source_attestation.write_text("attested\n", encoding="utf-8")
    (source / "tests" / "symlink.test.ts").write_text("current test\n", encoding="utf-8")

    raised = False
    try:
        gate_right_reason_red(
            source,
            "HEAD",
            {
                "py": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
                "ts": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
            },
            ["tests/symlink.test.ts"],
            tmp_path / "scratch",
        )
    except GateError as error:
        raised = "contains a symlink" in str(error)
    assert raised
    assert not (outside / "agent-fabric" / ".npm-ci-attestation").exists()


def test_changed_test_copy_rejects_symlinked_destination_components(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (source / "tracked.py").write_text("base\n", encoding="utf-8")
    (source / "tests").symlink_to(outside, target_is_directory=True)
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
    (source / "tests").unlink()
    (source / "tests").mkdir()
    (source / "tests" / "changed.py").write_text("current test\n", encoding="utf-8")
    destination = tmp_path / "destination"
    destination.mkdir()

    with pytest.raises(GateError, match="contains a symlink"):
        _materialise_base(source, "HEAD", destination, ["tests/changed.py"])

    assert not (outside / "changed.py").exists()


def test_archive_materialisation_rejects_symlinked_destination_components(tmp_path, monkeypatch):
    source = tmp_path / "source"
    source.mkdir()
    destination = tmp_path / "destination"
    destination.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    archive_bytes = io.BytesIO()
    with tarfile.open(fileobj=archive_bytes, mode="w") as archive:
        link = tarfile.TarInfo("tests")
        link.type = tarfile.SYMTYPE
        link.linkname = "../outside"
        archive.addfile(link)
        payload = b"escaped\n"
        member = tarfile.TarInfo("tests/escaped.py")
        member.size = len(payload)
        archive.addfile(member, io.BytesIO(payload))
    archive_payload = archive_bytes.getvalue()
    real_run = change_gates.subprocess.run

    def fake_run(arguments, *args, **kwargs):
        if arguments[:4] == ["git", "-C", str(source), "archive"]:
            return subprocess.CompletedProcess(arguments, 0, stdout=archive_payload, stderr=b"")
        return real_run(arguments, *args, **kwargs)

    monkeypatch.setattr(change_gates.subprocess, "run", fake_run)

    with pytest.raises(GateError, match="contains a symlink"):
        _materialise_base(source, "HEAD", destination, [])

    assert not (outside / "escaped.py").exists()


def test_overlay_rejects_symlinked_destination_components(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    outside = tmp_path / "outside.mjs"
    outside.write_text("outside verifier\n", encoding="utf-8")
    base_library = source / "runtime" / "agent-fabric" / "scripts" / "lib" / "npm-install-attestation.mjs"
    base_library.parent.mkdir(parents=True)
    base_library.symlink_to(outside)
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "overlay.test.ts").write_text("base test\n", encoding="utf-8")
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
    base_library.unlink()
    base_library.write_text("current verifier\n", encoding="utf-8")
    (source / "tests" / "overlay.test.ts").write_text("current test\n", encoding="utf-8")

    with pytest.raises(GateError, match="contains a symlink"):
        gate_right_reason_red(
            source,
            "HEAD",
            {
                "py": f'{sys.executable} -c "pass" {{test}}',
                "ts": f'{sys.executable} -c "raise AssertionError(\\"expected\\")" {{test}}',
            },
            ["tests/overlay.test.ts"],
            tmp_path / "scratch",
        )

    assert outside.read_text(encoding="utf-8") == "outside verifier\n"


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
    """A suite that cannot run fails the probe rather than laundering uncertainty.

    Reverting a hunk can break the very import the tests need, which reds the
    suite for a mechanical reason. That is an inability to measure, and treating
    it as a passing probe reports certification on evidence that says only "I
    could not tell". A survivor is a suite that stayed GREEN.
    """
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("new\n", encoding="utf-8")
    # Exits non-zero with a non-assertion setup error, never an assertion failure.
    command = "python3 -c 'import no_such_module_at_all'"

    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_revert_probe(source, [_hunk()], [command], [], tmp_path / "scratch")

    assert result == 1
    assert "REVERT_PROBE: FAIL" in capture.getvalue()
    assert "inconclusive=1" in capture.getvalue()


def test_revert_probe_turns_materialisation_gate_errors_into_inconclusive_evidence(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "production.py").write_text("new\n", encoding="utf-8")
    (source / "tests").mkdir()
    (source / "tests" / "missing-build.test.ts").write_text("base\n", encoding="utf-8")

    raised = False
    capture = io.StringIO()
    with redirect_stdout(capture):
        try:
            result = gate_revert_probe(
                source,
                [_hunk()],
                {"py": "python3 -c 'pass'", "ts": "python3 -c 'pass' {test}"},
                ["tests/missing-build.test.ts"],
                tmp_path / "scratch",
            )
        except GateError:
            raised = True

    assert not raised
    assert result == 1
    assert "inconclusive=1" in capture.getvalue()


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


def test_mutation_write_rejects_a_symlinked_destination(tmp_path):
    outside = tmp_path / "outside.py"
    outside.write_text("outside\n", encoding="utf-8")
    target = tmp_path / "production.py"
    target.symlink_to(outside)
    mutant = Mutant(
        path="production.py",
        line=1,
        before="enabled = True",
        after="enabled = False",
        description="symlink destination",
    )

    with pytest.raises(GateError, match="contains a symlink"):
        change_gates._write_mutant(tmp_path, mutant)

    assert outside.read_text(encoding="utf-8") == "outside\n"


def test_mutation_write_rechecks_the_destination_before_writing(tmp_path, monkeypatch):
    outside = tmp_path / "outside.py"
    outside.write_text("enabled = True\n", encoding="utf-8")
    target = tmp_path / "production.py"
    target.write_text("enabled = True\n", encoding="utf-8")
    real_read_text = Path.read_text

    def swap_to_symlink(path, *args, **kwargs):
        if path == target:
            target.unlink()
            target.symlink_to(outside)
        return real_read_text(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", swap_to_symlink)
    mutant = Mutant(
        path="production.py",
        line=1,
        before="enabled = True",
        after="enabled = False",
        description="symlink race destination",
    )

    with pytest.raises(GateError, match="contains a symlink"):
        change_gates._write_mutant(tmp_path, mutant)

    assert outside.read_text(encoding="utf-8") == "enabled = True\n"


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


def test_reverse_hunk_rejects_a_symlinked_destination(tmp_path):
    outside = tmp_path / "outside.py"
    outside.write_text("outside\n", encoding="utf-8")
    target = tmp_path / "example.py"
    target.symlink_to(outside)
    hunk = DiffHunk(
        path="example.py",
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-return False", "+return True"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("return False",),
        new_lines=("return True",),
    )

    with pytest.raises(GateError, match="contains a symlink"):
        _apply_reverse_hunk(tmp_path, hunk)

    assert outside.read_text(encoding="utf-8") == "outside\n"


def test_reverse_hunk_rechecks_the_destination_before_writing(tmp_path, monkeypatch):
    outside = tmp_path / "outside.py"
    outside.write_text("return True\n", encoding="utf-8")
    target = tmp_path / "example.py"
    target.write_text("return True\n", encoding="utf-8")
    real_read_text = Path.read_text

    def swap_to_symlink(path, *args, **kwargs):
        if path == target:
            target.unlink()
            target.symlink_to(outside)
        return real_read_text(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", swap_to_symlink)
    hunk = DiffHunk(
        path="example.py",
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-return False", "+return True"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("return False",),
        new_lines=("return True",),
    )

    with pytest.raises(GateError, match="contains a symlink"):
        _apply_reverse_hunk(tmp_path, hunk)

    assert outside.read_text(encoding="utf-8") == "return True\n"


def test_revert_probe_rejects_mixed_assertion_and_green_evidence(tmp_path):
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

    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_revert_probe(source, [hunk], commands, [], tmp_path / "scratch")

    assert result == 1
    assert "INCONCLUSIVE mixed assertion/green evidence" in capture.getvalue()
    assert "inconclusive=1" in capture.getvalue()


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
    # Keep the diagnostic-shaped line separate from the target result. This
    # models a genuine mechanical collection failure and ensures it cannot be
    # accepted as justified assertion-red evidence. See #622.
    marker = "ERROR during " + "collection"
    command = f'{sys.executable} -c "print(\\"{marker}\\"); raise SystemExit(1)" {{test}}'
    capture = io.StringIO()
    with redirect_stdout(capture):
        result = gate_right_reason_red(source, "HEAD", [command], ["tests/existing.py"], tmp_path / "scratch")

    assert result == 1
    # Assert against TARGET lines alone rather than the whole capture. The
    # command emits a structured collection diagnostic, and target-level
    # assertions must not be accepted through that mechanical failure. See #622.
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
