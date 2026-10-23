"""Launch spec and trial front-door integration contracts."""

import importlib
import json
import os
from pathlib import Path
import signal
import sys

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "skills/orchestrate/scripts"
from skills.orchestrate.scripts import provider_exec as supervisor
lane_launch = supervisor.lane_launch


def test_launch_spec_carries_exact_execution_and_integer_durations(tmp_path):
    plan = {"adapter": "claude", "model": "m", "argv": ["claude", "-p"], "cwd": str(tmp_path),
            "run_dir": str(tmp_path), "mode": "worktree_write", "stdin_policy": "prompt",
            "timeout_seconds": 1.125, "startup_seconds": 0.025, "grace_seconds": 0.050,
            "silence_seconds": 0.125, "_lineage_token": "token", "batch_id": "batch-001",
            "attempt_identity": {"run_dir": str(tmp_path / "run")}}
    env = {"PATH": "/bin", "SECRET_REFERENCE": "private-value"}
    spec = lane_launch.build_spec(plan, env, argv=["/bin/claude", "-p"], stdin_path=tmp_path / "prompt.md",
                                  events_path=tmp_path / "events.jsonl", stderr_path=tmp_path / "stderr.log",
                                  max_bytes=2048)
    assert spec["schema"] == "provenant.lane.launch.v1"
    assert spec["argv"] == ["/bin/claude", "-p"]
    assert spec["cwd"] == str(tmp_path)
    assert spec["env"] == env
    assert spec["stdin"] == {"path": str(tmp_path / "prompt.md")}
    assert spec["capture"] == {"events": "events.jsonl", "stderr": "stderr.log", "max_bytes": 2048}
    assert spec["run_token"] == "token"
    assert spec["timeouts_ms"] == {"startup": 25, "wall": 1125, "settle": 50, "child_grace": 500, "root_flush": 2000}
    assert all(type(value) is int for value in spec["timeouts_ms"].values())
    assert spec["silence_observation_ms"] == 125
    assert spec["progress"]["writer_tree"]["root"] == str(tmp_path)
    assert str(tmp_path / "cancel.request") in spec["cancel_markers"]
    assert str(tmp_path / "run/dispatch/batches/batch-001/cancel.request") in spec["cancel_markers"]
    assert spec["observer"]["argv"][2:5] == ["observe", "--adapter", "claude"]
    assert spec["spare"] == {"code_mode_host": True}


@pytest.mark.parametrize("seconds", [float("nan"), float("inf"), -1, 0, True])
def test_invalid_durations_never_enter_the_wire(seconds):
    with pytest.raises(ValueError):
        lane_launch.milliseconds(seconds)


def trial_plan(tmp_path, monkeypatch, code, *, adapter="codex", mode="run"):
    import shlex
    module = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    monkeypatch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")
    bindir = tmp_path / "bin"
    bindir.mkdir()
    front = bindir / "provenant"
    front.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " " +
                     shlex.quote(str(Path(__file__).with_name("lane_stub.py"))) + ' "$@"\n')
    front.chmod(0o755)
    env = {**os.environ, "PROVENANT_BIN_DIR": str(bindir), "PROVENANT_LANE_IMPL": "rust", "LANE_STUB_MODE": mode}
    plan = module.build_plan(adapter, {"resolved_model": "auto", "model_family": "unknown"}, "hello",
                             cwd=tmp_path, workspace_root=tmp_path, run_dir=tmp_path,
                             startup_seconds=1, timeout_seconds=3)
    plan["argv"] = [sys.executable, "-u", "-c", code]
    plan["grace_seconds"] = 0.05
    return module, plan, env


DONE = "import json; print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'DONE'}})); print(json.dumps({'type':'turn.completed'}))"


def test_rust_trial_uses_wire_and_provider_started_callback(tmp_path, monkeypatch):
    module, plan, env = trial_plan(tmp_path, monkeypatch, DONE)
    started, progress = [], []
    record = module.execute(plan, tmp_path / "result.md", env=env, on_start=lambda process: started.append(process.pid),
                            on_progress=progress.append)
    assert len(progress) == 1
    assert isinstance(progress[0], str)
    launch = json.loads((tmp_path / "launch.json").read_text())
    process_exit = json.loads((tmp_path / "supervisor.exit.json").read_text())
    assert launch["schema"] == "provenant.lane.launch.v1"
    assert all(value == name for name, value in launch["env"].items())
    assert started == [process_exit["pid"]]
    assert record["pgid"] == process_exit["pgid"]
    assert record["status"] == "ok"
    assert record["evidence"]["exit"] == 0
    assert (tmp_path / "result.md").read_text() == "DONE"


def test_invalid_spec_warns_and_runs_the_python_supervisor_once(tmp_path, monkeypatch, caplog):
    marker = tmp_path / "runs"
    code = "from pathlib import Path; p=Path(" + repr(str(marker)) + "); p.write_text(p.read_text()+'x' if p.exists() else 'x'); " + DONE
    module, plan, env = trial_plan(tmp_path, monkeypatch, code, mode="unsupported")
    started = []
    record = module.execute(plan, tmp_path / "result.md", env=env, on_start=lambda process: started.append(process.pid))
    assert record["status"] == "ok"
    assert len(started) == 1
    assert marker.read_text() == "x"
    assert "exit 64" in caplog.text
    assert any("exit 64" in warning for warning in record["warnings"])
    assert not (tmp_path / "supervisor.exit.json").exists()


@pytest.mark.parametrize("adapter", ["claude", "cursor", "opencode"])
def test_reopening_adapters_keep_python_until_the_terminal_wire_is_frozen(tmp_path, monkeypatch, caplog, adapter):
    code = "import json; print(json.dumps({'type':'result','result':'DONE','is_error':False}))"
    module, plan, env = trial_plan(tmp_path, monkeypatch, code, adapter=adapter)
    record = module.execute(plan, tmp_path / "result.md", env=env)
    assert not (tmp_path / "launch.json").exists()
    assert "terminal retraction is not frozen" in caplog.text
    assert any("terminal retraction is not frozen" in warning for warning in record["warnings"])


@pytest.mark.parametrize("mode", ["observer-lost", "stdout-exit"])
def test_rust_settlement_survives_observer_or_exit_publication_loss(tmp_path, monkeypatch, mode):
    module, plan, env = trial_plan(tmp_path, monkeypatch, DONE, mode=mode)
    record = module.execute(plan, tmp_path / "result.md", env=env)
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"
    if mode == "stdout-exit":
        assert "exit publication unavailable" in record["warnings"]


@pytest.mark.parametrize("kind,code", [
    ("startup_timeout", "import signal; signal.pause()"),
    ("timed_out", "import signal; print('started',flush=True); signal.pause()"),
])
def test_rust_deadlines_map_to_existing_receipt_evidence(tmp_path, monkeypatch, kind, code):
    module, plan, env = trial_plan(tmp_path, monkeypatch, code)
    plan["startup_seconds"] = 0.1
    plan["timeout_seconds"] = 0.2
    record = module.execute(plan, tmp_path / "result.md", env=env)
    assert record["status"] == kind
    assert record["evidence"]["exit"] == -signal.SIGTERM
    assert record["evidence"]["signature"] == ("wall_clock" if kind == "timed_out" else "startup_watchdog")


def test_rust_terminal_grace_preserves_signal_exit_and_success(tmp_path, monkeypatch):
    module, plan, env = trial_plan(tmp_path, monkeypatch, DONE + "; import signal; signal.pause()")
    record = module.execute(plan, tmp_path / "result.md", env=env)
    assert record["status"] == "ok"
    assert record["evidence"]["exit"] == -signal.SIGTERM
    assert record["evidence"]["termination_source"] == "terminal_grace"


def test_rust_cancellation_during_run_signals_only_the_supervisor(tmp_path, monkeypatch):
    module, plan, env = trial_plan(tmp_path, monkeypatch, "import signal; signal.pause()")
    started = []
    record = module.execute(plan, tmp_path / "result.md", env=env,
                            on_start=lambda process: started.append(process.pid), cancelled=lambda: bool(started))
    assert len(started) == 1
    assert record["status"] == "cancelled"
    assert record["evidence"]["termination_source"] == "cancelled"


def test_rust_cancel_before_launch_never_starts_a_supervisor(tmp_path, monkeypatch):
    module, plan, env = trial_plan(tmp_path, monkeypatch, DONE)
    started = []
    record = module.execute(plan, tmp_path / "result.md", env=env, cancelled=lambda: True,
                            on_start=lambda process: started.append(process.pid))
    assert record["status"] == "cancelled"
    assert started == []
    assert not (tmp_path / "launch.json").exists()


def test_rust_capture_rollover_keeps_retained_semantics(tmp_path, monkeypatch):
    padding = "; ".join("print('x'*100)" for _ in range(50))
    module, plan, env = trial_plan(tmp_path, monkeypatch, DONE + "; " + padding)
    monkeypatch.setattr(module.provider_observer, "MAX_EVENTS_BYTES", 2048)
    record = module.execute(plan, tmp_path / "result.md", env=env)
    assert (tmp_path / "events.jsonl").stat().st_size <= 2048
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"


def test_owner_bookkeeping_io_error_does_not_stop_the_rust_provider(tmp_path, monkeypatch):
    marker = tmp_path / "finished"
    code = "from pathlib import Path; Path(" + repr(str(marker)) + ").write_text('yes'); " + DONE
    module, plan, env = trial_plan(tmp_path, monkeypatch, code)
    def failed_write(_child):
        raise OSError("bookkeeping full")
    record = module.execute(plan, tmp_path / "result.md", env=env, on_start=failed_write)
    assert marker.read_text() == "yes"
    assert record["status"] == "ok"
    assert record["evidence"]["exit"] == 0


def test_front_door_uses_the_installer_bin_directory(tmp_path):
    assert lane_launch.front_door({"PROVENANT_BIN_DIR": str(tmp_path)}) == str(tmp_path / "provenant")
    with pytest.raises(ValueError):
        lane_launch.front_door({"PROVENANT_BIN_DIR": "relative"})
