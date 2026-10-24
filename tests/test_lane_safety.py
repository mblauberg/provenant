"""Legacy reap process tests, retained until the run owner replaces its caller."""

from __future__ import annotations

import os
import subprocess
import time
from pathlib import Path

import pytest

from tests.support.modules import load_script

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
reap = load_script(SCRIPTS / "reap.py", "reap_test", register=True)

# reap


@pytest.fixture
def sleeper():
    processes = []

    def start(*arguments):
        process = subprocess.Popen(arguments or ("sleep", "60"))
        processes.append(process)
        time.sleep(0.2)
        return process

    yield start
    for process in processes:
        process.kill()
        process.wait()


def test_reap_stops_explicit_pid_and_reports_its_state(sleeper, capsys):
    process = sleeper()
    status = reap.main([str(process.pid), "--grace", "3"])
    out = capsys.readouterr().out
    assert status == 0
    assert process.wait(timeout=5) == -15
    assert f"pid {process.pid} age " in out
    assert f"parent {os.getpid()}" in out
    assert "cmd sleep 60" in out
    assert f"stopped pid {process.pid}" in out


def test_reap_escalates_to_kill_after_grace(sleeper, capsys):
    process = sleeper("sh", "-c", "trap '' TERM; while :; do sleep 1; done")
    status = reap.main([str(process.pid), "--grace", "0.3"])
    assert status == 0
    assert process.wait(timeout=5) == -9
    assert "(KILL)" in capsys.readouterr().out


def test_reap_refuses_process_newer_than_started_before(sleeper, capsys):
    process = sleeper()
    status = reap.main([str(process.pid), "--started-before", str(time.time() - 3600)])
    assert status == 1
    assert process.poll() is None
    assert "started after --started-before" in capsys.readouterr().out


def test_reap_accepts_process_older_than_started_before(sleeper):
    process = sleeper()
    cutoff = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(time.time() + 5))
    assert reap.main([str(process.pid), "--started-before", cutoff, "--grace", "3"]) == 0
    process.wait(timeout=5)


def test_reap_tree_stops_descendants(sleeper):
    process = sleeper("sh", "-c", "sleep 60 & sleep 60 & wait")
    time.sleep(0.3)
    children = reap.descendants(process.pid)
    assert len(children) == 2
    assert reap.main([str(process.pid), "--tree", "--grace", "3"]) == 0
    process.wait(timeout=5)
    time.sleep(0.2)
    assert not any(reap.is_alive(child) for child in children)


def test_reap_without_tree_leaves_descendants(sleeper):
    process = sleeper("sh", "-c", "sleep 60 & wait")
    time.sleep(0.3)
    children = reap.descendants(process.pid)
    assert reap.main([str(process.pid), "--grace", "3"]) == 0
    process.wait(timeout=5)
    try:
        assert all(reap.is_alive(child) for child in children)
    finally:
        for child in children:
            os.kill(child, 9)


def test_reap_refuses_self_parent_and_init(capsys):
    for pid in (os.getpid(), os.getppid(), 1):
        assert reap.main([str(pid)]) == 1
    assert capsys.readouterr().out.count("refused pid") == 3


def test_reap_reports_missing_pid_as_gone(sleeper, capsys):
    process = sleeper("true")
    process.wait()
    assert reap.main([str(process.pid)]) == 0
    assert "already gone" in capsys.readouterr().out


@pytest.mark.parametrize("arguments", [
    ["-1"], ["12abc"], ["$(pgrep x)"], ["node"], ["1 2"],
    ["123", "--grace", "-1"], ["123", "--started-before", "yesterday"],
])
def test_reap_rejects_non_pid_arguments(arguments, capsys):
    with pytest.raises(SystemExit) as raised:
        status = reap.main(arguments)
        raise SystemExit(status)
    assert raised.value.code == 2


def test_reap_describes_age_compactly():
    assert reap.format_age(5) == "0m05s"
    assert reap.format_age(3725) == "1h02m"
    assert reap.format_age(90000) == "1d01h"


def test_reap_skips_kill_when_pid_identity_changed(sleeper, monkeypatch, capsys):
    process = sleeper("sh", "-c", "trap '' TERM; while :; do sleep 1; done")
    real = reap.process_info
    calls = {"n": 0}

    def shifted(pid):
        info = real(pid)
        calls["n"] += pid == process.pid
        if info is not None and pid == process.pid and calls["n"] > 1:
            info["started"] = float(info["started"]) + 100
        return info

    monkeypatch.setattr(reap, "process_info", shifted)
    status = reap.reap([process.pid], grace=0.3)
    assert status == 1
    assert process.poll() is None
    assert "KILL skipped" in capsys.readouterr().out
