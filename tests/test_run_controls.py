"""Focused tests for the file-backed ``provenant run`` operator surface."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import stat
import subprocess
import textwrap
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/run_controls.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
CLI = ROOT / "scripts/provenant"


def make_run(tmp_path: Path) -> Path:
    return Path(subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / "controls")], text=True).strip())


def write_attempt(
    run_dir: Path,
    task_id: str = "task-1",
    attempt_id: str = "attempt-001",
    *,
    status: str = "succeeded",
    result: str | None = "result\n",
    question: dict[str, str] | None = None,
) -> Path:
    attempt_dir = run_dir / "dispatch/tasks" / task_id / attempt_id
    attempt_dir.mkdir(parents=True)
    prompt_path = attempt_dir / "prompt.md"
    prompt_path.write_text("prompt\n", encoding="utf-8")
    stderr_path = attempt_dir / "stderr.log"
    stderr_path.write_text("diagnostic\n", encoding="utf-8")
    adapter_path = attempt_dir / "adapter-receipt.json"
    adapter_path.write_text("{}\n", encoding="utf-8")
    result_path = attempt_dir / "result.md"
    result_ref = None
    if result is not None:
        result_path.write_text(result, encoding="utf-8")
        result_ref = {"path": str(result_path.relative_to(run_dir)), "digest": file_digest(result_path)}
    record = {
        "schema_version": 1,
        "record_type": "dispatch-attempt",
        "run_id": run_dir.name,
        "task_id": task_id,
        "attempt_id": attempt_id,
        "status": status,
        "outcome": "ok" if status == "succeeded" else status,
        "requested_route": {
            "intent": "ordinary", "adapter": "codex", "alias": "scout",
            "task_class": "", "model": "", "role": "worker", "orchestrator_family": "openai",
        },
        "route": {"adapter_receipt": {"path": str(adapter_path.relative_to(run_dir)), "digest": file_digest(adapter_path)}},
        "prompt": {"path": str(prompt_path.relative_to(run_dir)), "digest": file_digest(prompt_path)},
        "stderr": {"path": str(stderr_path.relative_to(run_dir)), "digest": file_digest(stderr_path)},
        "result": result_ref,
        "process": {"observed_exit": True, "exit_code": 0},
        "started_at": "2026-08-29T00:00:00.000Z",
        "finished_at": "2026-08-29T00:00:01.000Z",
    }
    if question is not None:
        record["question"] = question
    attempt_path = attempt_dir / "attempt.json"
    record["attempt_path"] = str(attempt_path.relative_to(run_dir))
    attempt_path.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    digest_path = attempt_dir / "attempt.sha256"
    digest_path.write_text(f"{file_digest(attempt_path)}  attempt.json\n", encoding="utf-8")
    return attempt_path


def file_digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def invoke(*args: str, cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    merged = os.environ.copy()
    if env:
        merged.update(env)
    return subprocess.run([str(CLI), *args], cwd=cwd, env=merged, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def test_inspect_reports_exact_attempt_artifacts_without_reconciling_manifest(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir)
    before = (run_dir / "MANIFEST.md").read_bytes()

    result = invoke("run", "inspect", "--run-dir", str(run_dir), cwd=tmp_path)

    assert result.returncode == 0, result.stderr + result.stdout
    payload = json.loads(result.stdout)
    assert payload["tasks"][0]["task_id"] == "task-1"
    assert payload["tasks"][0]["attempts"][0]["attempt_id"] == "attempt-001"
    assert payload["tasks"][0]["attempts"][0]["artifacts"]["result"].endswith("result.md")
    assert (run_dir / "MANIFEST.md").read_bytes() == before


def test_artifact_commands_require_exact_attempt_and_emit_raw_content(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir)

    result = invoke("run", "result", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001", cwd=tmp_path)

    assert result.returncode == 0
    assert result.stdout == "result\n"


def test_artifact_command_rejects_tampered_retained_digest(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir)
    (run_dir / "dispatch/tasks/task-1/attempt-001/result.md").write_text("tampered\n", encoding="utf-8")

    result = invoke("run", "result", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001", cwd=tmp_path)

    assert result.returncode == 2
    assert "digest does not match" in json.loads(result.stdout)["message"]


def test_retry_requires_one_route_choice_and_delegates_parent(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir, status="failed", result=None)
    fake = tmp_path / "dispatch-run"
    invoked = tmp_path / "invoked.json"
    fake.write_text(
        f"#!/usr/bin/env python3\nimport json, sys\njson.dump(sys.argv[1:], open({str(invoked)!r}, 'w'))\n",
        encoding="utf-8",
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)

    module_spec = importlib.util.spec_from_file_location("run_controls_under_test", SCRIPT)
    assert module_spec and module_spec.loader
    module = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(module)
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    args = module.parser().parse_args([
        "retry", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001", "--same-route",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.run(args) == 0
    delegated = json.loads(invoked.read_text(encoding="utf-8"))
    assert "--retry-of" in delegated
    assert delegated[delegated.index("--retry-of") + 1] == "attempt-001"
    assert delegated[delegated.index("--prompt-file") + 1].endswith("prompt.md")


def test_retry_reroute_requires_a_complete_new_route(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir, status="failed", result=None)

    result = invoke("run", "retry", "--run-dir", str(run_dir), "--task-id", "task-1",
                    "--attempt-id", "attempt-001", "--reroute", "--adapter", "gemini", cwd=tmp_path)

    assert result.returncode == 2
    assert "route selector" in json.loads(result.stdout)["message"]


def test_blocked_inspection_exposes_question_and_continuation_reference(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir, status="blocked", result=None,
                  question={"code": "needs_input", "prompt": "Which source?"})

    result = invoke("run", "inspect", "--run-dir", str(run_dir), "--task-id", "task-1",
                    "--attempt-id", "attempt-001", cwd=tmp_path)

    assert result.returncode == 0
    attempt = json.loads(result.stdout)["tasks"][0]["attempts"][0]
    assert attempt["question"]["prompt"] == "Which source?"
    assert attempt["continuation_ref"] == "dispatch/tasks/task-1/attempt-001/attempt.json"


def test_reduce_requires_explicit_successes_and_names_batch_omissions(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    run_dir = make_run(tmp_path)
    selected = write_attempt(run_dir, task_id="one")
    omitted = write_attempt(run_dir, task_id="two")
    failed = write_attempt(run_dir, task_id="three", status="failed", result=None)
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1,
        "record_type": "dispatch-batch",
        "tasks": [
            {"task_id": "one", "status": "succeeded", "attempt_path": str(selected.relative_to(run_dir))},
            {"task_id": "two", "status": "succeeded", "attempt_path": str(omitted.relative_to(run_dir))},
            {"task_id": "three", "status": "failed", "attempt_path": str(failed.relative_to(run_dir))},
        ],
    }, sort_keys=True) + "\n", encoding="utf-8")
    prompt = tmp_path / "reduce.md"
    prompt.write_text("Summarise the evidence.\n", encoding="utf-8")
    fake = tmp_path / "dispatch-run"
    captured = tmp_path / "captured.json"
    captured_prompt = tmp_path / "captured-prompt.md"
    fake.write_text(
        f"#!/usr/bin/env python3\nimport json,sys,shutil\njson.dump(sys.argv[1:],open({str(captured)!r},'w'))\nshutil.copyfile(sys.argv[sys.argv.index('--prompt-file')+1], {str(captured_prompt)!r})\n",
        encoding="utf-8",
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    spec = importlib.util.spec_from_file_location("run_controls_reduce", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "reduce", "--run-dir", str(run_dir), "--task-id", "reducer", "--prompt-file", str(prompt),
        "--input", "one/attempt-001", "--adapter", "codex", "--alias", "scout",
    ])

    assert module.run(args) == 0
    delegated = json.loads(captured.read_text(encoding="utf-8"))
    reducer_prompt = captured_prompt
    assert reducer_prompt.exists()
    reducer_text = reducer_prompt.read_text(encoding="utf-8")
    assert "- two" in reducer_text and "- three: failed" in reducer_text
    assert "Summarise the evidence." in reducer_text
    assert "--task-id" in delegated and delegated[delegated.index("--task-id") + 1] == "reducer"
