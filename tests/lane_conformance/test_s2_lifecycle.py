"""S2 lifecycle contract, observed through real dispatches and retained files."""

from datetime import datetime
import json
import os
import signal

import pytest

from tests.support.proc import process_state, process_stopped

from .support import read_json, wait_for


def timestamp(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


@pytest.mark.parametrize("progress", [False, True], ids=["unchanged-tree", "writer-progress"])
def test_silence_only_observes_and_never_signals(lane, progress):
    owner = lane(writer=True)
    owner.env["PROVENANT_KNOB_LANES_SILENCE_SECONDS"] = "1"
    owner.start()
    pid = owner.ready()
    owner.acknowledged("activity")  # A started turn, followed by silence.
    first = wait_for(lambda: read_json(owner.evidence / "watchdog.json"), "first silence observation")
    prior_progress = owner.attempt()["last_progress_at"]
    if progress:
        owner.acknowledged("write")
        wait_for(lambda: timestamp(owner.attempt()["last_progress_at"]) > timestamp(prior_progress),
                 "writer-tree progress")

    def next_observation():
        current = read_json(owner.evidence / "watchdog.json")
        return current if current.get("checked_at", "") > first["checked_at"] else None

    observation = wait_for(next_observation, "second silence observation")
    assert observation["process_alive"] is True
    assert observation["pid"] == pid
    assert observation["silent_seconds"] >= 1
    assert observation["last_output_at"]
    assert observation["worktree_scan_complete"] is True
    if progress:
        written = datetime.fromtimestamp((owner.cwd / "partial.txt").stat().st_mtime,
                                         tz=timestamp(observation["checked_at"]).tzinfo)
        # Observation timestamps have millisecond precision.
        assert timestamp(observation["last_worktree_write_at"]).timestamp() >= written.timestamp() - .001
    else:
        assert not (owner.cwd / "partial.txt").exists()
    assert process_state(pid) and not process_stopped(pid)
    assert owner.attempt()["state"] == "running"
    assert not (owner.control / "signals.jsonl").exists()
    assert owner.acknowledged("ping")["pid"] == pid
    owner.send("ok")
    assert owner.finish(0)["status"] == "ok"


@pytest.mark.parametrize("adapter", ["claude", "codex", "cursor", "agy", "kiro", "copilot", "opencode"])
def test_wall_deadline_for_every_adapter(lane, adapter):
    owner = lane(adapter=adapter, startup=30, wall=3)
    owner.start()
    owner.ready()
    # No provider command can terminate this run; only the wall deadline can.
    row = owner.finish(1)
    assert row["status"] == "timed_out"
    assert row["evidence"]["signature"] == "wall_clock"
    assert row["evidence"]["exit"] is not None
    assert read_json(owner.run / "RUN_RECEIPT.json")["status"] == "failed"


def test_checkpoint_write_failure_preserves_terminal_publication(lane):
    owner = lane()
    owner.start()
    owner.ready()
    blocked = owner.evidence / "wip-checkpoint.json"
    blocked.mkdir()
    (blocked / "keep").write_text("cannot replace a nonempty directory")
    owner.send("fail")
    row = owner.finish(1)
    assert row["status"] == "failed"
    assert row["evidence"]["exit"] == 7
    assert any("checkpoint unavailable" in warning for warning in row["warnings"])
    assert "checkpoint" not in row["paths"]
    assert (blocked / "keep").read_text() == "cannot replace a nonempty directory"
    assert read_json(owner.run / "RUN_RECEIPT.json")["status"] == "failed"


@pytest.mark.xfail(os.environ.get("PROVENANT_LANE_IMPL", "python") == "python", strict=True,
                   reason="Python prelaunch cancellation loses the cancel_requested evidence signature")
def test_cancel_before_launch_has_no_provider(lane):
    owner = lane(adapter="opencode")
    owner.env["LANE_FIXTURE_PREFLIGHT_GATE"] = "1"
    owner.start()
    # The real adapter's version preflight blocks on a separate test FIFO.
    # Attempt allocation has completed, but no provider can launch yet.
    wait_for(lambda: read_json(owner.control / "preflight.json"), "version preflight")
    assert owner.evidence.is_dir()
    (owner.evidence / "cancel.request").touch()
    os.write(owner.descendant_fifo, b"continue\n")
    row = owner.finish(1)
    assert row["status"] == "cancelled"
    assert not row.get("pgid")
    assert not (owner.control / "provider.json").exists()
    assert not (owner.evidence / "cancel.request").exists()
    assert read_json(owner.run / "RUN_RECEIPT.json")["status"] == "cancelled"
    assert row["evidence"]["signature"] == "cancel_requested"


@pytest.mark.xfail(os.environ.get("PROVENANT_LANE_IMPL", "python") == "python", strict=True,
                   reason="Python lets a cancel marker supersede a leader exit while capture EOF is pending")
def test_cancel_after_natural_exit_preserves_natural_result(lane):
    owner = lane()
    owner.start()
    owner.ready()
    supervisor = read_json(owner.control / "provider.json")["ppid"]
    os.kill(supervisor, signal.SIGSTOP)
    try:
        wait_for(lambda: "T" in process_state(supervisor), "supervisor stopped before provider exit")
        owner.send("ok")
        owner.wait_provider_exit()
        (owner.evidence / "cancel.request").touch()
    finally:
        os.kill(supervisor, signal.SIGCONT)
    row = owner.finish(0)
    assert row["status"] == "ok"
    assert row["evidence"]["exit"] == 0
    assert (owner.run / row["paths"]["result"]).read_text().strip() == "DONE"
    assert not (owner.evidence / "cancel.request").exists()


def test_cancel_escalates_when_provider_ignores_sigterm(lane):
    owner = lane()
    owner.start()
    owner.ready()
    owner.acknowledged("ignore-term")
    owner.cancel()
    row = owner.finish(1)
    signals = [json.loads(line)["signal"] for line in (owner.control / "signals.jsonl").read_text().splitlines()]
    assert signal.SIGTERM in signals
    assert row["status"] == "cancelled"
    assert row["evidence"]["signature"] == "cancel_requested"
    assert row["evidence"]["exit"] == -signal.SIGKILL


def test_new_session_grandchild_is_reaped(lane):
    owner = lane()
    owner.start()
    pid = owner.ready()
    owner.send("grandchild")
    grandchild = wait_for(lambda: read_json(owner.control / "grandchild.json"), "new-session grandchild")
    middle = wait_for(lambda: read_json(owner.control / "middle.json"), "intermediate child")
    assert middle["ppid"] == pid
    assert grandchild["ppid"] == middle["pid"]
    assert grandchild["pgid"] == grandchild["pid"]
    assert grandchild["pgid"] != owner.attempt()["pgid"]
    # The FIFO holds both ancestors alive until cancellation takes a census.
    owner.cancel()
    assert owner.finish(1)["status"] == "cancelled"
    wait_for(lambda: not process_state(grandchild["pid"]), "grandchild reaped")
    wait_for(lambda: not process_state(middle["pid"]), "intermediate child reaped")


@pytest.mark.xfail(os.environ.get("PROVENANT_LANE_IMPL", "python") == "python", strict=True,
                   reason="Python terminates Cursor at terminal grace before closing its stdin pipe")
def test_terminal_event_closes_open_stdin(lane):
    owner = lane(adapter="cursor")
    owner.start()
    owner.ready()
    owner.send("terminal-stdin")
    assert wait_for(lambda: read_json(owner.control / "stdin.json"), "stdin observation")["open"] is True
    row = owner.finish(0)
    assert row["status"] == "ok"
    assert read_json(owner.control / "stdin-eof.json")["eof"] is True
    assert row["evidence"]["exit"] == 0


def test_fast_exit_with_prequeued_fifo_command(lane):
    owner = lane()
    # Already readable on exec: readiness polling cannot slow down this exit.
    owner.send("ok")
    owner.start()
    row = owner.finish(0)
    assert read_json(owner.control / "provider.json")["pid"] == row["pgid"]
    assert row["status"] == "ok"
    assert row["evidence"]["exit"] == 0
