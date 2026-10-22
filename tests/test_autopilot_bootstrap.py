import subprocess
from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parents[1]
BOOTSTRAP = ROOT / "skills" / "autopilot" / "scripts" / "bootstrap-autopilot.py"


def run_bootstrap(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(BOOTSTRAP), *args],
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


def test_bootstrap_creates_a_resumable_incomplete_mission(tmp_path):
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    result = run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")

    assert result.returncode == 3, result.stderr
    mission = mission_path(tmp_path)
    for relative in ("GOAL.md", "STATE.md", "QUEUE.md", "HANDOFF.md", "README.md"):
        assert (mission / relative).is_file(), relative

    readme = " ".join((mission / "README.md").read_text().split())
    assert str(mission) not in readme
    queue = (mission / "QUEUE.md").read_text()
    assert queue.strip()
    assert (mission / "STATE.md").read_text().strip()


def test_bootstrap_dry_run_creates_nothing(tmp_path):
    result = run_bootstrap("--dry-run", "--repo-root", str(tmp_path), "mission-id", "Example domain")

    assert result.returncode == 0, result.stderr
    assert not (tmp_path / ".agent-run").exists()


def test_bootstrap_rerun_does_not_clobber_existing_state(tmp_path):
    run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")
    mission = mission_path(tmp_path)

    state = mission / "STATE.md"
    state.write_text("user-owned state\n")

    rerun = run_bootstrap("--repo-root", str(tmp_path), "mission-id")

    assert state.read_text() == "user-owned state\n"


def test_bootstrap_refuses_a_mission_id_that_escapes_agent_run(tmp_path):
    result = run_bootstrap("--repo-root", str(tmp_path), "../escape")

    assert result.returncode == 2


def test_new_mission_uses_canonical_runs_layout(tmp_path):
    result = run_bootstrap("--repo-root", str(tmp_path), "fresh-mission", "Example domain")
    assert result.returncode == 3, result.stderr
    matches = list((tmp_path / ".agent-run" / "runs").glob("*-mission-fresh-mission-??????"))
    assert len(matches) == 1
    assert (matches[0] / "GOAL.md").is_file()
    assert not (tmp_path / ".agent-run" / "fresh-mission").exists()


def test_bootstrap_does_not_resume_a_root_level_legacy_mission(tmp_path):
    legacy = tmp_path / ".agent-run" / "retired-mission"
    legacy.mkdir(parents=True)
    (legacy / "GOAL.md").write_text("STATUS: STOP\n")
    result = run_bootstrap("--repo-root", str(tmp_path), "retired-mission", "Example domain")
    assert result.returncode == 3, result.stderr
    assert mission_path(tmp_path, "retired-mission") != legacy


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


def test_bootstrap_argument_errors_and_help_text(tmp_path):
    missing_value = run_bootstrap("--repo-root")
    assert (missing_value.returncode, missing_value.stderr) == (
        2,
        "bootstrap-autopilot.py: missing value for --repo-root\n",
    )

    invalid_slug = run_bootstrap("--repo-root", str(tmp_path), "bad slug")
    assert invalid_slug.returncode == 2
    assert invalid_slug.stderr == (
        "bootstrap-autopilot.py: mission id must be a path-safe slug "
        "(letters, digits, '.', '_', '-' only): bad slug\n"
    )

    help_result = run_bootstrap("--help")
    assert help_result.returncode == 0
    assert "USAGE:" in help_result.stdout
    assert "EXIT CODES:" in help_result.stdout


def test_bootstrap_force_overwrites_templates_and_quiet_suppresses_receipt(tmp_path):
    run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")
    mission = mission_path(tmp_path)
    (mission / "STATE.md").write_text("user-owned state\n")

    result = run_bootstrap("--force", "--quiet", "--repo-root", str(tmp_path), "mission-id")

    assert result.returncode == 3
    assert result.stdout == ""
    assert "STATUS: SCAFFOLDED" not in result.stdout
    assert (mission / "STATE.md").read_text() != "user-owned state\n"


def test_bootstrap_scaffold_file_modes_match_shell_creation(tmp_path):
    result = run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")
    assert result.returncode == 3
    mission = mission_path(tmp_path)
    for name in (".mission-id", "GOAL.md", "STATE.md", "QUEUE.md", "HANDOFF.md", "README.md"):
        assert (mission / name).stat().st_mode & 0o777 == 0o644


def test_bootstrap_reports_unsubstituted_placeholders_with_exit_four(tmp_path):
    run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")
    mission = mission_path(tmp_path)
    goal = mission / "GOAL.md"
    contents = goal.read_text()
    for placeholder, value in (
        ("{{DOMAIN}}", "Example domain"),
        ("{{MISSION}}", "Example mission"),
        ("{{LOCKED_CONSTRAINTS}}", "Stay in scope"),
        ("{{BUILD_CEILING}}", "No build"),
        ("{{ESCALATION_GATES}}", "Ask the user"),
        ("{{RUNAWAY_CAPS}}", "bounded"),
    ):
        contents = contents.replace(placeholder, value)
    goal.write_text(contents)
    (mission / "README.md").write_text("leftover {{TYPO}}\n")

    result = run_bootstrap("--repo-root", str(tmp_path), "mission-id")

    assert result.returncode == 4
    assert "UNSUBSTITUTED placeholders remain (1)" in result.stderr
    assert "{{TYPO}}   in   README.md" in result.stderr


def test_bootstrap_seeds_domain_metacharacters_literally(tmp_path):
    domain = r"Example & tools | scripts\paths"
    result = run_bootstrap("--repo-root", str(tmp_path), "mission-id", domain)

    assert result.returncode == 3
    mission = mission_path(tmp_path)
    assert f"DOMAIN            = {domain}" in (mission / "GOAL.md").read_text()
    assert f"# Autopilot mission: {domain} — start here" in (mission / "README.md").read_text()


def fill_required_knobs(mission: Path) -> None:
    goal = mission / "GOAL.md"
    contents = goal.read_text()
    for placeholder, value in (
        ("{{DOMAIN}}", "Example domain"),
        ("{{MISSION}}", "Example mission"),
        ("{{LOCKED_CONSTRAINTS}}", "Stay in scope"),
        ("{{BUILD_CEILING}}", "No build"),
        ("{{ESCALATION_GATES}}", "Ask the user"),
        ("{{RUNAWAY_CAPS}}", "bounded"),
    ):
        contents = contents.replace(placeholder, value)
    goal.write_text(contents)


def test_bootstrap_dry_run_previews_knob_substitution(tmp_path):
    run_bootstrap("--repo-root", str(tmp_path), "mission-id", "Example domain")
    mission = mission_path(tmp_path)
    fill_required_knobs(mission)
    before = (mission / "README.md").read_text()

    result = run_bootstrap("--dry-run", "--repo-root", str(tmp_path), "mission-id")

    output = result.stdout + result.stderr
    assert "[dry-run] would substitute knobs in README.md" in output
    assert "(no filled knobs to substitute yet)" not in output
    assert (mission / "README.md").read_text() == before


def test_bootstrap_dry_run_accepts_a_symlinked_repo_root(tmp_path):
    real = tmp_path / "real"
    real.mkdir()
    link = tmp_path / "link"
    link.symlink_to(real)

    result = run_bootstrap("--dry-run", "--repo-root", str(link), "mission-id", "Example domain")

    assert result.returncode == 0, result.stderr
    assert not (real / ".agent-run").exists()
