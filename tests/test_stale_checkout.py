import io
import os
from pathlib import Path
import subprocess
import sys
import time

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from lib.stale_checkout import (
    check_stale_checkout,
    warn_if_stale_checkout,
    resolve_product_checkout,
)


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    for key in (
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ):
        env.pop(key, None)
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        text=True,
        capture_output=True,
        env=env,
    )


def _init_repo_with_upstream(tmp_path: Path) -> tuple[Path, Path]:
    local = tmp_path / "product"
    _git(tmp_path, "init", "-b", "main", str(local))
    _git(local, "config", "user.name", "Test User")
    _git(local, "config", "user.email", "test@example.com")
    (local / "README.md").write_text("# Initial\n")
    _git(local, "add", "README.md")
    _git(local, "commit", "-m", "Initial commit")

    origin = tmp_path / "origin.git"
    _git(tmp_path, "clone", "--bare", str(local), str(origin))

    _git(local, "remote", "add", "origin", str(origin))
    _git(local, "fetch", "origin")
    _git(local, "branch", "--set-upstream-to=origin/main", "main")
    return origin, local


def test_silent_and_fast_when_up_to_date(tmp_path: Path) -> None:
    _, local = _init_repo_with_upstream(tmp_path)
    warning = check_stale_checkout(local)
    assert warning is None


def test_warns_when_not_on_main(tmp_path: Path) -> None:
    _, local = _init_repo_with_upstream(tmp_path)
    _git(local, "checkout", "-b", "feat/my-feature")

    warning = check_stale_checkout(local)
    assert warning == (
        f"warning: product checkout is on branch feat/my-feature (expected main); "
        f"run: git -C {local} checkout main"
    )


def test_warns_when_detached_head(tmp_path: Path) -> None:
    _, local = _init_repo_with_upstream(tmp_path)
    _git(local, "checkout", "--detach", "HEAD")

    warning = check_stale_checkout(local)
    assert warning == (
        f"warning: product checkout is not on main (detached HEAD); "
        f"run: git -C {local} checkout main"
    )


def test_warns_when_behind_upstream_recent_fetch(tmp_path: Path) -> None:
    origin, local = _init_repo_with_upstream(tmp_path)

    # Push a commit from another clone
    other = tmp_path / "other"
    _git(tmp_path, "clone", "-b", "main", str(origin), str(other))
    _git(other, "config", "user.name", "Other")
    _git(other, "config", "user.email", "other@example.com")
    (other / "file.txt").write_text("change\n")
    _git(other, "add", "file.txt")
    _git(other, "commit", "-m", "Upstream change")
    _git(other, "push", "origin", "main")

    # Fetch in local checkout (recent fetch)
    _git(local, "fetch", "origin")

    warning = check_stale_checkout(local)
    assert warning == (
        f"warning: product checkout is 1 commit behind origin/main; "
        f"run: git -C {local} pull --ff-only"
    )


def test_warns_when_behind_upstream_old_fetch(tmp_path: Path) -> None:
    origin, local = _init_repo_with_upstream(tmp_path)

    # Push 19 commits from another clone
    other = tmp_path / "other"
    _git(tmp_path, "clone", "-b", "main", str(origin), str(other))
    _git(other, "config", "user.name", "Other")
    _git(other, "config", "user.email", "other@example.com")
    for i in range(19):
        (other / f"file_{i}.txt").write_text(f"change {i}\n")
        _git(other, "add", f"file_{i}.txt")
        _git(other, "commit", "-m", f"Upstream change {i}")
    _git(other, "push", "origin", "main")

    # Fetch in local checkout
    _git(local, "fetch", "origin")

    # Set FETCH_HEAD mtime to 4 days ago
    fetch_head = local / ".git/FETCH_HEAD"
    assert fetch_head.is_file()
    now = time.time()
    old_time = now - (4 * 86400)
    os.utime(fetch_head, (old_time, old_time))

    warning = check_stale_checkout(local, now=now)
    assert warning == (
        f"warning: product checkout is 19 commits behind origin/main (last fetched 4d ago); "
        f"run: git -C {local} pull --ff-only"
    )


def test_silent_when_no_upstream(tmp_path: Path) -> None:
    repo = tmp_path / "no-upstream"
    _git(tmp_path, "init", "-b", "main", str(repo))
    _git(repo, "config", "user.name", "Test")
    _git(repo, "config", "user.email", "test@example.com")
    (repo / "README.md").write_text("# Test\n")
    _git(repo, "add", "README.md")
    _git(repo, "commit", "-m", "First")

    warning = check_stale_checkout(repo)
    assert warning is None


def test_silent_when_not_git_repo(tmp_path: Path) -> None:
    not_git = tmp_path / "plain-directory"
    not_git.mkdir()
    assert check_stale_checkout(not_git) is None
    assert check_stale_checkout(tmp_path / "non-existent") is None


def test_warn_if_stale_checkout_stream_and_env(tmp_path: Path, monkeypatch) -> None:
    _, local = _init_repo_with_upstream(tmp_path)
    _git(local, "checkout", "-b", "dev")

    buf = io.StringIO()
    msg = warn_if_stale_checkout(local, stream=buf)
    assert msg is not None
    assert "warning: product checkout is on branch dev" in buf.getvalue()

    # When suppressed via env var in check-provenant-install / provenant
    monkeypatch.setenv("PROVENANT_STALE_CHECKED", "1")
    buf2 = io.StringIO()
    # warn_if_stale_checkout itself does not suppress unless the caller checks, but we can verify check_stale_checkout
    assert check_stale_checkout(local) == msg


def test_provenant_check_surfaces_warning_on_stale_product(tmp_path: Path, monkeypatch) -> None:
    origin, product = _init_repo_with_upstream(tmp_path)
    # create another commit upstream and fetch
    other = tmp_path / "other"
    _git(tmp_path, "clone", "-b", "main", str(origin), str(other))
    _git(other, "config", "user.name", "Other")
    _git(other, "config", "user.email", "other@example.com")
    (other / "change.txt").write_text("c\n")
    _git(other, "add", "change.txt")
    _git(other, "commit", "-m", "change")
    _git(other, "push", "origin", "main")
    _git(product, "fetch", "origin")

    # Set up dummy scripts/check-harness in product
    scripts_dir = product / "scripts"
    scripts_dir.mkdir(parents=True, exist_ok=True)
    import shutil
    shutil.copy2(ROOT / "scripts/provenant", scripts_dir / "provenant")
    (scripts_dir / "provenant").chmod(0o755)
    (scripts_dir / "lib").symlink_to(ROOT / "scripts/lib")
    (scripts_dir / "worktree.py").symlink_to(ROOT / "scripts/worktree.py")
    checker = scripts_dir / "check-harness"
    checker.write_text("#!/bin/sh\nprintf 'HARNESS_OK\\n'\n")
    checker.chmod(0o755)

    # Set up dummy runtime/fabric/bin/fabric-mcp
    (product / "runtime/fabric/bin").mkdir(parents=True, exist_ok=True)
    mcp = product / "runtime/fabric/bin/fabric-mcp"
    mcp.write_text("#!/bin/sh\nexit 0\n")
    mcp.chmod(0o755)

    # Run provenant check pointing at product
    result = subprocess.run(
        [sys.executable, str(scripts_dir / "provenant"), "check"],
        cwd=product,
        env={
            **os.environ,
            "AGENT_FABRIC_PRODUCT_ROOT": str(product),
            "PROVENANT_STALE_CHECKED": "",
        },
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
    assert result.stdout == "HARNESS_OK\n"
    assert f"warning: product checkout is 1 commit behind origin/main; run: git -C {product} pull --ff-only" in result.stderr


def test_check_provenant_install_surfaces_warning_on_stale_product(tmp_path: Path) -> None:
    origin, product = _init_repo_with_upstream(tmp_path)
    other = tmp_path / "other"
    _git(tmp_path, "clone", "-b", "main", str(origin), str(other))
    _git(other, "config", "user.name", "Other")
    _git(other, "config", "user.email", "other@example.com")
    (other / "change.txt").write_text("c\n")
    _git(other, "add", "change.txt")
    _git(other, "commit", "-m", "change")
    _git(other, "push", "origin", "main")
    _git(product, "fetch", "origin")

    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True, exist_ok=True)
    import json
    pointer.write_text(json.dumps({"schema_version": 1, "product_root": str(product)}) + "\n")

    check_install_script = ROOT / "scripts/check-provenant-install.py"
    result = subprocess.run(
        [sys.executable, str(check_install_script)],
        cwd=product,
        env={
            **os.environ,
            "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
            "PROVENANT_STALE_CHECKED": "",
        },
        capture_output=True,
        text=True,
    )
    assert f"warning: product checkout is 1 commit behind origin/main; run: git -C {product} pull --ff-only" in result.stderr

