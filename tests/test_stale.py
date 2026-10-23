import os
from pathlib import Path
import shutil
import subprocess
import time

import pytest

IDENT = ["-c", "user.name=t", "-c", "user.email=t@example.com"]
from tests.support.front_door import front_door_binary

ROOT = Path(__file__).resolve().parents[1]


def git(repo: Path, *args: str) -> str:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    return subprocess.run(
        ["git", *IDENT, "-C", str(repo), *args], check=True, text=True, capture_output=True, env=env
    ).stdout


def commit(repo: Path, name: str) -> None:
    (repo / name).write_text(name)
    git(repo, "add", name)
    git(repo, "commit", "-qm", name)


@pytest.fixture(scope="session")
def front_door():
    return front_door_binary()


def install_product(front_door: Path, root: Path) -> None:
    wrapper = root / "runtime/fabric/bin/fabric-mcp"
    wrapper.parent.mkdir(parents=True)
    wrapper.write_text("#!/bin/sh\nexit 0\n")
    wrapper.chmod(0o755)
    scripts = root / "scripts"
    scripts.mkdir(exist_ok=True)
    binary = root / "bin/provenant"
    binary.parent.mkdir()
    shutil.copy2(front_door, binary)
    binary.chmod(0o755)
    owner = scripts / "provenant-profile"
    owner.write_text("#!/bin/sh\nexit 0\n")
    owner.chmod(0o755)
    (scripts / "check-harness").write_text("#!/bin/sh\nexit 0\n")
    (scripts / "check-harness").chmod(0o755)
    shutil.copy2(ROOT / "scripts/check_target.py", scripts / "check_target.py")
    shutil.copy2(ROOT / "scripts/worktree.py", scripts / "worktree.py")
    lib = scripts / "lib"
    lib.mkdir(exist_ok=True)
    shutil.copy2(ROOT / "scripts/lib/product_root_resolver.py", lib / "product_root_resolver.py")


def invoke(root: Path, *, suppressed: bool = False) -> subprocess.CompletedProcess:
    env = {**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": str(root), "PROVENANT_TEST_BIN": str(root / "bin/provenant")}
    if suppressed:
        env["PROVENANT_STALE_CHECKED"] = "1"
    else:
        env.pop("PROVENANT_STALE_CHECKED", None)
    return subprocess.run([str(root / "bin/provenant"), "check"], cwd=root, env=env,
                          text=True, capture_output=True, check=False)


def make(tmp_path: Path, front_door: Path, branch: str = "main"):
    """Bare origin plus two clones; returns both clones at one shared commit."""
    origin = tmp_path / "origin.git"
    subprocess.run(["git", "init", "-q", "--bare", f"--initial-branch={branch}", str(origin)], check=True)
    seed = tmp_path / "seed"
    git(tmp_path, "clone", "-q", str(origin), str(seed))
    git(seed, "checkout", "-q", "-B", branch)
    commit(seed, "a")
    git(seed, "push", "-q", "origin", branch)
    clone, other = tmp_path / "clone", tmp_path / "other"
    git(tmp_path, "clone", "-q", str(origin), str(clone))
    git(tmp_path, "clone", "-q", str(origin), str(other))
    install_product(front_door, clone)
    install_product(front_door, other)
    return clone, other


def push_from(other: Path, name: str, branch: str = "main") -> None:
    commit(other, name)
    git(other, "push", "-q", "origin", branch)


def test_up_to_date_is_silent(tmp_path, front_door):
    clone, _ = make(tmp_path, front_door)
    assert invoke(clone).stderr == ""


def test_behind(tmp_path, front_door):
    clone, other = make(tmp_path, front_door)
    push_from(other, "b")
    git(clone, "fetch", "-q")
    result = invoke(clone)
    assert "1 commits behind origin/main" in result.stderr
    assert f"git -C {clone} pull --ff-only" in result.stderr


def test_ahead_only_is_silent(tmp_path, front_door):
    clone, _ = make(tmp_path, front_door)
    commit(clone, "local")
    assert invoke(clone).stderr == ""


def test_off_default_branch(tmp_path, front_door):
    clone, _ = make(tmp_path, front_door)
    git(clone, "switch", "-q", "-c", "feature")
    commit(clone, "local")
    assert "on feature, not main" in invoke(clone).stderr
    git(clone, "switch", "-q", "--detach")
    assert "detached HEAD" in invoke(clone).stderr


def test_diverged(tmp_path, front_door):
    clone, other = make(tmp_path, front_door)
    commit(clone, "local")
    push_from(other, "b")
    git(clone, "fetch", "-q")
    result = invoke(clone)
    assert "diverged" in result.stderr and "origin/main" in result.stderr and "rebase" in result.stderr


def test_non_main_default_via_origin_head(tmp_path, front_door):
    clone, other = make(tmp_path, front_door, branch="trunk")
    push_from(other, "b", branch="trunk")
    git(clone, "fetch", "-q")
    result = invoke(clone)
    assert "origin/trunk" in result.stderr and "origin/main" not in result.stderr


def test_linked_worktree_is_silent(tmp_path, front_door):
    clone, other = make(tmp_path, front_door)
    push_from(other, "b")
    git(clone, "fetch", "-q")
    linked = tmp_path / "linked"
    git(clone, "worktree", "add", "-q", "-b", "feature", str(linked))
    install_product(front_door, linked)
    assert invoke(linked).stderr == ""
    assert invoke(clone).stderr


def test_non_repo_and_missing_dir_are_silent(tmp_path, front_door):
    plain = tmp_path / "plain"
    plain.mkdir()
    install_product(front_door, plain)
    assert invoke(plain).stderr == ""
    result = subprocess.run([str(front_door), "check"], cwd=tmp_path,
                            env={**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": str(tmp_path / "missing")},
                            text=True, capture_output=True, check=False)
    assert result.returncode == 3 and "warning:" not in result.stderr


def test_stale_fetch_head_without_being_behind(tmp_path, front_door):
    clone, _ = make(tmp_path, front_door)
    git(clone, "fetch", "-q")
    fetch_head = clone / ".git/FETCH_HEAD"
    old = time.time() - 5 * 86400
    os.utime(fetch_head, (old, old))
    assert "last fetched 5d ago" in invoke(clone).stderr


def test_missing_fetch_head_is_silent(tmp_path, front_door):
    clone, _ = make(tmp_path, front_door)
    (clone / ".git/FETCH_HEAD").unlink(missing_ok=True)
    assert "last fetched" not in invoke(clone).stderr


def test_dedupe_flag_suppresses_output(tmp_path, front_door):
    clone, other = make(tmp_path, front_door)
    push_from(other, "b")
    git(clone, "fetch", "-q")
    result = invoke(clone, suppressed=True)
    assert "warning:" not in result.stderr


def test_behind_ignores_inherited_git_configuration(tmp_path, front_door, monkeypatch):
    clone, other = make(tmp_path, front_door)
    push_from(other, "b")
    git(clone, "fetch", "-q")
    monkeypatch.setenv("GIT_CONFIG_COUNT", "invalid")
    monkeypatch.setenv("GIT_CONFIG_PARAMETERS", "invalid")

    assert "1 commits behind origin/main" in invoke(clone).stderr


def test_install_checker_advisory_warns_and_suppresses_duplicates(tmp_path, monkeypatch, capsys):
    """The install checker also uses the advisory and suppresses duplicate warnings."""
    from lib import stale_checkout

    monkeypatch.delenv("PROVENANT_STALE_CHECKED", raising=False)
    git_dir = str(tmp_path / ".git")
    responses = iter([[git_dir, git_dir, str(tmp_path / "FETCH_HEAD")],
                      ["origin/trunk"], ["trunk"], ["0\t2"]])
    monkeypatch.setattr(stale_checkout, "_git", lambda *_: next(responses))

    lines = stale_checkout.warn_if_stale_checkout(tmp_path)

    assert lines == [f"warning: product checkout is 2 commits behind origin/trunk; run: git -C {tmp_path} pull --ff-only"]
    assert capsys.readouterr().err == lines[0] + "\n"
    assert stale_checkout.warn_if_stale_checkout(tmp_path) == []
    assert capsys.readouterr().err == ""
