import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from lib.stale_checkout import check_stale_checkout  # noqa: E402

IDENT = ["-c", "user.name=t", "-c", "user.email=t@example.com"]


def git(repo: Path, *args: str) -> str:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    return subprocess.run(
        ["git", *IDENT, "-C", str(repo), *args], check=True, text=True, capture_output=True, env=env
    ).stdout


def commit(repo: Path, name: str) -> None:
    (repo / name).write_text(name)
    git(repo, "add", name)
    git(repo, "commit", "-qm", name)


def make(tmp_path: Path, branch: str = "main"):
    """Bare origin plus two clones; returns (clone, other) both at one shared commit."""
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
    return clone, other


def push_from(other: Path, name: str, branch: str = "main") -> None:
    commit(other, name)
    git(other, "push", "-q", "origin", branch)


def test_up_to_date_is_silent(tmp_path):
    clone, _ = make(tmp_path)
    assert check_stale_checkout(clone) == []


def test_behind(tmp_path):
    clone, other = make(tmp_path)
    push_from(other, "b")
    git(clone, "fetch", "-q")
    [line] = check_stale_checkout(clone)
    assert "1 commits behind origin/main" in line
    assert f"git -C {clone} pull --ff-only" in line


def test_ahead_only_is_silent(tmp_path):
    clone, _ = make(tmp_path)
    commit(clone, "local")
    assert check_stale_checkout(clone) == []


def test_diverged(tmp_path):
    clone, other = make(tmp_path)
    commit(clone, "local")
    push_from(other, "b")
    git(clone, "fetch", "-q")
    [line] = check_stale_checkout(clone)
    assert "diverged" in line and "origin/main" in line and "rebase" in line


def test_non_main_default_via_origin_head(tmp_path):
    clone, other = make(tmp_path, branch="trunk")
    push_from(other, "b", branch="trunk")
    git(clone, "fetch", "-q")
    [line] = check_stale_checkout(clone)
    assert "origin/trunk" in line and "origin/main" not in line


def test_linked_worktree_is_silent(tmp_path):
    clone, other = make(tmp_path)
    push_from(other, "b")
    git(clone, "fetch", "-q")
    linked = tmp_path / "linked"
    git(clone, "worktree", "add", "-q", "-b", "feature", str(linked))
    assert check_stale_checkout(linked) == []
    assert check_stale_checkout(clone) != []


def test_non_repo_and_missing_dir_are_silent(tmp_path):
    plain = tmp_path / "plain"
    plain.mkdir()
    assert check_stale_checkout(plain) == []
    assert check_stale_checkout(tmp_path / "missing") == []


def test_stale_fetch_head_without_being_behind(tmp_path):
    clone, _ = make(tmp_path)
    git(clone, "fetch", "-q")
    fetch_head = clone / ".git" / "FETCH_HEAD"
    old = time.time() - 5 * 86400
    os.utime(fetch_head, (old, old))
    [line] = check_stale_checkout(clone)
    assert "last fetched 5d ago" in line and f"git -C {clone} fetch" in line


def test_missing_fetch_head_is_silent(tmp_path):
    clone, _ = make(tmp_path)
    (clone / ".git" / "FETCH_HEAD").unlink(missing_ok=True)
    assert check_stale_checkout(clone) == []


def test_dedupe_flag_suppresses_output(tmp_path):
    clone, other = make(tmp_path)
    push_from(other, "b")
    git(clone, "fetch", "-q")
    code = f"import sys; sys.path.insert(0, {str(ROOT / 'scripts')!r}); from lib.stale_checkout import warn_if_stale_checkout as w; w(__import__('pathlib').Path({str(clone)!r}))"
    env = {k: v for k, v in os.environ.items() if k != "PROVENANT_STALE_CHECKED"}
    loud = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, env=env)
    assert "behind" in loud.stderr
    quiet = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True,
                           env={**env, "PROVENANT_STALE_CHECKED": "1"})
    assert quiet.stderr == ""
