import json
import hashlib
import base64
import copy
import importlib.util
import os
from datetime import datetime, timedelta, timezone
from pathlib import Path
import subprocess
import sys
import signal
import time

import pytest


ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"
VALIDATOR = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"


def run_producer(workspace: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(PRODUCER), *args],
        cwd=workspace,
        text=True,
        capture_output=True,
        check=False,
    )


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def assessment() -> dict[str, str]:
    return {
        "blast_radius": "local",
        "reversibility": "easy",
        "data_sensitivity": "public",
        "migration": "none",
        "oracle_quality": "strong",
        "external_effects": "none",
        "critical_surface": "none",
    }


def authority() -> dict[str, object]:
    return {
        "schema_version": 2,
        "approved_by": "human-owner",
        "evidence": "authority-approval",
        "evidence_digest": "sha256:" + "a" * 64,
        "workspace_roots": ["."],
        "expires_at": "2099-01-01T00:00:00Z",
        "allowed_source_paths": ["."],
        "allowed_artifact_paths": ["."],
        "allowed_fabric_operations": [],
        "denied_paths": [],
        "denied_fabric_operations": [],
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


def init_run(tmp_path: Path, run_id: str = "DEL-TEST") -> Path:
    (tmp_path / "intent.md").write_text("# Intent\n")
    result = run_producer(
        tmp_path, "init", "--run-dir", f".agent-run/{run_id}", "--run-id", run_id,
        "--profile", "software", "--chair-family", "openai",
        "--risk-assessment", json.dumps(assessment()), "--intent", "intent.md",
        "--authority", json.dumps(authority()),
    )
    assert result.returncode == 0, result.stderr
    return tmp_path / ".agent-run" / run_id


def add_bundle(tmp_path: Path, run_dir: Path, artifact_id: str = "evidence-bundle") -> None:
    bundle = run_dir / "evidence.json"
    bundle.write_text('{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n')
    result = run_producer(
        tmp_path, "artifact", "add", "--run-dir", str(run_dir), "--id", artifact_id,
        "--path", f".agent-run/{run_dir.name}/evidence.json", "--class", "evidence",
        "--media-type", "application/json", "--artifact-type", "evidence",
        "--owner", "delivery-chair", "--retention", "risk-policy",
    )
    assert result.returncode == 0, result.stderr


def test_init_creates_a_canonical_draft_receipt(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")
    assessment = {
        "blast_radius": "local",
        "reversibility": "easy",
        "data_sensitivity": "public",
        "migration": "none",
        "oracle_quality": "strong",
        "external_effects": "none",
        "critical_surface": "none",
    }
    authority = {
        "schema_version": 2,
        "approved_by": "human-owner",
        "evidence": "authority-approval",
        "evidence_digest": "sha256:" + "a" * 64,
        "workspace_roots": ["."],
        "expires_at": "2099-01-01T00:00:00Z",
        "allowed_source_paths": ["."],
        "allowed_artifact_paths": ["."],
        "allowed_fabric_operations": [],
        "denied_paths": [],
        "denied_fabric_operations": [],
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

    result = run_producer(
        tmp_path,
        "init",
        "--run-dir",
        ".agent-run/DEL-TEST",
        "--run-id",
        "DEL-TEST",
        "--profile",
        "software",
        "--chair-family",
        "openai",
        "--risk-assessment",
        json.dumps(assessment),
        "--intent",
        "intent.md",
        "--authority",
        json.dumps(authority),
    )

    assert result.returncode == 0, result.stderr
    receipt = json.loads((tmp_path / ".agent-run/DEL-TEST/RUN.json").read_text())
    assert receipt["contract"] == "delivery-run"
    assert receipt["status"] == "draft"
    assert receipt["risk_tier"] == "routine"
    assert receipt["state_history"][0]["state"] == "draft"
    assert receipt["state_history"][0]["at"].endswith("Z")
    assert (tmp_path / ".agent-run/DEL-TEST/.RUN.lock").exists()


def test_init_accepts_the_known_run_directory_scaffold(tmp_path):
    run_dir = tmp_path / ".agent-run" / "DEL-SCAFFOLD"
    (run_dir / "findings").mkdir(parents=True)
    (run_dir / "MANIFEST.md").write_text("# Manifest\n")
    (run_dir / "intent.md").write_text("# Intent\n")
    result = run_producer(
        tmp_path, "init", "--run-dir", ".agent-run/DEL-SCAFFOLD", "--run-id", "DEL-SCAFFOLD",
        "--profile", "software", "--chair-family", "openai",
        "--risk-assessment", json.dumps(assessment()),
        "--intent", ".agent-run/DEL-SCAFFOLD/intent.md",
        "--authority", json.dumps(authority()),
    )
    assert result.returncode == 0, result.stderr


def test_artifact_add_binds_live_digest_and_refuses_escape(tmp_path):
    run_dir = init_run(tmp_path)
    output = tmp_path / "output.txt"
    output.write_text("before\n")
    result = run_producer(
        tmp_path, "artifact", "add", "--run-dir", str(run_dir), "--id", "output",
        "--path", "output.txt", "--class", "canonical", "--media-type", "text/plain",
        "--artifact-type", "documentation", "--owner", "chair", "--retention", "project-policy",
    )
    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    artifact = next(row for row in receipt["artifacts"] if row["id"] == "output")
    assert artifact["digest"] == "sha256:" + hashlib.sha256(output.read_bytes()).hexdigest()

    escaping = run_producer(
        tmp_path, "artifact", "add", "--run-dir", str(run_dir), "--id", "escape",
        "--path", "../outside", "--class", "canonical", "--media-type", "text/plain",
        "--artifact-type", "documentation", "--owner", "chair", "--retention", "project-policy",
    )
    assert escaping.returncode == 1
    assert "safe and workspace-relative" in escaping.stderr


def test_evidence_run_records_observed_success_failure_and_signal(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    success = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests-pass",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "print('green')",
    )
    assert success.returncode == 0, success.stderr

    failed = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests-fail",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "import sys; print('red'); sys.exit(7)",
    )
    assert failed.returncode == 0, failed.stderr

    signalled = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests-signal",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "import os, signal; os.kill(os.getpid(), signal.SIGTERM)",
    )
    assert signalled.returncode == 0, signalled.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    by_id = {row["id"]: row for row in receipt["evidence"]}
    assert by_id["tests-pass"]["result"]["exit_code"] == 0
    assert by_id["tests-pass"]["stdout"] == "green\n"
    assert by_id["tests-fail"]["result"]["exit_code"] == 7
    assert by_id["tests-fail"]["status"] == "fail"
    assert by_id["tests-signal"]["result"]["exit_code"] == -signal.SIGTERM
    artifact = next(row for row in receipt["artifacts"] if row["id"] == "evidence-bundle")
    assert artifact["digest"] == "sha256:" + hashlib.sha256(
        (tmp_path / artifact["path"]).read_bytes()
    ).hexdigest()


def test_evidence_run_does_not_accept_a_supplied_exit_code(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md",
        "--exit-code", "0", "--", sys.executable, "-c", "pass",
    )
    assert result.returncode == 2
    assert "unrecognized arguments: --exit-code" in result.stderr


def test_evidence_bundle_publication_preserves_old_receipt_on_interruption(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    module = load_module(PRODUCER, "delivery_receipt_atomic_test")
    receipt_path = run_dir / "RUN.json"
    before = receipt_path.read_bytes()
    original_replace = module.os.replace

    def interrupted(source, destination):
        if Path(destination).resolve() == receipt_path.resolve():
            raise OSError("injected atomic interruption")
        return original_replace(source, destination)

    monkeypatch.setattr(module.os, "replace", interrupted)
    args = module.build_parser().parse_args([
        "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md",
        "--", sys.executable, "-c", "pass",
    ])
    with pytest.raises(OSError, match="injected atomic interruption"):
        module.command_evidence_run(args)
    assert receipt_path.read_bytes() == before
    assert json.loads(before)
    assert not list(run_dir.glob("evidence-bundle.*.json"))


def test_bundle_fsync_failure_does_not_leave_a_receipt_without_its_bundle(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    module = load_module(PRODUCER, "delivery_receipt_fsync_test")
    original_fsync = module.fsync_directory
    calls = 0

    def fail_receipt_directory(path):
        nonlocal calls
        calls += 1
        if calls == 2:
            raise OSError("injected receipt directory fsync failure")
        return original_fsync(path)

    monkeypatch.setattr(module, "fsync_directory", fail_receipt_directory)
    args = module.build_parser().parse_args([
        "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md",
        "--", sys.executable, "-c", "pass",
    ])
    with pytest.raises(OSError, match="receipt directory fsync"):
        module.command_evidence_run(args)
    receipt = json.loads((run_dir / "RUN.json").read_text())
    artifact = next(row for row in receipt["artifacts"] if row["id"] == "evidence-bundle")
    target = tmp_path / artifact["path"]
    assert target.is_file()
    assert artifact["digest"] == "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()


def test_producer_refuses_equal_clock_and_tier_downgrade(tmp_path, monkeypatch):
    run_dir = init_run(tmp_path)
    module = load_module(PRODUCER, "delivery_receipt_clock_test")
    run = module.load_run(run_dir, workspace_root=tmp_path)
    initial = run["state_history"][0]["at"]
    monkeypatch.setattr(module, "utc_now", lambda: initial)
    with pytest.raises(module.ReceiptError, match="strictly increase"):
        module.timestamp_after(run)

    receipt_path = run_dir / "RUN.json"
    altered = json.loads(receipt_path.read_text())
    altered["risk_tier"] = "substantial"
    receipt_path.write_text(json.dumps(altered, indent=2) + "\n")
    result = run_producer(
        tmp_path, "checkpoint", "set", "--run-dir", str(run_dir),
        "--current-slice", "test", "--next-action", "continue",
    )
    assert result.returncode == 1
    assert "risk tier is immutable" in result.stderr


def test_validator_rejects_timestamps_beyond_bounded_future_tolerance():
    validator = load_module(ROOT / "skills" / "deliver" / "scripts" / "delivery_validation_common.py", "delivery_validation_future_test")
    future = (datetime.now(timezone.utc) + timedelta(minutes=6)).isoformat().replace("+00:00", "Z")
    with pytest.raises(validator.Invalid, match="bounded future"):
        validator._utc(future, "state_history[0].at")


def test_transition_supports_contract_side_state_recovery(tmp_path):
    run_dir = init_run(tmp_path)
    blocked = run_producer(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "blocked",
        "--reason", "provider unavailable", "--recovery", "resume locally",
    )
    resumed = run_producer(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "draft",
    )
    assert blocked.returncode == 0, blocked.stderr
    assert resumed.returncode == 0, resumed.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert [row["state"] for row in receipt["state_history"]] == ["draft", "blocked", "draft"]
    assert receipt["state_history"][1]["resume_state"] == "draft"


def test_removing_deterministic_evidence_rebuilds_its_bundle_atomically(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    executed = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "pass",
    )
    assert executed.returncode == 0, executed.stderr
    removed = run_producer(
        tmp_path, "evidence", "remove", "--run-dir", str(run_dir), "--id", "tests",
    )
    assert removed.returncode == 0, removed.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["evidence"] == []
    artifact = next(row for row in receipt["artifacts"] if row["id"] == "evidence-bundle-v2")
    bundle = (tmp_path / artifact["path"]).read_bytes()
    assert json.loads(bundle)["checks"] == []
    assert artifact["digest"] == "sha256:" + hashlib.sha256(bundle).hexdigest()


def test_rebuilding_deterministic_evidence_allocates_an_immutable_bundle_version(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    executed = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "pass",
    )
    assert executed.returncode == 0, executed.stderr
    before = json.loads((run_dir / "RUN.json").read_text())
    original = next(item for item in before["artifacts"] if item["id"] == "evidence-bundle")
    original_bytes = (tmp_path / original["path"]).read_bytes()
    rebuilt = run_producer(tmp_path, "evidence", "rebuild", "--run-dir", str(run_dir), "--artifact-id", "evidence-bundle")
    assert rebuilt.returncode == 0, rebuilt.stderr
    after = json.loads((run_dir / "RUN.json").read_text())
    versioned = next(item for item in after["artifacts"] if item["id"] == "evidence-bundle-v2")
    assert original["path"] == next(item for item in after["artifacts"] if item["id"] == "evidence-bundle")["path"]
    assert (tmp_path / original["path"]).read_bytes() == original_bytes
    assert all(item.get("artifact_id") == "evidence-bundle-v2" for item in after["evidence"] if item.get("kind") == "deterministic")
    assert versioned["digest"] == "sha256:" + hashlib.sha256((tmp_path / versioned["path"]).read_bytes()).hexdigest()


def test_evidence_removal_scans_all_receipt_reference_sections():
    module = load_module(PRODUCER, "delivery_receipt_reference_test")
    run = {
        "state_history": [],
        "design": {"evidence": "design-approval"},
        "measures": {"outcome": [{"evidence_id": "measure-proof"}]},
        "assurance": {"evaluations": [{"evidence_id": "evaluation-proof"}]},
        "security": {"checks": [{"evidence_ids": ["security-proof"]}]},
    }
    assert module.lifecycle._evidence_references(run, "design-approval") == ["design"]
    assert module.lifecycle._evidence_references(run, "measure-proof") == ["measures"]
    assert module.lifecycle._evidence_references(run, "evaluation-proof") == ["assurance"]
    assert module.lifecycle._evidence_references(run, "security-proof") == ["security"]


def test_producer_rejects_foreign_and_symlinked_receipt_and_lock_paths(tmp_path):
    run_dir = init_run(tmp_path)
    foreign = tmp_path / "foreign" / "RUN.json"
    foreign.parent.mkdir()
    foreign.write_bytes((run_dir / "RUN.json").read_bytes())
    rejected_foreign = run_producer(
        tmp_path, "checkpoint", "set", "--run-dir", str(foreign),
        "--current-slice", "foreign", "--next-action", "reject",
    )
    assert rejected_foreign.returncode == 1
    assert "current workspace" in rejected_foreign.stderr

    linked_run = tmp_path / ".agent-run" / "LINKED"
    linked_run.symlink_to(run_dir, target_is_directory=True)
    rejected_run = run_producer(
        tmp_path, "checkpoint", "set", "--run-dir", str(linked_run),
        "--current-slice", "linked", "--next-action", "reject",
    )
    assert rejected_run.returncode == 1
    assert "symlink" in rejected_run.stderr

    linked_receipt = tmp_path / ".agent-run" / "DEL-LINKED"
    linked_receipt.mkdir()
    (linked_receipt / "RUN.json").symlink_to(run_dir / "RUN.json")
    rejected_receipt = run_producer(
        tmp_path, "checkpoint", "set", "--run-dir", str(linked_receipt),
        "--current-slice", "linked", "--next-action", "reject",
    )
    assert rejected_receipt.returncode == 1
    assert "symlink" in rejected_receipt.stderr

    lock = run_dir / ".RUN.lock"
    lock.unlink()
    lock.symlink_to(tmp_path / "foreign.lock")
    rejected_lock = run_producer(
        tmp_path, "checkpoint", "set", "--run-dir", str(run_dir),
        "--current-slice", "lock", "--next-action", "reject",
    )
    assert rejected_lock.returncode == 1
    assert "symlink" in rejected_lock.stderr


def test_concurrent_initialisers_have_one_create_if_absent_winner(tmp_path):
    (tmp_path / "intent.md").write_text("winner intent\n")
    commands = []
    for index in range(8):
        intent = tmp_path / f"intent-{index}.md"
        intent.write_text(f"intent-{index}\n")
        commands.append([
            sys.executable, str(PRODUCER), "init", "--run-dir", ".agent-run/DEL-RACE",
            "--run-id", "DEL-RACE", "--profile", "software", "--chair-family", "openai",
            "--risk-assessment", json.dumps(assessment()), "--intent", intent.name,
            "--authority", json.dumps(authority()),
        ])
    processes = [subprocess.Popen(command, cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE) for command in commands]
    results = [process.communicate(timeout=20) for process in processes]
    successes = sum(process.returncode == 0 for process in processes)
    assert successes == 1, results
    receipt = json.loads((tmp_path / ".agent-run/DEL-RACE/RUN.json").read_text())
    assert receipt["intent"]["artifact"] in {f"intent-{index}.md" for index in range(8)}


def test_evidence_records_complete_output_and_execution_identity(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    output = "x" * 70000
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "large-output",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", f"print({output!r}, end='')",
    )
    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    row = receipt["evidence"][0]
    observed = row["result"]
    assert observed["argv"] == [sys.executable, "-c", f"print({output!r}, end='')"]
    assert observed["run_identity"] == {
        "run_id": "DEL-TEST", "receipt": ".agent-run/DEL-TEST/RUN.json",
    }
    assert observed["stdout"]["bytes"] == 70000
    assert observed["stdout"]["retained_bytes"] == 65536
    assert observed["stdout"]["truncated"] is True
    assert observed["stdout"]["digest"].startswith("sha256:")
    assert observed["timed_out"] is False
    assert observed["signal"] is None
    assert observed["custody"]["status"] == "posix-process-group-cleanup"
    bundle = json.loads((tmp_path / next(item for item in receipt["artifacts"] if item["id"] == "evidence-bundle")["path"]).read_text())
    assert bundle["checks"][0]["result"] == {key: value for key, value in observed.items() if key != "receipt_digest"}


def test_evidence_bundle_round_trips_non_utf8_output_bytes(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "binary-output",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c",
        "import sys; sys.stdout.buffer.write(b'\\x00\\xffout'); sys.stderr.buffer.write(b'err\\x00\\xfe')",
    )
    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    row = receipt["evidence"][0]
    expected = {"stdout": b"\x00\xffout", "stderr": b"err\x00\xfe"}
    for stream, raw in expected.items():
        output = row["result"][stream]
        assert base64.b64decode(output["captured_b64"]) == raw
        assert base64.b64decode(output["retained_b64"]) == raw
    artifact = next(item for item in receipt["artifacts"] if item["id"] == "evidence-bundle")
    bundle = json.loads((tmp_path / artifact["path"]).read_text())
    assert bundle["checks"][0]["result"]["stdout"]["captured_b64"] == row["result"]["stdout"]["captured_b64"]


def test_git_backed_workspace_requires_committed_execution_provenance(tmp_path):
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "requires-git",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "pass",
    )
    assert result.returncode == 1
    assert "Git provenance is required" in result.stderr


def test_post_spawn_output_setup_failure_terminates_and_reaps_the_command(tmp_path, monkeypatch):
    module = load_module(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt_process.py",
        "delivery_receipt_post_spawn_setup_test",
    )
    completed = tmp_path / "completed"
    command = [
        sys.executable,
        "-c",
        (
            "import pathlib,time; time.sleep(.15); "
            f"pathlib.Path({str(completed)!r}).write_text('completed'); time.sleep(.5)"
        ),
    ]

    def fail_output_registration(*_args, **_kwargs):
        raise RuntimeError("injected output setup failure")

    monkeypatch.setattr(module.selectors.DefaultSelector, "register", fail_output_registration)
    with pytest.raises(ValueError, match="bounded process execution failed"):
        module.execute_bounded(
            command, cwd=tmp_path, timeout_seconds=1, max_log_bytes=128, error_type=ValueError,
        )
    time.sleep(.8)
    assert not completed.exists()


def test_process_group_runner_does_not_depend_on_ps_inventory(tmp_path, monkeypatch):
    module = load_module(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt_process.py",
        "delivery_receipt_process_group_no_ps_test",
    )

    def forbidden_process_table(*_args, **_kwargs):
        pytest.fail("process-group cleanup must not invoke ps")

    monkeypatch.setattr(module.subprocess, "check_output", forbidden_process_table)
    observed = module.execute_bounded(
        [sys.executable, "-c", "print('group')"],
        cwd=tmp_path,
        timeout_seconds=1,
        max_log_bytes=128,
        error_type=ValueError,
    )

    assert observed["exit_code"] == 0
    assert observed["retained_stdout"] == "group\n"
    assert observed["custody"]["status"] == "posix-process-group-cleanup"


def test_process_group_runner_kills_same_group_grandchild_that_ignores_term(tmp_path):
    module = load_module(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt_process.py",
        "delivery_receipt_process_group_escalation_test",
    )
    marker = tmp_path / "delayed-marker"
    child_code = (
        "import pathlib, signal, time; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        "time.sleep(0.5); "
        f"pathlib.Path({str(marker)!r}).write_text('late')"
    )
    command = [
        sys.executable,
        "-c",
        (
            "import subprocess, sys, time; "
            f"subprocess.Popen([sys.executable, '-c', {child_code!r}]); "
            "time.sleep(10)"
        ),
    ]
    started = time.monotonic()
    observed = module.execute_bounded(
        command,
        cwd=tmp_path,
        timeout_seconds=0.15,
        max_log_bytes=128,
        error_type=ValueError,
    )
    elapsed = time.monotonic() - started

    assert elapsed < 1.0
    assert isinstance(observed["exit_code"], int)
    assert observed["timed_out"] is True
    assert observed["custody"]["status"] == "posix-process-group-cleanup"
    assert observed["custody"]["cleanup"]["kill_sent"] is True
    time.sleep(0.7)
    assert not marker.exists()


@pytest.mark.parametrize(
    "mutation",
    [
        "from pathlib import Path; Path('intent.md').write_text('mutated\\n')",
        "import subprocess; subprocess.run(['git', 'commit', '--allow-empty', '-m', 'move-head'], check=True)",
    ],
)
def test_evidence_rejects_source_or_head_mutation_during_command(tmp_path, mutation):
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(["git", "config", "user.email", "test@example.invalid"], cwd=tmp_path, check=True)
    subprocess.run(["git", "config", "user.name", "Test User"], cwd=tmp_path, check=True)
    run_dir = init_run(tmp_path)
    subprocess.run(["git", "add", "intent.md"], cwd=tmp_path, check=True)
    subprocess.run(["git", "commit", "-qm", "initial"], cwd=tmp_path, check=True)
    add_bundle(tmp_path, run_dir)
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "mutating",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", mutation,
    )
    assert result.returncode == 1
    assert json.loads((run_dir / "RUN.json").read_text())["evidence"] == []


def test_independent_validator_rejects_forged_output_and_git_metadata(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "print('output')",
    )
    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    row = receipt["evidence"][0]
    artifact = next(item for item in receipt["artifacts"] if item["id"] == "evidence-bundle")
    evidence_validator = load_module(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_validation_evidence.py",
        "delivery_validation_forgery_test",
    )

    forged_output = copy.deepcopy(row)
    forged_output["result"]["stdout"]["bytes"] += 1
    forged_output["_declared_artifact"] = artifact
    with pytest.raises(evidence_validator.Invalid, match="stdout"):
        evidence_validator._validate_execution_result(
            receipt, forged_output, ["intent.md"], receipt_dir=None,
            workspace_root=None, verify_hashes=False,
        )

    forged_git = copy.deepcopy(row)
    forged_git["result"]["git"] = {}
    forged_git["_declared_artifact"] = artifact
    with pytest.raises(evidence_validator.Invalid, match="Git provenance|git"):
        evidence_validator._validate_execution_result(
            receipt, forged_git, ["intent.md"], receipt_dir=None,
            workspace_root=None, verify_hashes=False,
        )


def test_public_approved_transition_rejects_stale_design_bytes(tmp_path):
    assessment_value = assessment()
    assessment_value["blast_radius"] = "multi-module"
    (tmp_path / "intent.md").write_text("design before\n")
    result = run_producer(
        tmp_path, "init", "--run-dir", ".agent-run/DEL-APPROVED", "--run-id", "DEL-APPROVED",
        "--profile", "software", "--chair-family", "openai",
        "--risk-assessment", json.dumps(assessment_value), "--intent", "intent.md",
        "--authority", json.dumps(authority()),
    )
    assert result.returncode == 0, result.stderr
    run_dir = tmp_path / ".agent-run/DEL-APPROVED"
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    initial = receipt["state_history"][0]["at"]
    later = (datetime.fromisoformat(initial[:-1]) + timedelta(microseconds=1)).isoformat() + "Z"
    receipt["status"] = "scoped"
    receipt["state_history"].append({"state": "scoped", "at": later, "evidence_ids": [], "risk_tier": "substantial"})
    intent = receipt["artifacts"][0]
    receipt["intent"]["approval"] = {"status": "approved", "approver": "owner", "evidence": "intent-approval"}
    receipt["design"] = {"status": "approved", "artifact_id": intent["id"], "digest": intent["digest"], "evidence": "design-approval"}
    receipt["evidence"] = [
        {"id": "intent-approval", "kind": "human", "gate": "intent-approval", "status": "pass"},
        {"id": "design-approval", "kind": "human", "gate": "design-approval", "status": "pass"},
    ]
    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
    (tmp_path / "intent.md").write_text("design after\n")
    rejected = run_producer(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "approved",
        "--evidence", "intent-approval", "--evidence", "design-approval",
    )
    assert rejected.returncode == 1
    assert "live bytes" in rejected.stderr
    assert json.loads(receipt_path.read_text())["status"] == "scoped"


def test_timeout_path_reaps_before_publishing_child_exit(tmp_path, monkeypatch):
    module = load_module(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt_process.py",
        "delivery_receipt_timeout_reap_test",
    )
    command = [
        sys.executable,
        "-c",
        "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(.3)",
    ]
    observed = module.execute_bounded(
        command, cwd=tmp_path, timeout_seconds=.05, max_log_bytes=128, error_type=ValueError,
    )
    assert isinstance(observed["exit_code"], int)
    assert observed["custody"]["status"] == "posix-process-group-cleanup"


def test_timeout_result_is_validator_valid_failed_evidence(tmp_path, monkeypatch):
    producer = load_module(PRODUCER, "delivery_receipt_timeout_evidence_test")
    process_module = load_module(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt_process.py",
        "delivery_receipt_timeout_evidence_process_test",
    )
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(
        producer,
        "execute_bounded",
        lambda command, *, cwd: process_module.execute_bounded(
            command, cwd=cwd, timeout_seconds=.05, max_log_bytes=128, error_type=ValueError,
        ),
    )
    args = producer.build_parser().parse_args([
        "evidence", "run", "--run-dir", str(run_dir), "--id", "timed-out",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "import time; time.sleep(1)",
    ])
    assert producer.command_evidence_run(args)["status"] == "fail"
    receipt = json.loads((run_dir / "RUN.json").read_text())
    row = receipt["evidence"][0]
    artifact = next(item for item in receipt["artifacts"] if item["id"] == "evidence-bundle")
    validator = load_module(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_validation_evidence.py",
        "delivery_validation_timeout_evidence_test",
    )
    row["_declared_artifact"] = artifact
    validator._validate_execution_result(
        receipt, row, ["intent.md"], receipt_dir=run_dir,
        workspace_root=tmp_path, verify_hashes=True,
    )


def test_crash_journal_is_reconciled_by_ordinary_checkpoint_mutation(tmp_path, monkeypatch):
    run_dir = init_run(tmp_path)
    orphan = tmp_path / "binding" / "orphan.json"
    orphan.parent.mkdir()
    orphan.write_bytes(b"orphan\n")
    module = load_module(PRODUCER, "delivery_receipt_journal_recovery_test")
    journal = run_dir / ".bind-recovery-crashed.json"
    journal.write_text(json.dumps({"schema_version": 1, "targets": [{
        "path": "binding/orphan.json", "digest": module.digest_bytes(orphan.read_bytes()), "created": True,
    }]}))
    monkeypatch.chdir(tmp_path)
    checkpoint = load_module(
        ROOT / "skills" / "implement" / "scripts" / "checkpoint_run.py",
        "checkpoint_journal_recovery_test",
    )
    checkpoint.update(run_dir / "RUN.json", "recovery", "continue", [], [])
    assert not orphan.exists()
    assert not journal.exists()


def test_crash_journal_is_reconciled_by_direct_evidence_lock_entry(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    orphan = tmp_path / "binding" / "orphan.json"
    orphan.parent.mkdir()
    orphan.write_bytes(b"orphan\n")
    module = load_module(PRODUCER, "delivery_receipt_journal_evidence_entry_test")
    journal = run_dir / ".bind-recovery-evidence-entry.json"
    journal.write_text(json.dumps({"schema_version": 1, "targets": [{
        "path": "binding/orphan.json", "digest": module.digest_bytes(orphan.read_bytes()), "created": True,
    }]}))

    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "evidence-entry",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "pass",
    )

    assert result.returncode == 0, result.stderr
    assert not orphan.exists()
    assert not journal.exists()


def test_checkpoint_compatibility_wrapper_matches_canonical_primitive(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    first = init_run(tmp_path, "DEL-WRAPPER")
    second = init_run(tmp_path, "DEL-CANONICAL")
    artifact = tmp_path / "checkpoint.md"
    artifact.write_text("checkpoint\n")
    wrapper = load_module(
        ROOT / "skills" / "implement" / "scripts" / "checkpoint_run.py",
        "checkpoint_wrapper_parity_test",
    )
    wrapped = wrapper.update(first / "RUN.json", "slice", "next", ["worker"], ["checkpoint.md"])
    canonical = run_producer(
        tmp_path, "checkpoint", "set", "--run-dir", str(second), "--current-slice", "slice",
        "--next-action", "next", "--in-flight", "worker", "--artifact", "checkpoint.md",
    )
    assert canonical.returncode == 0, canonical.stderr
    assert wrapped["generation"] == 1
    first_checkpoint = json.loads((first / "RUN.json").read_text())["checkpoint"]
    second_checkpoint = json.loads((second / "RUN.json").read_text())["checkpoint"]
    assert first_checkpoint == second_checkpoint
    for current_slice, next_action in (("", "next"), ("slice", "")):
        with pytest.raises(ValueError, match="non-empty"):
            wrapper.update(first / "RUN.json", current_slice, next_action, [], [])

    malformed = json.loads((first / "RUN.json").read_text())
    malformed["risk_tier"] = "unrecognised"
    (first / "RUN.json").write_text(json.dumps(malformed, indent=2) + "\n")
    with pytest.raises(ValueError, match="risk_tier"):
        wrapper.update(first / "RUN.json", "slice", "next", [], [])


def test_deterministic_bundle_rejects_cross_role_artifact_references(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["evidence"].append({
        "id": "human-reference", "kind": "human", "gate": "manual", "status": "pass",
        "method": "manual", "artifact_id": "evidence-bundle", "source_paths": [],
    })
    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", "pass",
    )
    assert result.returncode == 1
    assert "exclusively" in result.stderr or "deterministic" in result.stderr


@pytest.mark.parametrize(
    ("artifact_class", "artifact_type", "retention"),
    [("evidence", "source", "risk-policy"), ("canonical", "report", "project-policy"), ("evidence", "evidence", "not-a-profile-retention")],
)
def test_artifact_add_rejects_validator_invalid_shape(tmp_path, artifact_class, artifact_type, retention):
    run_dir = init_run(tmp_path)
    output = tmp_path / "artifact.txt"
    output.write_text("artifact\n")
    result = run_producer(
        tmp_path, "artifact", "add", "--run-dir", str(run_dir), "--id", "invalid-artifact",
        "--path", "artifact.txt", "--class", artifact_class, "--media-type", "text/plain",
        "--artifact-type", artifact_type, "--owner", "chair", "--retention", retention,
    )
    assert result.returncode == 1
    assert "artifact" in result.stderr


def test_approval_gate_rehashes_changed_live_design_bytes(tmp_path):
    run_dir = init_run(tmp_path)
    module = load_module(PRODUCER, "delivery_receipt_live_design_test")
    run = module.load_run(run_dir, workspace_root=tmp_path)
    run["risk_tier"] = "substantial"
    run["state_history"][0]["risk_tier"] = "substantial"
    run["intent"]["approval"] = {"status": "approved", "approver": "owner", "evidence": "intent-approval"}
    run["design"] = {"status": "approved", "artifact_id": "intent", "digest": run["artifacts"][0]["digest"], "evidence": "design-approval"}
    run["evidence"] = [
        {"id": "intent-approval", "kind": "human", "gate": "intent-approval", "status": "pass"},
        {"id": "design-approval", "kind": "human", "gate": "design-approval", "status": "pass"},
    ]
    (tmp_path / "intent.md").write_text("changed after binding\n")
    error = module.lifecycle.transition_gate_error(
        run, "approved", {"intent-approval", "design-approval"}, module._api(), workspace=tmp_path,
    )
    assert error and "live bytes" in error


def test_setsid_descendant_is_outside_the_process_group_contract(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    marker = tmp_path / "escaped-marker"
    script = (
        "import os, subprocess, sys, time; "
        f"subprocess.Popen([sys.executable, '-c', \"import os,time; os.setsid(); time.sleep(0.4); open({str(marker)!r}, 'w').write('escaped')\"]); "
        "time.sleep(0.15); "
        "sys.exit(0)"
    )
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "escaped",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", script,
    )
    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    observed = receipt["evidence"][0]
    assert observed["result"]["custody"]["status"] == "posix-process-group-cleanup"
    assert observed["result"]["custody"]["unsupported"] == "Commands that daemonise or call setsid are unsupported."
    assert "escaped_descendants" not in observed["result"]["custody"]
    assert "remaining_descendants" not in observed["result"]["custody"]
    assert "unaccounted_descendants" not in observed["result"]["custody"]
    time.sleep(0.5)
    assert marker.exists()


def test_daemonised_descendant_is_not_claimed_as_contained(tmp_path):
    run_dir = init_run(tmp_path)
    add_bundle(tmp_path, run_dir)
    script = (
        "import subprocess, sys; "
        "subprocess.Popen([sys.executable, '-c', 'import os,time; os.setsid(); time.sleep(0.3)']);"
    )
    result = run_producer(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "unaccounted",
        "--gate", "tests", "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
        sys.executable, "-c", script,
    )
    assert result.returncode == 0, result.stderr
    row = json.loads((run_dir / "RUN.json").read_text())["evidence"][0]
    assert row["result"]["custody"]["status"] == "posix-process-group-cleanup"
    assert row["result"]["custody"]["unsupported"] == "Commands that daemonise or call setsid are unsupported."
    assert "escaped_descendants" not in row["result"]["custody"]
    assert "remaining_descendants" not in row["result"]["custody"]
    assert "unaccounted_descendants" not in row["result"]["custody"]
    time.sleep(0.4)


def test_binding_contract_rolls_back_side_artifacts_when_commit_validation_fails(tmp_path):
    run_dir = init_run(tmp_path)
    module = load_module(PRODUCER, "delivery_receipt_binding_rollback_test")
    receipt = run_dir / "RUN.json"
    before = receipt.read_bytes()

    def mutate(run, _run_dir, _workspace):
        run["checkpoint"]["current_slice"] = "binding"
        return {"ok": True}

    def reject(_run, _run_dir, _workspace):
        raise module.ReceiptError("injected bind failure")

    with pytest.raises(module.ReceiptError, match="injected bind failure"):
        module.mutate_receipt_with_artifacts(
            receipt, mutate, [("binding/payload.json", b"payload\n")],
            before_commit=reject, workspace=tmp_path,
        )
    assert receipt.read_bytes() == before
    assert not (tmp_path / "binding/payload.json").exists()
    assert not list(run_dir.glob(".bind-recovery-*.json"))
