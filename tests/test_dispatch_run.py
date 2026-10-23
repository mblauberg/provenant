"""Contract tests for the ordinary one-attempt dispatch owner."""

from __future__ import annotations

import hashlib
import io
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import textwrap
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path
from types import SimpleNamespace

import pytest

from tests.support.confinement import force_sandbox

from tests.support.dispatch import disable_memory_admission_floor, real_owner_fixture
from tests.support.fake_cli import fake_codex, write_executable
from tests.support.runs import make_run as _make_run

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/dispatch_run.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
FINALIZE = ROOT / "skills/orchestrate/scripts/run_dir_finalize.py"


def make_run(root: Path, name: str = "run") -> Path:
    return _make_run(root.resolve(), name)


def write_success_adapter(path: Path) -> None:
    write_executable(
        path,
        """#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --out) out="$2"; shift 2;;
            --intent) intent="$2"; shift 2;;
            --tool) tool="$2"; shift 2;;
            *) shift;;
          esac
        done
        printf 'OK\n' > "$out"
        digest="sha256:$(shasum -a 256 "$out" | awk '{print $1}')"
        printf '{"tool":"%s","adapter":"%s","execution_intent":"%s","resolved_model":"test-model","provider_family":"test-family","model_family":"test-family","endpoint_provider":"test-provider","identity_source":"test-fixture","status":"ok","exit":0,"output_path":"%s","output_digest":"%s","read_only_guarantee":"none","cross_family":false,"certification_eligible":false}\n' "$tool" "$tool" "$intent" "$out" "$digest"
        """,
    )


def write_evidence_adapter(path: Path, *, status: str = "ok") -> None:
    write_executable(
        path,
        f"""#!/usr/bin/env bash
        prompt=''; out=''; add_dir=''
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --out) out="$2"; shift 2;;
            --prompt-file) prompt="$2"; shift 2;;
            --add-dir) add_dir="$2"; shift 2;;
            *) shift;;
          esac
        done
        grep -qi 'read the supplied evidence files' "$prompt" || exit 21
        test -f "$add_dir/git-evidence.md" || exit 22
        test -z "${{CF_DISPATCH_AGY_ADD_DIR:-}}" || exit 23
        printf 'OK\\n' > "$out"
        digest="sha256:$(shasum -a 256 "$out" | awk '{{print $1}}')"
        printf '{{"tool":"agy","adapter":"agy","execution_intent":"ordinary","resolved_model":"gemini-3.7-flash","provider_family":"google","model_family":"google","endpoint_provider":"agy","identity_source":"test-fixture","status":"{status}","exit":{0 if status == 'ok' else 1},"output_path":"%s","output_digest":"%s","read_only_guarantee":"prompt_only","cross_family":true,"certification_eligible":false}}\\n' "$out" "$digest"
        """,
    )


def write_question_adapter(path: Path, *, exit_code: int = 0, result: str | None = None) -> None:
    output = (result or json.dumps({
        "schema_version": 1,
        "record_type": "provenant-worker-terminal",
        "classification": "question",
        "question": {"code": "needs_input", "prompt": "Which source should I use?"},
    })).rstrip("\n")
    write_executable(
        path,
        f"""#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --out) out="$2"; shift 2;;
            *) shift;;
          esac
        done
        printf '%s\\n' '{output}' > "$out"
        digest="sha256:$(shasum -a 256 "$out" | awk '{{print $1}}')"
        printf '{{"tool":"codex","adapter":"codex","execution_intent":"ordinary","resolved_model":"test-model","provider_family":"test-family","model_family":"test-family","endpoint_provider":"test-provider","identity_source":"test-fixture","status":"ok","exit":0,"output_path":"%s","output_digest":"%s","read_only_guarantee":"none","cross_family":false,"certification_eligible":false}}\\n' "$out" "$digest"
        exit {exit_code}
        """,
    )


def dispatch_args(**overrides):
    profile = overrides.pop("_profile", "common")
    profiles = {
        "common": {
            "tool": "codex", "alias": "senior", "model": "fixture", "effort": "low",
            "task_id": "task-1", "access_mode": "read_only", "worktree": None,
            "_last_plan": {},
        },
        "resume": {
            "run_dir": None, "resume": "resume-me", "task_id": None,
            "tool": None, "model": None, "effort": None, "access_mode": None,
            "worktree": None, "provider_cwd": None, "sandbox": None, "network": None,
            "add_dirs": [], "resume_session": None, "fallback": None,
            "context_ceiling": None, "intent": "ordinary", "orchestrator_family": "",
            "role": "worker", "risk_tier": "", "model_override_tier": "",
            "reviewer_id": "", "preface": True,
        },
        "sequence": {"fallback": False, "resume": False, "timeout_seconds": 1000, "confidential": False},
        "receipt": {
            "tool": "opencode", "alias": "senior", "model": "gpt-6-luna",
            "alias_supplied": True, "effort": None, "task_id": "dispatch-001",
            "access_mode": "read_only", "worktree": None, "_phase_timings": {},
            "reviewer_id": None, "risk_tier": None, "model_override_tier": None,
        },
    }
    defaults = profiles[profile].copy()
    defaults.update(overrides)
    return SimpleNamespace(**defaults)


def timestamp(offset: timedelta = timedelta()) -> str:
    return (datetime.now(UTC) + offset).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def load_dispatch_module():
    spec = importlib.util.spec_from_file_location("dispatch_run_under_test", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_secret_scan_module():
    import importlib
    load_dispatch_module()
    return importlib.import_module('secret_scan')


def test_process_evidence_without_start_time_is_unknown(tmp_path):
    mod = load_dispatch_module()
    evidence = mod.process_evidence(tmp_path, {"pgid": os.getpgrp(), "paths": {}})
    assert evidence["pgid_probe"] == "unknown"


@pytest.mark.parametrize("task_id,attempt", [("other-task", 1), ("task-1", 2), ("task-1", 1)])
def test_retire_provider_preserves_another_attempt_marker(tmp_path, task_id, attempt):
    mod = load_dispatch_module()
    marker = tmp_path / "dispatch-provider.json"
    marker.write_text(json.dumps({"run_token": mod.run_records.run_identity(tmp_path), "provider_pid": 12345,
                                  "task_id": task_id, "attempt": attempt}))
    mod._retire_provider_process(tmp_path, SimpleNamespace(pid=12345, returncode=0), task_id="task-1", attempt=1)
    assert marker.exists() is ((task_id, attempt) != ("task-1", 1))


def test_reap_attempt_after_group_sigkill_is_terminal_and_idempotent(tmp_path):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "runs/killed")
    status_path = run / "dispatch-status.json"
    status_path.write_text(json.dumps({"status": "running", "finished_at": None, "run_token": "keep"}))
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], start_new_session=True)
    try:
        started = mod.process_info.start_time(child.pid)
        assert started
        path = run / "tasks/task-1/attempt-001/attempt.json"
        path.parent.mkdir(parents=True)
        row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
        row.update(state="running", status=None, pgid=child.pid, pgid_started_at=started)
        path.write_text(json.dumps(row))
        (path.parent / "events.jsonl").write_bytes(b'{"type":"item.completed"}\n')
        (path.parent / "stderr.log").write_bytes(b"partial diagnostic\n")
        os.utime(path.parent / "events.jsonl", (1700000000, 1700000000))
        os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)
        mod.reap_attempts(run)
        checkpoint = json.loads((path.parent / "wip-checkpoint.json").read_text())
        assert checkpoint["process_alive"] is False
        assert checkpoint["diff_summary"]
        assert "last_tool_call" in checkpoint
        assert checkpoint["next_step"]
        lost = json.loads(path.read_text())
        assert lost["state"] == "terminal"
        assert lost["status"] == "lost"
        assert lost["error_class"] == "provider_error"
        assert lost["ended_at"]
        assert lost["evidence"]["signature"] == "pgid_gone"
        assert lost["evidence"]["pgid_probe"] == "gone"
        assert lost["evidence"]["last_event_at"] == "2023-11-14T22:13:20.000Z"
        assert lost["evidence"]["events_bytes"] == (path.parent / "events.jsonl").stat().st_size
        assert lost["evidence"]["stderr_bytes"] == (path.parent / "stderr.log").stat().st_size
        status = json.loads(status_path.read_text())
        assert status["status"] == "failed"
        assert status["finished_at"]
        assert status["run_token"] == "keep"
        before = path.read_bytes()
        mod.reap_attempts(run)
        assert path.read_bytes() == before
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=5)


@pytest.mark.parametrize("identity,live,stale,expected", [
    ("matching", True, True, "running"),
    ("reused", True, False, "terminal"),
    ("unknown", True, True, "running"),
    (None, True, True, "running"),
    (None, False, False, "running"),
    (None, False, True, "terminal"),
])
def test_reap_attempt_identity_and_legacy_progress(tmp_path, monkeypatch, identity, live, stale, expected):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "identity")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=123456,
               last_progress_at="2000-01-01T00:00:00Z" if stale else mod.now())
    if identity is not None:
        row["pgid_started_at"] = "recorded"
    path.write_text(json.dumps(row))
    def killpg(pgid, sig):
        assert (pgid, sig) == (123456, 0)
        if not live:
            raise ProcessLookupError()
    monkeypatch.setattr(mod.os, "killpg", killpg)
    monkeypatch.setattr(mod.process_info, "start_time", lambda pid: None if identity == "unknown" else "different" if identity == "reused" else "recorded")
    mod.reap_attempts(run)
    result = json.loads(path.read_text())
    assert result["state"] == expected
    if expected == "terminal":
        assert result["status"] == "lost"
        assert result["evidence"]["signature"] == ("pgid_reused" if live else "pgid_gone")


def test_reap_attempt_never_touches_live_owner(tmp_path):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "owned")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({"state": "running", "pgid": 99999999, "pgid_started_at": "gone"}))
    before = path.read_bytes()
    with mod.acquire_run_custody(run):
        mod.reap_attempts(run)
    assert path.read_bytes() == before


def test_reap_attempt_skips_malformed_records_and_continues(tmp_path):
    run = make_run(tmp_path, "runs/malformed")
    valid = make_run(tmp_path, "runs/valid")
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=99999999, pgid_started_at="gone")
    for task, record in (("a-bad", []), ("b-progress", {**row, "pgid_started_at": None, "last_progress_at": None}),
                         ("c-valid", row)):
        path = run / f"tasks/{task}/attempt-001/attempt.json"
        path.parent.mkdir(parents=True)
        path.write_text(json.dumps(record))
    valid_path = valid / "tasks/task-1/attempt-001/attempt.json"
    valid_path.parent.mkdir(parents=True)
    valid_path.write_text(json.dumps(row))
    result = subprocess.run([sys.executable, str(SCRIPT), "--reap-attempts"],
                            input=json.dumps([str(run), str(valid)]), capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == [str(run), str(valid)]
    assert json.loads((run / "tasks/c-valid/attempt-001/attempt.json").read_text())["status"] == "lost"
    assert json.loads(valid_path.read_text())["status"] == "lost"


def test_reap_attempt_preserves_terminal_publication_after_snapshot(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "publish-race")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=99999999, pgid_started_at="gone")
    path.write_text(json.dumps(row))
    terminal = {**row, "state": "terminal", "status": "ok", "ended_at": mod.now()}
    def gone(pgid, sig):
        path.write_text(json.dumps(terminal))
        raise ProcessLookupError()
    monkeypatch.setattr(mod.os, "killpg", gone)
    assert mod.reap_attempts(run) == []
    assert json.loads(path.read_text()) == terminal


def test_attempt_publication_waits_for_custody(tmp_path):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "publication-custody")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row["paths"]["receipt"] = str(path.relative_to(run))
    payload = json.dumps(row)
    code = """
import importlib.util, json, pathlib, sys
spec = importlib.util.spec_from_file_location("dispatch_publisher", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
print("ready", flush=True)
mod.publish_contract(pathlib.Path(sys.argv[2]), json.loads(sys.argv[3]))
"""
    child = None
    try:
        with mod.attempt_custody(run, path):
            child = subprocess.Popen([sys.executable, "-c", code, str(SCRIPT), str(run), payload],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            assert child.stdout.readline().strip() == "ready"
            with pytest.raises(subprocess.TimeoutExpired):
                child.communicate(timeout=0.1)
            assert not path.exists()
        stdout, stderr = child.communicate(timeout=5)
        assert child.returncode == 0, stderr
        assert json.loads(path.read_text())["state"] == "terminal"
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.communicate(timeout=5)


def test_reap_attempt_skips_busy_publisher_and_releases_run_custody(tmp_path):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "busy-publisher")
    paths = []
    for task in ("busy", "free"):
        path = run / f"tasks/{task}/attempt-001/attempt.json"
        path.parent.mkdir(parents=True)
        row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
        row.update(state="running", status=None, task_id=task, pgid=99999999, pgid_started_at="gone")
        path.write_text(json.dumps(row))
        paths.append(path)
    busy, free = paths
    code = """
import importlib.util, json, pathlib, sys
spec = importlib.util.spec_from_file_location("dispatch_reaper", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
print(json.dumps(mod.reap_attempts(pathlib.Path(sys.argv[2]))))
"""
    with mod.attempt_custody(run, busy):
        result = subprocess.run([sys.executable, "-c", code, str(SCRIPT), str(run)],
                                capture_output=True, text=True, timeout=3)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout) == [str(free)]
        assert json.loads(busy.read_text())["state"] == "running"
        assert json.loads(free.read_text())["status"] == "lost"
        with mod.acquire_run_custody(run):
            pass
    assert mod.reap_attempts(run) == [str(busy)]


@pytest.mark.parametrize("evidence,expected", [
    ("closure", "interrupted"), ("cancelled-closure", "cancelled"),
    ("marker", "cancelled"), ("batch-marker", "cancelled"),
    ("batch-closure", "interrupted"), ("completed-batch", "lost"),
    ("request", "interrupted"), ("cancel-request", "cancelled"),
    ("other-pid", "interrupted"), ("stale-time", "lost"), ("future-time", "lost"),
    ("stale-token", "lost"), ("no-token", "lost"),
])
def test_reap_attempt_stop_evidence(tmp_path, evidence, expected):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "stop-evidence")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=99999999, pgid_started_at="gone")
    owner = {"owner_pid": 99999999, "run_token": "current", "status": "running"}
    if evidence in {"closure", "cancelled-closure", "batch-closure"}:
        owner.update(status="cancelled" if evidence == "cancelled-closure" else "interrupted", finished_at=mod.now())
    if evidence == "no-token":
        owner.pop("run_token")
    (run / "dispatch-status.json").write_text(json.dumps(owner))
    if evidence == "marker":
        directory = run / "dispatch/tasks/task-1/attempt-001"
        directory.mkdir(parents=True)
        (directory / "cancel.request").touch()
    if evidence in {"batch-marker", "batch-closure", "completed-batch"}:
        row["batch_id"] = "batch-001"
        directory = run / "dispatch/batches/batch-001"
        directory.mkdir(parents=True)
        (directory / "cancel.request").touch()
        if evidence == "completed-batch":
            (directory / "summary.json").write_text(json.dumps({"status": "completed"}))
    if evidence in {"request", "cancel-request", "other-pid", "stale-time", "future-time", "stale-token", "no-token"}:
        request = {"run_token": "current", "provider_pid": row["pgid"], "signal": "SIGTERM",
                   "source": "cancel_requested" if evidence == "cancel-request" else "orphan_reaper",
                   "sender_pid": os.getpid(), "requested_at": mod.now()}
        if evidence == "other-pid": request["provider_pid"] = 99999998
        if evidence == "stale-time": request["requested_at"] = "2000-01-01T00:00:00Z"
        if evidence == "future-time": request["requested_at"] = "2999-01-01T00:00:00Z"
        if evidence == "stale-token": request["run_token"] = "previous"
        (run / "termination-request.json").write_text(json.dumps(request))
    path.write_text(json.dumps(row))
    assert mod.reap_attempts(run) == [str(path)]
    result = json.loads(path.read_text())
    assert result["status"] == expected
    assert result["retryable"] is (expected == "lost")
    if expected == "cancelled":
        assert result["error_class"] == "cancelled"


@pytest.mark.parametrize("provider_pid", [None, 99999000])
@pytest.mark.parametrize("stale", [False, True])
def test_reap_attempt_run_cancel_applies_to_every_batch_child(tmp_path, provider_pid, stale):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "batch-run-cancel")
    (run / "dispatch-status.json").write_text(json.dumps({
        "owner_pid": 99999999, "run_token": "current", "status": "running",
    }))
    paths = []
    for index in range(8):
        path = run / f"tasks/task-{index}/attempt-001/attempt.json"
        path.parent.mkdir(parents=True)
        row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
        row.update(state="running", status=None, pgid=99999000 + index, pgid_started_at="gone",
                   started_at="2001-01-01T00:00:00Z", task_id=f"task-{index}", batch_id="batch-001")
        path.write_text(json.dumps(row))
        paths.append(path)
    (run / "termination-request.json").write_text(json.dumps({
        "run_token": "current", "provider_pid": provider_pid, "signal": "SIGTERM",
        "source": "cancel_requested", "sender_pid": os.getpid(),
        "requested_at": "2000-01-01T00:00:00Z" if stale else mod.now(),
    }))
    assert set(mod.reap_attempts(run)) == {str(path) for path in paths}
    for path in paths:
        result = json.loads(path.read_text())
        assert result["status"] == ("lost" if stale else "cancelled")
        assert result["retryable"] is stale
        assert result["error_class"] == ("provider_error" if stale else "cancelled")


@pytest.mark.parametrize("evidence", ["closure", "marker", "request"])
def test_reap_attempt_refreshes_stop_evidence_after_snapshot(tmp_path, monkeypatch, evidence):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "stop-race")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=99999999, pgid_started_at="gone")
    path.write_text(json.dumps(row))
    owner = {"owner_pid": 99999999, "run_token": "current", "status": "running"}
    (run / "dispatch-status.json").write_text(json.dumps(owner))
    def gone(pgid, sig):
        if evidence == "closure":
            (run / "dispatch-status.json").write_text(json.dumps({**owner, "status": "interrupted", "finished_at": mod.now()}))
        elif evidence == "request":
            (run / "termination-request.json").write_text(json.dumps({
                "run_token": "current", "provider_pid": pgid, "signal": "SIGTERM", "source": "run_control",
                "sender_pid": os.getpid(), "requested_at": mod.now(),
            }))
        else:
            directory = run / "dispatch/tasks/task-1/attempt-001"
            directory.mkdir(parents=True)
            (directory / "cancel.request").touch()
        raise ProcessLookupError()
    monkeypatch.setattr(mod.os, "killpg", gone)
    mod.reap_attempts(run)
    assert json.loads(path.read_text())["status"] == ("cancelled" if evidence == "marker" else "interrupted")


@pytest.mark.parametrize("live", [False, True])
def test_reap_attempt_owners_without_run_token(tmp_path, monkeypatch, live):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "legacy-owner")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=99999999, pgid_started_at="gone")
    path.write_text(json.dumps(row))
    (run / "dispatch-owner.json").write_text(json.dumps({"owner_pid": os.getpid() if live else 99999999}))
    mod.reap_attempts(run)
    assert json.loads(path.read_text())["state"] == ("running" if live else "terminal")


@pytest.mark.parametrize('name,positive,placeholder', [
    ('PEM private key', '-' * 5 + 'BEGIN RSA ' + 'PRIVATE KEY-----\n' + 'A' * 64 + '\n' + '-' * 5 + 'END RSA PRIVATE KEY-----',
     '-' * 5 + 'BEGIN RSA PRIVATE KEY-----\nEXAMPLE\n' + '-' * 5 + 'END RSA PRIVATE KEY-----'),
    ('AWS access key ID', 'AKIA' + 'A' * 16, 'AKIA' + 'X' * 12 + 'XXXX'),
    ('AWS access key ID', 'ASIA' + 'A' * 16, '<ASIA' + 'A' * 16 + '>'),
    ('GitHub token', 'ghp_' + 'a' * 36, 'ghp_' + 'a' * 16 + 'EXAMPLE' + 'a' * 12),
    ('GitHub token', 'gho_' + 'a' * 36, 'gho_' + 'a' * 16 + 'XXXX' + 'a' * 12),
    ('GitHub token', 'ghu_' + 'a' * 36, 'ghu_' + 'a' * 16 + 'EXAMPLE' + 'a' * 12),
    ('GitHub token', 'ghs_' + 'a' * 36, 'ghs_' + 'a' * 16 + 'EXAMPLE' + 'a' * 12),
    ('GitHub token', 'ghr_' + 'a' * 36, 'ghr_' + 'a' * 16 + 'EXAMPLE' + 'a' * 12),
    ('GitHub token', 'github_pat_' + 'a' * 30, 'github_pat_EXAMPLE' + 'a' * 30),
    ('OpenAI/Anthropic key', 'sk-ant-' + 'a' * 50, 'sk-ant-' + 'a' * 42 + 'EXAMPLE'),
    ('OpenAI/Anthropic key', 'sk-proj-' + 'a' * 50, 'sk-proj-' + 'a' * 42 + 'EXAMPLE'),
    ('OpenAI/Anthropic key', 'sk-' + 'a' * 50, 'sk-' + 'a' * 42 + 'EXAMPLE'),
    ('Slack token', 'xoxb-' + 'a' * 24, 'xoxb-EXAMPLE' + 'a' * 24),
    ('Slack token', 'xoxp-' + 'a' * 24, 'xoxp-EXAMPLE' + 'a' * 24),
    ('Slack token', 'xoxa-' + 'a' * 24, 'xoxa-EXAMPLE' + 'a' * 24),
    ('Slack token', 'xoxr-' + 'a' * 24, 'xoxr-EXAMPLE' + 'a' * 24),
    ('Slack token', 'xoxs-' + 'a' * 24, 'xoxs-EXAMPLE' + 'a' * 24),
    ('Google API key', 'AIza' + 'a' * 35, 'AIza' + 'a' * 28 + 'EXAMPLE'),
    ('Stripe live key', 'sk_live_' + 'a' * 24, 'sk_live_EXAMPLE' + 'a' * 24),
    ('Stripe live key', 'rk_live_' + 'a' * 24, 'rk_live_EXAMPLE' + 'a' * 24),
    ('Bearer JWT', 'Bearer ' + 'a' * 12 + '.' + 'b' * 12 + '.' + 'c' * 32,
     'Bearer ' + 'a' * 12 + '.' + 'EXAMPLEabcde' + '.' + 'c' * 32),
])
def test_secret_patterns_find_live_shapes_but_skip_placeholders(name, positive, placeholder):
    scan = load_secret_scan_module()
    assert [item.name for item in scan.scan_bytes(positive.encode(), '<prompt>')] == [name]
    assert scan.scan_bytes(placeholder.encode(), '<prompt>') == []


def test_secret_angle_placeholder_requires_both_brackets():
    scan = load_secret_scan_module()
    key = 'AKIA' + 'A' * 16
    for content in (f'<{key}', f'{key}>'):
        assert [item.name for item in scan.scan_bytes(content.encode(), '<prompt>')] == ['AWS access key ID']
    assert scan.scan_bytes(f'<{key}>'.encode(), '<prompt>') == []


def test_secret_scan_skips_deleted_tracked_files(tmp_path):
    scan = load_secret_scan_module()
    subprocess.run(['git', 'init', '-q'], cwd=tmp_path, check=True)
    deleted = tmp_path / 'deleted.txt'
    deleted.write_text('ordinary content')
    subprocess.run(['git', 'add', 'deleted.txt'], cwd=tmp_path, check=True)
    deleted.unlink()
    (tmp_path / 'present.txt').write_text('AKIA' + 'A' * 16)

    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert [(finding.name, Path(finding.path).name) for finding in result.findings] == [
        ('AWS access key ID', 'present.txt')
    ]


def test_secret_scan_skips_ignored_regular_files_and_marks_budget_exceeded(tmp_path, monkeypatch):
    scan = load_secret_scan_module()
    subprocess.run(['git', 'init', '-q'], cwd=tmp_path, check=True)
    (tmp_path / '.gitignore').write_text('ignored.txt\n.env\n')
    (tmp_path / 'ignored.txt').write_text('AKIA' + 'A' * 16)
    (tmp_path / '.env').write_text('rk_live_' + 'a' * 24)
    (tmp_path / 'tracked.txt').write_text('AKIA' + 'B' * 16)
    (tmp_path / 'extra.txt').write_text('ghp_' + 'a' * 30)
    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert {finding.name for finding in result.findings} == {'AWS access key ID', 'GitHub token'}
    assert {Path(finding.path).name for finding in result.findings} == {
        'tracked.txt', 'extra.txt'
    }
    monkeypatch.setattr(scan, 'MAX_FILES', 1)
    limited = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert limited.budget_exceeded
    assert limited.warnings == ['secret scan budget reached']
    monkeypatch.setattr(scan, 'MAX_FILES', 2000)
    monkeypatch.setattr(scan, 'MAX_TOTAL_BYTES', 1)
    limited = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert limited.budget_exceeded
    assert limited.warnings == ['secret scan budget reached']


def test_secret_scan_skips_binary_and_untracked_system_dirs(tmp_path):
    scan = load_secret_scan_module()
    secret = b'AKIA' + b'A' * 16
    (tmp_path / 'binary.dat').write_bytes(b'\0' + secret)
    for name in ('node_modules', '.git'):
        directory = tmp_path / name
        directory.mkdir()
        (directory / 'secret.txt').write_bytes(secret)
    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert result.findings == []
    assert not result.budget_exceeded


def test_secret_scan_reads_whole_large_text_files_within_the_budget(tmp_path, monkeypatch):
    scan = load_secret_scan_module()
    megabyte = 1024 * 1024
    (tmp_path / 'head.txt').write_bytes(b'AKIA' + b'A' * 16 + b'\n' + b'a' * megabyte)
    (tmp_path / 'tail.log').write_bytes(b'a' * 2 * megabyte + b'\n' + b'AKIA' + b'B' * 16)
    # Only a NUL near the start marks a file binary, as Git judges it, so a trailing one hides nothing.
    (tmp_path / 'nul.txt').write_bytes(b'AKIA' + b'C' * 16 + b'\n' + b'a' * 9000 + b'\0')
    (tmp_path / 'image.bin').write_bytes(b'\0' + b'AKIA' + b'D' * 16)
    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert not result.budget_exceeded
    assert sorted(Path(item.path).name for item in result.findings) == ['head.txt', 'nul.txt', 'tail.log']
    assert result.warnings == []
    # A file the remaining budget cannot hold is not scanned in part: the scan fails closed.
    monkeypatch.setattr(scan, 'MAX_TOTAL_BYTES', 2 * megabyte)
    limited = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert limited.budget_exceeded


def test_secret_scan_budget_fix_names_the_largest_subtree(tmp_path, monkeypatch):
    scan = load_secret_scan_module()
    deep = tmp_path / 'review' / 'deep'
    deep.mkdir(parents=True)
    for index in range(5):
        (deep / f'{index}.txt').write_text('x' * 100)
    (tmp_path / 'small').mkdir()
    (tmp_path / 'small' / 'one.txt').write_text('y' * 10)
    monkeypatch.setattr(scan, 'MAX_FILES', 3)
    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert result.budget_exceeded
    assert f"largest subtree {deep}" in result.fix()
    assert 'secret_scan_exclude' in result.fix()


def test_secret_scan_skips_excluded_directories_with_a_warning(tmp_path):
    scan = load_secret_scan_module()
    (tmp_path / 'fixtures').mkdir()
    (tmp_path / 'fixtures' / 'key.pem').write_text('AKIA' + 'A' * 16)
    (tmp_path / 'source.txt').write_text('ghp_' + 'a' * 30)
    (tmp_path / '.agents').mkdir()
    outside = tmp_path.parent / (tmp_path.name + '-outside')
    outside.mkdir()
    (outside / 'key.txt').write_text('AKIA' + 'C' * 16)
    (tmp_path / 'escape').symlink_to(outside)
    (tmp_path / '.agents' / 'fabric-policy.json').write_text(
        json.dumps({'secret_scan_exclude': ['fixtures', '../outside', 7, 'escape']}))
    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path), str(outside)], workspace_root=tmp_path)
    assert sorted(Path(item.path).name for item in result.findings) == ['key.txt', 'source.txt']
    assert result.warnings == [
        ".agents/fabric-policy.json secret_scan_exclude takes relative paths inside the project; ignoring '../outside'",
        ".agents/fabric-policy.json secret_scan_exclude takes relative paths inside the project; ignoring 7",
        ".agents/fabric-policy.json secret_scan_exclude takes relative paths inside the project; ignoring 'escape'",
        f"secret scan skipped {(tmp_path / 'fixtures').resolve()} (secret_scan_exclude)",
    ]


def test_preflight_honours_policy_secret_scan_exclusions(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    (tmp_path / '.agents').mkdir()
    (tmp_path / '.agents' / 'fabric-policy.json').write_text(json.dumps({'secret_scan_exclude': ['shared/fixtures']}))
    fixtures = tmp_path / 'shared' / 'fixtures'
    fixtures.mkdir(parents=True)
    (fixtures / 'key.txt').write_text('rk_live_' + 'a' * 24)
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior',
            'prompt': 'review shared files', 'add_dirs': [str(tmp_path / 'shared')]}
    module = load_dispatch_module()
    assert module.dispatch_preflight.preflight_tasks([task], tmp_path)['status'] == 'validated'


def test_secret_scan_budget_skips_vendored_build_and_large_binary_before_counting(tmp_path, monkeypatch):
    scan = load_secret_scan_module()
    for dirname in ("node_modules", ".venv", "dist", "build", "coverage", ".agent-run",
                    "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".next", ".turbo"):
        target = tmp_path / dirname
        target.mkdir()
        (target / "ignored.txt").write_text("ordinary")
    (tmp_path / "large.bin").write_bytes(b"\0" + b"x" * (2 * 1024 * 1024))
    (tmp_path / "source.txt").write_text("ordinary")
    monkeypatch.setattr(scan, "MAX_FILES", 1)
    result = scan.scan_inputs(b"hello", "<prompt>", [str(tmp_path)])
    assert not result.budget_exceeded
    assert result.findings == []


def test_secret_scan_budget_fix_names_offending_directory(tmp_path, monkeypatch):
    scan = load_secret_scan_module()
    directory = tmp_path / "wide-tree"
    directory.mkdir()
    (directory / "one.txt").write_text("one")
    monkeypatch.setattr(scan, "MAX_FILES", 0)
    result = scan.scan_inputs(b"hello", "<prompt>", [str(directory)])
    assert result.budget_exceeded
    assert str(directory) in result.fix()
    assert "narrower" in result.fix()


def test_secret_scan_budget_names_largest_files_including_the_file_over_the_limit(tmp_path, monkeypatch):
    scan = load_secret_scan_module()
    for index, size in enumerate((20, 10, 80, 5)):
        (tmp_path / f"scratch-{index}.txt").write_text("x" * size)
    monkeypatch.setattr(scan, "MAX_TOTAL_BYTES", 100)
    result = scan.scan_inputs(b"hello", "<prompt>", [str(tmp_path)])
    assert result.budget_exceeded
    fix = result.fix()
    assert "largest files" in fix
    assert "scratch-2.txt" in fix and "scratch-0.txt" in fix
    assert "add_dirs" in fix and "allow_secrets" in fix


def test_secret_scan_skips_git_metadata_file(tmp_path):
    scan = load_secret_scan_module()
    (tmp_path / '.git').write_text('AKIA' + 'A' * 16)
    assert scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)]).findings == []


def test_secret_scan_avoids_generic_credential_words_and_unprefixed_jwt():
    scan = load_secret_scan_module()
    content = b'password=' + b'a' * 60 + b'\n' + b'a' * 12 + b'.' + b'b' * 12 + b'.' + b'c' * 32
    assert scan.scan_bytes(content, '<prompt>') == []


def test_terminal_attempt_carries_nested_owner_spared_count(
    tmp_path: Path,
) -> None:
    module = load_dispatch_module()
    args = dispatch_args()
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": "ok", "status": "ok", "result": "result.md",
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    row = module.terminal_contract(args, tmp_path, legacy, {"status": "ok", "spared": 2}, 1,
                                   tmp_path / "tasks/task-1/attempt-001")
    assert row["spared"] == 2


def test_terminal_attempt_does_not_promote_an_invalid_success_receipt(tmp_path: Path) -> None:
    module = load_dispatch_module()
    args = dispatch_args(alias="", effort="high")
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": "ok", "status": "failed", "result": {"path": "result.md"},
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    row = module.terminal_contract(
        args, tmp_path, legacy, {"status": "ok", "exit": None}, 1,
        tmp_path / "tasks/task-1/attempt-001",
    )
    assert row["status"] == "failed"
    assert row["error_class"] == "provider_error"


def test_terminal_attempt_retains_partial_transport_failure(tmp_path):
    module = load_dispatch_module()
    args = dispatch_args(alias="", effort="high")
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": "failed", "status": "failed", "result": {"path": "result.md"},
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    adapter = {"status": "failed", "result_partial": True, "evidence": {"signature": "provider_transport"}}
    row = module.terminal_contract(args, tmp_path, legacy, adapter, 1, tmp_path / "tasks/task-1/attempt-001")
    assert row["state"] == "terminal"
    assert row["status"] == "failed"
    assert row["error_class"] == "transient"
    assert row["result_partial"] is True
    assert "partial result" in module.render_digest(row)


@pytest.mark.parametrize("status,outcome,signature", [
    ("timed_out", "timeout", "wall_clock"),
    ("cancelled", "cancelled", "cancel_requested"),
])
def test_terminal_attempt_keeps_provider_stop_evidence(tmp_path, status, outcome, signature):
    module = load_dispatch_module()
    args = dispatch_args(alias="", effort="high")
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": outcome, "status": status, "process_error": outcome, "result": None,
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    adapter = {"status": status, "evidence": {"signature": signature, "excerpt": "provider diagnostic"}}
    row = module.terminal_contract(args, tmp_path, legacy, adapter, 1, tmp_path / "tasks/task-1/attempt-001")
    assert row["evidence"]["signature"] == signature
    assert row["evidence"]["excerpt"] == "provider diagnostic"


def test_terminal_attempt_measures_suffixed_capture_paths(tmp_path):
    module = load_dispatch_module()
    args = dispatch_args(alias="", effort="high")
    attempt = tmp_path / "tasks/task-1/attempt-001"
    attempt.mkdir(parents=True)
    (attempt / "events.jsonl").write_text("old events")
    (attempt / "stderr.log").write_text("old stderr")
    events = attempt / "events.jsonl.abcdef"
    stderr = attempt / "stderr.log.abcdef"
    events.write_text("new provider events")
    stderr.write_text("")
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": "ok", "status": "ok", "result": None,
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    adapter = {"status": "ok", "paths": {"events": str(events), "stderr": str(stderr)}}
    row = module.terminal_contract(args, tmp_path, legacy, adapter, 1, attempt)
    assert tmp_path / row["paths"]["events"] == events
    assert row["evidence"]["events_bytes"] == events.stat().st_size
    assert tmp_path / row["paths"]["stderr"] == stderr
    assert row["evidence"]["stderr_bytes"] == 0


@pytest.mark.parametrize("adapter_status,legacy_status,signature,source,expected", [
    ("failed", "failed", None, "observed", "provider_error"),
    ("ok", "timed_out", None, "observed", "timeout"),
    ("ok", "ok", None, "estimated", "none"),
    ("failed", "failed", "empty_output", None, "empty_output"),
])
def test_terminal_attempt_keeps_usage_and_classifies_final_status(
    tmp_path, adapter_status, legacy_status, signature, source, expected
):
    module = load_dispatch_module()
    args = dispatch_args(alias="", effort="high")
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": legacy_status, "status": legacy_status, "result": None,
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    adapter = {"status": adapter_status, "evidence": {"signature": signature},
               "error_class": "stale_adapter_class"}
    if source:
        adapter.update(context={"input_tokens": 10, "cache_write_tokens": 3, "reasoning_tokens": 2},
                       usage_source=source)
    row = module.terminal_contract(args, tmp_path, legacy, adapter, 1,
                                   tmp_path / "tasks/task-1/attempt-001")
    assert row["error_class"] == expected
    assert row["usage_source"] == (source or "missing")
    if source:
        assert row["context"] == adapter["context"]


def test_terminalised_confidential_run_stays_confidential_on_resume(tmp_path: Path) -> None:
    module = load_dispatch_module()
    args = dispatch_args(alias="", confidential=True)
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": "ok", "status": "ok", "result": "result.md",
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    provider = {"requested": {"adapter": "codex", "alias": "", "model": "fixture", "effort": "low"},
                "resolved_model": "fixture", "effort_applied": "low", "line": "Route: codex/fixture@low (openai; resolved)"}
    row = module.terminal_contract(args, tmp_path, legacy, {"status": "ok", "provenance": provider}, 1,
                                   tmp_path / "tasks/task-1/attempt-001")
    assert row["provenance"]["requested"]["confidential"] is True
    assert row["provenance"]["resolved_model"] == "fixture", "the provider's provenance still wins otherwise"
    run_dir = make_run(tmp_path, "confidential-resume")
    attempt_dir = run_dir / "tasks/task-1/attempt-001"
    attempt_dir.mkdir(parents=True)
    previous = {
        "run_id": "resume-me", "task_id": "task-1", "attempt": 1, "state": "terminal", "status": "ok",
        "mode": "read_only", "cwd": str(tmp_path), "worktree": None, "workspace": {"root": str(tmp_path)},
        "session_id": "saved-session", "provenance": row["provenance"],
        "applied": {"sandbox": "read-only", "network": None, "add_dirs": [], "capabilities": []},
        "paths": {"events": None}, "requested_route": {},
    }
    (attempt_dir / "attempt.json").write_text(json.dumps(previous), encoding="utf-8")
    resumed = dispatch_args(_profile="resume", run_dir=run_dir)
    module.prepare_resume(resumed)
    assert resumed.confidential is True


def test_prepare_resume_restores_previous_workspace_root(tmp_path: Path):
    module = load_dispatch_module()
    run_dir = make_run(tmp_path, "resume-root")
    attempt_dir = run_dir / "tasks/task-1/attempt-001"
    attempt_dir.mkdir(parents=True)
    workspace = tmp_path / "recorded-workspace"
    workspace.mkdir()
    previous = {
        "run_id": "resume-me", "task_id": "task-1", "attempt": 1, "state": "terminal",
        "delivery_run_id": "delivery:resume",
        "status": "ok", "mode": "read_only", "cwd": str(workspace / "src"), "worktree": None,
        "workspace": {"root": str(workspace)}, "session_id": "saved-session",
        "read_roots": [str(tmp_path / "other-project")],
        "provenance": {"requested": {"adapter": "codex"}, "resolved_model": "fixture", "effort_applied": ""},
        "applied": {"sandbox": "read-only", "network": None, "add_dirs": [],
                    "capabilities": ["browser", "postgres"]},
        "paths": {"events": None}, "requested_route": {},
    }
    (attempt_dir / "attempt.json").write_text(json.dumps(previous), encoding="utf-8")
    args = dispatch_args(_profile="resume", run_dir=run_dir)

    module.prepare_resume(args)

    assert args.workspace_root == workspace
    assert args.delivery_run_id == 'delivery:resume'
    assert args.read_roots == [str(tmp_path / "other-project")]
    assert args.capabilities == ["browser", "postgres"]


def test_prepare_resume_drops_missing_add_dir_with_warning(tmp_path: Path):
    module = load_dispatch_module()
    run_dir = make_run(tmp_path, "resume-add-dirs")
    attempt_dir = run_dir / "tasks/task-1/attempt-001"
    attempt_dir.mkdir(parents=True)
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    existing_dir = workspace / "existing"
    existing_dir.mkdir()
    removed_dir = workspace / "removed"
    previous = {
        "run_id": "resume-me", "task_id": "task-1", "attempt": 1, "state": "terminal",
        "status": "ok", "mode": "read_only", "cwd": str(workspace), "worktree": None,
        "workspace": {"root": str(workspace)}, "session_id": "saved-session",
        "read_roots": [],
        "provenance": {"requested": {"adapter": "codex"}, "resolved_model": "fixture", "effort_applied": ""},
        "applied": {"sandbox": "read-only", "network": None, "add_dirs": [str(existing_dir), str(removed_dir)],
                    "capabilities": []},
        "paths": {"events": None}, "requested_route": {},
    }
    (attempt_dir / "attempt.json").write_text(json.dumps(previous), encoding="utf-8")
    args = dispatch_args(_profile="resume", run_dir=run_dir)

    module.prepare_resume(args)

    assert args.add_dirs == [str(existing_dir)]
    assert any("removed" in w for w in getattr(args, "_resume_warnings", []))


def test_secret_scan_unavailable_fix_includes_failing_path_and_reason(tmp_path: Path):
    module = load_dispatch_module()
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    unreadable_dir = workspace / "unreadable"
    unreadable_dir.mkdir()
    unreadable_dir.chmod(0)
    try:
        tasks = [{
            "id": "task-1", "adapter": "codex", "model": "fixture",
            "prompt": "hello", "add_dirs": [str(unreadable_dir)],
        }]
        res = module.dispatch_preflight.preflight_tasks(tasks, workspace_root=workspace)
        assert res["status"] == "rejected"
        assert res["error"] == "secret_scan_unavailable"
        assert str(unreadable_dir) in res["fix"]
    finally:
        unreadable_dir.chmod(0o755)


def test_ordinary_single_dispatch_records_one_attempt_and_route_identity(tmp_path: Path) -> None:
    disable_memory_admission_floor(tmp_path)
    run_dir = make_run(tmp_path, "one")
    receipt_before = (run_dir / "RUN_RECEIPT.json").read_bytes()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_codex(bin_dir, body="cat >/dev/null\nprintf 'OK\\n'")
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"

    result = subprocess.run(
        [
            str(SCRIPT),
            "--run-dir",
            str(run_dir),
            "--task-id",
            "task-1",
            "--delivery-run-id",
            "delivery:1202",
            "--adapter",
            "codex",
            "--prompt-file",
            str(prompt),
            "--orchestrator-family",
            "openai",
            "--alias",
            "senior",
            "--role",
            "worker",
        ],
        cwd=tmp_path,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert result.returncode == 0, result.stderr + result.stdout
    receipt = json.loads(result.stdout)
    assert receipt["status"] == "ok"
    attempt = run_dir / "dispatch" / "tasks" / "task-1" / "attempt-001" / "attempt.json"
    assert attempt.exists()
    record = json.loads(attempt.read_text(encoding="utf-8"))
    assert record['delivery_run_id'] == 'delivery:1202'
    assert record["attempt_id"] == "attempt-001"
    assert record["requested_route"]["adapter"] == "codex"
    assert record["route"]["resolved_model"].startswith("gpt-")
    assert record["process"]["observed_exit"] is True
    assert record["process"]["exit_code"] == 0
    assert record["prompt"]["digest"] == "sha256:" + hashlib.sha256(prompt.read_bytes()).hexdigest()
    result_path = run_dir / record["result"]["path"]
    assert result_path.read_text(encoding="utf-8") == "OK\n"
    assert record["result"]["digest"] == "sha256:" + hashlib.sha256(result_path.read_bytes()).hexdigest()
    assert "task-1" in (run_dir / "MANIFEST.md").read_text(encoding="utf-8")
    sidecar = run_dir / "dispatch/tasks/task-1/attempt-001/attempt.sha256"
    expected_digest = hashlib.sha256(attempt.read_bytes()).hexdigest()
    assert sidecar.read_text(encoding="utf-8").strip() == f"sha256:{expected_digest}  attempt.json"
    assert receipt["attempt_digest"] == f"sha256:{expected_digest}"
    assert receipt["attempt_digest_path"].endswith("attempt.sha256")
    assert (run_dir / "RUN_RECEIPT.json").read_bytes() == receipt_before

    retry = subprocess.run([
        str(SCRIPT), '--run-dir', str(run_dir), '--task-id', 'task-1', '--adapter', 'codex',
        '--prompt-file', str(prompt), '--alias', 'senior', '--retry-of', 'attempt-001',
    ], cwd=tmp_path, env=env, text=True, capture_output=True)
    assert retry.returncode == 0, retry.stderr + retry.stdout
    retried = json.loads((attempt.parent.parent / 'attempt-002/attempt.json').read_text())
    assert retried['delivery_run_id'] == 'delivery:1202'
    changed = subprocess.run([
        str(SCRIPT), '--run-dir', str(run_dir), '--task-id', 'task-1', '--adapter', 'codex',
        '--prompt-file', str(prompt), '--alias', 'senior', '--retry-of', 'attempt-002',
        '--delivery-run-id', 'delivery:other',
    ], cwd=tmp_path, env=env, text=True, capture_output=True)
    assert changed.returncode == 2
    assert 'retry must keep delivery_run_id' in changed.stdout


@pytest.mark.parametrize("legacy", [False, True])
@pytest.mark.parametrize("floor", ['"invalid"', "-1", "101"])
def test_invalid_memory_floor_retains_failed_attempt_with_fix(tmp_path, monkeypatch, legacy, floor):
    run_dir = make_run(tmp_path, f"bad-floor-{legacy}-{floor}")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_codex(bin_dir, body='exit 99')
    monkeypatch.setenv("PATH", f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    policy = tmp_path / ".agents/fabric-policy.json"
    policy.parent.mkdir()
    policy.write_text('{"memory_floor_percent":{"read_only":' + floor + '}}')
    monkeypatch.chdir(tmp_path)
    module = load_dispatch_module()
    if legacy:
        adapter = tmp_path / "adapter"
        write_success_adapter(adapter)
        module.CF_DISPATCH = adapter
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "bad-floor", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/bad-floor/attempt-001/attempt.json").read_text())
    state = json.loads((run_dir / "tasks/bad-floor/attempt-001/attempt.json").read_text())
    assert attempt["status"] == state["status"] == "failed"
    assert attempt["usage_source"] == state["usage_source"] == "missing"
    assert attempt["error_class"] == state["error_class"] == "provider_error"
    assert state["fix"] == "Set .agents/fabric-policy.json memory_floor_percent to numbers from 0 to 100."
    assert "memory_floor_percent" in attempt["process_error"]


@pytest.mark.parametrize("legacy", [False, True])
def test_memory_wait_expiry_is_typed_and_visible(tmp_path, monkeypatch, legacy):
    run_dir = make_run(tmp_path, f"memory-expiry-{legacy}")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_codex(bin_dir, body='exit 99')
    monkeypatch.setenv("PATH", f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    module = load_dispatch_module()
    monkeypatch.setattr(module.memory_admission, "available_memory_mb", lambda: (640, 16384))
    original_admit = module.memory_admission.admit
    admission_timeouts = []
    def track_admission_timeout(*args, **kwargs):
        admission_timeouts.append(kwargs.get("timeout_seconds"))
        return original_admit(*args, **kwargs)
    monkeypatch.setattr(module.memory_admission, "admit", track_admission_timeout)
    monkeypatch.chdir(tmp_path)
    if legacy:
        adapter = tmp_path / "adapter"
        write_success_adapter(adapter)
        module.CF_DISPATCH = adapter
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "memory-expiry", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        "--timeout-seconds", "0.01",
    ])
    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/memory-expiry/attempt-001/attempt.json").read_text())
    state = json.loads((run_dir / "tasks/memory-expiry/attempt-001/attempt.json").read_text())
    assert attempt["status"] == state["status"] == "failed"
    assert attempt["outcome"] == state["error"] == "memory_unavailable"
    assert admission_timeouts == [0.01]
    assert "lower the mode's memory_floor_percent" in state["fix"]
    assert "memory_unavailable" in state["digest"]


def test_queued_receipt_is_published_where_admission_scans(tmp_path, monkeypatch):
    run_dir = make_run(tmp_path, "runs/20260930-1200-dispatch-queue")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_codex(bin_dir, body='exit 99')
    monkeypatch.setenv("PATH", f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    module = load_dispatch_module()
    admission = module.memory_admission
    monkeypatch.setattr(admission, "available_memory_mb", lambda: (640, 16384))
    original_admit = admission.admit
    published = []

    def observe_queue(on_wait, *args, **kwargs):
        def waiting(reason):
            on_wait(reason)
            published.extend((path, json.loads(path.read_text()), kwargs["queued_since"])
                             for path in admission.queued_receipts(kwargs["queue_root"]))
        return original_admit(waiting, *args, **kwargs)

    monkeypatch.setattr(admission, "admit", observe_queue)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "queue-wiring", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        "--timeout-seconds", "0.01",
    ])
    assert module.dispatch(args) == 1
    assert published
    path, row, queued_since = published[0]
    assert path == run_dir / "tasks/queue-wiring/attempt-001/attempt.json"
    assert row["state"] == "queued" and row["queue_reason"] == "memory"
    assert row["timing"]["queued_since"] == queued_since
    assert row["admission"] == {
        "owner_pid": os.getpid(),
        "owner_start_epoch": admission._start_epoch(os.getpid()),
        "floor_percent": admission.floor_percent(tmp_path, args.access_mode),
    }


def test_opencode_explicit_model_receipt_drops_implied_alias(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "opencode-explicit-model")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "opencode",
        """#!/usr/bin/env bash
        printf '{\"type\":\"text\",\"part\":{\"text\":\"OK\"}}\\n'
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    env["PROVENANT_NO_OS_CONFINEMENT"] = "1"

    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "task-1",
         "--adapter", "opencode", "--prompt-file", str(prompt),
         "--orchestrator-family", "openai", "--model", "mimo", "--role", "worker"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    assert result.returncode == 0, result.stderr + result.stdout
    attempt = run_dir / "dispatch/tasks/task-1/attempt-001"
    adapter_receipt = json.loads((attempt / "adapter-receipt.json").read_text(encoding="utf-8"))
    record = json.loads(
        (run_dir / "tasks/task-1/attempt-001/attempt.json").read_text(encoding="utf-8")
    )
    assert adapter_receipt["route_alias"] == ""
    assert record["provenance"]["requested"]["alias"] == ""


def test_batch_preflight_does_not_invent_an_explicit_alias_for_model_routes(tmp_path: Path, monkeypatch) -> None:
    module = load_dispatch_module()
    monkeypatch.chdir(tmp_path)
    commands: list[list[str]] = []

    def resolve(command, **_kwargs):
        commands.append(command)
        explicit_alias = "--alias" in command
        has_model = "--model" in command
        notes = ["alias and model both supplied; model won"] if explicit_alias and has_model else []
        return subprocess.CompletedProcess(command, 0, json.dumps({
            "status": "ok", "adapter": "codex", "resolved_model": "gpt-6-luna",
            "provider_family": "openai", "execution_intent": "ordinary", "notes": notes,
        }), "")

    monkeypatch.setattr(module.subprocess, "run", resolve)
    result = module.dispatch_preflight.preflight_tasks([
        {"id": "model-only", "adapter": "codex", "model": "gpt-6-luna", "prompt": "hi"},
        {"id": "explicit-both", "adapter": "codex", "alias": "senior", "model": "gpt-6-luna", "prompt": "hi"},
    ])

    model_only, explicit_both = result["routes"]
    resolve_commands = [command for command in commands if "resolve" in command[:3]]
    assert len(resolve_commands) == 2
    assert "--alias" not in resolve_commands[0]
    assert "--model" in resolve_commands[0]
    assert model_only["notes"] == []
    assert "--alias" in resolve_commands[1]
    assert explicit_both["notes"] == ["alias and model both supplied; model won"]


@pytest.mark.parametrize(("alias", "model", "alias_supplied", "expected_alias"), [
    ("principal", "mimo", False, ""),
    ("senior", "gpt-6-luna", True, "senior"),
], ids=[
    "test_explicit_model_receipt_does_not_record_an_implied_alias",
    "test_explicit_model_and_alias_are_both_recorded_in_receipt",
])
def test_explicit_model_receipt_does_not_record_an_implied_alias(
    tmp_path, alias, model, alias_supplied, expected_alias,
):
    mod = load_dispatch_module()
    args = dispatch_args(_profile="receipt", alias=alias, model=model, alias_supplied=alias_supplied)
    row = mod.contract_row(args, tmp_path, 1, tmp_path / "attempt", {}, "now")
    assert row["provenance"]["requested"]["alias"] == expected_alias


def test_config_fallback_detail_is_persisted_in_the_attempt_record(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    args = dispatch_args(_profile="receipt")
    fallback = {"from_host": "hosts.json", "reason": "invalid_config", "detail": "Response deadline must be positive and finite"}
    monkeypatch.setenv("PROVENANT_PLACEMENT_FALLBACK", json.dumps(fallback))
    plan = {}
    row = mod.contract_row(args, tmp_path, 1, tmp_path / "attempt", plan, "now")
    assert row["placement_fallback"] == fallback
    assert plan["warnings"] == ["local fallback from hosts.json: invalid_config: Response deadline must be positive and finite"]


def test_workspace_identity_keeps_root_and_records_provider_cwd(tmp_path):
    mod = load_dispatch_module()
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    identity = mod.writer_lease.workspace_identity(root, cwd)
    assert identity["cwd"] == str(cwd)
    assert identity["root"] == str(root)


def test_workspace_identity_canonicalizes_fallback_root(tmp_path):
    mod = load_dispatch_module()
    real_root = tmp_path / "workspace"
    (real_root / "sub").mkdir(parents=True)
    workspace_link = tmp_path / "workspace-link"
    workspace_link.symlink_to(real_root, target_is_directory=True)

    identity = mod.writer_lease.workspace_identity(workspace_link, workspace_link / "sub")

    assert identity["root"] == str(real_root.resolve())
    assert identity["cwd"] == str((real_root / "sub").resolve())


def test_workspace_identity_keeps_canonical_caller_root_inside_git_checkout(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    repo = tmp_path / "repo"
    workspace = repo / "workspace"
    provider_cwd = workspace / "sub"
    provider_cwd.mkdir(parents=True)
    monkeypatch.setattr(
        mod.writer_lease.subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(
            args[0], 0,
            f"{repo}\n{'a' * 40}\n" if args[0][1:3] == ["rev-parse", "--show-toplevel"] else "",
            "",
        ),
    )

    identity = mod.writer_lease.workspace_identity(workspace, provider_cwd)

    assert identity["root"] == str(workspace.resolve())
    assert identity["cwd"] == str(provider_cwd.resolve())


def test_agy_git_evidence_is_copied_into_attempt_and_bound_to_prompt(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "agy-evidence")
    source = run_dir / "evidence" / "git-evidence.md"
    source.parent.mkdir()
    source.write_text(
        json.dumps({
            "schema_version": 1,
            "record_type": "provenant-git-evidence",
            "repository": str(tmp_path / "repo"),
            "git_root": str(tmp_path / "repo"),
            "head": "a" * 40,
            "diff_base": "a" * 40,
            "working_tree": "dirty",
            "diff_from": "HEAD",
            "paths": ["file.txt"],
            "encoding": "utf-8-replacement",
        }) + "\n--- status ---\n M file.txt\n--- diff ---\n+after\n" + ("x" * 2_000_000),
        encoding="utf-8",
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Review the supplied change.\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    received = tmp_path / "agy-argv.json"
    write_executable(bin_dir / "agy", f"""#!/usr/bin/env python3
import json, sys
from pathlib import Path
if sys.argv[1:] == ["models"]:
    print("gemini-3.8-flash-medium")
else:
    Path({str(received)!r}).write_text(json.dumps(sys.argv[1:]))
    print(json.dumps({{"status": "SUCCESS", "response": "OK"}}))
""")
    monkeypatch.setenv("PATH", f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    module = load_dispatch_module()
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("CF_DISPATCH_AGY_ADD_DIR", str(tmp_path / "unrelated"))
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "agy", "--adapter", "agy",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "reviewer",
        "--git-evidence", str(source),
    ])

    assert module.dispatch(args) == 0
    attempt = json.loads((run_dir / "dispatch/tasks/agy/attempt-001/attempt.json").read_text())
    evidence = attempt["git_evidence"]
    evidence_path = run_dir / evidence["path"]
    assert evidence_path.parent.name == "evidence"
    assert evidence_path != source
    assert evidence_path.read_text(encoding="utf-8") == source.read_text(encoding="utf-8")
    assert evidence["digest"].startswith("sha256:")
    assert evidence["checkout"]["repository"] == str(tmp_path / "repo")
    assert evidence["checkout"]["git_root"] == str(tmp_path / "repo")
    assert evidence["checkout"]["head"] == "a" * 40
    assert evidence["checkout"]["working_tree"] == "dirty"
    assert evidence["checkout"]["diff_from"] == "HEAD"
    assert evidence["checkout"]["paths"] == ["file.txt"]
    assert "read the supplied evidence files" in (run_dir / attempt["prompt"]["path"]).read_text().lower()
    argv = json.loads(received.read_text())
    effective_prompt = argv[argv.index("--print") + 1]
    assert effective_prompt.startswith(f"Workspace root: {tmp_path.resolve()}\n")
    assert str(evidence_path) in effective_prompt
    assert argv[argv.index("--add-dir") + 1] == str(evidence_path.parent)
    assert "Use file-reading tools only; do not invoke shell or Git" in effective_prompt
    assert effective_prompt.endswith(prompt.read_text())
    module.run_records.reconcile_manifest(run_dir)


def test_agy_git_evidence_retains_denial_without_accepting_result(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "agy-denial")
    source = run_dir / "evidence.md"
    source.write_text(
        json.dumps({
            "schema_version": 1,
            "record_type": "provenant-git-evidence",
            "repository": str(tmp_path), "git_root": str(tmp_path), "head": "b" * 40,
            "diff_base": "b" * 40, "working_tree": "clean", "diff_from": "HEAD", "paths": [],
            "encoding": "utf-8-replacement",
        }) + "\n--- status ---\n--- diff ---\n",
        encoding="utf-8",
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Review.\n", encoding="utf-8")
    adapter = tmp_path / "agy-adapter-denied"
    write_evidence_adapter(adapter, status="permission_denied")
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "deny", "--adapter", "agy",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "reviewer",
        "--git-evidence", str(source),
    ])

    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/deny/attempt-001/attempt.json").read_text())
    assert attempt["status"] == "failed"
    assert attempt["outcome"] == "permission_denied"
    assert attempt["git_evidence"]["path"].startswith("dispatch/tasks/deny/attempt-001/")


def test_agy_git_evidence_rejects_symlink_source(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "agy-symlink")
    source = run_dir / "evidence.md"
    source.write_text("not a packet\n", encoding="utf-8")
    alias = run_dir / "evidence-alias.md"
    alias.symlink_to(source)
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Review.\n", encoding="utf-8")
    module = load_dispatch_module()
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "symlink", "--adapter", "agy",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "reviewer",
        "--git-evidence", str(alias),
    ])

    assert module.dispatch(args) == 2
    assert not (run_dir / "dispatch/tasks/symlink/attempt-001/evidence/git-evidence.md").exists()


def test_valid_worker_question_envelope_is_retained_as_blocked_attempt(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "question-envelope")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Need a source choice\n", encoding="utf-8")
    adapter = tmp_path / "question-adapter"
    write_question_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "question", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    attempt = json.loads(
        (run_dir / "dispatch/tasks/question/attempt-001/attempt.json").read_text(encoding="utf-8")
    )
    assert attempt["status"] == "blocked"
    assert attempt["outcome"] == "question"
    assert attempt["failure_code"] == "needs_input"
    assert attempt["question"] == {"code": "needs_input", "prompt": "Which source should I use?"}
    assert attempt["process"]["observed_exit"] is True
    assert attempt["process"]["exit_code"] == 0


@pytest.mark.parametrize(
    ("result", "status", "outcome"),
    [
        ("Do you agree?\n", "ok", "ok"),
        ("```json\n{\"schema_version\":1,\"record_type\":\"provenant-worker-terminal\",\"classification\":\"question\",\"question\":{\"code\":\"needs_input\",\"prompt\":\"x\"}}\n```\n", "ok", "ok"),
        ('{"record_type":"other","question":"quoted?"}\n', "ok", "ok"),
        ('{"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"x"},"extra":true}\n', "failed", "terminal_envelope_invalid"),
        ('{"schema_version":1,"record_type":"provenant-worker-terminal","classification":"complete","question":{"code":"needs_input","prompt":"x"}}\n', "failed", "terminal_envelope_invalid"),
        ('{"schema_version":1,"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"x","extra":true}}\n', "failed", "terminal_envelope_invalid"),
        ('{"schema_version":true,"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"x"}}\n', "failed", "terminal_envelope_invalid"),
        ('{"schema_version":1.0,"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"x"}}\n', "failed", "terminal_envelope_invalid"),
        ('{"schema_version":1,"record_type":"provenant-worker-terminal","record_type":"other","classification":"question","question":{"code":"needs_input","prompt":"x"}}\n', "failed", "terminal_envelope_invalid"),
        ('{"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":""}}\n', "failed", "terminal_envelope_invalid"),
        ('{"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":null}}\n', "failed", "terminal_envelope_invalid"),
        ('{"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"a\\u0000b"}}\n', "failed", "terminal_envelope_invalid"),
    ],
)
def test_worker_question_detection_is_exact_and_fail_closed(
    tmp_path: Path, monkeypatch, result: str, status: str, outcome: str
) -> None:
    run_dir = make_run(tmp_path, "envelope-case")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prompt\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_question_adapter(adapter, result=result)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "case", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == (0 if status == "ok" else 1)
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == status
    assert attempt["outcome"] == outcome
    if status == "ok":
        assert "question" not in attempt


@pytest.mark.parametrize(("result", "status", "outcome"), [
    (None, "blocked", "question"),
    ('{"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"x"},"extra":true}\n',
     "failed", "terminal_envelope_invalid"),
    ("plain result\n", "quarantined", "quarantined"),
], ids=["question", "invalid-envelope", "plain"])
def test_quarantined_lane_gets_the_same_question_and_envelope_checks(tmp_path: Path, monkeypatch, result, status, outcome) -> None:
    run_dir = make_run(tmp_path, "quarantine-envelope")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prompt\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_question_adapter(adapter, result=result)
    adapter.write_text(adapter.read_text().replace(
        '"status":"ok"', '"status":"quarantined","landable":false,"quarantine":{"reason":"protected_instructions_changed","patches":[],"restored":true}'))
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "case", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    module.dispatch(args)
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == status
    assert attempt["outcome"] == outcome


def test_valid_worker_question_cannot_override_nonzero_provider_exit(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "question-nonzero")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prompt\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_question_adapter(adapter, exit_code=7)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "case", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == "failed"
    assert attempt["process"]["exit_code"] == 7
    assert "question" not in attempt


def test_worker_question_prompt_size_is_bounded(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "question-too-large")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prompt\n", encoding="utf-8")
    result = json.dumps({
        "schema_version": 1,
        "record_type": "provenant-worker-terminal",
        "classification": "question",
        "question": {"code": "needs_input", "prompt": "x" * 4097},
    })
    adapter = tmp_path / "adapter"
    write_question_adapter(adapter, result=result)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "case", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["outcome"] == "terminal_envelope_invalid"


def test_worker_question_candidate_is_bounded_and_digest_bound(tmp_path: Path) -> None:
    module = load_dispatch_module()
    result = tmp_path / "result.md"
    envelope = json.dumps({
        "schema_version": 1,
        "record_type": "provenant-worker-terminal",
        "classification": "question",
        "question": {"code": "needs_input", "prompt": "Which source?"},
    }).encode() + b"\n"
    result.write_bytes(envelope)

    with pytest.raises(module.run_records.TerminalEnvelopeIntegrityError):
        module.run_records.worker_question_envelope(result, "sha256:not-the-result")
    assert module.run_records.worker_question_envelope(result, module.run_records.digest(result)) == {
        "code": "needs_input", "prompt": "Which source?"
    }
    result.write_bytes(envelope + b"x" * (module.run_records.MAX_WORKER_TERMINAL_ENVELOPE_BYTES + 1))
    assert module.run_records.worker_question_envelope(result, module.run_records.digest(result)) is None


def test_valid_maximum_astral_worker_question_envelope_is_blocked(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "question-astral")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prompt\n", encoding="utf-8")
    result = json.dumps({
        "schema_version": 1,
        "record_type": "provenant-worker-terminal",
        "classification": "question",
        "question": {"code": "needs_input", "prompt": "😀" * 4096},
    }, ensure_ascii=False)
    adapter = tmp_path / "adapter"
    write_question_adapter(adapter, result=result)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "case", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == "blocked"
    assert len(attempt["question"]["prompt"]) == 4096


def test_dispatch_fails_when_terminal_candidate_cannot_be_safely_reopened(
    tmp_path: Path, monkeypatch
) -> None:
    run_dir = make_run(tmp_path, "question-reread-failure")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prompt\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_question_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    real_open = module.os.open

    def refuse_result_open(path, flags, *args, **kwargs):
        if Path(path).name == "result.md" and flags & getattr(module.os, "O_NOFOLLOW", 0):
            raise OSError("result replaced during terminal validation")
        return real_open(path, flags, *args, **kwargs)

    monkeypatch.setattr(module.os, "open", refuse_result_open)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "case", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == "failed"
    assert attempt["outcome"] == "result_integrity_error"


def test_ordinary_dispatch_without_lead_family_is_not_certification(tmp_path: Path) -> None:
    task_id = "no-family"
    run_dir = make_run(tmp_path, task_id)
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_codex(bin_dir, body="cat >/dev/null\nprintf 'OK\\n'")
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", task_id,
         "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    record = json.loads(result.stdout)
    assert record["route"]["orchestrator_family"] == ""
    assert record["route"]["cross_family"] is False
    assert record["route"]["certification_eligible"] is False


def test_attempt_rows_are_accepted_by_existing_finalizer(tmp_path: Path) -> None:
    task_id = "finalizer"
    run_dir = make_run(tmp_path, task_id)
    prompt = tmp_path / "prompt.md"
    prompt.write_text("finalizer\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_codex(bin_dir, body="cat >/dev/null\nprintf 'OK\\n'")
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", task_id,
         "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    finalized = subprocess.run(
        [str(FINALIZE), str(run_dir), "--status", "failed", "--reason", "dispatch test"],
        cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert finalized.returncode == 0, finalized.stderr
    assert json.loads((run_dir / "RUN_RECEIPT.json").read_text())["status"] == "failed"
    rejected = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "after-close",
         "--adapter", "codex", "--prompt-file", str(prompt),
         "--alias", "senior", "--role", "worker"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert rejected.returncode == 2
    assert json.loads(rejected.stdout)["status"] == "run_custody_closed"
    assert not (run_dir / "dispatch/tasks/after-close").exists()


def test_dispatch_owner_keeps_lifecycle_risk_separate_from_model_override(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "separate-route-metadata")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Synthesis\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "claude",
        """#!/usr/bin/env bash
        cat >/dev/null
        printf 'FABLE OWNER OK\n'
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(tmp_path / ".agents")

    result = subprocess.run(
        [
            str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "fable",
            "--adapter", "claude", "--prompt-file", str(prompt),
            "--model", "claude-fable-5-1", "--role", "synthesis",
            "--risk-tier", "routine", "--model-override-tier", "crucial",
        ],
        cwd=tmp_path, env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    assert result.returncode == 0, result.stderr + result.stdout
    attempt_path = run_dir / "dispatch/tasks/fable/attempt-001/attempt.json"
    attempt = json.loads(attempt_path.read_text(encoding="utf-8"))
    assert attempt["requested_route"]["risk_tier"] == "routine"
    assert attempt["requested_route"]["model_override_tier"] == "crucial"
    assert attempt["route"]["risk_tier"] == "routine"
    assert attempt["route"]["model_override_tier"] == "crucial"
    assert attempt["route"]["resolved_model"] == "claude-fable-5-1"
    assert attempt["route"]["policy_override"] == "crucial-claude-fable-5-1-synthesis-adjudication"


def test_batch_child_defers_shared_manifest_append(tmp_path: Path, monkeypatch) -> None:
    disable_memory_admission_floor(tmp_path)
    run_dir = make_run(tmp_path, "batch-child")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("batch child\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    competing = tmp_path / 'competing.json'
    import shlex
    command = [sys.executable, str(SCRIPT), '--run-dir', str(run_dir),
               '--task-id', 'competing', '--adapter', 'codex', '--model', 'fixture',
               '--prompt-file', str(prompt)]
    adapter.write_text(adapter.read_text().replace(
        'while [ "$#" -gt 0 ]; do',
        shlex.join(command) + ' > ' + shlex.quote(str(competing)) + '\nwhile [ "$#" -gt 0 ]; do'))
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    acquire = module.acquire_run_custody

    def acquire_after_another_owner(run):
        # A previous owner can finish between initialisation and acquisition.
        owner_path = run / 'dispatch-owner.json'
        owner = json.loads(owner_path.read_text())
        owner['owner_pid'] = os.getppid()
        owner['run_token'] = 'intervening-owner-token'
        owner_path.write_text(json.dumps(owner))
        return acquire(run)

    monkeypatch.setattr(module, 'acquire_run_custody', acquire_after_another_owner)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "deferred", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "junior", "--role", "worker",
        "--risk-tier", "substantial", "--reviewer-id", "reviewer-1", "--effort", "high", "--batch-child",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 0
    assert "dispatch-deferred" not in (run_dir / "MANIFEST.md").read_text(encoding="utf-8")
    attempt = json.loads((run_dir / "dispatch/tasks/deferred/attempt-001/attempt.json").read_text(encoding="utf-8"))
    assert attempt["requested_route"]["risk_tier"] == "substantial"
    assert attempt["requested_route"]["reviewer_id"] == "reviewer-1"
    assert attempt["requested_route"]["effort"] == "high"

    assert json.loads(competing.read_text())['status'] == 'run_custody_busy'
    assert not (run_dir / 'tasks/competing').exists()
    assert json.loads((run_dir / 'dispatch-owner.json').read_text())['owner_pid'] == os.getpid()


def test_prompt_stdin_is_retained_by_dispatch_owner(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "stdin")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "stdin-task", "--adapter", "codex",
        "--prompt-stdin", "--alias", "junior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(sys, "stdin", io.TextIOWrapper(io.BytesIO(b"stdin prompt\n")))

    assert module.dispatch(args) == 0
    attempt = run_dir / "dispatch/tasks/stdin-task/attempt-001"
    assert (attempt / "prompt.md").read_bytes() == b"stdin prompt\n"


def test_route_failure_is_typed_and_provider_is_not_invoked(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "route")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("route failure\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    invoked = tmp_path / "invoked"
    fake_codex(bin_dir, body=f"touch {invoked}\nexit 9\n")
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"

    result = subprocess.run(
        [
            str(SCRIPT),
            "--run-dir", str(run_dir), "--task-id", "route-failure",
            "--adapter", "codex", "--prompt-file", str(prompt),
            "--alias", "does-not-exist", "--role", "worker",
            "--intent", "assurance", "--orchestrator-family", "anthropic",
        ], cwd=tmp_path, env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["failure_code"] == "unknown_alias"
    assert record["route"]["status"] == "unknown_alias"
    assert not invoked.exists()
    assert record["process"]["observed_exit"] is True
    contract = json.loads((run_dir / "tasks/route-failure/attempt-001/attempt.json").read_text())
    assert contract["status"] == "rejected"
    assert contract["evidence"]["signature"] == "unknown_alias"
    assert "unknown_alias" in contract["fix"] and "inspect stderr" not in contract["digest"]


def test_nonzero_provider_exit_is_recorded_without_substitution(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "exit")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("provider failure\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_codex(bin_dir, models={'gpt-6.1-sol': ('high',), 'gpt-6-luna': ('high',)}, body='cat >/dev/null\necho "provider failed" >&2\nexit 9')
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"

    result = subprocess.run(
        [
            str(SCRIPT),
            "--run-dir", str(run_dir), "--task-id", "provider-failure",
            "--adapter", "codex", "--prompt-file", str(prompt),
            "--alias", "senior", "--role", "worker",
        ], cwd=tmp_path, env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["status"] == "failed"
    assert record["failure_code"] == "failed"
    assert record["process"]["exit_code"] != 0
    assert record["process"]["observed_exit"] is True
    assert record["route"]["substitution"] == ""


def test_malformed_adapter_receipt_is_fail_closed(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "malformed")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("malformed adapter\n", encoding="utf-8")
    adapter = tmp_path / "fake-adapter"
    write_executable(
        adapter,
        """#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in --out) out="$2"; shift 2;; *) shift;; esac
        done
        printf 'result\\n' > "$out"
        printf 'not-json\\n'
        """,
    )
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "malformed", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 1
    record = json.loads((run_dir / "dispatch/tasks/malformed/attempt-001/attempt.json").read_text())
    assert record["failure_code"] == "adapter_receipt_invalid"
    assert record["status"] == "failed"
    assert record["process"]["observed_exit"] is True


def test_incomplete_success_receipt_is_fail_closed(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "incomplete-success")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("incomplete success\n", encoding="utf-8")
    adapter = tmp_path / "fake-incomplete-success"
    write_executable(
        adapter,
        """#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in --out) out="$2"; shift 2;; *) shift;; esac
        done
        printf 'result\n' > "$out"
        printf '{"status":"ok"}\n'
        """,
    )
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "incomplete-success", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    record = json.loads(
        (run_dir / "dispatch/tasks/incomplete-success/attempt-001/attempt.json").read_text()
    )
    assert record["failure_code"] == "adapter_receipt_invalid"


def test_reentry_rejects_tampered_success_adapter_receipt(tmp_path: Path, monkeypatch) -> None:
    """Manifest repair is intentionally narrow; re-entry still revalidates success."""
    run_dir = make_run(tmp_path, "reentry-invalid-success")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("revalidate retained success\n", encoding="utf-8")
    adapter = tmp_path / "success-adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "reentry", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    assert module.dispatch(args) == 0
    attempt = run_dir / "dispatch/tasks/reentry/attempt-001/attempt.json"
    receipt = attempt.with_name("adapter-receipt.json")
    receipt.write_text("{}\n", encoding="utf-8")
    record = json.loads(attempt.read_text(encoding="utf-8"))
    record["route"]["adapter_receipt"]["digest"] = "sha256:" + hashlib.sha256(receipt.read_bytes()).hexdigest()
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    attempt.with_name("attempt.sha256").write_text(
        "sha256:" + hashlib.sha256(attempt.read_bytes()).hexdigest() + "  attempt.json\n",
        encoding="utf-8",
    )

    with pytest.raises(module.run_records.AttemptEvidenceError, match="successful adapter receipt is invalid"):
        module.run_records.reconcile_manifest(run_dir)


def test_typed_adapter_auth_and_missing_tool_outcomes_are_preserved(tmp_path: Path, monkeypatch) -> None:
    module = load_dispatch_module()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("typed adapter failure\n", encoding="utf-8")
    for name, status, exit_code in (
        ("auth", "auth_or_quota_error", 1),
        ("missing", "tool_not_found", 127),
    ):
        run_dir = make_run(tmp_path, name)
        adapter = tmp_path / f"fake-{name}"
        write_executable(
            adapter,
            f"""#!/usr/bin/env bash
            printf '{{"status":"{status}","substitution":""}}'
            exit {exit_code}
            """,
        )
        monkeypatch.setattr(module, "CF_DISPATCH", adapter)
        args = module.parser().parse_args([
            "--run-dir", str(run_dir), "--task-id", name, "--adapter", "codex",
            "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        ])
        monkeypatch.chdir(tmp_path)
        assert module.dispatch(args) == 1
        record = json.loads((run_dir / f"dispatch/tasks/{name}/attempt-001/attempt.json").read_text())
        assert record["failure_code"] == status
        assert record["route"]["status"] == status
        assert record["process"]["observed_exit"] is True


def test_timeout_records_reaped_exit(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "timeout")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("timeout\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "codex",
        """#!/usr/bin/env bash
        if [ "$1" = "mcp" ]; then printf '[]'; exit 0; fi
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        sleep 10
        """,
    )
    env = os.environ.copy()
    env["PROVENANT_PREFLIGHT_ROUTES"] = json.dumps({"timeout": {"adapter": "codex", "alias": "senior", "resolved_model": "gpt-6-luna", "effort": "high"}})
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "timeout", "--adapter", "codex",
         "--prompt-file", str(prompt), "--model", "gpt-6-luna", "--role", "worker", "--timeout", "0.1"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["status"] == "timed_out"
    assert record["failure_code"] == "timeout"
    assert record["route"]["resolved_model"] == "gpt-6-luna"
    assert record["route"]["effort"] == "high"
    assert record["process"]["observed_exit"] is True
    assert record["process"]["exit_code"] is not None


def test_sigterm_interrupts_and_reaps_provider_group(tmp_path: Path) -> None:
    disable_memory_admission_floor(tmp_path)
    run_dir = make_run(tmp_path, "cancel")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("cancel\n", encoding="utf-8")
    provider_pid_path = tmp_path / "provider.pid"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "codex",
        """#!/usr/bin/env bash
        if [ "$1" = "--version" ]; then echo 'codex-cli 1.0'; exit 0; fi
        if [ "$1" = "--help" ]; then exit 0; fi
        if [ "$1" = "mcp" ]; then printf '[]'; exit 0; fi
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-6.1-sol","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        printf '%s\n' "$$" > "$PROBE_PID_PATH"
        sleep 30
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    env["PROBE_PID_PATH"] = str(provider_pid_path)
    process = subprocess.Popen(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "cancel",
         "--adapter", "codex", "--prompt-file", str(prompt),
         "--alias", "senior", "--role", "worker", "--timeout", "30"],
        cwd=tmp_path, env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    deadline = time.monotonic() + 5
    while not provider_pid_path.exists() and time.monotonic() < deadline:
        time.sleep(0.02)
    assert provider_pid_path.exists()
    provider_pid = int(provider_pid_path.read_text().strip())

    process.send_signal(signal.SIGTERM)
    stdout, stderr = process.communicate(timeout=5)

    assert process.returncode == 1, stderr + stdout
    record = json.loads(stdout)
    assert record["status"] == "failed"
    attempt = json.loads((run_dir / "dispatch/tasks/cancel/attempt-001/attempt.json").read_text())
    assert attempt["status"] == "failed"
    assert attempt["failure_code"] == "interrupted"
    assert record["process"]["observed_exit"] is True
    with pytest.raises(ProcessLookupError):
        os.kill(provider_pid, 0)


def test_late_signal_after_provider_exit_preserves_attempt_publication(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "late-signal")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("late signal\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    driver = tmp_path / "late-signal-driver.py"
    driver.write_text(textwrap.dedent(f"""
        import importlib.util, os, signal, sys
        spec = importlib.util.spec_from_file_location("late_signal_dispatch", {str(SCRIPT)!r})
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.CF_DISPATCH = __import__('pathlib').Path({str(adapter)!r})
        original = module.run_records.write_owned
        fired = False
        def publish(run_dir, path, content):
            global fired
            original(run_dir, path, content)
            if path == run_dir / "dispatch/tasks/late/attempt-001/attempt.json" and not fired:
                fired = True
                os.kill(os.getpid(), signal.SIGTERM)
        module.run_records.write_owned = publish
        raise SystemExit(module.dispatch(module.parser().parse_args(sys.argv[1:])))
    """), encoding="utf-8")
    result = subprocess.run([
        sys.executable, str(driver), "--run-dir", str(run_dir), "--task-id", "late",
        "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ], cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    assert result.returncode == 0, result.stderr + result.stdout
    record = json.loads(result.stdout)
    assert record["status"] == "ok"
    assert record["process"]["observed_exit"] is True
    assert (run_dir / "dispatch/tasks/late/attempt-001/attempt.json").is_file()


def test_signal_during_popen_return_reaps_provider_and_publishes_cancelled_attempt(tmp_path: Path) -> None:
    """A signal after spawn but before Popen returns cannot orphan the provider."""
    run_dir = make_run(tmp_path, "popen-window")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("popen window\n", encoding="utf-8")
    provider_pid_path = tmp_path / "provider.pid"
    adapter = tmp_path / "term-ignoring-adapter"
    write_executable(
        adapter,
        f"""#!/usr/bin/env bash
        printf '%s' "$$" > "{provider_pid_path}"
        trap '' TERM HUP
        while :; do sleep 1; done
        """,
    )
    driver = tmp_path / "popen-window-driver.py"
    driver.write_text(textwrap.dedent(f"""
        import importlib.util, os, pathlib, signal, sys, time
        spec = importlib.util.spec_from_file_location("popen_window_dispatch", {str(SCRIPT)!r})
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.CF_DISPATCH = pathlib.Path({str(adapter)!r})
        original_popen = module.subprocess.Popen
        def delayed_popen(*args, **kwargs):
            process = original_popen(*args, **kwargs)
            command = args[0] if args else kwargs.get("args")
            if not command or command[0] != str(module.CF_DISPATCH):
                return process
            pid_path = pathlib.Path({str(provider_pid_path)!r})
            deadline = time.monotonic() + 5
            while not pid_path.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            os.kill(os.getpid(), signal.SIGTERM)
            time.sleep(0.05)
            return process
        module.subprocess.Popen = delayed_popen
        raise SystemExit(module.dispatch(module.parser().parse_args(sys.argv[1:])))
    """), encoding="utf-8")
    result = subprocess.run([
        sys.executable, str(driver), "--run-dir", str(run_dir), "--task-id", "window",
        "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ], cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)

    assert result.returncode == 1, result.stderr + result.stdout
    record = json.loads(result.stdout)
    assert record["status"] == "cancelled"
    assert record["process"]["observed_exit"] is True
    attempt_path = run_dir / "dispatch/tasks/window/attempt-001/attempt.json"
    assert attempt_path.is_file()
    assert json.loads(attempt_path.read_text())["status"] == "cancelled"
    provider_pid = int(provider_pid_path.read_text())
    with pytest.raises(ProcessLookupError):
        os.kill(provider_pid, 0)


def test_external_task_cancel_reaps_only_owned_provider_group(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "external-cancel")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("cancel\n", encoding="utf-8")
    provider_pid_path = tmp_path / "provider.pid"
    unrelated = subprocess.Popen(["sleep", "30"])
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "codex",
        f"""#!/usr/bin/env bash
        if [ "$1" = "mcp" ]; then printf '[]'; exit 0; fi
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{{"models":[{{"slug":"gpt-6-luna","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
          exit 0
        fi
        printf '%s' "$$" > "{provider_pid_path}"
        sleep 30
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    dispatch = subprocess.Popen(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "cancel-me", "--adapter", "codex",
         "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker", "--timeout", "30"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    deadline = time.monotonic() + 5
    attempt_dir = run_dir / "dispatch/tasks/cancel-me/attempt-001"
    while not attempt_dir.is_dir() and time.monotonic() < deadline:
        time.sleep(0.01)
    assert attempt_dir.is_dir()
    while not provider_pid_path.exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    assert provider_pid_path.exists()

    cancelled = subprocess.run(
        [str(ROOT / "skills/orchestrate/scripts/run_controls.py"), "cancel", "--run-dir", str(run_dir),
         "--task-id", "cancel-me", "--attempt-id", "attempt-001", "--wait-seconds", "5"],
        cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    stdout, stderr = dispatch.communicate(timeout=5)
    unrelated_status = unrelated.poll()
    unrelated.terminate()
    unrelated.wait(timeout=5)

    assert cancelled.returncode == 0, cancelled.stderr + cancelled.stdout
    assert json.loads(cancelled.stdout)["status"] == "cancelled"
    assert dispatch.returncode == 1, stderr + stdout
    record = json.loads(stdout)
    assert record["status"] == "cancelled"
    assert record["process"]["observed_exit"] is True
    with pytest.raises(ProcessLookupError):
        os.kill(int(provider_pid_path.read_text()), 0)
    assert unrelated_status is None
    assert not (attempt_dir / "cancel.request").exists()


def test_batch_marker_prelaunch_cancellation_has_no_provider_pid(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "batch-prelaunch")
    batch_dir = run_dir / "dispatch/batches/batch-001"
    batch_dir.mkdir(parents=True)
    (batch_dir / "cancel.request").touch()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prelaunch\n", encoding="utf-8")
    launched = tmp_path / "launched"
    adapter = tmp_path / "adapter"
    write_executable(adapter, f"#!/usr/bin/env bash\ntouch '{launched}'\nexit 99\n")
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "prelaunch", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        "--batch-child", "--batch-id", "batch-001",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    record = json.loads((run_dir / "dispatch/tasks/prelaunch/attempt-001/attempt.json").read_text())
    assert record["status"] == "cancelled"
    assert record["process"]["pid"] is None
    assert record["process"]["observed_exit"] is True
    assert not launched.exists()
    assert (batch_dir / "cancel.request").exists()


def test_stale_marker_on_prior_attempt_does_not_cancel_next_attempt(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "stale-marker")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("stale\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "stale", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    assert module.dispatch(args) == 0
    stale = run_dir / "dispatch/tasks/stale/attempt-001/cancel.request"
    stale.touch()

    assert module.dispatch(args) == 0
    next_record = json.loads((run_dir / "dispatch/tasks/stale/attempt-002/attempt.json").read_text())
    assert next_record["status"] == "ok"


def test_counterfeit_active_receipt_is_rejected_before_launch(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "counterfeit-receipt")
    (run_dir / "RUN_RECEIPT.json").write_text(
        '{"status":"active","closed_at":null}\n', encoding="utf-8"
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("counterfeit\n", encoding="utf-8")
    module = load_dispatch_module()
    adapter = tmp_path / "adapter-never-run-receipt"
    write_executable(adapter, "#!/usr/bin/env bash\nexit 99\n")
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "blocked", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert not (run_dir / "dispatch/tasks/blocked").exists()


def test_missing_run_receipt_reports_missing_custody(tmp_path: Path, monkeypatch, capsys) -> None:
    run_dir = make_run(tmp_path, "missing-receipt")
    (run_dir / "RUN_RECEIPT.json").unlink()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("missing receipt\n", encoding="utf-8")
    module = load_dispatch_module()
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "missing-receipt", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert json.loads(capsys.readouterr().out)["status"] == "run_custody_missing"


def test_hard_linked_prompt_is_rejected_before_provider_launch(tmp_path: Path, monkeypatch) -> None:
    task_id = "hardlink"
    run_dir = make_run(tmp_path, task_id)
    source = tmp_path / "prompt.md"
    source.write_text("secret boundary\n", encoding="utf-8")
    linked = tmp_path / "linked-prompt.md"
    linked.hardlink_to(source)
    module = load_dispatch_module()
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", task_id, "--adapter", "codex",
        "--prompt-file", str(linked), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 2


def test_hard_linked_prompt_reports_typed_custody_error(tmp_path: Path, monkeypatch, capsys) -> None:
    task_id = "hardlink-typed"
    run_dir = make_run(tmp_path, task_id)
    source = tmp_path / "prompt.md"
    source.write_text("secret boundary\n", encoding="utf-8")
    linked = tmp_path / "linked-prompt.md"
    linked.hardlink_to(source)
    module = load_dispatch_module()
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", task_id, "--adapter", "codex",
        "--prompt-file", str(linked), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 2
    assert json.loads(capsys.readouterr().out)["status"] == "prompt_hard_link_denied"


def test_relative_protected_prompt_is_rejected_before_staging(tmp_path: Path, monkeypatch, capsys) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / ".agents").mkdir()
    (repo / ".agents/fabric-policy.json").write_text('{"protected_paths":["private/"]}')
    (repo / "private").mkdir()
    (repo / "private/prompt.md").write_text("secret", encoding="utf-8")
    run_dir = make_run(repo, "protected-direct")
    module = load_dispatch_module()
    monkeypatch.chdir(repo)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(repo / ".agents"))
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "blocked", "--adapter", "opencode",
        "--prompt-file", "private/prompt.md",
        "--model", "opencode/mimo-v2.6-flash-free", "--role", "worker",
    ])
    assert module.dispatch(args) == 2
    rejection = json.loads(capsys.readouterr().out)
    assert rejection["status"] == "protected_path_denied"
    assert "protected path" in str(rejection), rejection
    assert not (run_dir / "dispatch/tasks/blocked/attempt-001/prompt.md").exists()


def test_relative_protected_add_dir_is_rejected_before_staging(tmp_path: Path, monkeypatch, capsys) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / ".agents").mkdir()
    (repo / ".agents/fabric-policy.json").write_text('{"protected_paths":["private/"]}')
    (repo / "private").mkdir()
    (repo / "prompt.md").write_text("safe", encoding="utf-8")
    run_dir = make_run(repo, "protected-add-dir")
    module = load_dispatch_module()
    monkeypatch.chdir(repo)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(repo / ".agents"))
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "blocked", "--adapter", "opencode",
        "--prompt-file", "prompt.md", "--add-dir", "private",
        "--model", "opencode/mimo-v2.6-flash-free", "--role", "worker",
    ])
    assert module.dispatch(args) == 2
    rejection = json.loads(capsys.readouterr().out)
    assert "protected path" in str(rejection), rejection
    assert not (run_dir / "dispatch/tasks/blocked/attempt-001/prompt.md").exists()


@pytest.mark.parametrize('browser', [True, False])
def test_remote_long_attempt_path_fits_browser_socket_budget(tmp_path, monkeypatch, capsys, browser):
    workspace = tmp_path / ('long-peer-project-' + 'x' * 60)
    workspace.mkdir()
    disable_memory_admission_floor(workspace)
    run_dir = make_run(workspace, 'long-browser')
    prompt = workspace / 'prompt.md'
    prompt.write_text('review\n')
    launched = workspace / 'launched'
    adapter = workspace / 'adapter'
    write_success_adapter(adapter)
    adapter.write_text(adapter.read_text().replace('#!/usr/bin/env bash\n',
        '#!/usr/bin/env bash\ntouch ' + str(launched) + '\n', 1))
    module = load_dispatch_module()
    monkeypatch.setattr(module, 'CF_DISPATCH', adapter)
    monkeypatch.setenv('PROVENANT_REMOTE_LANE', '1')
    monkeypatch.chdir(workspace)
    args = module.parser().parse_args([
        '--run-dir', str(run_dir), '--task-id', 'browser', '--adapter', 'claude',
        '--prompt-file', str(prompt), '--alias', 'senior', '--role', 'worker',
        '--capabilities', '["browser"]' if browser else '[]',
    ])
    assert module.dispatch(args) == 0
    assert launched.exists()


def test_nonfinite_or_nonpositive_timeout_is_rejected() -> None:
    module = load_dispatch_module()
    for value in ("0", "-1", "nan", "inf", "-inf"):
        try:
            module.parser().parse_args([
                "--run-dir", "/tmp", "--adapter", "codex", "--prompt-file", "/tmp/prompt",
                "--alias", "senior", "--role", "worker", "--timeout", value,
            ])
        except SystemExit as exc:
            assert exc.code == 2
        else:
            raise AssertionError(f"accepted invalid timeout {value}")


@pytest.mark.parametrize("mode", ["appendability", "read_only"], ids=[
    "test_manifest_appendability_failure_is_typed_before_launch",
    "test_read_only_manifest_is_rejected_before_launch",
])
def test_manifest_appendability_failure_is_typed_before_launch(tmp_path: Path, monkeypatch, mode) -> None:
    run_dir = make_run(tmp_path, "manifest-readonly" if mode == "appendability" else "manifest-mode")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("manifest\n" if mode == "appendability" else "manifest mode\n", encoding="utf-8")
    module = load_dispatch_module()
    manifest = run_dir / "MANIFEST.md"
    original_mode = manifest.stat().st_mode
    try:
        if mode == "appendability":
            def refuse(_run_dir):
                raise OSError("read-only fixture")
            monkeypatch.setattr(module.run_records, "ensure_manifest_appendable", refuse)
        else:
            manifest.chmod(0o444)
            try:
                with manifest.open("a", encoding="utf-8"):
                    pass
            except OSError:
                pass
            else:
                pytest.skip("test user can append to chmod 0444 files")
        args = module.parser().parse_args([
            "--run-dir", str(run_dir), "--task-id", "manifest-readonly" if mode == "appendability" else "manifest-mode", "--adapter", "codex",
            "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        ])
        monkeypatch.chdir(tmp_path)
        assert module.dispatch(args) == 2
    finally:
        if mode == "read_only":
            manifest.chmod(original_mode)


def test_manifest_append_failure_retains_terminal_attempt(tmp_path: Path, monkeypatch) -> None:
    disable_memory_admission_floor(tmp_path)
    run_dir = make_run(tmp_path, "manifest-write")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("manifest write\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    module.CF_DISPATCH = adapter
    monkeypatch.setattr(module.run_records, "append_manifest", lambda *_: (_ for _ in ()).throw(OSError("append failed")))
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "manifest-write", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 1
    attempt = run_dir / "dispatch/tasks/manifest-write/attempt-001/attempt.json"
    record = json.loads(attempt.read_text(encoding="utf-8"))
    assert record["status"] == "failed"
    assert record["failure_code"] == "manifest_write_error"
    assert record["error_class"] == "provider_error"
    assert (attempt.parent / "attempt.sha256").is_file()


def test_reentry_reconciles_missing_manifest_rows_and_retry_lineage(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "reconcile")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("reconcile\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    module.CF_DISPATCH = adapter
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "reconcile", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 0
    manifest = run_dir / "MANIFEST.md"
    manifest.write_text("\n".join(
        line for line in manifest.read_text(encoding="utf-8").splitlines()
        if "dispatch-reconcile-attempt-001" not in line
    ) + "\n", encoding="utf-8")
    retry_args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "reconcile", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        "--retry-of", "attempt-001",
    ])
    assert module.dispatch(retry_args) == 0
    text = manifest.read_text(encoding="utf-8")
    assert "dispatch-reconcile-attempt-001-attempt" in text
    second = json.loads((run_dir / "dispatch/tasks/reconcile/attempt-002/attempt.json").read_text())
    assert second["retry_of"] == "attempt-001"


@pytest.mark.parametrize("contents", ["{broken", "[]"])
def test_retry_receipt_parse_and_shape_errors_use_retry_of_invalid(tmp_path: Path, contents: str) -> None:
    run_dir = make_run(tmp_path, "invalid-retry-receipt")
    attempt = run_dir / "dispatch/tasks/retry/attempt-001/attempt.json"
    attempt.parent.mkdir(parents=True)
    attempt.write_text(contents, encoding="utf-8")
    module = load_dispatch_module()

    with pytest.raises(module.RetryReceiptInvalid) as error:
        module.retry_receipt(run_dir, attempt.parent)

    assert error.value.code == "retry_of_invalid"


def test_attempt_number_and_retry_lineage_continue_past_999(tmp_path: Path, monkeypatch) -> None:
    disable_memory_admission_floor(tmp_path)
    run_dir = make_run(tmp_path, "long-retry")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("long retry\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    module.CF_DISPATCH = adapter
    task_dir = run_dir / "dispatch/tasks/long-retry"
    task_dir.mkdir(parents=True)
    for number in range(1, 1001):
        (task_dir / f"attempt-{number:03d}").mkdir()
    (task_dir / "attempt-1000/attempt.json").write_text("{}\n", encoding="utf-8")
    retry_args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "long-retry", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        "--retry-of", "attempt-1000", "--batch-child",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(retry_args) == 0
    record = json.loads((task_dir / "attempt-1001/attempt.json").read_text())
    assert record["attempt_id"] == "attempt-1001"
    assert record["retry_of"] == "attempt-1000"


def test_reentry_does_not_verify_missing_attempt_evidence(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "missing-evidence")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("missing evidence\n", encoding="utf-8")
    adapter = tmp_path / "adapter-missing-evidence"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    module.CF_DISPATCH = adapter
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "missing", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 0
    attempt_dir = run_dir / "dispatch/tasks/missing/attempt-001"
    (attempt_dir / "result.md").unlink()
    manifest = run_dir / "MANIFEST.md"
    manifest.write_text("\n".join(
        line for line in manifest.read_text(encoding="utf-8").splitlines()
        if "dispatch-missing-attempt-001" not in line
    ) + "\n", encoding="utf-8")

    blocked = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "next", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    assert module.dispatch(blocked) == 2
    assert "dispatch-missing-attempt-001" not in manifest.read_text(encoding="utf-8")
    assert not (run_dir / "dispatch/tasks/next").exists()


def test_reentry_fails_closed_for_malformed_attempt_record(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "malformed-retained")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("malformed retained\n", encoding="utf-8")
    attempt_dir = run_dir / "dispatch/tasks/old/attempt-001"
    attempt_dir.mkdir(parents=True)
    (attempt_dir / "attempt.json").write_text(
        '{"record_type":"dispatch-attempt","result":{}}\n', encoding="utf-8"
    )
    module = load_dispatch_module()
    adapter = tmp_path / "adapter-never-run"
    write_executable(adapter, "#!/usr/bin/env bash\nexit 99\n")
    module.CF_DISPATCH = adapter
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "next", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert not (run_dir / "dispatch/tasks/next").exists()


def test_reentry_rejects_retained_paths_that_escape_the_run(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "escaping-retained")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("escaping retained\n", encoding="utf-8")
    attempt_dir = run_dir / "dispatch/tasks/old/attempt-001"
    attempt_dir.mkdir(parents=True)
    outside = tmp_path / "outside"
    outside.write_text("outside\n", encoding="utf-8")
    (attempt_dir / "attempt.json").write_text(json.dumps({
        "record_type": "dispatch-attempt",
        "attempt_path": "../../outside",
        "task_id": "old",
        "attempt_id": "attempt-001",
        "finished_at": timestamp(),
        "prompt": {"path": "../../outside"},
        "result": None,
        "route": {"adapter_receipt": {"path": "../../outside"}},
        "stderr": {"path": "../../outside"},
    }) + "\n", encoding="utf-8")
    module = load_dispatch_module()
    adapter = tmp_path / "adapter-never-run-escape"
    write_executable(adapter, "#!/usr/bin/env bash\nexit 99\n")
    module.CF_DISPATCH = adapter
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "next", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert "../../outside" not in (run_dir / "MANIFEST.md").read_text(encoding="utf-8")
    assert not (run_dir / "dispatch/tasks/next").exists()


def test_reentry_rejects_orphan_attempt_directory(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "orphan-attempt")
    orphan = run_dir / "dispatch/tasks/old/attempt-001"
    orphan.mkdir(parents=True)
    (orphan / "prompt.md").write_text("partial\n", encoding="utf-8")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("next\n", encoding="utf-8")
    adapter = tmp_path / "adapter-never-run-orphan"
    write_executable(adapter, "#!/usr/bin/env bash\nexit 99\n")
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "next", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert not (run_dir / "dispatch/tasks/next").exists()


def test_result_symlink_is_rejected_without_hashing_target(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "result-symlink")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("symlink\n", encoding="utf-8")
    outside = tmp_path / "outside-result"
    outside.write_text("unchanged\n", encoding="utf-8")
    adapter = tmp_path / "fake-symlink-adapter"
    write_executable(
        adapter,
        f"""#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in --out) out="$2"; shift 2;; *) shift;; esac
        done
        ln -s {outside} "$out"
        printf '{{"status":"ok"}}\n'
        """,
    )
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "symlink", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    assert outside.read_text(encoding="utf-8") == "unchanged\n"
    record = json.loads(
        (run_dir / "dispatch/tasks/symlink/attempt-001/attempt.json").read_text()
    )
    assert record["failure_code"] == "result_invalid_path"
    assert record["result"] is None


def test_attempt_records_available_git_base_identity(tmp_path: Path, monkeypatch) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=workspace, check=True)
    subprocess.run(["git", "config", "user.email", "test@example.invalid"], cwd=workspace, check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=workspace, check=True)
    (workspace / "tracked.txt").write_text("base\n", encoding="utf-8")
    subprocess.run(["git", "add", "tracked.txt"], cwd=workspace, check=True)
    subprocess.run(["git", "commit", "-qm", "base"], cwd=workspace, check=True)
    expected_head = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=workspace, text=True
    ).strip()
    run_dir = make_run(workspace, "git-identity")
    prompt = workspace / "prompt.md"
    prompt.write_text("identity\n", encoding="utf-8")
    adapter = workspace / "success-adapter"
    write_success_adapter(adapter)
    write_executable(
        adapter,
        adapter.read_text(encoding="utf-8").replace(
            "#!/usr/bin/env bash\n",
            "#!/usr/bin/env bash\ngit commit --allow-empty -qm provider-change\n",
            1,
        ),
    )
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "identity", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(workspace)

    assert module.dispatch(args) == 0
    record = json.loads(
        (run_dir / "dispatch/tasks/identity/attempt-001/attempt.json").read_text()
    )
    assert record["workspace"]["base_revision"] == expected_head
    assert record["workspace"]["working_tree"] == "dirty"
    assert subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=workspace, text=True
    ).strip() != expected_head


@pytest.mark.parametrize("link_level", ["tasks", "task"])
def test_new_attempt_rejects_preexisting_directory_symlink(
    tmp_path: Path, monkeypatch, link_level: str
) -> None:
    run_dir = make_run(tmp_path, f"directory-symlink-{link_level}")
    outside = tmp_path / f"outside-{link_level}"
    outside.mkdir()
    if link_level == "tasks":
        (run_dir / "dispatch").mkdir()
        (run_dir / "dispatch/tasks").symlink_to(outside, target_is_directory=True)
    else:
        (run_dir / "dispatch/tasks").mkdir(parents=True)
        (run_dir / "dispatch/tasks/escaped").symlink_to(outside, target_is_directory=True)
    prompt = tmp_path / "prompt.md"
    prompt.write_text("contained\n", encoding="utf-8")
    adapter = tmp_path / f"adapter-never-run-{link_level}"
    write_executable(adapter, "#!/usr/bin/env bash\nexit 99\n")
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "escaped", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert list(outside.iterdir()) == []


def make_worktree(root: Path) -> Path:
    repo = root / "writer-repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c",
                    "user.email=test@example.invalid", "commit", "-q", "--allow-empty", "-m", "initial"], check=True)
    worktree = root / "writer-worktree"
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b", "writer", str(worktree)], check=True)
    return worktree.resolve()


def run_writer_dispatch(
    tmp_path: Path, run_dir: Path, prompt: Path, *extra: str, task_id: str = "task-1",
    adapter: str = "claude",
) -> subprocess.CompletedProcess[str]:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    write_success_adapter(bin_dir / "cf_dispatch_stub.sh")
    if adapter == "opencode":
        write_executable(bin_dir / "opencode", """#!/usr/bin/env bash
            printf '%s\\n' '{"type":"text","part":{"text":"OK"}}'
        """)
    env = os.environ.copy()
    env.pop("AGENTS_HOME", None)
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(tmp_path / ".agents")
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(ROOT)
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    return subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", task_id, "--adapter", adapter,
         "--prompt-file", str(prompt), "--orchestrator-family", "openai",
         *([] if adapter == "opencode" else ["--alias", "senior"]),
         "--role", "worker", *extra],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )


def test_opencode_worktree_writer_reaches_adapter_and_attempt(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "opencode-writer")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Make a change\n", encoding="utf-8")
    worktree = make_worktree(tmp_path)
    result = run_writer_dispatch(
        tmp_path, run_dir, prompt, "--access-mode", "worktree_write",
        "--worktree", str(worktree), "--model", "opencode-go/deepseek-v4.1-flash",
        adapter="opencode",
    )
    assert result.returncode == 0, result.stderr + result.stdout
    receipt = json.loads(result.stdout)
    assert receipt["status"] == "ok"
    attempt = json.loads((run_dir / "dispatch/tasks/task-1/attempt-001/attempt.json").read_text())
    assert attempt["requested_route"]["adapter"] == "opencode"
    assert attempt["requested_route"]["access_mode"] == "worktree_write"


def test_writer_branch_name_warns_in_receipt_without_refusing(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "branch-warning")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Make a change\n", encoding="utf-8")
    worktree = make_worktree(tmp_path)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    write_executable(
        bin_dir / "claude",
        """#!/usr/bin/env bash
        cat >/dev/null
        printf 'PWD=%s\\n' "$PWD"
        """,
    )
    result = run_writer_dispatch(tmp_path, run_dir, prompt, "--access-mode", "worktree_write",
                                 "--worktree", str(worktree))
    receipt = json.loads(result.stdout.splitlines()[-1])
    assert result.returncode == 0, result.stderr + result.stdout
    assert receipt["status"] == "ok"
    assert any("branch writer is outside" in note for note in receipt["fabric"]["warnings"])
    attempt = json.loads((run_dir / "tasks/task-1/attempt-001/attempt.json").read_text())
    assert any("branch writer is outside" in note for note in attempt["warnings"])

    subprocess.run(["git", "-C", str(worktree), "branch", "-m", "feat/fabric-branch-warning"], check=True)
    next_run = make_run(tmp_path, "branch-valid")
    result = run_writer_dispatch(tmp_path, next_run, prompt, "--access-mode", "worktree_write",
                                 "--worktree", str(worktree))
    receipt = json.loads(result.stdout.splitlines()[-1])
    assert receipt["status"] != "invalid_worktree"
    assert not any("branch " in note and "outside" in note for note in receipt["fabric"]["warnings"])


def test_default_suite_blocks_provider_clis(_provider_cli_stub_dir: Path) -> None:
    for executable in ("claude", "codex", "herdr", "pi", "agy", "cursor-agent", "copilot", "kiro-cli", "opencode"):
        path = shutil.which(executable)
        assert path is not None, executable
        assert Path(path).parent == _provider_cli_stub_dir, executable
        result = subprocess.run([path], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert result.returncode == 97, executable
        assert "blocked real provider CLI" in result.stderr, executable


def test_primary_checkout_writer_is_rejected(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "primary-writer")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello", encoding="utf-8")
    primary = tmp_path / "primary"
    primary.mkdir()
    subprocess.run(["git", "init", "-q", str(primary)], check=True)
    result = run_writer_dispatch(tmp_path, run_dir, prompt, "--access-mode", "worktree_write",
                                 "--worktree", str(primary))
    assert result.returncode == 2
    assert "create a linked worktree" in result.stdout


def test_worktree_writer_route_reaches_the_adapter_and_the_attempt_record(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "writer")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    worktree = make_worktree(tmp_path)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "claude",
        """#!/usr/bin/env bash
        cat >/dev/null
        printf 'PWD=%s\\n' "$PWD"
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "task-1", "--adapter", "claude",
         "--prompt-file", str(prompt), "--orchestrator-family", "openai", "--alias", "senior",
         "--role", "worker", "--access-mode", "worktree_write", "--worktree", str(worktree)],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    assert result.returncode == 0, result.stderr + result.stdout
    record = json.loads(
        (run_dir / "dispatch/tasks/task-1/attempt-001/attempt.json").read_text(encoding="utf-8")
    )
    assert record["requested_route"]["access_mode"] == "worktree_write"
    assert record["requested_route"]["worktree"] == str(worktree)
    assert record["prompt"]["original_path"] == str(prompt)
    boundary = record["route"]["applied"]["write_boundary"]
    assert boundary["kind"] in {"sandbox-exec", "none"}
    if boundary["kind"] == "sandbox-exec":
        assert str(run_dir / "dispatch/tasks/task-1/attempt-001") in boundary["writable_paths"]
    else:
        assert any("writes are unconfined" in warning for warning in record["route"]["warnings"])
    receipt = json.loads(
        (run_dir / "dispatch/tasks/task-1/attempt-001/adapter-receipt.json").read_text(encoding="utf-8")
    )
    assert receipt["access_mode"] == "worktree_write"
    assert receipt["worktree"] == str(worktree)
    assert receipt["read_only_guarantee"] == "none"
    result_text = (run_dir / record["result"]["path"]).read_text(encoding="utf-8")
    assert result_text.strip() == f"PWD={worktree}"


def test_read_only_route_is_the_default_and_refuses_a_worktree(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "default-read-only")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    worktree = make_worktree(tmp_path)

    module = load_dispatch_module()
    parsed = module.parser().parse_args([
        "--run-dir", str(run_dir), "--adapter", "claude", "--prompt-file", str(prompt),
        "--alias", "senior", "--role", "worker",
    ])
    assert parsed.access_mode == "read_only"
    assert parsed.worktree is None

    result = run_writer_dispatch(tmp_path, run_dir, prompt, "--worktree", str(worktree))
    assert result.returncode != 0
    assert json.loads(result.stdout)["status"] == "worktree_not_applicable"


@pytest.mark.parametrize("adapter", ["claude", "agy"])
def test_concurrent_writer_on_one_worktree_is_rejected(tmp_path: Path, adapter: str) -> None:
    run_dir = make_run(tmp_path, "one-writer")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    worktree = make_worktree(tmp_path)
    module = load_dispatch_module()

    lease = module.writer_lease.acquire_worktree_lease(worktree)
    try:
        with pytest.raises(module.writer_lease.WorktreeLeaseError, match="another writer"):
            module.writer_lease.acquire_worktree_lease(worktree)
        result = run_writer_dispatch(
            tmp_path, run_dir, prompt, "--access-mode", "worktree_write", "--worktree", str(worktree),
            adapter=adapter,
        )
        assert result.returncode != 0
        assert json.loads(result.stdout)["status"] == "worktree_busy"
    finally:
        module.writer_lease.release_worktree_lease(lease)

    assert module.writer_lease.acquire_worktree_lease(worktree) is not None



def test_worktree_writer_route_is_refused_for_assurance_and_unsupported_adapters(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "writer-rails")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    worktree = make_worktree(tmp_path)

    assurance = run_writer_dispatch(
        tmp_path, run_dir, prompt, "--intent", "assurance",
        "--access-mode", "worktree_write", "--worktree", str(worktree),
    )
    assert json.loads(assurance.stdout)["status"] == "worktree_write_intent_denied"

    missing = run_writer_dispatch(tmp_path, run_dir, prompt, "--access-mode", "worktree_write")
    assert json.loads(missing.stdout)["status"] == "worktree_required"

    invalid = run_writer_dispatch(
        tmp_path, run_dir, prompt, "--access-mode", "worktree_write", "--worktree", str(tmp_path / "absent"),
    )
    assert json.loads(invalid.stdout)["status"] == "worktree_invalid"


def test_provider_deadline_is_passed_below_the_owner_deadline(tmp_path: Path) -> None:
    """The caller's deadline reaches the dispatcher, shortened so the provider exits first."""
    module = load_dispatch_module()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    args = module.parser().parse_args([
        "--run-dir", str(tmp_path), "--task-id", "deadline", "--adapter", "agy",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        "--timeout", "900",
    ])
    command = module.build_command(args, prompt, tmp_path / "result.md")
    assert "--timeout-seconds" in command
    passed = int(command[command.index("--timeout-seconds") + 1])
    assert passed == 895
    assert 0 < passed < args.timeout_seconds
    # A short deadline still leaves the provider a positive whole second.
    assert module.dispatch_preflight.provider_timeout_seconds(2.0) == 1
    assert module.dispatch_preflight.provider_timeout_seconds(0.25) == 1


def test_agy_arm_receives_the_deadline_and_reports_a_typed_timeout(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "agytimeout")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    args_file = tmp_path / "agy.args"
    write_executable(
        bin_dir / "agy",
        f"""#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-high\\ngemini-3.7-flash-medium\\ngemini-3.7-flash-low\\n'
          exit 0
        fi
        printf '%s\\n' "$@" > {args_file}
        sleep 30
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "agytimeout", "--adapter", "agy",
         "--prompt-file", str(prompt), "--model", "gemini-3.7-flash", "--role", "worker",
         "--effort", "medium",
         "--intent", "ordinary", "--timeout", "3"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["status"] == "timed_out"
    assert record["failure_code"] in {"timeout", "deadline_exceeded"}
    passed = args_file.read_text(encoding="utf-8").splitlines()
    assert "--print-timeout" in passed
    assert passed[passed.index("--print-timeout") + 1] == "2s"


def test_provider_reported_timeout_is_typed_rather_than_an_empty_result(tmp_path: Path) -> None:
    """A deadline the provider enforced itself is a timeout, not a result defect."""
    run_dir = make_run(tmp_path, "providertimeout")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-high\ngemini-3.7-flash-medium\ngemini-3.7-flash-low\n'
          exit 0
        fi
        printf '%s\n' '{"status":"ERROR","response":"","error":"print timeout of 2s exceeded"}'
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "providertimeout", "--adapter", "agy",
         "--prompt-file", str(prompt), "--model", "gemini-3.7-flash", "--role", "worker",
         "--effort", "medium",
         "--intent", "ordinary", "--timeout", "60"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["route"]["status"] == "timed_out"
    assert record["status"] == "timed_out"
    assert record["failure_code"] == "provider_timeout"
    assert record["process"]["observed_exit"] is True


def test_claude_arm_short_timeout_reports_a_timeout_not_a_receipt_failure(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "claudetimeout")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "claude",
        """#!/usr/bin/env bash
        # A partial answer on the wire, then silence: the shape that used to be
        # published as a corrupt receipt when the owner killed the group.
        printf 'PART'
        sleep 30
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "claudetimeout", "--adapter", "claude",
         "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
         "--intent", "ordinary", "--timeout", "2"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["status"] == "timed_out"
    assert record["failure_code"] in {"timeout", "deadline_exceeded"}
    assert record["failure_code"] not in {
        "adapter_receipt_invalid", "result_missing_or_empty", "terminal_envelope_invalid",
        "result_integrity_error", "empty_result",
    }


def test_result_missing_past_the_provider_deadline_is_a_timeout_not_a_result_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The deadline is checked before a missing or partial result is diagnosed."""
    module = load_dispatch_module()
    run_dir = make_run(tmp_path, "deadlinenet")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    adapter = tmp_path / "adapter.sh"
    write_executable(
        adapter,
        """#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --out) out="$2"; shift 2;;
            *) shift;;
          esac
        done
        # Past the provider deadline (1s of a 2s owner budget), under the owner's,
        # and with nothing to publish.
        sleep 1.4
        : > "$out"
        printf '{"tool":"claude","adapter":"claude","execution_intent":"ordinary","resolved_model":"test-model","provider_family":"test-family","model_family":"test-family","endpoint_provider":"test-provider","identity_source":"test-fixture","status":"ok","exit":0,"output_path":"%s","output_digest":"","read_only_guarantee":"none","cross_family":false,"certification_eligible":false}\n' "$out"
        """,
    )
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "deadlinenet", "--adapter", "claude",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
        "--timeout", "2",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 1
    record = json.loads(
        (run_dir / "dispatch/tasks/deadlinenet/attempt-001/attempt.json").read_text()
    )
    assert record["status"] == "timed_out"
    assert record["failure_code"] not in {
        "result_missing_or_empty", "adapter_receipt_invalid", "terminal_envelope_invalid",
    }
    assert record["process"]["observed_exit"] is True


def test_front_door_preflight_rejects_all_invalid_tasks_without_run(tmp_path):
    result = subprocess.run(
        [sys.executable, str(SCRIPT), '--preflight-json'], cwd=tmp_path,
        input=json.dumps({'tasks': [
            {'id': 'missing', 'adapter': 'claude', 'alias': 'senior', 'prompt_file': 'absent.md'},
            {'id': 'broker', 'adapter': 'opencode', 'alias': 'senior', 'prompt': 'hello'},
        ]}), text=True, capture_output=True,
        env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': str(tmp_path / '.agents')},
    )
    record = json.loads(result.stdout)
    assert record['status'] == 'rejected'
    # OpenCode now resolves its catalogue default model, so only the missing prompt is rejected.
    assert {error['error'] for error in record['errors']} == {'prompt_unavailable'}
    assert all(error['fix'] for error in record['errors'])
    assert not (tmp_path / '.agent-run').exists()


@pytest.mark.parametrize(("field", "value", "error"), [
    ("access_mode", "read_only", "capabilities_mode_invalid"),
    ("platform", "linux", "capabilities_platform_invalid"),
    ("sandbox_exec", None, "capabilities_confinement_unavailable"),
    ("network", False, "capabilities_network_required"),
    ("sandbox", "read-only", "capabilities_sandbox_invalid"),
])
def test_front_door_capability_preflight_fails_closed(monkeypatch, tmp_path, field, value, error):
    module = load_dispatch_module()
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(tmp_path / ".agents"))
    monkeypatch.setattr(module.provider_exec.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    task = {"id": "capability", "adapter": "codex", "access_mode": "worktree_write",
            "network": True, "capabilities": ["postgres"], "prompt": "hello"}
    if field == "adapter": task["adapter"] = value
    elif field == "access_mode": task["access_mode"] = value
    elif field == "sandbox": task["sandbox"] = value
    elif field == "platform": monkeypatch.setattr(module.provider_exec.sys, "platform", value)
    elif field == "sandbox_exec": force_sandbox(monkeypatch, value)
    elif field == "network": task["network"] = value

    result = module.dispatch_preflight.preflight_tasks([task], tmp_path)

    assert result["status"] == "rejected"
    assert result["error"] == error
    assert result["fix"].strip().endswith(".")
    assert "\n" not in result["fix"]


@pytest.mark.parametrize(("field", "value"), [("adapter", "claude"), ("sandbox", "full")])
def test_front_door_capability_preflight_accepts_other_adapters_and_full_sandbox(monkeypatch, tmp_path, field, value):
    module = load_dispatch_module()
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(module.provider_exec.sys, "platform", "linux")
    task = {"id": "capability", "adapter": "codex", "access_mode": "worktree_write",
            "network": True, "capabilities": ["postgres"], "prompt": "hello", field: value}
    result = module.dispatch_preflight.preflight_tasks([task], tmp_path)
    # Later checks (here the missing worktree) may still refuse it, but not its capabilities.
    assert not result.get("error", "").startswith("capabilities"), result


@pytest.mark.parametrize("capabilities", [["unknown"], ["browser", "browser"], "browser", None])
def test_front_door_capability_preflight_rejects_malformed_values(monkeypatch, tmp_path, capabilities):
    module = load_dispatch_module()
    monkeypatch.chdir(tmp_path)
    task = {"id": "capability", "adapter": "codex", "access_mode": "worktree_write",
            "network": True, "capabilities": capabilities, "prompt": "hello"}
    result = module.dispatch_preflight.preflight_tasks([task], tmp_path)
    assert result["status"] == "rejected"
    assert result["error"] == "capabilities_invalid"
    assert result["fix"].strip().endswith(".")


def test_dispatch_command_forwards_capabilities_as_json(tmp_path):
    module = load_dispatch_module()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello", encoding="utf-8")
    args = module.parser().parse_args([
        "--run-dir", str(tmp_path), "--task-id", "caps", "--adapter", "codex",
        "--prompt-file", str(prompt), "--model", "fixture", "--timeout", "900",
        "--capabilities", '["browser","postgres"]',
    ])
    command = module.build_command(args, prompt, tmp_path / "result.md")
    index = command.index("--capabilities")
    assert json.loads(command[index + 1]) == ["browser", "postgres"]


def test_secret_in_inline_prompt_is_rejected_before_preflight_route(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    module = load_dispatch_module()
    result = module.dispatch_preflight.preflight_tasks([{
        'id': 'task-1', 'adapter': 'claude', 'prompt': 'Bearer ' +
        'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.' + 'a' * 43,
    }])
    assert result['status'] == 'rejected'
    assert result['error'] == 'secret_detected'
    assert 'Bearer JWT' in result['fix']
    assert 'Bearer eyJ' not in json.dumps(result)


def test_preflight_secret_file_rejects_and_explicit_override_accepts(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    module = load_dispatch_module()
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('hello\n' + 'AKIA' + 'A' * 16)
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior', 'prompt_file': str(prompt)}
    rejected = module.dispatch_preflight.preflight_tasks([task])
    assert rejected['error'] == 'secret_detected'
    assert f'{prompt}:2' in rejected['fix']
    assert module.dispatch_preflight.preflight_tasks([{**task, 'allow_secrets': True}])['status'] == 'validated'


def test_preflight_accepts_a_prompt_file_only_inside_a_read_root(tmp_path, monkeypatch):
    workspace = tmp_path / 'caller'
    other = tmp_path / 'other-project'
    workspace.mkdir()
    other.mkdir()
    monkeypatch.chdir(workspace)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    module = load_dispatch_module()
    monkeypatch.setattr(module.run_records, "prompt_temp_roots", lambda: ())
    prompt = other / 'brief.md'
    prompt.write_text('read the other project')
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior', 'prompt_file': str(prompt)}

    rejected = module.dispatch_preflight.preflight_tasks([task], workspace)
    assert rejected['error'] == 'prompt_path_forbidden'
    assert 'registered Fabric project' in rejected['fix']
    assert 'dispatch from it' in rejected['fix']
    assert module.dispatch_preflight.preflight_tasks([{**task, 'read_roots': [str(other)]}], workspace)['status'] == 'validated'


def test_preflight_accepts_temp_prompts_and_retains_secret_and_link_checks(tmp_path, monkeypatch):
    workspace = tmp_path / 'workspace'
    scratch = tmp_path / 'scratch'
    workspace.mkdir()
    scratch.mkdir()
    monkeypatch.chdir(workspace)
    monkeypatch.setenv('TMPDIR', str(scratch))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    module = load_dispatch_module()
    prompt = scratch / 'brief.md'
    prompt.write_text('hello')
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior', 'prompt_file': str(prompt)}
    assert module.dispatch_preflight.preflight_tasks([task], workspace)['status'] == 'validated'
    prompt.write_text('AKIA' + 'A' * 16)
    assert module.dispatch_preflight.preflight_tasks([task], workspace)['error'] == 'secret_detected'
    auth = scratch / '.codex' / 'auth.json'
    auth.parent.mkdir()
    auth.write_text('{}')
    assert module.dispatch_preflight.preflight_tasks([{**task, 'prompt_file': str(auth)}], workspace)['error'] == 'credential_or_auth_store_denied'
    link = scratch / 'linked.md'
    link.symlink_to(auth)
    assert module.dispatch_preflight.preflight_tasks([{**task, 'prompt_file': str(link)}], workspace)['error'] == 'credential_or_auth_store_denied'
    linked = scratch / 'hardlinked.md'
    linked.hardlink_to(prompt)
    assert module.dispatch_preflight.preflight_tasks([{**task, 'prompt_file': str(linked)}], workspace)['error'] == 'prompt_hard_link_denied'


def test_preflight_scans_a_read_root_prompt_for_secrets(tmp_path, monkeypatch):
    workspace = tmp_path / 'caller'
    other = tmp_path / 'other-project'
    workspace.mkdir()
    other.mkdir()
    monkeypatch.chdir(workspace)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    module = load_dispatch_module()
    prompt = other / 'brief.md'
    prompt.write_text('hello\n' + 'AKIA' + 'A' * 16)
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior', 'prompt_file': str(prompt),
            'read_roots': [str(other)]}
    assert module.dispatch_preflight.preflight_tasks([task], workspace)['error'] == 'secret_detected'


def test_preflight_refuses_a_credential_store_prompt_in_any_root(tmp_path, monkeypatch):
    workspace = tmp_path / 'caller'
    other = tmp_path / 'other-project'
    (workspace / 'notes').mkdir(parents=True)
    (other / '.codex').mkdir(parents=True)
    monkeypatch.chdir(workspace)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    module = load_dispatch_module()
    auth = other / '.codex' / 'auth.json'
    auth.write_text('{"tokens": {"opaque": "value"}}')
    local = workspace / 'notes' / 'auth.json'
    local.write_text('{"opaque": "value"}')
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior'}
    for prompt, roots in ((auth, [str(other)]), (local, [])):
        result = module.dispatch_preflight.preflight_tasks([{**task, 'prompt_file': str(prompt), 'read_roots': roots}], workspace)
        assert result['error'] == 'credential_or_auth_store_denied', prompt


def test_prepare_resume_keeps_an_opencode_variant_as_model_hash_variant(tmp_path: Path):
    module = load_dispatch_module()
    run_dir = make_run(tmp_path, "resume-variant")
    attempt_dir = run_dir / "tasks/task-1/attempt-001"
    attempt_dir.mkdir(parents=True)
    workspace = tmp_path / "ws"
    workspace.mkdir()
    previous = {
        "run_id": "resume-me", "task_id": "task-1", "attempt": 1, "state": "terminal",
        "status": "ok", "mode": "read_only", "cwd": str(workspace), "worktree": None,
        "workspace": {"root": str(workspace)}, "session_id": "saved-session", "read_roots": [],
        "provenance": {"requested": {"adapter": "opencode"}, "resolved_model": "opencode/deepseek-v4.1-flash-free",
                       "effort_applied": "high"},
        "applied": {"sandbox": "read-only", "network": None, "add_dirs": [], "capabilities": []},
        "paths": {"events": None}, "requested_route": {},
    }
    (attempt_dir / "attempt.json").write_text(json.dumps(previous), encoding="utf-8")
    args = dispatch_args(_profile="resume", run_dir=run_dir)
    module.prepare_resume(args)
    assert args.model == "opencode/deepseek-v4.1-flash-free#high" and args.effort == "high"


def test_prepare_resume_refuses_a_read_root_that_now_resolves_elsewhere(tmp_path: Path):
    module = load_dispatch_module()
    run_dir = make_run(tmp_path, "resume-moved")
    attempt_dir = run_dir / "tasks/task-1/attempt-001"
    attempt_dir.mkdir(parents=True)
    other, elsewhere = tmp_path / "other-project", tmp_path / "elsewhere"
    (other / "src").mkdir(parents=True)
    (elsewhere / "src").mkdir(parents=True)
    previous = {
        "run_id": "resume-me", "task_id": "task-1", "attempt": 1, "state": "terminal",
        "status": "ok", "mode": "read_only", "cwd": str(other / "src"), "worktree": None,
        "workspace": {"root": str(tmp_path)}, "session_id": "saved-session",
        "read_roots": [str(other / "src")],
        "provenance": {"requested": {"adapter": "codex"}, "resolved_model": "fixture", "effort_applied": ""},
        "applied": {"sandbox": "read-only", "network": None, "add_dirs": []},
        "paths": {"events": None}, "requested_route": {},
    }
    (attempt_dir / "attempt.json").write_text(json.dumps(previous), encoding="utf-8")
    fresh = lambda: SimpleNamespace(
        run_dir=run_dir, resume="resume-me", task_id=None, tool=None, model=None, effort=None,
        access_mode=None, worktree=None, provider_cwd=None, sandbox=None, network=None,
        add_dirs=[], resume_session=None, fallback=None, context_ceiling=None,
    )
    module.prepare_resume(fresh())
    other.rename(tmp_path / "moved")
    other.symlink_to(elsewhere)
    with pytest.raises(module.ResumeError) as refused:
        module.prepare_resume(fresh())
    assert refused.value.code == "resume_read_root_changed"


def test_read_only_dispatch_runs_in_a_read_root_outside_the_workspace(tmp_path, monkeypatch):
    code = '''import json,os,sys
sys.stdin.read()
print(json.dumps({"type":"result","result":os.getcwd()}))
'''
    workspace = tmp_path / 'caller'
    other = tmp_path / 'other-project'
    workspace.mkdir()
    (other / 'src').mkdir(parents=True)
    run, prompt, command = real_owner_fixture(workspace, monkeypatch, code)
    brief = other / 'brief.md'
    brief.write_text('read the other project')
    command[command.index(str(prompt))] = str(brief)
    command += ['--cwd', str(other / 'src'), '--no-preface']

    refused_run = make_run(workspace, 'outside-refused')
    refused_command = [str(refused_run) if arg == str(run) else arg for arg in command]
    refused = subprocess.run(refused_command, cwd=workspace, capture_output=True, text=True)
    assert refused.returncode != 0
    assert json.loads(refused.stdout)['status'] == 'failed'

    # A non-canonical root is resolved before use and saved canonical, so resume accepts it.
    result = subprocess.run([*command, '--read-root', str(other / 'src' / '..')], cwd=workspace,
                            capture_output=True, text=True)
    assert result.returncode == 0, result.stdout + result.stderr
    row = json.loads(next(run.glob('tasks/*/attempt-001/attempt.json')).read_text())
    assert row['status'] == 'ok'
    assert row['cwd'] == str((other / 'src').resolve())
    assert row['read_roots'] == [str(other.resolve())]
    assert (run / row['paths']['result']).read_text().strip() == str((other / 'src').resolve())
    resumed = subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', row['run_id'],
                              '--prompt-file', str(brief)], cwd=workspace, capture_output=True, text=True)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    second = json.loads(next(run.glob('tasks/*/attempt-002/attempt.json')).read_text())
    assert (second['status'], second['read_roots']) == ('ok', [str(other.resolve())])


def test_preflight_secret_in_add_dirs_rejects_and_override_accepts(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    directory = tmp_path / 'shared'
    directory.mkdir()
    (directory / 'source.txt').write_text('rk_live_' + 'a' * 24)
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior',
            'prompt': 'review shared files', 'add_dirs': [str(directory)]}
    module = load_dispatch_module()
    rejected = module.dispatch_preflight.preflight_tasks([task])
    assert rejected['error'] == 'secret_detected'
    assert 'Stripe live key' in rejected['fix']
    assert module.dispatch_preflight.preflight_tasks([{**task, 'allow_secrets': True}])['status'] == 'validated'


def test_preflight_and_dispatch_refuse_scan_budget_overrun_unless_overridden(tmp_path, monkeypatch, capsys):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    scan = load_secret_scan_module()
    monkeypatch.setattr(scan, 'MAX_FILES', 0)
    directory = tmp_path / 'shared'
    directory.mkdir()
    (directory / 'source.txt').write_text('ordinary content')
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'senior',
            'prompt': 'review shared files', 'add_dirs': [str(directory)]}
    module = load_dispatch_module()

    rejected = module.dispatch_preflight.preflight_tasks([task])
    assert rejected['status'] == 'rejected'
    assert rejected['error'] == 'secret_scan_budget_exceeded'
    assert str(directory) in rejected['fix'] and 'narrower path' in rejected['fix']
    assert module.dispatch_preflight.preflight_tasks([{**task, 'allow_secrets': True}])['status'] == 'validated'

    run_dir = make_run(tmp_path, 'scan-budget')
    adapter = tmp_path / 'adapter'
    write_success_adapter(adapter)
    module.CF_DISPATCH = adapter
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('review shared files')
    args = module.parser().parse_args(['--run-dir', str(run_dir), '--adapter', 'codex',
        '--prompt-file', str(prompt), '--alias', 'senior', '--role', 'worker',
        '--add-dir', str(directory)])
    assert module.dispatch(args) == 2
    refused = json.loads(capsys.readouterr().out.splitlines()[-1])
    assert refused['status'] == 'rejected'
    assert refused['error'] == 'secret_scan_budget_exceeded'
    assert 'allow_secrets: true and explain why in the prompt' in refused['fix']

    args.allow_secrets = True
    assert module.dispatch(args) == 0


def test_stale_instance_routing_uses_fresh_product_and_warns(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    instance = tmp_path / 'instance'
    (instance / 'config').mkdir(parents=True)
    (instance / 'config/model-routing.json').write_text(
        json.dumps({'catalog_date': (datetime.now(UTC).date() - timedelta(days=3650)).isoformat(), 'adapters': {},
                    'families': {'anthropic': {'aliases': {'senior': ['claude-stale']}}}}))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    module = load_dispatch_module()

    result = module.dispatch_preflight.preflight_tasks([{
        'id': 'fresh-route', 'adapter': 'claude', 'alias': 'senior', 'prompt': 'review this',
    }])

    assert result['status'] == 'validated'
    assert result['routes'][0]['resolved_model'] == 'sonnet'
    warning = result['routes'][0]['warnings'][-1]
    assert 'resolved against the fresh product snapshot' in warning
    assert 'use the current release routing catalogue' in warning


def test_effort_rejection_retries_once_at_nearest_lower_capability(tmp_path):
    module = load_dispatch_module()
    plan = module.provider_exec.build_plan(
        'codex', {'resolved_model': 'fixture', 'effort': 'high', 'effort_applied': 'high',
                  'supported_efforts': ['low', 'medium', 'high']},
        'hello', cwd=tmp_path, workspace_root=tmp_path,
    )
    rejected = {
        'error': 'invalid_input', 'status': 'rejected',
        'reason': '--effort is not supported for model fixture',
        'evidence': {'excerpt': '--effort is not supported for model fixture'},
    }

    retry = module.dispatch_preflight.effort_retry_plan(plan, rejected)

    assert retry['effort'] == retry['route']['effort_applied'] == 'medium'
    assert retry['warnings'][-1] == 'adapter rejected high effort; retrying at medium'
    assert module.dispatch_preflight.effort_retry_plan(
        plan, {**rejected, 'error': 'model_unavailable'}) is None


def test_direct_dispatch_rejects_before_prompt_staging_and_override_records_finding(tmp_path, monkeypatch, capsys):
    run_dir = make_run(tmp_path, 'secret-direct')
    module = load_dispatch_module()
    adapter = tmp_path / 'adapter'
    write_success_adapter(adapter)
    module.CF_DISPATCH = adapter
    monkeypatch.chdir(tmp_path)
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('hello\n' + 'AKIA' + 'A' * 16)
    args = module.parser().parse_args(['--run-dir', str(run_dir), '--adapter', 'codex',
        '--prompt-file', str(prompt), '--alias', 'senior', '--role', 'worker'])
    assert module.dispatch(args) == 2
    rejected = json.loads(capsys.readouterr().out.splitlines()[-1])
    assert rejected['status'] == 'rejected' and rejected['error'] == 'secret_detected'
    assert f'{prompt}:2' in rejected['fix']
    assert not list((run_dir / 'dispatch/tasks').glob('*/attempt-*/prompt.md'))
    args.allow_secrets = True
    assert module.dispatch(args) == 0
    attempt = json.loads(next((run_dir / 'tasks').glob('*/attempt-*/attempt.json')).read_text())
    assert attempt['secret_scan'] == {'allow_secrets': True, 'finding_names': ['AWS access key ID']}


def test_mcp_owner_closes_receipt(tmp_path, monkeypatch, capsys):
    run_dir = make_run(tmp_path, 'mcp-finished')
    module = load_dispatch_module()
    adapter = tmp_path / 'adapter'
    write_success_adapter(adapter)
    module.CF_DISPATCH = adapter
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('PROVENANT_RUN_TOKEN', 'fixture-token')
    monkeypatch.setenv('PROVENANT_RUN_DIR', str(run_dir))
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('hello')
    args = module.parser().parse_args(['--run-dir', str(run_dir), '--adapter', 'codex',
        '--prompt-file', str(prompt), '--alias', 'senior', '--role', 'worker'])
    assert module.dispatch(args) == 0
    terminal = json.loads(capsys.readouterr().out.splitlines()[-1])
    assert terminal['schema'] == 'fabric.attempt.v1'
    assert terminal['status'] == 'ok'
    assert terminal['provenance']['line'].startswith('Route:')
    receipt = json.loads((run_dir / 'RUN_RECEIPT.json').read_text())
    assert receipt['status'] == 'ok'
    assert receipt['closed_at']


def test_mcp_batch_cancelled_before_dispatch_closes_receipt(tmp_path, monkeypatch):
    run_dir = make_run(tmp_path, 'mcp-cancelled-before-dispatch')
    module = load_dispatch_module()
    monkeypatch.setenv('PROVENANT_RUN_TOKEN', 'fixture-token')
    monkeypatch.setenv('PROVENANT_RUN_DIR', str(run_dir))
    summary = run_dir / 'dispatch/batches/batch-001/summary.json'
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({'status': 'cancelled', 'tasks': [
        {'task_id': 'never-started', 'status': 'cancelled'}]}))
    module.close_mcp_run(run_dir)
    assert json.loads((run_dir / 'RUN_RECEIPT.json').read_text())['status'] == 'cancelled'


@pytest.mark.parametrize(('mode', 'timeout'), [('read_only', 3600), ('worktree_write', 10800)])
def test_front_door_mode_timeout_defaults(tmp_path, mode, timeout):
    module = load_dispatch_module()
    args = module.parser().parse_args(['--run-dir', str(tmp_path), '--adapter', 'codex',
        '--prompt-stdin', '--alias', 'senior', '--role', 'worker', '--access-mode', mode])
    module.dispatch(args)
    assert args.timeout_seconds == timeout


def test_front_door_preflight_reuses_registered_routes_without_capability_probe(tmp_path, monkeypatch):
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    counter = tmp_path / 'probes'
    write_executable(bin_dir / 'codex', '''#!/usr/bin/env python3
import json, os
from pathlib import Path
p = Path(os.environ['PROBE_COUNTER'])
p.write_text(p.read_text() + 'probe\\n' if p.exists() else 'probe\\n')
print(json.dumps({'models': [{'slug': 'gpt-6-luna', 'supported_reasoning_levels': [{'effort': 'high'}]}]}))
''')
    result = subprocess.run([sys.executable, str(SCRIPT), '--preflight-json'], cwd=tmp_path,
        input=json.dumps({'tasks': [{'id': f't{i}', 'adapter': 'codex', 'model': 'gpt-6-luna',
            'effort': 'high', 'prompt': 'hello'} for i in range(3)]}), text=True, capture_output=True,
        env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': str(tmp_path / '.agents'), 'PROBE_COUNTER': str(counter),
             'PATH': str(bin_dir) + os.pathsep + os.environ['PATH']})
    record = json.loads(result.stdout)
    assert record['status'] == 'validated', record
    assert len(record['routes']) == 3
    assert not counter.exists()


@pytest.mark.parametrize('owner', ['dispatch', 'batch'])
@pytest.mark.parametrize('instance', ['configured', 'missing', 'unset'])
@pytest.mark.parametrize('mode', ['read_only', 'worktree_write'])
def test_provider_does_not_inherit_chair_fabric_identity(tmp_path, owner, instance, mode):
    policy = tmp_path / '.agents/fabric-policy.json'
    policy.parent.mkdir()
    policy.write_text('{"memory_floor_percent":{"read_only":0,"worktree_write":0}}')
    run_dir = make_run(tmp_path, 'isolated-provider')
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('Reply OK')
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    write_executable(bin_dir / 'codex', '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
if sys.argv[1:3] == ['debug', 'models']:
    print(json.dumps({'models': [{'slug': 'gpt-6-luna', 'supported_reasoning_levels': [{'effort': 'high'}]}]}))
else:
    names = ['AGENT_FABRIC_STATE_DIRECTORY', 'AGENT_FABRIC_SEAT', 'AGENT_FABRIC_CLIENT_LABEL',
             'AGENT_FABRIC_LABEL', 'AGENT_FABRIC_PRODUCT_ROOT']
    names += [k for k in os.environ if k.startswith(('PROVENANT_RUN_', 'PROVENANT_PREFLIGHT_'))]
    names += ['PROVENANT_FABRIC_PHASES']
    Path(os.environ['PROVIDER_ENV_CAPTURE']).write_text(json.dumps({k: os.environ[k] for k in names if k in os.environ}))
    Path(os.environ['PROVIDER_INSTANCE_CAPTURE']).write_text(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT', ''))
    Path(os.environ['PROVIDER_TMP_CAPTURE']).write_text(os.environ['TMPDIR'])
    sys.stdin.read()
    print('OK')
''')
    capture = tmp_path / 'provider-env.json'
    env = {**os.environ, 'PATH': f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}",
           'AGENT_FABRIC_INSTANCE_ROOT': str(tmp_path / '.agents'), 'AGENT_FABRIC_PRODUCT_ROOT': str(ROOT),
           'AGENT_FABRIC_STATE_DIRECTORY': str(tmp_path / 'chair-state'),
           'AGENT_FABRIC_SEAT': 'claude', 'AGENT_FABRIC_CLIENT_LABEL': 'chair-client',
           'AGENT_FABRIC_LABEL': 'chair-label', 'PROVIDER_ENV_CAPTURE': str(capture),
           'PROVIDER_TMP_CAPTURE': str(tmp_path / 'provider-tmp.txt'),
           'PROVENANT_RUN_TOKEN': 'mcp-fixture-token', 'PROVENANT_RUN_DIR': str(run_dir),
           'PROVENANT_PREFLIGHT_ROUTES': '{}', 'PROVENANT_RUN_PARENT_TOKEN': 'parent-token',
           'PROVENANT_FABRIC_PHASES': '{"validate":1}',
           'PROVIDER_INSTANCE_CAPTURE': str(tmp_path / 'provider-instance.txt')}
    if instance != 'configured':
        env['HOME'] = str(tmp_path / 'home')
        env['AGENT_FABRIC_INSTANCE_ROOT'] = str(tmp_path / 'missing-instance')
    if instance == 'unset':
        env.pop('AGENT_FABRIC_INSTANCE_ROOT', None)
    expected_instance = env.get('AGENT_FABRIC_INSTANCE_ROOT', '')
    worktree = make_worktree(tmp_path) if mode == 'worktree_write' else None
    if owner == 'dispatch':
        command = [str(SCRIPT), '--run-dir', str(run_dir), '--adapter', 'codex',
                   '--prompt-file', str(prompt), '--alias', 'senior', '--role', 'worker']
        if worktree is not None:
            command += ['--access-mode', mode, '--worktree', str(worktree)]
    else:
        manifest = tmp_path / 'tasks.json'
        manifest.write_text(json.dumps({'schema_version': 1, 'tasks': [
            {'id': 'isolated', 'adapter': 'codex', 'prompt_file': str(prompt),
             'alias': 'senior', 'role': 'worker', 'access_mode': mode,
             **({'worktree': str(worktree)} if worktree is not None else {})}]}))
        command = [str(SCRIPT.with_name('batch_run.py')), '--run-dir', str(run_dir), '--manifest', str(manifest)]
    result = subprocess.run(command, cwd=tmp_path, env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stdout + result.stderr
    run_id = json.loads((run_dir / 'RUN_RECEIPT.json').read_text())['run_id']
    expected = {"PROVENANT_RUN_ID": run_id}
    if mode == 'worktree_write':
        expected['AGENT_FABRIC_STATE_DIRECTORY'] = env['AGENT_FABRIC_STATE_DIRECTORY']
    assert json.loads(capture.read_text()) == expected
    assert (tmp_path / 'provider-instance.txt').read_text() == expected_instance
    assert not (tmp_path / 'chair-state').exists()
    scratch = Path((tmp_path / 'provider-tmp.txt').read_text())
    # The provider tmp lives inside its attempt and is pruned once the attempt is terminal.
    assert scratch.is_relative_to(run_dir.resolve()) and not scratch.exists()
    assert json.loads((run_dir / 'RUN_RECEIPT.json').read_text())['status'] == 'ok'


@pytest.mark.parametrize("floor,free,mode,blocked", [
    (None, 500 * 1024**2 - 1, "worktree_write", True),
    (None, 500 * 1024**2, "worktree_write", False),
    (100, 99, "worktree_write", True),
    (100, 100, "worktree_write", False),
    (0, 0, "worktree_write", False),
    (None, 500 * 1024**2 - 1, "read_only", True),
    (None, 500 * 1024**2, "read_only", False),
    (100, 99, "read_only", True),
    (100, 100, "read_only", False),
    (0, 0, "read_only", False),
])
def test_disk_admission_floor(tmp_path, monkeypatch, floor, free, mode, blocked):
    marker = tmp_path / "provider-started"
    code = f"import json,sys\nfrom pathlib import Path\nsys.stdin.read()\nPath({str(marker)!r}).touch()\nprint(json.dumps({{'type':'result','result':'DONE'}}))\n"
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path) if mode == "worktree_write" else None
    if worktree:
        command += ["--access-mode", mode, "--worktree", str(worktree)]
    if floor is not None:
        policy = tmp_path / ".agents/fabric-policy.json"
        value = json.loads(policy.read_text())
        value["disk_floor_bytes"] = {mode: floor}
        policy.write_text(json.dumps(value))
    monkeypatch.setenv("PROVENANT_HOST_LOCAL_ONLY", "1")
    monkeypatch.delenv("PROVENANT_RUN_ID", raising=False)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    probes = []
    def disk_usage(path):
        probes.append(path)
        return SimpleNamespace(free=free)
    monkeypatch.setattr(mod.shutil, "disk_usage", disk_usage)
    assert mod.dispatch(mod.parser().parse_args(command[2:])) == (1 if blocked else 0)
    legacy = json.loads((run / "dispatch/tasks/dispatch-001/attempt-001/attempt.json").read_text())
    row = json.loads((run / "tasks/dispatch-001/attempt-001/attempt.json").read_text())
    assert marker.exists() is not blocked
    if blocked:
        assert probes == [run]
        assert legacy["outcome"] == row["error"] == "host_disk_full"
        assert row["error_class"] == legacy["error_class"] == "host_resource"
        assert legacy["process"]["pid"] is None
        assert "disk_floor_bytes" in row["fix"]


@pytest.mark.parametrize("started,wrapped,number,outcome", [
    (True, True, 28, "host_disk_full"),
    (True, False, 28, "host_disk_full"),
    (True, False, 5, "provider_lost"),
    (False, False, 28, "host_disk_full"),
])
def test_process_io_failure_classification_and_terminal_evidence(tmp_path, monkeypatch, started, wrapped, number, outcome):
    code = "import json,sys\nsys.stdin.read()\nprint(json.dumps({'type':'result','result':'DONE'}), flush=True)\n"
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    original_write = mod.run_records.write_owned
    original_open = os.open
    live = []
    injected = []
    def fail():
        injected.append(True)
        error = OSError(number, "No space left on device" if number == 28 else "Input/output error")
        if wrapped:
            raise mod.OwnedFileError("attempt receipt cannot be written safely") from error
        raise error
    def write(run_dir, path, content):
        if path.name == "attempt.json":
            row = json.loads(content)
            if row.get("state") == "running" and row.get("pgid"):
                live.append(row)
        if started and path.name == "adapter-receipt.json" and not injected:
            fail()
        return original_write(run_dir, path, content)
    def open_file(path, *args, **kwargs):
        if not started and Path(path).name == "events.jsonl":
            fail()
        return original_open(path, *args, **kwargs)
    monkeypatch.setattr(mod.run_records, "write_owned", write)
    monkeypatch.setattr(mod.os, "open", open_file)
    assert mod.dispatch(mod.parser().parse_args(command[2:])) == 1
    legacy = json.loads((run / "dispatch/tasks/dispatch-001/attempt-001/attempt.json").read_text())
    row = json.loads((run / "tasks/dispatch-001/attempt-001/attempt.json").read_text())
    assert injected
    assert legacy["outcome"] == outcome
    assert legacy["process"]["observed_exit"] is False
    assert (legacy["process"]["pid"] is not None) == started
    if started:
        assert row["error_class"] == legacy["error_class"] == ("host_resource" if number == 28 else "provider_error")
        assert row["pgid"] == legacy["process"]["pid"] == live[-1]["pgid"]
        assert row["pgid_started_at"] == live[-1]["pgid_started_at"]
        assert row["last_progress_at"] == live[-1]["last_progress_at"]
        assert row["process_error"] == legacy["process_error"]
        assert row["evidence"]["signature"] == outcome
        assert f"Errno {number}" in row["evidence"]["excerpt"]
        assert row["evidence"]["pgid_probe"] == "gone"
        events = run / "dispatch/tasks/dispatch-001/attempt-001/events.jsonl"
        assert row["evidence"]["events_bytes"] == events.stat().st_size
        assert row["evidence"]["stderr_bytes"] == (events.parent / "stderr.log").stat().st_size
        assert row["evidence"]["last_event_at"] == mod.datetime.fromtimestamp(events.stat().st_mtime, mod.UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")


@pytest.mark.parametrize("stage", ["progress", "start"])
def test_progress_bookkeeping_enospc_does_not_signal_provider(tmp_path, monkeypatch, stage):
    code = """import json,sys,time
sys.stdin.read()
print(json.dumps({'type':'system','subtype':'init','session_id':'disk-test','model':'opus'}), flush=True)
time.sleep(0.2)
print(json.dumps({'type':'result','result':'DONE'}), flush=True)
"""
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    original_write = mod.run_records.write_owned
    failed = []
    retried = []
    signals = []
    original_killpg = os.killpg
    def killpg(pgid, sig):
        if sig:
            try:
                original_killpg(pgid, 0)
            except ProcessLookupError:
                pass
            else:
                signals.append((pgid, sig))
        return original_killpg(pgid, sig)
    def write(run_dir, path, content):
        if stage == "start" and path.name == "pgid" and not failed:
            failed.append(True)
            raise mod.OwnedFileError("pgid write failed") from OSError(28, "No space left on device")
        if path.name == "attempt.json":
            row = json.loads(content)
            if row.get("state") == "running" and row.get("pgid") and row["last_progress_at"] != row["started_at"]:
                if stage == "progress" and not failed:
                    failed.append(True)
                    raise mod.OwnedFileError("progress write failed") from OSError(28, "No space left on device")
                if failed:
                    retried.append(True)
        return original_write(run_dir, path, content)
    monkeypatch.setattr(mod.run_records, "write_owned", write)
    monkeypatch.setattr(mod.os, "killpg", killpg)
    assert mod.dispatch(mod.parser().parse_args(command[2:])) == 0
    legacy = json.loads((run / "dispatch/tasks/dispatch-001/attempt-001/attempt.json").read_text())
    row = json.loads((run / "tasks/dispatch-001/attempt-001/attempt.json").read_text())
    assert failed and retried
    assert signals == []
    assert legacy["process"]["observed_exit"] is True
    assert row["status"] == "ok"
    assert len([warning for warning in row["warnings"] if "provider bookkeeping write failed" in warning]) == 1


@pytest.mark.parametrize("failure", ["probe", "policy"])
def test_disk_admission_probe_warning_and_policy_fix(tmp_path, monkeypatch, failure):
    code = "import json,sys\nsys.stdin.read()\nprint(json.dumps({'type':'result','result':'DONE'}))\n"
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    if failure == "probe":
        def probe(path):
            raise OSError(5, "disk probe unavailable")
        monkeypatch.setattr(mod.shutil, "disk_usage", probe)
    else:
        policy = tmp_path / ".agents/fabric-policy.json"
        value = json.loads(policy.read_text())
        value["disk_floor_bytes"] = {"read_only": -1}
        policy.write_text(json.dumps(value))
    assert mod.dispatch(mod.parser().parse_args(command[2:])) == (0 if failure == "probe" else 1)
    row = json.loads((run / "tasks/dispatch-001/attempt-001/attempt.json").read_text())
    if failure == "probe":
        assert any("disk admission probe failed" in warning for warning in row["warnings"])
        assert row["status"] == "ok"
    else:
        assert row["fix"].startswith("Set .agents/fabric-policy.json disk_floor_bytes")


@pytest.mark.parametrize("source", ["pgid", "provider"])
@pytest.mark.parametrize("disk_full", ["stderr", "space", None])
def test_reap_attempt_recovers_missing_pgid(tmp_path, monkeypatch, source, disk_full):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "runs/missing-pgid")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=None, pgid_started_at=None,
               workspace_root=str(tmp_path), last_progress_at="2000-01-01T00:00:00Z")
    path.write_text(json.dumps(row))
    attempt = run / "dispatch/tasks/task-1/attempt-001"
    attempt.mkdir(parents=True)
    if source == "pgid":
        (attempt / "pgid").write_text("123456\n")
    else:
        (run / "dispatch-provider.json").write_text(json.dumps({
            "run_token": mod.run_records.run_identity(run), "provider_pgid": 123456,
            "provider_pid": 123456, "provider_started_at": "recorded",
        }))
    (path.parent / "stderr.log").write_text("No space left on device" if disk_full == "stderr" else "diagnostic")
    def gone(pgid, sig):
        assert (pgid, sig) == (123456, 0)
        raise ProcessLookupError()
    monkeypatch.setattr(mod.os, "killpg", gone)
    monkeypatch.setattr(mod.shutil, "disk_usage", lambda path: SimpleNamespace(
        free=0 if disk_full == "space" else 1024**3))
    assert mod.reap_attempts(run) == [str(path)]
    terminal = json.loads(path.read_text())
    assert terminal["pgid"] == 123456
    assert terminal["state"] == "terminal"
    disk_proven = disk_full == "stderr"
    assert terminal["evidence"]["signature"] == ("host_disk_full" if disk_proven else "pgid_gone")
    assert terminal["error_class"] == ("host_resource" if disk_proven else "provider_error")
    assert terminal["status"] == ("failed" if disk_proven else "lost")
    assert terminal["retryable"] is (not disk_proven)
    assert ("low_free_space_hint" in terminal["evidence"]) is (disk_full == "space")
    assert not (run / "dispatch-status.json").exists()


def test_attempt_warnings_do_not_leak_between_attempts(tmp_path, monkeypatch):
    code = "import json,sys\nsys.stdin.read()\nprint(json.dumps({'type':'result','result':'DONE'}))\n"
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    policy = tmp_path / ".agents/fabric-policy.json"
    policy.write_text(json.dumps({"memory_floor_percent": {"read_only": 5}}))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    original_open = os.open
    original_write = mod.run_records.write_owned
    first = [True]
    def open_file(path, *args, **kwargs):
        if first[0] and Path(path).name == "admission.lock":
            raise OSError(5, "lock unavailable")
        return original_open(path, *args, **kwargs)
    def write(run_dir, path, content):
        if first[0] and path.name == "pgid":
            raise OSError(28, "No space left on device")
        return original_write(run_dir, path, content)
    monkeypatch.setattr(mod.os, "open", open_file)
    monkeypatch.setattr(mod.run_records, "write_owned", write)
    monkeypatch.setattr(mod.memory_admission, "available_memory_mb", lambda: (100, 100))
    args = mod.parser().parse_args(command[2:] + ["--batch-child"])
    assert mod.dispatch(args) == 0
    first[0] = False
    assert mod.dispatch(args) == 0
    rows = [json.loads(path.read_text()) for path in sorted(run.glob("tasks/*/attempt-*/attempt.json"))]
    assert any("memory admission lock unavailable" in warning for warning in rows[0]["warnings"])
    assert any("provider bookkeeping write failed" in warning for warning in rows[0]["warnings"])
    assert not any("memory admission lock unavailable" in warning or "provider bookkeeping write failed" in warning
                   for warning in rows[1]["warnings"])


def test_disk_admission_rechecks_after_memory_wait(tmp_path, monkeypatch):
    code = "import json,sys\nsys.stdin.read()\nprint(json.dumps({'type':'result','result':'DONE'}))\n"
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    (tmp_path / ".agents/fabric-policy.json").write_text(json.dumps({"memory_floor_percent": {"read_only": 5}}))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    waited = []
    def probe():
        waited.append(True)
        return (100, 100) if len(waited) > 1 else (0, 100)
    monkeypatch.setattr(mod.memory_admission, "available_memory_mb", probe)
    monkeypatch.setattr(mod.memory_admission, "POLL_SECONDS", 0.001)
    monkeypatch.setattr(mod.shutil, "disk_usage", lambda path: SimpleNamespace(free=0 if waited else 1024**3))
    assert mod.dispatch(mod.parser().parse_args(command[2:])) == 1
    row = json.loads((run / "tasks/dispatch-001/attempt-001/attempt.json").read_text())
    assert len(waited) >= 2
    assert row["error"] == "host_disk_full"
    assert row["pgid"] is None


@pytest.mark.parametrize("failure", ["adapter", "stderr", "terminal", "terminal_once"])
def test_terminal_io_is_best_effort(tmp_path, monkeypatch, failure):
    code = "import json,sys\nsys.stdin.read()\nprint(json.dumps({'type':'result','result':'DONE'}))\n"
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    original_write = mod.run_records.write_owned
    original_open = os.open
    failed = []
    def open_file(path, *args, **kwargs):
        if not failure.startswith("terminal") and Path(path).name == "events.jsonl":
            raise OSError(28, "No space left on device")
        return original_open(path, *args, **kwargs)
    def write(run_dir, path, content):
        terminal = (path.name == "attempt.json" and json.loads(content).get("state") == "terminal")
        if ((failure == "adapter" and path.name == "adapter-receipt.json")
                or (failure == "stderr" and path.name == "stderr.log")
                or (failure.startswith("terminal") and terminal and (failure == "terminal" or not failed))):
            failed.append(path)
            raise mod.OwnedFileError("publication failed") from OSError(28, "No space left on device")
        return original_write(run_dir, path, content)
    monkeypatch.setattr(mod.os, "open", open_file)
    monkeypatch.setattr(mod.run_records, "write_owned", write)
    args = mod.parser().parse_args(command[2:])
    assert mod.dispatch(args) == (0 if failure.startswith("terminal") else 1)
    assert failed
    assert args._last_row["state"] == "terminal"
    if not failure.startswith("terminal"):
        assert args._last_row["evidence"]["signature"] == "host_disk_full"
    elif failure == "terminal":
        assert (run / "dispatch/tasks/dispatch-001/attempt-001/tmp/children").is_dir()
    else:
        assert not (run / "dispatch/tasks/dispatch-001/attempt-001/tmp").exists()


def test_local_writer_launch_does_not_read_the_host_configuration(tmp_path, monkeypatch):
    code = '''import json,sys
sys.stdin.read()
print(json.dumps({'type':'system','subtype':'init','session_id':'writer-session','model':'opus'}))
print(json.dumps({'type':'result','result':'DONE'}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path)
    instance = tmp_path / 'writer-instance'
    (instance / '.agent-fabric/writer-owners').mkdir(parents=True)
    (instance / '.agent-fabric/hosts.json').write_text('{broken')
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.setenv('PROVENANT_HOST_LOCAL_ONLY', '1')
    monkeypatch.delenv('PROVENANT_RUN_ID', raising=False)
    result = subprocess.run([*command, '--access-mode', 'worktree_write', '--worktree', str(worktree)],
                            cwd=tmp_path, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stdout + result.stderr


@pytest.mark.parametrize('ownership', ['current', 'superseded', 'unresolved', 'stale'])
def test_local_writer_resume_requires_its_durable_context_owner(tmp_path, monkeypatch, ownership):
    """A transported run needs its claim; an ordinary local writer clears a stale one."""
    code = '''import json,sys
from pathlib import Path
sys.stdin.read()
Path('provider-launched.txt').write_text('writer ran')
print(json.dumps({'type':'system','subtype':'init','session_id':'writer-session','model':'opus'}))
print(json.dumps({'type':'result','result':'DONE'}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path)
    instance = tmp_path / 'writer-instance'
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.setenv('PROVENANT_HOST_LOCAL_ONLY', '1')
    monkeypatch.delenv('PROVENANT_RUN_ID', raising=False)
    first = subprocess.run([*command, '--access-mode', 'worktree_write', '--worktree', str(worktree)],
                           cwd=tmp_path, capture_output=True, text=True, timeout=30)
    assert first.returncode == 0, first.stdout + first.stderr
    previous = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())
    provider_effect = worktree / 'provider-launched.txt'
    assert provider_effect.exists()
    provider_effect.unlink()
    git_dir = subprocess.check_output(
        ['git', '-C', str(worktree), 'rev-parse', '--absolute-git-dir'], text=True).strip()
    state = instance / '.agent-fabric'
    owners = state / 'writer-owners'
    owners.mkdir(parents=True)
    operations = state / 'operations'
    operations.mkdir()
    marker = owners / (hashlib.sha256(git_dir.encode()).hexdigest() + '.json')
    operation = 'current-writer' if ownership in {'current', 'stale'} else 'replacement-writer'
    marker.write_text(json.dumps({'operation_id': operation}))
    if ownership != 'stale':
        # A peer operation launched this run for a transported writer.
        (operations / 'current-writer.json').write_text(json.dumps({
            'runId': previous['run_id'], 'taskId': 'dispatch-001', 'attempt': 1}))
        (state / 'writer-contexts').mkdir()
        (state / 'writer-contexts/current-writer.json').write_text('[]')
    if ownership == 'superseded':
        (operations / 'replacement-writer.json').write_text(json.dumps({
            'runId': 'mcp-replacement', 'taskId': 'dispatch-001', 'attempt': 1}))
    resumed = subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run),
                              '--resume', previous['run_id'], '--prompt-file', str(prompt)],
                             cwd=tmp_path, capture_output=True, text=True, timeout=30)
    if ownership in {'current', 'stale'}:
        assert resumed.returncode == 0, resumed.stdout + resumed.stderr
        assert provider_effect.exists()
        assert (run / 'tasks/dispatch-001/attempt-002/attempt.json').is_file()
    else:
        assert resumed.returncode != 0, resumed.stdout + resumed.stderr
        assert json.loads(resumed.stdout)['error'] == 'writer_context_superseded'
        assert not provider_effect.exists()
        assert not (run / 'tasks/dispatch-001/attempt-002/attempt.json').exists()
    if ownership == 'stale':
        assert not marker.exists()
    else:
        assert json.loads(marker.read_text()) == {'operation_id': operation}
    lease_owner = load_dispatch_module()
    lease = lease_owner.writer_lease.acquire_worktree_lease(worktree)
    lease_owner.writer_lease.release_worktree_lease(lease)


def test_normal_attempt_records_reaped_new_session_child(tmp_path, monkeypatch):
    pid_path = tmp_path / "leftover.pid"
    code = f'''import json, pathlib, subprocess, sys, time
sys.stdin.read()
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                         start_new_session=True, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))
time.sleep(1.1)  # Let the supervisor's process census observe the detached child.
print(json.dumps({{'type': 'result', 'result': 'DONE', 'is_error': False}}), flush=True)
'''
    run, _prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    try:
        result = subprocess.run(command, cwd=tmp_path, capture_output=True, text=True, timeout=12)
        assert result.returncode == 0, result.stdout + result.stderr
        pid = int(pid_path.read_text())
        row = json.loads((run / "tasks/dispatch-001/attempt-001/attempt.json").read_text())
        assert row["status"] == "ok"
        assert any(item["pid"] == pid for item in row["reaped"])
        assert "! reaped 1 leftover process(es)" in row["digest"]
        route_health = json.loads(Path(os.environ["AGENT_FABRIC_ROUTE_HEALTH_PATH"]).read_text())
        assert any(
            route.get("task_class") == "ordinary"
            and route.get("recent", [{}])[0].get("status") == "ok"
            for route in route_health["routes"].values()
        )
        with pytest.raises(ProcessLookupError):
            os.kill(pid, 0)
    finally:
        if pid_path.exists():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def isolate_fabric_plan_env(monkeypatch, tmp_path):
    for key in tuple(os.environ):
        if key == 'PROVENANT_RUN_ID' or key.startswith('AGENT_FABRIC_'):
            monkeypatch.delenv(key, raising=False)
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))


def test_close_mcp_run_reads_legacy_successful_canonical_attempt(tmp_path):
    mod = load_dispatch_module()
    run = Path(subprocess.check_output([str(INIT), '--kind', 'dispatch'], cwd=tmp_path, text=True).strip())
    row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
    row['status'] = 'succeeded'
    path = run / 'tasks/task-1/attempt-001/attempt.json'
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps(row))
    mod.close_mcp_run(run)
    receipt = json.loads((run / 'RUN_RECEIPT.json').read_text())
    assert receipt['status'] == 'ok'
    assert receipt['attempts'] == [{**row, 'status': 'ok'}]
    assert not (run / 'dispatch-status.json').exists()


def test_close_mcp_run_preserves_input_required_for_resumption(tmp_path):
    mod = load_dispatch_module()
    run = Path(subprocess.check_output([str(INIT), '--kind', 'dispatch'], cwd=tmp_path, text=True).strip())
    row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
    row.update(status='input_required', question='Which branch?')
    path = run / 'tasks/task-1/attempt-001/attempt.json'
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps(row))
    mod.close_mcp_run(run)
    receipt = json.loads((run / 'RUN_RECEIPT.json').read_text())
    assert receipt['status'] == 'input_required'
    assert receipt['resumable'] is True


def test_close_mcp_run_includes_batch_tasks_without_canonical_attempts(tmp_path):
    mod = load_dispatch_module()
    run = Path(subprocess.check_output([str(INIT), '--kind', 'batch'], cwd=tmp_path, text=True).strip())
    row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
    path = run / 'tasks/finished/attempt-001/attempt.json'
    path.parent.mkdir(parents=True)
    row['task_id'] = 'finished'
    path.write_text(json.dumps(row))
    summary = run / 'dispatch/batches/batch-001/summary.json'
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({'status': 'completed', 'tasks': [
        {'task_id': 'finished', 'status': 'ok'},
        {'task_id': 'busy', 'status': 'worktree_busy'},
    ]}))
    mod.close_mcp_run(run)
    assert json.loads((run / 'RUN_RECEIPT.json').read_text())['status'] == 'failed'


@pytest.mark.parametrize('stop', ['marker', 'SIGTERM', 'timeout'])
def test_real_dispatcher_stops_group_releases_writer_lease_and_closes_receipt(tmp_path, monkeypatch, stop):
    code = '''import json, os, subprocess, sys, time
from pathlib import Path
sys.stdin.read()
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
Path('provider-pids').write_text(json.dumps([os.getpid(), child.pid]))
print(json.dumps({'type':'system','session_id':'cancel-session','model':'opus'}), flush=True)
time.sleep(30)
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path)
    command += ['--access-mode', 'worktree_write', '--worktree', str(worktree), '--timeout', '1' if stop == 'timeout' else '15']
    process = subprocess.Popen(command, cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 10
        while not (worktree / 'provider-pids').exists() and time.monotonic() < deadline and process.poll() is None:
            time.sleep(.02)
        assert (worktree / 'provider-pids').exists()
        mod = load_dispatch_module()
        if stop != 'timeout':
            active = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())
            assert active['pgid_started_at']
            assert active['pgid_started_at'] == mod.process_info.start_time(active['pgid'])
        with pytest.raises(mod.writer_lease.WorktreeLeaseError):
            mod.writer_lease.acquire_worktree_lease(worktree)
        if stop == 'SIGTERM':
            process.send_signal(signal.SIGTERM)
        elif stop == 'marker':
            mod.run_records.create_cancellation_marker(run, run / 'dispatch/tasks/dispatch-001/attempt-001')
        stdout, stderr = process.communicate(timeout=10)
        row = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())
        expected_status = 'timed_out' if stop == 'timeout' else ('interrupted' if stop == 'SIGTERM' else 'cancelled')
        assert row['status'] == expected_status, stdout + stderr
        assert row['evidence']['exit'] is not None
        pids = json.loads((worktree / 'provider-pids').read_text())
        for pid in pids:
            with pytest.raises(ProcessLookupError):
                os.kill(pid, 0)
        lease = mod.writer_lease.acquire_worktree_lease(worktree)
        mod.writer_lease.release_worktree_lease(lease)
        receipt = json.loads((run / 'RUN_RECEIPT.json').read_text())
        assert receipt['status'] == ('cancelled' if stop == 'marker' else 'failed')
    finally:
        if process.poll() is None:
            process.terminate()
            process.communicate(timeout=10)


def test_real_dispatcher_restores_signal_handlers(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys,json\nsys.stdin.read()\nprint(json.dumps({"type":"result","result":"DONE"}))\n')
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)}
    assert mod.dispatch(mod.parser().parse_args(command[2:])) == 0
    assert {sig: signal.getsignal(sig) for sig in handlers} == handlers


def test_real_dispatcher_records_attempt_phase_timings(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json\nsys.stdin.read()\nprint(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('PROVENANT_FABRIC_PHASES', json.dumps({'owner_started_at_ms': time.time() * 1000}))
    assert subprocess.run(command, cwd=tmp_path, capture_output=True).returncode == 0
    attempt = json.loads(next(run.glob('tasks/*/attempt-*/attempt.json')).read_text())
    phases = attempt['timing']['phases']
    assert set(phases) == {'validate', 'run_dir_init', 'owner_setup', 'route_plan', 'snapshot',
                           'spawn', 'provider', 'finalize'}
    assert phases['validate'] is None
    assert all(type(phases[name]) in (int, float) and phases[name] >= 0
               for name in ('owner_setup', 'route_plan', 'spawn', 'provider', 'finalize'))
    assert phases['route_plan'] > 0


def test_fallback_reentry_does_not_charge_front_door_phases_again(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json\nsys.stdin.read()\n'
        'if any("opus" in item for item in sys.argv): print("model unavailable", file=sys.stderr); sys.exit(1)\n'
        'print(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('PROVENANT_FABRIC_PHASES', json.dumps({
        'validate': 12, 'run_dir_init': 34, 'snapshot': 56,
        'owner_started_at_ms': time.time() * 1000,
    }))
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    args.fallback = json.dumps(['claude/sonnet'])
    assert mod.dispatch(args) == 0
    attempts = sorted(run.glob('tasks/*/attempt-*/attempt.json'))
    first, second = (json.loads(path.read_text()) for path in attempts)
    assert [first['timing']['phases'][name] for name in ('validate', 'run_dir_init', 'snapshot')] == [12, 34, 56]
    assert all(second['timing']['phases'][name] is None for name in
               ('validate', 'run_dir_init', 'snapshot', 'owner_setup'))


def test_startup_timeout_falls_back_when_the_dispatch_allows(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json,time\nsys.stdin.read()\n'
        'if any("opus" in item for item in sys.argv): time.sleep(60)\n'
        'print(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('CF_DISPATCH_STARTUP_SECONDS', '1')
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    args.fallback = json.dumps(['claude/sonnet'])
    assert mod.dispatch(args) == 0
    attempts = sorted(run.glob('tasks/*/attempt-*/attempt.json'))
    first, second = (json.loads(path.read_text()) for path in attempts)
    assert first['status'] == 'startup_timeout'
    assert first['retryable'] is True
    assert first['evidence']['signature'] == 'startup_watchdog'
    assert first['digest'].startswith('startup_timeout ')
    assert 're-route to another model or adapter' in first['digest']
    assert second['status'] == 'ok'
    assert second['provenance']['fallback_from']['status'] == 'startup_timeout'


@pytest.mark.parametrize('edits', [None, 'partial.txt', 'config/installation.json', 'local-config/settings.json', 'cap'])
def test_writer_startup_timeout_falls_back_only_over_an_untouched_worktree(tmp_path, monkeypatch, edits):
    path = edits if edits not in {None, 'cap'} else None
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json,time\nfrom pathlib import Path\nsys.stdin.read()\n'
        'if any("opus" in item for item in sys.argv):\n'
        + (f'    Path({path!r}).write_text("half done")\n' if path else '')
        # Churn under skipped directories never blocks fallback.
        + '    for churn in ("node_modules/launch.lock", "pkg/__pycache__/mod.pyc", ".pytest_cache/v"):\n'
        '        Path(churn).write_text("launch churn")\n'
        '    time.sleep(60)\n'
        'print(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    worktree = make_worktree(tmp_path)
    # An ignored file, a file inside an ignored directory, and ignored caches.
    for name in ('config/installation.json', 'local-config/settings.json', 'node_modules/cache',
                 'pkg/__pycache__/old.pyc', '.pytest_cache/v'):
        (worktree / name).parent.mkdir(parents=True, exist_ok=True)
        (worktree / name).write_text('{}')
    exclude = Path(subprocess.check_output(['git', '-C', str(worktree), 'rev-parse', '--git-path', 'info/exclude'],
                                           text=True).strip())
    exclude = exclude if exclude.is_absolute() else worktree / exclude
    exclude.parent.mkdir(parents=True, exist_ok=True)
    exclude.write_text('config/installation.json\nlocal-config/\nnode_modules/\npkg/\n.pytest_cache/\n')
    time.sleep(1.2)  # ctime must predate the launch tolerance; utime cannot set it
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('CF_DISPATCH_STARTUP_SECONDS', '1')
    mod = load_dispatch_module()
    if edits == 'cap':
        monkeypatch.setattr(mod.writer_lease, 'STARTUP_WALK_MAX_ENTRIES', 2)
    args = mod.parser().parse_args(command[2:] + ['--access-mode', 'worktree_write', '--worktree', str(worktree)])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    args.fallback = json.dumps(['claude/sonnet'])
    mod.dispatch(args)
    attempts = [json.loads(path.read_text()) for path in sorted(run.glob('tasks/*/attempt-*/attempt.json'))]
    assert attempts[0]['status'] == 'startup_timeout'
    if edits:
        assert len(attempts) == 1
        assert attempts[0]['retryable'] is False
        assert attempts[0]['fix'].startswith('inspect the worktree')
    else:
        assert [row['status'] for row in attempts] == ['startup_timeout', 'ok']
    if path:
        assert (worktree / path).read_text() == 'half done'


def test_finalize_phase_includes_receipt_publication(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json\nsys.stdin.read()\nprint(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    publish = mod.publish_contract

    def delayed_publish(run_dir, row):
        if row.get('state') == 'terminal':
            time.sleep(0.12)
        return publish(run_dir, row)

    monkeypatch.setattr(mod, 'publish_contract', delayed_publish)
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    assert mod.dispatch(args) == 0
    attempt = json.loads(next(run.glob('tasks/*/attempt-*/attempt.json')).read_text())
    assert attempt['timing']['phases']['finalize'] >= 120


def normalise_plan_identity(plan):
    """Replace the per-plan session id and Codex permissions profile name, which differ by design."""
    for key, placeholder in (('session_id', '<session>'), ('profile', '<profile>')):
        owner = plan if key == 'session_id' else plan['applied']['write_boundary']
        value = owner.pop(key, None)
        plan['argv'] = [arg.replace(value, placeholder) if value else arg for arg in plan['argv']]


@pytest.mark.parametrize('adapter, model, effort', [
    ('claude', 'opus', None), ('codex', 'gpt-6-luna', 'low'),
])
def test_fabric_fast_plan_matches_shell_for_explicit_model(tmp_path, monkeypatch, adapter, model, effort):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json\nsys.stdin.read()\nprint(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    instance = tmp_path / 'instance'
    config = instance / 'config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {'pro': {'identity_dir': str(tmp_path / 'identity')}}}}))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.chdir(tmp_path)
    if adapter == 'codex':
        write_executable(tmp_path / 'provider-bin/codex', '#!/bin/sh\n[ "$1" = mcp ] && printf "[]"\nexit 0\n')
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.tool = adapter
    args.model = model
    args.effort = effort
    args.account = "pro"  # A fallback to Codex must drop this Claude-only selector.
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    result = run / 'result.md'
    shell = subprocess.run(
        [*mod.build_command(args, prompt, result), '--plan-only'],
        cwd=tmp_path, env=mod.dispatch_preflight.routing_environment(preserve_instance=True), capture_output=True, text=True,
    )
    assert shell.returncode == 0, shell.stdout + shell.stderr
    planned = mod.dispatch_preflight.fast_fabric_plan(args, prompt, result, tmp_path)
    assert planned is not None
    expected = json.loads(shell.stdout)
    for value in (planned, expected):
        normalise_plan_identity(value)
    assert planned == expected
    if adapter == "claude":
        assert planned["account"] == "pro"
    else:
        assert "account" not in planned


@pytest.mark.parametrize('adapter, model, effort, content', [
    ('claude', 'opus', None, 'a\r\nb\rc'),
    ('codex', 'gpt-6-luna', 'max', 'hello'),
    ('codex', 'gpt-6-luna', 'bogus', 'hello'),
    ('claude', 'opus', 'max', 'hello'),
    ('claude', 'some-unknown-model', None, 'hello'),
    ('claude', '-x', None, 'hello'),
    ('codex', 'gpt-5', 'high', 'hello'),
])
def test_fabric_fast_plan_matches_or_delegates_shell_edge_routes(
    tmp_path, monkeypatch, adapter, model, effort, content,
):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    instance = tmp_path / 'instance'
    config = instance / 'config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {'pro': {'identity_dir': str(tmp_path / 'identity')}}}}))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.chdir(tmp_path)
    if adapter == 'codex':
        write_executable(tmp_path / 'provider-bin/codex', '#!/bin/sh\n[ "$1" = mcp ] && printf "[]"\nexit 0\n')
    prompt.write_bytes(content.encode())
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.tool, args.model, args.effort = adapter, model, effort
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    result = run / 'result.md'
    shell = subprocess.run([*mod.build_command(args, prompt, result), '--plan-only'],
                           cwd=tmp_path, env=mod.dispatch_preflight.routing_environment(preserve_instance=True), capture_output=True, text=True)
    planned = mod.dispatch_preflight.fast_fabric_plan(args, prompt, result, tmp_path)
    if planned is None:
        return  # The shell planner owns routes outside the narrow fast path.
    assert shell.returncode == 0, shell.stderr
    expected = json.loads(shell.stdout)
    for value in (planned, expected):
        normalise_plan_identity(value)
    assert planned == expected


def test_fabric_fast_plan_delegates_shell_environment_knobs(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    for name in (
        'CF_DISPATCH_AGY_ADD_DIR', 'CF_DISPATCH_ENABLE_KIRO',
        'CF_DISPATCH_ENABLE_COPILOT', 'CF_DISPATCH_CURSOR_MODEL', 'CF_DISPATCH_KIRO_MODEL',
        'CF_DISPATCH_COPILOT_MODEL', 'CF_DISPATCH_OPENCODE_MODEL', 'CF_DISPATCH_ENDPOINT',
        'CF_DISPATCH_CODEX_NETWORK', 'CF_DISPATCH_AGY_SANDBOX',
    ):
        monkeypatch.setenv(name, '1')
        assert mod.dispatch_preflight.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None
        monkeypatch.delenv(name)


@pytest.mark.parametrize('variant', ['agy_add_dir', 'resume', 'fallback'])
def test_fabric_fast_plan_delegates_valid_shell_only_options(tmp_path, monkeypatch, variant):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    if variant == 'agy_add_dir':
        extra = tmp_path / 'extra'
        extra.mkdir()
        monkeypatch.setenv('CF_DISPATCH_AGY_ADD_DIR', str(extra))
    elif variant == 'resume':
        args.resume_session = 'existing-session'
    else:
        args.fallback = 'true'
    result = run / 'result.md'
    shell = subprocess.run([*mod.build_command(args, prompt, result), '--plan-only'],
                           cwd=tmp_path, env=mod.dispatch_preflight.routing_environment(preserve_instance=True), capture_output=True, text=True)
    assert shell.returncode == 0, shell.stderr
    expected = json.loads(shell.stdout)
    assert expected['adapter'] == 'claude'
    assert mod.dispatch_preflight.fast_fabric_plan(args, prompt, result, tmp_path) is None
    if variant == 'agy_add_dir':
        assert str(extra) in expected['applied']['add_dirs']
    elif variant == 'resume':
        assert 'existing-session' in expected['argv']


def test_fabric_fast_plan_delegates_missing_tmpdir(tmp_path, monkeypatch):
    """cf_dispatch.py rejects a TMPDIR that is not a directory; the fast path must not accept it."""
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    monkeypatch.setenv('TMPDIR', str(tmp_path / 'missing'))
    assert mod.dispatch_preflight.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


def test_fabric_fast_plan_guard_covers_shell_environment_reads():
    import re
    mod = load_dispatch_module()
    shell = (ROOT / 'skills/orchestrate/scripts/cf_dispatch.py').read_text()
    read_names = set(re.findall(r'"(CF_DISPATCH_[A-Z0-9_]+)"', shell))
    assert read_names == mod.dispatch_preflight.FAST_PLAN_SHELL_ENV_READS


@pytest.mark.parametrize('raw', [
    '{"status":"ok","resolved_model":"opus","resolved_model":"sonnet","model_family":"anthropic","endpoint_provider":"anthropic","identity_source":"test"}',
    '{"status":"ok","resolved_model":"","model_family":"anthropic","endpoint_provider":"anthropic","identity_source":"test"}',
    '{"status":"ok","resolved_model":"opus","model_family":"anthropic","endpoint_provider":"anthropic","identity_source":"test","effort":2}',
    '{"status":"ok","resolved_model":"opus","model_family":"anthropic","endpoint_provider":"anthropic","identity_source":"test","reason":"bad\\u0000value"}',
    '[{"status":"ok"}]',
])
def test_fabric_fast_route_validation_matches_shell_rejections(raw):
    mod = load_dispatch_module()
    with pytest.raises(ValueError):
        mod.dispatch_preflight.parse_fast_route_json(raw)


def test_fabric_fast_route_validation_tracks_shell_fields():
    import importlib.util
    mod = load_dispatch_module()
    path = ROOT / 'skills/orchestrate/scripts/cf_dispatch.py'
    spec = importlib.util.spec_from_file_location('cf_dispatch_under_test', path)
    dispatcher = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = dispatcher
    spec.loader.exec_module(dispatcher)
    assert set(dispatcher.ROUTE_REQUIRED_FIELDS) == set(mod.dispatch_preflight.FAST_PLAN_ROUTE_REQUIRED)
    assert set(dispatcher.ROUTE_FIELDS) == set(mod.dispatch_preflight.FAST_PLAN_ROUTE_FIELDS)


@pytest.mark.parametrize('failure', [
    ImportError('module unavailable'), SyntaxError('bad module'),
    subprocess.CalledProcessError(1, ['router']), subprocess.TimeoutExpired(['router'], 1),
    SystemExit(2),
])
def test_fabric_fast_plan_delegates_import_and_router_failures(tmp_path, monkeypatch, failure):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS

    def fail_import(*_args, **_kwargs):
        raise failure

    monkeypatch.setattr(mod.importlib.util, 'spec_from_file_location', fail_import)
    assert mod.dispatch_preflight.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


@pytest.mark.parametrize('adapter, idle', [('agy', None), ('claude', '0.5')])
def test_fabric_fast_plan_delegates_only_active_shell_checks(tmp_path, monkeypatch, adapter, idle):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n',
    )
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    if idle is not None:
        monkeypatch.setenv('CF_DISPATCH_IDLE_SECONDS', idle)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.tool = adapter
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    plan = mod.dispatch_preflight.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path)
    assert (plan is None) is (adapter == 'agy')


def test_fabric_fast_plan_rejects_invalid_git_context(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n',
    )
    monkeypatch.chdir(tmp_path)
    # An existing malformed target also blocks Git discovery into an enclosing
    # checkout when the test scratch directory lives inside one.
    (tmp_path / 'invalid-git').write_text('not a git file\n')
    (tmp_path / '.git').symlink_to(tmp_path / 'invalid-git')
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    assert mod.dispatch_preflight.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


@pytest.mark.parametrize('content', ['', 'hello\x00world'])
def test_fabric_fast_plan_preserves_shell_prompt_rejections(tmp_path, monkeypatch, content):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n',
    )
    monkeypatch.chdir(tmp_path)
    prompt.write_text(content)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    assert mod.dispatch_preflight.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


@pytest.mark.parametrize('previous_status', ['interrupted', 'timed_out', 'cancelled', 'stalled', 'lost'])
def test_interrupted_attempt_resumes_from_nested_cwd_with_absolute_caller_prompt(tmp_path, monkeypatch, previous_status):
    code = '''import sys,json,os
prompt = sys.stdin.read()
if '--resume' in sys.argv:
 print('No conversation found with session ID', file=sys.stderr)
 sys.exit(1)
print(json.dumps({"type":"result","result":os.getcwd()}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    nested = tmp_path / 'src'
    nested.mkdir()
    first = subprocess.run([*command, '--cwd', str(nested), '--no-preface'], cwd=tmp_path, capture_output=True, text=True)
    assert first.returncode == 0, first.stdout + first.stderr
    path = run / 'tasks/dispatch-001/attempt-001/attempt.json'
    row = json.loads(path.read_text())
    # Model the B-owner recovery contract: terminal interrupted canonical row,
    # before the owner could publish its legacy terminal evidence.
    row.update(status=previous_status, state='terminal')
    path.write_text(json.dumps(row))
    if previous_status in {'interrupted', 'lost'}:
        legacy_path = run / row['legacy_attempt_path']
        legacy_path.unlink()
        legacy_path.with_name('attempt.sha256').unlink()
        (run / row['paths']['result']).unlink()
    receipt = json.loads((run / 'RUN_RECEIPT.json').read_text())
    receipt['status'] = 'interrupted'
    (run / 'RUN_RECEIPT.json').write_text(json.dumps(receipt))
    resumed = subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', row['run_id'],
                              '--prompt-file', str(prompt), '--cwd', str(nested)],
                             cwd=nested, capture_output=True, text=True)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    second = json.loads((run / 'tasks/dispatch-001/attempt-002/attempt.json').read_text())
    assert second['status'] == 'ok'
    assert second['cwd'] == str(nested)
    second_legacy = json.loads((run / second['legacy_attempt_path']).read_text())
    assert second_legacy['workspace']['root'] == str(tmp_path.resolve())
    assert second['requested_route']['preface'] is False
    assert second['requested_route']['intent'] == row['requested_route']['intent']
    assert 'resumed_by_relaunch' in second['provenance']['notes']
    assert 'resume: relaunched' in second['warnings']


def test_claude_resume_relaunches_when_saved_session_is_missing(tmp_path, monkeypatch):
    code = '''import json,sys
prompt = sys.stdin.read()
if '--resume' in sys.argv:
 print('No conversation found with session ID', file=sys.stderr)
 sys.exit(1)
print(json.dumps({'type':'system','subtype':'init','session_id':'saved-session','model':'opus'}))
print(json.dumps({'type':'result','result':'DONE'}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    first = subprocess.run(command, cwd=tmp_path, capture_output=True, text=True)
    assert first.returncode == 0, first.stdout + first.stderr
    previous = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())
    assert previous['session_id'] == 'saved-session'
    command = [sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', previous['run_id'],
               '--prompt-file', str(prompt)]
    resumed = subprocess.run(command, cwd=tmp_path, capture_output=True, text=True)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    row = json.loads((run / 'tasks/dispatch-001/attempt-002/attempt.json').read_text())
    assert row['status'] == 'ok'
    assert 'resumed_by_relaunch' in row['provenance']['notes']
    assert 'resume: relaunched' in row['warnings']


def test_named_session_resume_never_relaunches_a_missing_claude_session(tmp_path, monkeypatch):
    code = '''import json,sys
prompt = sys.stdin.read()
if '--resume' in sys.argv:
 print('No conversation found with session ID', file=sys.stderr)
 sys.exit(1)
print(json.dumps({'type':'system','subtype':'init','session_id':'saved-session','model':'opus'}))
print(json.dumps({'type':'result','result':'DONE'}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    first = subprocess.run(command, cwd=tmp_path, capture_output=True, text=True)
    assert first.returncode == 0, first.stdout + first.stderr
    previous = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())
    command = [sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', previous['run_id'],
               '--prompt-file', str(prompt), '--resume-attempt', '1', '--require-session', 'saved-session']
    resumed = subprocess.run(command, cwd=tmp_path, capture_output=True, text=True)
    row = json.loads((run / 'tasks/dispatch-001/attempt-002/attempt.json').read_text())
    assert row['status'] == 'rejected', resumed.stdout + resumed.stderr
    assert row['error'] == 'continuation_unsupported'
    assert 'fresh: true' in row['fix']
    assert 'resumed_by_relaunch' not in row['provenance']['notes']
    assert not (run / 'tasks/dispatch-001/attempt-003').exists()


def test_named_session_resume_continues_the_pinned_attempt_session(tmp_path, monkeypatch):
    code = '''import json,sys
prompt = sys.stdin.read()
session = sys.argv[sys.argv.index('--resume') + 1] if '--resume' in sys.argv else 'saved-session'
if prompt.strip().endswith('fail'):
 print(json.dumps({'type':'system','subtype':'init','session_id':'other-session','model':'opus'}))
 sys.exit(3)
print(json.dumps({'type':'system','subtype':'init','session_id':session,'model':'opus'}))
print(json.dumps({'type':'result','result':'RESUMED ' + session}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    assert subprocess.run(command, cwd=tmp_path, capture_output=True, text=True).returncode == 0
    run_id = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())['run_id']
    assert not (run / 'dispatch-provider.json').exists()  # retired once the owner reaped its provider
    failing = tmp_path / 'failing.md'
    failing.write_text('fail')
    subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', run_id,
                    '--prompt-file', str(failing)], cwd=tmp_path, capture_output=True, text=True)
    failed = json.loads((run / 'tasks/dispatch-001/attempt-002/attempt.json').read_text())
    assert failed['status'] != 'ok'
    resume = [sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', run_id, '--prompt-file', str(prompt),
              '--resume-attempt', '1']
    mismatch = subprocess.run([*resume, '--require-session', 'not-this-one'], cwd=tmp_path, capture_output=True, text=True)
    assert json.loads(mismatch.stdout)['error'] == 'continuation_unsupported'
    assert not (run / 'tasks/dispatch-001/attempt-003').exists()
    resumed = subprocess.run([*resume, '--require-session', 'saved-session'], cwd=tmp_path, capture_output=True, text=True)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    row = json.loads((run / 'tasks/dispatch-001/attempt-003/attempt.json').read_text())
    assert row['status'] == 'ok'
    assert row['session_id'] == 'saved-session'


@pytest.mark.parametrize(('status', 'signature'), [
    ('timed_out', 'wall_clock'),
    ('interrupted', 'supervisor_signal'),
    ('failed', 'provider_signal'),
    ('failed', 'provider_incomplete'),
    ('failed', 'host_disk_full'),
    ('failed', 'provider_lost'),
])
def test_incomplete_writer_without_claude_session_returns_typed_fix(tmp_path, status, signature):
    mod = load_dispatch_module()
    run = Path(subprocess.check_output([str(INIT), '--kind', 'dispatch'], cwd=tmp_path, text=True).strip())
    row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
    row.update(run_id=run.name, status=status, mode='worktree_write', cwd=str(tmp_path),
               worktree=str(tmp_path), session_id='generated-but-unobserved')
    row['evidence']['signature'] = signature
    row['provenance']['requested']['adapter'] = 'claude'
    path = run / 'tasks/task-1/attempt-001/attempt.json'
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps(row))
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('continue')
    result = subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', row['run_id'],
                             '--prompt-file', str(prompt)], cwd=tmp_path, capture_output=True, text=True)
    assert result.returncode != 0
    response = json.loads(result.stdout)
    assert response['status'] == 'rejected'
    assert 'review worktree' in response['message'].lower()


@pytest.mark.parametrize('observed_session', [False, True])
def test_claude_signal_dead_writer_resume_never_relaunches_over_partial_edits(tmp_path, monkeypatch, observed_session):
    code = '''import json,os,signal,sys
from pathlib import Path
sys.stdin.read()
with Path('launches.txt').open('a') as stream:
 stream.write('launch\\n')
if '--resume' in sys.argv:
 print('No conversation found with session ID', file=sys.stderr)
 sys.exit(1)
if Path('partial.txt').exists():
 print(json.dumps({'type':'result','result':'FRESH OVER PARTIAL EDITS'}))
 sys.exit(0)
Path('partial.txt').write_text('unfinished edit')
'''
    if observed_session:
        code += "print(json.dumps({'type':'system','subtype':'init','session_id':'writer-session','model':'opus'}), flush=True)\n"
    code += 'os.kill(os.getpid(), signal.SIGTERM)\n'
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path)
    first = subprocess.run([*command, '--access-mode', 'worktree_write', '--worktree', str(worktree)],
                           cwd=tmp_path, capture_output=True, text=True, timeout=30)
    assert first.returncode != 0, first.stdout + first.stderr
    previous = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())
    assert previous['status'] == 'failed'
    assert previous['evidence']['signature'] == 'provider_signal'
    assert previous['evidence']['signal'] == signal.SIGTERM

    resumed = subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run),
                              '--resume', previous['run_id'], '--prompt-file', str(prompt)],
                             cwd=tmp_path, capture_output=True, text=True, timeout=30)
    assert resumed.returncode != 0, resumed.stdout + resumed.stderr
    if observed_session:
        row = json.loads((run / 'tasks/dispatch-001/attempt-002/attempt.json').read_text())
        assert row['status'] == 'rejected'
        assert row['evidence']['signature'] == 'resume_session_missing'
        assert 'resumed_by_relaunch' not in row['provenance']['notes']
    else:
        response = json.loads(resumed.stdout)
        assert response['status'] == 'rejected'
        assert 'review worktree' in response['message'].lower()
        assert not (run / 'tasks/dispatch-001/attempt-002').exists()
    assert (worktree / 'partial.txt').read_text() == 'unfinished edit'
    assert (worktree / 'launches.txt').read_text().splitlines() == ['launch'] * (2 if observed_session else 1)


def test_planner_stopped_by_signal_is_interrupted_not_rejected():
    mod = load_dispatch_module()
    stopped = subprocess.CompletedProcess([], -15, stdout="", stderr="Terminated: 15")
    assert mod.dispatch_preflight.planner_result(stopped)["status"] == "interrupted"
    broken = subprocess.CompletedProcess([], 0, stdout="not json", stderr="")
    assert mod.dispatch_preflight.planner_result(broken) == {"status": "rejected", "fix": "route planner returned invalid JSON"}


def test_planner_capability_statuses_become_typed_failures_with_fixes():
    mod = load_dispatch_module()
    row = mod.dispatch_preflight.router_failure("capability_model_unavailable", "codex")
    assert row["status"] == "model_unavailable"
    assert row["fix"].startswith("choose a registered model:") and "gpt-6-luna" in row["fix"]
    assert row["evidence"]["signature"] == "capability_model_unavailable"
    assert mod.dispatch_preflight.router_failure("capability_discovery_failed", "codex")["status"] == "failed"


@pytest.mark.parametrize("status,expected", [
    ("no_candidate_available", "model_unavailable"),
    ("alias_unavailable", "model_unavailable"),
    ("capability_snapshot_stale", "failed"),
    ("probe_cache_busy", "failed"),
    ("effort_unsupported", "rejected"),
    ("same_family_forbidden", "rejected"),
    ("some_future_router_status", "rejected"),
])
def test_every_router_status_reaches_the_caller_typed_with_a_fix(status, expected):
    mod = load_dispatch_module()
    row = mod.dispatch_preflight.router_failure(status, "codex")
    assert row["status"] == expected
    assert row["fix"] and row["evidence"]["signature"] == status


def test_planner_crash_signal_is_not_an_interruption():
    mod = load_dispatch_module()
    crashed = mod.dispatch_preflight.planner_result(subprocess.CompletedProcess([], -11, stdout="", stderr=""))
    assert crashed["status"] == "failed" and "signal 11" in crashed["fix"]


def test_provider_exec_failures_are_not_router_refusals():
    mod = load_dispatch_module()
    assert mod.dispatch_preflight.route_refusal({"status": "output_write_error", "provenance": {"line": "Route: x"}}, "codex") is None
    assert mod.dispatch_preflight.route_refusal({"status": "ok"}, "codex") is None
    assert mod.dispatch_preflight.route_refusal({"schema": "fabric.exec-plan.v1", "status": "odd"}, "codex") is None
    assert mod.dispatch_preflight.route_refusal({"status": "unknown_alias"}, "codex")["status"] == "rejected"


def _run_fake_codex(tmp_path: Path, name: str, body: str, extra_env: dict[str, str] | None = None):
    disable_memory_admission_floor(tmp_path)
    run_dir = make_run(tmp_path, name)
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "codex",
        """#!/usr/bin/env bash
        if [ "$1" = "mcp" ]; then printf '[]'; exit 0; fi
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        cat >/dev/null
        """ + body,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    env.update(extra_env or {})
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "task-1", "--adapter", "codex",
         "--prompt-file", str(prompt), "--orchestrator-family", "openai", "--alias", "senior",
         "--role", "worker"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return run_dir / "dispatch/tasks/task-1/attempt-001", result


def _invalid_hosts_env(tmp_path: Path, config: dict) -> dict[str, str]:
    state = tmp_path / ".agents/.agent-fabric"
    state.mkdir(parents=True)
    (state / "hosts.json").write_text(json.dumps(config))
    return {"HOME": str(tmp_path), "AGENT_FABRIC_INSTANCE_ROOT": str(tmp_path / ".agents"),
            "AGENT_FABRIC_STATE_DIRECTORY": str(tmp_path / "state"), "PROVENANT_HOST_LOCAL_ONLY": "0"}


@pytest.mark.parametrize("config", [
    {"schema_version": 1, "local_host": "here", "peers": {"there": {"ssh_destination": "there", "response_deadline": 0}}},
    {"schema_version": 1, "local_host": "here", "projects": {"/absolute/project": {}}},
])
def test_cli_dispatch_runs_locally_and_reports_an_invalid_hosts_config(tmp_path: Path, config) -> None:
    env = _invalid_hosts_env(tmp_path, config)
    attempt, result = _run_fake_codex(tmp_path, "badcfg", "printf 'OK\\n'\n", env)
    assert result.returncode == 0, result.stderr + result.stdout
    assert "local fallback" in result.stderr and "invalid_config" in result.stderr
    row = json.loads((attempt.parents[3] / "tasks/task-1/attempt-001/attempt.json").read_text())
    assert row["placement_fallback"]["reason"] == "invalid_config"
    assert row["placement_fallback"]["detail"]


def test_cli_dispatch_to_an_explicit_peer_still_rejects_with_invalid_hosts_config(tmp_path: Path) -> None:
    env = _invalid_hosts_env(tmp_path, {"schema_version": 1, "local_host": "here", "peers": {
        "there": {"ssh_destination": "there", "response_deadline": 0}}})
    run_dir = make_run(tmp_path, "explicit")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hi\n")
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "task-1", "--adapter", "codex", "--host", "there",
         "--prompt-file", str(prompt), "--orchestrator-family", "openai", "--alias", "senior", "--role", "worker"],
        cwd=tmp_path, env={**os.environ, **env}, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert result.returncode == 1
    assert json.loads(result.stdout)["error"] == "unknown_host"


def test_terminal_ok_attempt_prunes_private_tmp_but_keeps_records(tmp_path: Path) -> None:
    outside = tmp_path / "outside.txt"
    outside.write_text("keep\n")
    attempt, result = _run_fake_codex(
        tmp_path, "prune",
        f"""head -c 65536 /dev/zero > "$TMPDIR/scratch.bin"
        ln -s {outside} "$TMPDIR/link"
        ln -s {tmp_path} "$TMPDIR/dirlink"
        printf 'OK\\n'
        """)
    assert result.returncode == 0, result.stderr + result.stdout
    assert not (attempt / "tmp").exists()
    assert (attempt / "attempt.json").is_file() and (attempt / "result.md").is_file()
    assert outside.read_text() == "keep\n" and (tmp_path / "prompt.md").is_file()


def test_keep_attempt_tmp_env_retains_private_tmp(tmp_path: Path) -> None:
    attempt, result = _run_fake_codex(
        tmp_path, "keep", 'echo x > "$TMPDIR/note"\nprintf \'OK\\n\'\n',
        {"PROVENANT_KEEP_ATTEMPT_TMP": "1"})
    assert result.returncode == 0, result.stderr + result.stdout
    assert (attempt / "tmp/note").is_file()

@pytest.mark.parametrize(('signin', 'sandbox', 'expected'), [
    (0, 'workspace-write', 'validated'),
    (1, 'workspace-write', 'remote_signin_unusable'),
    (0, 'full', 'remote_confinement_degraded'),
])
def test_remote_writer_preflight_uses_host_signin_and_effective_writer_confinement(tmp_path, monkeypatch, signin, sandbox, expected):
    """Exercise the real preflight with only provider/routing process fixtures."""
    module = load_dispatch_module()
    repo = tmp_path / 'project'
    repo.mkdir()
    subprocess.run(['git', '-C', str(repo), 'init', '-q'], check=True)
    subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Fixture',
        '-c', 'user.email=fixture@example.invalid', 'commit', '--allow-empty', '-qm', 'base'], check=True)
    worktree = repo / '.worktrees/feat-writer'
    subprocess.run(['git', '-C', str(repo), 'worktree', 'add', '-qb', 'feat/writer', str(worktree)], check=True)
    binary = tmp_path / 'bin/codex'
    binary.parent.mkdir()
    write_executable(binary, '#!/bin/sh\necho ' + ('Not logged in' if signin else 'Logged in') + '\nexit ' + str(signin) + '\n')
    monkeypatch.setenv('PATH', str(binary.parent) + os.pathsep + os.environ['PATH'])
    monkeypatch.setenv('PROVENANT_REMOTE_LANE', '1')
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    original_run = subprocess.run
    def process(command, **kwargs):
        if any(str(part).endswith(('model_route.py', 'model-route')) for part in command):
            return subprocess.CompletedProcess(command, 0, json.dumps({
                'status': 'ok', 'adapter': 'codex', 'resolved_model': 'gpt-6.1-sol',
                'provider_family': 'openai', 'execution_intent': 'ordinary', 'notes': []}), '')
        return original_run(command, **kwargs)
    monkeypatch.setattr(module.subprocess, 'run', process)
    value = module.dispatch_preflight.preflight_tasks([{'id': 'writer', 'adapter': 'codex', 'model': 'gpt-6.1-sol',
        'access_mode': 'worktree_write', 'worktree': str(worktree), 'sandbox': sandbox, 'prompt': 'fixture only'}], repo)
    assert (value.get('error') or value['status']) == expected, value


@pytest.mark.parametrize('invalid_digest', [False, True])
def test_dispatch_retains_quarantined_result_without_clean_success(tmp_path, monkeypatch, invalid_digest):
    run_dir = make_run(tmp_path, 'quarantined')
    disable_memory_admission_floor(tmp_path)
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('hello')
    adapter = tmp_path / 'adapter'
    write_success_adapter(adapter)
    text = adapter.read_text().replace('"status":"ok"', '"status":"quarantined","landable":false,"quarantine":{"reason":"protected_instructions_changed","patches":["protected.patch"],"restored":false}')
    if invalid_digest:
        text = text.replace('digest="sha256:', 'digest="invalid-sha256:')
    adapter.write_text(text + '\nexit 1\n')
    module = load_dispatch_module()
    monkeypatch.setattr(module, 'CF_DISPATCH', adapter)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        '--run-dir', str(run_dir), '--task-id', 'review', '--adapter', 'codex',
        '--prompt-file', str(prompt), '--alias', 'junior', '--role', 'worker'])
    assert module.dispatch(args) == 1
    record = json.loads((run_dir / 'dispatch/tasks/review/attempt-001/attempt.json').read_text())
    assert record['status'] == ('failed' if invalid_digest else 'quarantined')
    if invalid_digest:
        assert record['outcome'] == 'adapter_receipt_invalid'
        canonical = json.loads((run_dir / 'tasks/review/attempt-001/attempt.json').read_text())
        assert canonical['status'] == 'failed'
    if not invalid_digest:
        assert record['landable'] is False
        assert record['quarantine']['patches'] == ['protected.patch']
        assert (run_dir / record['result']['path']).read_text() == 'OK\n'
        canonical = json.loads((run_dir / 'tasks/review/attempt-001/attempt.json').read_text())
        assert canonical['status'] == 'quarantined'
        assert canonical['landable'] is False
        assert 'review quarantine' in canonical['digest']
        import batch_run
        claimed = {**record, 'attempt_digest': module.run_records.digest(run_dir / record['attempt_path'])}
        child = batch_run._validate_child_record(
            {'id': 'review', 'adapter': 'codex', 'alias': 'junior', 'role': 'worker'}, claimed, run_dir, 1)
        assert child['status'] == 'quarantined'
        assert child['landable'] is False
        assert child['quarantine'] == record['quarantine']
        import run_controls
        retained, *_ = run_controls._attempt(run_dir, 'review', 'attempt-001')
        assert retained['status'] == 'quarantined'
        monkeypatch.setenv('PROVENANT_RUN_TOKEN', 'fixture-token')
        monkeypatch.setenv('PROVENANT_RUN_DIR', str(run_dir))
        module.close_mcp_run(run_dir)
        assert json.loads((run_dir / 'RUN_RECEIPT.json').read_text())['status'] == 'quarantined'


def test_preflight_types_an_opencode_below_v2_as_unsupported_cli_version(monkeypatch, tmp_path):
    module = load_dispatch_module()
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(tmp_path / ".agents"))
    monkeypatch.setenv("PROVENANT_REMOTE_LANE", "1")
    monkeypatch.setattr(module.dispatch_preflight, "_fabric_hosts_owner", lambda: SimpleNamespace(
        adapter_doctor=lambda *_: {"signin": {"status": "ok"}}))
    monkeypatch.setattr(module.provider_exec.provider_confinement, "_opencode_version_error",
                        lambda *_: "OpenCode 1.14.2 is unsupported; fix: upgrade OpenCode to v2")
    task = {"id": "old", "adapter": "opencode", "model": "opencode/deepseek-v4.1-flash-free", "prompt": "hello"}

    result = module.dispatch_preflight.preflight_tasks([task], tmp_path)

    assert result["status"] == "rejected"
    assert result["error"] == "unsupported_cli_version"
    assert "upgrade OpenCode to v2" in result["fix"]


def test_dispatch_usage_error_is_one_line_and_exits_2():
    dispatch_script = ROOT / "skills/orchestrate/scripts/dispatch_run.py"
    result = subprocess.run([sys.executable, str(dispatch_script), "--run-dir", "/tmp", "--prompt-stdin", "--bad-arg"], capture_output=True, text=True)
    assert result.returncode == 2
    assert result.stdout == ""
    assert result.stderr == "error: unrecognized arguments: --bad-arg; fix: provenant help dispatch\n"

    missing = subprocess.run([sys.executable, str(dispatch_script)], capture_output=True, text=True)
    assert missing.returncode == 2
    assert missing.stdout == ""
    assert missing.stderr.startswith("error: the following arguments are required: ")
    assert "; fix: pass " in missing.stderr
    assert len(missing.stderr.splitlines()) == 1


@pytest.mark.parametrize("selector", ["explicit", "task_class"])
@pytest.mark.parametrize("persistent", [False, True])
def test_rate_limit_retries_same_model_once_with_fallback_off(tmp_path, monkeypatch, persistent, selector):
    launches = tmp_path / "launches"
    code = f'''import json,sys
from pathlib import Path
if sys.argv[1:3] == ["auth", "status"]:
    print(json.dumps({{"loggedIn":True, "authMethod":"claude.ai", "subscriptionType":"pro"}}))
    sys.exit(0)
if "Reply exactly OK." in sys.argv:
    print(json.dumps({{"type":"result", "subtype":"success", "is_error":False, "result":"OK", "modelUsage":{{"claude-haiku-5-5":{{}}}}}}))
    sys.exit(0)
sys.stdin.read()
path = Path({str(launches)!r})
count = int(path.read_text()) + 1 if path.exists() else 1
path.write_text(str(count))
if count == 1 or {persistent!r}:
    print(json.dumps({{"type":"result", "is_error":True, "result":"rate limit", "retry_after":0.05}}))
    sys.exit(1)
print(json.dumps({{"type":"result", "result":"DONE"}}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    if selector == "task_class":
        model_index = command.index("--model")
        del command[model_index:model_index + 2]
        command += ["--task-class", "mechanical", "--role", "worker"]
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.chdir(tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 10
    assert mod.dispatch(args) == (1 if persistent else 0)
    attempts = [json.loads(path.read_text()) for path in sorted(run.glob("tasks/*/attempt-*/attempt.json"))]
    assert [row["status"] for row in attempts] == ["rate_limited", "rate_limited" if persistent else "ok"]
    assert launches.read_text() == "2"
    expected_model = "claude-haiku-5-5" if selector == "task_class" else "claude-opus-5-5"
    assert {row["provenance"]["resolved_model"] for row in attempts} == {expected_model}
    if selector == "task_class":
        assert all(row["task_class"] == "mechanical" for row in attempts)
        assert all(row["provenance"]["requested"]["task_class"] == "mechanical" for row in attempts)
    legacy = json.loads(next(run.glob("dispatch/tasks/*/attempt-002/attempt.json")).read_text())
    assert legacy["retry_of"] == "attempt-001"
    assert attempts[1]["provenance"]["fallback_from"]["status"] == "rate_limited"
    health = mod.exec_routing._model_route_module()._fabric_records().read_route_health()
    assert all(row["recent_failures"] == 0 for row in health.values())


def test_rate_limit_does_not_retry_when_cooldown_exceeds_timeout(tmp_path, monkeypatch):
    code = 'import json,sys\nsys.stdin.read()\nprint(json.dumps({"type":"result", "is_error":True, "result":"rate limit", "retry_after":60}))\nsys.exit(1)\n'
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.chdir(tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 5
    assert mod.dispatch(args) == 1
    attempts = [json.loads(path.read_text()) for path in run.glob("tasks/*/attempt-*/attempt.json")]
    assert [row["status"] for row in attempts] == ["rate_limited"]


@pytest.mark.parametrize("edits", [None, "partial.txt"])
def test_rate_limited_writer_retries_only_if_unchanged(tmp_path, monkeypatch, edits):
    launches = tmp_path / "launches"
    code = f'''import json,sys
from pathlib import Path
sys.stdin.read()
path = Path({str(launches)!r})
count = int(path.read_text()) + 1 if path.exists() else 1
path.write_text(str(count))
if count == 1:
    if {edits!r}:
        Path({edits!r}).write_text("half done")
    print(json.dumps({{"type":"result", "is_error":True, "result":"rate limit", "retry_after":0.05}}))
    sys.exit(1)
print(json.dumps({{"type":"result", "result":"DONE"}}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path)
    time.sleep(1.2)
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.chdir(tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:] + ["--access-mode", "worktree_write", "--worktree", str(worktree)])
    args.timeout_seconds = 10
    assert mod.dispatch(args) == (1 if edits else 0)
    attempts = [json.loads(path.read_text()) for path in sorted(run.glob("tasks/*/attempt-*/attempt.json"))]
    assert [row["status"] for row in attempts] == (["rate_limited"] if edits else ["rate_limited", "ok"])
    if edits:
        assert attempts[0]["retryable"] is False
        assert (worktree / edits).read_text() == "half done"


@pytest.mark.parametrize("action", ["cancel", "edit"])
def test_rate_limit_wait_observes_cancellation_and_intervening_writer_edits(tmp_path, monkeypatch, action):
    code = 'import json,sys\nsys.stdin.read()\nprint(json.dumps({"type":"result", "is_error":True, "result":"rate limit", "retry_after":2}))\nsys.exit(1)\n'
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path) if action == "edit" else None
    if worktree:
        time.sleep(1.2)
        command += ["--access-mode", "worktree_write", "--worktree", str(worktree)]
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.chdir(tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    mod = load_dispatch_module()
    sleep = time.sleep
    acted = []
    stores_before = {}
    test_thread = threading.get_ident()
    def during_wait(seconds):
        # time.sleep is patched process-wide; under pytest-xdist the execnet
        # threads call it too, so only the dispatching thread may act.
        if threading.get_ident() != test_thread:
            return sleep(seconds)
        queued = list(run.glob("tasks/*/attempt-002/attempt.json"))
        if queued and not acted and json.loads(queued[0].read_text()).get("queue_reason") == "rate_limit":
            # Record first: the lease retry below can itself call the patched
            # sleep, which must not re-enter this branch.
            acted.append(action)
            if action == "cancel":
                (run / "dispatch/tasks" / queued[0].relative_to(run / "tasks").parent / "cancel.request").touch()
            else:
                with pytest.raises(mod.writer_lease.WorktreeLeaseError):
                    mod.writer_lease.acquire_worktree_lease(worktree)
                for store in (tmp_path / "cooldowns.json", tmp_path / "state/route-health.json"):
                    stores_before[store] = store.read_bytes()
                (worktree / "intervening.txt").write_text("another edit")
        if not acted:
            sleep(seconds)
    monkeypatch.setattr(mod.time, "sleep", during_wait)
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 10
    assert mod.dispatch(args) == 1
    attempts = [json.loads(path.read_text()) for path in sorted(run.glob("tasks/*/attempt-*/attempt.json"))]
    assert acted == [action]
    assert [row["status"] for row in attempts] == ["rate_limited", "cancelled" if action == "cancel" else "writer_changed"]
    assert attempts[1]["pgid"] is None
    if action == "edit":
        assert attempts[1]["retryable"] is False
        assert (worktree / "intervening.txt").read_text() == "another edit"
        assert all(path.read_bytes() == content for path, content in stores_before.items())


def test_rate_limit_retry_rechecks_writer_after_memory_admission(tmp_path, monkeypatch):
    launches = tmp_path / "launches"
    code = f'''import json,sys
from pathlib import Path
sys.stdin.read()
path = Path({str(launches)!r})
count = int(path.read_text()) + 1 if path.exists() else 1
path.write_text(str(count))
if count == 1:
    print(json.dumps({{"type":"result", "is_error":True, "result":"rate limit", "retry_after":0.05}}))
    sys.exit(1)
print(json.dumps({{"type":"result", "result":"DONE"}}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path)
    (tmp_path / ".agents/fabric-policy.json").write_text(json.dumps({"memory_floor_percent": {"worktree_write":10}}))
    time.sleep(1.2)
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    monkeypatch.chdir(tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    mod = load_dispatch_module()
    edited = []
    def probe():
        if list(run.glob("tasks/*/attempt-002/attempt.json")) and not edited:
            (worktree / "during-admission.txt").write_text("another edit")
            edited.append(True)
        return 100, 100
    monkeypatch.setattr(mod.memory_admission, "available_memory_mb", probe)
    args = mod.parser().parse_args(command[2:] + ["--access-mode", "worktree_write", "--worktree", str(worktree)])
    args.timeout_seconds = 10
    assert mod.dispatch(args) == 1
    assert edited == [True]
    assert launches.read_text() == "1"
    attempts = [json.loads(path.read_text()) for path in sorted(run.glob("tasks/*/attempt-*/attempt.json"))]
    assert [row["status"] for row in attempts] == ["rate_limited", "writer_changed"]
    assert attempts[1]["retryable"] is False
    assert attempts[1]["pgid"] is None


def configure_fast_pacing(tmp_path, monkeypatch, **overrides):
    config = tmp_path / ".agents/config/model-routing.json"
    config.parent.mkdir(parents=True, exist_ok=True)
    policy = {"base_delay_seconds": 0.05, "cap_seconds": 0.1,
              "jitter_fraction": 0, "minimum_budget_seconds": 0.1, **overrides}
    config.write_text(json.dumps({"rate_limit_pacing": policy}))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(tmp_path / ".agents"))


@pytest.mark.parametrize("retry_after", [None, 0.2])
def test_missing_cooldown_still_waits_before_retry(tmp_path, monkeypatch, retry_after):
    launches = tmp_path / "launches"
    code = f'''import json,sys,time
from pathlib import Path
sys.stdin.read()
path = Path({str(launches)!r})
with path.open("a") as stream:
    stream.write(str(time.monotonic()) + "\\n")
print(json.dumps({{"type":"result", "is_error":True, "result":"rate limit", "retry_after":{retry_after!r}}}))
sys.exit(1)
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch, base_delay_seconds=0.1)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    records = sys.modules[mod.write_cooldown.__module__]
    original = records.store_open
    def no_lock(path, flags, create_parents=False):
        if Path(path).name == "cooldowns.lock":
            raise PermissionError("state unavailable")
        return original(path, flags, create_parents)
    monkeypatch.setattr(records, "store_open", no_lock)
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 5
    assert mod.dispatch(args) == 1
    times = list(map(float, launches.read_text().splitlines()))
    assert len(times) == 2
    assert times[1] - times[0] >= (retry_after or 0.1)


def test_rate_limit_reserves_run_budget_after_the_delay(tmp_path, monkeypatch):
    code = 'import json,sys; sys.stdin.read(); print(json.dumps({"type":"result", "is_error":True, "result":"rate limit", "retry_after":0.2})); sys.exit(1)'
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch, minimum_budget_seconds=10)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 10
    assert mod.dispatch(args) == 1
    assert len(list(run.glob("tasks/*/attempt-*/attempt.json"))) == 1


@pytest.mark.parametrize("fallback", ["false", '[{"adapter":"claude","model":"claude-opus-5-5"}]', '[{"adapter":"claude","model":"opus"}]'])
def test_usage_limit_never_retries_the_same_model(tmp_path, monkeypatch, fallback):
    code = 'import json,sys; sys.stdin.read(); print(json.dumps({"type":"result", "is_error":True, "result":"429 RESOURCE_EXHAUSTED: Quota exceeded per day"})); sys.exit(1)'
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    command[command.index("--fallback") + 1] = fallback
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 10
    records = sys.modules[mod.write_cooldown.__module__]
    original = records.store_open
    def no_cooldown_lock(path, flags, create_parents=False):
        if Path(path).name == "cooldowns.lock":
            raise PermissionError("state unavailable")
        return original(path, flags, create_parents)
    monkeypatch.setattr(records, "store_open", no_cooldown_lock)
    assert mod.dispatch(args) == 1
    rows = [json.loads(path.read_text()) for path in run.glob("tasks/*/attempt-*/attempt.json")]
    assert [row["status"] for row in rows] == ["usage_limited"]
    assert "Quota exceeded per day" in rows[0]["reason"]


def test_pacing_jitter_is_added_to_the_wait_without_shortening_it(monkeypatch):
    mod = load_dispatch_module()
    monkeypatch.setattr(mod.random, "uniform", lambda low, high: high)
    policy = {"base_delay_seconds": 30, "jitter_fraction": 0.2}
    assert mod.rate_limit_delay({}, None, policy) == 36
    assert mod.rate_limit_delay({"retry_after": 60}, None, policy) == 72


def test_agy_retry_rebuilds_argv_timeout_after_the_wait(tmp_path, monkeypatch):
    launches = tmp_path / "argv-timeouts"
    code = f'''import json,sys
from pathlib import Path
path = Path({str(launches)!r})
count = len(path.read_text().splitlines()) if path.exists() else 0
with path.open("a") as stream:
    stream.write(sys.argv[sys.argv.index("--print-timeout") + 1] + "\\n")
if count == 0:
    print(json.dumps({{"type":"error", "message":"429 rate limit", "retry_after":1.3}}))
    sys.exit(1)
print(json.dumps({{"status":"SUCCESS", "response":"DONE"}}))
'''
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    write_executable(tmp_path / "provider-bin/agy", "#!/usr/bin/env python3\n" + code)
    command[command.index("--adapter") + 1] = "agy"
    command[command.index("--model") + 1] = "gemini-3.8-flash"
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 10
    assert mod.dispatch(args) == 0
    first, second = [int(value.removesuffix("s")) for value in launches.read_text().splitlines()]
    assert second < first


def _sequence_fixture(monkeypatch, plan, rows, alternatives):
    mod = load_dispatch_module()
    args = dispatch_args(_profile="sequence")
    launches = []

    def fake_dispatch(args, custody=None):
        launches.append((getattr(args, "tool", None), getattr(args, "model", None), getattr(args, "account", None)))
        args._last_row = rows[len(launches) - 1] if len(launches) <= len(rows) else {**rows[-1], "retryable": False}
        args._last_plan = plan
        return 0

    row = {"retryable": True, "attempt": 1, "task_class": "",
           "provenance": {"line": "Route: claude/claude-opus-5-5 (x)"}}
    rows[:] = [{**row, **item} for item in rows]
    monkeypatch.setattr(mod, "_dispatch", fake_dispatch)
    monkeypatch.setattr(mod.exec_routing, "candidates", lambda *a, **k: alternatives)
    monkeypatch.setattr(mod.exec_routing, "cooling", lambda *a, **k: None)
    monkeypatch.setattr(mod, "pacing_policy", lambda: {"base_delay_seconds": 30, "cap_seconds": 900,
                                                       "jitter_fraction": 0, "minimum_budget_seconds": 30})
    return mod, args, launches


def test_fallback_skips_the_route_of_an_effort_suffixed_plan_model(monkeypatch):
    plan = {"adapter": "claude", "model": "claude-opus-5-5@high"}
    rows = [{"status": "usage_limited"}]
    alternatives = [{"adapter": "claude", "model": "claude-opus-5-5"}]
    mod, args, launches = _sequence_fixture(monkeypatch, plan, rows, alternatives)
    mod.execute_attempt_sequence(args)
    assert len(launches) == 1


def test_paced_retry_tolerates_a_row_with_null_evidence(monkeypatch):
    plan = {"adapter": "claude", "model": "claude-opus-5-5"}
    rows = [{"status": "rate_limited", "evidence": None, "retry_after": 1}]
    mod, args, launches = _sequence_fixture(monkeypatch, plan, rows, [])
    mod.execute_attempt_sequence(args)
    assert len(launches) == 2
    assert launches[1][1] == "claude-opus-5-5"


def test_paced_retry_retains_the_selected_claude_account(monkeypatch):
    plan = {"adapter": "claude", "model": "claude-opus-5-5", "account": "pro"}
    rows = [{"status": "rate_limited", "retry_after": 1}, {"status": "ok", "retryable": False}]
    mod, args, launches = _sequence_fixture(monkeypatch, plan, rows, [])
    mod.execute_attempt_sequence(args)
    assert launches == [(None, None, None), ("claude", "claude-opus-5-5", "pro")]


@pytest.mark.parametrize('restored_account', [False, True])
def test_paced_retry_does_not_use_a_released_account_cooldown(tmp_path, monkeypatch, restored_account):
    plan = {"adapter": "claude", "model": "claude-opus-5-5", "account": "pro"}
    rows = [{"status": "rate_limited", "retry_after": 1}, {"status": "ok", "retryable": False}]
    real_cooling = load_dispatch_module().exec_routing.cooling
    mod, args, launches = _sequence_fixture(monkeypatch, plan, rows, [])
    monkeypatch.setattr(mod.exec_routing, "cooling", real_cooling)
    monkeypatch.setenv('FABRIC_COOLDOWNS_PATH', str(tmp_path / 'cooldowns.json'))
    _claude_quota_accounts(tmp_path, monkeypatch, pro=100, max=10)
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'pro'))
    if restored_account:
        args.account, args._explicit_account = 'pro', None
    mod.execute_attempt_sequence(args)

    assert launches == [(None, None, 'pro' if restored_account else None), ('claude', plan['model'], 'max')]


def _claude_quota_accounts(tmp_path, monkeypatch, **usage):
    from datetime import UTC, datetime, timedelta
    import provider_quota as quota

    instance = tmp_path / 'quota-instance'
    config = instance / 'config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {
        label: {'identity_dir': str(tmp_path / label)} for label in usage}}}))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.delenv('CLAUDE_CONFIG_DIR', raising=False)
    for label, used in usage.items():
        quota.save_snapshot({'schema': quota.SCHEMA, 'host': quota.telemetry_host(),
            'observed_at': datetime.now(UTC).isoformat(),
            'provider': 'claude', 'account': label, 'status': 'ok', 'windows': [
                {'name': 'five_hour', 'used_percent': used, 'window_minutes': 300,
                 'reset_at': (datetime.now(UTC) + timedelta(hours=2)).isoformat()},
                {'name': 'seven_day', 'used_percent': 10, 'window_minutes': 10080,
                 'reset_at': (datetime.now(UTC) + timedelta(days=3)).isoformat()},
            ]}, tmp_path / label)


def test_paced_retry_uses_the_longer_account_wide_cooldown():
    mod = load_dispatch_module()
    short = {"cooling_until": timestamp(timedelta(seconds=1)), "status": "rate_limited"}
    wide = {"cooling_until": timestamp(timedelta(hours=2)), "status": "quota_exhausted"}
    records = {"claude/claude-opus-5-5": short, "claude/*": wide}
    assert mod.exec_routing.cooling("claude", "claude-opus-5-5", records, account="pro") == wide


@pytest.mark.parametrize('state', ['credit_only', 'exhausted'])
@pytest.mark.parametrize('selector', ['model', 'alias'])
def test_explicit_account_out_of_quota_launches_with_warning(tmp_path, monkeypatch, capsys, state, selector):
    import provider_quota as quota

    launched = tmp_path / 'launched'
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch,
        f'from pathlib import Path\nPath({str(launched)!r}).touch()\n'
        'import json\nprint(json.dumps({"type":"result","result":"DONE"}))\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    _claude_quota_accounts(tmp_path, monkeypatch, pro=100)
    if state == 'credit_only':
        value = quota.load_snapshot('claude', 'pro', tmp_path / 'pro')
        value['usage_credits'] = [{'kind': 'credits', 'balance': 100, 'unit': 'credits'}]
        quota.save_snapshot(value, tmp_path / 'pro')
    if selector == 'alias':
        model_index = command.index('--model')
        del command[model_index:model_index + 2]
        command += ['--alias', 'senior']
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:] + ['--account', 'pro'])
    args.timeout_seconds = 60
    if selector == 'model':
        # A pinned retry also launches when planning discovers exhausted quota.
        args.fallback_from = {'attempt': 1, 'status': 'rate_limited'}
        args._rate_limit_retry = {'until': mod.now(), 'deadline': time.monotonic() + 60}
    assert mod.dispatch(args) == 0
    assert launched.exists()
    warning = f'account pro is out of quota ({state}): launching because it was pinned explicitly'
    assert warning in capsys.readouterr().err
    attempt = json.loads(next(run.glob('tasks/*/attempt-*/attempt.json')).read_text())
    assert warning in attempt['warnings']


def test_paced_retry_skips_wait_for_a_known_cooldown_block(tmp_path, monkeypatch):
    from datetime import UTC, datetime, timedelta

    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch,
        'raise AssertionError("blocked provider must not launch")\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    _claude_quota_accounts(tmp_path, monkeypatch, pro=10)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    mod.write_cooldown({'status': 'rate_limited', 'account': 'pro', 'run_id': 'prior',
        'evidence': {'signature': 'rate_limit'},
        'reset_at': (datetime.now(UTC) + timedelta(hours=2)).isoformat(),
        'provenance': {'requested': {'adapter': 'claude'}, 'resolved_model': 'claude-opus-5-5'}})
    args = mod.parser().parse_args(command[2:] + ['--account', 'pro'])
    args.timeout_seconds = 60
    args.fallback_from = {'attempt': 1, 'status': 'rate_limited'}
    args._rate_limit_retry = {'until': (datetime.now(UTC) + timedelta(seconds=5)).isoformat(),
                            'deadline': time.monotonic() + 60}
    sleep = time.sleep

    def no_pacing_sleep(seconds):
        attempts = list(run.glob('tasks/*/attempt-*/attempt.json'))
        assert not any(json.loads(path.read_text()).get('queue_reason') == 'rate_limit'
                       for path in attempts), 'known cooldown block must not wait for pacing'
        sleep(seconds)

    monkeypatch.setattr(mod.time, 'sleep', no_pacing_sleep)
    assert mod.dispatch(args) == 1
    attempt = json.loads(next(run.glob('tasks/*/attempt-*/attempt.json')).read_text())
    assert attempt['evidence']['signature'] == 'cooldown_active'


def test_explicit_account_recovers_after_a_newer_healthy_quota_reading(tmp_path, monkeypatch):
    from datetime import UTC, datetime, timedelta

    _claude_quota_accounts(tmp_path, monkeypatch, pro=10)
    mod = load_dispatch_module()
    mod.write_cooldown({'status': 'usage_limited', 'account': 'pro', 'run_id': 'prior',
        'evidence': {'signature': 'usage_limit', 'excerpt': 'weekly limit'},
        'reset_at': (datetime.now(UTC) + timedelta(hours=2)).isoformat(),
        'provenance': {'requested': {'adapter': 'claude'}, 'resolved_model': 'claude-opus-5-5'}},
        at=datetime.now(UTC) - timedelta(hours=1))

    assert mod.exec_routing.cooling('claude', 'claude-opus-5-5', account='pro', explicit_account=True) is None


def test_nested_attempt_retains_parent_in_receipt_and_index(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 2,
              'run_dir': str(tmp_path / 'parent')}
    monkeypatch.setenv('PROVENANT_PARENT_ATTEMPT', json.dumps(parent))
    args = dispatch_args(task_id='child')
    row = mod.contract_row(args, tmp_path, 1, tmp_path / 'dispatch/tasks/child/attempt-001', {}, timestamp())
    assert row['parent'] == parent
    row.update(status='ok', ended_at=timestamp())
    mod.append_index(row, tmp_path, root=tmp_path / '.agent-run')
    entry = json.loads((tmp_path / '.agent-run/runs/index.jsonl').read_text())
    assert entry['parent'] == parent


def lineage_parent_environment(parent):
    (Path(parent['run_dir']) / 'dispatch-owner.json').write_text(json.dumps({'run_token': 'parent-token'}))
    return {'PROVENANT_PARENT_ATTEMPT': json.dumps(parent), 'PROVENANT_PARENT_RUN_TOKEN': 'parent-token'}


@pytest.mark.parametrize('kind', ['dispatch', 'batch'])
@pytest.mark.parametrize('parent_state', ['running', 'terminal'])
def test_nested_cli_publishes_matching_owner_and_bidirectional_lineage(tmp_path, kind, parent_state):
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 2, 'run_dir': str(parent_dir)}
    parent_path = parent_dir / 'tasks/writer/attempt-002/attempt.json'
    parent_path.parent.mkdir(parents=True)
    parent_path.write_text(json.dumps({**parent, 'schema': 'fabric.attempt.v1', 'state': parent_state}))
    (parent_dir / 'dispatch/tasks/writer/attempt-002/tmp/children').mkdir(parents=True)
    disable_memory_admission_floor(tmp_path)
    run = make_run(tmp_path, 'runs/nested')
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('Reply OK')
    adapter = tmp_path / 'adapter'
    write_success_adapter(adapter)
    manifest = tmp_path / 'tasks.json'
    manifest.write_text(json.dumps({'schema_version': 1, 'tasks': [{'id': 'child', 'adapter': 'codex', 'model': 'fixture',
                                              'prompt': 'Reply OK'}]}))
    wrapper = tmp_path / 'nested.py'
    # A fresh image starts with the lane's environment, just like provenant's CLI.
    wrapper.write_text(f'''import os,sys,json,pathlib
sys.path.insert(0, {str(SCRIPT.parent)!r})
import dispatch_run as dispatch
import batch_run as batch
real_setsid = os.setsid
def detach():
    real_setsid()
    row = dispatch.provider_exec._process_snapshot()[os.getpid()]
    pathlib.Path({str(tmp_path / 'detach-verified')!r}).write_text(str(bool(dispatch.provider_exec._nested_fabric_owner_token(row))))
os.setsid = detach
dispatch.CF_DISPATCH = pathlib.Path({str(adapter)!r})
batch.DISPATCH_RUN = pathlib.Path({str(wrapper)!r})
if '--batch-child' in sys.argv or {kind!r} == 'dispatch':
    raise SystemExit(dispatch.dispatch(dispatch.parser().parse_args()))
raise SystemExit(batch.batch(batch.parser().parse_args()))
''')
    wrapper.chmod(0o755)
    # Batch's child entry point must be executable as well.
    wrapper.write_text('#!' + sys.executable + '\n' + wrapper.read_text())
    argv = (['--run-dir', str(run), '--task-id', 'child', '--adapter', 'codex', '--model', 'fixture',
             '--prompt-file', str(prompt)] if kind == 'dispatch' else
            ['--run-dir', str(run), '--manifest', str(manifest)])
    env = {**os.environ, **lineage_parent_environment(parent), 'PROVENANT_HOST_LOCAL_ONLY': '1'}
    for key in ('PROVENANT_RUN_DIR', 'PROVENANT_RUN_TOKEN', 'PROVENANT_RUN_ID'):
        env.pop(key, None)
    result = subprocess.run([sys.executable, str(wrapper), *argv], cwd=tmp_path,
                            env=env, capture_output=True, text=True, timeout=30)
    if parent_state == 'terminal':
        assert result.returncode == 2, result.stdout + result.stderr
        failure = json.loads(result.stdout)
        assert failure['status'] == ('lineage_unavailable' if kind == 'dispatch' else 'custody_preflight_failed')
        assert 'parent attempt is not running' in failure['message']
        assert 'Traceback' not in result.stderr
        return
    assert result.returncode == 0, result.stdout + result.stderr
    assert (tmp_path / 'detach-verified').read_text() == 'True'
    owner_path = run / 'dispatch-owner.json'
    assert owner_path.exists(), 'direct nested CLI did not publish a verifiable owner'
    owner = json.loads(owner_path.read_text())
    assert owner['owner_pid'] == owner['owner_pgid']
    assert owner['owner_started_at'] and owner['run_token']
    assert owner['kind'] == kind
    assert json.loads((run / 'RUN_RECEIPT.json').read_text())['parent'] == parent
    child = json.loads((run / 'tasks/child/attempt-001/attempt.json').read_text())
    assert child['parent'] == parent
    row = mod.contract_row(SimpleNamespace(tool='codex', alias='senior', model='fixture', effort='low',
                           task_id='writer', access_mode='read_only', worktree=None), parent_dir, 2,
                           parent_path.parent, {}, timestamp())
    assert [(item['run_id'], item['task_id'], item['attempt']) for item in row['children']] == [
        (child['run_id'], 'child', 1)]


@pytest.mark.parametrize('provider_status', ['ok', 'failed'])
@pytest.mark.parametrize(('child_status', 'reason'), [('failed', 'adapter_exit'), ('lost', 'owner_reaped'),
                                                      ('unreadable', 'completion evidence unavailable'),
                                                      ('invalidshape', 'completion evidence unavailable'),
                                                      ('unpublished', 'prelaunch_failed'),
                                                      ('reaped', 'owner exited without terminal attempt evidence')])
def test_nested_failure_is_named_without_replacing_parent_provider_status(
        tmp_path, monkeypatch, provider_status, child_status, reason):
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    parent_path = parent_dir / 'tasks/writer/attempt-001/attempt.json'
    parent_path.parent.mkdir(parents=True)
    parent_path.write_text(json.dumps({**parent, 'state': 'running'}))
    (parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children').mkdir(parents=True)
    child_dir = make_run(tmp_path, 'runs/child')
    (child_dir / 'dispatch-owner.json').write_text(json.dumps({
        'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
        'run_token': 'child-token', 'parent': parent}))
    with monkeypatch.context() as patch:
        for key, value in lineage_parent_environment(parent).items():
            patch.setenv(key, value)
        mod.run_lineage.register_run(child_dir, 'mcp-child')
    child_path = child_dir / 'tasks/junior/attempt-001/attempt.json'
    child_path.parent.mkdir(parents=True)
    child_path.write_text(json.dumps({'run_id': 'mcp-child', 'task_id': 'junior', 'attempt': 1,
                                     'state': 'terminal', 'status': child_status, 'error': reason}))
    if child_status == 'invalidshape':
        child_path.write_text('[]')
    elif child_status == 'unreadable':
        child_path.write_text('invalid JSON')
    elif child_status == 'reaped':
        child_path.unlink()
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            **json.loads((child_dir / 'dispatch-owner.json').read_text()),
            'owner_pid': os.getpid(), 'owner_started_at': 'previous process with this PID'}))
    elif child_status == 'unpublished':
        child_path.unlink()
        receipt_path = child_dir / 'RUN_RECEIPT.json'
        receipt_path.write_text(json.dumps({**json.loads(receipt_path.read_text()),
            'status': 'failed', 'terminal_reason': reason}))
    args = dispatch_args(task_id='writer')
    legacy = {'started_at': timestamp(), 'finished_at': timestamp(),
              'outcome': provider_status, 'status': provider_status, 'result': 'result.md',
              'attempt_path': 'dispatch/tasks/writer/attempt-001/attempt.json', 'requested_route': {}}
    adapter = {'status': provider_status, 'error': None if provider_status == 'ok' else 'provider_exit'}
    row = mod.terminal_contract(args, parent_dir, legacy, adapter, 1, parent_path.parent)
    assert row['status'] == provider_status
    assert row['children'][0]['status'] == {'unreadable': 'lost', 'invalidshape': 'lost', 'unpublished': 'failed', 'reaped': 'lost'}.get(child_status, child_status)
    assert 'mcp-child' in (row.get('error') or '') and reason in row['error']
    if provider_status == 'failed':
        assert 'provider_exit' in row['error']


@pytest.mark.parametrize('stop', ['normal', 'cancelled'])
def test_parent_cleanup_spares_real_direct_nested_cli(tmp_path, monkeypatch, stop):
    mod = load_dispatch_module()
    disable_memory_admission_floor(tmp_path)
    parent_dir = make_run(tmp_path, 'runs/parent')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    parent_path = parent_dir / 'tasks/writer/attempt-001/attempt.json'
    parent_path.parent.mkdir(parents=True)
    parent_path.write_text(json.dumps({**parent, 'state': 'running'}))
    lineage_parent_environment(parent)
    child_dir = make_run(tmp_path, 'runs/child')
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('Reply OK')
    adapter = tmp_path / 'adapter'
    write_success_adapter(adapter)
    ready = tmp_path / 'child-ready'
    released = tmp_path / 'child-released'
    observed = tmp_path / 'child-observed'
    adapter.write_text(adapter.read_text().replace('while [',
        f'touch {str(ready)!r}\nwhile [ ! -f {str(released)!r} ]; do sleep .02; done\nwhile [', 1))
    original_sample = mod.provider_exec._Descendants.sample

    def sample(descendants, include_reparented=False):
        rows = original_sample(descendants, include_reparented)
        if ready.exists():
            owner = json.loads((child_dir / 'dispatch-owner.json').read_text())
            if any(pid == owner['owner_pid'] for pid, _ in descendants.spared):
                observed.touch()
        return rows

    # Reparenting before the first census leaves no observed ancestry to spare.
    # Keep the provider alive until the supervisor recognises the nested owner.
    monkeypatch.setattr(mod.provider_exec._Descendants, 'sample', sample)
    wrapper = tmp_path / 'nested.py'
    wrapper.write_text(f'''import sys,pathlib
sys.path.insert(0, {str(SCRIPT.parent)!r})
import dispatch_run as dispatch
dispatch.CF_DISPATCH = pathlib.Path({str(adapter)!r})
raise SystemExit(dispatch.dispatch(dispatch.parser().parse_args()))
''')
    argv = [sys.executable, str(wrapper), '--run-dir', str(child_dir), '--task-id', 'junior',
            '--adapter', 'codex', '--model', 'fixture', '--prompt-file', str(prompt), '--timeout-seconds', '30']
    code = f'''import json,os,pathlib,subprocess,time
child = subprocess.Popen({argv!r}, env={{**os.environ, 'PROVENANT_HOST_LOCAL_ONLY':'1'}},
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path('nested.pid').write_text(str(child.pid))
deadline = time.monotonic() + 8
while not pathlib.Path({str(observed)!r}).exists():
    if child.poll() is not None or time.monotonic() >= deadline:
        raise SystemExit('supervisor did not observe the live nested owner')
    time.sleep(.02)
print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
if {stop!r} == 'normal':
    print(json.dumps({{'type':'turn.completed'}}), flush=True)
else:
    time.sleep(30)
'''
    old = os.environ.get('PROVENANT_NO_OS_CONFINEMENT')
    os.environ['PROVENANT_NO_OS_CONFINEMENT'] = '1'
    try:
        plan = mod.provider_exec.build_plan('codex', {'resolved_model': 'fixture', 'model_family': 'openai',
            'endpoint_provider': 'openai', 'effort': 'high'}, 'hello', cwd=tmp_path,
            workspace_root=tmp_path, run_dir=parent_dir / 'dispatch/tasks/writer/attempt-001',
            timeout_seconds=12)
    finally:
        if old is None:
            os.environ.pop('PROVENANT_NO_OS_CONFINEMENT', None)
        else:
            os.environ['PROVENANT_NO_OS_CONFINEMENT'] = old
    plan.update(argv=[sys.executable, '-u', '-c', code], attempt_identity=parent, grace_seconds=.1)
    try:
        record = mod.provider_exec.execute(plan, tmp_path / 'result.md',
            cancelled=lambda: stop == 'cancelled' and observed.exists(),
            env={**os.environ, 'PROVENANT_RUN_TOKEN': 'parent-token'})
        assert record['status'] == ('ok' if stop == 'normal' else 'cancelled')
        assert record.get('spared', 0) >= 1
        assert record['reaped'] == []
        # Completion must follow parent cleanup, independent of scheduler speed.
        released.touch()
        owner = json.loads((child_dir / 'dispatch-owner.json').read_text())
        child_path = child_dir / 'tasks/junior/attempt-001/attempt.json'
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if child_path.exists() and json.loads(child_path.read_text()).get('state') == 'terminal':
                break
            time.sleep(.05)
        assert json.loads(child_path.read_text())['status'] == ('cancelled' if stop == 'cancelled' else 'ok')
        assert owner['owner_pid'] == owner['owner_pgid']
    finally:
        released.touch()
        # Observe the owner's exit too: its first terminal row precedes final cleanup.
        path = child_dir / 'dispatch-owner.json'
        if path.exists():
            owner = json.loads(path.read_text())
            attempt_dir = child_dir / 'dispatch/tasks/junior/attempt-001'
            if attempt_dir.exists():
                mod.run_records.create_cancellation_marker(child_dir, attempt_dir)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                info = mod.process_info.process(owner['owner_pid'])
                if info is None or info.lstart != owner['owner_started_at'] or 'Z' in info.stat:
                    break
                time.sleep(.02)
            else:
                pytest.fail('nested owner did not finish after cooperative cleanup')


def test_nested_registration_writes_only_the_parent_attempt_private_tmp(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    parent_path = parent_dir / 'tasks/writer/attempt-001/attempt.json'
    parent_path.parent.mkdir(parents=True)
    parent_path.write_text(json.dumps({**parent, 'state': 'running'}))
    private_tmp = parent_dir / 'dispatch/tasks/writer/attempt-001/tmp'
    (private_tmp / 'children').mkdir(parents=True)
    child_dir = make_run(tmp_path, 'runs/child')
    (child_dir / 'dispatch-owner.json').write_text(json.dumps({
        'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
        'run_token': 'child-token', 'parent': parent}))
    for key, value in lineage_parent_environment(parent).items():
        monkeypatch.setenv(key, value)
    parent_path.parent.chmod(0o500)
    try:
        mod.run_lineage.register_run(child_dir, 'mcp-child')
        assert list((private_tmp / 'children').glob('*.json'))
    finally:
        parent_path.parent.chmod(0o700)


@pytest.mark.parametrize('spoof_fd', [False, True])
def test_busy_nested_cli_preserves_the_active_owner_record(tmp_path, spoof_fd):
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'busy-child')
    owner_path = run / 'dispatch-owner.json'
    owner_path.write_text(json.dumps({'run_token': 'active-owner', 'owner_pid': os.getpid()}))
    before = owner_path.read_bytes()
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(tmp_path)}
    custody = mod.acquire_run_custody(run)
    fd = os.open(run / 'MANIFEST.md', os.O_RDWR)
    environment = {**os.environ, **lineage_parent_environment(parent), 'PROVENANT_HOST_LOCAL_ONLY': '1'}
    if spoof_fd:
        environment.update(PROVENANT_NESTED_CUSTODY_FD=str(fd), PROVENANT_RUN_DIR=str(run),
                           PROVENANT_RUN_TOKEN='spoofed-token')
    try:
        result = subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run), '--prompt-stdin',
            '--adapter', 'codex', '--model', 'fixture'], input='hello', cwd=tmp_path,
            env=environment, pass_fds=(fd,),
            text=True, capture_output=True, timeout=10)
        assert json.loads(result.stdout)['status'] == 'run_custody_busy'
        assert owner_path.read_bytes() == before
    finally:
        os.close(fd)
        custody.close()


@pytest.mark.parametrize('terminal_status', ['ok', 'cancelled'])
def test_terminal_publication_includes_child_registered_while_waiting_for_custody(tmp_path, monkeypatch, terminal_status):
    import threading
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    args = SimpleNamespace(tool='codex', alias='senior', model='fixture', effort='low',
                           task_id='writer', access_mode='read_only', worktree=None)
    path = parent_dir / 'tasks/writer/attempt-001/attempt.json'
    row = mod.contract_row(args, parent_dir, 1, parent_dir / 'dispatch/tasks/writer/attempt-001',
                           {'run_id': 'mcp-parent'}, '2026-10-22T00:00:00Z')
    mod.publish_contract(parent_dir, row)
    (parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children').mkdir(parents=True)
    child_dir = make_run(tmp_path, 'runs/child')
    child_process = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'],
                                     env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                          'PROVENANT_RUN_DIR': str(child_dir)},
                                     start_new_session=True)
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    (child_dir / 'dispatch-owner.json').write_text(json.dumps({
        'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
        'run_token': 'child-token', 'parent': parent, 'owner_pid': child_process.pid,
        'owner_pgid': child_process.pid, 'owner_started_at': mod.process_info.start_time(child_process.pid)}))
    waiting = threading.Event()
    real_flock = mod.fcntl.flock
    failures = []

    def observed_flock(fd, operation):
        if threading.current_thread().name == 'terminal-publisher':
            waiting.set()
        return real_flock(fd, operation)

    def publish():
        try:
            mod.publish_contract(parent_dir, {**row, 'state': 'terminal', 'status': terminal_status,
                                               'ended_at': '2026-10-22T00:00:01Z'})
        except BaseException as exc:
            failures.append(exc)

    monkeypatch.setattr(mod.fcntl, 'flock', observed_flock)
    thread = threading.Thread(target=publish, name='terminal-publisher')
    with mod.attempt_custody(parent_dir, path):
        thread.start()
        assert waiting.wait(3), failures
        with monkeypatch.context() as patch:
            for key, value in lineage_parent_environment(parent).items():
                patch.setenv(key, value)
            mod.run_lineage.register_run(child_dir, 'mcp-child')
    thread.join(timeout=8)
    assert not thread.is_alive() and not failures
    terminal = json.loads(path.read_text())
    assert terminal['state'] == 'terminal'
    try:
        assert [child['run_id'] for child in terminal['children']] == ['mcp-child']
        if terminal_status == 'cancelled':
            assert child_process.poll() is not None, 'late registered child survived parent cancellation'
    finally:
        child_process.terminate()
        child_process.wait(timeout=3)


@pytest.fixture
def cancel_cascade(tmp_path):
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    child_dir = make_run(tmp_path, 'runs/child')
    args = SimpleNamespace(tool='codex', alias='senior', model='fixture', effort='low',
                           task_id='writer', access_mode='read_only', worktree=None)
    row = mod.contract_row(args, parent_dir, 1, parent_dir / 'dispatch/tasks/writer/attempt-001',
                           {'run_id': 'mcp-parent'}, '2026-10-22T00:00:00Z')
    row['ended_at'] = '2026-10-22T00:00:01Z'
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    (child_dir / 'RUN_RECEIPT.json').write_text(json.dumps({'status': 'active', 'parent': parent}))
    journal = parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children'
    journal.mkdir(parents=True)
    (journal / 'child.json').write_text(json.dumps({
        'run_id': 'mcp-child', 'run_dir': str(child_dir), 'run_token': 'child-token'}))
    return mod, parent_dir, child_dir, row


@pytest.mark.parametrize('token', [None, 'unrelated-token', 'child-token-extra'])
def test_cancel_cascade_does_not_signal_forged_owner(cancel_cascade, token):
    mod, parent_dir, child_dir, row = cancel_cascade
    environment = {key: value for key, value in os.environ.items() if key != 'PROVENANT_RUN_TOKEN'}
    if token:
        environment['PROVENANT_RUN_TOKEN'] = token
    sleeper = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                               env=environment, start_new_session=True)
    try:
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'child-token', 'owner_pid': sleeper.pid, 'owner_pgid': sleeper.pid,
            'owner_started_at': mod.process_info.start_time(sleeper.pid)}))
        mod.publish_contract(parent_dir, {**row, 'state': 'terminal', 'status': 'cancelled'})
        assert sleeper.poll() is None, 'forged owner caused an unrelated process to be signalled'
        terminal = json.loads((parent_dir / row['paths']['receipt']).read_text())
        cancellation = terminal['children'][0]['cancellation']
        assert cancellation['status'] == 'skipped'
        assert 'token' in cancellation['reason']
    finally:
        if sleeper.poll() is None:
            sleeper.kill()
        sleeper.wait(timeout=3)


@pytest.mark.parametrize('ignore_term', [False, True])
def test_cancel_cascade_still_cancels_genuine_child(cancel_cascade, ignore_term):
    mod, parent_dir, child_dir, row = cancel_cascade
    code = f'''import json,os,pathlib,signal,time
def cancelled(signum, frame):
    # Cascade TERMs can arrive twice; a nested call would truncate the receipt.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    receipt = pathlib.Path({str(child_dir / 'RUN_RECEIPT.json')!r})
    receipt.write_text(json.dumps({{**json.loads(receipt.read_text()), 'status': 'cancelled'}}))
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    os.kill(os.getpid(), signal.SIGTERM)
signal.signal(signal.SIGTERM, signal.SIG_IGN if {ignore_term!r} else cancelled)
print('ready', flush=True)
time.sleep(60)
'''
    child = subprocess.Popen([sys.executable, '-c', code], stdout=subprocess.PIPE, text=True,
                             env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                  'PROVENANT_RUN_DIR': str(child_dir)},
                             start_new_session=True)
    try:
        assert child.stdout.readline().strip() == 'ready'
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'child-token', 'owner_pid': child.pid, 'owner_pgid': child.pid,
            'owner_started_at': mod.process_info.start_time(child.pid)}))
        mod.publish_contract(parent_dir, {**row, 'state': 'terminal', 'status': 'cancelled'})
        assert child.wait(timeout=3) == -(signal.SIGKILL if ignore_term else signal.SIGTERM)
        terminal = json.loads((parent_dir / row['paths']['receipt']).read_text())
        assert terminal['children'][0]['cancellation']['status'] == 'signalled'
        assert any(target['scope'] == 'group' and target['status'] == 'signalled'
                   for target in terminal['children'][0]['cancellation']['targets'])
        if not ignore_term:
            assert terminal['children'][0]['status'] == 'cancelled'
            assert 'cancelled' in terminal['error']
        evidence = terminal['children'][0]['cancellation']
        mod.publish_contract(parent_dir, terminal)
        republished = json.loads((parent_dir / row['paths']['receipt']).read_text())
        assert republished['children'][0]['cancellation'] == evidence
        assert republished['children'][0]['status'] == ('lost' if ignore_term else 'cancelled')
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=3)
        child.stdout.close()


def test_cancel_cascade_rejects_matching_sibling_environment(cancel_cascade):
    mod, parent_dir, child_dir, row = cancel_cascade
    sibling = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                               env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                    'PROVENANT_RUN_DIR': str(child_dir)}, start_new_session=True)
    try:
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'child-token', 'owner_pid': sibling.pid, 'owner_pgid': sibling.pid,
            'owner_started_at': mod.process_info.start_time(sibling.pid)}))
        code = '''import json,pathlib,sys
sys.path.insert(0, sys.argv[1])
import dispatch_run
dispatch_run.publish_contract(pathlib.Path(sys.argv[2]), json.loads(sys.argv[3]))
'''
        result = subprocess.run([sys.executable, '-c', code, str(SCRIPT.parent), str(parent_dir),
                                 json.dumps({**row, 'state': 'terminal', 'status': 'cancelled'})],
                                capture_output=True, text=True, timeout=15)
        assert result.returncode == 0, result.stderr
        assert sibling.poll() is None, 'matching environment authorised signalling a sibling'
        terminal = json.loads((parent_dir / row['paths']['receipt']).read_text())
        assert terminal['children'][0]['cancellation']['status'] == 'skipped'
        assert 'descendant' in terminal['children'][0]['cancellation']['reason']
    finally:
        if sibling.poll() is None:
            sibling.kill()
        sibling.wait(timeout=3)


@pytest.mark.parametrize('shared_host', [False, True])
@pytest.mark.parametrize('subreaper', [False, True])
def test_cancel_cascade_preserves_observed_owner_after_provider_exit(cancel_cascade, tmp_path, monkeypatch,
                                                                  shared_host, subreaper):
    mod, parent_dir, child_dir, row = cancel_cascade
    provider = mod.provider_exec
    child_script = tmp_path / 'nested-owner.py'
    child_script.write_text(f'''import json,os,pathlib,signal,sys,time
sys.path.insert(0, {str(SCRIPT.parent)!r})
import process_info
directory = pathlib.Path({str(child_dir)!r})
def cancelled(signum, frame):
    # Cascade TERMs can arrive twice; a nested call would truncate the receipt.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    receipt = directory / 'RUN_RECEIPT.json'
    receipt.write_text(json.dumps({{**json.loads(receipt.read_text()), 'status': 'cancelled'}}))
    raise SystemExit(0)
signal.signal(signal.SIGTERM, cancelled)
pid = os.getpid()
(directory / 'dispatch-owner.json').write_text(json.dumps({{
    'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(directory),
    'run_token': 'child-token', 'owner_pid': pid, 'owner_pgid': pid,
    'owner_started_at': process_info.start_time(pid)}}))
time.sleep(60)
''')
    code = f'''import os,subprocess,time
subprocess.Popen([{sys.executable!r}, {str(child_script)!r}], start_new_session=True,
    env={{**os.environ, 'PROVENANT_RUN_DIR': {str(child_dir)!r}, 'PROVENANT_RUN_TOKEN': 'child-token'}},
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(60)
'''
    host_pid_path = tmp_path / 'host.pid'
    if shared_host:
        host_script = tmp_path / 'shared-host.py'
        host_script.write_text(code)
        code = f'''import pathlib,subprocess,time
host = subprocess.Popen([{sys.executable!r}, {str(host_script)!r}], start_new_session=True)
pathlib.Path({str(host_pid_path)!r}).write_text(str(host.pid))
time.sleep(60)
'''
        from dataclasses import replace
        snapshot = provider._process_snapshot
        linux_snapshot = provider._linux_tree_snapshot

        def named_host(rows):
            # Model the OS command name at its boundary; all ancestry is real.
            if rows and host_pid_path.exists():
                pid = int(host_pid_path.read_text())
                if pid in rows:
                    rows[pid] = replace(rows[pid], command='codex-code-mode-host')
            return rows

        monkeypatch.setattr(provider, '_process_snapshot', lambda: named_host(snapshot()))
        monkeypatch.setattr(provider, '_linux_tree_snapshot', lambda *args: named_host(linux_snapshot(*args)))
    observed = False
    sample = provider._Descendants.sample

    def observe(tree, include_reparented=False):
        nonlocal observed
        rows = sample(tree, include_reparented)
        observed = observed or bool(tree.verified_owners)
        return rows

    monkeypatch.setattr(provider._Descendants, 'sample', observe)
    # Exercise macOS-style reparenting on Linux too, rather than adopting the child.
    if not subreaper:
        monkeypatch.setattr(provider, '_enable_subreaper', lambda: False)
    monkeypatch.setenv('PROVENANT_NO_OS_CONFINEMENT', '1')
    plan = provider.build_plan('claude', {'resolved_model': 'fixture', 'model_family': 'anthropic',
        'endpoint_provider': 'anthropic', 'effort': 'high'}, 'hello', cwd=tmp_path,
        workspace_root=tmp_path, run_dir=parent_dir / 'dispatch/tasks/writer/attempt-001',
        timeout_seconds=10)
    plan.update(argv=[sys.executable, '-u', '-c', code], grace_seconds=.1,
                attempt_identity={'run_id': 'mcp-parent', 'run_dir': str(parent_dir),
                                  'task_id': 'writer', 'attempt': 1})
    try:
        record = provider.execute(plan, tmp_path / 'result.md', cancelled=lambda: observed)
        assert record['status'] == 'cancelled'
        if shared_host:
            assert record['children'][0]['cancellation']['status'] == 'skipped'
            assert 'descendant' in record['children'][0]['cancellation']['reason']
            owner = json.loads((child_dir / 'dispatch-owner.json').read_text())
            assert mod.process_info._probe_process_row(owner['owner_pid']) is not None
            return
        assert record['children'][0]['cancellation']['status'] == 'signalled'
        args = SimpleNamespace(tool='claude', alias='senior', model='fixture', effort='high',
                               task_id='writer', access_mode='read_only', worktree=None, _last_plan=plan)
        legacy = {'started_at': row['started_at'], 'finished_at': row['ended_at'],
                  'outcome': 'cancelled', 'status': 'cancelled', 'result': None,
                  'attempt_path': 'dispatch/tasks/writer/attempt-001/attempt.json', 'requested_route': {}}
        terminal = mod.terminal_contract(args, parent_dir, legacy, record, 1, Path(plan['run_dir']))
        mod.publish_contract(parent_dir, terminal)
        saved = json.loads((parent_dir / row['paths']['receipt']).read_text())
        assert saved['children'][0]['cancellation'] == record['children'][0]['cancellation']
        assert saved['children'][0]['status'] == 'cancelled'
        assert 'cancelled' in saved['error']
    finally:
        owner_path = child_dir / 'dispatch-owner.json'
        if owner_path.exists():
            try:
                os.killpg(json.loads(owner_path.read_text())['owner_pid'], signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        if host_pid_path.exists():
            try:
                os.killpg(int(host_pid_path.read_text()), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass


def test_terminal_contract_ignores_adapter_paths_outside_run(cancel_cascade, tmp_path):
    mod, parent_dir, child_dir, row = cancel_cascade
    attempt_dir = parent_dir / 'dispatch/tasks/writer/attempt-001'
    args = dispatch_args(tool='claude', effort='high', task_id='writer')
    legacy = {'started_at': row['started_at'], 'finished_at': row['ended_at'],
              'outcome': 'cancelled', 'status': 'cancelled', 'result': None,
              'attempt_path': 'dispatch/tasks/writer/attempt-001/attempt.json', 'requested_route': {}}
    adapter = {'status': 'cancelled', 'paths': {'events': str(tmp_path / 'outside-events.jsonl'),
                                                'stderr': '../../outside-stderr.log'}}
    terminal = mod.terminal_contract(args, parent_dir, legacy, adapter, 1, attempt_dir)
    assert terminal['paths']['events'] == row['paths']['events']
    assert terminal['paths']['stderr'] == row['paths']['stderr']
    mod.publish_contract(parent_dir, terminal)
    saved = json.loads((parent_dir / row['paths']['receipt']).read_text())
    assert saved['state'] == 'terminal'


@pytest.mark.parametrize('unavailable', ['environment', 'exited', 'identity'])
def test_cancel_cascade_reports_owner_verification_failure(cancel_cascade, monkeypatch, unavailable):
    mod, parent_dir, child_dir, row = cancel_cascade
    child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                             env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                  'PROVENANT_RUN_DIR': str(child_dir)}, start_new_session=True)
    try:
        owner = {'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
                 'run_token': 'child-token', 'owner_pid': child.pid, 'owner_pgid': child.pid,
                 'owner_started_at': mod.process_info.start_time(child.pid)}
        if unavailable == 'identity':
            owner['owner_started_at'] = 'changed'
        (child_dir / 'dispatch-owner.json').write_text(json.dumps(owner))
        if unavailable == 'environment':
            monkeypatch.setattr(mod.provider_exec, '_process_environment', lambda pid: ())
        elif unavailable == 'exited':
            child.kill()
            child.wait(timeout=3)
        children = mod.run_lineage.cancel_children(parent_dir, 'writer', 1, timeout_seconds=0)
        evidence = children[0]['cancellation']
        assert evidence['status'] == 'skipped'
        assert {'environment': 'environment unreadable', 'exited': 'owner already exited',
                'identity': 'owner identity changed or unavailable'}[unavailable] == evidence['reason']
        if unavailable != 'exited':
            assert child.poll() is None
        if unavailable in {'exited', 'identity'}:
            # Even if a mismatch is repaired later, its first outcome is final.
            if unavailable == 'identity':
                owner['owner_started_at'] = mod.process_info.start_time(child.pid)
                (child_dir / 'dispatch-owner.json').write_text(json.dumps(owner))
            mod.publish_contract(parent_dir, {**row, 'state': 'terminal', 'status': 'cancelled',
                                               'children': children})
            terminal = json.loads((parent_dir / row['paths']['receipt']).read_text())
            assert terminal['children'][0]['cancellation'] == evidence
            if unavailable == 'identity':
                assert child.poll() is None
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=3)


def test_cancel_cascade_rejects_owner_replaced_after_enumeration(cancel_cascade):
    mod, parent_dir, child_dir, _ = cancel_cascade
    sleeper = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                               env={**os.environ, 'PROVENANT_RUN_TOKEN': 'sibling-token',
                                    'PROVENANT_RUN_DIR': str(child_dir)},
                               start_new_session=True)
    try:
        owner = {'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
                 'run_token': 'child-token', 'owner_pid': sleeper.pid, 'owner_pgid': sleeper.pid,
                 'owner_started_at': mod.process_info.start_time(sleeper.pid)}
        (child_dir / 'dispatch-owner.json').write_text(json.dumps(owner))
        child_rows = mod.run_lineage.children(parent_dir, 'writer', 1)
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({**owner, 'run_token': 'sibling-token'}))
        mod.run_lineage.cancel_children(parent_dir, 'writer', 1, child_rows=child_rows, timeout_seconds=0)
        assert sleeper.poll() is None, 'replacement owner redirected cancellation to a sibling run'
        assert child_rows[0]['cancellation']['status'] == 'skipped'
        assert 'token' in child_rows[0]['cancellation']['reason']
    finally:
        if sleeper.poll() is None:
            sleeper.kill()
        sleeper.wait(timeout=3)


def test_cancel_cascade_rejects_inherited_parent_identity(cancel_cascade):
    mod, parent_dir, child_dir, row = cancel_cascade
    sleeper = subprocess.Popen([sys.executable, '-c', 'import time; print("ready", flush=True); time.sleep(60)'],
                               env={**os.environ, 'PROVENANT_RUN_TOKEN': 'parent-token',
                                    'PROVENANT_RUN_DIR': str(parent_dir)}, start_new_session=True,
                               stdout=subprocess.PIPE, text=True)
    try:
        assert sleeper.stdout.readline().strip() == 'ready'
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'parent-token', 'owner_pid': sleeper.pid, 'owner_pgid': sleeper.pid,
            'owner_started_at': mod.process_info.start_time(sleeper.pid)}))
        journal = parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children/child.json'
        journal.write_text(json.dumps({'run_id': 'mcp-child', 'run_dir': str(child_dir),
                                       'run_token': 'parent-token'}))
        mod.publish_contract(parent_dir, {**row, 'state': 'terminal', 'status': 'cancelled'})
        assert sleeper.poll() is None, 'inherited parent identity authorised a forged child run'
        terminal = json.loads((parent_dir / row['paths']['receipt']).read_text())
        assert terminal['children'][0]['cancellation']['status'] == 'skipped'
        assert 'directory' in terminal['children'][0]['cancellation']['reason']
    finally:
        if sleeper.poll() is None:
            sleeper.kill()
        sleeper.wait(timeout=3)
        sleeper.stdout.close()


@pytest.mark.parametrize('failure_at', ['initial', 'escalation'])
def test_cancel_cascade_records_unavailable_process_census(cancel_cascade, monkeypatch, failure_at):
    mod, parent_dir, child_dir, _ = cancel_cascade
    provider = mod.provider_exec
    child = subprocess.Popen([sys.executable, '-c', 'import time; print("ready", flush=True); time.sleep(60)'],
                             env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                  'PROVENANT_RUN_DIR': str(child_dir)}, start_new_session=True,
                             stdout=subprocess.PIPE, text=True)
    real_kill = os.kill
    snapshot = provider._process_snapshot
    linux_snapshot = provider._linux_tree_snapshot
    failed = failure_at == 'initial'
    signals = []

    def send_signal(pid, signum):
        nonlocal failed
        if pid == child.pid and signum != 0:
            signals.append(signum)
            failed = True
        else:
            real_kill(pid, signum)

    try:
        assert child.stdout.readline().strip() == 'ready'
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'child-token', 'owner_pid': child.pid, 'owner_pgid': child.pid,
            'owner_started_at': mod.process_info.start_time(child.pid)}))
        monkeypatch.setattr(provider, '_process_snapshot', lambda: None if failed else snapshot())
        monkeypatch.setattr(provider, '_linux_tree_snapshot',
                            lambda *args: None if failed else linux_snapshot(*args))
        monkeypatch.setattr(os, 'kill', send_signal)
        monkeypatch.setattr(os, 'killpg', send_signal)
        child_rows = mod.run_lineage.cancel_children(parent_dir, 'writer', 1, timeout_seconds=0)
        assert signals == ([] if failure_at == 'initial' else [signal.SIGTERM, signal.SIGTERM])
        evidence = child_rows[0]['cancellation']
        assert evidence['status'] == ('skipped' if failure_at == 'initial' else 'signalled')
        assert 'census' in (evidence.get('reason') or evidence['targets'][-1]['reason'])
    finally:
        real_kill(child.pid, signal.SIGKILL)
        child.wait(timeout=3)
        child.stdout.close()


@pytest.mark.parametrize('unavailable', ['census', 'signal', 'verification', 'environment'])
def test_cancel_cascade_retries_transient_skip_at_publication(cancel_cascade, monkeypatch, unavailable):
    mod, parent_dir, child_dir, row = cancel_cascade
    provider = mod.provider_exec
    child = subprocess.Popen([sys.executable, '-c', 'import time; print("ready", flush=True); time.sleep(60)'],
                             env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                  'PROVENANT_RUN_DIR': str(child_dir)}, start_new_session=True,
                             stdout=subprocess.PIPE, text=True)
    real_kill = os.kill

    def unavailable_signal(pid, signum):
        if signum:
            raise PermissionError('signal unavailable')
        return real_kill(pid, signum)

    def unavailable_environment(pid):
        raise OSError('verification unavailable')

    try:
        assert child.stdout.readline().strip() == 'ready'
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'child-token', 'owner_pid': child.pid, 'owner_pgid': child.pid,
            'owner_started_at': mod.process_info.start_time(child.pid)}))
        with monkeypatch.context() as patch:
            if unavailable == 'census':
                patch.setattr(provider, '_process_snapshot', lambda: None)
                patch.setattr(provider, '_linux_tree_snapshot', lambda *args: None)
            elif unavailable == 'signal':
                patch.setattr(os, 'kill', unavailable_signal)
                patch.setattr(os, 'killpg', unavailable_signal)
            else:
                patch.setattr(provider, '_process_environment',
                              unavailable_environment if unavailable == 'verification' else lambda pid: ())
            row['children'] = mod.run_lineage.cancel_children(parent_dir, 'writer', 1, timeout_seconds=0)
        evidence = row['children'][0]['cancellation']
        assert evidence['status'] == 'skipped'
        assert evidence['reason'] == {
            'census': 'process census unavailable', 'signal': 'process signal unavailable',
            'verification': 'owner verification or signal unavailable',
            'environment': 'environment unreadable'}[unavailable]
        assert child.poll() is None

        mod.publish_contract(parent_dir, {**row, 'state': 'terminal', 'status': 'cancelled'})
        assert child.wait(timeout=3) == -signal.SIGTERM
        terminal = json.loads((parent_dir / row['paths']['receipt']).read_text())
        assert terminal['children'][0]['cancellation']['status'] == 'signalled'
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=3)
        child.stdout.close()


@pytest.mark.parametrize('change_before', ['owner validation', 'SIGTERM', 'SIGKILL'])
def test_cancel_cascade_rechecks_identity_before_each_signal(cancel_cascade, monkeypatch, change_before):
    from dataclasses import replace
    import provider_exec

    mod, parent_dir, child_dir, _ = cancel_cascade
    child = subprocess.Popen([sys.executable, '-c', 'import time; print("ready", flush=True); time.sleep(60)'],
                             env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                  'PROVENANT_RUN_DIR': str(child_dir)},
                             start_new_session=True, stdout=subprocess.PIPE, text=True)
    real_kill = os.kill
    probe = mod.process_info._probe_process_row
    start_time = mod.process_info.start_time
    environment = provider_exec._process_environment
    changed = False
    signals = []

    def owner_start_time(pid):
        nonlocal changed
        result = start_time(pid)
        if pid == child.pid and change_before == 'owner validation':
            changed = True
        return result

    def read_environment(pid):
        nonlocal changed
        result = environment(pid)
        if pid == child.pid and change_before == 'SIGTERM':
            changed = True
        return result

    def current_process(pid):
        row = probe(pid)
        return replace(row, started='0') if row and pid == child.pid and changed else row

    def send_signal(pid, signum):
        nonlocal changed
        if pid == child.pid and signum != 0:
            signals.append(signum)
            changed = True
        else:
            real_kill(pid, signum)

    try:
        assert child.stdout.readline().strip() == 'ready'
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'child-token', 'owner_pid': child.pid, 'owner_pgid': child.pid,
            'owner_started_at': mod.process_info.start_time(child.pid)}))
        child_rows = mod.run_lineage.children(parent_dir, 'writer', 1)
        monkeypatch.setattr(provider_exec, '_process_environment', read_environment)
        monkeypatch.setattr(mod.process_info, 'start_time', owner_start_time)
        monkeypatch.setattr(mod.process_info, '_probe_process_row', current_process)
        monkeypatch.setattr(os, 'kill', send_signal)
        monkeypatch.setattr(os, 'killpg', send_signal)
        mod.run_lineage.cancel_children(parent_dir, 'writer', 1, child_rows=child_rows, timeout_seconds=0)
        assert signals == ([signal.SIGTERM] if change_before == 'SIGKILL' else [])
        evidence = child_rows[0]['cancellation']
        assert evidence['status'] == ('signalled' if change_before == 'SIGKILL' else 'skipped')
        reasons = [evidence.get('reason', ''), *(target.get('reason', '') for target in evidence.get('targets', []))]
        assert any('identity' in reason for reason in reasons)
        assert child.poll() is None
    finally:
        real_kill(child.pid, signal.SIGKILL)
        child.wait(timeout=3)
        child.stdout.close()


def test_remote_nested_dispatch_records_source_child_and_refreshes_failure(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    parent_path = parent_dir / 'tasks/writer/attempt-001/attempt.json'
    parent_path.parent.mkdir(parents=True)
    parent_path.write_text(json.dumps({**parent, 'state': 'running'}))
    (parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children').mkdir(parents=True)
    child_dir = make_run(tmp_path, 'remote-launch')
    remote = {'run_id': 'mcp-child@peer', 'task_id': 'bulk@peer', 'attempt': 1, 'status': 'running'}
    request_path = tmp_path / 'request.json'
    wrapper = tmp_path / 'source.py'
    wrapper.write_text(f'''import os,sys,json,pathlib
from types import SimpleNamespace
sys.path.insert(0, {str(SCRIPT.parent)!r})
import dispatch_run as dispatch
def call(verb,cwd,request):
    row = dispatch.provider_exec._process_snapshot().get(os.getpid())
    captured = {{**request, 'owner_valid': bool(row and dispatch.provider_exec._nested_fabric_owner_token(row))}}
    pathlib.Path({str(request_path)!r}).write_text(json.dumps(captured))
    return {{'runs':[{remote!r}]}}
owner = SimpleNamespace(config=SimpleNamespace(peers={{'peer':{{}}}}), config_error=None, client_call=call)
dispatch.dispatch_preflight._fabric_hosts_owner = lambda: SimpleNamespace(lane_owner=lambda:owner, HostError=ValueError)
raise SystemExit(dispatch.dispatch(dispatch.parser().parse_args()))
''')
    result = subprocess.run([sys.executable, str(wrapper), '--run-dir', str(child_dir), '--adapter', 'codex',
        '--model', 'fixture', '--prompt-stdin'], input='hello', cwd=tmp_path, text=True, capture_output=True,
        env={**os.environ, **lineage_parent_environment(parent), 'PROVENANT_HOST_LOCAL_ONLY': '0'}, timeout=10)
    assert result.returncode == 0, result.stdout + result.stderr
    request = json.loads(request_path.read_text())
    assert request['prompt'] == 'hello'
    assert request['owner_valid']
    assert request.get('_lineage') == parent
    assert mod.run_lineage.children(parent_dir, 'writer', 1)[0]['run_id'] == remote['run_id']
    import fabric_lanes
    owner = fabric_lanes.LaneFederation.__new__(fabric_lanes.LaneFederation)
    owner.config = SimpleNamespace(peers={'peer': {}}, local_host='here')
    owner.config_error = None
    owner.request_project = lambda *_args: ('Repos/project', None)
    owner.reads = lambda *_args: {'runs': [{**remote, 'status': 'failed', 'error': 'peer_adapter_exit'}]}
    # Fabric owners carry this flag to avoid redispatching their own launch.
    # Their terminal child lookup must still reach the owning peer.
    monkeypatch.setenv('PROVENANT_HOST_LOCAL_ONLY', '1')
    monkeypatch.setattr(mod.dispatch_preflight, '_fabric_hosts_owner', lambda: SimpleNamespace(lane_owner=lambda: owner))
    args = dispatch_args(task_id='writer')
    legacy = {'started_at': timestamp(), 'finished_at': timestamp(),
              'outcome': 'ok', 'status': 'ok', 'result': 'result.md',
              'attempt_path': 'dispatch/tasks/writer/attempt-001/attempt.json', 'requested_route': {}}
    row = mod.terminal_contract(args, parent_dir, legacy, {'status': 'ok'}, 1, parent_path.parent)
    assert row['status'] == 'ok'
    assert 'mcp-child@peer' in row['error'] and 'peer_adapter_exit' in row['error']


def test_peer_placement_request_carries_delivery_run_id(tmp_path, monkeypatch, capsys):
    mod = load_dispatch_module()
    run_dir = make_run(tmp_path, 'placed')
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('placed\n', encoding='utf-8')
    seen = []
    owner = SimpleNamespace(config=SimpleNamespace(peers={'peer': {}}), config_error=None,
                            client_call=lambda _op, _cwd, request: seen.append(request) or {'status': 'ok', 'local': False})
    monkeypatch.delenv('PROVENANT_HOST_LOCAL_ONLY', raising=False)
    monkeypatch.setattr(mod.dispatch_preflight, '_fabric_hosts_owner', lambda: SimpleNamespace(lane_owner=lambda: owner, HostError=ValueError))
    monkeypatch.setattr(mod.run_lineage, 'register_external', lambda _result: None)
    monkeypatch.chdir(tmp_path)
    args = mod.parser().parse_args([
        '--run-dir', str(run_dir), '--task-id', 'placed', '--adapter', 'codex', '--prompt-file', str(prompt),
        '--alias', 'senior', '--role', 'worker', '--host', 'peer', '--delivery-run-id', 'delivery:1202'])
    assert mod.dispatch(args) == 0
    assert seen[0]['delivery_run_id'] == 'delivery:1202'


def test_peer_child_retains_parent_without_writing_the_source_host(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    child_dir = make_run(tmp_path, 'runs/child')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': '/unavailable-source/run'}
    monkeypatch.setenv('PROVENANT_PARENT_ATTEMPT', json.dumps(parent))
    monkeypatch.setenv('PROVENANT_PARENT_REMOTE', '1')
    mod.run_lineage.register_run(child_dir, 'mcp-child')
    assert json.loads((child_dir / 'RUN_RECEIPT.json').read_text())['parent'] == parent


@pytest.mark.parametrize('forgery', ['outside', 'symlink', 'missing_owner', 'wrong_token'])
def test_lineage_ignores_unverified_child_paths(tmp_path, monkeypatch, capsys, forgery):
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    journal = parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children'
    journal.mkdir(parents=True)
    outside = tmp_path / 'private'
    outside.mkdir()
    child_dir = tmp_path / '.agent-run/runs/child'
    child_dir.parent.mkdir(parents=True, exist_ok=True)
    if forgery == 'outside':
        child_dir = outside
    elif forgery == 'symlink':
        child_dir.symlink_to(outside, target_is_directory=True)
    else:
        child_dir.mkdir()
    (child_dir / 'RUN_RECEIPT.json').write_text(json.dumps({'status': 'failed', 'terminal_reason': 'PRIVATE_SENTINEL', 'parent': parent}))
    (child_dir / 'MANIFEST.md').write_text('# Run')
    if forgery != 'missing_owner':
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'owner-token', 'parent': parent}))
    (journal / 'forged.json').write_text(json.dumps({
        'run_id': 'mcp-child', 'run_dir': str(child_dir),
        'run_token': 'forged-token' if forgery == 'wrong_token' else 'owner-token'}))
    opened = []
    real_open = os.open
    def observed_open(path, *args, **kwargs):
        opened.append(str(path))
        return real_open(path, *args, **kwargs)
    monkeypatch.setattr(os, 'open', observed_open)
    assert mod.run_lineage.children(parent_dir, 'writer', 1) == []
    assert not any(path.endswith('RUN_RECEIPT.json') or path.endswith('attempt.json') for path in opened)
    assert 'ignored unverified lineage journal entry' in capsys.readouterr().err


@pytest.mark.parametrize('token', [None, 'other-lane-token', 'parent-token'])
def test_nested_registration_requires_parent_run_token(tmp_path, monkeypatch, token):
    mod = load_dispatch_module()
    parent_dir = make_run(tmp_path, 'runs/parent')
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 1, 'run_dir': str(parent_dir)}
    path = parent_dir / 'tasks/writer/attempt-001/attempt.json'
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({**parent, 'state': 'running'}))
    (parent_dir / 'dispatch-owner.json').write_text(json.dumps({'run_token': 'parent-token'}))
    journal = parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children'
    journal.mkdir(parents=True)
    child_dir = make_run(tmp_path, 'runs/child')
    (child_dir / 'dispatch-owner.json').write_text(json.dumps({'run_token': 'child-token'}))
    monkeypatch.setenv('PROVENANT_PARENT_ATTEMPT', json.dumps(parent))
    monkeypatch.delenv('PROVENANT_PARENT_RUN_TOKEN', raising=False)
    if token is not None:
        monkeypatch.setenv('PROVENANT_PARENT_RUN_TOKEN', token)
    if token == 'parent-token':
        mod.run_lineage.register_run(child_dir, 'mcp-child')
        assert list(journal.glob('*.json'))
    else:
        before = (child_dir / 'RUN_RECEIPT.json').read_bytes()
        with pytest.raises(ValueError, match='parent run token'):
            mod.run_lineage.register_run(child_dir, 'mcp-child')
        assert not list(journal.glob('*.json'))
        assert (child_dir / 'RUN_RECEIPT.json').read_bytes() == before


def test_model_fallback_retains_the_automatically_selected_claude_account(monkeypatch):
    plan = {"adapter": "claude", "model": "claude-opus-5-5", "account": "pro"}
    rows = [{"status": "usage_limited"}, {"status": "ok", "retryable": False}]
    alternatives = [{"adapter": "claude", "model": "sonnet"}]
    mod, args, launches = _sequence_fixture(monkeypatch, plan, rows, alternatives)
    mod.execute_attempt_sequence(args)
    assert launches == [(None, None, None), ("claude", "sonnet", "pro")]


def test_unreadable_account_quota_releases_an_automatic_pin_instead_of_crashing(tmp_path, monkeypatch):
    instance = tmp_path / 'instance'
    config = instance / 'config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {'pro': {'identity_dir': 5}}}}))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    mod = load_dispatch_module()
    assert mod.exec_routing.claude_account_needs_reselection('pro') is True


@pytest.mark.parametrize('reading', ['credit_only', 'error', 'advisory', 'malformed'])
def test_model_fallback_reselects_an_automatically_pinned_exhausted_account(tmp_path, monkeypatch, reading):
    from datetime import UTC, datetime, timedelta
    import provider_quota as quota

    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    instance = tmp_path / 'instance'
    config = instance / 'config/telemetry.json'
    config.parent.mkdir(parents=True)
    identities = {label: tmp_path / label for label in ('a', 'b')}
    config.write_text(json.dumps({'accounts': {'claude': {
        label: {'identity_dir': str(path)} for label, path in identities.items()}}}))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.delenv('CLAUDE_CONFIG_DIR', raising=False)
    for label, path in identities.items():
        quota.save_snapshot({'schema': quota.SCHEMA, 'host': quota.telemetry_host(),
                             'provider': 'claude', 'account': label, 'status': 'ok', 'windows': [
                                 {'name': 'five_hour', 'used_percent': 10, 'window_minutes': 300,
                                  'reset_at': (datetime.now(UTC) + timedelta(hours=2)).isoformat()},
                                 {'name': 'seven_day', 'used_percent': 10, 'window_minutes': 10080,
                                  'reset_at': (datetime.now(UTC) + timedelta(days=3)).isoformat()},
                             ]}, path)
    cache = quota.cache_path('claude', 'a', identity_dir=identities['a'])
    code = f'''import json,sys,os
from pathlib import Path
sys.stdin.read()
if any('opus' in arg for arg in sys.argv):
    path = Path({str(cache)!r})
    value = json.loads(path.read_text())
    value['windows'][0]['used_percent'] = 100
    value['windows'][0]['advisory'] = {reading == 'advisory'!r}
    value['status'] = {'error' if reading == 'error' else 'ok'!r}
    value['usage_credits'] = [{{'kind': 'credits', 'balance': 100, 'unit': 'credits'}}]
    path.write_text(json.dumps(value))
    if {reading == 'malformed'!r}:
        Path({str(config)!r}).write_text(json.dumps({{'accounts': ['broken']}}))
    print(json.dumps({{'type':'result','is_error':True,'result':'429 rate limit; retry after 2000 seconds'}}))
    sys.exit(1)
print(json.dumps({{'type':'result','result':str(os.environ.get('CLAUDE_CONFIG_DIR'))}}))
'''
    write_executable(tmp_path / 'provider-bin/claude', '#!/usr/bin/env python3\n' + code)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = 60
    args.fallback = json.dumps(['claude/sonnet'])
    assert mod.dispatch(args) == 0
    attempts = [json.loads(path.read_text()) for path in sorted(run.glob('tasks/*/attempt-*/attempt.json'))]
    # The healthy reselected account retries the same model before any model fallback.
    reselected = 'default' if reading == 'malformed' else 'b'
    assert [(row['account'], 'opus' in row['requested_route']['model']) for row in attempts] == [
        ('a', True), (reselected, True), (reselected, False)]
    assert (run / attempts[-1]['paths']['result']).read_text().strip() == (
        'None' if reading == 'malformed' else str(identities['b']))


def test_cross_adapter_fallback_plans_claude_before_checking_account_cooldown(monkeypatch):
    plan = {"adapter": "codex", "model": "gpt-6.1-sol"}
    rows = [{"status": "usage_limited"}, {"status": "ok", "retryable": False}]
    alternatives = [{"adapter": "claude", "model": "sonnet"}]
    mod, args, launches = _sequence_fixture(monkeypatch, plan, rows, alternatives)
    monkeypatch.setattr(mod.exec_routing, "cooling",
                        lambda adapter, *a, **k: {"cooling_until": timestamp(timedelta(days=365))} if adapter == "claude" else None)
    mod.execute_attempt_sequence(args)
    assert launches == [(None, None, None), ("claude", "sonnet", None)]


def test_claude_fallback_does_not_override_selected_account_cooldown(tmp_path, monkeypatch):
    launched = tmp_path / 'launches'
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch,
        'import sys,json\nfrom pathlib import Path\nsys.stdin.read()\n'
        f'with Path({str(launched)!r}).open("a") as stream: stream.write("launch\\n")\n'
        'print(json.dumps({"type":"result","is_error":True,"result":"You have hit your weekly limit"}))\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    instance = tmp_path / 'instance'
    config = instance / 'config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {'pro': {'identity_dir': str(tmp_path / 'identity')}}}}))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(instance))
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:] + ['--account', 'pro'])
    args.fallback = json.dumps(['claude/sonnet'])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    mod.dispatch(args)
    attempts = [json.loads(path.read_text()) for path in sorted(run.glob('tasks/*/attempt-*/attempt.json'))]
    assert len(attempts) == 2
    assert attempts[1]['evidence']['signature'] == 'cooldown_active'
    assert launched.read_text().splitlines() == ['launch']
    assert {row['account'] for row in attempts} == {'pro'}


@pytest.mark.parametrize(('owner_kind', 'locked', 'inherited'), [
    ('dispatch', True, 'custody'), ('batch', False, 'none'), ('batch', True, 'custody'),
    ('batch', True, 'none'), ('batch', True, 'separate'), ('batch', True, 'other_file'),
    ('batch', False, 'separate'),
])
def test_batch_child_shares_only_a_locked_batch_parent(tmp_path, monkeypatch, owner_kind, locked, inherited):
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'batch-parent-custody')
    (run / 'dispatch-owner.json').write_text(json.dumps({
        'schema_version': 1, 'kind': owner_kind, 'run_dir': str(run),
        'run_token': 'parent-token', 'owner_pid': os.getpid(),
        'owner_started_at': mod.process_info.start_time(os.getpid()),
    }))
    before = (run / 'dispatch-owner.json').read_bytes()
    custody = mod.acquire_run_custody(run) if locked else None
    fd = (custody.fileno() if custody is not None and inherited == 'custody' else
          os.open(run / ('RUN_RECEIPT.json' if inherited == 'other_file' else 'MANIFEST.md'), os.O_RDWR)
          if inherited in {'separate', 'other_file'} else None)
    environment = dict(os.environ)
    environment.pop(mod.run_lineage.CUSTODY_ENV, None)
    if fd is not None:
        environment[mod.run_lineage.CUSTODY_ENV] = str(fd)
    # A reparented provider orphan has exactly this parent PID relationship.
    code = '''import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import run_lineage
try:
    owned = run_lineage.ensure_nested_owner(Path(sys.argv[2]), 'dispatch', batch_child=True)
except (OSError, ValueError):
    print('refused')
else:
    print('owned' if owned else 'shared')
'''
    try:
        result = subprocess.run([sys.executable, '-c', code, str(SCRIPT.parent), str(run)],
                                env=environment, pass_fds=() if fd is None else (fd,),
                                text=True, capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr
        shared = owner_kind == 'batch' and locked and inherited == 'custody'
        assert result.stdout.strip() == ('shared' if shared else 'refused')
        assert (run / 'dispatch-owner.json').read_bytes() == before
        if custody is not None:
            with pytest.raises(OSError):
                mod.acquire_run_custody(run)
    finally:
        if fd is not None and inherited in {'separate', 'other_file'}:
            os.close(fd)
        if custody is not None:
            custody.close()
    # Refusing an unlocked owner must release the probe's acquired lock.
    with mod.acquire_run_custody(run):
        pass


def test_batch_child_refuses_unrelated_custody_after_recorded_batch_dies(tmp_path):
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'dead-batch-unrelated-custody')
    child = tmp_path / 'child.py'
    child.write_text(f'''import os, signal, sys, time
from pathlib import Path
sys.path.insert(0, {str(SCRIPT.parent)!r})
import dispatch_run as dispatch
owner_pid = os.getppid()
start_time = dispatch.process_info.start_time
observations = 0
def observe(pid):
    global observations
    value = start_time(pid)
    if pid == owner_pid:
        observations += 1
        if observations == 2:
            # The batch dies just after its last successful identity observation.
            Path({str(tmp_path / 'killed')!r}).write_text('killed')
            os.kill(owner_pid, signal.SIGTERM)
            deadline = time.monotonic() + 5
            while os.getppid() == owner_pid and time.monotonic() < deadline:
                time.sleep(0.01)
            assert os.getppid() != owner_pid
    return value
dispatch.process_info.start_time = observe
raise SystemExit(dispatch.dispatch(dispatch.parser().parse_args([
    '--batch-child', '--run-dir', {str(run)!r}, '--adapter', 'codex', '--model', 'fixture',
    '--prompt-file', {str(tmp_path / 'missing.md')!r}])))
''')
    batch = f'''import os, sys, subprocess
from pathlib import Path
sys.path.insert(0, {str(SCRIPT.parent)!r})
import run_lineage
run_lineage.record_owner(Path({str(run)!r}), 'batch')
subprocess.run([sys.executable, {str(child)!r}], check=False)
'''
    environment = {**os.environ, 'PROVENANT_HOST_LOCAL_ONLY': '1'}
    # This test process holds custody; the recorded batch never holds it.
    with mod.acquire_run_custody(run):
        result = subprocess.run([sys.executable, '-c', batch], cwd=tmp_path,
                                env=environment, input='', capture_output=True, text=True, timeout=15)
        assert result.returncode == -signal.SIGTERM, result.stderr
        assert (tmp_path / 'killed').exists()
        owner = json.loads((run / 'dispatch-owner.json').read_text())
        assert mod.process_info.start_time(owner['owner_pid']) is None
        assert result.stdout, result.stderr
        assert json.loads(result.stdout)['status'] == 'run_custody_busy'
        assert not (run / 'tasks').exists()


# A recycled pid and a vanished pid; ids stay stable across xdist workers.
@pytest.mark.parametrize('previous', ['recycled_pid', 'missing_pid'])
def test_standalone_batch_child_replaces_a_dead_owner(tmp_path, monkeypatch, previous):
    previous_pid = os.getpid() if previous == 'recycled_pid' else 2147483647
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'dead-batch-child-owner')
    (run / 'dispatch-owner.json').write_text(json.dumps({
        'schema_version': 1, 'kind': 'batch', 'run_dir': str(run),
        'run_token': 'dead-token', 'owner_pid': previous_pid,
        'owner_started_at': 'previous-process-start',
    }))
    assert mod.run_lineage.ensure_nested_owner(run, 'dispatch', batch_child=True) is True
    owner = json.loads((run / 'dispatch-owner.json').read_text())
    assert owner['owner_pid'] == os.getpid()
    assert owner['owner_started_at'] == mod.process_info.start_time(os.getpid())
    assert owner['kind'] == 'dispatch'
    assert owner['run_token'] != 'dead-token'
    with mod.acquire_run_custody(run):
        pass


@pytest.mark.parametrize('self_hosted', [False, True])
def test_record_owner_keeps_the_launcher_host_the_reaper_reads(tmp_path, monkeypatch, self_hosted):
    # Fabric writes the hosted record before the owner starts; dropping its host
    # fields made the next dispatch's orphan reaper stop this live run.
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'fabric-hosted-owner')
    monkeypatch.delenv('PROVENANT_RUN_OWNER', raising=False)
    monkeypatch.delenv('PROVENANT_PARENT_ATTEMPT', raising=False)
    monkeypatch.setenv('PROVENANT_RUN_TOKEN', 'fabric-token')
    monkeypatch.setenv('PROVENANT_RUN_DIR', str(run))
    launcher = os.getpid() if self_hosted else os.getppid()
    (run / 'dispatch-owner.json').write_text(json.dumps({
        'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(run), 'run_token': 'fabric-token',
        'owner_pid': os.getpid() if self_hosted else 999999, 'owner_pgid': 999999,
        'owner_started_at': 'launcher-view', 'host_pid': launcher, 'host_started_at': 'host-start',
        'owner_stdout': 'out.log', 'owner_stderr': 'err.log', 'task_id': 'lane'}))
    with mod.acquire_run_custody(run):
        mod.run_lineage.record_owner(run)
    owner = json.loads((run / 'dispatch-owner.json').read_text())
    assert owner['owner_pid'] == os.getpid()
    assert owner['run_token'] == 'fabric-token'
    assert (owner['owner_stdout'], owner['owner_stderr'], owner['task_id']) == ('out.log', 'err.log', 'lane')
    if self_hosted:
        assert (owner['host_pid'], owner['host_started_at']) == (os.getpid(), owner['owner_started_at'])
    else:
        assert (owner['host_pid'], owner['host_started_at']) == (launcher, 'host-start')


def test_record_owner_without_a_launcher_hosts_itself(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'direct-owner')
    for key in ('PROVENANT_RUN_OWNER', 'PROVENANT_PARENT_ATTEMPT', 'PROVENANT_RUN_TOKEN', 'PROVENANT_RUN_DIR'):
        monkeypatch.delenv(key, raising=False)
    with mod.acquire_run_custody(run):
        mod.run_lineage.record_owner(run)
    owner = json.loads((run / 'dispatch-owner.json').read_text())
    assert (owner['host_pid'], owner['host_started_at']) == (owner['owner_pid'], owner['owner_started_at'])


@pytest.mark.parametrize('spelling', ['other', 'same', 'alias'])
@pytest.mark.parametrize('published_owner', [False, True])
def test_record_owner_inherits_token_only_for_the_same_run(tmp_path, monkeypatch, spelling, published_owner):
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'owner-token-scope')
    # A symlinked spelling, like /var for /private/var, names the same run.
    (tmp_path / 'alias').symlink_to(run)
    same_run = spelling != 'other'
    monkeypatch.delenv('PROVENANT_RUN_OWNER', raising=False)
    monkeypatch.delenv('PROVENANT_PARENT_ATTEMPT', raising=False)
    monkeypatch.setenv('PROVENANT_RUN_TOKEN', 'inherited-token')
    monkeypatch.setenv('PROVENANT_RUN_DIR', str({'other': tmp_path / 'other-run', 'same': run, 'alias': tmp_path / 'alias'}[spelling]))
    if published_owner:
        monkeypatch.setenv('PROVENANT_RUN_OWNER', json.dumps({
            'owner_pid': os.getpid(), 'run_dir': str(run), 'run_token': 'inherited-token'}))
    with mod.acquire_run_custody(run):
        mod.run_lineage.record_owner(run)
    token = json.loads((run / 'dispatch-owner.json').read_text())['run_token']
    assert bool(token)
    assert (token == 'inherited-token') is same_run


@pytest.mark.parametrize('change', ['none', 'file', 'commit', 'dirty', 'read_only'])
def test_finished_writer_records_whether_worktree_is_unchanged(tmp_path, monkeypatch, change):
    code = 'import json,sys\nfrom pathlib import Path\nsys.stdin.read()\n'
    if change == 'file':
        code += 'Path("new.txt").write_text("done")\n'
    elif change == 'commit':
        code += ('import subprocess\nsubprocess.run(["git", "-c", "user.name=Test", "-c", '
                 '"user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "work"], check=True)\n')
    code += 'print(json.dumps({"type":"result","result":"DONE"}))\n'
    run, _prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    worktree = make_worktree(tmp_path)
    if change == 'dirty':
        (worktree / 'preexisting.txt').write_text('already here')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    extra = [] if change == 'read_only' else ['--access-mode', 'worktree_write', '--worktree', str(worktree)]
    args = mod.parser().parse_args(command[2:] + extra)
    assert mod.dispatch(args) == 0
    row = json.loads(next(run.glob('tasks/*/attempt-*/attempt.json')).read_text())
    assert row['status'] == 'ok'
    if change == 'read_only':
        assert 'empty_patch' not in row
    else:
        assert row.get('empty_patch') is {'none': True, 'dirty': None}.get(change, False)


def test_worktree_unchanged_is_unknown_when_git_cannot_say(tmp_path):
    mod = load_dispatch_module()
    assert mod.writer_lease.worktree_unchanged(None, str(tmp_path)) is None
    assert mod.writer_lease.worktree_unchanged(('old-head', b''), str(tmp_path)) is None


def test_dispatch_refuses_escalation_without_retry(tmp_path, monkeypatch, capsys):
    run, _prompt, command = real_owner_fixture(
        tmp_path, monkeypatch, 'raise SystemExit("provider must not launch")\n')
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:] + ['--escalation-reason', 'capability'])
    assert mod.dispatch(args) == 2
    assert json.loads(capsys.readouterr().out)['error'] == 'escalation_reason_invalid'
    assert not list(run.glob('tasks/*/attempt-*/attempt.json'))


def test_automatic_retry_does_not_inherit_explicit_escalation(tmp_path, monkeypatch):
    launches = tmp_path / 'launches'
    code = f'''import json,sys
from pathlib import Path
sys.stdin.read()
path = Path({str(launches)!r})
count = int(path.read_text()) + 1 if path.exists() else 1
path.write_text(str(count))
if count == 2:
    print(json.dumps({{"type":"result", "is_error":True, "result":"rate limit", "retry_after":0.05}}))
    sys.exit(1)
print(json.dumps({{"type":"result", "result":"DONE"}}))
'''
    _unused_run, _prompt, command = real_owner_fixture(tmp_path, monkeypatch, code)
    run = make_run(tmp_path, 'escalation')
    command[3] = str(run)
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.chdir(tmp_path)
    configure_fast_pacing(tmp_path, monkeypatch)
    mod = load_dispatch_module()
    assert mod.dispatch(mod.parser().parse_args(command[2:])) == 0
    args = mod.parser().parse_args(command[2:] + [
        '--retry-of', 'attempt-001', '--escalation-reason', 'capability'])
    args.fallback = 'true'
    args.timeout_seconds = 10
    assert mod.dispatch(args) == 0
    paths = sorted(run.glob('tasks/*/attempt-*/attempt.json'))
    first, explicit, automatic = (json.loads(path.read_text()) for path in paths)
    assert [first['status'], explicit['status'], automatic['status']] == ['ok', 'rate_limited', 'ok']
    assert explicit['retry_of'] == 'attempt-001'
    assert explicit['escalation_reason'] == 'capability'
    assert automatic['retry_of'] == 'attempt-002'
    assert automatic['escalation_reason'] is None
    assert automatic['provenance']['fallback_from']['status'] == 'rate_limited'


@pytest.mark.parametrize('checkpoint_error', [None, PermissionError('checkpoint denied'), ValueError('unsafe checkpoint')])
def test_provider_death_checkpoints_partial_work_before_failure(tmp_path, monkeypatch, checkpoint_error):
    subprocess.run(['git', 'init', '-q', str(tmp_path)], check=True)
    subprocess.run(['git', '-C', str(tmp_path), '-c', 'user.name=Fixture',
                    '-c', 'user.email=fixture@example.com', 'commit', '--allow-empty', '-qm', 'initial'], check=True)
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json\nfrom pathlib import Path\nsys.stdin.read()\n'
        'Path("partial.txt").write_text("unfinished change")\n'
        'print(json.dumps({"type":"assistant","message":{"content":'
        '[{"type":"tool_use","name":"Bash","input":{"command":"write partial.txt"}}]}}), flush=True)\n'
        'sys.exit(1)\n',
    )
    isolate_fabric_plan_env(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    if checkpoint_error:
        def unavailable(*args, **kwargs):
            raise checkpoint_error
        monkeypatch.setattr(mod.stall_watchdog, 'checkpoint', unavailable)
    args = mod.parser().parse_args(command[2:])
    assert mod.dispatch(args) != 0
    path = next(run.glob('tasks/*/attempt-001/attempt.json'))
    row = json.loads(path.read_text())
    assert row['state'] == 'terminal'
    assert row['status'] == 'failed'
    if checkpoint_error:
        assert 'checkpoint' not in row['paths']
        assert any('checkpoint unavailable' in warning for warning in row['warnings'])
        return
    checkpoint_path = run / json.loads(path.read_text())['paths']['checkpoint']
    checkpoint = json.loads(checkpoint_path.read_text())
    assert checkpoint['process_alive'] is False
    assert 'partial.txt' in checkpoint['diff_summary']
    assert 'write partial.txt' in checkpoint['last_tool_call']
    assert checkpoint_path.stat().st_mtime_ns <= path.stat().st_mtime_ns
    assert json.loads(path.read_text())['status'] == 'failed'


@pytest.mark.parametrize('checkpoint_error', [PermissionError('checkpoint denied'), OSError(28, 'disk full'), ValueError('unsafe checkpoint')])
@pytest.mark.parametrize('disk_full', [False, True])
def test_reaper_checkpoint_failure_still_publishes_terminal(tmp_path, monkeypatch, checkpoint_error, disk_full):
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'checkpoint-error')
    path = run / 'tasks/task-1/attempt-001/attempt.json'
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
    row.update(state='running', status=None, pgid=123456, pgid_started_at='recorded')
    path.write_text(json.dumps(row))
    (path.parent / 'stderr.log').write_text('ENOSPC' if disk_full else '')
    def gone(*args):
        raise ProcessLookupError()
    def unavailable(*args, **kwargs):
        raise checkpoint_error
    monkeypatch.setattr(mod.os, 'killpg', gone)
    monkeypatch.setattr(mod.stall_watchdog, 'checkpoint', unavailable)
    assert mod.reap_attempts(run) == [str(path)]
    terminal = json.loads(path.read_text())
    assert terminal['state'] == 'terminal'
    assert terminal['status'] == ('failed' if disk_full else 'lost')
    assert terminal['evidence']['signature'] == ('host_disk_full' if disk_full else 'pgid_gone')
    assert 'checkpoint' not in terminal['paths']
    assert any('checkpoint unavailable' in warning for warning in terminal['warnings'])


@pytest.mark.parametrize("threshold,observed", [(1, True), (3600, False)])
def test_reaper_observes_silent_live_lane_with_stamped_knob(tmp_path, monkeypatch, threshold, observed):
    mod = load_dispatch_module()
    run = make_run(tmp_path, "silent")
    path = run / "tasks/task-1/attempt-001/attempt.json"
    path.parent.mkdir(parents=True)
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row.update(state="running", status=None, pgid=123456, pgid_started_at="recorded",
               cwd=str(tmp_path), profile={"knobs": {"lanes.silence_seconds": threshold}})
    path.write_text(json.dumps(row))
    events = run / row['paths']['events']
    events.write_text('{"type":"item.started"}\n')
    os.utime(events, (time.time() - 10, time.time() - 10))
    probes = []
    monkeypatch.setattr(mod.os, "killpg", lambda pid, sig: probes.append((pid, sig)))
    monkeypatch.setattr(mod.process_info, "start_time", lambda pid: "recorded")
    before = path.read_bytes()
    assert mod.reap_attempts(run) == []
    assert path.read_bytes() == before
    assert probes == [(123456, 0)]
    observation_path = path.parent / "watchdog.json"
    assert observation_path.exists() is observed
    if observed:
        record = json.loads(observation_path.read_text())
        assert record['process_alive'] is True
        assert record['last_event_at']
        assert record['last_worktree_write_at']
        assert record['silent_seconds'] >= 10
    assert not (path.parent / 'wip-checkpoint.json').exists()


@pytest.mark.parametrize('mode', ['read_only', 'worktree_write'])
def test_reaper_git_summaries_share_one_pass_budget(tmp_path, monkeypatch, mode):
    mod = load_dispatch_module()
    run = make_run(tmp_path, 'git-budget')
    for task in ('one', 'two', 'three'):
        path = run / f'tasks/{task}/attempt-001/attempt.json'
        path.parent.mkdir(parents=True)
        row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
        row.update(task_id=task, state='running', status=None, mode=mode,
                   cwd=str(tmp_path), worktree=str(tmp_path) if mode == 'worktree_write' else None,
                   pgid=123456, pgid_started_at='recorded')
        row['paths'] = {'events': str(path.parent.relative_to(run) / 'events.jsonl')}
        path.write_text(json.dumps(row))
    clock = [100.0]
    timeouts = []
    def gone(*args):
        raise ProcessLookupError()
    def slow_git(argv, **kwargs):
        timeouts.append(kwargs['timeout'])
        clock[0] += kwargs['timeout']
        raise subprocess.TimeoutExpired(argv, kwargs['timeout'])
    monkeypatch.setattr(mod.os, 'killpg', gone)
    monkeypatch.setattr(mod.time, 'monotonic', lambda: clock[0])
    monkeypatch.setattr(mod.stall_watchdog.subprocess, 'run', slow_git)
    assert len(mod.reap_attempts(run)) == 3
    if mode == 'read_only':
        assert timeouts == []
    else:
        assert len(timeouts) == 1
        assert sum(timeouts) <= 1
    for path in run.glob('tasks/*/attempt-001/attempt.json'):
        row = json.loads(path.read_text())
        assert row['state'] == 'terminal'
        checkpoint = json.loads((run / row['paths']['checkpoint']).read_text())
        assert ('read_only' if mode == 'read_only' else 'unavailable') in checkpoint['diff_summary']


@pytest.mark.parametrize("check", ["passed", "invalid", "unavailable"])
def test_terminal_receipt_persists_model_check_and_changes(tmp_path, check):
    module = load_dispatch_module()
    run = make_run(tmp_path, "run")
    args = dispatch_args(tool="agy", alias="", effort="")
    status = "model_unavailable" if check == "invalid" else "ok"
    legacy = {"started_at": timestamp(), "finished_at": timestamp(),
              "outcome": status, "status": status, "result": None,
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    fields = {"model_check": check, "model_check_detail": "listing evidence",
              "model_changes": [{"from": "fixture", "to": "other"},
                                {"from": "other", "to": "fixture"}]}
    row = module.terminal_contract(args, run, legacy, {"status": status, **fields}, 1,
                                   run / "tasks/task-1/attempt-001")
    module.publish_contract(run, row)
    saved = json.loads((run / row["paths"]["receipt"]).read_text())
    assert saved['paths']['watchdog'] == 'tasks/task-1/attempt-001/watchdog.json'
    for key, value in fields.items():
        assert saved[key] == value
    if check == "invalid":
        assert saved["model_check"] == "invalid"
        assert saved["model_check_detail"] == "listing evidence"


def _packet_metadata():
    return {
        "schema_version": 1, "record_type": "provenant-git-evidence",
        "repository": "/fixture", "git_root": "/fixture", "head": "a" * 40,
        "diff_base": "b" * 40, "working_tree": "clean", "diff_from": "HEAD",
        "paths": [], "encoding": "utf-8-replacement",
    }


_PACKET_HEADER_MUTATIONS = [
    ("schema_version", 2), ("record_type", "other"), ("repository", None),
    ("git_root", None), ("head", None), ("head", "A" * 40),
    ("head", "a" * 39), ("head", "a" * 65), ("diff_base", None),
    ("diff_base", "G" * 40), ("diff_base", "b" * 39), ("diff_base", "b" * 65),
    ("working_tree", "unknown"), ("working_tree", []), ("diff_from", None), ("diff_from", ""),
    ("paths", None), ("paths", [1]), ("encoding", "utf-8"),
]


def _packet_bytes(case):
    metadata = _packet_metadata()
    body = b"--- status ---\n--- diff ---\n"
    if case == "empty": return b""
    if case == "no-newline": return json.dumps(metadata).encode()
    if case == "oversize-no-newline": return b"x" * (65536 + 1)
    if case == "oversize-newline": return b"x" * (65536 + 1) + b"\n" + body
    if case == "bad-json": return b"{\n" + body
    if case == "bad-utf8": return b"\xff\n" + body
    if case == "null": return b"null\n" + body
    if case == "non-object": return b"[]\n" + body
    if case == "missing-key": del metadata["head"]
    if case == "extra-key": metadata["extra"] = True
    if case == "wrong-status": body = b"--- wrong ----\n--- diff ---\n"
    if case == "short-status": body = b"--- status"
    if case == "no-body": body = b""
    if case == "missing-diff": body = b"--- status ---\nno diff\n"
    if case == "late-diff": body = b"--- status ---\n" + b"x" * 100 + b"\n--- diff ---\n" + b"x" * 100
    if case == "header-at-limit":
        header = json.dumps(metadata).encode()
        return header + b" " * (65536 - len(header)) + b"\n" + body
    return json.dumps(metadata).encode() + b"\n" + body


@pytest.mark.parametrize("field,value", _PACKET_HEADER_MUTATIONS)
def test_git_evidence_copy_rejects_each_header_field(tmp_path, field, value):
    module = load_dispatch_module()
    metadata = _packet_metadata()
    metadata[field] = value
    source, destination = tmp_path / "source", tmp_path / "destination"
    source.write_bytes(json.dumps(metadata).encode() + b"\n--- status ---\n--- diff ---\n")
    with pytest.raises(module.run_records.AttemptEvidenceError) as error:
        module._copy_git_evidence(tmp_path, tmp_path, source, destination)
    assert str(error.value) == "Git evidence packet header is invalid"
    assert not destination.exists()


@pytest.mark.parametrize("case,message", [
    ("empty", "has no JSON header"), ("no-newline", "has no JSON header"),
    ("oversize-no-newline", "header exceeds its 64 KiB limit"),
    ("oversize-newline", "header exceeds its 64 KiB limit"),
    ("bad-json", "has an invalid JSON header"), ("bad-utf8", "has an invalid JSON header"),
    ("null", "header is invalid"), ("non-object", "header is invalid"),
    ("missing-key", "header is invalid"), ("extra-key", "header is invalid"),
    ("wrong-status", "is missing its status section"),
    ("short-status", "is missing its status section"), ("no-body", "is missing its status section"),
    ("missing-diff", "is missing its diff section"),
])
def test_git_evidence_copy_rejection_messages_and_cleanup(tmp_path, case, message):
    module = load_dispatch_module()
    source, destination = tmp_path / "source", tmp_path / "destination"
    source.write_bytes(_packet_bytes(case))
    with pytest.raises(module.run_records.AttemptEvidenceError) as error:
        module._copy_git_evidence(tmp_path, tmp_path, source, destination)
    assert str(error.value) == "Git evidence packet " + message
    assert not destination.exists()


@pytest.mark.parametrize("case,chunk_size", [("late-diff", 1), ("late-diff", 7), ("late-diff", 64), ("header-at-limit", 1048576)])
def test_git_evidence_copy_stream_boundaries(tmp_path, monkeypatch, case, chunk_size):
    module = load_dispatch_module()
    source, destination = tmp_path / "source", tmp_path / "destination"
    payload = _packet_bytes(case)
    source.write_bytes(payload)
    read = module.os.read
    monkeypatch.setattr(module.os, "read", lambda fd, size: read(fd, min(size, chunk_size)))
    metadata, digest = module._copy_git_evidence(tmp_path, tmp_path, source, destination)
    assert metadata == _packet_metadata()
    assert digest == "sha256:" + hashlib.sha256(payload).hexdigest()
    assert destination.read_bytes() == payload


@pytest.mark.parametrize("outside", ["source", "destination"])
def test_git_evidence_copy_rejects_paths_outside_run(tmp_path, outside):
    module = load_dispatch_module()
    root = tmp_path / "run"
    root.mkdir()
    source = (tmp_path if outside == "source" else root) / "source"
    destination = (tmp_path if outside == "destination" else root) / "destination"
    source.write_bytes(_packet_bytes("late-diff"))
    with pytest.raises(module.run_records.AttemptEvidenceError) as error:
        module._copy_git_evidence(tmp_path, root, source, destination)
    assert str(error.value) == "Git evidence must be materialised inside the run directory"
    assert not destination.exists()


@pytest.mark.parametrize("target", ["source", "destination"])
def test_git_evidence_copy_containment_errors_preserve_message(tmp_path, monkeypatch, target):
    module = load_dispatch_module()
    source, destination = tmp_path / "source", tmp_path / "destination"
    source.write_bytes(_packet_bytes("late-diff"))
    open_regular = module.open_contained_regular
    def refuse(root, relative, flags, **kwargs):
        if relative == Path(target):
            raise module.OwnedLinkError("fixture containment rejection")
        return open_regular(root, relative, flags, **kwargs)
    monkeypatch.setattr(module, "open_contained_regular", refuse)
    with pytest.raises(module.run_records.AttemptEvidenceError) as error:
        module._copy_git_evidence(tmp_path, tmp_path, source, destination)
    assert str(error.value) == "fixture containment rejection"
    assert not destination.exists()


def test_git_evidence_copy_invalid_metadata_precedes_status(tmp_path):
    module = load_dispatch_module()
    source, destination = tmp_path / "source", tmp_path / "destination"
    source.write_bytes(b"[]\n--- wrong ----\n")
    with pytest.raises(module.run_records.AttemptEvidenceError) as error:
        module._copy_git_evidence(tmp_path, tmp_path, source, destination)
    assert str(error.value) == "Git evidence packet header is invalid"
