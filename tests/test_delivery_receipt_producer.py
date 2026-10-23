"""Legacy bounded process-runner tests; receipt fixtures retired in workflow B."""

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"


def load(path: Path, name: str):
    return load_script(path, name)


def load_producer():
    return load(PRODUCER, "delivery_receipt_under_test")


def descendant_pair(ready: Path, child_body: str, *, leader_tail: str = "") -> str:
    """Build a leader command that spawns a SIGTERM-immune descendant."""
    child = (
        "import os,pathlib,signal,time; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        + child_body
    )
    return (
        "import pathlib,subprocess,sys,time; "
        f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
        f"ready=pathlib.Path({str(ready)!r}); "
        "deadline=time.monotonic()+0.5; "
        "\nwhile not ready.exists() and time.monotonic()<deadline: time.sleep(0.005)"
        + leader_tail
    )


def wait_for_ready_pid(path: Path, timeout: float = 1) -> int:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            return int(path.read_text())
        except (OSError, ValueError):
            # Absent, or caught between the writer's truncate and write.
            time.sleep(0.005)
    pytest.fail(f"descendant did not write its ready file: {path}")


def wait_for_process_exit(pid: int, timeout: float = 1) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.005)
    return False


def test_bounded_execution_times_out_and_truncates_output():
    module = load_producer()

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            cwd=ROOT,
            timeout_seconds=0.05,
        )
    noisy = (
        f"import sys; sys.stdout.write('x'*{module.MAX_LOG_BYTES * 2}); "
        f"sys.stderr.write('y'*{module.MAX_LOG_BYTES * 2})"
    )

    exit_code, stdout, stderr = module.execute_bounded(
        [sys.executable, "-c", noisy], cwd=ROOT,
    )

    assert exit_code == 0
    assert len(stdout.encode()) <= module.MAX_LOG_BYTES
    assert len(stderr.encode()) <= module.MAX_LOG_BYTES


def test_bounded_execution_enforces_deadline_after_output_closes():
    module = load_producer()
    started = time.monotonic()

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [
                sys.executable, "-c",
                "import os,time; os.close(1); os.close(2); time.sleep(2)",
            ],
            cwd=ROOT,
            timeout_seconds=0.05,
        )

    assert time.monotonic() - started < 1


def test_bounded_execution_kills_descendants_on_timeout(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    release = tmp_path / "descendant-release"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        f"pathlib.Path({str(ready)!r}).write_text(str(os.getpid())); "
        f"\nwhile not pathlib.Path({str(release)!r}).exists(): time.sleep(0.005)\n"
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
        leader_tail="\ntime.sleep(60)",
    )

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=0.25,
        )
    pid = wait_for_ready_pid(ready)
    release.touch()
    assert wait_for_process_exit(pid)
    assert not marker.exists()


def test_bounded_execution_kills_descendants_after_successful_leader_exit(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    release = tmp_path / "descendant-release"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        "os.close(1); os.close(2); "
        f"pathlib.Path({str(ready)!r}).write_text(str(os.getpid())); "
        f"\nwhile not pathlib.Path({str(release)!r}).exists(): time.sleep(0.005)\n"
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )
    assert exit_code == 0
    pid = wait_for_ready_pid(ready)
    release.touch()
    assert wait_for_process_exit(pid)
    assert not marker.exists()


def test_bounded_execution_preserves_success_while_cleaning_descendants(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    release = tmp_path / "descendant-release"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        f"pathlib.Path({str(ready)!r}).write_text(str(os.getpid())); "
        f"\nwhile not pathlib.Path({str(release)!r}).exists(): time.sleep(0.005)\n"
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )

    assert exit_code == 0
    pid = wait_for_ready_pid(ready)
    release.touch()
    assert wait_for_process_exit(pid)
    assert not marker.exists()


def test_bounded_execution_stops_draining_pipes_from_escaped_descendant(tmp_path):
    module = load_producer()
    child_pid = tmp_path / "escaped-child-pid"
    leader = descendant_pair(
        child_pid,
        f"os.setsid(); pathlib.Path({str(child_pid)!r}).write_text(str(os.getpid())); "
        "\nwhile True: time.sleep(0.005)",
    )
    started = time.monotonic()

    try:
        exit_code, _stdout, _stderr = module.execute_bounded(
            [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
        )
    finally:
        if child_pid.exists():
            try:
                os.kill(int(child_pid.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass

    assert exit_code == 0
    assert time.monotonic() - started < 0.6


def test_post_sigkill_wait_uses_executor_timeout(monkeypatch):
    module = load_producer()
    waits = []

    class StuckProcess:
        pid = 12345

        def wait(self, timeout=None):
            waits.append(timeout)
            raise subprocess.TimeoutExpired(["stuck-process"], timeout)

    monkeypatch.setattr(module.process_runner.os, "killpg", lambda _pid, _signal: None)
    monkeypatch.setattr(module.process_runner.time, "sleep", lambda _seconds: None)

    with pytest.raises(
        module.ReceiptError, match="did not exit after SIGKILL within 0.25 seconds",
    ):
        module.process_runner.terminate_process_group(
            StuckProcess(), timeout_seconds=0.25, error_type=module.ReceiptError,
        )

    assert waits == [0.1, 0.25]


def test_bounded_execution_closes_resources_when_termination_raises(
    tmp_path, monkeypatch,
):
    module = load_producer()
    real_selector = module.process_runner.selectors.DefaultSelector
    real_popen = module.process_runner.subprocess.Popen
    real_terminate = module.process_runner.terminate_process_group
    selectors: list = []
    processes: list = []
    termination_attempts: list = []

    class TrackingSelector:
        def __init__(self):
            self.inner = real_selector()
            self.closed = False
            selectors.append(self)

        def __getattr__(self, name):
            return getattr(self.inner, name)

        def close(self):
            self.closed = True
            self.inner.close()

    def tracking_popen(*args, **kwargs):
        process = real_popen(*args, **kwargs)
        processes.append(process)
        return process

    def failing_terminate(process, *, timeout_seconds, error_type):
        termination_attempts.append(process.pid)
        real_terminate(process, timeout_seconds=timeout_seconds, error_type=error_type)
        raise error_type("forced termination failure")

    monkeypatch.setattr(
        module.process_runner.selectors, "DefaultSelector", TrackingSelector,
    )
    monkeypatch.setattr(module.process_runner.subprocess, "Popen", tracking_popen)
    monkeypatch.setattr(module.process_runner, "terminate_process_group", failing_terminate)

    with pytest.raises(module.ReceiptError, match="forced termination failure"):
        module.execute_bounded(
            [sys.executable, "-c", "pass"], cwd=tmp_path, timeout_seconds=1,
        )

    assert len(termination_attempts) == 1
    assert selectors[0].closed
    assert processes[0].stdout is not None and processes[0].stdout.closed
    assert processes[0].stderr is not None and processes[0].stderr.closed


def run_cli(workspace: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(PRODUCER), *args],
        cwd=workspace,
        text=True,
        capture_output=True,
        check=False,
    )


def routine_assessment() -> dict[str, str]:
    return {
        "blast_radius": "local",
        "reversibility": "easy",
        "data_sensitivity": "public",
        "migration": "none",
        "oracle_quality": "strong",
        "external_effects": "none",
        "critical_surface": "none",
    }


def authority(run_id: str) -> dict[str, object]:
    return {
        "schema_version": 2,
        "approved_by": "human-owner",
        "evidence": "authority-approval",
        "evidence_digest": "sha256:" + "a" * 64,
        "workspace_roots": ["."],
        "expires_at": "2099-01-01T00:00:00Z",
        "allowed_source_paths": ["."],
        "allowed_artifact_paths": [".", f".agent-run/{run_id}"],
        "denied_paths": [],
        "prohibited_actions": ["external-publish", "deployment", "irreversible-action"],
        "disclosure": "local-only",
        "secrets_access": "none",
        "secret_refs": [],
        "deployment": False,
        "deployment_targets": [],
        "irreversible_actions": False,
        "irreversible_action_ids": [],
        "network": {"tool_egress": "none", "allowed_hosts": []},
        "budget": {},
        "delegations": [],
    }


def init_args(run_id: str = "DEL-TEST") -> list[str]:
    return [
        "init",
        "--run-dir", f".agent-run/{run_id}",
        "--run-id", run_id,
        "--profile", "software",
        "--chair-family", "openai",
        "--risk-assessment", json.dumps(routine_assessment()),
        "--intent", "intent.md",
        "--authority", json.dumps(authority(run_id)),
    ]


def test_init_rejects_canonical_run_in_linked_worktree(tmp_path):
    primary = tmp_path / "primary"
    primary.mkdir()
    subprocess.run(["git", "init", "-q", str(primary)], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.name", "Test"], check=True)
    (primary / "intent.md").write_text("# Intent\n")
    subprocess.run(["git", "-C", str(primary), "add", "intent.md"], check=True)
    subprocess.run(["git", "-C", str(primary), "commit", "-qm", "init"], check=True)
    linked = tmp_path / "linked"
    subprocess.run(["git", "-C", str(primary), "worktree", "add", "-qb", "test/linked", str(linked)], check=True)
    args = init_args("DEL-TEST")
    args[2] = ".agent-run/runs/20260923-1000-delivery-del-test-a1b2c3"
    result = run_cli(linked, *args)
    assert result.returncode != 0
    assert "primary checkout" in result.stderr
    assert not (linked / ".agent-run").exists()
