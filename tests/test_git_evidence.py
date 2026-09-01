from __future__ import annotations

import json
import io
import os
import subprocess
from pathlib import Path

import pytest
from scripts import git_evidence

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/git_evidence.py"


def git(cwd: Path, *args: str, env: dict[str, str] | None = None) -> str:
    return subprocess.check_output(["git", *args], cwd=cwd, text=True, env=env).strip()


@pytest.fixture
def repository(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    git(repo, "init", "-q")
    git(repo, "config", "user.email", "test@example.invalid")
    git(repo, "config", "user.name", "Test")
    (repo / "file.txt").write_text("before\n", encoding="utf-8")
    git(repo, "add", "file.txt")
    git(repo, "commit", "-qm", "initial")
    (repo / "file.txt").write_text("after\n", encoding="utf-8")
    return repo


def test_materialise_records_exact_head_status_and_selected_diff_under_redirect(repository, tmp_path):
    output = tmp_path / "run" / "evidence.md"
    output.parent.mkdir()
    environment = os.environ.copy()
    environment.update({"GIT_DIR": str(tmp_path / "wrong.git"), "GIT_WORK_TREE": str(tmp_path)})
    result = subprocess.run(
        [
            "python3", str(SCRIPT), "--repository", str(repository), "--output", str(output),
            "--diff-from", "HEAD", "--path", "file.txt",
        ],
        cwd=repository,
        env=environment,
        text=True,
        capture_output=True,
    )
    assert result.returncode == 0, result.stderr
    lines = output.read_text(encoding="utf-8").splitlines()
    header = json.loads(lines[0])
    assert header["record_type"] == "provenant-git-evidence"
    assert header["repository"] == str(repository.resolve())
    assert header["head"] == git(repository, "rev-parse", "HEAD")
    assert header["diff_base"] == header["head"]
    assert header["working_tree"] == "dirty"
    assert header["diff_from"] == "HEAD"
    assert header["paths"] == ["file.txt"]
    assert header["encoding"] == "utf-8-replacement"
    packet = output.read_text(encoding="utf-8")
    assert "--- status ---" in packet
    assert "--- diff ---" in packet
    assert "+after" in packet


def test_materialise_works_from_registered_linked_worktree(repository, tmp_path):
    linked = tmp_path / "linked"
    git(repository, "worktree", "add", "-q", str(linked), "HEAD")
    output = tmp_path / "run" / "linked.md"
    output.parent.mkdir()
    result = subprocess.run(
        [
            "python3", str(SCRIPT), "--repository", str(linked), "--output", str(output),
            "--diff-from", "HEAD",
        ],
        text=True,
        capture_output=True,
    )
    assert result.returncode == 0, result.stderr
    header = json.loads(output.read_text(encoding="utf-8").splitlines()[0])
    assert header["repository"] == str(linked.resolve())
    assert header["head"] == git(linked, "rev-parse", "HEAD")
    assert header["diff_base"] == header["head"]


@pytest.mark.parametrize("linked", [False, True])
def test_materialise_scopes_untracked_rejection_to_selected_paths(repository, tmp_path, linked):
    target = repository
    if linked:
        target = tmp_path / "linked-scoped"
        git(repository, "worktree", "add", "-q", str(target), "HEAD")
    (target / "unrelated.txt").write_text("not selected\n", encoding="utf-8")
    output = tmp_path / ("linked" if linked else "primary") / "allowed.md"
    output.parent.mkdir()

    metadata = git_evidence.materialise_packet(target, output, diff_from="HEAD", paths=["file.txt"])

    assert metadata["paths"] == ["file.txt"]
    (target / "selected.txt").write_text("not captured\n", encoding="utf-8")
    with pytest.raises(git_evidence.GitEvidenceUntrackedError, match="git_evidence_untracked"):
        git_evidence.materialise_packet(target, output.with_name("rejected.md"), diff_from="HEAD", paths=["selected.txt"])


def test_materialise_treats_metacharacter_path_as_literal(repository, tmp_path):
    (repository / "other.py").write_text("not selected\n", encoding="utf-8")
    output = tmp_path / "literal.md"

    git_evidence.materialise_packet(repository, output, diff_from="HEAD", paths=["*.py"])

    packet = output.read_text(encoding="utf-8")
    assert "not selected" not in packet
    assert "?? other.py" in packet
    (repository / "*.py").write_text("selected but untracked\n", encoding="utf-8")
    with pytest.raises(git_evidence.GitEvidenceUntrackedError, match="git_evidence_untracked"):
        git_evidence.materialise_packet(repository, output.with_name("literal-rejected.md"), diff_from="HEAD", paths=["*.py"])


def test_materialise_binds_symbolic_diff_base_to_object_id(repository, tmp_path):
    (repository / "file.txt").write_text("second\n", encoding="utf-8")
    git(repository, "add", "file.txt")
    git(repository, "commit", "-qm", "second")
    base = git(repository, "rev-parse", "HEAD~1")
    output = tmp_path / "run" / "base.md"
    output.parent.mkdir()

    metadata = git_evidence.materialise_packet(repository, output, diff_from="HEAD~1")

    assert metadata["diff_from"] == "HEAD~1"
    assert metadata["diff_base"] == base
    assert "+second" in output.read_text(encoding="utf-8")


def test_materialise_requires_an_explicit_diff_base(repository, tmp_path):
    result = subprocess.run(
        ["python3", str(SCRIPT), "--repository", str(repository), "--output", str(tmp_path / "x")],
        text=True,
        capture_output=True,
    )
    assert result.returncode != 0
    assert "--diff-from" in result.stderr


def test_materialise_rejects_untracked_content_instead_of_claiming_a_complete_diff(repository, tmp_path):
    (repository / "untracked.txt").write_text("not captured\n", encoding="utf-8")
    result = subprocess.run(
        [
            "python3", str(SCRIPT), "--repository", str(repository), "--output", str(tmp_path / "x"),
            "--diff-from", "HEAD",
        ],
        text=True,
        capture_output=True,
    )
    assert result.returncode != 0
    assert "git_evidence_untracked" in result.stderr
    assert not (tmp_path / "x").exists()


def test_status_disables_configured_fsmonitor_and_diff_disables_textconv(repository, tmp_path):
    marker = tmp_path / "hook-ran"
    hook = tmp_path / "fsmonitor-hook"
    hook.write_text(f"#!/bin/sh\ntouch {marker}\n", encoding="utf-8")
    hook.chmod(0o755)
    git(repository, "config", "core.fsmonitor", str(hook))
    textconv_marker = tmp_path / "textconv-ran"
    textconv = tmp_path / "textconv"
    textconv.write_text(f"#!/bin/sh\ntouch {textconv_marker}\ncat \"$1\"\n", encoding="utf-8")
    textconv.chmod(0o755)
    git(repository, "config", "diff.provenant.textconv", str(textconv))
    filter_marker = tmp_path / "filter-ran"
    filter_hook = tmp_path / "filter-hook"
    filter_hook.write_text(f"#!/bin/sh\ntouch {filter_marker}\ncat\n", encoding="utf-8")
    filter_hook.chmod(0o755)
    included_config = tmp_path / "included-filter.cfg"
    included_config.write_text(
        f'[filter "evil"]\n\tclean = {filter_hook}\n\trequired = true\n', encoding="utf-8"
    )
    git(repository, "config", "include.path", str(included_config))
    (repository / ".gitattributes").write_text(
        "file.txt diff=provenant filter=evil\n", encoding="utf-8"
    )
    git(repository, "add", ".gitattributes")
    git(repository, "commit", "-qm", "attributes")
    (repository / "file.txt").write_bytes(b"\xffchanged\n")
    expected_head = git(repository, "rev-parse", "HEAD")
    marker.unlink(missing_ok=True)
    textconv_marker.unlink(missing_ok=True)
    filter_marker.unlink(missing_ok=True)
    output = tmp_path / "run" / "evidence.md"
    output.parent.mkdir()

    metadata = git_evidence.materialise_packet(repository, output, diff_from="HEAD")

    assert metadata["head"] == expected_head
    assert not marker.exists()
    assert not textconv_marker.exists()
    assert not filter_marker.exists()


def test_filter_discovery_covers_linked_worktree_config(repository, tmp_path):
    git(repository, "config", "extensions.worktreeConfig", "true")
    linked = tmp_path / "linked-filter"
    git(repository, "worktree", "add", "-q", str(linked), "HEAD")
    filter_marker = tmp_path / "worktree-filter-ran"
    filter_hook = tmp_path / "worktree-filter-hook"
    filter_hook.write_text(f"#!/bin/sh\ntouch {filter_marker}\ncat\n", encoding="utf-8")
    filter_hook.chmod(0o755)
    git(linked, "config", "--worktree", "filter.worktree.clean", str(filter_hook))
    git(linked, "config", "--worktree", "filter.worktree.required", "true")
    (linked / ".gitattributes").write_text("file.txt filter=worktree\n", encoding="utf-8")
    git(linked, "add", ".gitattributes")
    git(linked, "commit", "-qm", "worktree attributes")
    (linked / "file.txt").write_bytes(b"linked change\n")
    filter_marker.unlink(missing_ok=True)
    output = tmp_path / "linked-run" / "evidence.md"
    output.parent.mkdir()

    git_evidence.materialise_packet(linked, output, diff_from="HEAD")

    assert not filter_marker.exists()


def test_diff_stream_replaces_invalid_utf8_deterministically(monkeypatch, tmp_path):
    class FakeProcess:
        args = ["git", "diff"]
        stdout = io.BytesIO(b"ok\xff\n")
        stderr = io.BytesIO()

        def wait(self):
            return 0

    monkeypatch.setattr(git_evidence.subprocess, "Popen", lambda *args, **kwargs: FakeProcess())
    destination = io.BytesIO()

    git_evidence._git_diff_to_file(tmp_path, destination, ["diff"])

    assert destination.getvalue() == "ok\ufffd\n".encode("utf-8")


def test_diff_stream_preserves_utf8_split_across_chunks(monkeypatch, tmp_path):
    class SplitStream:
        def __init__(self):
            self.chunks = iter([b"prefix \xc3", b"\xa9 suffix\n", b""])

        def read(self, _size):
            return next(self.chunks)

        def close(self):
            pass

    class SplitProcess:
        args = ["git", "diff"]
        stdout = SplitStream()
        stderr = io.BytesIO()

        def wait(self):
            return 0

    monkeypatch.setattr(git_evidence.subprocess, "Popen", lambda *args, **kwargs: SplitProcess())
    destination = io.BytesIO()

    git_evidence._git_diff_to_file(tmp_path, destination, ["diff"])

    assert destination.getvalue() == "prefix é suffix\n".encode("utf-8")


def test_diff_stream_cleans_up_child_when_destination_fails(monkeypatch, tmp_path):
    class FailingStream:
        def read(self, _size):
            raise OSError("destination is closed")

        def close(self):
            pass

    class FailingProcess:
        args = ["git", "diff"]
        stdout = FailingStream()
        stderr = io.BytesIO()
        terminated = False
        waited = False

        def poll(self):
            return None

        def terminate(self):
            self.terminated = True

        def wait(self, timeout=None):
            self.waited = True
            return -15

        def kill(self):
            raise AssertionError("terminate should have stopped the child")

    process = FailingProcess()
    monkeypatch.setattr(git_evidence.subprocess, "Popen", lambda *args, **kwargs: process)

    with pytest.raises(OSError, match="destination is closed"):
        git_evidence._git_diff_to_file(tmp_path, io.BytesIO(), ["diff"])
    assert process.terminated
    assert process.waited


def test_materialise_fails_if_checkout_changes_during_capture(monkeypatch, tmp_path):
    repository = tmp_path / "repo"
    repository.mkdir()
    output = tmp_path / "packet.md"
    heads = iter(["a" * 40, "a" * 40, "b" * 40])

    def fake_git_output(_repository, *args, **kwargs):
        if args[0] == "rev-parse" and "--show-toplevel" in args:
            return str(repository)
        if args[0] == "rev-parse":
            return next(heads)
        return ""

    monkeypatch.setattr(git_evidence, "git_output", fake_git_output)
    monkeypatch.setattr(git_evidence, "_git_diff_to_file", lambda *args, **kwargs: None)

    with pytest.raises(git_evidence.GitEvidenceChangedError, match="git_evidence_changed"):
        git_evidence.materialise_packet(repository, output, diff_from="HEAD")
    assert not output.exists()


def test_materialise_fails_if_tracked_file_changes_during_diff(monkeypatch, tmp_path):
    repository = tmp_path / "repo"
    repository.mkdir()
    output = tmp_path / "packet.md"
    snapshots = iter([
        {"file.txt": (1, 2, 0o100644, 5, 10)},
        {"file.txt": (1, 2, 0o100644, 6, 11)},
    ])

    def fake_git_output(_repository, *args, **kwargs):
        if args[0] == "rev-parse" and "--show-toplevel" in args:
            return str(repository)
        if args[0] == "rev-parse":
            return "a" * 40
        return ""

    monkeypatch.setattr(git_evidence, "git_output", fake_git_output)
    monkeypatch.setattr(git_evidence, "_tracked_snapshot", lambda *args, **kwargs: next(snapshots))
    monkeypatch.setattr(git_evidence, "_git_diff_to_file", lambda *args, **kwargs: None)

    with pytest.raises(git_evidence.GitEvidenceChangedError, match="git_evidence_changed"):
        git_evidence.materialise_packet(repository, output, diff_from="HEAD")
    assert not output.exists()
