import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "session" / "scripts" / "state_check.py"
TEMPLATE = ROOT / "skills" / "session" / "templates" / "STATE.template.md"


def run_check(cwd: Path, *args: str, input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        cwd=cwd,
        input=input_text,
        text=True,
        capture_output=True,
        check=False,
    )


def write_state(path: Path, *, actions: int = 1, missing: str = "") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    headings = [
        "Goal and authority",
        "Stage and blockers",
        "Active lanes",
        "Queue",
        "Last checkpoint",
        "Next actions",
        "Links",
    ]
    sections = []
    for heading in headings:
        if heading == missing:
            continue
        body = "- Continue the approved work.\n" if heading == "Next actions" else "Placeholder.\n"
        if heading == "Next actions":
            body = "".join(f"- Action {index}.\n" for index in range(actions))
        sections.append(f"## {heading}\n{body}")
    path.write_text("\n".join(sections), encoding="utf-8")
    return path


def test_template_is_accepted_and_clean_file_has_no_output(tmp_path):
    state = tmp_path / ".agent-run" / "sessions" / "chair" / "STATE.md"
    state.parent.mkdir(parents=True)
    state.write_text(TEMPLATE.read_text(encoding="utf-8"), encoding="utf-8")

    result = run_check(tmp_path, str(state))

    assert result.returncode == 0
    assert result.stdout == ""


def test_heading_prefixes_are_case_insensitive(tmp_path):
    state = write_state(tmp_path / "STATE.md")
    state.write_text(
        state.read_text(encoding="utf-8").replace(
            "## Goal and authority", "## gOaL aNd AuThOrItY details"
        ),
        encoding="utf-8",
    )

    result = run_check(tmp_path, str(state))

    assert result.returncode == 0
    assert result.stdout == ""


def test_oversize_file_reports_problem_and_fix(tmp_path):
    state = write_state(tmp_path / "STATE.md")
    state.write_bytes(state.read_bytes() + b"x" * 6144)

    result = run_check(tmp_path, str(state))

    assert result.returncode == 1
    assert result.stdout.splitlines() == [
        f"state_check: STATE.md: {len(state.read_bytes())} bytes exceeds 6144; trim the state file below 6 KiB"
    ]


def test_missing_heading_reports_problem_and_fix(tmp_path):
    state = write_state(tmp_path / "STATE.md", missing="Queue")

    result = run_check(tmp_path, str(state))

    assert result.returncode == 1
    assert result.stdout.splitlines() == [
        "state_check: STATE.md: missing level-2 heading 'Queue'; add the required heading"
    ]


def test_too_many_next_actions_reports_problem_and_fix(tmp_path):
    state = write_state(tmp_path / "STATE.md", actions=4)

    result = run_check(tmp_path, str(state))

    assert result.returncode == 1
    assert result.stdout.splitlines() == [
        "state_check: STATE.md: Next actions has 4 list items (expected 1 to 3); keep 1 to 3 next actions"
    ]


def test_stale_file_reports_problem_and_fix(tmp_path):
    state = write_state(tmp_path / "STATE.md")
    old = state.stat().st_mtime - 6 * 60
    os.utime(state, (old, old))

    result = run_check(tmp_path, "--max-age-minutes", "5", str(state))

    assert result.returncode == 1
    assert result.stdout.splitlines() == [
        "state_check: STATE.md: file is older than 5 minutes; refresh the checkpoint"
    ]


def test_discovery_skips_sessions_untouched_for_a_day(tmp_path):
    live = write_state(tmp_path / ".agent-run" / "sessions" / "live" / "STATE.md", missing="Links")
    finished = write_state(tmp_path / ".agent-run" / "sessions" / "finished" / "STATE.md", missing="Links")
    old = finished.stat().st_mtime - 25 * 60 * 60
    os.utime(finished, (old, old))

    result = run_check(tmp_path)

    assert result.returncode == 1
    assert result.stdout.splitlines() == [
        "state_check: .agent-run/sessions/live/STATE.md: missing level-2 heading 'Links'; add the required heading"
    ]
    assert live.exists()


def test_no_discovered_state_files_is_silent_success(tmp_path):
    result = run_check(tmp_path)

    assert result.returncode == 0
    assert result.stdout == ""


def test_hook_uses_stdin_cwd_and_never_fails_compaction(tmp_path):
    project = tmp_path / "hook-project"
    state = write_state(project / ".agent-run" / "sessions" / "chair" / "STATE.md", missing="Links")
    other = tmp_path / "other"
    other.mkdir()

    result = run_check(other, "--hook", input_text=json.dumps({"cwd": str(project)}))

    assert result.returncode == 0
    assert result.stdout.splitlines() == [
        f"Checkpoint check: state_check: {state.relative_to(project).as_posix()}: missing level-2 heading 'Links'; add the required heading"
    ]


def test_hook_ignores_malformed_stdin_and_uses_process_cwd(tmp_path):
    state = write_state(tmp_path / ".agent-run" / "sessions" / "chair" / "STATE.md", missing="Links")

    result = run_check(tmp_path, "--hook", input_text="not JSON")

    assert result.returncode == 0
    assert result.stdout.splitlines() == [
        f"Checkpoint check: state_check: {state.relative_to(tmp_path).as_posix()}: missing level-2 heading 'Links'; add the required heading"
    ]
