"""S0 invariants shared by the Python and future Rust lane supervisors."""

import json
import signal

import pytest

from .support import process_state, read_json, wait_for


@pytest.mark.parametrize("command,status,exit_code,provider_exit", [
    ("ok", "ok", 0, 0),
    ("fail", "failed", 1, 7),
])
def test_ordinary_dispatch_has_one_attempt_and_retains_exit(lane, command, status, exit_code, provider_exit):
    owner = lane()
    owner.start()
    owner.ready()
    owner.send(command)
    row = owner.finish(exit_code)
    assert row["status"] == status
    assert row["evidence"]["exit"] == provider_exit
    assert read_json(owner.run / "RUN_RECEIPT.json")["status"] == status


def test_startup_timeout(lane):
    owner = lane(startup=2)
    owner.start()
    owner.ready()
    # The provider remains silent and blocked on its FIFO until the watchdog.
    row = owner.finish(1)
    assert row["status"] == "startup_timeout"
    assert row["evidence"]["signature"] == "startup_watchdog"
    assert row["evidence"]["exit"] is not None
    assert read_json(owner.run / "RUN_RECEIPT.json")["status"] == "failed"


def test_sigterm_to_owner(lane):
    owner = lane()
    owner.start()
    owner.ready()
    owner.process.send_signal(signal.SIGTERM)
    row = owner.finish(1)
    assert row["status"] == "interrupted"
    assert row["evidence"]["signature"] == "supervisor_signal"
    assert row["evidence"]["exit"] is not None
    assert read_json(owner.run / "RUN_RECEIPT.json")["status"] == "failed"


def test_cancel_during_run(lane):
    owner = lane()
    owner.start()
    owner.ready()
    owner.cancel()
    row = owner.finish(1)
    assert row["status"] == "cancelled"
    assert row["evidence"]["signature"] == "cancel_requested"
    assert row["evidence"]["exit"] is not None
    assert read_json(owner.run / "RUN_RECEIPT.json")["status"] == "cancelled"


def test_nested_owner_spared(lane):
    parent = lane("parent")
    child = lane("child")
    (parent.control / "nested.json").write_text(json.dumps({
        "argv": child.argv, "cwd": str(child.root), "env": {
            "LANE_FIXTURE_DIR": str(child.control),
            "PROVENANT_HOST_LOCAL_ONLY": "1", "PROVENANT_NO_OS_CONFINEMENT": "1",
            "AGENT_FABRIC_PRODUCT_ROOT": parent.env["AGENT_FABRIC_PRODUCT_ROOT"],
        },
    }))
    parent.start()
    parent.ready()
    parent.send("nested")
    child_pid = child.ready()
    nested = read_json(child.run / "dispatch-owner.json")
    assert nested["owner_pid"] == nested["owner_pgid"]
    parent.send("terminal")
    row = parent.finish(0)
    assert row["status"] == "ok"
    assert row.get("spared", 0) >= 1
    assert not row.get("reaped")
    assert process_state(nested["owner_pid"])
    assert "Z" not in process_state(nested["owner_pid"])
    assert process_state(child_pid)
    assert child.attempt()["state"] == "running"
    child.send("ok")
    wait_for(lambda: child.attempt().get("state") == "terminal", "nested terminal record")
    assert child.finish(0)["status"] == "ok"
