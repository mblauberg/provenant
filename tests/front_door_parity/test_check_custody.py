"""`check` custody: registered-checkout selection (contract C§6; assertions 35 to 39).

The selection logic stays in Python (ADR 0027), so for the Rust front door these
cases prove the helper handshake as well as the policy. The fixtures follow the
cases in `tests/test_provenant_cli.py` (custody section), run differentially.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess

import pytest

from parity_support import ROLES, FrontDoor, add_linked, build_checkout, git, init_primary


@pytest.fixture
def repo(fd):
    """The fixture checkout as a primary git repo with one registered linked worktree."""
    init_primary(fd.checkout)
    fd.linked = add_linked(fd.checkout, fd.tmp / "linked", "feature")
    return fd


def checker(result) -> str:
    return result.payload()["argv0"]


def selected_roots(result) -> tuple[str, str]:
    env = result.payload()["env"]
    return env["AGENT_FABRIC_PRODUCT_ROOT"], env["AGENTS_HOME"]


def assert_refused(result, *needles: str) -> None:
    assert result.returncode == 3, (result.out, result.err)
    assert result.stdout == b""
    assert result.err.startswith("provenant: ")
    assert any(n in result.err for n in needles), result.err
    assert "Traceback" not in result.err


def test_primary_linked_nested_and_non_repo_callers_select_their_own_checker(repo):
    """C§16 #35: the registered checkout containing the caller supplies the checker."""
    nested = repo.linked / "scripts" / "lib"
    outside = repo.tmp / "outside"
    outside.mkdir()
    cases = [
        (repo.checkout, repo.checkout),
        (repo.checkout / "scripts", repo.checkout),
        (repo.linked, repo.linked),
        (nested, repo.linked),
        (outside, repo.checkout),  # non-repo cwd: the product checkout
    ]
    for cwd, root in cases:
        result = repo.run("check", "--flag", "two words", cwd=cwd)
        payload = result.payload()
        assert checker(result) == str(root / "scripts/check-harness"), (cwd, result.err)
        assert payload["argv"] == ["--flag", "two words"]
        assert payload["cwd"] == str(cwd)
        assert selected_roots(result) == (str(root), str(root))


def test_copied_linked_worktree_is_refused(repo):
    """C§16 #36: a copy of a registered worktree is not a registered worktree."""
    copy = repo.tmp / "copy"
    shutil.copytree(repo.linked, copy, symlinks=True)

    result = repo.run("check", cwd=copy)

    assert_refused(result, "not a registered worktree of product root", "invalid Git metadata")


def test_orphaned_copy_is_refused(repo):
    """C§16 #36: the registration is gone, the copy remains."""
    copy = repo.tmp / "orphan"
    shutil.copytree(repo.linked, copy, symlinks=True)
    git(repo.checkout, "worktree", "remove", "--force", str(repo.linked))

    assert_refused(repo.run("check", cwd=copy), "invalid Git metadata", "not a registered")


def test_copy_nested_in_a_registered_checkout_is_refused(repo):
    """C§16 #36: a copy dropped inside a registered checkout does not inherit its identity."""
    copy = repo.checkout / "nested-copy"
    shutil.copytree(repo.linked, copy, symlinks=True)

    assert_refused(repo.run("check", cwd=copy), "not a registered worktree", "invalid Git metadata")


def test_registered_path_replaced_by_a_symlink_is_refused(repo):
    """C§16 #36: the registered path now points somewhere else."""
    moved = repo.tmp / "moved"
    repo.linked.rename(moved)
    repo.linked.symlink_to(moved)

    assert_refused(repo.run("check", cwd=repo.linked), "symlink", "not registered")


def test_registered_path_replaced_by_another_repository_is_refused(repo):
    """C§16 #36: same path, different repository."""
    shutil.rmtree(repo.linked)
    repo.linked.mkdir()
    git(repo.linked, "init", "-q", "-b", "main")
    shutil.copytree(repo.checkout / "scripts", repo.linked / "scripts", symlinks=True)

    assert_refused(
        repo.run("check", cwd=repo.linked), "belongs to another Git repository", "invalid Git metadata"
    )


@pytest.mark.parametrize("damage", ["garbled", "missing", "symlink"])
def test_damaged_dot_git_in_a_registered_worktree_is_refused(repo, damage):
    """C§16 #36: `.git` garbled, missing or a symlink."""
    dot_git = repo.linked / ".git"
    dot_git.unlink()
    if damage == "garbled":
        dot_git.write_text("gitdir: /nonexistent/elsewhere\n")
    elif damage == "symlink":
        dot_git.symlink_to(repo.checkout / ".git")

    assert_refused(
        repo.run("check", cwd=repo.linked),
        "invalid Git metadata",
        "no .git metadata",
        "symlinked .git",
        "cannot verify",
    )


def test_symlinked_checker_is_refused(repo):
    """C§16 #36: `scripts/check-harness` must be a regular file."""
    checker_path = repo.linked / "scripts/check-harness"
    checker_path.unlink()
    checker_path.symlink_to(repo.checkout / "scripts/check-harness")

    assert_refused(repo.run("check", cwd=repo.linked), "no regular executable scripts/check-harness")


def test_checker_resolving_outside_its_checkout_is_refused(repo):
    """C§16 #36: a `scripts` directory that is a link out of the checkout."""
    real = repo.tmp / "real-scripts"
    shutil.move(str(repo.linked / "scripts"), real)
    (repo.linked / "scripts").symlink_to(real)

    assert_refused(repo.run("check", cwd=repo.linked), "resolves outside checkout")


def test_deleted_working_directory_is_refused(repo):
    """C§16 #36: the caller's cwd no longer exists."""
    script = 'mkdir doomed && cd doomed && rmdir ../doomed && exec "$0" "$@"'
    done = subprocess.run(
        ["/bin/sh", "-c", script, str(repo.command), "check"],
        cwd=repo.tmp,
        env=repo.env(AGENT_FABRIC_PRODUCT_ROOT=str(repo.checkout)),
        capture_output=True,
    )
    result = type("R", (), {"returncode": done.returncode, "stdout": done.stdout,
                            "err": done.stderr.decode(errors="replace")})
    assert_refused(result, "cannot resolve the working directory")


@pytest.mark.parametrize("role", ROLES)
def test_paths_containing_a_newline_work(tmp_path, role):
    """C§16 #37: primary and linked worktree with newlines in their paths."""
    base = tmp_path / "new\nline"
    base.mkdir()
    door = FrontDoor(base, role)
    init_primary(door.checkout)
    linked = add_linked(door.checkout, base / "linked\nworktree", "feature")

    in_primary = door.run("check")
    in_linked = door.run("check", cwd=linked)

    assert checker(in_primary) == str(door.checkout / "scripts/check-harness")
    assert checker(in_linked) == str(linked / "scripts/check-harness")


def test_non_git_product_nested_in_an_unrelated_repository_stays_non_git(fd):
    """C§16 #37: the product is not a repo, so its own checker runs."""
    outer = fd.tmp / "outer"
    outer.mkdir()
    git(outer, "init", "-q", "-b", "main")
    door = FrontDoor(outer, fd.role)
    nested = door.checkout / "sub"
    nested.mkdir()

    result = door.run("check", cwd=nested)

    assert checker(result) == str(door.checkout / "scripts/check-harness"), result.err


def test_check_ignores_a_shadow_git_and_a_caller_git_dir(repo):
    """C§16 #38: git runs with the trusted PATH and a clean redirect set."""
    shadow = repo.tmp / "shadow"
    shadow.mkdir()
    record = repo.tmp / "shadow-called"
    (shadow / "git").write_text(f"#!/bin/sh\necho called >> {record}\nexit 1\n")
    (shadow / "git").chmod(0o755)
    path = f"{shadow}{os.pathsep}{os.environ['PATH']}"

    for cwd, root in ((repo.checkout, repo.checkout), (repo.linked, repo.linked)):
        result = repo.run("check", cwd=cwd, PATH=path, GIT_DIR=str(repo.tmp / "elsewhere/.git"))
        assert checker(result) == str(root / "scripts/check-harness"), result.err

    assert not record.exists(), "the shadow git was invoked"


def test_dispatcher_tree_beats_an_ambient_product_root(dispatcher):
    """C§16 #39 [D]: the tree the dispatcher lives in is the checker, not the ambient root."""
    ambient = build_checkout(dispatcher.tmp, "dispatcher", "ambient")
    init_primary(ambient)

    result = dispatcher.run("check", product=ambient)

    assert checker(result) == str(dispatcher.owner("scripts/check-harness")), result.err
