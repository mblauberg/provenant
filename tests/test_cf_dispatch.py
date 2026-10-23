#!/usr/bin/env python3
"""Contract tests for cf_dispatch.py, the provider adapter, with stubbed CLIs."""
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
from pathlib import Path

import pytest

from _shared.bounded_process import run_bounded
from tests.cf_dispatch_support import (
    PRODUCT_ROOT,
    RUN_DIR_SCRIPT,
    SCRIPT,
    SCRIPTS,
    fabric_free_env,
)
from tests.support.fake_cli import fake_bin, fake_codex, write_executable
from tests.support.proc import run_cli, run_cf_dispatch

DISPATCH_SCHEMA = {
    "tool",
    "adapter",
    "model",
    "requested_model",
    "fallback_model",
    "effort",
    "requested_effort",
    "effort_source",
    "effort_capability_source",
    "effort_substitution",
    "substitution",
    "status",
    "reason",
    "exit",
    "output_path",
    "output_digest",
    "read_only_guarantee",
    "provider_sandbox",
    "provider_network",
    "orchestrator_family",
    "provider_family",
    "endpoint_provider",
    "model_family",
    "resolved_model",
    "identity_source",
    "catalog_model",
    "model_selection",
    "route_alias",
    "reviewer_id",
    "risk_tier",
    "model_override_tier",
    "policy_override",
    "certification_eligible",
    "cross_family",
}


def test_output_install_replaces_symlink_without_overwriting_target(tmp_path):
    root = tmp_path
    bin_dir = fake_bin(root)
    fake_codex(bin_dir, "cat >/dev/null\nprintf 'safe output\n'", models={'gpt-6-luna': ['high']}, mcp=False)
    prompt = root / "prompt.md"
    prompt.write_text("test\n", encoding="utf-8")
    outside = root / "outside"
    outside.write_text("unchanged\n", encoding="utf-8")
    out = root / "result.md"
    out.symlink_to(outside)
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'codex', '--prompt-file', str(prompt), '--out', str(out), '--alias', 'senior', '--role', 'worker'], cwd=root, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    assert result.returncode == 0, result.stderr + result.stdout
    assert outside.read_text(encoding="utf-8") == "unchanged\n"
    assert not out.is_symlink()
    assert out.read_text(encoding="utf-8") == "safe output\n"
    record = json.loads(result.stdout)
    assert record["output_digest"] == "sha256:" + __import__("hashlib").sha256(
        out.read_bytes()
    ).hexdigest()


def test_directory_symlink_output_is_rejected_without_escaping(tmp_path):
    root = tmp_path
    bin_dir = fake_bin(root)
    fake_codex(bin_dir, "cat >/dev/null\nprintf 'safe output\n'", models={'gpt-6.1-sol': ['high']}, mcp=False)
    prompt = root / "prompt.md"
    prompt.write_text("test\n", encoding="utf-8")
    outside = root / "outside"
    outside.mkdir()
    out = root / "result.md"
    out.symlink_to(outside, target_is_directory=True)
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'codex', '--prompt-file', str(prompt), '--out', str(out), '--alias', 'senior', '--role', 'worker'], cwd=root, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "output_write_error"
    assert record["output_path"] == ""
    assert record["output_digest"] == ""
    assert record["certification_eligible"] is False
    assert list(outside.iterdir()) == []


def test_symlinked_output_parent_is_rejected_without_escaping(tmp_path):
    root = tmp_path
    bin_dir = fake_bin(root)
    fake_codex(bin_dir, "cat >/dev/null\nprintf 'MUST STAY INSIDE\n'", models={'gpt-6-luna': ['high']}, mcp=False)
    prompt = root / "prompt.md"
    prompt.write_text("test\n", encoding="utf-8")
    outside = root / "outside"
    outside.mkdir()
    linked = root / "linked"
    linked.symlink_to(outside, target_is_directory=True)
    out = linked / "result.md"
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'codex', '--prompt-file', str(prompt), '--out', str(out), '--alias', 'senior', '--role', 'worker'], cwd=root, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "output_write_error"
    assert record["output_path"] == ""
    assert record["output_digest"] == ""
    assert record["certification_eligible"] is False
    assert list(outside.iterdir()) == []


def test_output_parent_swap_cannot_certify_an_identical_outside_file(tmp_path):
    root = tmp_path
    bin_dir = fake_bin(root)
    payload = root / "payload"
    payload.write_bytes(b"A" * (16 * 1024 * 1024))
    fake_codex(bin_dir, textwrap.dedent("""\
        cat >/dev/null
        cat "$CF_DISPATCH_TEST_PAYLOAD"
        """), models={'gpt-6.1-sol': ['high']}, mcp=False)
    prompt = root / "prompt.md"
    prompt.write_text("test\n", encoding="utf-8")
    safe_parent = root / "safe"
    safe_parent.mkdir()
    detached_parent = root / "detached"
    outside_parent = root / "outside"
    outside_parent.mkdir()
    outside_output = outside_parent / "result.md"
    outside_output.write_bytes(payload.read_bytes())
    out = safe_parent / "result.md"
    swapped = threading.Event()

    def swap_parent_after_install():
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            try:
                if out.is_file() and out.stat().st_size == payload.stat().st_size:
                    safe_parent.rename(detached_parent)
                    safe_parent.symlink_to(outside_parent, target_is_directory=True)
                    swapped.set()
                    return
            except FileNotFoundError:
                pass
            time.sleep(0.001)

    watcher = threading.Thread(target=swap_parent_after_install)
    watcher.start()
    env = fabric_free_env()
    env["CF_DISPATCH_TEST_PAYLOAD"] = str(payload)
    result = run_cli([str(SCRIPT), '--intent', 'assurance', '--tool', 'codex', '--orchestrator-family', 'anthropic', '--prompt-file', str(prompt), '--out', str(out), '--alias', 'senior', '--role', 'worker'], cwd=root, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    watcher.join(timeout=16)

    assert swapped.is_set(), "watcher did not replace the installed output parent"
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "output_identity_invalid"
    assert record["output_path"] == ""
    assert record["output_digest"] == ""
    assert record["certification_eligible"] is False
    assert out.resolve() == outside_output.resolve()


def run_dispatch_with_stub(
    stub,
    role="reviewer",
    extra_args=None,
    model_route_stub=None,
    extra_env=None,
):
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = fake_bin(tmp)
        write_executable(bin_dir / "claude", stub)
        if model_route_stub is not None:
            product_root = tmp / "product"
            (product_root / "scripts").mkdir(parents=True)
            (product_root / "config").symlink_to(PRODUCT_ROOT / "config", target_is_directory=True)
            write_executable(product_root / "scripts/model-route", model_route_stub)
        out = tmp / "out.txt"
        # PATH precedence keeps the checkout's stubs first.
        env = fabric_free_env()
        if model_route_stub is not None:
            env["AGENT_FABRIC_PRODUCT_ROOT"] = str(product_root)
        env.update(extra_env or {})
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        command = [
                str(SCRIPT),
                "--tool",
                "claude",
                "--orchestrator-family",
                "codex",
                "--role",
                role,
                "--out",
                str(out),
                "--prompt",
                "Reply exactly OK",
            ]
        command.extend(extra_args or [])
        result = run_bounded(
            command,
            cwd=tmp,
            env=env,
            timeout_seconds=30,
            output_limit_bytes=1_048_576,
        )
        record = json.loads(result.output.splitlines()[-1])
        return result, record, out.read_text(encoding="utf-8") if out.exists() else ""


def test_invalid_routing_output_returns_a_structured_failure_record():
    stub = """\
        #!/usr/bin/env bash
        echo "provider must not run" >&2
        exit 9
    """
    result, record, _ = run_dispatch_with_stub(
        stub,
        model_route_stub="""\
            #!/usr/bin/env bash
            exit 127
        """,
    )

    assert result.returncode != 0
    assert record["status"] == "routing_record_invalid"
    assert record["read_only_guarantee"] == "none"


def test_non_ok_routing_record_never_launches_provider_even_with_zero_exit(tmp_path):
    invoked = tmp_path / "provider.invoked"
    stub = f"""\
        #!/usr/bin/env bash
        touch {shlex.quote(str(invoked))}
        cat >/dev/null
        printf 'provider must not run\n'
    """
    result, record, _ = run_dispatch_with_stub(
        stub,
        model_route_stub="""\
            #!/usr/bin/env bash
            printf '%s\n' '{"status":"adapter_disabled","reason":"configured off","adapter_enabled":false,"resolved_model":"opus","model_family":"anthropic","endpoint_provider":"disabled","identity_source":"runtime-configuration","alias":"senior"}'
            exit 0
        """,
    )

    assert result.returncode != 0
    assert record["status"] == "adapter_disabled"
    assert record["reason"] == "configured off"
    assert record["read_only_guarantee"] == "none"
    assert not invoked.exists()


def test_ok_routing_record_requires_complete_execution_identity(tmp_path):
    complete_route = {
        "status": "ok",
        "resolved_model": "opus",
        "model_family": "anthropic",
        "endpoint_provider": "anthropic",
        "identity_source": "runtime-configuration",
        "alias": "senior",
    }
    root = tmp_path
    for missing_field in (
        "resolved_model", "model_family", "endpoint_provider", "identity_source",
    ):
        invoked = root / f"{missing_field}.invoked"
        stub = f"""\
            #!/usr/bin/env bash
            touch {shlex.quote(str(invoked))}
            cat >/dev/null
            printf 'provider must not run\n'
        """
        route = {
            key: value for key, value in complete_route.items()
            if key != missing_field
        }
        route_payload = shlex.quote(json.dumps(route, separators=(",", ":")))
        result, record, _ = run_dispatch_with_stub(
            stub,
            model_route_stub=f"""\
                #!/usr/bin/env bash
                printf '%s\\n' {route_payload}
                exit 0
            """,
        )

        assert result.returncode != 0, missing_field
        assert record["status"] == "routing_record_invalid", missing_field
        assert record["read_only_guarantee"] == "none", missing_field
        assert not invoked.exists(), missing_field


def test_claude_other_primary_uses_sonnet_without_implicit_fable_route():
    stub = """\
        #!/usr/bin/env bash
        model=""
        while [ $# -gt 0 ]; do
          if [ "$1" = "--model" ]; then model="$2"; shift 2; else shift; fi
        done
        cat >/dev/null
        [ "$model" = "sonnet" ] || exit 9
        echo "SONNET OK"
    """
    result, record, output = run_dispatch_with_stub(stub, role="other-primary")
    assert result.returncode == 0, result.output
    assert record["resolved_model"] == "sonnet"
    assert record["requested_model"] == "sonnet"
    # The role `other-primary` takes the senior default; Opus remains an
    # admissible fallback, while Fable is never reached implicitly.
    assert record["fallback_model"] == "opus"
    assert record["identity_source"] == "dated-catalog"
    assert record["substitution"] == ""
    assert output.strip() == "SONNET OK"


def test_claude_crucial_synthesis_dispatches_explicit_fable_5_1_override():
    stub = """\
        #!/usr/bin/env bash
        model=""
        while [ $# -gt 0 ]; do
          if [ "$1" = "--model" ]; then model="$2"; shift 2; else shift; fi
        done
        cat >/dev/null
        [ "$model" = "claude-fable-5-1" ] || exit 9
        echo "FABLE OK"
    """
    result, record, output = run_dispatch_with_stub(
        stub,
        role="synthesis",
        extra_args=[
            "--risk-tier", "crucial", "--model-override-tier", "crucial",
            "--model", "claude-fable-5-1", "--effort", "medium",
        ],
    )
    assert result.returncode == 0, result.output
    assert record["resolved_model"] == "claude-fable-5-1"
    assert record["risk_tier"] == "crucial"
    assert record["model_override_tier"] == "crucial"
    assert record["policy_override"] == "crucial-claude-fable-5-1-synthesis-adjudication"
    assert output.strip() == "FABLE OK"


@pytest.mark.parametrize("risk_tier", ["routine", "substantial"], ids=["routine-risk", "substantial-risk"])
def test_task_class_route_accepts_lifecycle_risk_without_model_override(tmp_path, risk_tier):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    fake_codex(bin_dir, "cat >/dev/null\nprintf 'TASK RISK OK\\n'", models={"gpt-6-luna": ["low"]}, mcp=False)
    out = tmp / "out.txt"
    result, record = run_cf_dispatch(tmp, '--intent', 'ordinary', '--tool', 'codex', '--orchestrator-family', 'anthropic', '--task-class', 'mechanical', '--role', 'worker', '--risk-tier', risk_tier, '--prompt', 'Review', '--out', str(out), path=[bin_dir, PRODUCT_ROOT / 'scripts'])

    assert result.returncode == 0, result.stderr + result.stdout
    assert record["status"] == "ok"
    assert record["route_alias"] == "junior"
    assert record["risk_tier"] == risk_tier
    assert record["model_override_tier"] == ""
    assert out.read_text(encoding="utf-8") == "TASK RISK OK\n"


def test_reviewer_id_round_trips_into_dispatch_receipt():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "OK"
    """
    result, record, output = run_dispatch_with_stub(
        stub, extra_args=["--reviewer-id", "reviewer-1"]
    )
    assert result.returncode == 0, result.output
    assert record["reviewer_id"] == "reviewer-1"
    assert output.strip() == "OK"


def test_claude_auth_failure_does_not_hide_an_unindexed_retry():
    result, record, output = run_dispatch_with_stub("#!/bin/sh\ncat >/dev/null\necho 'Not logged in' >&2\nexit 1\n", role="other-primary")
    assert result.returncode != 0
    assert record["status"] == "auth_required"
    assert record["resolved_model"] == "sonnet"
    assert record["auth_or_quota_error"] is True
    assert record["certification_eligible"] is False
    assert "Not logged in" in output

def test_claude_tool_not_found_keeps_diagnostic_instead_of_retrying_fallback(tmp_path):
    tmp = tmp_path
    misleading_path = tmp / "model-unavailable"
    misleading_path.mkdir()
    home = tmp / "home"
    home.mkdir()
    env = fabric_free_env()
    env["HOME"] = str(home)
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(PRODUCT_ROOT)
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(PRODUCT_ROOT)
    env["PATH"] = os.pathsep.join(
        [
            str(misleading_path),
            str(PRODUCT_ROOT / "scripts"),
            *(
                entry
                for entry in env["PATH"].split(os.pathsep)
                if not (Path(entry) / "claude").exists()
            ),
        ]
    )
    out = tmp / "out.txt"
    result = run_cli(
        [
            str(SCRIPT),
            "--tool",
            "claude",
            "--orchestrator-family",
            "codex",
            "--role",
            "other-primary",
            "--out",
            str(out),
            "--prompt",
            "Reply exactly OK",
        ],
        cwd=tmp,
        env=env,
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "tool_missing"
    assert record["fallback_model"] == "opus"
    assert "claude not found. PATH=" in out.read_text(encoding="utf-8")
    assert "model-unavailable" in out.read_text(encoding="utf-8")


def test_help_exits_cleanly():
    result = run_cli([str(SCRIPT), '--help'], cwd=Path('.'))
    assert result.returncode == 0
    assert "--tool" in result.stdout and "--orchestrator-family" in result.stdout


def test_doctor_exits_cleanly():
    result = run_cli([str(SCRIPT), '--doctor'], cwd=Path('.'))
    assert result.returncode == 0
    assert "PATH=" in result.stdout


def test_missing_option_value_names_the_flag():
    result = run_cli([str(SCRIPT), '--tool'], cwd=Path('.'))
    assert result.returncode == 2
    assert result.stderr == "missing value for --tool\n"


def test_doctor_reports_every_provider_cli():
    result = run_cli([str(SCRIPT), '--doctor'], cwd=Path('.'))
    assert result.returncode == 0
    lines = result.stdout.splitlines()
    assert lines[0] == "cf_dispatch doctor"
    keys = {line.split("=", 1)[0] for line in lines if "=" in line}
    assert {"pwd", "PATH", "CF_DISPATCH_ENABLE_KIRO", "CF_DISPATCH_ENABLE_COPILOT",
            "CF_DISPATCH_AGY_ADD_DIR", "claude", "codex", "cursor-agent", "agy",
            "kiro-cli", "copilot", "opencode"} <= keys


def test_missing_prompt_file_is_clean_error():
    result = run_cli([str(SCRIPT), '--tool', 'claude', '--orchestrator-family', 'codex', '--prompt-file', '/no/such/file'], cwd=Path('.'))
    assert result.returncode == 2
    assert "prompt file" in result.stderr


def test_claude_auth_failure_is_typed_for_owner_recovery():
    result, record, output = run_dispatch_with_stub("#!/bin/sh\ncat >/dev/null\necho 'Authentication required' >&2\nexit 1\n")
    assert result.returncode != 0
    assert record["status"] == "auth_required"
    assert record["retryable"] is False

def test_claude_persistent_session_keeps_neutral_read_only_boundary(tmp_path):
    args_file = tmp_path / "args"
    stub = f"#!/bin/sh\nprintf '%s\\n' \"$@\" > {args_file}\ncat >/dev/null\necho OK\n"
    result, record, output = run_dispatch_with_stub(stub)
    assert result.returncode == 0
    arguments = args_file.read_text()
    assert "--session-id" in arguments
    assert "--no-session-persistence" not in arguments
    assert "--permission-prompts" in arguments
    assert "Read,Grep,Glob" in arguments
    assert "Do not modify files" in arguments

def test_agy_direct_route_dispatches_json_sandbox_and_file_prompt(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    args_file = tmp / "agy.args"
    stdin_file = tmp / "agy.stdin"
    allowed_one = tmp / "allowed-one"
    allowed_two = tmp / "allowed-two"
    allowed_one.mkdir()
    allowed_two.mkdir()
    # The real agy requires a value for --print: with none it exits 2 on
    # "flag needs an argument", and `--print -` is worse, because it treats
    # the dash as the literal prompt, ignores stdin and answers it. The
    # stub enforces that contract so this test cannot pass against a
    # dispatcher the installed CLI would reject.
    write_executable(
        bin_dir / "agy",
        f"""\
        #!/usr/bin/env bash
        printf '%s\\n' "$@" > {args_file}
        cat > {stdin_file}
        prev=""
        prompt=""
        for arg in "$@"; do
          [ "$prev" = "--print" ] && prompt="$arg"
          prev="$arg"
        done
        if [ -z "$prompt" ] || [ "$prompt" = "-" ]; then
          echo "flag needs an argument: -print" >&2
          exit 2
        fi
        printf '%s\\n' '{{"status":"SUCCESS","response":"AGY OK","conversation_id":"c1"}}'
        """,
    )
    env = fabric_free_env()
    env["CF_DISPATCH_AGY_ADD_DIR"] = str(allowed_one)
    out = tmp / "out.txt"
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'agy',
            '--model',
            'gemini-3.7-flash',
            '--effort',
            'medium',
            '--add-dir',
            str(allowed_two),
            '--orchestrator-family',
            'codex',
            '--out',
            str(out),
            '--prompt',
            'Reply exactly AGY OK',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr
    assert DISPATCH_SCHEMA <= set(record)
    assert record["status"] == "ok"
    assert record["adapter"] == "agy"
    assert record["provider_family"] == "google"
    assert record["endpoint_provider"] == "agy"
    assert record["effort"] == "medium"
    assert record["read_only_guarantee"] == "prompt_only"
    assert record["certification_eligible"] is False
    assert out.read_text(encoding="utf-8") == "AGY OK"
    args = args_file.read_text(encoding="utf-8").splitlines()
    assert args[args.index("--output-format") + 1] == "stream-json"
    assert "--sandbox" in args
    assert args[args.index("--model") + 1] == "gemini-3.7-flash"
    assert args[args.index("--effort") + 1] == "medium"
    assert args.count("--add-dir") == 2, args
    assert str(allowed_one.resolve()) in args
    assert str(allowed_two.resolve()) in args
    # agy has no file-backed prompt input, so the prompt is one argv value.
    assert args[args.index("--print") + 1] == f"Workspace root: {tmp.resolve()}"
    assert any("Do not modify files or run commands that mutate state." in arg for arg in args)
    assert args[-1] == "Reply exactly AGY OK"


def test_agy_task_class_uses_its_runtime_capability_producer(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    args_file = tmp / "agy.args"
    write_executable(
        bin_dir / "agy",
        f"""#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.8-flash-high\ngemini-3.8-flash-medium\ngemini-3.8-flash-low\n'
          exit 0
        fi
        printf '%s\n' "$@" > {args_file}
        printf '%s\n' '{{"status":"SUCCESS","response":"AGY TASK OK"}}'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--orchestrator-family', 'openai', '--task-class', 'mechanical', '--role', 'worker', '--risk-tier', 'substantial', '--prompt', 'Review', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr + result.stdout
    assert record["status"] == "ok"
    assert record["route_alias"] == "junior"
    assert record["resolved_model"] == "gemini-3.8-flash"
    assert record["provider_family"] == "google"
    assert record["effort"] == "low"
    assert record["effort_capability_source"] == "runtime-model-catalog"
    assert record["risk_tier"] == "substantial"
    assert out.read_text(encoding="utf-8") == "AGY TASK OK"


def test_agy_explicit_effort_never_claims_an_unavailable_probe(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          echo 'capability source unavailable' >&2
          exit 7
        fi
        printf '%s\n' '{"status":"SUCCESS","response":"EXPLICIT OK"}'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli(
        [
            str(SCRIPT),
            '--intent',
            'ordinary',
            '--tool',
            'agy',
            '--orchestrator-family',
            'openai',
            '--model',
            'gemini-3.7-flash',
            '--effort',
            'medium',
            '--prompt',
            'Review',
            '--out',
            str(out),
        ],
        cwd=tmp,
        env=env,
        path=[bin_dir, PRODUCT_ROOT / "scripts"],
    )

    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr + result.stdout
    assert record["status"] == "ok"
    assert record["effort"] == "medium"
    assert record["effort_capability_source"] == "provider-unverified"


def test_claude_task_class_runs_the_subscription_capability_canary(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    calls = tmp / "claude.calls"
    write_executable(
        bin_dir / "claude",
        f"""#!/usr/bin/env bash
        printf '%s\n' "$*" >> {calls}
        if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
          printf '%s\n' '{{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro"}}'
          exit 0
        fi
        for arg in "$@"; do
          if [ "$arg" = "--output-format" ]; then
            printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"OK","modelUsage":{{"claude-opus-5-5":{{"inputTokens":1}}}}}}'
            exit 0
          fi
        done
        cat >/dev/null
        printf 'CLAUDE TASK OK\n'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli(
        [
            str(SCRIPT),
            '--intent',
            'ordinary',
            '--tool',
            'claude',
            '--orchestrator-family',
            'openai',
            '--task-class',
            'critical-review',
            '--role',
            'critical-review',
            '--risk-tier',
            'crucial',
            '--prompt',
            'Review',
            '--out',
            str(out),
        ],
        cwd=tmp,
        env=env,
        path=[bin_dir, PRODUCT_ROOT / "scripts"],
    )

    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr + result.stdout
    assert record["status"] == "ok"
    assert record["route_alias"] == "principal"
    assert record["resolved_model"] == "claude-opus-5-5"
    assert record["provider_family"] == "anthropic"
    assert record["effort"] == "high"
    assert record["effort_capability_source"] == "provider-unverified"
    assert record["risk_tier"] == "crucial"
    assert "--output-format json" in calls.read_text(encoding="utf-8")
    assert out.read_text(encoding="utf-8") == "OK"


def test_agy_oversized_prompt_fails_closed_instead_of_truncating(tmp_path):
    """agy takes the prompt as one argv value, so ARG_MAX is a real ceiling.

    A brief silently clipped by the kernel would be reviewed as if complete,
    which is the same class of quiet wrongness as a denied read reported as
    SUCCESS. It must fail closed instead.
    """
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """\
        #!/usr/bin/env bash
        printf '%s\\n' '{"status":"SUCCESS","response":"SHOULD NOT RUN"}'
        """,
    )
    env = fabric_free_env()
    out = tmp / "out.txt"
    big_prompt = tmp / "big-prompt.txt"
    big_prompt.write_text("x" * 126_975, encoding="utf-8")
    result = run_cli([str(SCRIPT), '--tool', 'agy', '--model', 'gemini-3.7-flash', '--effort', 'low', '--orchestrator-family', 'anthropic', '--out', str(out), '--prompt-file', str(big_prompt)], cwd=tmp_path, env=env, path=[bin_dir])
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "rejected"
    assert record["certification_eligible"] is False
    assert not out.exists() or "SHOULD NOT RUN" not in out.read_text(encoding="utf-8")


def test_agy_success_with_empty_response_is_non_passing(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """\
        #!/usr/bin/env bash
        cat >/dev/null
        printf '%s\\n' '{"status":"SUCCESS","response":""}'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()
    result = run_cli([str(SCRIPT), '--tool', 'agy', '--model', 'gemini-3.8-flash-high', '--orchestrator-family', 'codex', '--out', str(out), '--prompt', 'Reply'], cwd=tmp_path, env=env, path=[bin_dir])
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "failed"
    assert record["certification_eligible"] is False
    # The output carries the diagnostic rather than a review body. It must
    # never be empty, or the caller cannot tell a failed dispatch from a
    # dispatch that has not run.
    written = out.read_text(encoding="utf-8")
    assert "status=failed" in written
    assert "no assistant text" in written


def test_agy_success_envelope_with_nonzero_process_exit_is_failure(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-medium\n'
          exit 0
        fi
        printf '%s\n' '{"status":"SUCCESS","response":"MUST NOT PASS"}'
        exit 7
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--model', 'gemini-3.7-flash', '--effort', 'medium', '--orchestrator-family', 'openai', '--out', str(out), '--prompt', 'Review'], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "failed"
    assert record["exit"] == 7
    assert record["certification_eligible"] is False
    assert "MUST NOT PASS" not in out.read_text(encoding="utf-8")
    written = out.read_text(encoding="utf-8")
    assert "provider exited 7" in written


def test_agy_success_envelope_with_error_never_publishes_response(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-medium\n'
          exit 0
        fi
        printf '%s\n' '{"status":"SUCCESS","response":"MUST NOT PASS","error":"quota exceeded"}'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--model', 'gemini-3.7-flash', '--effort', 'medium', '--orchestrator-family', 'openai', '--out', str(out), '--prompt', 'Review'], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "usage_limited"
    assert record["certification_eligible"] is False
    written = out.read_text(encoding="utf-8")
    assert "MUST NOT PASS" not in written
    assert "provider error: quota exceeded" in written


def test_agy_success_envelope_with_null_error_publishes_response(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-medium\\n'
          exit 0
        fi
        printf '%s\\n' '{"status":"SUCCESS","response":"AGY NULL OK","error":null}'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--model', 'gemini-3.7-flash', '--effort', 'medium', '--orchestrator-family', 'openai', '--out', str(out), '--prompt', 'Review'], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr + result.stdout
    assert record["status"] == "ok"
    assert out.read_text(encoding="utf-8") == "AGY NULL OK"


def test_agy_requires_one_well_typed_json_envelope(tmp_path):
    invalid_envelopes = (
        '{"status":"SUCCESS","response":"MUST NOT PASS","response":"duplicate"}',
        'provider noise\n{"status":"SUCCESS","response":"MUST NOT PASS"}\n',
        '{"status":"SUCCESS","response":7}',
    )
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-medium\n'
          exit 0
        fi
        printf '%s' "$AGY_TEST_ENVELOPE"
        """,
    )
    env = fabric_free_env()

    for index, envelope in enumerate(invalid_envelopes):
        out = tmp / f"out-{index}.txt"
        env["AGY_TEST_ENVELOPE"] = envelope
        result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--model', 'gemini-3.7-flash', '--effort', 'medium', '--orchestrator-family', 'openai', '--out', str(out), '--prompt', 'Review'], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "failed"
        assert record["certification_eligible"] is False
        assert out.read_text(encoding="utf-8").startswith("agy dispatch failed:")


def test_agy_rejects_non_utf8_success_envelope(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-medium\n'
          exit 0
        fi
        printf '{"status":"SUCCESS","response":"A'
        printf '\\377'
        printf 'B"}'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--model', 'gemini-3.7-flash', '--effort', 'medium', '--orchestrator-family', 'openai', '--out', str(out), '--prompt', 'Review'], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "failed"
    assert record["certification_eligible"] is False
    assert out.read_text(encoding="utf-8").startswith(
        "agy dispatch failed: status=failed"
    )


def test_disabled_execution_routes_keep_configured_reason_and_never_launch_provider(tmp_path):
    import yaml
    expected_reason = "Disabled by the test compatibility overlay."
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    compatibility = yaml.safe_load((PRODUCT_ROOT / "config/adapter-compatibility.yaml").read_text())
    compatibility["adapters"]["kiro-acp"].update(enabled=False, disabled_reason=expected_reason)
    overlay = tmp / "adapter-compatibility.yaml"
    overlay.write_text(yaml.safe_dump(compatibility))
    product_root = tmp / "product"
    (product_root / "scripts").mkdir(parents=True)
    (product_root / "config").symlink_to(PRODUCT_ROOT / "config", target_is_directory=True)
    write_executable(product_root / "scripts/model-route", f"""#!/usr/bin/env bash
        [ "$1" = resolve ] || exit 2
        exec {shlex.quote(sys.executable)} {shlex.quote(str(PRODUCT_ROOT / 'scripts/model_route.py'))} "$@" --adapter-compatibility {shlex.quote(str(overlay))}
    """)
    env = fabric_free_env()
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(product_root)
    for tool, model, executable in (("kiro", "deepseek-v3.2", "kiro-cli"),):
        invoked = tmp / f"{tool}.invoked"
        write_executable(
            bin_dir / executable,
            f"#!/usr/bin/env bash\ntouch {invoked}\nexit 99\n",
        )
        out = tmp / f"{tool}.txt"
        result = run_cli(
            [
                str(SCRIPT),
                '--intent',
                'ordinary',
                '--tool',
                tool,
                '--model',
                model,
                '--alias',
                'junior',
                '--role',
                'worker',
                '--prompt',
                'Review',
                '--out',
                str(out),
            ],
            cwd=tmp,
            env=env,
            path=[bin_dir, PRODUCT_ROOT / "scripts"],
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "adapter_disabled"
        assert record["reason"] == expected_reason
        assert record["exit"] == 1
        assert record["read_only_guarantee"] == "none"
        assert record["certification_eligible"] is False
        assert not invoked.exists()


def test_disabled_capability_adapters_are_rejected_before_their_probe_runs(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    product_root = tmp / "product"
    (product_root / "scripts").mkdir(parents=True)
    (product_root / "config").symlink_to(PRODUCT_ROOT / "config", target_is_directory=True)
    write_executable(
        product_root / "scripts/model-route",
        """#!/usr/bin/env bash
        printf '%s\n' '{"schema_version":1,"status":"adapter_disabled","reason":"configured off","adapter_enabled":false,"endpoint_provider":"disabled"}'
        exit 1
        """,
    )
    env = fabric_free_env()
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(product_root)

    for tool, model in (("codex", "gpt-6-luna"), ("agy", "gemini-3.7-flash")):
        invoked = tmp / f"{tool}.invoked"
        write_executable(
            bin_dir / tool,
            f"#!/usr/bin/env bash\ntouch {invoked}\nexit 99\n",
        )
        out = tmp / f"{tool}.txt"
        result = run_cli(
            [
                str(SCRIPT),
                '--intent',
                'ordinary',
                '--tool',
                tool,
                '--model',
                model,
                '--alias',
                'senior',
                '--role',
                'worker',
                '--prompt',
                'Review',
                '--out',
                str(out),
            ],
            cwd=tmp,
            env=env,
            path=[bin_dir],
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "adapter_disabled"
        assert record["reason"] == "configured off"
        assert not invoked.exists()


def test_prompt_file_trailing_newlines_reach_stdin_adapter_byte_for_byte(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    received = tmp / "received.bin"
    fake_codex(bin_dir, f"cat > {received}\nprintf 'OK\n'", models={'gpt-6-luna': ['high']}, mcp=False)
    prompt = tmp / "prompt.bin"
    prompt.write_bytes(b"Review exactly\n\n")
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli(
        [
            str(SCRIPT),
            '--no-preface',
            '--intent',
            'ordinary',
            '--tool',
            'codex',
            '--orchestrator-family',
            'anthropic',
            '--alias',
            'senior',
            '--role',
            'worker',
            '--prompt-file',
            str(prompt),
            '--out',
            str(out),
        ],
        cwd=tmp,
        env=env,
        path=[bin_dir, PRODUCT_ROOT / "scripts"],
    )

    assert result.returncode == 0, result.stderr + result.stdout
    assert received.read_bytes() == prompt.read_bytes()


def test_prompt_file_trailing_newlines_reach_argv_adapter_byte_for_byte(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    received = tmp / "received.bin"
    write_executable(
        bin_dir / "agy",
        f"""#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-medium\n'
          exit 0
        fi
        previous=""
        for argument in "$@"; do
          if [ "$previous" = "--print" ]; then
            printf '%s' "$argument" > {received}
          fi
          previous="$argument"
        done
        printf '%s\n' '{{"status":"SUCCESS","response":"OK"}}'
        """,
    )
    prompt = tmp / "prompt.bin"
    prompt.write_bytes(b"Review exactly\n\n")
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli([str(SCRIPT), '--no-preface', '--intent', 'ordinary', '--tool', 'agy', '--orchestrator-family', 'openai', '--model', 'gemini-3.7-flash', '--effort', 'medium', '--prompt-file', str(prompt), '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    assert result.returncode == 0, result.stderr + result.stdout
    assert received.read_bytes().split(b"\nTask:\n", 1)[1] == prompt.read_bytes()


def test_nul_prompt_file_is_rejected_before_provider_execution(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    invoked = tmp / "invoked"
    write_executable(bin_dir / "codex", f"#!/usr/bin/env bash\ntouch {invoked}\n")
    prompt = tmp / "prompt.bin"
    prompt.write_bytes(b"A\0B\n")
    env = fabric_free_env()

    result = run_cli(
        [
            str(SCRIPT),
            '--intent',
            'ordinary',
            '--tool',
            'codex',
            '--orchestrator-family',
            'anthropic',
            '--prompt-file',
            str(prompt),
        ],
        cwd=tmp,
        env=env,
        path=[bin_dir, PRODUCT_ROOT / "scripts"],
    )

    assert result.returncode == 2
    assert json.loads(result.stdout)["status"] == "prompt_unavailable"
    assert not invoked.exists()


@pytest.mark.parametrize("field", ["prompt_file", "add_dir"])
def test_shell_checks_original_protected_inputs_before_copy(tmp_path, field):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / ".agents").mkdir()
    (repo / ".agents/fabric-policy.json").write_text('{"protected_paths":["private/"]}')
    (repo / "private").mkdir()
    (repo / "private/prompt.md").write_text("secret", encoding="utf-8")
    (repo / "prompt.md").write_text("safe", encoding="utf-8")
    bin_dir = fake_bin(tmp_path)
    marker = tmp_path / "copied"
    write_executable(bin_dir / "cp", f"#!/bin/sh\ntouch {shlex.quote(str(marker))}\nexec /bin/cp \"$@\"\n")
    env = fabric_free_env()
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(PRODUCT_ROOT)
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(PRODUCT_ROOT)
    command = [
        str(SCRIPT), "--intent", "ordinary", "--tool", "opencode",
        "--model", "opencode/mimo-v2.6-flash-free", "--prompt-file",
        "private/prompt.md" if field == "prompt_file" else "prompt.md",
    ]
    if field == "add_dir":
        command.extend(("--add-dir", "private"))
    result = run_cli(command, cwd=repo, env=env, path=[bin_dir])
    assert result.returncode != 0
    assert "protected_path_denied" in result.stdout
    assert not marker.exists()


def test_shell_rejects_primary_checkout_writer(tmp_path):
    repo = tmp_path / "primary"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'claude', '--prompt', 'hello', '--access-mode', 'worktree_write', '--worktree', str(repo)], cwd=tmp_path, env=fabric_free_env())
    assert result.returncode == 2
    assert "create a linked worktree" in result.stderr


def test_agy_failure_preserves_the_provider_reason_in_the_output(tmp_path):
    """agy reports failures in the stdout envelope, not on stderr.

    Classifying the status is not enough on its own: an exhausted quota and a
    revoked credential both land on auth_or_quota_error and want opposite
    responses, so the provider's own words have to survive into the output.
    """
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """\
        #!/usr/bin/env bash
        cat >/dev/null
        printf '%s\\n' '{"status":"ERROR","response":"","error":"Individual quota reached. Resets in 55m39s."}'
        exit 1
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()
    result = run_cli([str(SCRIPT), '--tool', 'agy', '--model', 'gemini-3.8-flash-high', '--orchestrator-family', 'codex', '--out', str(out), '--prompt', 'Reply'], cwd=tmp_path, env=env, path=[bin_dir])
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "usage_limited"
    assert record["certification_eligible"] is False
    written = out.read_text(encoding="utf-8")
    assert "Individual quota reached" in written
    assert "Resets in 55m39s" in written


def test_agy_failure_with_null_error_uses_generic_reason_and_never_publishes_response(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """#!/usr/bin/env bash
        cat >/dev/null
        printf '%s\\n' '{"status":"ERROR","response":"SHOULD NOT PASS","error":null}'
        exit 1
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()

    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'agy',
            '--model',
            'gemini-3.8-flash-high',
            '--orchestrator-family',
            'codex',
            '--out',
            str(out),
            '--prompt',
            'Reply',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )

    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "failed"
    written = out.read_text(encoding="utf-8")
    assert "SHOULD NOT PASS" not in written
    assert (
        "provider error: provider returned a non-success status without an error message"
        in written
    )
    assert "provider error: None" not in written


def test_agy_permission_denial_overrides_false_success_envelope(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "agy",
        """\
        #!/usr/bin/env bash
        cat >/dev/null
        printf '%s\\n' '{"status":"SUCCESS","response":""}'
        echo 'jetski: no output produced - a tool required the "read_file" permission that headless mode cannot prompt for' >&2
        exit 0
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'agy',
            '--model',
            'gemini-3.8-flash-high',
            '--orchestrator-family',
            'codex',
            '--out',
            str(out),
            '--prompt',
            'Reply',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "permission_blocked"
    assert "read_file" in out.read_text(encoding="utf-8")


def test_default_failure_retains_only_the_declared_output_tempfile(tmp_path):
    tmp = tmp_path
    temp_root = tmp / "tmp"
    temp_root.mkdir()
    env = fabric_free_env()
    env["TMPDIR"] = str(temp_root)
    result = run_cli([str(SCRIPT), '--tool', 'kiro', '--orchestrator-family', 'codex', '--prompt', 'Review'], cwd=tmp_path, env=env)
    record = json.loads(result.stdout)
    output = Path(record["output_path"])
    assert result.returncode != 0
    assert output.exists()
    assert output.is_file()
    assert all(path == output or path.name.startswith(output.name+".") for path in temp_root.iterdir())
    output.unlink()


def test_orchestrator_family_is_required(tmp_path):
    result = run_cli([str(SCRIPT), '--tool', 'claude', '--prompt', 'Reply exactly OK'], cwd=tmp_path)
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert DISPATCH_SCHEMA <= set(record)
    assert record["status"] == "orchestrator_family_required"
    assert record["cross_family"] is False


def test_same_family_cli_is_forbidden_when_family_declared(tmp_path):
    result = run_cli([str(SCRIPT), '--tool', 'codex', '--orchestrator-family', 'codex', '--prompt', 'Reply exactly OK'], cwd=tmp_path)
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert DISPATCH_SCHEMA <= set(record)
    assert record["status"] == "same_family_forbidden"
    assert record["read_only_guarantee"] == "none"
    assert record["cross_family"] is False


def test_ordinary_intent_allows_same_family_without_certification():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "ORDINARY OK"
    """
    result, record, output = run_dispatch_with_stub(
        stub,
        extra_args=["--intent", "ordinary", "--orchestrator-family", "anthropic"],
    )
    assert result.returncode == 0, result.output
    assert record["status"] == "ok"
    assert record["execution_intent"] == "ordinary"
    assert record["provider_family"] == "anthropic"
    assert record["cross_family"] is False
    assert record["certification_eligible"] is False
    assert output.strip() == "ORDINARY OK"


def test_ordinary_cross_family_enforced_route_is_not_certification():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "ORDINARY CROSS-FAMILY OK"
    """
    result, record, _ = run_dispatch_with_stub(
        stub,
        extra_args=["--intent", "ordinary"],
    )
    assert result.returncode == 0, result.output
    assert record["cross_family"] is True
    assert record["read_only_guarantee"] == "enforced"
    assert record["certification_eligible"] is False


def test_task_class_route_preserves_effective_alias():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "TASK CLASS OK"
    """
    result, record, _ = run_dispatch_with_stub(
        stub,
        role="worker",
        extra_args=["--intent", "ordinary", "--task-class", "mechanical"],
        model_route_stub="""#!/usr/bin/env bash
            printf '{\"status\":\"ok\",\"alias\":\"junior\",\"resolved_model\":\"haiku\",\"model_family\":\"anthropic\",\"endpoint_provider\":\"anthropic\",\"identity_source\":\"test\"}\n'
        """,
    )
    assert result.returncode == 0, result.output
    assert record["route_alias"] == "junior"


def test_route_json_fields_are_read_by_key_without_delimiter_shifting():
    stub = """\
        #!/usr/bin/env bash
        model=""
        while [ $# -gt 0 ]; do
          if [ "$1" = "--model" ]; then model="$2"; shift 2; else shift; fi
        done
        cat >/dev/null
        [ "$model" = "opus|candidate" ] || exit 9
        echo "STRUCTURED ROUTE OK"
    """
    result, record, output = run_dispatch_with_stub(
        stub,
        extra_args=["--intent", "ordinary"],
        model_route_stub="""#!/usr/bin/env bash
            printf '%s\n' '{"status":"ok","alias":"principal","resolved_model":"opus|candidate","model_family":"anthropic","endpoint_provider":"anthropic","identity_source":"test-route","requested_effort":"high","effort":"high","effort_source":"test","effort_capability_source":"test-capability"}'
        """,
    )

    assert result.returncode == 0, result.output
    assert record["resolved_model"] == "opus|candidate"
    assert record["provider_family"] == "anthropic"
    assert record["endpoint_provider"] == "anthropic"
    assert record["identity_source"] == "test-route"
    assert record["effort"] == "high"
    assert output.strip() == "STRUCTURED ROUTE OK"


def test_cursor_model_provider_prevents_disguised_same_family_review(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "cursor-agent", "#!/usr/bin/env bash\necho OK\n")
    env = fabric_free_env()
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'cursor',
            '--model',
            'gpt-5.6-sol',
            '--orchestrator-family',
            'openai',
            '--prompt',
            'Review',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["provider_family"] == "openai"
    assert record["status"] == "same_family_forbidden"
    assert record["cross_family"] is False


def test_cursor_distinct_model_records_adapter_and_provider_family(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    args_file = tmp / "cursor.args"
    write_executable(
        bin_dir / "cursor-agent",
        f"#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > {args_file}\necho OK\n",
    )
    out = tmp / "out.txt"
    env = fabric_free_env()
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'cursor',
            '--model',
            'cursor-grok-4.5-high',
            '--orchestrator-family',
            'openai',
            '--out',
            str(out),
            '--prompt',
            'Review',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode == 0
    assert record["adapter"] == "cursor"
    assert record["provider_family"] == "xai"
    assert record["endpoint_provider"] == "cursor"
    assert record["model_family"] == "xai"
    assert record["resolved_model"] == "cursor-grok-4.5-high"
    assert record["output_digest"] == "sha256:" + __import__("hashlib").sha256(out.read_bytes()).hexdigest()
    assert record["certification_eligible"] is True
    assert record["cross_family"] is True
    cursor_args = args_file.read_text(encoding="utf-8").splitlines()
    assert "--trust" in cursor_args
    assert "--sandbox" in cursor_args
    assert "enabled" in cursor_args
    assert "--mode" in cursor_args
    assert cursor_args[cursor_args.index("--mode") + 1] == "ask"


def test_explicit_output_path_preserves_adapter_failure_diagnostics(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(
        bin_dir / "cursor-agent",
        "#!/usr/bin/env bash\necho 'simulated adapter failure' >&2\nexit 9\n",
    )
    out = tmp / "review.txt"
    env = fabric_free_env()
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'cursor',
            '--model',
            'cursor-grok-4.5-high',
            '--orchestrator-family',
            'openai',
            '--out',
            str(out),
            '--prompt',
            'Review',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "failed"
    assert record["output_path"] == str(out)
    assert "simulated adapter failure" in out.read_text(encoding="utf-8")


def test_unwritable_output_path_cannot_certify_success(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    fake_codex(bin_dir, 'echo OK', models={'gpt-6.1-sol': ['high', 'max', 'ultra']}, mcp=False)
    env = fabric_free_env()
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'codex',
            '--effort',
            'high',
            '--orchestrator-family',
            'anthropic',
            '--out',
            str(tmp / 'missing' / 'out.txt'),
            '--prompt',
            'Review',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "output_write_error"
    assert record["certification_eligible"] is False
    assert record["output_path"] == ""


@pytest.mark.parametrize(
    "case_args,models,expected_alias,expected_model,requested_effort,effort,extra_absent,check_effort_fields,check_catalog_fields,check_argv,check_unquoted_xhigh",
    [
        pytest.param(["--role", "critical-review"], {"gpt-6-astra": ["high"]}, "principal", "gpt-6-astra", "xhigh", "high", [], True, True, True, True, id="critical-review-effort-substitution"),
        pytest.param([], {"gpt-6-luna": ["medium", "high"], "gpt-6.1-sol": ["medium", "high", "max"]}, "senior", "gpt-6.1-sol", "medium", "medium", ["gpt-6-luna", "gpt-6-astra"], True, False, True, False, id="bare-dispatch-defaults-to-senior"),
        pytest.param(["--role", "critical-review"], {"gpt-6-luna": ["medium"], "gpt-6-astra": ["xhigh"]}, "principal", "gpt-6-astra", "xhigh", "xhigh", [], False, False, False, False, id="critical-review-defaults-to-principal"),
    ],
)
def test_codex_role_effort_selection(
    tmp_path, case_args, models, expected_alias, expected_model,
    requested_effort, effort, extra_absent, check_effort_fields,
    check_catalog_fields, check_argv, check_unquoted_xhigh,
):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    args_file = tmp / "codex.args"
    fake_codex(bin_dir, f'''printf '%s\\n' "$@" > {args_file}
        cat >/dev/null
        echo OK''', models=models, mcp=False)
    result, record = run_cf_dispatch(tmp, "--tool", "codex", "--orchestrator-family", "anthropic", *case_args, "--out", str(tmp / "out.txt"), "--prompt", "Review", path=[bin_dir])
    assert result.returncode == 0, result.stderr
    assert record["route_alias"] == expected_alias
    assert record["resolved_model"] == expected_model
    if check_effort_fields:
        assert record["requested_effort"] == requested_effort
        assert record["effort"] == effort
    if check_catalog_fields:
        assert record["effort_capability_source"] == "runtime-model-catalog"
        assert record["catalog_model"] == ""
        assert record["model_selection"] == ""
    if check_argv:
        args = args_file.read_text(encoding="utf-8").splitlines()
        assert "-m" in args
        assert expected_model in args
        assert 'service_tier="default"' in args
        assert f'model_reasoning_effort="{effort}"' in args
        for absent in extra_absent:
            assert absent not in args
    if check_unquoted_xhigh:
        args = args_file.read_text(encoding="utf-8").splitlines()
        assert "model_reasoning_effort=xhigh" not in args


@pytest.mark.parametrize(
    "catalogue,probe_failure,expected_source",
    [
        pytest.param(None, True, "runtime-discovery-failed", id="probe-exit-failure"),
        pytest.param({"models": [{"display_name": "missing slug"}, {"slug": "gpt-5.6-terra", "supported_reasoning_levels": [{"effort": "high"}]}]}, False, "runtime-discovery-failed", id="mixed-malformed-catalogue"),
        pytest.param({"models": [{"slug": "gpt-5.6-terra", "supported_reasoning_levels": []}]}, False, "runtime-discovery-failed", id="unrelated-model-without-efforts"),
    ],
)
def test_codex_capability_failures_block_execution_with_receipt(tmp_path, catalogue, probe_failure, expected_source):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    invoked = tmp / "codex.exec-invoked"
    if probe_failure:
        write_executable(bin_dir / "codex", f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              echo "capability discovery unavailable" >&2
              exit 23
            fi
            touch {invoked}
            exit 9''')
    else:
        fake_codex(bin_dir, f"touch {invoked}\nexit 9", catalogue=catalogue)
    result, record = run_cf_dispatch(tmp, "--tool", "codex", "--orchestrator-family", "anthropic", "--out", str(tmp / "out.txt"), "--prompt", "Review", path=[bin_dir])
    assert result.returncode != 0
    assert DISPATCH_SCHEMA <= set(record)
    assert record["status"] == "capability_discovery_failed"
    assert record["effort_capability_source"] == expected_source
    assert record["certification_eligible"] is False
    if probe_failure:
        assert record["read_only_guarantee"] == "none"
    assert not invoked.exists()


def test_duplicate_codex_discovery_member_blocks_execution_with_receipt(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    invoked = tmp / "codex.exec-invoked"
    write_executable(
        bin_dir / "codex",
        f'''#!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"high","effort":"max"}}]}}]}}'
          exit 0
        fi
        touch {invoked}
        exit 9
        ''',
    )
    env = fabric_free_env()
    result = run_cli(
        [str(SCRIPT), "--tool", "codex", "--orchestrator-family", "anthropic", "--out", str(tmp / "out.txt"), "--prompt", "Review"],
        cwd=tmp, env=env, path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert DISPATCH_SCHEMA <= set(record)
    assert record["status"] == "capability_discovery_failed"
    assert record["certification_eligible"] is False
    assert not invoked.exists()


def test_codex_explicit_model_reaches_adapter_and_reports_runtime_failure(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    invoked = tmp / "codex.invoked"
    fake_codex(bin_dir, f'touch {invoked}\nexit 9', models={'gpt-5.6-sol': ['high']}, mcp=False)
    env = fabric_free_env()
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'codex',
            '--model',
            'gpt-5.6-sol',
            '--orchestrator-family',
            'anthropic',
            '--prompt',
            'Review',
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "failed"
    assert record["resolved_model"] == "gpt-5.6-sol"
    assert record["requested_model"] == "gpt-5.6-sol"
    assert record["route_alias"] == ""
    assert record["catalog_model"] == ""
    assert record["model_selection"] == ""
    assert invoked.exists()


def test_interrupted_dispatch_cleans_internal_tempfiles(tmp_path):
    tmp = tmp_path
    bin_dir = tmp / "bin"
    temp_root = tmp / "tmp"
    bin_dir.mkdir()
    temp_root.mkdir()
    write_executable(bin_dir / "codex", "#!/usr/bin/env bash\nsleep 10\n")
    env = fabric_free_env()
    env["PATH"] = f"{bin_dir}:{env['PATH']}"
    env["TMPDIR"] = str(temp_root)
    proc = subprocess.Popen(
        [str(SCRIPT), "--tool", "codex", "--orchestrator-family", "anthropic", "--out", str(tmp / "out.txt"), "--prompt", "Review"],
        cwd=tmp_path,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    for _ in range(20):
        if any(temp_root.iterdir()):
            break
        time.sleep(0.05)
    os.killpg(proc.pid, signal.SIGTERM)
    proc.communicate(timeout=5)
    assert list(temp_root.iterdir()) == []


def test_cursor_default_auto_model_keeps_unknown_family_ineligible_for_certification(tmp_path):
    bin_dir = fake_bin(tmp_path)
    write_executable(bin_dir / "cursor-agent", "#!/usr/bin/env bash\necho OK\n")
    env = fabric_free_env()
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'cursor', '--orchestrator-family', 'openai', '--prompt', 'Review'], cwd=tmp_path, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr + result.stdout
    assert record["status"] == "ok"
    assert record["resolved_model"] == "auto"
    assert record["model_selection"] == "adapter-default"
    assert record["model_family"] == "generic-open"
    assert record["cross_family"] is False
    assert record["certification_eligible"] is False


def test_manual_provider_override_is_not_supported(tmp_path):
    result = run_cli([str(SCRIPT), '--tool', 'claude', '--provider-family', 'google', '--orchestrator-family', 'anthropic', '--prompt', 'Review'], cwd=tmp_path)
    assert result.returncode == 2
    assert "unknown arg: --provider-family" in result.stderr


def test_invalid_orchestrator_family_fails_closed(tmp_path):
    result = run_cli([str(SCRIPT), '--tool', 'claude', '--orchestrator-family', 'Claude', '--prompt', 'Reply exactly OK'], cwd=tmp_path)
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert DISPATCH_SCHEMA <= set(record)
    assert record["status"] == "invalid_orchestrator_family"
    assert record["cross_family"] is False


def test_successful_output_with_auth_words_stays_ok():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "The string Not logged in appears in the artifact under review."
    """
    result, record, output = run_dispatch_with_stub(stub)
    assert result.returncode == 0, result.stderr
    assert record["status"] == "ok"
    assert output.strip() == "The string Not logged in appears in the artifact under review."


def test_chain_all_failed_uses_dispatch_schema(tmp_path):
    result = run_cli([str(SCRIPT), '--chain', 'kiro copilot', '--orchestrator-family', 'codex', '--prompt', 'Reply exactly OK'], cwd=tmp_path)
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert DISPATCH_SCHEMA <= set(record)
    assert record["tool"] == "chain"
    assert record["status"] == "all_failed"
    assert record["read_only_guarantee"] == "none"


def test_chain_tool_missing_uses_the_entry_model_when_recording_alias(tmp_path):
    env = fabric_free_env()
    env["HOME"] = str(tmp_path / "home")
    bin_dir = fake_bin(tmp_path)
    # A wrapper, not a symlink: a symlinked venv python loses its site-packages.
    (bin_dir / "python3").write_text(f'#!/bin/sh\nexec "{sys.executable}" "$@"\n', encoding="utf-8")
    (bin_dir / "python3").chmod(0o755)
    available_path = os.pathsep.join(
        path for path in os.environ["PATH"].split(os.pathsep)
        if path and not (Path(path) / "cursor-agent").exists()
    )
    env["PATH"] = f"{bin_dir}:{available_path}"
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--chain', 'cursor:cursor-grok-4.5-high:', '--orchestrator-family', 'openai', '--prompt', 'Reply OK'], cwd=tmp_path, env=env)

    assert result.returncode != 0
    entry_record = next(
        json.loads(line) for line in result.stderr.splitlines() if line.startswith("{")
    )
    assert entry_record["tool"] == "cursor"
    assert entry_record["status"] == "tool_missing", result.stderr
    assert entry_record["requested_model"] == "cursor-grok-4.5-high"
    assert entry_record["route_alias"] == ""


def test_opencode_chain_all_failed_removes_raw_sidecar(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "opencode", """#!/usr/bin/env bash
        echo '{"type":"error","error":"forbidden"}'
    """)
    env = fabric_free_env()
    out = tmp / "out.txt"
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--chain', 'opencode', '--prompt', 'Reply OK', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    assert json.loads(result.stdout)["status"] == "all_failed"
    assert not (tmp / "out.txt.raw.jsonl").exists()


def test_opencode_failed_chain_arm_does_not_leave_raw_for_next_provider(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "opencode", """#!/usr/bin/env bash
        echo '{"type":"error","error":"forbidden"}'
    """)
    write_executable(bin_dir / "cursor-agent", "#!/usr/bin/env bash\necho 'fallback OK'\n")
    env = fabric_free_env()
    out = tmp / "out.txt"
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--chain', 'opencode cursor:cursor-grok-4.5-high', '--prompt', 'Reply OK', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    assert result.returncode == 0, result.stderr + result.stdout
    assert json.loads(result.stdout)["tool"] == "cursor"
    assert out.read_text().strip() == "fallback OK"
    assert not (tmp / "out.txt.raw.jsonl").exists()


def test_run_dir_init_force_flag_only_creates_final_gate(tmp_path):
    subprocess.run(["git", "init", "-q", tmp_path], check=True)
    result = run_cli([str(RUN_DIR_SCRIPT), "--force"], cwd=tmp_path)
    assert result.returncode == 0, result.stderr
    run_dir = Path(result.stdout.strip())
    if not run_dir.is_absolute():
        run_dir = tmp_path / run_dir
    assert (run_dir / "FINAL_GATE.md").exists()
    receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text(encoding="utf-8"))
    assert receipt["status"] == "active"
    assert receipt["retention_policy"] == "capsule-plus-referenced-evidence"
    assert (run_dir / "traces" / "README.md").exists()


def test_run_dir_init_force_does_not_clobber_existing_manifest(tmp_path):
    run_dir = tmp_path / "existing"
    run_dir.mkdir()
    manifest = run_dir / "MANIFEST.md"
    manifest.write_text("KEEP\\n", encoding="utf-8")
    result = run_cli([str(RUN_DIR_SCRIPT), str(run_dir), "--force"], cwd=tmp_path)
    assert result.returncode == 0, result.stderr
    assert manifest.read_text(encoding="utf-8") == "KEEP\\n"
    assert (run_dir / "FINAL_GATE.md").exists()
    assert (run_dir / "RUN_RECEIPT.json").exists()


def test_non_git_product_root_uses_its_model_route_wrapper(tmp_path):
    # From a non-git copy of the product tree, dispatch uses that tree's
    # interpreter-selecting scripts/model-route owner wrapper.
    tmp = tmp_path
    product = tmp / "product"
    shutil.copytree(
        SCRIPTS,
        product / "skills" / "orchestrate" / "scripts",
    )
    shutil.copytree(PRODUCT_ROOT / "skills" / "_shared", product / "skills" / "_shared")
    shutil.copytree(PRODUCT_ROOT / "config", product / "config")
    shutil.copytree(PRODUCT_ROOT / "scripts", product / "scripts")
    (product / ".venv/bin").mkdir(parents=True)
    (product / ".venv/bin/python").symlink_to(sys.executable)
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "python3", f'#!/bin/sh\nexec {shlex.quote(sys.executable)} "$@"\n')
    write_executable(
        bin_dir / "claude",
        """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "OK"
        """,
    )
    env = fabric_free_env()
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(product)
    env["PATH"] = f"{bin_dir}:/usr/bin:/bin"
    # cf_dispatch.py appends $HOME/.local/bin and $HOME/bin to PATH;
    # point HOME at the sandbox so no ambient command can take precedence.
    env["HOME"] = str(tmp)
    instance = tmp / "instance"
    (instance / "config").mkdir(parents=True)
    for name in ("model-routing.json", "model-preferences.json"):
        shutil.copy2(product / "config" / name, instance / "config" / name)
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(instance)
    out = tmp / "out.txt"
    result = run_cli(
        [
            str(product / 'skills' / 'orchestrate' / 'scripts' / 'cf_dispatch.py'),
            '--tool',
            'claude',
            '--orchestrator-family',
            'codex',
            '--role',
            'reviewer',
            '--out',
            str(out),
            '--prompt',
            'Reply exactly OK',
        ],
        cwd=str(tmp),
        env=env,
    )
    assert result.stdout, result.stderr
    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr
    assert record["status"] == "ok"
    assert record["resolved_model"]
    assert out.read_text(encoding="utf-8").strip() == "OK"


if __name__ == "__main__":
    test_help_exits_cleanly()
    test_doctor_exits_cleanly()
    test_missing_prompt_file_is_clean_error()
    test_claude_oauth_fallback_after_bare_auth_failure()
    test_claude_oauth_fallback_uses_neutral_read_only_system_prompt()
    test_agy_direct_route_dispatches_json_sandbox_and_file_prompt()
    test_agy_oversized_prompt_fails_closed_instead_of_truncating()
    test_agy_success_with_empty_response_is_non_passing()
    test_agy_permission_denial_overrides_false_success_envelope()
    test_orchestrator_family_is_required()
    test_same_family_cli_is_forbidden_when_family_declared()
    test_invalid_orchestrator_family_fails_closed()
    test_successful_output_with_auth_words_stays_ok()
    test_chain_all_failed_uses_dispatch_schema()
    test_run_dir_init_force_flag_only_creates_final_gate()
    test_run_dir_init_force_does_not_clobber_existing_manifest()
    print("cf_dispatch behaviour tests: PASS")


def make_worktree(root):
    """Create a real Git worktree root a writer may own."""
    repo = root / "primary"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c",
                    "user.email=test@example.invalid", "commit", "-q", "--allow-empty", "-m", "initial"], check=True)
    worktree = root / "worktree"
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b", "writer", str(worktree)], check=True)
    return worktree.resolve()


def run_worktree_dispatch(
    tool, stub, worktree=None, extra_args=None, intent="ordinary", extra_env=None,
):
    """Dispatch one stubbed provider, optionally on the worktree writer route."""
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td).resolve()
        bin_dir = fake_bin(tmp)
        args_file = tmp / "provider.args"
        body = stub.format(args_file=args_file)
        if tool == "codex":
            fake_codex(bin_dir, body)
        else:
            write_executable(bin_dir / tool, body)
        out = tmp / "out.txt"
        env = fabric_free_env()
        env.update(extra_env or {})
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        command = [
            str(SCRIPT), "--tool", tool, "--orchestrator-family", "codex",
            "--role", "worker", "--intent", intent, "--out", str(out),
            "--prompt", "Reply exactly OK",
        ]
        if worktree == "make":
            worktree = make_worktree(tmp)
            command.extend(["--access-mode", "worktree_write", "--worktree", str(worktree)])
        command.extend(extra_args or [])
        result = run_bounded(
            command, cwd=tmp, env=env, timeout_seconds=30, output_limit_bytes=1_048_576
        )
        recorded = args_file.read_text(encoding="utf-8") if args_file.exists() else ""
        return result, recorded, worktree


CLAUDE_ARGV_STUB = """\
    #!/usr/bin/env bash
    if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
      echo '{{"loggedIn":true,"authMethod":"claude.ai"}}'
      exit 0
    fi
    printf '%s\\n' "$@" >> {args_file}
    printf 'PWD=%s\\n' "$PWD" >> {args_file}
    cat >/dev/null
    echo "OK"
"""

def recorded_codex_profile(recorded):
    """The per-plan Codex permissions profile a recorded argv selects."""
    names = re.findall(r'^default_permissions="(provenant-[0-9a-f]{32})"$', recorded, re.MULTILINE)
    assert len(names) == 1, recorded
    return names[0]


def test_claude_worktree_writer_route_runs_inside_the_owned_worktree():
    result, recorded, worktree = run_worktree_dispatch("claude", CLAUDE_ARGV_STUB, worktree="make")
    assert result.returncode == 0, result.output
    record = json.loads(result.output.splitlines()[-1])
    assert DISPATCH_SCHEMA <= set(record)
    assert record["status"] == "ok"
    assert record["access_mode"] == "worktree_write"
    assert record["worktree"] == str(worktree)
    assert record["read_only_guarantee"] == "none"
    assert "--permission-mode\nacceptEdits" in recorded
    assert f"--add-dir\n{worktree}" in recorded
    # A permission prompt is a denial under -p, so the write tools have to be on
    # the allow-list or the lane cannot run its own tests or commit its own work.
    assert "--allowedTools\nBash,Edit,Write,MultiEdit,NotebookEdit,Read,Grep,Glob" in recorded
    assert "--tools\nRead,Grep,Glob" not in recorded
    assert "--permission-mode\nplan" not in recorded
    assert f"PWD={worktree}" in recorded
    assert "owned worktree" in recorded
    assert "run commands and commit only inside this owned worktree" in recorded
    assert "Do not author changes under .agents/" in recorded


def test_claude_read_only_route_remains_the_default():
    result, recorded, _ = run_worktree_dispatch("claude", CLAUDE_ARGV_STUB)
    assert result.returncode == 0, result.output
    record = json.loads(result.output.splitlines()[-1])
    assert record["access_mode"] == "read_only"
    assert record["worktree"] == ""
    assert record["read_only_guarantee"] == "enforced"
    assert "--permission-mode\ndefault" in recorded
    assert "--tools\nRead,Grep,Glob,Bash\n" in recorded
    assert "--allowedTools\nBash\n" in recorded
    assert "acceptEdits" not in recorded


def test_codex_worktree_writer_route_uses_the_workspace_write_sandbox():
    result, recorded, worktree = run_worktree_dispatch("codex", CODEX_ARGV_STUB, worktree="make")
    assert result.returncode == 0, result.output
    record = json.loads(result.output.splitlines()[-1])
    assert record["access_mode"] == "worktree_write"
    assert record["read_only_guarantee"] == "none"
    profile = recorded_codex_profile(recorded)
    assert f'permissions.{profile}.extends=":workspace"' in recorded
    assert f"--cd\n{worktree}" in recorded
    assert "read-only" not in recorded
    # A linked worktree keeps its Git metadata outside the worktree root.
    assert f"permissions.{profile}.filesystem={{" in recorded
    # --ignore-user-config drops the user's own network setting, so the arm
    # grants the lane network itself: gh, git push and installs need it.
    assert f"permissions.{profile}.network.enabled=true" in recorded
    assert record["provider_network"] is True


def test_codex_worktree_writer_network_can_be_disabled():
    result, recorded, _ = run_worktree_dispatch(
        "codex", CODEX_ARGV_STUB, worktree="make",
        extra_env={"CF_DISPATCH_CODEX_NETWORK": "0"},
    )
    assert result.returncode == 0, result.output
    record = json.loads(result.output.splitlines()[-1])
    assert f"permissions.{recorded_codex_profile(recorded)}.network.enabled=false" in recorded
    assert record["provider_network"] is False


def test_codex_read_only_route_denies_writes_but_keeps_network():
    result, recorded, _ = run_worktree_dispatch("codex", CODEX_ARGV_STUB)
    assert result.returncode == 0, result.output
    record = json.loads(result.output.splitlines()[-1])
    assert record["access_mode"] == "read_only"
    assert record["read_only_guarantee"] == "enforced"
    assert record["provider_network"] is True
    # `-s` would override the profile and drop network again.
    assert "-s\nread-only" not in recorded
    assert "workspace-write" not in recorded
    profile = recorded_codex_profile(recorded)
    assert f'permissions.{profile}.extends=":read-only"' in recorded
    assert f"permissions.{profile}.network.enabled=true" in recorded
    assert "--skip-git-repo-check" in recorded


def test_codex_read_only_route_without_network_writes_only_its_temp():
    result, recorded, _ = run_worktree_dispatch(
        "codex", CODEX_ARGV_STUB, extra_env={"CF_DISPATCH_CODEX_NETWORK": "0"},
    )
    assert result.returncode == 0, result.output
    record = json.loads(result.output.splitlines()[-1])
    assert record["provider_network"] is False
    assert "-s\nread-only" not in recorded
    profile = recorded_codex_profile(recorded)
    state = Path(os.environ["XDG_STATE_HOME"]) / "provenant"
    assert f'permissions.{profile}.filesystem={{":tmpdir" = "write", {json.dumps(str(state))} = "write"}}' in recorded
    assert f"permissions.{profile}.network.enabled=false" in recorded
    assert "workspace-write" not in recorded


def test_codex_network_setting_rejects_other_values():
    result, _, _ = run_worktree_dispatch(
        "codex", CODEX_ARGV_STUB, extra_env={"CF_DISPATCH_CODEX_NETWORK": "yes"},
    )
    record = json.loads(result.output.splitlines()[-1])
    assert record["status"] == "invalid_configuration"
    assert record["provider_network"] is None


def test_worktree_writer_route_is_refused_for_assurance_intent():
    result, recorded, _ = run_worktree_dispatch(
        "claude", CLAUDE_ARGV_STUB, worktree="make", intent="assurance"
    )
    assert result.returncode == 2
    assert "requires --intent ordinary" in result.output
    assert recorded == ""


def test_worktree_writer_route_is_refused_for_unknown_adapters():
    result, recorded, _ = run_worktree_dispatch("unknown-fixture", "#!/bin/sh\necho OK\n", worktree="make")
    assert result.returncode == 2
    assert "unimplemented adapter" in result.output
    assert recorded == ""

def test_worktree_path_requires_the_writer_access_mode(tmp_path):
    worktree = make_worktree(tmp_path.resolve())
    result, recorded, _ = run_worktree_dispatch(
        "claude", CLAUDE_ARGV_STUB, extra_args=["--worktree", str(worktree)]
    )
    assert result.returncode == 2
    assert "--worktree requires --access-mode worktree_write" in result.output
    assert recorded == ""


def test_worktree_writer_route_rejects_a_path_that_is_not_a_worktree_root(tmp_path):
    plain = tmp_path.resolve() / "plain"
    plain.mkdir()
    result, recorded, _ = run_worktree_dispatch(
        "claude", CLAUDE_ARGV_STUB,
        extra_args=["--access-mode", "worktree_write", "--worktree", str(plain)],
    )
    assert result.returncode == 2
    assert "worktree" in result.output
    assert recorded == ""


ENDPOINT_STUB = """\
    #!/usr/bin/env bash
    cat >/dev/null
    printf 'base=%s token=%s api_key=%s\n' \
      "${ANTHROPIC_BASE_URL:-unset}" \
      "${ANTHROPIC_AUTH_TOKEN:-unset}" \
      "${ANTHROPIC_API_KEY-__unset__}"
"""


def test_named_endpoint_reaches_the_claude_process_environment():
    result, record, output = run_dispatch_with_stub(
        ENDPOINT_STUB,
        role="other-primary",
        extra_args=["--model", "glm-4.7"],
        extra_env={
            "CF_DISPATCH_ENDPOINT": "zai-glm",
            "ZAI_API_KEY": "endpoint-token-fixture",
            "ANTHROPIC_API_KEY": "must-not-reach-child",
        },
    )

    assert result.returncode == 0, result.output
    assert record["status"] == "ok"
    assert record["model_family"] == "zhipu"
    assert record["resolved_model"] == "glm-4.7"
    # These endpoints expose no effort control, so the dispatch carries none.
    assert record["effort"] == ""
    assert output.strip() == (
        "base=https://api.z.ai/api/anthropic "
        "token=endpoint-token-fixture "
        "api_key="
    )


def test_claude_without_a_named_endpoint_keeps_the_default_anthropic_environment():
    result, record, output = run_dispatch_with_stub(ENDPOINT_STUB, role="other-primary")

    assert result.returncode == 0, result.output
    assert record["status"] == "ok"
    assert record["model_family"] == "anthropic"
    assert output.strip() == "base=unset token=unset api_key=__unset__"


def test_openrouter_anthropic_endpoint_reaches_claude_with_blank_api_key():
    # Ordinary worker: stealth/generic-open is fine for capability, not assurance.
    result, record, output = run_dispatch_with_stub(
        ENDPOINT_STUB,
        role="worker",
        extra_args=[
            "--intent", "ordinary",
            "--model", "stealth/union-alpha",
            "--alias", "junior",
        ],
        extra_env={
            "CF_DISPATCH_ENDPOINT": "openrouter-anthropic",
            "OPENROUTER_API_KEY": "endpoint-token-fixture",
            "ANTHROPIC_API_KEY": "must-not-reach-child",
        },
    )

    assert result.returncode == 0, result.output
    assert record["status"] == "ok"
    assert record["model_family"] == "generic-open"
    assert record["resolved_model"] == "stealth/union-alpha"
    assert output.strip() == (
        "base=https://openrouter.ai/api "
        "token=endpoint-token-fixture "
        "api_key="
    )


def test_openrouter_kimi_attributes_moonshot_for_ordinary_dispatch():
    result, record, output = run_dispatch_with_stub(
        ENDPOINT_STUB,
        role="worker",
        extra_args=[
            "--intent", "ordinary",
            "--model", "moonshotai/kimi-k3",
            "--alias", "junior",
        ],
        extra_env={
            "CF_DISPATCH_ENDPOINT": "openrouter-anthropic",
            "OPENROUTER_API_KEY": "endpoint-token-fixture",
        },
    )

    assert result.returncode == 0, result.output
    assert record["status"] == "ok"
    assert record["model_family"] == "moonshot"
    assert record["resolved_model"] == "moonshotai/kimi-k3"
    assert "base=https://openrouter.ai/api" in output



CLAUDE_ENDPOINT_ARGV_STUB = """\
    #!/usr/bin/env bash
    if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
      echo '{{"loggedIn":true,"authMethod":"claude.ai"}}'
      exit 0
    fi
    printf 'base=%s\\n' "${{ANTHROPIC_BASE_URL:-unset}}" >> {args_file}
    printf 'token=%s\\n' "${{ANTHROPIC_AUTH_TOKEN:-unset}}" >> {args_file}
    cat >/dev/null
    echo "OK"
"""


def test_named_endpoint_reaches_the_claude_writer_route_environment():
    result, recorded, worktree = run_worktree_dispatch(
        "claude",
        CLAUDE_ENDPOINT_ARGV_STUB,
        worktree="make",
        extra_args=["--model", "glm-4.7"],
        extra_env={"CF_DISPATCH_ENDPOINT": "zai-glm", "ZAI_API_KEY": "endpoint-token-fixture"},
    )

    assert result.returncode == 0, result.output
    record = json.loads(result.output.splitlines()[-1])
    assert record["status"] == "ok"
    assert record["access_mode"] == "worktree_write"
    assert record["worktree"] == str(worktree)
    assert record["model_family"] == "zhipu"
    assert "base=https://api.z.ai/api/anthropic" in recorded
    assert "token=endpoint-token-fixture" in recorded


CODEX_ARGV_STUB = """\
printf '%s\\n' "$@" > {args_file}
cat >/dev/null
echo "OK"
"""

def run_codex_dispatch(extra_args=None, extra_env=None, instance_root=None):
    """Dispatch to codex with a stub that records its own argument vector."""
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = fake_bin(tmp)
        args_file = tmp / "codex.args"
        fake_codex(bin_dir, CODEX_ARGV_STUB.format(args_file=args_file))
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        if instance_root is not None:
            env["AGENT_FABRIC_INSTANCE_ROOT"] = str(instance_root)
        env.update(extra_env or {})
        command = [
            str(SCRIPT), "--tool", "codex", "--orchestrator-family", "anthropic",
            "--out", str(tmp / "out.txt"), "--prompt", "Review",
        ]
        command.extend(extra_args or [])
        result = run_cli(command, cwd=td, env=env)
        recorded = (
            args_file.read_text(encoding="utf-8").splitlines()
            if args_file.exists()
            else []
        )
        return result, recorded


CODEX_ENDPOINT_PROFILE = {
    "base_url": "https://api.deepseek.com/v1",
    "token_env": "DEEPSEEK_API_KEY",
    "model_family": "deepseek",
    "wire_api": "responses",
    "adapters": ["codex"],
}


def codex_endpoint_instance(tmp, name="deepseek-openai", profile=None):
    """An instance root whose catalogue carries one codex endpoint profile."""
    instance = tmp / "instance"
    (instance / "config").mkdir(parents=True)
    for config_name in ("model-routing.json", "model-preferences.json"):
        shutil.copy2(
            PRODUCT_ROOT / "config" / config_name, instance / "config" / config_name
        )
    catalog_path = instance / "config" / "model-routing.json"
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    catalog["endpoints"][name] = dict(profile or CODEX_ENDPOINT_PROFILE)
    catalog_path.write_text(json.dumps(catalog), encoding="utf-8")
    return instance


def test_ordinary_codex_dispatch_inherits_user_config_and_assurance_ignores_it():
    """The user's own codex config reaches an ordinary lane, as it does a native subagent."""
    result, recorded = run_codex_dispatch(extra_args=["--model", "gpt-6-luna", "--intent", "ordinary"])

    assert result.returncode == 0, result.stderr
    record = json.loads(result.stdout)
    assert record["status"] == "ok"
    assert record["model_family"] == "openai"
    assert "--ignore-user-config" not in recorded and "--ignore-rules" not in recorded
    assert 'web_search="live"' in recorded
    assert [arg for arg in recorded if arg.startswith("model_provider")] == ['model_provider="openai"']

    # Assurance is the dispatcher's default intent; it runs on a clean, recorded configuration.
    result, recorded = run_codex_dispatch(extra_args=["--model", "gpt-6-luna"])
    assert result.returncode == 0, result.stderr
    assert "--ignore-user-config" in recorded and "--ignore-rules" in recorded
    assert 'web_search="live"' not in recorded


def test_codex_endpoint_profile_supplies_the_provider_inline(tmp_path):
    instance = codex_endpoint_instance(tmp_path)
    result, recorded = run_codex_dispatch(
        extra_args=["--model", "deepseek-3.2"],
        extra_env={
            "CF_DISPATCH_ENDPOINT": "deepseek-openai",
            "DEEPSEEK_API_KEY": "endpoint-token-fixture",
        },
        instance_root=instance,
    )

    assert result.returncode == 0, result.stderr + result.stdout
    record = json.loads(result.stdout)
    assert record["status"] == "ok"
    assert record["model_family"] == "deepseek"
    assert record["resolved_model"] == "deepseek-3.2"
    # The provider the run needs arrives inline, so the route never depends on `~/.codex/config.toml`.
    assert 'model_providers.provenant_endpoint.name="deepseek-openai"' in recorded
    assert (
        'model_providers.provenant_endpoint.base_url="https://api.deepseek.com/v1"'
        in recorded
    )
    assert 'model_providers.provenant_endpoint.env_key="DEEPSEEK_API_KEY"' in recorded
    assert 'model_providers.provenant_endpoint.wire_api="responses"' in recorded
    assert 'model_provider="provenant_endpoint"' in recorded
    # The credential is named, never carried: it reaches codex through the
    # environment variable the profile names, not through an argument or record.
    assert "endpoint-token-fixture" not in "\n".join(recorded)
    assert "endpoint-token-fixture" not in result.stdout


def test_codex_endpoint_with_an_unusable_wire_api_dispatches_nothing(tmp_path):
    profile = dict(CODEX_ENDPOINT_PROFILE, wire_api="grpc")
    instance = codex_endpoint_instance(tmp_path, profile=profile)
    result, recorded = run_codex_dispatch(
        extra_args=["--model", "deepseek-3.2"],
        extra_env={
            "CF_DISPATCH_ENDPOINT": "deepseek-openai",
            "DEEPSEEK_API_KEY": "endpoint-token-fixture",
        },
        instance_root=instance,
    )

    assert result.returncode != 0
    assert json.loads(result.stdout)["status"] == "endpoint_config_invalid"
    assert recorded == []


def test_unimplemented_adapter_is_refused_before_any_provider_work(tmp_path):
    """An adapter with no executing arm is an input error, not a late refusal.

    The shell keeps only the implemented list; dispatch state lives in the
    product registry. `pi` has no catalogue entry and no arm (absent, not
    stubbed), so like any other unknown name it is refused before the
    temporary directory, prompt staging and route resolution have been paid
    for — about ten processes saved to report a typo.
    """
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    invoked = tmp / "pi.invoked"
    write_executable(
        bin_dir / "pi",
        f"#!/usr/bin/env bash\ntouch {invoked}\nexit 0\n",
    )
    env = fabric_free_env()
    out = tmp / "out.txt"
    for args in (
        ["--tool", "pi"],
        ["--tool", "not-an-adapter"],
        ["--chain", "claude:opus:low pi:pi-model:low"],
    ):
        result = run_cli(
            [
                str(SCRIPT),
                *args,
                '--orchestrator-family',
                'anthropic',
                '--out',
                str(out),
                '--prompt',
                'Review',
            ],
            cwd=tmp_path,
            env=env,
            path=[bin_dir],
        )
        assert result.returncode == 2, (args, result.stdout, result.stderr)
        assert "unimplemented adapter" in result.stderr
        assert result.stdout == ""
        assert not out.exists()
        assert not invoked.exists()


def test_opencode_arm_runs_with_explicit_model_and_reports_unsupported_variant(tmp_path):
    """Uncatalogued OpenCode models run with the effort sent as a model variant and a note."""
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    args_file = tmp / "opencode.args"
    write_executable(
        bin_dir / "opencode",
        f"""#!/usr/bin/env bash
        printf '%s\\n' "$@" > {args_file}
        echo '{{"type":"text","part":{{"text":"OPENCODE OK"}}}}'
        """,
    )
    env = fabric_free_env()
    out = tmp / "out.txt"
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'opencode', '--model', 'opencode/union-alpha', '--alias', 'junior', '--role', 'worker', '--effort', 'high', '--prompt', 'Reply with pong only.', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr
    assert record["status"] == "ok"
    assert record["resolved_model"] == "opencode/union-alpha"
    assert record["model_family"] == "generic-open"
    assert record["read_only_guarantee"] == "best_effort"
    recorded = args_file.read_text(encoding="utf-8")
    assert "run" in recorded
    assert "--format" in recorded
    assert "json" in recorded
    assert "--model" in recorded
    assert "opencode/union-alpha#high" in recorded
    assert "--variant" not in recorded
    assert "--standalone" in recorded
    assert record["provenance"]["effort_applied"] == "high"
    assert "--auto" in recorded
    assert out.read_text(encoding="utf-8") == "OPENCODE OK"
    assert '"type":"text"' in (tmp / "out.txt.raw.jsonl").read_text()


OPENCODE_EVENT_STUB = """\
    #!/usr/bin/env bash
    printf 'PWD=%s\\n' "$PWD" > {args_file}
    printf 'CONFIG=%s\\n' "$OPENCODE_CONFIG_CONTENT" >> {args_file}
    printf '%s\\n' "$@" >> {args_file}
    printf '%s\\n' '{{"type":"step_start"}}' '{{"type":"text","part":{{"text":"OK"}}}}' '{{"type":"step_finish"}}'
"""


def test_opencode_read_only_route_denies_shell_writes_and_questions():
    result, recorded, _ = run_worktree_dispatch("opencode", OPENCODE_EVENT_STUB)
    assert result.returncode == 0, result.output
    config = json.loads(next(line[7:] for line in recorded.splitlines() if line.startswith("CONFIG=")))
    assert config["permission"]["bash"]["*"] == "deny"
    assert config["permission"]["edit"]["*"] == "deny"
    assert config["permission"]["question"] == "deny"
    assert config["permission"]["external_directory"]["*"] == "deny"

def test_opencode_environment_model_overrides_adapter_default():
    result, recorded, _ = run_worktree_dispatch(
        "opencode", OPENCODE_EVENT_STUB,
        extra_env={"CF_DISPATCH_OPENCODE_MODEL": "openrouter/deepseek/deepseek-v4"},
    )
    record = json.loads(result.output.splitlines()[-1])
    assert result.returncode == 0, result.output
    assert record["resolved_model"] == "openrouter/deepseek/deepseek-v4"
    assert record["model_family"] == "deepseek"
    assert record["model_selection"] != "adapter-default"
    assert "openrouter/deepseek/deepseek-v4" in recorded


def test_opencode_writer_route_uses_owned_worktree():
    result, recorded, worktree = run_worktree_dispatch(
        "opencode", OPENCODE_EVENT_STUB, worktree="make"
    )
    record = json.loads(result.output.splitlines()[-1])
    assert result.returncode == 0, result.output
    assert record["access_mode"] == "worktree_write"
    assert record["read_only_guarantee"] == "none"
    assert f"PWD={worktree}" in recorded
    assert "--dir" not in recorded
    assert "--standalone" in recorded
    config = json.loads(recorded.split("CONFIG=", 1)[1].splitlines()[0])
    assert config["permission"]["external_directory"] == {"*": "deny"}
    assert config["permission"]["edit"] == {"*": "allow"}
    assert config["permission"]["bash"] == {"*": "allow"}


@pytest.mark.parametrize(
    "events, expected_status, diagnostic",
    [
        ('{"type":"error","error":{"name":"APIError","data":{"statusCode":403,"message":"free tier denied"}}}',
         "permission_blocked", "free tier denied"),
        ('{"type":"tool_use","part":{"state":{"status":"error","error":"blocked"}}}',
         "failed", "tool_use"),
    ],
)
def test_opencode_event_failures_are_typed_and_keep_raw_jsonl(events, expected_status, diagnostic, tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "opencode", f"#!/usr/bin/env bash\nprintf '%s\\n' '{events}'\n")
    env = fabric_free_env()
    out = tmp / "out.txt"
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'opencode', '--prompt', 'Reply OK', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == expected_status
    assert record["fix"] or record["evidence"]["signature"]
    assert diagnostic in out.read_text()
    assert events in (tmp / "out.txt.raw.jsonl").read_text()


@pytest.mark.parametrize("events,expected_status,expected_text", [
    ('null\n[1,2]\n{"type":"text","part":{"text":"OK"}}', "ok", "OK"),
    ('{"type":"error","error":"forbidden"}', "permission_blocked", "forbidden"),
    ('{"type":"error","error":{"data":null,"message":"forbidden"}}', "permission_blocked", "forbidden"),
    ('{"type":"error","error":{"data":{"statusCode":"403","message":"access denied"}}}',
     "permission_blocked", "access denied"),
])
def test_opencode_parser_accepts_non_object_json_and_error_shapes(events, expected_status, expected_text, tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "opencode", "#!/usr/bin/env bash\ncat <<'EOF'\n" + events + "\nEOF\n")
    env = fabric_free_env()
    out = tmp / "out.txt"
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'opencode', '--prompt', 'Reply OK', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    record = json.loads(result.stdout)
    assert record["status"] == expected_status, result.stderr
    assert expected_text in out.read_text()
    assert "Traceback" not in result.stderr


@pytest.mark.parametrize("provider_exit", [2, 124])
def test_opencode_provider_exit_with_text_is_not_watchdog_error(provider_exit, tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "opencode", f"""#!/usr/bin/env bash
        echo '{{"type":"text","part":{{"text":"provider detail"}}}}'
        exit {provider_exit}
    """)
    env = fabric_free_env()
    out = tmp / "out.txt"
    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'opencode', '--prompt', 'Reply OK', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
    record = json.loads(result.stdout)
    assert record["status"] == "failed"
    assert "provider detail" in out.read_text()


def test_opencode_silence_observation_keeps_provider_alive():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "opencode", '#!/usr/bin/env bash\n[ "$1" = "--version" ] && { echo 2.0.30; exit 0; }\n[ "$1" = "models" ] && exit 1\necho started\nsleep 2\necho finished\n')
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        env["CF_DISPATCH_IDLE_SECONDS"] = "oops"  # Retired; ignored even when invalid.
        env["PROVENANT_KNOB_LANES_SILENCE_SECONDS"] = "1"
        out = tmp / "out.txt"
        result = run_bounded(
            [str(SCRIPT), "--intent", "ordinary", "--tool", "opencode",
             "--prompt", "Reply OK", "--out", str(out)],
            cwd=tmp, env=env, timeout_seconds=5, output_limit_bytes=1_048_576,
        )
        record = json.loads(result.output.splitlines()[-1])
        assert record["status"] == "ok"
        observation = json.loads((tmp / 'watchdog.json').read_text())
        assert observation['process_alive'] is True
        assert observation['silent_seconds'] >= 1
        assert "finished" in out.read_text()


def test_opencode_stderr_progress_keeps_writer_alive():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "opencode", """#!/usr/bin/env bash
            for i in 1 2 3 4 5 6; do
              echo "test progress $i" >&2
              sleep 0.3
            done
            echo '{"type":"text","part":{"text":"tests passed"}}'
        """)
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        env["PROVENANT_KNOB_LANES_SILENCE_SECONDS"] = "1"
        out = tmp / "out.txt"
        worktree = make_worktree(tmp)
        result = subprocess.run(
            [str(SCRIPT), "--intent", "ordinary", "--tool", "opencode",
             "--access-mode", "worktree_write", "--worktree", str(worktree),
             "--prompt", "Run tests", "--out", str(out)],
            cwd=tmp, env=env, text=True, capture_output=True,
        )
        assert result.returncode == 0, result.stderr + result.stdout
        assert json.loads(result.stdout)["status"] == "ok"
        assert out.read_text() == "tests passed"
        assert not (tmp / 'watchdog.json').exists()


def test_opencode_dispatch_group_signal_terminates_provider_and_grandchild():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "opencode", f"""#!/usr/bin/env bash
            [ "$1" = "--version" ] && {{ echo 2.0.30; exit 0; }}
            echo "$$" > "{tmp / 'provider.pid'}"
            trap 'touch "{tmp / 'provider.stopped'}"; exit 143' TERM
            (trap 'touch "{tmp / 'grandchild.stopped'}"; exit 143' TERM; touch "{tmp / 'ready'}"; while :; do sleep 0.1; done) &
            wait
        """)
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        process = subprocess.Popen(
            [str(SCRIPT), "--intent", "ordinary", "--tool", "opencode",
             "--prompt", "Reply OK", "--out", str(tmp / "out.txt")],
            cwd=tmp, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 10
            while not (tmp / "ready").exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            assert (tmp / "ready").exists()
            os.killpg(process.pid, signal.SIGTERM)
            process.communicate(timeout=5)
            assert (tmp / "provider.stopped").exists()
            assert (tmp / "grandchild.stopped").exists()
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
            provider_pid = tmp / "provider.pid"
            if provider_pid.exists():
                try:
                    os.killpg(int(provider_pid.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass


def test_opencode_normal_exit_reaps_grandchild():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "opencode", f"""#!/usr/bin/env bash
            [ "$1" = "--version" ] && {{ echo 2.0.30; exit 0; }}
            [ "$1" = "models" ] && exit 1
            echo "$$" > "{tmp / 'provider.pid'}"
            (trap 'touch "{tmp / 'grandchild.stopped'}"; exit 143' TERM; touch "{tmp / 'ready'}"; while :; do sleep 0.1; done) &
            while [ ! -f "{tmp / 'ready'}" ]; do sleep 0.05; done
            echo '{{"type":"text","part":{{"text":"OK"}}}}'
        """)
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        try:
            result = subprocess.run(
                [str(SCRIPT), "--intent", "ordinary", "--tool", "opencode",
                 "--prompt", "Reply OK", "--out", str(tmp / "out.txt")],
                cwd=tmp, env=env, text=True, capture_output=True, timeout=10,
            )
            assert json.loads(result.stdout)["status"] == "ok"
            assert (tmp / "grandchild.stopped").exists()
        finally:
            provider_pid = tmp / "provider.pid"
            if provider_pid.exists():
                try:
                    os.killpg(int(provider_pid.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass


def test_oversized_argv_prompt_is_typed_for_cursor(tmp_path):
    """cursor takes the prompt as one argv value, like agy.

    Without a bound the kernel refuses the exec with E2BIG and the caller sees
    an untyped error, or worse a brief the kernel silently clipped.
    """
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    invoked = tmp / "cursor.invoked"
    write_executable(
        bin_dir / "cursor-agent",
        f"#!/usr/bin/env bash\ntouch {invoked}\necho 'SHOULD NOT RUN'\n",
    )
    big_prompt = tmp / "big-prompt.txt"
    big_prompt.write_text("x" * 200_000, encoding="utf-8")
    env = fabric_free_env()
    out = tmp / "out.txt"
    result = run_cli(
        [
            str(SCRIPT),
            '--tool',
            'cursor',
            '--model',
            'cursor-grok-4.5-high',
            '--orchestrator-family',
            'openai',
            '--out',
            str(out),
            '--prompt-file',
            str(big_prompt),
        ],
        cwd=tmp_path,
        env=env,
        path=[bin_dir],
    )
    record = json.loads(result.stdout)
    assert result.returncode != 0
    assert record["status"] == "rejected", record
    assert record["certification_eligible"] is False
    assert not invoked.exists()
    assert not out.exists() or "SHOULD NOT RUN" not in out.read_text(encoding="utf-8")


def test_every_argv_prompt_adapter_bounds_the_prompt():
    import importlib
    module = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    for adapter in ("agy", "cursor", "kiro", "copilot", "opencode"):
        with pytest.raises(ValueError, match="prompt_too_large"):
            module.build_plan(adapter, {"resolved_model":"fixture"}, "X"*100000)

def test_timeout_seconds_reaches_the_agy_print_timeout(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    args_file = tmp / "agy.args"
    write_executable(
        bin_dir / "agy",
        f"""#!/usr/bin/env bash
        if [ "$1" = "models" ]; then
          printf 'gemini-3.7-flash-high\ngemini-3.7-flash-medium\ngemini-3.7-flash-low\n'
          exit 0
        fi
        printf '%s\n' "$@" > {args_file}
        printf '%s\n' '{{"status":"SUCCESS","response":"AGY OK"}}'
        """,
    )
    out = tmp / "out.txt"
    env = fabric_free_env()
    # The environment override must lose to the caller's explicit deadline,
    # because the caller is the one enforcing it.
    env["CF_DISPATCH_AGY_TIMEOUT"] = "900s"

    result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--orchestrator-family', 'openai', '--role', 'worker', '--model', 'gemini-3.7-flash', '--effort', 'medium', '--timeout-seconds', '7', '--prompt', 'Review', '--out', str(out)], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])

    record = json.loads(result.stdout)
    assert result.returncode == 0, result.stderr + result.stdout
    assert record["status"] == "ok"
    passed = args_file.read_text(encoding="utf-8").splitlines()
    assert "--print-timeout" in passed
    assert passed[passed.index("--print-timeout") + 1] == "7s"


def test_timeout_seconds_rejects_a_value_that_is_not_a_positive_integer(tmp_path):
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "agy", "#!/usr/bin/env bash\necho 'SHOULD NOT RUN' >&2\nexit 1\n")
    env = fabric_free_env()
    for value in ("0", "-5", "3.5", "7s", "1e3"):
        result = run_cli([str(SCRIPT), '--intent', 'ordinary', '--tool', 'agy', '--orchestrator-family', 'openai', '--role', 'worker', '--timeout-seconds', value, '--prompt', 'Review'], cwd=tmp, env=env, path=[bin_dir, PRODUCT_ROOT / "scripts"])
        assert result.returncode == 2, value
        assert "invalid timeout-seconds" in result.stderr, value
        assert "SHOULD NOT RUN" not in result.stderr, value


def test_agy_denied_actions_never_publish_partial_success(tmp_path):
    cases = [
        ({}, "ok"),
        ({"denied_actions": None}, "ok"),
        ({"denied_actions": []}, "ok"),
        ({"denied_actions": ["fixture permission refusal"]}, "permission_blocked"),
        ({"denied_actions": [{"tool": "fixture-read", "permission": "unsandboxed"}]}, "permission_blocked"),
        ({"denied_actions": "refused"}, "failed"),
        ({"denied_actions": False}, "failed"),
        ({"denied_actions": {}}, "failed"),
    ]
    tmp = tmp_path
    bin_dir = fake_bin(tmp)
    write_executable(bin_dir / "agy", """#!/usr/bin/env bash
    if [ "$1" = "models" ]; then
      printf 'gemini-3.8-flash-low\\n'
      exit 0
    fi
    printf '%s' "$AGY_TEST_ENVELOPE"
    """)
    env = fabric_free_env()
    for index, (extra, expected) in enumerate(cases):
        env["AGY_TEST_ENVELOPE"] = json.dumps({"status": "SUCCESS", "response": "PARTIAL RESULT", **extra})
        out = tmp / f"result-{index}.txt"
        result = run_cli(
            [
                str(SCRIPT),
                '--intent',
                'ordinary',
                '--tool',
                'agy',
                '--model',
                'gemini-3.8-flash',
                '--effort',
                'low',
                '--out',
                str(out),
                '--prompt',
                'Read the fixture',
            ],
            cwd=tmp,
            env=env,
            path=[bin_dir],
        )
        record = json.loads(result.stdout)
        assert record["status"] == expected, (extra, record)
        assert (result.returncode == 0) == (expected == "ok")
        if expected == "ok":
            assert out.read_text() == "PARTIAL RESULT"
        else:
            assert "PARTIAL RESULT" not in out.read_text()
            assert record["certification_eligible"] is False
            if expected == "permission_blocked":
                assert "denied_actions" in out.read_text()


def test_agy_sandbox_setting_is_explicit_and_assurance_stays_bounded(tmp_path):
    tmp = tmp_path / "agy workspace x"
    tmp.mkdir()
    bin_dir = fake_bin(tmp)
    args_file = tmp / "args.json"
    prompt = "Read the fixture: café\n\n"
    prompt_file = tmp / "prompt.txt"
    prompt_file.write_text(prompt, encoding="utf-8")
    write_executable(bin_dir / "agy", f"""#!/usr/bin/env python3
import json,sys
from pathlib import Path
if sys.argv[1:] == ['models']:
    print('gemini-3.8-flash-low')
else:
    Path({str(args_file)!r}).write_text(json.dumps(sys.argv[1:]))
    print(json.dumps({{'status':'SUCCESS','response':'READ OK'}}))
""")
    for index, (intent, setting, expected) in enumerate([
        ("ordinary", None, False), ("ordinary", "0", False),
        ("ordinary", "1", True), ("assurance", None, True),
        ("assurance", "0", True), ("ordinary", "false", None),
        ("ordinary", "", None),
    ]):
        env = fabric_free_env()
        env.pop("CF_DISPATCH_AGY_SANDBOX", None)
        if setting is not None:
            env["CF_DISPATCH_AGY_SANDBOX"] = setting
        if args_file.exists():
            args_file.unlink()
        result = run_cli(
            [
                str(SCRIPT),
                "--intent",
                intent,
                "--tool",
                "agy",
                "--alias",
                "junior",
                "--orchestrator-family",
                "openai",
                "--out",
                str(tmp / f"out-{index}"),
                "--prompt-file",
                str(prompt_file),
            ],
            cwd=tmp,
            env=env,
            path=[bin_dir],
        )
        record = json.loads(result.stdout)
        if expected is None:
            assert result.returncode != 0
            assert record["status"] == "invalid_configuration"
            assert not args_file.exists()
            continue
        assert result.returncode == 0, result.stdout + result.stderr
        argv = json.loads(args_file.read_text())
        assert ("--sandbox" in argv) is expected
        assert "--dangerously-skip-permissions" not in argv
        effective_prompt = argv[argv.index("--print") + 1]
        assert effective_prompt.startswith(f"Workspace root: {tmp.resolve()}\n")
        assert "Do not modify files or run commands that mutate state." in effective_prompt
        assert effective_prompt.endswith(prompt)
        assert record["provider_sandbox"] is expected
        assert record["read_only_guarantee"] == "prompt_only"
        assert record["certification_eligible"] is False
