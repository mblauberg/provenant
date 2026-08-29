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
            "risk_tier": "substantial", "reviewer_id": "reviewer-1", "effort": "high",
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


def write_dispatch_wrapper(path: Path, adapter: Path) -> None:
    path.write_text(
        f"#!/usr/bin/env python3\nimport sys\nfrom pathlib import Path\nsys.path.insert(0, {str(ROOT / 'skills/orchestrate/scripts')!r})\nimport dispatch_run\ndispatch_run.CF_DISPATCH = Path({str(adapter)!r})\nsys.argv = [{str(ROOT / 'skills/orchestrate/scripts/dispatch_run.py')!r}, *sys.argv[1:]]\nraise SystemExit(dispatch_run.dispatch(dispatch_run.parser().parse_args()))\n",
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


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


def test_artifact_command_rejects_attempt_copied_from_another_run(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    attempt = write_attempt(run_dir)
    record = json.loads(attempt.read_text(encoding="utf-8"))
    record["run_id"] = "different-run"
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    (attempt.parent / "attempt.sha256").write_text(f"{file_digest(attempt)}  attempt.json\n", encoding="utf-8")

    result = invoke("run", "result", "--run-dir", str(run_dir), "--task-id", "task-1", "--attempt-id", "attempt-001", cwd=tmp_path)

    assert result.returncode == 2
    assert "invalid identity" in json.loads(result.stdout)["message"]


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
    assert "--prompt-stdin" in delegated
    assert delegated[delegated.index("--risk-tier") + 1] == "substantial"
    assert delegated[delegated.index("--reviewer-id") + 1] == "reviewer-1"
    assert delegated[delegated.index("--effort") + 1] == "high"


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


def test_inspect_keeps_preterminal_batch_task_conservative_when_receipt_is_absent(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "status": "cancelled", "tasks": [{"task_id": "not-started", "status": "cancelled"}],
    }) + "\n", encoding="utf-8")

    result = invoke("run", "inspect", "--run-dir", str(run_dir), cwd=tmp_path)

    assert result.returncode == 0
    batch_task = json.loads(result.stdout)["batches"][0]["tasks"][0]
    assert batch_task == {"task_id": "not-started", "status": "incomplete", "claimed_status": "cancelled",
                          "receipt_status": "receipt_unavailable"}


def test_inspect_does_not_publish_terminal_batch_claim_without_valid_attempt(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "status": "failed", "tasks": [{"task_id": "missing", "status": "failed",
                                           "attempt_path": "dispatch/tasks/missing/attempt-001/attempt.json"}],
    }) + "\n", encoding="utf-8")

    result = invoke("run", "inspect", "--run-dir", str(run_dir), cwd=tmp_path)

    assert result.returncode == 0
    task = json.loads(result.stdout)["batches"][0]["tasks"][0]
    assert task["status"] == "incomplete" and task["claimed_status"] == "failed"


def test_inspect_rejects_non_string_batch_attempt_path(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "status": "failed", "tasks": [{"task_id": "missing", "status": "failed", "attempt_path": 7}],
    }) + "\n", encoding="utf-8")

    result = invoke("run", "inspect", "--run-dir", str(run_dir), cwd=tmp_path)

    assert result.returncode == 2
    assert "attempt path is not a string" in json.loads(result.stdout)["message"]


def test_inspect_rejects_invalid_batch_task_id(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    summary = run_dir / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "status": "failed", "tasks": [{"task_id": "../outside", "status": "failed"}],
    }) + "\n", encoding="utf-8")

    result = invoke("run", "inspect", "--run-dir", str(run_dir), cwd=tmp_path)

    assert result.returncode == 2
    assert "task universe" in json.loads(result.stdout)["message"]


def test_inspect_reports_incomplete_attempt_directory_without_inventing_liveness(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    (run_dir / "dispatch/tasks/task-1/attempt-001").mkdir(parents=True)

    result = invoke("run", "inspect", "--run-dir", str(run_dir), cwd=tmp_path)

    assert result.returncode == 0
    attempt = json.loads(result.stdout)["tasks"][0]["attempts"][0]
    assert attempt == {
        "attempt_id": "attempt-001", "status": "incomplete", "receipt_status": "unavailable",
        "artifacts": {"attempt": "dispatch/tasks/task-1/attempt-001/attempt.json"},
    }


def test_evidence_reader_rejects_intermediate_directory_symlink(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "evidence.txt").write_text("secret", encoding="utf-8")
    (run_dir / "dispatch").mkdir()
    (run_dir / "dispatch/link").symlink_to(outside, target_is_directory=True)
    spec = importlib.util.spec_from_file_location("run_controls_reader", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    with pytest.raises(module.ControlError):
        module._read_regular(run_dir, "dispatch/link/evidence.txt", "evidence")


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
        "batch_id": "batch-001",
        "tasks": [
            {"task_id": "one", "status": "succeeded", "attempt_path": str(selected.relative_to(run_dir)),
             "result_path": "dispatch/tasks/one/attempt-001/result.md"},
            {"task_id": "two", "status": "succeeded", "attempt_path": str(omitted.relative_to(run_dir)),
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
    spec = importlib.util.spec_from_file_location("run_controls_reduce", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "DISPATCH_RUN", fake)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "reduce", "--run-dir", str(run_dir), "--task-id", "reducer", "--prompt-file", str(prompt),
        "--batch-id", "batch-001", "--input", "one/attempt-001", "--adapter", "codex", "--alias", "scout",
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


def test_reduce_does_not_conflate_repeated_task_across_batch_summaries(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    first = write_attempt(run_dir, task_id="same", attempt_id="attempt-001")
    second = write_attempt(run_dir, task_id="same", attempt_id="attempt-002")
    for batch_id, attempt in (("batch-001", first), ("batch-002", second)):
        summary = run_dir / "dispatch/batches" / batch_id / "summary.json"
        summary.parent.mkdir(parents=True, exist_ok=True)
        summary.write_text(json.dumps({
            "schema_version": 1, "record_type": "dispatch-batch", "batch_id": batch_id,
            "tasks": [{"task_id": "same", "status": "succeeded",
                       "attempt_path": str(attempt.relative_to(run_dir)),
                       "result_path": str(attempt.parent / "result.md").replace(str(run_dir) + "/", "")}],
        }) + "\n", encoding="utf-8")
    prompt = tmp_path / "reduce.md"
    prompt.write_text("reduce", encoding="utf-8")

    result = invoke("run", "reduce", "--run-dir", str(run_dir), "--task-id", "reducer",
                    "--prompt-file", str(prompt), "--batch-id", "batch-001",
                    "--input", "same/attempt-002", "--adapter", "codex", "--alias", "scout", cwd=tmp_path)

    assert result.returncode == 2
    assert "selected batch" in json.loads(result.stdout)["message"]


def test_retry_and_reduce_use_real_dispatch_owner_and_retain_new_attempts(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    adapter = tmp_path / "adapter"
    dispatch = tmp_path / "dispatch-wrapper"
    write_success_adapter(adapter)
    write_dispatch_wrapper(dispatch, adapter)
    module_spec = importlib.util.spec_from_file_location("run_controls_e2e", SCRIPT)
    assert module_spec and module_spec.loader
    module = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(module)
    monkeypatch.setattr(module, "DISPATCH_RUN", dispatch)

    retry_run = make_run(tmp_path / "retry")
    parent = write_attempt(retry_run, status="failed", result=None)
    parent_before = parent.read_bytes()
    monkeypatch.chdir(tmp_path / "retry")
    retry_args = module.parser().parse_args([
        "retry", "--run-dir", str(retry_run), "--task-id", "task-1", "--attempt-id", "attempt-001", "--same-route",
    ])
    assert module.run(retry_args) == 0
    assert parent.read_bytes() == parent_before
    retry_record = json.loads((retry_run / "dispatch/tasks/task-1/attempt-002/attempt.json").read_text(encoding="utf-8"))
    assert retry_record["retry_of"] == "attempt-001" and retry_record["status"] == "succeeded"

    reduce_root = tmp_path / "reduce"
    reduce_run = make_run(reduce_root)
    selected = write_attempt(reduce_run, task_id="source")
    summary = reduce_run / "dispatch/batches/batch-001/summary.json"
    summary.parent.mkdir(parents=True)
    summary.write_text(json.dumps({
        "schema_version": 1, "record_type": "dispatch-batch", "batch_id": "batch-001",
        "tasks": [{"task_id": "source", "status": "succeeded",
                   "attempt_path": str(selected.relative_to(reduce_run)),
                   "result_path": "dispatch/tasks/source/attempt-001/result.md"}],
    }) + "\n", encoding="utf-8")
    prompt = reduce_root / "reduce.md"
    prompt.write_text("Reduce these results.", encoding="utf-8")
    monkeypatch.chdir(reduce_root)
    reduce_args = module.parser().parse_args([
        "reduce", "--run-dir", str(reduce_run), "--task-id", "reducer", "--prompt-file", str(prompt),
        "--batch-id", "batch-001", "--input", "source/attempt-001", "--adapter", "codex", "--alias", "scout",
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
        "--batch-id", "batch-001", "--input", "source/attempt-001", "--adapter", "codex", "--alias", "scout",
    ])
    assert module.run(same_dir_args) == 0
    assert (reduce_run / "dispatch/tasks/reducer2/attempt-001/prompt.md").is_file()
    assert not list(reduce_run.glob(".provenant-reducer-*.md"))


@pytest.mark.parametrize("name", [".env", ".ssh/known_hosts"])
def test_reduce_rejects_credential_or_auth_prompt_before_read(tmp_path: Path, name: str) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir, task_id="one")
    prompt = tmp_path / name
    prompt.parent.mkdir(parents=True, exist_ok=True)
    prompt.write_text("secret prompt", encoding="utf-8")

    result = invoke("run", "reduce", "--run-dir", str(run_dir), "--task-id", "reducer",
                    "--prompt-file", str(prompt), "--batch-id", "batch-001",
                    "--input", "one/attempt-001", "--adapter", "codex", "--alias", "scout", cwd=tmp_path)

    assert result.returncode == 2
    assert "credential or authentication" in json.loads(result.stdout)["message"]


def test_reduce_rejects_original_prompt_symlink_before_resolving(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path)
    write_attempt(run_dir, task_id="one")
    target = tmp_path / "ordinary-prompt.md"
    target.write_text("prompt", encoding="utf-8")
    supplied = tmp_path / "prompt-link.md"
    supplied.symlink_to(target)

    result = invoke("run", "reduce", "--run-dir", str(run_dir), "--task-id", "reducer",
                    "--prompt-file", str(supplied), "--batch-id", "batch-001",
                    "--input", "one/attempt-001", "--adapter", "codex", "--alias", "scout", cwd=tmp_path)

    assert result.returncode == 2
    assert "must not be a symlink" in json.loads(result.stdout)["message"]
