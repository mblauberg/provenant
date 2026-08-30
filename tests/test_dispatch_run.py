"""Contract tests for the ordinary one-attempt dispatch owner."""

from __future__ import annotations

import hashlib
import io
import importlib.util
import json
import os
import signal
import stat
import subprocess
import sys
import textwrap
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/dispatch_run.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
FINALIZE = ROOT / "skills/orchestrate/scripts/run_dir_finalize.py"


def write_executable(path: Path, body: str) -> None:
    path.write_text(textwrap.dedent(body), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


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


def make_run(tmp_path: Path, name: str) -> Path:
    return Path(
        subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / name)], text=True).strip()
    ).resolve()


def load_dispatch_module():
    spec = importlib.util.spec_from_file_location("dispatch_run_under_test", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_ordinary_single_dispatch_records_one_attempt_and_route_identity(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "one")
    receipt_before = (run_dir / "RUN_RECEIPT.json").read_bytes()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "codex",
        """#!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        cat >/dev/null
        printf 'OK\\n'
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"

    result = subprocess.run(
        [
            str(SCRIPT),
            "--run-dir",
            str(run_dir),
            "--task-id",
            "task-1",
            "--adapter",
            "codex",
            "--prompt-file",
            str(prompt),
            "--orchestrator-family",
            "openai",
            "--alias",
            "workhorse",
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
    assert receipt["status"] == "succeeded"
    attempt = run_dir / "dispatch" / "tasks" / "task-1" / "attempt-001" / "attempt.json"
    assert attempt.exists()
    record = json.loads(attempt.read_text(encoding="utf-8"))
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        ("Do you agree?\n", "succeeded", "ok"),
        ("```json\n{\"schema_version\":1,\"record_type\":\"provenant-worker-terminal\",\"classification\":\"question\",\"question\":{\"code\":\"needs_input\",\"prompt\":\"x\"}}\n```\n", "succeeded", "ok"),
        ('{"record_type":"other","question":"quoted?"}\n', "succeeded", "ok"),
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == (0 if status == "succeeded" else 1)
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == status
    assert attempt["outcome"] == outcome
    if status == "succeeded":
        assert "question" not in attempt


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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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

    with pytest.raises(module.TerminalEnvelopeIntegrityError):
        module.worker_question_envelope(result, "sha256:not-the-result")
    assert module.worker_question_envelope(result, module.digest(result)) == {
        "code": "needs_input", "prompt": "Which source?"
    }
    result.write_bytes(envelope + b"x" * (module.MAX_WORKER_TERMINAL_ENVELOPE_BYTES + 1))
    assert module.worker_question_envelope(result, module.digest(result)) is None


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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == "failed"
    assert attempt["outcome"] == "result_integrity_error"


def test_ordinary_dispatch_without_lead_family_is_not_certification(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "no-family")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(bin_dir / "codex", """#!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        cat >/dev/null
        printf 'OK\\n'
        """)
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "no-family",
         "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    record = json.loads(result.stdout)
    assert record["route"]["orchestrator_family"] == ""
    assert record["route"]["cross_family"] is False
    assert record["route"]["certification_eligible"] is False


def test_batch_child_defers_shared_manifest_append(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "batch-child")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("batch child\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "deferred", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "scout", "--role", "worker",
        "--risk-tier", "substantial", "--reviewer-id", "reviewer-1", "--effort", "high", "--batch-child",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 0
    assert "dispatch-deferred" not in (run_dir / "MANIFEST.md").read_text(encoding="utf-8")
    attempt = json.loads((run_dir / "dispatch/tasks/deferred/attempt-001/attempt.json").read_text(encoding="utf-8"))
    assert attempt["requested_route"]["risk_tier"] == "substantial"
    assert attempt["requested_route"]["reviewer_id"] == "reviewer-1"
    assert attempt["requested_route"]["effort"] == "high"


def test_prompt_stdin_is_retained_by_dispatch_owner(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "stdin")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "stdin-task", "--adapter", "codex",
        "--prompt-stdin", "--alias", "scout", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(sys, "stdin", io.TextIOWrapper(io.BytesIO(b"stdin prompt\n")))

    assert module.dispatch(args) == 0
    attempt = run_dir / "dispatch/tasks/stdin-task/attempt-001"
    assert (attempt / "prompt.md").read_bytes() == b"stdin prompt\n"


def test_route_failure_is_typed_and_provider_is_not_invoked(tmp_path: Path) -> None:
    run_dir = Path(
        subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / "route")], text=True).strip()
    ).resolve()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("route failure\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    invoked = tmp_path / "invoked"
    write_executable(
        bin_dir / "codex",
        f"""
        #!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{{"models":[{{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
          exit 0
        fi
        touch {invoked}
        exit 9
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"

    result = subprocess.run(
        [
            str(SCRIPT),
            "--run-dir", str(run_dir), "--task-id", "route-failure",
            "--adapter", "codex", "--prompt-file", str(prompt),
            "--alias", "does-not-exist", "--role", "worker",
        ], cwd=tmp_path, env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["failure_code"] == "unknown_alias"
    assert record["route"]["status"] == "unknown_alias"
    assert not invoked.exists()
    assert record["process"]["observed_exit"] is True


def test_nonzero_provider_exit_is_recorded_without_substitution(tmp_path: Path) -> None:
    run_dir = Path(
        subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / "exit")], text=True).strip()
    ).resolve()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("provider failure\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "codex",
        """#!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        cat >/dev/null
        echo "provider failed" >&2
        exit 9
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"

    result = subprocess.run(
        [
            str(SCRIPT),
            "--run-dir", str(run_dir), "--task-id", "provider-failure",
            "--adapter", "codex", "--prompt-file", str(prompt),
            "--alias", "workhorse", "--role", "worker",
        ], cwd=tmp_path, env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["status"] == "failed"
    assert record["failure_code"] == "error"
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 1
    record = json.loads(
        (run_dir / "dispatch/tasks/incomplete-success/attempt-001/attempt.json").read_text()
    )
    assert record["failure_code"] == "adapter_receipt_invalid"


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
            "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        sleep 10
        """,
    )
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "timeout", "--adapter", "codex",
         "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker", "--timeout", "0.1"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert result.returncode != 0
    record = json.loads(result.stdout)
    assert record["status"] == "timed_out"
    assert record["failure_code"] == "timeout"
    assert record["process"]["observed_exit"] is True
    assert record["process"]["exit_code"] is not None


def test_sigterm_cancels_and_reaps_provider_group(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "cancel")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("cancel\n", encoding="utf-8")
    provider_pid_path = tmp_path / "provider.pid"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "codex",
        """#!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
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
         "--alias", "workhorse", "--role", "worker", "--timeout", "30"],
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
    assert record["status"] == "cancelled"
    assert record["failure_code"] == "cancelled"
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
        original = module.atomic_write
        fired = False
        def publish(path, content):
            global fired
            original(path, content)
            if path.name == "attempt.json" and not fired:
                fired = True
                os.kill(os.getpid(), signal.SIGTERM)
        module.atomic_write = publish
        raise SystemExit(module.dispatch(module.parser().parse_args(sys.argv[1:])))
    """), encoding="utf-8")
    result = subprocess.run([
        sys.executable, str(driver), "--run-dir", str(run_dir), "--task-id", "late",
        "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ], cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    assert result.returncode == 0, result.stderr + result.stdout
    record = json.loads(result.stdout)
    assert record["status"] == "succeeded"
    assert record["process"]["observed_exit"] is True
    assert (run_dir / "dispatch/tasks/late/attempt-001/attempt.json").is_file()


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
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{{"models":[{{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
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
         "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker", "--timeout", "30"],
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
        [str(ROOT / "scripts/provenant"), "run", "cancel", "--run-dir", str(run_dir),
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    assert module.dispatch(args) == 0
    stale = run_dir / "dispatch/tasks/stale/attempt-001/cancel.request"
    stale.touch()

    assert module.dispatch(args) == 0
    next_record = json.loads((run_dir / "dispatch/tasks/stale/attempt-002/attempt.json").read_text())
    assert next_record["status"] == "succeeded"


def test_attempt_rows_are_accepted_by_existing_finalizer(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "finalizer")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("finalizer\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(bin_dir / "codex", """#!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        cat >/dev/null
        printf 'OK\\n'
        """)
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", "finalizer", "--adapter", "codex",
         "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker"],
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
         "--alias", "workhorse", "--role", "worker"],
        cwd=tmp_path, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert rejected.returncode == 2
    assert json.loads(rejected.stdout)["status"] == "run_custody_closed"
    assert not (run_dir / "dispatch/tasks/after-close").exists()


def test_counterfeit_active_receipt_is_rejected_before_launch(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "counterfeit-receipt")
    (run_dir / "RUN_RECEIPT.json").write_text(
        '{"status":"active","closed_at":null}\n', encoding="utf-8"
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("counterfeit\n", encoding="utf-8")
    adapter = tmp_path / "adapter-never-run-receipt"
    write_executable(adapter, "#!/usr/bin/env bash\nexit 99\n")
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "blocked", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert not (run_dir / "dispatch/tasks/blocked").exists()


def test_hard_linked_prompt_is_rejected_before_provider_launch(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "hardlink")
    source = tmp_path / "prompt.md"
    source.write_text("secret boundary\n", encoding="utf-8")
    linked = tmp_path / "linked-prompt.md"
    linked.hardlink_to(source)
    module = load_dispatch_module()
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "hardlink", "--adapter", "codex",
        "--prompt-file", str(linked), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 2


def test_nonfinite_or_nonpositive_timeout_is_rejected() -> None:
    module = load_dispatch_module()
    for value in ("0", "-1", "nan", "inf", "-inf"):
        try:
            module.parser().parse_args([
                "--run-dir", "/tmp", "--adapter", "codex", "--prompt-file", "/tmp/prompt",
                "--alias", "workhorse", "--role", "worker", "--timeout", value,
            ])
        except SystemExit as exc:
            assert exc.code == 2
        else:
            raise AssertionError(f"accepted invalid timeout {value}")


def test_manifest_appendability_failure_is_typed_before_launch(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "manifest-readonly")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("manifest\n", encoding="utf-8")
    module = load_dispatch_module()

    def refuse(_run_dir):
        raise OSError("read-only fixture")

    monkeypatch.setattr(module, "ensure_manifest_appendable", refuse)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "manifest-readonly", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 2


def test_read_only_manifest_is_rejected_before_launch(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "manifest-mode")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("manifest mode\n", encoding="utf-8")
    manifest = run_dir / "MANIFEST.md"
    original_mode = manifest.stat().st_mode
    manifest.chmod(0o444)
    try:
        try:
            with manifest.open("a", encoding="utf-8"):
                pass
        except OSError:
            module = load_dispatch_module()
            args = module.parser().parse_args([
                "--run-dir", str(run_dir), "--task-id", "manifest-mode", "--adapter", "codex",
                "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
            ])
            monkeypatch.chdir(tmp_path)
            assert module.dispatch(args) == 2
        else:
            pytest.skip("test user can append to chmod 0444 files")
    finally:
        manifest.chmod(original_mode)


def test_manifest_append_failure_retains_terminal_attempt(tmp_path: Path, monkeypatch) -> None:
    run_dir = make_run(tmp_path, "manifest-write")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("manifest write\n", encoding="utf-8")
    adapter = tmp_path / "adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    module.CF_DISPATCH = adapter
    monkeypatch.setattr(module, "append_manifest", lambda *_: (_ for _ in ()).throw(OSError("append failed")))
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "manifest-write", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 1
    attempt = run_dir / "dispatch/tasks/manifest-write/attempt-001/attempt.json"
    record = json.loads(attempt.read_text(encoding="utf-8"))
    assert record["status"] == "failed"
    assert record["failure_code"] == "manifest_write_error"
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
        "--retry-of", "attempt-001",
    ])
    assert module.dispatch(retry_args) == 0
    text = manifest.read_text(encoding="utf-8")
    assert "dispatch-reconcile-attempt-001-attempt" in text
    second = json.loads((run_dir / "dispatch/tasks/reconcile/attempt-002/attempt.json").read_text())
    assert second["retry_of"] == "attempt-001"


def test_attempt_number_and_retry_lineage_continue_past_999(tmp_path: Path, monkeypatch) -> None:
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "finished_at": "2026-08-29T00:00:00Z",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == 2
    assert list(outside.iterdir()) == []
