"""Adversarial custody regressions for issue #697."""

from __future__ import annotations

import importlib.util
import hashlib
import json
import subprocess
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
DISPATCH = ROOT / "skills/orchestrate/scripts/dispatch_run.py"
FINALIZE = ROOT / "skills/orchestrate/scripts/run_dir_finalize.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def make_run(tmp_path: Path) -> Path:
    return Path(subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / "run")], text=True).strip()).resolve()


def write_valid_attempt(run: Path, task_id: str = "valid", *, observed_exit: bool = True) -> Path:
    root = run / "dispatch" / "tasks" / task_id / "attempt-001"
    root.mkdir(parents=True)
    prompt = root / "prompt.md"
    stderr = root / "stderr.log"
    result = root / "result.md"
    adapter = root / "adapter-receipt.json"
    prompt.write_bytes(b"prompt\n")
    stderr.write_bytes(b"")
    result.write_bytes(b"OK\n")
    result_digest = "sha256:" + hashlib.sha256(result.read_bytes()).hexdigest()
    adapter.write_text(json.dumps({
        "tool": "codex", "adapter": "codex", "execution_intent": "ordinary",
        "resolved_model": "fixture", "provider_family": "openai", "model_family": "openai",
        "endpoint_provider": "fixture", "identity_source": "fixture", "status": "ok",
        "exit": 0, "output_path": str(result), "output_digest": result_digest,
        "read_only_guarantee": "none", "cross_family": False, "certification_eligible": False,
    }) + "\n")
    rel = lambda path: path.relative_to(run).as_posix()
    digest = lambda path: "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()
    record = {
        "schema_version": 1, "record_type": "dispatch-attempt", "run_id": run.name,
        "task_id": task_id, "attempt_id": "attempt-001", "attempt_path": rel(root / "attempt.json"),
        "status": "succeeded", "outcome": "ok", "requested_route": {
            "intent": "ordinary", "adapter": "codex", "alias": "scout", "task_class": "",
            "model": "", "role": "worker", "effort": "", "orchestrator_family": "",
        },
        "route": {"adapter": "codex", "execution_intent": "ordinary", "resolved_model": "fixture",
                  "provider_family": "openai", "adapter_receipt": {"path": rel(adapter), "digest": digest(adapter)}},
        "prompt": {"path": rel(prompt), "digest": digest(prompt)},
        "result": {"path": rel(result), "digest": digest(result)},
        "stderr": {"path": rel(stderr), "digest": digest(stderr)},
        "process": {"observed_exit": observed_exit, "exit_code": 0},
    }
    attempt = root / "attempt.json"
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n")
    sidecar = root / "attempt.sha256"
    sidecar.write_text(f"{digest(attempt)}  attempt.json\n")
    with (run / "MANIFEST.md").open("a") as manifest:
        for kind, path in (("attempt", attempt), ("prompt", prompt), ("adapter", adapter),
                           ("stderr", stderr), ("attempt-digest", sidecar), ("result", result)):
            manifest.write(f"| {task_id}-{kind} | {rel(path)} | evidence | test | 2026-08-30 | verified | evidence | - |\n")
    return attempt


def close_successful_run(run: Path) -> None:
    receipt = json.loads((run / "RUN_RECEIPT.json").read_text())
    receipt["task"] = "issue 697 finalization"
    (run / "RUN_RECEIPT.json").write_text(json.dumps(receipt))
    (run / "SYNTHESIS.md").write_text("verified\n")
    gate = (run / "FINAL_GATE.md").read_text().splitlines()
    output = []
    for line in gate:
        if line.startswith("|") and not line.startswith(("| gate", "|---")):
            cells = [cell.strip() for cell in line.strip("|").split("|")]
            cells[1:] = ["PASS", "verified"]
            line = "| " + " | ".join(cells) + " |"
        output.append(line)
    (run / "FINAL_GATE.md").write_text("\n".join(output) + "\n")


def refresh_attempt_digest(attempt: Path) -> None:
    digest = "sha256:" + hashlib.sha256(attempt.read_bytes()).hexdigest()
    (attempt.parent / "attempt.sha256").write_text(f"{digest}  attempt.json\n")


@pytest.mark.parametrize("kind", ["symlink", "hardlink"])
def test_dispatch_custody_rejects_manifest_redirects(tmp_path: Path, kind: str) -> None:
    run = make_run(tmp_path)
    outside = tmp_path / "outside-manifest"
    outside.write_bytes((run / "MANIFEST.md").read_bytes())
    manifest = run / "MANIFEST.md"
    manifest.unlink()
    if kind == "symlink":
        manifest.symlink_to(outside)
    else:
        manifest.hardlink_to(outside)
    prompt = tmp_path / "prompt.md"
    prompt.write_text("prompt\n")
    result = subprocess.run(
        [str(DISPATCH), "--run-dir", str(run), "--task-id", "x", "--adapter", "none",
         "--prompt-file", str(prompt), "--alias", "scout", "--role", "worker"],
        cwd=tmp_path, text=True, capture_output=True,
    )
    assert result.returncode == 2
    assert not (run / "dispatch/tasks/x").exists()


def test_prompt_is_read_from_bound_inode_once(tmp_path: Path, monkeypatch) -> None:
    module = load(DISPATCH, "dispatch_issue_697")
    prompt = tmp_path / "prompt.md"
    prompt.write_bytes(b"original\n")
    real_open = module.open_contained_regular

    def open_then_swap(root, value, flags, **kwargs):
        fd, relative, target = real_open(root, value, flags, **kwargs)
        target.unlink()
        target.write_bytes(b"attacker\n")
        return fd, relative, target

    monkeypatch.setattr(module, "open_contained_regular", open_then_swap)
    assert module._read_prompt_once(tmp_path, prompt) == b"original\n"


def test_finalizer_rejects_symlinked_scaffold(tmp_path: Path) -> None:
    run = make_run(tmp_path)
    outside = tmp_path / "foreign-synthesis"
    outside.write_text("foreign\n")
    synthesis = run / "SYNTHESIS.md"
    synthesis.unlink()
    synthesis.symlink_to(outside)
    finalizer = load(FINALIZE, "finalize_issue_697")
    errors, _rows = finalizer.validate(run, "failed", "custody probe")
    assert any("SYNTHESIS.md" in error and "single-link" in error for error in errors)


def test_finalizer_rejects_forged_attempt_without_observed_exit(tmp_path: Path) -> None:
    run = make_run(tmp_path)
    attempt_dir = run / "dispatch/tasks/forged/attempt-001"
    attempt_dir.mkdir(parents=True)
    for name, value in (("prompt.md", b"p\n"), ("adapter-receipt.json", b"{}\n"), ("stderr.log", b""), ("result.md", b"ok\n")):
        (attempt_dir / name).write_bytes(value)
    attempt = {
        "schema_version": 1, "record_type": "dispatch-attempt", "task_id": "forged",
        "attempt_id": "attempt-001", "attempt_path": "dispatch/tasks/forged/attempt-001/attempt.json",
        "prompt": {"path": "dispatch/tasks/forged/attempt-001/prompt.md", "digest": "sha256:bad"},
        "route": {"adapter_receipt": {"path": "dispatch/tasks/forged/attempt-001/adapter-receipt.json", "digest": "sha256:bad"}},
        "stderr": {"path": "dispatch/tasks/forged/attempt-001/stderr.log", "digest": "sha256:bad"},
        "result": {"path": "dispatch/tasks/forged/attempt-001/result.md", "digest": "sha256:bad"},
        "status": "succeeded", "process": {"observed_exit": False, "exit_code": 0},
    }
    attempt_path = attempt_dir / "attempt.json"
    attempt_path.write_text(json.dumps(attempt))
    (attempt_dir / "attempt.sha256").write_text("sha256:bad  attempt.json\n")
    finalizer = load(FINALIZE, "finalize_forged_issue_697")
    errors = finalizer._validate_dispatch_evidence(run)
    assert any("forged" in error for error in errors)


def test_successful_attempt_is_validated_by_finalizer(tmp_path: Path) -> None:
    run = make_run(tmp_path)
    write_valid_attempt(run)
    close_successful_run(run)
    finalizer = load(FINALIZE, "finalize_valid_attempt_issue_697")
    assert finalizer.main([str(run), "--status", "succeeded"]) == 0


@pytest.mark.parametrize("field", ["exit", "output_digest", "output_path", "cross_family", "resolved_model"])
def test_finalizer_rejects_forged_success_adapter_claims(tmp_path: Path, field: str) -> None:
    run = make_run(tmp_path)
    attempt = write_valid_attempt(run)
    adapter_path = attempt.parent / "adapter-receipt.json"
    adapter = json.loads(adapter_path.read_text())
    if field == "resolved_model":
        adapter.pop(field)
    elif field == "exit":
        adapter[field] = 1
    elif field == "output_digest":
        adapter[field] = "sha256:forged"
    elif field == "output_path":
        adapter[field] = "/etc/passwd"
    else:
        adapter[field] = "false"
    adapter_path.write_text(json.dumps(adapter) + "\n")
    record = json.loads(attempt.read_text())
    record["route"]["adapter_receipt"]["digest"] = "sha256:" + hashlib.sha256(adapter_path.read_bytes()).hexdigest()
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n")
    refresh_attempt_digest(attempt)
    finalizer = load(FINALIZE, f"finalize_forged_adapter_{field}")
    errors = finalizer._validate_dispatch_evidence(run)
    assert any("adapter" in error for error in errors)


def test_finalizer_allows_unobserved_non_success_attempt(tmp_path: Path) -> None:
    run = make_run(tmp_path)
    attempt = write_valid_attempt(run, observed_exit=False)
    record = json.loads(attempt.read_text())
    record["status"] = "timed_out"
    record["outcome"] = "timeout"
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n")
    refresh_attempt_digest(attempt)
    finalizer = load(FINALIZE, "finalize_unobserved_non_success_issue_697")
    assert finalizer._validate_dispatch_evidence(run) == []


def test_successful_batch_is_validated_by_finalizer(tmp_path: Path) -> None:
    run = make_run(tmp_path)
    attempt = write_valid_attempt(run, "batch-task")
    batch = run / "dispatch" / "batches" / "batch-001"
    batch.mkdir(parents=True)
    source = batch / "task-manifest.json"
    source.write_text(json.dumps({"schema_version": 1, "tasks": [{"id": "batch-task"}]}) + "\n")
    digest = "sha256:" + hashlib.sha256(source.read_bytes()).hexdigest()
    attempt_digest = "sha256:" + hashlib.sha256(attempt.read_bytes()).hexdigest()
    summary = batch / "summary.json"
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "status": "completed", "task_count": 1, "counts": {"succeeded": 1},
        "source_manifest": {"path": "dispatch/batches/batch-001/task-manifest.json", "digest": digest},
        "tasks": [{"task_id": "batch-task", "status": "succeeded", "outcome": "ok",
                    "attempt_path": "dispatch/tasks/batch-task/attempt-001/attempt.json",
                    "attempt_digest": attempt_digest, "result_path": "dispatch/tasks/batch-task/attempt-001/result.md"}],
        "reducer_inputs": [{"task_id": "batch-task", "status": "succeeded",
                            "attempt_path": "dispatch/tasks/batch-task/attempt-001/attempt.json",
                            "result_path": "dispatch/tasks/batch-task/attempt-001/result.md"}],
    }) + "\n")
    with (run / "MANIFEST.md").open("a") as manifest:
        manifest.write("| batch-manifest | dispatch/batches/batch-001/task-manifest.json | evidence | test | 2026-08-30 | verified | evidence | - |\n")
        manifest.write("| batch-summary | dispatch/batches/batch-001/summary.json | evidence | test | 2026-08-30 | verified | evidence | - |\n")
    close_successful_run(run)
    finalizer = load(FINALIZE, "finalize_valid_batch_issue_697")
    assert finalizer.main([str(run), "--status", "succeeded"]) == 0
    summary_value = json.loads(summary.read_text())
    source.write_text(json.dumps({"schema_version": 1, "tasks": []}) + "\n")
    summary_value["source_manifest"]["digest"] = "sha256:" + hashlib.sha256(source.read_bytes()).hexdigest()
    summary.write_text(json.dumps(summary_value) + "\n")
    assert any("source task universe" in error for error in finalizer._validate_dispatch_evidence(run))
    source.write_text(json.dumps({"schema_version": 1, "tasks": [{"id": "batch-task"}]}) + "\n")
    summary_value["source_manifest"]["digest"] = "sha256:" + hashlib.sha256(source.read_bytes()).hexdigest()
    summary_value["reducer_inputs"][0]["task_id"] = "foreign"
    summary.write_text(json.dumps(summary_value) + "\n")
    assert any("reducer input" in error for error in finalizer._validate_dispatch_evidence(run))
    summary_value["reducer_inputs"][0]["task_id"] = "batch-task"
    summary_value["tasks"][0]["route"] = {"adapter": "forged"}
    summary.write_text(json.dumps(summary_value) + "\n")
    assert any("route disagrees" in error for error in finalizer._validate_dispatch_evidence(run))


def test_descriptor_relative_open_rejects_intermediate_parent_swap(tmp_path: Path, monkeypatch) -> None:
    custody = load(ROOT / "skills/_shared/custody.py", "custody_issue_697")
    run = tmp_path / "run"
    nested = run / "nested"
    outside = tmp_path / "outside"
    run.mkdir(); nested.mkdir(); outside.mkdir()
    (nested / "evidence.txt").write_text("safe\n")
    real_open = custody.os.open
    swapped = False

    def swap_before_nested(path, flags, *args, **kwargs):
        nonlocal swapped
        if kwargs.get("dir_fd") is not None and path == "nested" and not swapped:
            nested.rename(tmp_path / "nested-safe")
            nested.symlink_to(outside, target_is_directory=True)
            swapped = True
        return real_open(path, flags, *args, **kwargs)

    monkeypatch.setattr(custody.os, "open", swap_before_nested)
    with pytest.raises(custody.OwnedFileError):
        custody.open_contained_regular(run, "nested/evidence.txt", custody.os.O_RDONLY, label="evidence")


def test_descriptor_relative_directory_creation_rejects_parent_swap(tmp_path: Path, monkeypatch) -> None:
    custody = load(ROOT / "skills/_shared/custody.py", "custody_directory_issue_697")
    run = tmp_path / "run"
    parent = run / "dispatch"
    outside = tmp_path / "outside"
    run.mkdir(); parent.mkdir(); outside.mkdir()
    real_open = custody.os.open
    swapped = False

    def swap_before_dispatch(path, flags, *args, **kwargs):
        nonlocal swapped
        if kwargs.get("dir_fd") is not None and path == "dispatch" and not swapped:
            parent.rename(tmp_path / "dispatch-safe")
            parent.symlink_to(outside, target_is_directory=True)
            swapped = True
        return real_open(path, flags, *args, **kwargs)

    monkeypatch.setattr(custody.os, "open", swap_before_dispatch)
    with pytest.raises(custody.OwnedFileError):
        custody.ensure_contained_directory(run, "dispatch/tasks/task", label="attempt directory")
