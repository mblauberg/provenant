"""Rust-only failure paths; require a binary built with fault-injection."""

import os
import signal
import subprocess

import pytest

from tests.support.proc import process_state, process_stopped

from .support import read_json, wait_for

pytestmark = pytest.mark.rust_only


def exit_record(owner):
    path = owner.evidence / "supervisor.exit.json"
    assert path.stat().st_size <= 16 * 1024
    record = read_json(path)
    assert record["schema"] == "provenant.lane.exit.v1"
    assert record["ended_at"]
    assert record["pid"] == read_json(owner.control / "provider.json")["pid"]
    return record


@pytest.mark.parametrize("site", ["capture", "progress"])
def test_enospc_bookkeeping_never_signals_healthy_provider(lane, site):
    owner = lane()
    owner.env["PROVENANT_LANE_FAULT"] = site
    owner.start()
    pid = owner.ready()
    previous = owner.attempt()["last_progress_at"]
    owner.acknowledged("activity")
    wait_for(lambda: owner.attempt().get("last_progress_at") != previous, "output consumed despite ENOSPC")
    assert owner.acknowledged("ping")["pid"] == pid
    assert process_state(pid) and not process_stopped(pid)
    assert not (owner.control / "signals.jsonl").exists()
    owner.send("ok")
    assert owner.finish(0)["status"] == "ok"
    record = exit_record(owner)
    assert record["forced"] is None
    assert record["exit"] == 0
    assert any(site in error and ("28" in error or "space" in error.lower() or "ENOSPC" in error)
               for error in record["io_errors"])


def test_enospc_exit_record_uses_reserved_space(lane):
    owner = lane()
    owner.env["PROVENANT_LANE_FAULT"] = "exit-once"
    owner.start()
    owner.ready()
    owner.send("ok")
    assert owner.finish(0)["status"] == "ok"
    record = exit_record(owner)
    assert record["exit"] == 0
    assert record["forced"] is None
    assert not (owner.evidence / "supervisor.reserve").exists()
    assert not list(owner.evidence.glob(".supervisor-*"))


def test_persistent_enospc_exit_record_is_a_visible_publication_failure(lane):
    owner = lane()
    owner.env["PROVENANT_LANE_FAULT"] = "exit"
    owner.start()
    owner.ready()
    owner.send("ok")
    row = owner.finish(1, supervisor_exit=False)
    assert row["status"] == "failed"
    assert not (owner.evidence / "supervisor.exit.json").exists()
    retained = "\n".join(path.read_text() for path in
                         (owner.control / "owner.log", owner.evidence / "adapter.json", owner.evidence / "stderr.log")
                         if path.is_file())
    assert "75" in retained
    assert "ENOSPC" in retained or "space" in retained.lower() or "os error 28" in retained
    assert not (owner.control / "signals.jsonl").exists()


def observer_pid(owner, argv):
    result = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,command="],
                            check=True, capture_output=True, text=True, timeout=5)
    rows = [line.split(None, 2) for line in result.stdout.splitlines()]
    descendants = {owner.process.pid}
    while True:
        found = {int(pid) for pid, parent, _command in rows if int(parent) in descendants}
        if found <= descendants:
            break
        descendants |= found
    # The command comes from the retained launch contract, not an imported
    # supervisor function or an assumed observer module name.
    matches = [int(pid) for pid, _parent, command in rows
               if int(pid) in descendants and all(str(arg) in command for arg in argv)]
    assert len(matches) <= 1, matches
    return matches[0] if matches else None


def test_observer_death_leaves_provider_alive_and_settles_from_capture(lane):
    owner = lane()
    owner.start()
    pid = owner.ready()
    launch = read_json(owner.evidence / "launch.json")
    argv = launch["observer"]["argv"]
    observer = wait_for(lambda: observer_pid(owner, argv), "observer process")
    os.kill(observer, signal.SIGKILL)
    wait_for(lambda: process_stopped(observer), "observer death")
    previous = owner.attempt()["last_progress_at"]
    owner.acknowledged("activity")
    wait_for(lambda: owner.attempt().get("last_progress_at") != previous, "capture after observer death")
    assert owner.acknowledged("ping")["pid"] == pid
    assert process_state(pid) and not process_stopped(pid)
    assert not (owner.control / "signals.jsonl").exists()
    owner.send("ok")
    row = owner.finish(0)
    assert row["status"] == "ok"
    assert (owner.run / row["paths"]["result"]).read_text().strip() == "DONE"
    record = exit_record(owner)
    assert record["observer_lost"] is True
    assert record["forced"] is None
    assert record["exit"] == 0
    assert not process_state(observer)
