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


def tag(path: Path, session_id: str) -> Path:
    path.write_text(path.read_text(encoding="utf-8") + f"\nChair session: {session_id}\n", encoding="utf-8")
    return path


def hook_json(cwd: Path, session_id: str = "sess-1") -> str:
    return json.dumps({"cwd": str(cwd), "session_id": session_id})


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


def assert_report_contains(result, *parts: str) -> None:
    assert all(part in result.stdout for part in parts), result.stdout


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
    assert_report_contains(result, f"{len(state.read_bytes())} bytes exceeds 6144")


def test_missing_heading_reports_problem_and_fix(tmp_path):
    state = write_state(tmp_path / "STATE.md", missing="Queue")

    result = run_check(tmp_path, str(state))

    assert result.returncode == 1
    assert_report_contains(result, "missing level-2 heading 'Queue'")


def test_too_many_next_actions_reports_problem_and_fix(tmp_path):
    state = write_state(tmp_path / "STATE.md", actions=4)

    result = run_check(tmp_path, str(state))

    assert result.returncode == 1
    assert_report_contains(result, "Next actions has 4 list items", "expected 1 to 3")


def test_stale_file_reports_problem_and_fix(tmp_path):
    state = write_state(tmp_path / "STATE.md")
    old = state.stat().st_mtime - 6 * 60
    os.utime(state, (old, old))

    result = run_check(tmp_path, "--max-age-minutes", "5", str(state))

    assert result.returncode == 1
    assert_report_contains(result, "file is older than 5 minutes")


def test_discovery_skips_sessions_untouched_for_a_day(tmp_path):
    tag(write_state(tmp_path / ".agent-run" / "sessions" / "live" / "STATE.md", missing="Links"), "sess-1")
    finished = tag(write_state(tmp_path / ".agent-run" / "sessions" / "finished" / "STATE.md", missing="Links"), "sess-1")
    old = finished.stat().st_mtime - 25 * 60 * 60
    os.utime(finished, (old, old))

    result = run_check(tmp_path, "--hook", input_text=hook_json(tmp_path))

    assert result.returncode == 0
    assert_report_contains(result, ".agent-run/sessions/live/STATE.md", "missing level-2 heading 'Links'")


def test_hook_checks_only_the_state_naming_its_session(tmp_path):
    tag(write_state(tmp_path / ".agent-run" / "sessions" / "mine" / "STATE.md", missing="Links"), "sess-1")
    tag(write_state(tmp_path / ".agent-run" / "sessions" / "theirs" / "STATE.md", missing="Links"), "sess-2")

    result = run_check(tmp_path, "--hook", input_text=hook_json(tmp_path, "sess-1"))

    assert result.returncode == 0
    assert_report_contains(result, ".agent-run/sessions/mine/STATE.md", "missing level-2 heading 'Links'")


def test_unidentified_session_is_silent(tmp_path):
    write_state(tmp_path / ".agent-run" / "sessions" / "chair" / "STATE.md", missing="Links")

    for result in (
        run_check(tmp_path),
        run_check(tmp_path, "--hook", input_text=json.dumps({"cwd": str(tmp_path)})),
        run_check(tmp_path, "--hook", input_text=hook_json(tmp_path, "unmatched")),
    ):
        assert result.returncode == 0
        assert result.stdout == ""


def test_session_id_must_match_the_chair_session_line_exactly(tmp_path):
    tag(write_state(tmp_path / ".agent-run" / "sessions" / "ten" / "STATE.md", missing="Links"), "sess-10")
    other = write_state(tmp_path / ".agent-run" / "sessions" / "notes" / "STATE.md", missing="Links")
    other.write_text(other.read_text(encoding="utf-8") + "\nSee sess-1 for context.\n", encoding="utf-8")

    result = run_check(tmp_path, "--hook", input_text=hook_json(tmp_path, "sess-1"))

    assert result.returncode == 0
    assert result.stdout == ""


def test_explicit_path_overrides_environment(tmp_path):
    named = write_state(tmp_path / "named.md", missing="Links")
    env_state = write_state(tmp_path / "env.md", missing="Queue")

    result = subprocess.run(
        [sys.executable, str(SCRIPT), str(named)], cwd=tmp_path, text=True, capture_output=True,
        check=False, env={**os.environ, "PROVENANT_SESSION_STATE": str(env_state)},
    )

    assert result.returncode == 1
    assert "missing level-2 heading 'Links'" in result.stdout
    assert "Queue" not in result.stdout


def test_environment_names_the_state_file(tmp_path):
    state = write_state(tmp_path / ".agent-run" / "sessions" / "chair" / "STATE.md", missing="Links")

    result = subprocess.run(
        [sys.executable, str(SCRIPT)], cwd=tmp_path, text=True, capture_output=True, check=False,
        env={**os.environ, "PROVENANT_SESSION_STATE": str(state)},
    )

    assert result.returncode == 1
    assert "missing level-2 heading 'Links'" in result.stdout


def test_hook_config_registers_this_script():
    result = run_check(ROOT, "--hook-config")

    config = json.loads(result.stdout)
    hook = config["hooks"]["PreCompact"][0]["hooks"][0]
    assert result.returncode == 0
    assert hook["type"] == "command"
    assert hook["command"] == f'python3 "{SCRIPT.resolve()}" --hook'
    assert hook["timeout"] == 5


def test_no_discovered_state_files_is_silent_success(tmp_path):
    result = run_check(tmp_path)

    assert result.returncode == 0
    assert result.stdout == ""


def test_hook_uses_stdin_cwd_and_never_fails_compaction(tmp_path):
    project = tmp_path / "hook-project"
    state = tag(write_state(project / ".agent-run" / "sessions" / "chair" / "STATE.md", missing="Links"), "sess-1")
    other = tmp_path / "other"
    other.mkdir()

    result = run_check(other, "--hook", input_text=hook_json(project))

    assert result.returncode == 0
    assert_report_contains(result, state.relative_to(project).as_posix(), "missing level-2 heading 'Links'")


def test_hook_ignores_malformed_stdin_and_uses_process_cwd(tmp_path):
    state = write_state(tmp_path / ".agent-run" / "sessions" / "chair" / "STATE.md", missing="Links")

    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--hook"], cwd=tmp_path, input="not JSON", text=True,
        capture_output=True, check=False, env={**os.environ, "PROVENANT_SESSION_STATE": str(state)},
    )

    assert result.returncode == 0
    assert_report_contains(result, state.relative_to(tmp_path).as_posix(), "missing level-2 heading 'Links'")


def test_nested_bullets_do_not_count_as_separate_next_actions(tmp_path):
    state = write_state(tmp_path / "STATE.md", actions=1)
    text = state.read_text(encoding="utf-8").replace(
        "- Action 0.\n", "- Action 0.\n  - step a\n  - step b\n  - step c\n"
    )
    state.write_text(text, encoding="utf-8")

    result = run_check(tmp_path, str(state))

    assert result.returncode == 0
    assert result.stdout == ""


def test_discovery_walks_up_from_a_nested_checkout(tmp_path):
    tag(write_state(tmp_path / ".agent-run" / "sessions" / "chair" / "STATE.md", missing="Links"), "sess-1")
    nested = tmp_path / "app" / ".worktrees" / "lane"
    nested.mkdir(parents=True)

    result = run_check(nested, "--hook", input_text=hook_json(nested))

    assert result.returncode == 0
    assert_report_contains(result, ".agent-run/sessions/chair/STATE.md", "missing level-2 heading 'Links'")


def test_hook_skips_a_dangling_state_link(tmp_path):
    sessions = tmp_path / ".agent-run" / "sessions" / "gone"
    sessions.mkdir(parents=True)
    (sessions / "STATE.md").symlink_to(tmp_path / "missing.md")

    result = run_check(tmp_path, "--hook", input_text=hook_json(tmp_path))

    assert result.returncode == 0
    assert result.stdout == ""


def test_install_hook_adds_one_entry_and_keeps_existing_settings(tmp_path):
    settings = tmp_path / "settings.json"
    settings.write_text(json.dumps({"model": "x", "hooks": {"SessionStart": [{"hooks": []}]}}), encoding="utf-8")

    first = run_check(tmp_path, "--install-hook", str(settings))
    second = run_check(tmp_path, "--install-hook", str(settings))

    data = json.loads(settings.read_text(encoding="utf-8"))
    assert first.returncode == 0 and second.returncode == 0
    assert "already installed" in second.stdout
    assert data["model"] == "x"
    assert data["hooks"]["SessionStart"] == [{"hooks": []}]
    assert len(data["hooks"]["PreCompact"]) == 1
    assert data["hooks"]["PreCompact"][0]["hooks"][0]["command"] == f'python3 "{SCRIPT.resolve()}" --hook'


def test_install_hook_refuses_unexpected_shapes_without_writing(tmp_path):
    for text in ("[]", '{"hooks": null}', '{"hooks": {"PreCompact": {}}}'):
        settings = tmp_path / "settings.json"
        settings.write_text(text, encoding="utf-8")

        result = run_check(tmp_path, "--install-hook", str(settings))

        assert result.returncode == 1
        assert settings.read_text(encoding="utf-8") == text


def test_install_hook_tolerates_null_inner_hooks_and_writes_through_symlink(tmp_path):
    real = tmp_path / "dotfiles" / "settings.json"
    real.parent.mkdir()
    real.write_text(json.dumps({"hooks": {"PreCompact": [{"hooks": None}]}}), encoding="utf-8")
    link = tmp_path / "settings.json"
    link.symlink_to(real)

    result = run_check(tmp_path, "--install-hook", str(link))

    assert result.returncode == 0
    assert link.is_symlink()
    assert len(json.loads(real.read_text(encoding="utf-8"))["hooks"]["PreCompact"]) == 2


def test_session_directory_named_by_session_id_is_checked(tmp_path):
    write_state(tmp_path / ".agent-run" / "sessions" / "sess-7" / "STATE.md", missing="Links")

    result = run_check(tmp_path, "--hook", input_text=hook_json(tmp_path, "sess-7"))

    assert_report_contains(result, ".agent-run/sessions/sess-7/STATE.md", "missing level-2 heading 'Links'")
