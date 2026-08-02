import os
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "provision-lane-worktree"


def _fixture_repo(tmp_path: Path, *, fail_install: bool = False) -> Path:
    repo = tmp_path / "repo"
    (repo / "scripts").mkdir(parents=True)
    for name in ("provision-lane-worktree", "worktree", "worktree.py"):
        source = ROOT / "scripts" / name
        target = repo / "scripts" / name
        shutil.copy2(source, target)
        target.chmod(0o755)
    (repo / "scripts" / "install-agent-fabric-dependencies").write_text(
        "#!/bin/sh\nset -eu\n"
        "root=$(CDPATH= cd -- \"$(dirname -- \"$0\")/..\" && pwd)\n"
        "echo install >> \"$AGENT_PROVISION_LOG\"\n"
        + ("exit 7\n" if fail_install else "mkdir -p \"$root/node_modules\"\n")
    )
    (repo / "scripts" / "agent-fabric-protocol-build").write_text(
        "#!/bin/sh\nset -eu\n"
        "root=${AGENTS_HOME:-$(CDPATH= cd -- \"$(dirname -- \"$0\")/..\" && pwd)}\n"
        "echo build >> \"$AGENT_PROVISION_LOG\"\n"
        "mkdir -p \"$root/runtime/agent-fabric-protocol/dist\"\n"
        "touch \"$root/runtime/agent-fabric-protocol/dist/index.js\"\n"
    )
    for path in (
        repo / "scripts" / "install-agent-fabric-dependencies",
        repo / "scripts" / "agent-fabric-protocol-build",
    ):
        path.chmod(0o755)
    (repo / "README").write_text("fixture\n")
    subprocess.run(["git", "init", "--quiet"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "fixture"], cwd=repo, check=True)
    subprocess.run(
        ["git", "config", "user.email", "fixture@example.invalid"], cwd=repo, check=True,
    )
    subprocess.run(["git", "add", "."], cwd=repo, check=True)
    subprocess.run(["git", "commit", "--quiet", "-m", "fixture"], cwd=repo, check=True)
    return repo


def test_provision_creates_then_installs_then_builds(tmp_path: Path) -> None:
    repo = _fixture_repo(tmp_path)
    log = tmp_path / "provision.log"
    result = subprocess.run(
        [str(SCRIPT), "lane", "--repo", str(repo), "--detach", "HEAD", "--human-authorised"],
        cwd=repo,
        env={**os.environ, "AGENT_PROVISION_LOG": str(log)},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    target = repo / ".worktrees" / "lane"
    assert result.returncode == 0, result.stderr
    assert "worktree ready=" in result.stdout
    assert log.read_text().splitlines() == ["install", "build"]
    assert (target / "node_modules").is_dir()
    assert (target / "runtime/agent-fabric-protocol/dist/index.js").is_file()


def test_provision_requires_explicit_authorisation(tmp_path: Path) -> None:
    repo = _fixture_repo(tmp_path)
    result = subprocess.run(
        [str(SCRIPT), "lane", "--repo", str(repo), "--detach", "HEAD"],
        cwd=repo,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 2
    assert "--human-authorised" in result.stderr
    assert not (repo / ".worktrees" / "lane").exists()


def test_provision_reports_the_failed_step_and_does_not_build(tmp_path: Path) -> None:
    repo = _fixture_repo(tmp_path, fail_install=True)
    log = tmp_path / "provision.log"
    result = subprocess.run(
        [str(SCRIPT), "lane", "--repo", str(repo), "--detach", "HEAD", "--human-authorised"],
        cwd=repo,
        env={**os.environ, "AGENT_PROVISION_LOG": str(log)},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode != 0
    assert "dependency installation" in result.stderr
    assert str(repo / ".worktrees" / "lane") in result.stderr
    assert log.read_text().splitlines() == ["install"]
