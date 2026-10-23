"""Black-box dispatch driver; no supervisor imports or monkeypatches."""

import json
import os
import select
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from tests.support.dispatch import ROOT, real_owner_fixture
from tests.support.git import git
from tests.support.proc import process_group_pids, process_state, process_stopped

PROVIDER = Path(__file__).with_name("fake_provider.py").read_text()


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


def wait_for(probe, description, seconds=25):
    deadline = time.monotonic() + seconds
    while True:
        value = probe()
        if value:
            return value
        if time.monotonic() >= deadline:
            pytest.fail(f"deadline waiting for {description}")
        # Polling only observes a file/process predicate; elapsed time never
        # releases a provider or establishes scenario ordering.
        select.select([], [], [], .02)


class Lane:
    def __init__(self, root, monkeypatch, startup):
        root.mkdir()
        # A real fixture repository also stops discovery at the sandbox's
        # scratch boundary instead of inheriting the enclosing lane's Git dir.
        git(root, "init", "--quiet")
        self.root = root
        self.run, _, self.argv = real_owner_fixture(root, monkeypatch, PROVIDER)
        self.argv += ["--task-id", "seed", "--timeout-seconds", "45"]
        self.control = self.run / "fixture"
        self.control.mkdir()
        os.mkfifo(self.control / "commands")
        # A held writer prevents EOF between test commands. Writes are tiny
        # and nonblocking, so a failed launch cannot hang the test driver.
        self.fifo = os.open(self.control / "commands", os.O_RDWR | os.O_NONBLOCK)
        self.env = {**os.environ, "LANE_FIXTURE_DIR": str(self.control),
                    "PROVENANT_HOST_LOCAL_ONLY": "1", "PROVENANT_NO_OS_CONFINEMENT": "1",
                    "CF_DISPATCH_STARTUP_SECONDS": str(startup)}
        self.process = None
        self.log = None

    def start(self):
        self.log = (self.control / "owner.log").open("w")
        self.process = subprocess.Popen(self.argv, cwd=self.root, env=self.env,
                                        stdout=self.log, stderr=subprocess.STDOUT,
                                        start_new_session=True)

    def send(self, command):
        os.write(self.fifo, (command + "\n").encode())

    def ready(self):
        def provider_ready():
            if self.process is not None and self.process.poll() is not None:
                diagnostics = self.run / "dispatch/tasks/seed/attempt-001/stderr.log"
                pytest.fail((diagnostics.read_text() if diagnostics.exists() else "")
                            + (self.control / "owner.log").read_text()[-4000:])
            return read_json(self.control / "provider.json")

        provider = wait_for(provider_ready, "provider FIFO")
        wait_for(lambda: self.attempt().get("pgid"), "retained provider identity")
        return provider["pid"]

    def attempt(self):
        return read_json(self.run / "tasks/seed/attempt-001/attempt.json")

    def finish(self, exit_code):
        if self.process is not None:
            actual = self.process.wait(timeout=25)
            assert actual == exit_code, (self.control / "owner.log").read_text()
        else:
            owner = read_json(self.run / "dispatch-owner.json")
            wait_for(lambda: process_stopped(owner["owner_pid"]), "nested owner exit")
        row = self.attempt()
        assert row["state"] == "terminal"
        assert row["ended_at"]
        assert list((self.run / "tasks/seed").glob("attempt-*/attempt.json")) == [
            self.run / "tasks/seed/attempt-001/attempt.json"]
        assert list((self.run / "dispatch/tasks/seed").glob("attempt-*/attempt.json")) == [
            self.run / "dispatch/tasks/seed/attempt-001/attempt.json"]
        provider = read_json(self.control / "provider.json")
        assert not process_state(provider["pid"]), "provider was not reaped"
        wait_for(lambda: not process_group_pids(row["pgid"]), "provider process group exit")
        return row

    def cancel(self):
        script = ROOT / "skills/orchestrate/scripts/run_controls.py"
        subprocess.run([sys.executable, str(script), "cancel", "--run-dir", str(self.run),
                        "--task-id", "seed", "--attempt-id", "attempt-001", "--wait-seconds", "15"],
                       cwd=self.root, env=self.env, check=True, capture_output=True, text=True, timeout=20)

    def close(self):
        try:
            self.send("ok")
            if self.process is not None:
                if self.process.poll() is None:
                    self.process.terminate()
                self.process.wait(timeout=25)
            else:
                owner = read_json(self.run / "dispatch-owner.json")
                if owner and not process_stopped(owner["owner_pid"]):
                    self.cancel()
                    wait_for(lambda: process_stopped(owner["owner_pid"]), "nested cleanup")
        except BaseException:
            owner = read_json(self.run / "dispatch-owner.json")
            owner_pgid = owner.get("owner_pgid")
            if self.process is not None:
                owner_pgid = self.process.pid  # start_new_session=True
            for pgid in {owner_pgid, self.attempt().get("pgid")} - {None}:
                if pgid == os.getpgrp():
                    continue
                try:
                    os.killpg(pgid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if self.process is not None:
                self.process.wait(timeout=5)
            elif owner:
                wait_for(lambda: process_stopped(owner["owner_pid"]), "killed nested owner exit")
            raise
        finally:
            pgid = self.attempt().get("pgid")
            if pgid and pgid != os.getpgrp() and process_group_pids(pgid):
                try:
                    os.killpg(pgid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            os.close(self.fifo)
            if self.log:
                self.log.close()
