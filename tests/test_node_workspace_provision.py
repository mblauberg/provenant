import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
PROVISION = ROOT / "scripts" / "node-workspace-provision.py"


def linked_project(tmp_path):
    primary = tmp_path / "project"
    primary.mkdir()
    subprocess.run(["git", "init", "-q", str(primary)], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.name", "Test"], check=True)
    (primary / ".gitignore").write_text("node_modules/\n")
    (primary / "package.json").write_text(json.dumps({
        "private": True, "dependencies": {"fake-dep": "1.0.0"},
    }))
    (primary / "package-lock.json").write_text('{"lockfileVersion":3}\n')
    (primary / "scripts").mkdir()
    (primary / "scripts" / "node-workspace-preflight.mjs").write_bytes(
        (ROOT / "scripts" / "node-workspace-preflight.mjs").read_bytes()
    )
    for filename in ("node-workspace-provision.py", "worktree.py"):
        (primary / "scripts" / filename).write_bytes((ROOT / "scripts" / filename).read_bytes())
    manifest = json.loads((primary / "package.json").read_text())
    manifest["scripts"] = {
        "check": "python3 scripts/node-workspace-provision.py && node scripts/node-workspace-preflight.mjs && npm run typecheck && npm run test",
        "typecheck": "",
        "test": "",
    }
    (primary / "package.json").write_text(json.dumps(manifest))
    dependency = primary / "node_modules" / "fake-dep"
    dependency.mkdir(parents=True)
    (dependency / "package.json").write_text('{"name":"fake-dep"}\n')
    subprocess.run(["git", "-C", str(primary), "add", "."], check=True)
    subprocess.run(["git", "-C", str(primary), "commit", "-qm", "base"], check=True)
    worktree = primary / ".worktrees" / "linked"
    worktree.parent.mkdir()
    subprocess.run([
        "git", "-C", str(primary), "worktree", "add", "--detach", str(worktree), "HEAD",
    ], check=True, stdout=subprocess.DEVNULL)
    return primary, worktree


def run_provision(worktree, tmp_path, npm_body, **extra_environment):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    npm = bin_dir / "npm"
    npm.write_text("#!/usr/bin/env python3\n" + npm_body)
    npm.chmod(0o755)
    # Linux CI's filesystem has no reflink; emulate the copy-on-write clone so the
    # tests exercise provisioning rather than the host filesystem.
    cp = bin_dir / "cp"
    cp.write_text(
        "#!/usr/bin/env python3\n"
        "import os, shutil, sys\n"
        "source, target = sys.argv[-2:]\n"
        "if os.path.lexists(target):\n"
        "    sys.exit('cp: target exists')\n"
        "shutil.copytree(source, target, symlinks=True)\n"
    )
    cp.chmod(0o755)
    environment = os.environ.copy()
    environment["PATH"] = str(bin_dir) + os.pathsep + environment["PATH"]
    environment.update(extra_environment)
    return subprocess.run(
        [sys.executable, str(PROVISION)], cwd=worktree, env=environment,
        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )


def run_npm_check(worktree, tmp_path, npm_ci_body):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    real_npm = shutil.which("npm")
    npm = bin_dir / "npm"
    npm.write_text(
        "#!/usr/bin/env python3\n"
        "import subprocess, sys\n"
        "from pathlib import Path\n"
        f"real_npm = {real_npm!r}\n"
        "if sys.argv[1:3] == ['run', 'check']:\n"
        "    raise SystemExit(subprocess.run([real_npm, *sys.argv[1:]], check=False).returncode)\n"
        "if sys.argv[1:2] == ['ci']:\n"
        + "\n".join(f"    {line}" for line in npm_ci_body.splitlines()) + "\n"
        "raise SystemExit(0)\n"
    )
    npm.chmod(0o755)
    environment = os.environ.copy()
    environment["PATH"] = str(bin_dir) + os.pathsep + environment["PATH"]
    return subprocess.run(
        [str(npm), "run", "check"], cwd=worktree, env=environment,
        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )


def test_linked_worktree_lazily_clones_matching_lock_without_running_npm(tmp_path):
    _primary, worktree = linked_project(tmp_path)
    result = run_provision(worktree, tmp_path, "raise SystemExit(99)\n")

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "node-workspace-provision: cloned node_modules from primary checkout"
    assert (worktree / "node_modules" / "fake-dep" / "package.json").is_file()
    check = subprocess.run(
        ["node", str(worktree / "scripts" / "node-workspace-preflight.mjs")],
        cwd=worktree, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    assert check.returncode == 0, check.stderr


def test_linked_worktree_runs_offline_npm_ci_when_lock_differs(tmp_path):
    primary, worktree = linked_project(tmp_path)
    (worktree / "package-lock.json").write_text('{"lockfileVersion":3,"local":true}\n')
    marker = tmp_path / "npm-called"
    body = (
        "from pathlib import Path\n"
        f"Path({str(marker)!r}).write_text('called')\n"
        "package = Path('node_modules/fake-dep')\n"
        "package.mkdir(parents=True, exist_ok=True)\n"
        "(package / 'package.json').write_text('{\\\"name\\\":\\\"fake-dep\\\"}')\n"
    )

    result = run_npm_check(worktree, tmp_path, body)

    assert result.returncode == 0, result.stderr
    assert marker.read_text() == "called"
    assert "node-workspace-provision: ran npm ci in linked worktree" in result.stdout
    assert (primary / "node_modules" / "fake-dep" / "package.json").is_file()


def test_primary_checkout_keeps_preflight_failure_without_installing(tmp_path):
    primary, _worktree = linked_project(tmp_path)
    shutil.rmtree(primary / "node_modules")
    marker = tmp_path / "npm-called"
    result = run_npm_check(
        primary, tmp_path,
        f"from pathlib import Path\nPath({str(marker)!r}).write_text('called')\n",
    )

    assert result.returncode != 0
    assert "node-workspace-preflight: missing checkout dependencies" in result.stderr
    assert "node-workspace-provision:" not in result.stdout + result.stderr
    assert not marker.exists()


def test_borrowed_link_to_primary_dependencies_is_replaced_not_written_through(tmp_path):
    primary, worktree = linked_project(tmp_path)
    (worktree / "node_modules").symlink_to(primary / "node_modules")
    # The preflight rejects a borrowed tree even when the package resolves through it.
    before = sorted(path.name for path in (primary / "node_modules").iterdir())

    result = run_provision(worktree, tmp_path, "raise SystemExit(99)\n")

    assert result.returncode == 0, result.stderr
    assert not (worktree / "node_modules").is_symlink()
    assert (worktree / "node_modules" / "fake-dep" / "package.json").is_file()
    assert sorted(path.name for path in (primary / "node_modules").iterdir()) == before


def test_stale_worktree_dependencies_are_replaced_not_nested(tmp_path):
    primary, worktree = linked_project(tmp_path)
    stale = worktree / "node_modules" / "old-dep"
    stale.mkdir(parents=True)
    (stale / "package.json").write_text('{"name":"old-dep"}\n')

    result = run_provision(worktree, tmp_path, "raise SystemExit(99)\n")

    assert result.returncode == 0, result.stderr
    assert (worktree / "node_modules" / "fake-dep" / "package.json").is_file()
    assert not (worktree / "node_modules" / "node_modules").exists()
    assert not (worktree / "node_modules" / "old-dep").exists()
    assert not list(worktree.glob("node_modules.stale-*"))
    assert not list(worktree.glob(".agent-run/*.stale-*"))


def test_outside_a_git_checkout_provisioning_steps_aside(tmp_path):
    exported = tmp_path / "exported"
    exported.mkdir()
    result = run_provision(exported, tmp_path, "raise SystemExit(99)\n")

    assert result.returncode == 0
    assert "skipped" in result.stderr


def test_a_hook_git_dir_does_not_make_a_worktree_look_like_the_primary(tmp_path):
    primary, worktree = linked_project(tmp_path)
    result = run_provision(worktree, tmp_path, "raise SystemExit(99)\n", GIT_DIR=str(primary / ".git"), GIT_WORK_TREE=str(primary))

    assert result.returncode == 0, result.stderr
    assert (worktree / "node_modules" / "fake-dep" / "package.json").is_file()


def test_a_linked_agent_run_never_carries_a_stale_tree_out_of_the_worktree(tmp_path):
    primary, worktree = linked_project(tmp_path)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    (worktree / ".agent-run").symlink_to(elsewhere)
    stale = worktree / "node_modules" / "old-dep"
    stale.mkdir(parents=True)

    result = run_provision(worktree, tmp_path, "raise SystemExit(99)\n")

    assert result.returncode == 0, result.stderr
    assert (worktree / "node_modules" / "fake-dep" / "package.json").is_file()
    assert list(elsewhere.iterdir()) == []
