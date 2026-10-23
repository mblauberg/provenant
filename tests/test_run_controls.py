"""Focused tests for the file-backed ``provenant run`` operator surface."""

from __future__ import annotations

import io
import json
import os
import stat
import subprocess
import textwrap
import threading
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from tests.support.fake_cli import fake_bin, fake_codex
from tests.support.modules import load_script
from tests.support.runs import file_digest, make_run, write_attempt

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/run_controls.py"
FINALIZE = ROOT / "skills/orchestrate/scripts/run_dir_finalize.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"


@pytest.mark.parametrize("outcome,pid", [("host_disk_full", 12345), ("host_disk_full", None), ("provider_lost", 12345), ("process_spawn_error", None)])
def test_retained_host_failure_accepts_unobserved_exit(tmp_path, outcome, pid):
    run_dir = make_run(tmp_path, "controls")
    attempt = write_attempt(run_dir, status="failed", result=None, observed_exit=False)
    row = json.loads(attempt.read_text())
    row.update(outcome=outcome)
    row["process"].update(pid=pid, exit_code=None)
    attempt.write_text(json.dumps(row) + "\n")
    attempt.with_name("attempt.sha256").write_text(f"{file_digest(attempt)}  attempt.json\n")
    module = load_script(SCRIPT, "run_controls_host_failure")
    assert module.validate_retained_dispatch(run_dir) == []
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "status": "failed", "tasks": [{"task_id": "task-1", "status": "failed",
            "attempt_path": str(attempt.relative_to(run_dir))}],
    }))
    assert module._load_summary(run_dir, "batch-001")["status"] == "failed"


def test_retained_legacy_success_is_valid(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "controls")
    write_attempt(run_dir, status="succeeded")
    module = load_script(SCRIPT, "legacy_run_controls")

    assert module.validate_retained_dispatch(run_dir) == []


def test_retained_git_evidence_path_digest_and_payload_are_validated(tmp_path: Path):
    run_dir = make_run(tmp_path, "controls")
    attempt = write_attempt(run_dir, status="failed", result=None)
    attempt_dir = attempt.parent
    packet = attempt_dir / "evidence" / "git-evidence.md"
    packet.parent.mkdir()
    metadata = {
        "schema_version": 1, "record_type": "provenant-git-evidence",
        "repository": str(tmp_path), "git_root": str(tmp_path), "head": "a" * 40,
        "diff_base": "b" * 40, "working_tree": "clean", "diff_from": "HEAD",
        "paths": [], "encoding": "utf-8-replacement",
    }
    packet.write_text(
        json.dumps(metadata, sort_keys=True) + "\n--- status ---\n--- diff ---\n" + ("x" * 2_000_000),
        encoding="utf-8",
    )
    record = json.loads(attempt.read_text(encoding="utf-8"))
    record["git_evidence"] = {
        "path": str(packet.relative_to(run_dir)), "digest": file_digest(packet), "checkout": metadata,
    }
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    (attempt_dir / "attempt.sha256").write_text(f"{file_digest(attempt)}  attempt.json\n", encoding="utf-8")
    valid_packet = packet.read_bytes()
    module = load_script(SCRIPT, "run_controls_git_evidence")

    module._attempt(run_dir, "task-1", "attempt-001")
    assert module.validate_retained_dispatch(run_dir) == []
    packet.write_text(packet.read_text(encoding="utf-8") + "tampered\n", encoding="utf-8")
    with pytest.raises(module.ControlError, match="Git evidence digest"):
        module._attempt(run_dir, "task-1", "attempt-001")
    packet.write_bytes(valid_packet)
    packet.unlink()
    with pytest.raises(module.ControlError, match="unavailable"):
        module._attempt(run_dir, "task-1", "attempt-001")
    packet.write_bytes(valid_packet)
    substitute = packet.with_name("substitute.md")
    substitute.write_bytes(valid_packet)
    record["git_evidence"]["path"] = str(substitute.relative_to(run_dir))
    record["git_evidence"]["digest"] = file_digest(substitute)
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    (attempt_dir / "attempt.sha256").write_text(f"{file_digest(attempt)}  attempt.json\n", encoding="utf-8")
    with pytest.raises(module.ControlError, match="does not match its attempt"):
        module._attempt(run_dir, "task-1", "attempt-001")


def test_retained_success_rejects_malformed_adapter_receipt(tmp_path: Path) -> None:
    """Controls and finalisation must not trust a success record by itself."""
    run_dir = make_run(tmp_path, "controls")
    attempt = write_attempt(run_dir)
    adapter = attempt.with_name("adapter-receipt.json")
    adapter.write_text("{}\n", encoding="utf-8")
    record = json.loads(attempt.read_text(encoding="utf-8"))
    record["route"]["adapter_receipt"]["digest"] = file_digest(adapter)
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    attempt.with_name("attempt.sha256").write_text(
        f"{file_digest(attempt)}  attempt.json\n", encoding="utf-8"
    )
    module = load_script(SCRIPT, "run_controls_invalid_success")

    errors = module.validate_retained_dispatch(run_dir)

    assert errors == [
        "dispatch attempt dispatch/tasks/task-1/attempt-001/attempt.json: "
        "successful adapter receipt is invalid"
    ]
    finalizer = load_script(FINALIZE, "run_dir_finalize_invalid_success")
    assert finalizer._validate_dispatch_evidence(run_dir) == errors


@pytest.mark.parametrize("exit_code", [1, False])
def test_retained_success_rejects_a_non_zero_exit(tmp_path: Path, exit_code: object) -> None:
    """Controls and finalisation reject a success without exit 0 (own check and shared validator)."""
    run_dir = make_run(tmp_path, "controls")
    attempt = write_attempt(run_dir)
    record = json.loads(attempt.read_text(encoding="utf-8"))
    record["process"]["exit_code"] = exit_code
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    attempt.with_name("attempt.sha256").write_text(
        f"{file_digest(attempt)}  attempt.json\n", encoding="utf-8"
    )
    module = load_script(SCRIPT, "run_controls_invalid_exit")

    errors = module.validate_retained_dispatch(run_dir)

    assert errors == [
        "dispatch attempt dispatch/tasks/task-1/attempt-001/attempt.json: "
        "successful attempt does not prove exit 0"
    ]
    finalizer = load_script(FINALIZE, "run_dir_finalize_invalid_exit")
    assert finalizer._validate_dispatch_evidence(run_dir) == errors


def write_success_adapter(path: Path) -> None:
    path.write_text(textwrap.dedent("""
        #!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --out) out="$2"; shift 2;;
            --intent) intent="$2"; shift 2;;
            --tool) tool="$2"; shift 2;;
            *) shift;;
          esac
        done
        printf 'provider result\\n' > "$out"
        digest="sha256:$(shasum -a 256 "$out" | awk '{print $1}')"
        printf '{"tool":"%s","adapter":"%s","execution_intent":"%s","resolved_model":"test-model","provider_family":"test-family","model_family":"test-family","endpoint_provider":"test-provider","identity_source":"test-fixture","status":"ok","exit":0,"output_path":"%s","output_digest":"%s","read_only_guarantee":"none","cross_family":false,"certification_eligible":false}\\n' "$tool" "$tool" "$intent" "$out" "$digest"
    """).strip() + "\n", encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def write_question_adapter(path: Path) -> None:
    path.write_text(textwrap.dedent("""
        #!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in
            --out) out="$2"; shift 2;;
            *) shift;;
          esac
        done
        printf '%s\\n' '{"schema_version":1,"record_type":"provenant-worker-terminal","classification":"question","question":{"code":"needs_input","prompt":"Which source should I use?"}}' > "$out"
        digest="sha256:$(shasum -a 256 "$out" | awk '{print $1}')"
        printf '{"tool":"codex","adapter":"codex","execution_intent":"ordinary","resolved_model":"test-model","provider_family":"test-family","model_family":"test-family","endpoint_provider":"test-provider","identity_source":"test-fixture","status":"ok","exit":0,"output_path":"%s","output_digest":"%s","read_only_guarantee":"none","cross_family":false,"certification_eligible":false}\\n' "$out" "$digest"
    """).strip() + "\n", encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def write_dispatch_wrapper(path: Path, adapter: Path) -> None:
    path.write_text(
        "#!/usr/bin/env python3\n"
        "import importlib.util\n"
        "import sys\n"
        "from pathlib import Path\n"
        # The wrapper runs as its own process, so it loads the dispatch owner
        # from its file rather than repairing sys.path (#755).
        f"_spec = importlib.util.spec_from_file_location('dispatch_run', {str(ROOT / 'skills/orchestrate/scripts/dispatch_run.py')!r})\n"
        "dispatch_run = importlib.util.module_from_spec(_spec)\n"
        "_spec.loader.exec_module(dispatch_run)\n"
        f"dispatch_run.CF_DISPATCH = Path({str(adapter)!r})\n"
        f"sys.argv = [{str(ROOT / 'skills/orchestrate/scripts/dispatch_run.py')!r}, *sys.argv[1:]]\n"
        "raise SystemExit(dispatch_run.dispatch(dispatch_run.parser().parse_args()))\n",
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def test_cancel_waits_through_attempt_sidecar_gap(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "controls")
    attempt = write_attempt(run_dir, status="failed", result=None)
    sidecar = attempt.with_name("attempt.sha256")
    sidecar_bytes = sidecar.read_bytes()
    sidecar.unlink()
    module = load_script(SCRIPT, "run_controls_sidecar_gap")

    def publish_sidecar() -> None:
        time.sleep(0.1)
        sidecar.write_bytes(sidecar_bytes)

    publisher = threading.Thread(target=publish_sidecar)
    publisher.start()
    try:
        record = module._wait_for_attempt_terminal(run_dir, "task-1", "attempt-001", 1.0)
    finally:
        publisher.join()
    assert record is not None
    assert record["status"] == "failed"


def test_attempt_terminal_tolerates_receipt_renamed_before_its_digest_sidecar(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "controls")
    attempt = write_attempt(run_dir, status="failed", result=None)
    sidecar = attempt.with_name("attempt.sha256")
    fresh = sidecar.read_bytes()
    sidecar.write_bytes(b"sha256:" + b"0" * 64 + b"  attempt.json\n")  # previous receipt's digest
    module = load_script(SCRIPT, "run_controls_stale_sidecar")

    def publish_sidecar() -> None:
        time.sleep(0.1)
        sidecar.write_bytes(fresh)

    publisher = threading.Thread(target=publish_sidecar)
    publisher.start()
    try:
        record = module._attempt_terminal(run_dir, "task-1", "attempt-001")
    finally:
        publisher.join()
    assert record is not None and record["status"] == "failed"

    sidecar.write_bytes(b"sha256:" + b"0" * 64 + b"  attempt.json\n")
    module.DIGEST_SETTLE_SECONDS = 0.1
    with pytest.raises(module.ControlError, match="attempt evidence is invalid"):
        module._attempt_terminal(run_dir, "task-1", "attempt-001")


def test_cancel_waits_through_late_summary_bundle_completion(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str],
) -> None:
    run_dir = make_run(tmp_path, "controls")
    attempt = write_attempt(run_dir, status="failed", result=None)
    sidecar = attempt.with_name("attempt.sha256")
    sidecar_bytes = sidecar.read_bytes()
    sidecar.unlink()
    batch_dir = run_dir / "dispatch/batches/batch-001"
    batch_dir.mkdir(parents=True)
    source_manifest = batch_dir / "task-manifest.json"
    source_manifest.write_text("{\"schema_version\":1,\"tasks\":[{\"id\":\"task-1\"}]}\n", encoding="utf-8")
    (batch_dir / "summary.json").write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "status": "failed", "task_count": 1, "concurrency": 1,
        "counts": {"failed": 1}, "tasks": [{
                "task_id": "task-1", "status": "failed", "outcome": "failed",
                "attempt_path": "dispatch/tasks/task-1/attempt-001/attempt.json",
                "attempt_digest": file_digest(attempt),
        }],
        "source_manifest": {
            "path": "dispatch/batches/batch-001/task-manifest.json",
            "digest": file_digest(source_manifest),
        },
        "reducer_inputs": [{
            "task_id": "task-1", "status": "failed",
            "attempt_path": "dispatch/tasks/task-1/attempt-001/attempt.json",
            "result_path": None,
        }],
    }) + "\n", encoding="utf-8")
    module = load_script(SCRIPT, "run_controls_late_summary")
    monkeypatch.chdir(tmp_path)

    publisher_ready = threading.Event()
    publish_now = threading.Event()
    published = threading.Event()
    fake_time = [0.0]

    def publish_sidecar() -> None:
        publisher_ready.set()
        assert publish_now.wait(timeout=5)
        sidecar.write_bytes(sidecar_bytes)
        published.set()

    def release_publisher_during_wait(seconds: float) -> None:
        publish_now.set()
        assert published.wait(timeout=5)
        fake_time[0] += seconds

    monkeypatch.setattr(module, "time", SimpleNamespace(
        monotonic=lambda: fake_time[0], sleep=release_publisher_during_wait,
    ))

    publisher = threading.Thread(target=publish_sidecar)
    publisher.start()
    try:
        assert publisher_ready.wait(timeout=5)
        args = module.parser().parse_args([
            "cancel", "--run-dir", str(run_dir), "--batch-id", "batch-001",
            "--wait-seconds", "1",
        ])
        assert module.run(args) == 0
        assert json.loads(capsys.readouterr().out)["status"] == "already_terminal"
    finally:
        publisher.join()


def test_retry_requires_one_route_choice_and_delegates_parent(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    write_attempt(run_dir, status="failed", result=None)
    fake = tmp_path / "dispatch-run"
    invoked = tmp_path / "invoked.json"
    fake.write_text(
        f"#!/usr/bin/env python3\nimport json, sys\njson.dump(sys.argv[1:], open({str(invoked)!r}, 'w'))\n",
        encoding="utf-8",
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)

    module = load_script(SCRIPT, "run_controls_under_test")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001", "--same-route",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.run(args) == 0
    delegated = json.loads(invoked.read_text(encoding="utf-8"))
    assert "--retry-of" in delegated
    assert delegated[delegated.index("--retry-of") + 1] == "attempt-001"
    assert "--prompt-stdin" in delegated
    assert delegated[delegated.index("--risk-tier") + 1] == "substantial"
    assert delegated[delegated.index("--reviewer-id") + 1] == "reviewer-1"
    assert delegated[delegated.index("--effort") + 1] == "high"


def test_blocked_retry_requires_response_and_retains_new_attempt_prompt(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    question = {"code": "needs_input", "prompt": "Which source should I use?"}
    envelope = json.dumps({
        "schema_version": 1, "record_type": "provenant-worker-terminal",
        "classification": "question", "question": question,
    }) + "\n"
    parent = write_attempt(run_dir, status="blocked", result=envelope, question=question)
    parent_before = parent.read_bytes()
    fake = tmp_path / "dispatch-run"
    captured = tmp_path / "captured-prompt"
    fake.write_text(
        f"#!/usr/bin/env python3\nimport sys\nopen({str(captured)!r}, 'wb').write(sys.stdin.buffer.read())\n",
        encoding="utf-8",
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    module = load_script(SCRIPT, "run_controls_blocked_retry")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    response = tmp_path / "response.md"
    response.write_text("Use the primary archive.\n", encoding="utf-8")
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route", "--response-file", str(response),
    ])
    monkeypatch.chdir(tmp_path)

    assert module.run(args) == 0
    assert parent.read_bytes() == parent_before
    prompt = captured.read_text(encoding="utf-8")
    assert prompt.startswith("prompt\n")
    assert "prompt" in prompt and "Which source should I use?" in prompt
    assert "Use the primary archive." in prompt


def test_blocked_retry_accepts_response_stdin_and_nonblocked_rejects_response(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    question = {"code": "needs_input", "prompt": "Which source?"}
    parent = write_attempt(run_dir, status="blocked", result=json.dumps({
        "schema_version": 1, "record_type": "provenant-worker-terminal",
        "classification": "question", "question": question,
    }) + "\n", question=question)
    fake = tmp_path / "dispatch-run"
    captured = tmp_path / "captured-prompt"
    fake.write_text(
        f"#!/usr/bin/env python3\nimport sys\nopen({str(captured)!r}, 'wb').write(sys.stdin.buffer.read())\n",
        encoding="utf-8",
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    module = load_script(SCRIPT, "run_controls_stdin_retry")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(module.sys, "stdin", io.TextIOWrapper(io.BytesIO(b"stdin response\n")))
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route", "--response-stdin",
    ])
    assert module.run(args) == 0
    assert "stdin response" in captured.read_text(encoding="utf-8")
    assert parent.exists()

    nonblocked = make_run(tmp_path / "nonblocked", "controls")
    write_attempt(nonblocked, status="failed", result=None)
    rejected = module.parser().parse_args([
        "retry", "--run-dir", str(nonblocked), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route", "--response-stdin",
    ])
    assert module.run(rejected) == 2


def test_blocked_retry_without_response_is_rejected(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    write_attempt(run_dir, status="blocked", result="question\n",
                  question={"code": "needs_input", "prompt": "Which source?"})
    module = load_script(SCRIPT, "run_controls_missing_response")
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route",
    ])

    assert module.run(args) == 2


@pytest.mark.parametrize("response", [b"\xff\n", b"\x00answer\n", b" \t\n"])
def test_blocked_retry_rejects_invalid_response_file_bytes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, response: bytes
) -> None:
    run_dir = make_run(tmp_path, "controls")
    write_attempt(run_dir, status="blocked", result="question\n",
                  question={"code": "needs_input", "prompt": "Which source?"})
    response_file = tmp_path / "response"
    response_file.write_bytes(response)
    module = load_script(SCRIPT, "run_controls_bad_response")
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route", "--response-file", str(response_file),
    ])

    assert module.run(args) == 2


def test_blocked_retry_rejects_oversized_response_before_dispatch(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    write_attempt(run_dir, status="blocked", result="question\n",
                  question={"code": "needs_input", "prompt": "Which source?"})
    response_file = tmp_path / "response"
    response_file.write_bytes(b"x" * (64 * 1024 + 1))
    invoked = tmp_path / "invoked"
    fake = tmp_path / "dispatch-run"
    fake.write_text(f"#!/bin/sh\ntouch {invoked}\n", encoding="utf-8")
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    module = load_script(SCRIPT, "run_controls_large_response")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route", "--response-file", str(response_file),
    ])

    assert module.run(args) == 2
    assert not invoked.exists()


def test_blocked_retry_rejects_oversized_response_stdin_before_dispatch(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    write_attempt(run_dir, status="blocked", result="question\n",
                  question={"code": "needs_input", "prompt": "Which source?"})
    invoked = tmp_path / "invoked"
    fake = tmp_path / "dispatch-run"
    fake.write_text(f"#!/bin/sh\ntouch {invoked}\n", encoding="utf-8")
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    module = load_script(SCRIPT, "run_controls_large_stdin")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(module.sys, "stdin", io.TextIOWrapper(io.BytesIO(b"x" * (64 * 1024 + 1))))
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route", "--response-stdin",
    ])

    assert module.run(args) == 2
    assert not invoked.exists()


def test_blocked_retry_escapes_lone_surrogate_in_question_prompt(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    question = {"code": "needs_input", "prompt": "bad\ud800"}
    write_attempt(run_dir, status="blocked", result=json.dumps({
        "schema_version": 1, "record_type": "provenant-worker-terminal",
        "classification": "question", "question": question,
    }) + "\n", question=question)
    fake = tmp_path / "dispatch-run"
    captured = tmp_path / "captured-prompt"
    fake.write_text(
        f"#!/usr/bin/env python3\nimport sys\nopen({str(captured)!r}, 'wb').write(sys.stdin.buffer.read())\n",
        encoding="utf-8",
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    response = tmp_path / "response"
    response.write_text("answer\n", encoding="utf-8")
    module = load_script(SCRIPT, "run_controls_surrogate")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001",
        "--same-route", "--response-file", str(response),
    ])

    assert module.run(args) == 0
    assert b"\\ud800" in captured.read_bytes()


def test_evidence_reader_rejects_intermediate_directory_symlink(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "controls")
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "evidence.txt").write_text("secret", encoding="utf-8")
    (run_dir / "dispatch").mkdir()
    (run_dir / "dispatch/link").symlink_to(outside, target_is_directory=True)
    module = load_script(SCRIPT, "run_controls_reader")

    with pytest.raises(module.ControlError):
        module._read_regular(run_dir, "dispatch/link/evidence.txt", "evidence")


def test_prompt_file_rejects_intermediate_parent_swap(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    prompt_dir = tmp_path / "prompts"
    prompt_dir.mkdir()
    prompt_path = prompt_dir / "operator.md"
    prompt_path.write_text("safe\n", encoding="utf-8")
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "operator.md").write_text("outside\n", encoding="utf-8")
    module = load_script(SCRIPT, "run_controls_prompt_reader")
    original_open = module.os.open
    swapped = False

    def swap_parent(path, flags, mode=0o777, *, dir_fd=None):
        nonlocal swapped
        if dir_fd is not None and path == "prompts" and not swapped:
            prompt_dir.rename(tmp_path / "prompts-original")
            (tmp_path / "prompts").symlink_to(outside, target_is_directory=True)
            swapped = True
        return original_open(path, flags, mode, dir_fd=dir_fd)

    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(module.os, "open", swap_parent)
    with pytest.raises(module.ControlError):
        module._prompt_file(prompt_path)
    assert swapped


def test_reduce_requires_explicit_successes_and_names_batch_omissions(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path, "controls")
    selected = write_attempt(run_dir, task="one")
    omitted = write_attempt(run_dir, task="two")
    failed = write_attempt(run_dir, task="three", status="failed", result=None)
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1,
        "record_type": "dispatch-batch",
        "batch_id": "batch-001",
        "tasks": [
            {"task_id": "one", "status": "ok", "attempt_path": str(selected.relative_to(run_dir)),
             "result_path": "dispatch/tasks/one/attempt-001/result.md"},
            {"task_id": "two", "status": "ok", "attempt_path": str(omitted.relative_to(run_dir)),
             "result_path": "dispatch/tasks/two/attempt-001/result.md"},
            {"task_id": "three", "status": "failed", "attempt_path": str(failed.relative_to(run_dir))},
        ],
    }, sort_keys=True) + "\n", encoding="utf-8")
    prompt = tmp_path / "reduce.md"
    prompt.write_text("Summarise the evidence.\n", encoding="utf-8")
    fake = tmp_path / "dispatch-run"
    captured = tmp_path / "captured.json"
    captured_prompt = tmp_path / "captured-prompt.md"
    fake.write_text(
        f"#!/usr/bin/env python3\nimport json,sys\njson.dump(sys.argv[1:],open({str(captured)!r},'w'))\nopen({str(captured_prompt)!r},'wb').write(sys.stdin.buffer.read())\n",
        encoding="utf-8",
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    module = load_script(SCRIPT, "run_controls_reduce")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "reduce", "--run-dir", str(run_dir), "--task-id", "reducer", "--prompt-file", str(prompt),
        "--batch-id", "batch-001", "--input", "one/attempt-001", "--adapter", "codex", "--alias", "junior",
    ])

    assert module.run(args) == 0
    delegated = json.loads(captured.read_text(encoding="utf-8"))
    reducer_prompt = captured_prompt
    assert reducer_prompt.exists()
    reducer_text = reducer_prompt.read_text(encoding="utf-8")
    assert "- two/attempt-001" in reducer_text and "- three/attempt-001: failed" in reducer_text
    assert "Summarise the evidence." in reducer_text
    assert "--prompt-stdin" in delegated
    assert "--task-id" in delegated and delegated[delegated.index("--task-id") + 1] == "reducer"


@pytest.mark.parametrize(("second_delivery", "expected_delivery"), [
    ("delivery:1202", "delivery:1202"), ("delivery:other", None), (None, None),
])
def test_reduce_forwards_delivery_run_id_only_when_selected_attempts_agree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, second_delivery: str | None, expected_delivery: str | None,
) -> None:
    run_dir = make_run(tmp_path, "controls")
    first = write_attempt(run_dir, task="one", delivery_run_id="delivery:1202")
    second = write_attempt(run_dir, task="two", delivery_run_id=second_delivery)
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "tasks": [{"task_id": task, "status": "ok", "attempt_path": str(attempt.relative_to(run_dir)),
                   "result_path": str(attempt.parent / "result.md").replace(str(run_dir) + "/", "")}
                  for task, attempt in (("one", first), ("two", second))],
    }) + "\n", encoding="utf-8")
    prompt = tmp_path / "reduce.md"
    prompt.write_text("reduce", encoding="utf-8")
    fake = tmp_path / "dispatch-run"
    captured = tmp_path / "captured.json"
    fake.write_text(f"#!/usr/bin/env python3\nimport json,sys\njson.dump(sys.argv[1:],open({str(captured)!r},'w'))\n", encoding="utf-8")
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    module = load_script(SCRIPT, "run_controls_reduce_delivery")
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "reduce", "--run-dir", str(run_dir), "--task-id", "reducer", "--prompt-file", str(prompt),
        "--batch-id", "batch-001", "--input", "one/attempt-001", "--input", "two/attempt-001",
        "--adapter", "codex", "--alias", "junior",
    ])

    assert module.run(args) == 0
    delegated = json.loads(captured.read_text(encoding="utf-8"))
    if expected_delivery is None:
        assert "--delivery-run-id" not in delegated
    else:
        assert delegated[delegated.index("--delivery-run-id") + 1] == expected_delivery


def test_retry_and_reduce_use_real_dispatch_owner_and_retain_new_attempts(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    adapter = tmp_path / "adapter"
    dispatch = tmp_path / "dispatch-wrapper"
    write_success_adapter(adapter)
    write_dispatch_wrapper(dispatch, adapter)
    module = load_script(SCRIPT, "run_controls_e2e")
    monkeypatch.setattr(module, "DISPATCH_RUN", dispatch)

    retry_run = make_run(tmp_path / "retry", "controls")
    parent = write_attempt(retry_run, status="failed", result=None)
    parent_before = parent.read_bytes()
    monkeypatch.chdir(tmp_path / "retry")
    retry_args = module.parser().parse_args([
        "retry", "--run-dir", str(retry_run), "--task-id", "task-1", "--attempt-id", "attempt-001", "--same-route",
    ])
    assert module.run(retry_args) == 0
    assert parent.read_bytes() == parent_before
    retry_record = json.loads((retry_run / "dispatch/tasks/task-1/attempt-002/attempt.json").read_text(encoding="utf-8"))
    assert retry_record["retry_of"] == "attempt-001" and retry_record["status"] == "ok"

    reduce_root = tmp_path / "reduce"
    reduce_run = make_run(reduce_root, "controls")
    selected = write_attempt(reduce_run, task="source")
    summary = reduce_run / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "tasks": [{"task_id": "source", "status": "ok",
                   "attempt_path": str(selected.relative_to(reduce_run)),
                   "result_path": "dispatch/tasks/source/attempt-001/result.md"}],
    }) + "\n", encoding="utf-8")
    prompt = reduce_root / "reduce.md"
    prompt.write_text("Reduce these results.", encoding="utf-8")
    monkeypatch.chdir(reduce_root)
    reduce_args = module.parser().parse_args([
        "reduce", "--run-dir", str(reduce_run), "--task-id", "reducer", "--prompt-file", str(prompt),
        "--batch-id", "batch-001", "--input", "source/attempt-001", "--adapter", "codex", "--alias", "junior",
    ])
    assert module.run(reduce_args) == 0
    reducer_attempt = reduce_run / "dispatch/tasks/reducer/attempt-001"
    assert "result" in (reducer_attempt / "prompt.md").read_text(encoding="utf-8")
    assert not list(reduce_root.glob(".provenant-reducer-*.md"))

    operator_prompt = reduce_run / "operator.md"
    operator_prompt.write_text("Reduce from the run directory.", encoding="utf-8")
    monkeypatch.chdir(reduce_run)
    same_dir_args = module.parser().parse_args([
        "reduce", "--run-dir", str(reduce_run), "--task-id", "reducer2", "--prompt-file", str(operator_prompt),
        "--batch-id", "batch-001", "--input", "source/attempt-001", "--adapter", "codex", "--alias", "junior",
    ])
    assert module.run(same_dir_args) == 0
    assert (reduce_run / "dispatch/tasks/reducer2/attempt-001/prompt.md").is_file()
    assert not list(reduce_run.glob(".provenant-reducer-*.md"))


def make_listed_run(tmp_path: Path) -> Path:
    name = "20261022-1200-dispatch-task-1-abc123"
    return Path(subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / "runs" / name)], text=True).strip())


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


def _retained_packet(tmp_path, payload):
    root = Path("dispatch/tasks/task-1/attempt-001")
    packet = tmp_path / root / "evidence/git-evidence.md"
    packet.parent.mkdir(parents=True)
    packet.write_bytes(payload)
    return root, {"path": packet.relative_to(tmp_path).as_posix(),
                  "digest": file_digest(packet), "checkout": _packet_metadata()}


@pytest.mark.parametrize("field,value", _PACKET_HEADER_MUTATIONS)
def test_git_evidence_retained_rejects_each_header_field(tmp_path, field, value):
    module = load_script(SCRIPT, "retained_packet_fields")
    metadata = _packet_metadata()
    metadata[field] = value
    root, claim = _retained_packet(tmp_path, json.dumps(metadata).encode() + b"\n--- status ---\n--- diff ---\n")
    if field == "working_tree" and isinstance(value, list):
        with pytest.raises(TypeError, match="unhashable type"):
            module._validate_git_evidence(tmp_path, root, claim)
    else:
        with pytest.raises(module.ControlError) as error:
            module._validate_git_evidence(tmp_path, root, claim)
        assert str(error.value) == "Git evidence packet is invalid"


@pytest.mark.parametrize("case,message", [
    ("empty", "is invalid"), ("no-newline", "is invalid"),
    ("oversize-no-newline", "header exceeds its 64 KiB limit"),
    ("oversize-newline", "header exceeds its 64 KiB limit"),
    ("bad-json", "header is invalid"), ("bad-utf8", "header is invalid"),
    ("null", "is invalid"), ("non-object", "is invalid"),
    ("missing-key", "is invalid"), ("extra-key", "is invalid"),
    ("wrong-status", "is missing its status section"),
    ("short-status", "is invalid"), ("no-body", "is invalid"), ("missing-diff", "is invalid"),
])
def test_git_evidence_retained_rejection_messages(tmp_path, case, message):
    module = load_script(SCRIPT, "retained_packet_cases")
    root, claim = _retained_packet(tmp_path, _packet_bytes(case))
    with pytest.raises(module.ControlError) as error:
        module._validate_git_evidence(tmp_path, root, claim)
    assert str(error.value) == "Git evidence packet " + message


@pytest.mark.parametrize("case,chunk_size", [("late-diff", 1), ("late-diff", 7), ("late-diff", 64), ("header-at-limit", 1048576)])
def test_git_evidence_retained_stream_boundaries(tmp_path, monkeypatch, case, chunk_size):
    module = load_script(SCRIPT, "retained_packet_chunks")
    root, claim = _retained_packet(tmp_path, _packet_bytes(case))
    read = module.os.read
    monkeypatch.setattr(module.os, "read", lambda fd, size: read(fd, min(size, chunk_size)))
    assert module._validate_git_evidence(tmp_path, root, claim) == claim["path"]


@pytest.mark.parametrize("claim,message", [
    (None, "Git evidence receipt is malformed"),
    ({}, "Git evidence receipt is malformed"),
    ({"path": None, "digest": "", "checkout": {}}, "Git evidence path is missing"),
    ({"path": "", "digest": "", "checkout": {}}, "Git evidence path is missing"),
    ({"path": "/outside", "digest": "", "checkout": {}}, "Git evidence path escapes the run: /outside"),
    ({"path": "../outside", "digest": "", "checkout": {}}, "Git evidence path escapes the run: ../outside"),
    ({"path": ".", "digest": "", "checkout": {}}, "Git evidence path escapes the run: ."),
    ({"path": "a/../b", "digest": "", "checkout": {}}, "Git evidence path escapes the run: a/../b"),
    ({"path": "x", "digest": "", "checkout": {}, "extra": True}, "Git evidence receipt is malformed"),
    ({"path": "other", "digest": "", "checkout": {}}, "Git evidence path does not match its attempt: other"),
])
def test_git_evidence_retained_rejects_claims(tmp_path, claim, message):
    module = load_script(SCRIPT, "retained_packet_claims")
    with pytest.raises(module.ControlError) as error:
        module._validate_git_evidence(tmp_path, Path("attempt"), claim)
    assert str(error.value) == message


def test_git_evidence_retained_checkout_must_match(tmp_path):
    module = load_script(SCRIPT, "retained_packet_checkout")
    root, claim = _retained_packet(tmp_path, _packet_bytes("late-diff"))
    claim["checkout"] = {}
    with pytest.raises(module.ControlError) as error:
        module._validate_git_evidence(tmp_path, root, claim)
    assert str(error.value) == "Git evidence checkout identity disagrees with packet"


@pytest.mark.parametrize("case", ["empty", "missing-key", "short-status", "missing-diff"])
def test_git_evidence_retained_digest_precedes_deferred_structure(tmp_path, case):
    module = load_script(SCRIPT, "retained_packet_precedence")
    root, claim = _retained_packet(tmp_path, _packet_bytes(case))
    claim["digest"] = "sha256:wrong"
    with pytest.raises(module.ControlError) as error:
        module._validate_git_evidence(tmp_path, root, claim)
    assert str(error.value) == "Git evidence digest does not match retained evidence: " + claim["path"]


def test_git_evidence_retained_status_precedes_metadata_and_digest(tmp_path):
    module = load_script(SCRIPT, "retained_packet_status_precedence")
    root, claim = _retained_packet(tmp_path, b"[]\n--- wrong ----\n")
    claim["digest"] = "sha256:wrong"
    with pytest.raises(module.ControlError) as error:
        module._validate_git_evidence(tmp_path, root, claim)
    assert str(error.value) == "Git evidence packet is missing its status section"


def test_git_evidence_retained_rejects_symlink(tmp_path):
    module = load_script(SCRIPT, "retained_packet_symlink")
    root, claim = _retained_packet(tmp_path, _packet_bytes("late-diff"))
    packet = tmp_path / claim["path"]
    outside = tmp_path / "other"
    packet.rename(outside)
    packet.symlink_to(outside)
    with pytest.raises(module.ControlError) as error:
        module._validate_git_evidence(tmp_path, root, claim)
    assert str(error.value) == "Git evidence path is outside or unavailable: " + claim["path"]
