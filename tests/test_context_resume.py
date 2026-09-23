"""The real owner and supervisor replay recorded provider streams: context, ceiling, resume."""

import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/dispatch_run.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
FIX = ROOT / "tests/fixtures/fabric-context"
BINARY = {"claude": "claude", "cursor": "cursor-agent"}
REPLAY = """#!/usr/bin/env python3
import json, os, sys
argv = sys.argv[1:]
with open(os.environ["REPLAY_LOG"], "a") as log:
    log.write(json.dumps(argv) + "\\n")
prompt = sys.stdin.read() if os.environ["REPLAY_STDIN"] == "1" else argv[-1]
turn = os.environ["REPLAY_TURN1"]
if "--resume" in argv:
    session = argv[argv.index("--resume") + 1]
    if session != os.environ["REPLAY_SESSION"]:
        print("No conversation found with session ID " + session, file=sys.stderr)
        sys.exit(1)
    turn = os.environ["REPLAY_TURN2"]
sys.stdout.write(open(turn).read())
"""


def replay_owner(tmp_path, monkeypatch, adapter, turn1, turn2, session, model):
    bindir = tmp_path / "provider-bin"
    bindir.mkdir()
    binary = bindir / BINARY[adapter]
    binary.write_text(REPLAY)
    binary.chmod(0o755)
    log = tmp_path / "argv.jsonl"
    for key, value in {
        "PATH": str(bindir) + os.pathsep + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
        "FABRIC_COOLDOWNS_PATH": str(tmp_path / "cooldowns.json"),
        "REPLAY_LOG": str(log), "REPLAY_TURN1": str(turn1), "REPLAY_TURN2": str(turn2),
        "REPLAY_SESSION": session, "REPLAY_STDIN": "1" if adapter == "claude" else "0",
        "CLAUDE_CONFIG_DIR": str(tmp_path / "claude-config"), "CODEX_HOME": str(tmp_path / "codex-home"),
    }.items():
        monkeypatch.setenv(key, value)
    monkeypatch.delenv("PROVENANT_RUN_TOKEN", raising=False)
    run = Path(subprocess.check_output([str(INIT), "--kind", "dispatch"], cwd=tmp_path, text=True).strip())
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello")
    command = [sys.executable, str(SCRIPT), "--run-dir", str(run), "--adapter", adapter,
               "--model", model, "--prompt-file", str(prompt), "--fallback", "false"]
    return run, prompt, command, log


def attempt(run, number, task="dispatch-001"):
    return json.loads((run / f"tasks/{task}/attempt-{number:03d}/attempt.json").read_text())


def argv_calls(log):
    return [json.loads(line) for line in log.read_text().splitlines()]


def follows(argv, flag, value):
    return any(argv[i:i + 2] == [flag, value] for i in range(len(argv)))


def resume(run, row, prompt, *extra):
    return subprocess.run([sys.executable, str(SCRIPT), "--run-dir", str(run), "--resume", row["run_id"],
                           "--prompt-file", str(prompt), *extra], cwd=run.parents[2], capture_output=True, text=True)


def test_attempt_records_observed_context_and_enforced_ceiling(tmp_path, monkeypatch):
    run, _prompt, command, log = replay_owner(tmp_path, monkeypatch, "claude", FIX / "claude.jsonl",
                                              FIX / "claude.jsonl", "session-0003", "haiku")
    first = subprocess.run([*command, "--context-ceiling", "250000"], cwd=tmp_path, capture_output=True, text=True)
    assert first.returncode == 0, first.stdout + first.stderr
    row = attempt(run, 1)
    assert row["context"] == {"context_tokens": 8039, "input_tokens": 8035, "output_tokens": 4,
                              "cached_input_tokens": 4587, "context_window_tokens": 1000000,
                              "context_percent": None, "source": "observed"}
    assert row["applied"]["context_ceiling"] == "enforced"
    assert row["applied"]["context_ceiling_tokens"] == 250000
    assert follows(argv_calls(log)[0], "--autocompact", "250000")
    assert row["digest"].splitlines()[1].endswith(" · ctx 8k/1M")


def test_resume_of_a_large_session_warns_with_handoff_and_keeps_the_ceiling(tmp_path, monkeypatch):
    run, prompt, command, log = replay_owner(tmp_path, monkeypatch, "claude", FIX / "claude.jsonl",
                                             FIX / "claude.jsonl", "session-0003", "haiku")
    assert subprocess.run([*command, "--context-ceiling", "250000"], cwd=tmp_path, capture_output=True).returncode == 0
    path = run / "tasks/dispatch-001/attempt-001/attempt.json"
    row = json.loads(path.read_text())
    row["context"]["context_tokens"] = 620000
    path.write_text(json.dumps(row))
    resumed = resume(run, row, prompt)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    second = attempt(run, 2)
    advice = f'resuming a ~620k-token session; fresh: fabric_dispatch{{prompt, handoff:"{row["run_id"]}"}}'
    assert second["warnings"][0] == advice
    assert "\n  ! " + advice in second["digest"]
    argv = argv_calls(log)[1]
    assert follows(argv, "--resume", "session-0003")
    assert follows(argv, "--autocompact", "250000")


def test_resume_warning_uses_the_provider_compaction_point_below_the_requested_ceiling(tmp_path, monkeypatch):
    run, prompt, command, log = replay_owner(tmp_path, monkeypatch, "claude", FIX / "claude.jsonl",
                                             FIX / "claude.jsonl", "session-0003", "claude-opus-5-5")
    (tmp_path / "claude-config").mkdir()
    (tmp_path / "claude-config/settings.json").write_text(json.dumps({"autoCompactWindow": 200000}))
    assert subprocess.run(command, cwd=tmp_path, capture_output=True).returncode == 0
    path = run / "tasks/dispatch-001/attempt-001/attempt.json"
    row = json.loads(path.read_text())
    assert (row["applied"]["context_ceiling"], row["applied"]["context_ceiling_tokens"]) == ("provider_default", 200000)
    row["context"]["context_tokens"] = 250000
    path.write_text(json.dumps(row))
    resumed = resume(run, row, prompt)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    assert attempt(run, 2)["warnings"][0].startswith("resuming a ~250k-token session; fresh:")
    assert all("--autocompact" not in argv for argv in argv_calls(log))


def test_resume_targets_one_task_of_a_multi_task_run(tmp_path, monkeypatch):
    run, prompt, command, log = replay_owner(tmp_path, monkeypatch, "claude", FIX / "claude.jsonl",
                                             FIX / "claude.jsonl", "session-0003", "haiku")
    for task in ("one", "two"):
        receipt = json.loads((run / "RUN_RECEIPT.json").read_text())
        (run / "RUN_RECEIPT.json").write_text(json.dumps({**receipt, "status": "active", "closed_at": None}))
        done = subprocess.run([*command, "--task-id", task], cwd=tmp_path, capture_output=True, text=True)
        assert done.returncode == 0, done.stdout + done.stderr
    row = attempt(run, 1, "two")
    unnamed = resume(run, row, prompt)
    assert unnamed.returncode != 0
    assert "pass task_id" in unnamed.stdout
    resumed = resume(run, row, prompt, "--task-id", "two")
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    assert attempt(run, 2, "two")["status"] == "ok"
    assert not (run / "tasks/one/attempt-002").exists()


@pytest.mark.parametrize("adapter,model,session,window", [
    ("claude", "haiku", "session-claude-001", 1000000),
    ("cursor", "auto", "session-cursor-001", None),
])
def test_recorded_kestrel_resume_continues_the_provider_session(tmp_path, monkeypatch, adapter, model, session, window):
    """Live turns of 2026-09-23: turn 2 answers only when the saved session is resumed."""
    run, prompt, command, log = replay_owner(tmp_path, monkeypatch, adapter, FIX / f"kestrel-{adapter}-1.jsonl",
                                             FIX / f"kestrel-{adapter}-2.jsonl", session, model)
    first = subprocess.run(command, cwd=tmp_path, capture_output=True, text=True)
    assert first.returncode == 0, first.stdout + first.stderr
    row = attempt(run, 1)
    assert row["session_id"] == session
    assert (run / row["paths"]["result"]).read_text().strip() == "OK"
    resumed = resume(run, row, prompt)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    second = attempt(run, 2)
    assert follows(argv_calls(log)[1], "--resume", session)
    assert (run / second["paths"]["result"]).read_text().strip() == "KESTREL"
    assert second["context"]["context_tokens"] > 0
    assert second["context"]["context_window_tokens"] == window
    assert "resume: relaunched" not in second["warnings"]
