"""Contract tests for the ordinary one-attempt dispatch owner."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import textwrap
import time
import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/dispatch_run.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
FINALIZE = ROOT / "skills/orchestrate/scripts/run_dir_finalize.py"


def write_executable(path: Path, body: str) -> None:
    path.write_text(textwrap.dedent(body), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


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
        """
        #!/usr/bin/env bash
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
    write_executable(adapter, """#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in --out) out="$2"; shift 2;; *) shift;; esac
        done
        printf '{\"status\":\"ok\"}\n'
        printf 'OK\n' > "$out"
    """)
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
    write_executable(adapter, """#!/usr/bin/env bash
        while [ "$#" -gt 0 ]; do
          case "$1" in --out) out="$2"; shift 2;; *) shift;; esac
        done
        printf '{\"status\":\"ok\"}\n'
        printf 'OK\n' > "$out"
    """)
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
