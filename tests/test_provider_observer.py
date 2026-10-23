"""Replay live semantics through the in-process observer and its wire interface."""

import importlib
import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "skills/orchestrate/scripts"
FIXTURES = ROOT / "tests/fixtures/fabric-context"
from skills.orchestrate.scripts import provider_exec as supervisor
provider_observer = supervisor.provider_observer


def observe(tmp_path, adapter, lines):
    path = tmp_path / "observer.json"
    run = subprocess.run(
        [sys.executable, str(SCRIPTS / "provider_exec.py"), "observe", "--adapter", adapter,
         "--model", "fixture", "--out", str(path)],
        input="".join(json.dumps({"s": stream, "line": line}) + "\n" for stream, line in lines),
        text=True, capture_output=True, timeout=10,
    )
    assert run.returncode == 0, run.stderr
    return [json.loads(line) for line in run.stdout.splitlines()], json.loads(path.read_text())


def test_observe_requests_terminal_and_persists_semantics(tmp_path):
    lines = [("out", json.dumps({"type": "result", "result": "DONE", "is_error": False,
                                 "session_id": "session"}))]
    requests, result = observe(tmp_path, "claude", lines)
    assert requests == [{"terminal": True}]
    assert result["session_id"] == "session"
    assert result["terminal_text"] == "DONE"
    assert result["terminal_event_seen"] is True


def synthetic_cases():
    def event(**value):
        return ("out", json.dumps(value))
    done = event(type="result", is_error=False, result="DONE", num_turns=1)
    return [
        ("copilot-result", "copilot", [done], 1, None),
        ("claude-preamble", "claude", [event(type="result", is_error=False, result="", num_turns=0), done], 1, None),
        ("claude-reopened", "claude", [done, event(type="user", message={"content": "next"}), done], 2, None),
        ("claude-retry", "claude", [event(type="api_retry", error={"message": "rate limit 429"}), done], 1, None),
        ("codex-retry", "codex", [event(type="error", message="Reconnecting... 1/5"),
          event(type="item.completed", item={"type": "agent_message", "text": "DONE"}),
          event(type="turn.completed")], 1, None),
        ("kiro-stdout-auth", "kiro", [("out", "Opening browser...")], 0, "auth_required"),
        ("kiro-stderr-auth", "kiro", [("err", "Opening browser…")], 0, "auth_required"),
        ("opencode-warning", "opencode", [("err", "WARN retrying: Individual quota reached"),
          ("err", "ERROR service=bus unrelated failure")], 0, None),
        ("opencode-fatal", "opencode", [("err", "Resets in 2 days."),
          ("err", "ERROR error=Monthly usage limit reached.")], 0, "usage_limited"),
        ("opencode-period", "opencode", [("err", "retrying: weekly usage limit reached")], 0, "usage_limited"),
        ("opencode-stdout-ignored", "opencode", [("out", "ERROR Individual quota reached")], 0, None),
    ]


CASES = synthetic_cases()
for adapter in ("claude", "codex", "cursor", "agy", "kiro", "opencode"):
    CASES.append(("recorded-" + adapter, adapter,
                  [("out", line) for line in (FIXTURES / (adapter + ".jsonl")).read_text().splitlines()],
                  None, None))


@pytest.mark.parametrize("name,adapter,lines,terminals,stop", CASES, ids=[case[0] for case in CASES])
def test_transcripts_replay_the_same_semantics(tmp_path, name, adapter, lines, terminals, stop):
    live = provider_observer.LiveObserver({"adapter": adapter, "model": "fixture"})
    for stream, line in lines:
        data = (line + "\n").encode()
        # Split chunks as real pipe reads may do.
        for chunk in (data[:len(data)//2], data[len(data)//2:]):
            if stream == "out":
                live.feed_stdout(chunk)
                live.feed_stderr(chunk, "stdout")
            else:
                live.feed_stderr(chunk, "stderr")
    requests, result = observe(tmp_path, adapter, lines)
    expected = live.result()
    # Relative reset hints are evaluated in separate processes a fraction of a second apart.
    from datetime import datetime
    def normalize(value):
        if isinstance(value, dict):
            return {key: ("relative-reset" if key == "reset_at" and item else normalize(item))
                    for key, item in value.items()}
        if isinstance(value, list):
            return [normalize(item) for item in value]
        return value
    if result.get("live_failure") and result["live_failure"].get("reset_at"):
        difference = (datetime.fromisoformat(result["live_failure"]["reset_at"]) -
                      datetime.fromisoformat(expected["live_failure"]["reset_at"])).total_seconds()
        assert abs(difference) < 5
    assert normalize(result) == normalize(expected)
    assert all(request.get("stop") != "stalled" for request in requests)
    if terminals is not None:
        assert requests.count({"terminal": True}) == terminals
    assert [request["stop"] for request in requests if "stop" in request] == ([stop] if stop else [])
    if "retry" in name:
        assert result["retry_failure"] is None


def fixture_plan(tmp_path, adapter, code):
    module = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    with pytest.MonkeyPatch.context() as patch:
        patch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")
        plan = module.build_plan(adapter, {"resolved_model": "auto", "model_family": "unknown"}, "hello",
                                 cwd=tmp_path, workspace_root=tmp_path, run_dir=tmp_path,
                                 startup_seconds=2, timeout_seconds=4)
    plan["argv"] = [sys.executable, "-u", "-c", code]
    plan["grace_seconds"] = 0.05
    return module, plan


@pytest.mark.parametrize("name,adapter,lines,terminals,stop", CASES, ids=[case[0] for case in CASES])
def test_live_attempt_matches_observer_decisions(tmp_path, name, adapter, lines, terminals, stop):
    # Fixtures' literal timestamps are historical transcript data; no deadline depends on them.
    requests, result = observe(tmp_path, adapter, lines)
    code = "import sys,time\n"
    for stream, line in lines:
        code += "print(" + repr(line) + ", file=sys." + ("stdout" if stream == "out" else "stderr") + ", flush=True)\n"
    if stop or result["terminal_event_seen"]:
        code += "import signal; signal.pause()\n"
    module, plan = fixture_plan(tmp_path, adapter, code)
    record = module.execute(plan, tmp_path / "result.md", env={**os.environ,
                                                               "CF_DISPATCH_ENABLE_COPILOT": "1"})
    if stop:
        assert record["status"] == stop
        assert record["evidence"]["termination_source"] == stop
    elif result["terminal_event_seen"]:
        assert record["evidence"]["termination_source"] == "terminal_grace"
    else:
        assert record["evidence"]["termination_source"] == "provider_exit"
    if result.get("session_id"):
        assert record["session_id"] == result["session_id"]


def test_both_import_styles_work_in_a_fresh_interpreter():
    run = subprocess.run([sys.executable, "-c", "import skills.orchestrate.scripts.provider_observer; import provider_observer"],
                         cwd=ROOT, capture_output=True, text=True, timeout=10)
    assert run.returncode == 0, run.stderr
