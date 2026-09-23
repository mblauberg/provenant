import re
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "skills" / "autopilot" / "templates" / "README.template.md"
BOOTSTRAP = ROOT / "skills" / "autopilot" / "scripts" / "bootstrap-autopilot.sh"
HOME_PATH = re.compile(r"/(?:Users|home)/[A-Za-z0-9._-]+/")


def assert_portable(text: str) -> None:
    assert HOME_PATH.search(text) is None


def inline_readme(script: str) -> str:
    start = script.index("gen_readme() {")
    end = script.index("install_file \"GOAL.md\"", start)
    return script[start:end]


def run_bootstrap(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["sh", str(BOOTSTRAP), *args],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )


def mission_path(root: Path, mission_id: str = "mission-id") -> Path:
    matches = list((root / ".agent-run" / "runs").glob(f"*-mission-{mission_id}-??????"))
    assert len(matches) == 1
    return matches[0]


def test_readme_template_and_fallback_are_mission_root_relative_and_portable():
    template_text = TEMPLATE.read_text()
    assert_portable(template_text)
    assert_portable(inline_readme(BOOTSTRAP.read_text()))
    assert "mission root" in template_text
    assert "references/operating-loop.md" in template_text
    assert "IN FULL first" in template_text


def test_bootstrap_generates_a_machine_portable_readme(tmp_path):
    result = run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Portability test")

    assert result.returncode == 3, result.stderr
    mission = mission_path(tmp_path)
    readme = (mission / "README.md").read_text()
    assert str(mission) not in readme
    assert_portable(readme)


def test_bootstrap_creates_a_resumable_incomplete_mission_without_self_wake_loop(tmp_path):
    result = run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")

    assert result.returncode == 3, result.stderr
    mission = mission_path(tmp_path)
    for relative in ("GOAL.md", "STATE.md", "QUEUE.md", "HANDOFF.md", "README.md"):
        assert (mission / relative).is_file(), relative

    assert "DOMAIN            = Example domain" in (mission / "GOAL.md").read_text()
    readme = " ".join((mission / "README.md").read_text().split())
    assert "validate_idle_pause.py" in readme
    assert "--queue" in readme
    assert "non-zero" in readme
    assert "self-wake forever" not in readme
    assert "never self-halt" not in readme
    assert "while STATUS != STOP" not in readme
    assert str(mission) not in readme
    queue = (mission / "QUEUE.md").read_text()
    assert "## Tier 0" in queue
    state = (mission / "STATE.md").read_text()
    assert "Conductor lease" in state
    assert "Resume protocol" in state


def test_bootstrap_dry_run_creates_nothing(tmp_path):
    result = run_bootstrap("--dry-run", "--repo-root", str(tmp_path), "mission-id", "Example domain")

    assert result.returncode == 0, result.stderr
    assert not (tmp_path / ".agent-run").exists()
    assert "/.agent-run/runs/" in result.stdout


def test_bootstrap_rerun_does_not_clobber_existing_state(tmp_path):
    run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")
    mission = mission_path(tmp_path)

    state = mission / "STATE.md"
    state.write_text("user-owned state\n")

    rerun = run_bootstrap("--repo-root", str(tmp_path), "mission-id")

    assert state.read_text() == "user-owned state\n"
    assert "exists, kept (not clobbered): " + str(mission / ".mission-id") not in rerun.stderr


def test_bootstrap_refuses_a_mission_id_that_escapes_agent_run(tmp_path):
    result = run_bootstrap("--repo-root", str(tmp_path), "../escape")

    assert result.returncode == 2
    assert "must not contain" in result.stderr


def test_new_mission_uses_canonical_runs_layout(tmp_path):
    result = run_bootstrap("--repo-root", str(tmp_path), "fresh-mission", "Example domain")
    assert result.returncode == 3, result.stderr
    matches = list((tmp_path / ".agent-run" / "runs").glob("*-mission-fresh-mission-??????"))
    assert len(matches) == 1
    assert (matches[0] / "GOAL.md").is_file()
    assert not (tmp_path / ".agent-run" / "fresh-mission").exists()


def test_linked_worktree_bootstrap_anchors_at_primary_checkout(tmp_path):
    primary = tmp_path / "primary"
    primary.mkdir()
    subprocess.run(["git", "init", "-q", str(primary)], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.name", "Test"], check=True)
    (primary / "README.md").write_text("test\n")
    subprocess.run(["git", "-C", str(primary), "add", "README.md"], check=True)
    subprocess.run(["git", "-C", str(primary), "commit", "-qm", "init"], check=True)
    linked = primary / ".worktrees" / "lane-bootstrap"
    linked.parent.mkdir()
    subprocess.run(["git", "-C", str(primary), "worktree", "add", "-q", "--detach", str(linked)], check=True)
    result = run_bootstrap("--repo-root", str(linked), "linked-mission", "Example domain")
    assert result.returncode == 3, result.stderr
    assert len(list((primary / ".agent-run" / "runs").glob("*-mission-linked-mission-??????"))) == 1
    assert not (linked / ".agent-run").exists()


def test_colliding_normalized_ids_remain_separate_missions(tmp_path):
    first = run_bootstrap("--repo-root", str(tmp_path), "A_B", "First domain")
    second = run_bootstrap("--repo-root", str(tmp_path), "a.b", "Second domain")
    assert first.returncode == second.returncode == 3
    matches = list((tmp_path / ".agent-run" / "runs").glob("*-mission-a-b-??????"))
    assert len(matches) == 2
    assert {path.joinpath(".mission-id").read_text().strip() for path in matches} == {"A_B", "a.b"}
