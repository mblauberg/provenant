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
import yaml

from tests.support.dispatch import ROOT, SCRIPT, disable_memory_admission_floor
from tests.support.fake_cli import write_executable
from tests.support.git import git
from tests.support.proc import process_group_pids, process_state, process_stopped
from tests.support.runs import INIT

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


def fixture_product(root, adapter):
    if adapter != "copilot":
        return ROOT
    # Copilot is deliberately dormant in the shipped compatibility policy.
    # Opt in only this fake CLI through a private product configuration;
    # every executable and supervisor module still comes from the real tree.
    product = root / "product"
    product.mkdir()
    for name in ("skills", "scripts", "runtime", "tools"):
        (product / name).symlink_to(ROOT / name, target_is_directory=True)
    config = product / "config"
    config.mkdir()
    for source in (ROOT / "config").iterdir():
        if source.name not in {"adapter-compatibility.yaml", "model-routing.json"}:
            (config / source.name).symlink_to(source, target_is_directory=source.is_dir())
    compatibility = yaml.safe_load((ROOT / "config/adapter-compatibility.yaml").read_text())
    compatibility["adapters"]["copilot"].update(enabled=True, disabled_reason="")
    (config / "adapter-compatibility.yaml").write_text(yaml.safe_dump(compatibility, sort_keys=False))
    catalog = read_json(ROOT / "config/model-routing.json")
    catalog["model_patterns"].insert(0, {"pattern": "^lane-fixture-copilot$", "family": "github"})
    (config / "model-routing.json").write_text(json.dumps(catalog))
    return product


class Lane:
    def __init__(self, root, monkeypatch, startup, *, impl="python", adapter="claude",
                 wall=45, writer=False):
        root.mkdir()
        # A real fixture repository also stops discovery at the sandbox's
        # scratch boundary instead of inheriting the enclosing lane's Git dir.
        git(root, "init", "--quiet")
        self.root = root
        # Keep entry-point selection here: tests.support.dispatch's S0 guard
        # rejects Rust, although S2 still uses this same Python owner.
        disable_memory_admission_floor(root)
        bindir = root / "provider-bin"
        bindir.mkdir()
        binary = {"cursor": "cursor-agent", "kiro": "kiro-cli"}.get(adapter, adapter)
        write_executable(bindir / binary, "#!/usr/bin/env python3\n" + PROVIDER)
        monkeypatch.setenv("PATH", str(bindir) + os.pathsep + os.environ["PATH"])
        monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(fixture_product(root, adapter)))
        monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(root / ".agents"))
        monkeypatch.setenv("FABRIC_COOLDOWNS_PATH", str(root / "cooldowns.json"))
        monkeypatch.delenv("PROVENANT_RUN_TOKEN", raising=False)
        self.run = Path(subprocess.check_output(
            [str(INIT), "--kind", "dispatch"], cwd=root, text=True).strip())
        prompt = root / "caller-prompt.md"
        prompt.write_text("hello")
        self.argv = [sys.executable, str(SCRIPT), "--run-dir", str(self.run),
                     "--adapter", adapter, "--model", "auto", "--prompt-file", str(prompt),
                     "--fallback", "false", "--task-id", "seed", "--timeout-seconds", str(wall)]
        self.cwd = root
        if writer:
            git(root, "commit", "--allow-empty", "--quiet", "-m", "fixture")
            self.cwd = root / "writer"
            git(root, "worktree", "add", "--quiet", "-b", "fixture-writer", str(self.cwd))
            self.argv += ["--access-mode", "worktree_write", "--worktree", str(self.cwd)]
        self.control = self.run / "fixture"
        self.control.mkdir()
        os.mkfifo(self.control / "commands")
        os.mkfifo(self.control / "descendant-commands")
        os.mkfifo(self.control / "provider-exit")
        # A held writer prevents EOF between test commands. Writes are tiny
        # and nonblocking, so a failed launch cannot hang the test driver.
        self.fifo = os.open(self.control / "commands", os.O_RDWR | os.O_NONBLOCK)
        self.descendant_fifo = os.open(self.control / "descendant-commands", os.O_RDWR | os.O_NONBLOCK)
        self.exit_fifo = os.open(self.control / "provider-exit", os.O_RDONLY | os.O_NONBLOCK)
        self.env = {**os.environ, "LANE_FIXTURE_DIR": str(self.control),
                    "HARNESS_PYTHON": sys.executable,
                    "LANE_FIXTURE_ADAPTER": adapter, "PROVENANT_LANE_IMPL": impl,
                    "PROVENANT_HOST_LOCAL_ONLY": "1", "PROVENANT_NO_OS_CONFINEMENT": "1",
                    "CF_DISPATCH_ENABLE_COPILOT": "1",
                    "CF_DISPATCH_ENABLE_KIRO": "1",
                    "CF_DISPATCH_CODEX_CONFIG": "clean",
                    "CF_DISPATCH_STARTUP_SECONDS": str(startup)}
        self.impl = impl
        if adapter == "copilot":
            self.argv[self.argv.index("--model") + 1] = "lane-fixture-copilot"
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

    @property
    def evidence(self):
        return self.run / "dispatch/tasks/seed/attempt-001"

    def acknowledged(self, command):
        self.send(command)
        return wait_for(lambda: read_json(self.control / (command + ".json")), command + " acknowledgement")

    def wait_provider_exit(self):
        seen_writer = False

        def closed_by_kernel():
            nonlocal seen_writer
            try:
                data = os.read(self.exit_fifo, 256)
            except BlockingIOError:
                return False
            if data:
                seen_writer = True
                return False
            return seen_writer

        # The provider leaves this raw descriptor open. Only process death
        # closes it; observing EOF works even when ps cannot inspect a zombie.
        wait_for(closed_by_kernel, "provider exit FIFO EOF")

    def finish(self, exit_code, *, supervisor_exit=True):
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
        if provider:
            assert not process_state(provider["pid"]), "provider was not reaped"
            if self.impl == "rust" and supervisor_exit:
                record = read_json(self.evidence / "supervisor.exit.json")
                assert record.get("schema") == "provenant.lane.exit.v1", "Rust must not fall back to Python"
                assert record["pid"] == provider["pid"]
        if row.get("pgid"):
            wait_for(lambda: not process_group_pids(row["pgid"]), "provider process group exit")
        return row

    def cancel(self):
        script = ROOT / "skills/orchestrate/scripts/run_controls.py"
        result = subprocess.run([sys.executable, str(script), "cancel", "--run-dir", str(self.run),
                        "--task-id", "seed", "--attempt-id", "attempt-001", "--wait-seconds", "15"],
                       cwd=self.root, env=self.env, check=True, capture_output=True, text=True, timeout=20)
        return json.loads(result.stdout)

    def close(self):
        try:
            os.write(self.descendant_fifo, b"close\n")
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
                if not pgid or pgid == os.getpgrp():
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
            try:
                pgid = self.attempt().get("pgid")
                if pgid and pgid != os.getpgrp() and process_group_pids(pgid):
                    os.killpg(pgid, signal.SIGKILL)
            except Exception:  # best effort; never mask the in-flight error
                pass
            finally:
                for name in ("middle.json", "grandchild.json"):
                    descendant = read_json(self.control / name)
                    if descendant and not process_stopped(descendant["pid"]):
                        try:
                            os.kill(descendant["pid"], signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                os.close(self.fifo)
                os.close(self.descendant_fifo)
                os.close(self.exit_fifo)
                if self.log:
                    self.log.close()
