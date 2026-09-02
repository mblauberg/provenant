#!/usr/bin/env python3
"""Contract tests for cf_dispatch.sh, the provider adapter, with stubbed CLIs."""
import json
import os
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
from pathlib import Path

PRODUCT_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = PRODUCT_ROOT / "skills" / "orchestrate" / "scripts"
SCRIPT = SCRIPTS / "cf_dispatch.sh"
RUN_DIR_SCRIPT = SCRIPTS / "run_dir_init.sh"

sys.path.insert(0, str(PRODUCT_ROOT / "skills"))
from _shared.bounded_process import run_bounded

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
REQUIRED_GATE_ROWS = [
    "P0/P1 findings triaged or explicitly deferred",
    "status=ok, cross_family=true, and read_only_guarantee=enforced/oauth_safe_mode",
    "CROSS-FAMILY-NOT-RUN reasons recorded",
    "Advisory cross-family findings triaged and either verified or rejected",
    "Document update wave run or explicitly N/A",
    "Updated docs verified against current source/artifacts",
]


def write_executable(path, body):
    path.write_text(textwrap.dedent(body), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def test_output_install_replaces_symlink_without_overwriting_target():
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "codex",
            """#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
              exit 0
            fi
            cat >/dev/null
            printf 'safe output\n'
            """,
        )
        prompt = root / "prompt.md"
        prompt.write_text("test\n", encoding="utf-8")
        outside = root / "outside"
        outside.write_text("unchanged\n", encoding="utf-8")
        out = root / "result.md"
        out.symlink_to(outside)
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [str(SCRIPT), "--intent", "ordinary", "--tool", "codex",
             "--prompt-file", str(prompt), "--out", str(out),
             "--alias", "workhorse", "--role", "worker"],
            cwd=root, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        assert result.returncode == 0, result.stderr + result.stdout
        assert outside.read_text(encoding="utf-8") == "unchanged\n"
        assert not out.is_symlink()
        assert out.read_text(encoding="utf-8") == "safe output\n"
        record = json.loads(result.stdout)
        assert record["output_digest"] == "sha256:" + __import__("hashlib").sha256(
            out.read_bytes()
        ).hexdigest()


def test_directory_symlink_output_is_rejected_without_escaping():
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "codex",
            """#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
              exit 0
            fi
            cat >/dev/null
            printf 'safe output\n'
            """,
        )
        prompt = root / "prompt.md"
        prompt.write_text("test\n", encoding="utf-8")
        outside = root / "outside"
        outside.mkdir()
        out = root / "result.md"
        out.symlink_to(outside, target_is_directory=True)
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "codex",
                "--prompt-file", str(prompt), "--out", str(out),
                "--alias", "workhorse", "--role", "worker",
            ],
            cwd=root, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "output_write_error"
        assert record["output_path"] == ""
        assert record["output_digest"] == ""
        assert record["certification_eligible"] is False
        assert list(outside.iterdir()) == []


def test_symlinked_output_parent_is_rejected_without_escaping():
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "codex",
            """#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
              exit 0
            fi
            cat >/dev/null
            printf 'MUST STAY INSIDE\n'
            """,
        )
        prompt = root / "prompt.md"
        prompt.write_text("test\n", encoding="utf-8")
        outside = root / "outside"
        outside.mkdir()
        linked = root / "linked"
        linked.symlink_to(outside, target_is_directory=True)
        out = linked / "result.md"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "codex",
                "--prompt-file", str(prompt), "--out", str(out),
                "--alias", "workhorse", "--role", "worker",
            ],
            cwd=root, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "output_write_error"
        assert record["output_path"] == ""
        assert record["output_digest"] == ""
        assert record["certification_eligible"] is False
        assert list(outside.iterdir()) == []


def test_output_parent_swap_cannot_certify_an_identical_outside_file():
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        payload = root / "payload"
        payload.write_bytes(b"A" * (32 * 1024 * 1024))
        write_executable(
            bin_dir / "codex",
            """#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
              exit 0
            fi
            cat >/dev/null
            cat "$CF_DISPATCH_TEST_PAYLOAD"
            """,
        )
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        env["CF_DISPATCH_TEST_PAYLOAD"] = str(payload)
        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "assurance", "--tool", "codex",
                "--orchestrator-family", "anthropic",
                "--prompt-file", str(prompt), "--out", str(out),
                "--alias", "workhorse", "--role", "worker",
            ],
            cwd=root, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        watcher.join(timeout=16)

        assert swapped.is_set(), "watcher did not replace the installed output parent"
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "output_identity_invalid"
        assert record["output_path"] == ""
        assert record["output_digest"] == ""
        assert record["certification_eligible"] is False
        assert out.resolve() == outside_output.resolve()


def fabric_free_env():
    # Keep the evals on this checkout's fused fixtures rather than an inherited
    # developer instance or the operator's default ~/.agents instance.
    env = {
        key: value
        for key, value in os.environ.items()
        if key != "AGENTS_HOME" and not key.startswith("AGENT_FABRIC_")
    }
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(PRODUCT_ROOT)
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(PRODUCT_ROOT)
    return env


def run_dispatch_with_stub(
    stub,
    role="reviewer",
    extra_args=None,
    provenant_stub=None,
):
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "claude", stub)
        if provenant_stub is not None:
            write_executable(bin_dir / "provenant", provenant_stub)
        out = tmp / "out.txt"
        # PATH precedence keeps the checkout's stubs first.
        env = fabric_free_env()
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
        record = json.loads(result.output)
        return result, record, out.read_text(encoding="utf-8") if out.exists() else ""


def test_invalid_routing_output_returns_a_structured_failure_record():
    stub = """\
        #!/usr/bin/env bash
        echo "provider must not run" >&2
        exit 9
    """
    result, record, _ = run_dispatch_with_stub(
        stub,
        provenant_stub="""\
            #!/usr/bin/env bash
            exit 127
        """,
    )

    assert result.returncode != 0
    assert record["status"] == "routing_record_invalid"
    assert record["read_only_guarantee"] == "none"


def test_non_ok_routing_record_never_launches_provider_even_with_zero_exit():
    with tempfile.TemporaryDirectory() as td:
        invoked = Path(td) / "provider.invoked"
        stub = f"""\
            #!/usr/bin/env bash
            touch {shlex.quote(str(invoked))}
            cat >/dev/null
            printf 'provider must not run\n'
        """
        result, record, _ = run_dispatch_with_stub(
            stub,
            provenant_stub="""\
                #!/usr/bin/env bash
                printf '%s\n' '{"status":"adapter_disabled","reason":"configured off","adapter_enabled":false,"resolved_model":"opus","model_family":"anthropic","endpoint_provider":"disabled","identity_source":"runtime-configuration","alias":"workhorse"}'
                exit 0
            """,
        )

        assert result.returncode != 0
        assert record["status"] == "adapter_disabled"
        assert record["reason"] == "configured off"
        assert record["read_only_guarantee"] == "none"
        assert not invoked.exists()


def test_ok_routing_record_requires_complete_execution_identity():
    complete_route = {
        "status": "ok",
        "resolved_model": "opus",
        "model_family": "anthropic",
        "endpoint_provider": "anthropic",
        "identity_source": "runtime-configuration",
        "alias": "workhorse",
    }
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
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
                provenant_stub=f"""\
                    #!/usr/bin/env bash
                    printf '%s\\n' {route_payload}
                    exit 0
                """,
            )

            assert result.returncode != 0, missing_field
            assert record["status"] == "routing_record_invalid", missing_field
            assert record["read_only_guarantee"] == "none", missing_field
            assert not invoked.exists(), missing_field


def test_claude_other_primary_uses_opus_without_implicit_fable_route():
    stub = """\
        #!/usr/bin/env bash
        model=""
        while [ $# -gt 0 ]; do
          if [ "$1" = "--model" ]; then model="$2"; shift 2; else shift; fi
        done
        cat >/dev/null
        [ "$model" = "opus" ] || exit 9
        echo "OPUS OK"
    """
    result, record, output = run_dispatch_with_stub(stub, role="other-primary")
    assert result.returncode == 0, result.output
    assert record["resolved_model"] == "opus"
    assert record["requested_model"] == "opus"
    # The role `other-primary` takes the workhorse default, whose anthropic
    # candidates are opus then sonnet, so a fallback exists. What this test pins is
    # that it is not fable: a crucial-tier model must never be reached implicitly.
    assert record["fallback_model"] == "sonnet"
    assert record["identity_source"] == "dated-catalog"
    assert record["substitution"] == ""
    assert output.strip() == "OPUS OK"


def test_claude_crucial_synthesis_dispatches_explicit_fable_override():
    stub = """\
        #!/usr/bin/env bash
        model=""
        while [ $# -gt 0 ]; do
          if [ "$1" = "--model" ]; then model="$2"; shift 2; else shift; fi
        done
        cat >/dev/null
        [ "$model" = "fable" ] || exit 9
        echo "FABLE OK"
    """
    result, record, output = run_dispatch_with_stub(
        stub,
        role="synthesis",
        extra_args=[
            "--risk-tier", "crucial", "--model-override-tier", "crucial",
            "--model", "fable", "--effort", "medium",
        ],
    )
    assert result.returncode == 0, result.output
    assert record["resolved_model"] == "fable"
    assert record["risk_tier"] == "crucial"
    assert record["model_override_tier"] == "crucial"
    assert record["policy_override"] == "crucial-fable-synthesis-adjudication"
    assert output.strip() == "FABLE OK"


def test_task_class_route_accepts_lifecycle_risk_without_model_override():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "codex",
            """#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"low"}]}]}'
              exit 0
            fi
            cat >/dev/null
            printf 'TASK RISK OK\n'
            """,
        )
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "codex",
                "--orchestrator-family", "anthropic", "--task-class", "mechanical",
                "--role", "worker", "--risk-tier", "routine",
                "--prompt", "Review", "--out", str(out),
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr + result.stdout
        assert record["status"] == "ok"
        assert record["route_alias"] == "scout"
        assert record["risk_tier"] == "routine"
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


def test_claude_bare_oauth_model_fallback_reuses_verifier_contract():
    with tempfile.TemporaryDirectory() as td:
        args_file = Path(td) / "claude.args"
        stub = f"""\
            #!/usr/bin/env bash
            if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
              echo '{{"loggedIn":true,"authMethod":"claude.ai"}}'
              exit 0
            fi
            printf '%s\\n' "$@" >> {args_file}
            printf 'CLAUDE_CODE_DISABLE_WORKFLOWS=%s\\n' "$CLAUDE_CODE_DISABLE_WORKFLOWS" >> {args_file}
            printf '%s\\n' '--END-INVOCATION--' >> {args_file}
            model=""; safe=0; bare=0
            while [ $# -gt 0 ]; do
              case "$1" in
                --model) model="$2"; shift 2 ;;
                --safe-mode) safe=1; shift ;;
                --bare) bare=1; shift ;;
                *) shift ;;
              esac
            done
            cat >/dev/null
            if [ "$bare" = 1 ]; then echo "Not logged in" >&2; exit 1; fi
            if [ "$safe" = 1 ] && [ "$model" = "opus" ]; then echo "model opus is not available" >&2; exit 1; fi
            if [ "$safe" = 1 ] && [ "$model" = "sonnet" ]; then echo "SAFE SONNET"; exit 0; fi
            exit 9
        """
        result, record, output = run_dispatch_with_stub(stub, role="other-primary")
        assert result.returncode == 0, result.output
        assert record["resolved_model"] == "sonnet"
        assert record["requested_model"] == "opus"
        assert record["fallback_model"] == "sonnet"
        assert record["identity_source"] == "runtime-provider-fallback"
        assert "opus unavailable; used sonnet" in record["substitution"]
        assert record["read_only_guarantee"] == "oauth_safe_mode"
        assert output.strip() == "SAFE SONNET"
        invocations = args_file.read_text(encoding="utf-8").split("--END-INVOCATION--\n")[:-1]
        assert len(invocations) == 3
        assert "--bare" in invocations[0]
        assert "--safe-mode" in invocations[1]
        assert "--safe-mode" in invocations[2]
        for invocation in invocations:
            assert "--disable-slash-commands" in invocation
            assert "--no-session-persistence" in invocation
            assert "--permission-mode\nplan" in invocation
            assert "--tools\nRead,Grep,Glob" in invocation
            assert "--system-prompt" in invocation
            assert "Fabric MCP tools are not exposed" in invocation
            assert "Return only the file-backed verification result" in invocation
            assert "caller owns any Fabric correlation" in invocation
            assert "independent verifier" in invocation
            assert "cross-family verifier" not in invocation
            assert "CLAUDE_CODE_DISABLE_WORKFLOWS=1" in invocation
        assert "--model\nopus" in invocations[0]
        assert "--model\nopus" in invocations[1]
        assert "--model\nsonnet" in invocations[2]


def test_claude_tool_not_found_keeps_diagnostic_instead_of_retrying_fallback():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        misleading_path = tmp / "model-unavailable"
        misleading_path.mkdir()
        home = tmp / "home"
        home.mkdir()
        bash_env = tmp / "bash-env"
        bash_env.write_text(
            "command() {\n"
            "  if [ \"$1\" = \"-v\" ] && [ \"$2\" = \"claude\" ]; then return 1; fi\n"
            "  builtin command \"$@\"\n"
            "}\n",
            encoding="utf-8",
        )
        env = fabric_free_env()
        env["HOME"] = str(home)
        env["BASH_ENV"] = str(bash_env)
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
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool", "claude",
                "--orchestrator-family", "codex",
                "--role", "other-primary",
                "--out", str(out),
                "--prompt", "Reply exactly OK",
            ],
            cwd=tmp,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "tool_not_found"
        assert record["fallback_model"] == "sonnet"
        assert "claude not found. PATH=" in out.read_text(encoding="utf-8")
        assert "model-unavailable" in out.read_text(encoding="utf-8")


def test_help_exits_cleanly():
    result = subprocess.run(
        [str(SCRIPT), "--help"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 0
    assert "caller records any Fabric correlation" in result.stdout
    assert "--doctor" in result.stdout


def test_doctor_exits_cleanly():
    result = subprocess.run(
        [str(SCRIPT), "--doctor"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 0
    assert "cf_dispatch doctor" in result.stdout
    assert "PATH=" in result.stdout
    assert "agy=" in result.stdout


def test_missing_option_value_is_clean_error():
    result = subprocess.run(
        [str(SCRIPT), "--tool"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 2
    assert "missing value for --tool" in result.stderr
    assert "unbound variable" not in result.stderr


def test_missing_prompt_file_is_clean_error():
    result = subprocess.run(
        [str(SCRIPT), "--tool", "claude", "--orchestrator-family", "codex", "--prompt-file", "/no/such/file"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 2
    assert "cannot read prompt file: /no/such/file" in result.stderr


def test_claude_oauth_fallback_after_bare_auth_failure():
    stub = """\
        #!/usr/bin/env bash
        if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
          echo '{"loggedIn":true,"authMethod":"claude.ai"}'
          exit 0
        fi
        for arg in "$@"; do
          if [ "$arg" = "--bare" ]; then
            echo "Not logged in · Please run /login" >&2
            exit 1
          fi
        done
        cat >/dev/null
        echo "OK"
    """
    result, record, output = run_dispatch_with_stub(stub)
    assert result.returncode == 0, result.stderr
    assert record["status"] == "ok"
    assert record["tool"] == "claude"
    assert record["read_only_guarantee"] == "oauth_safe_mode"
    assert output.strip() == "OK"


def test_claude_oauth_fallback_uses_verifier_system_prompt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "claude.args"
        write_executable(
            bin_dir / "claude",
            f"""\
            #!/usr/bin/env bash
            if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
              echo '{{"loggedIn":true,"authMethod":"claude.ai"}}'
              exit 0
            fi
            printf '%s\\n' "$@" >> {args_file}
            printf 'CLAUDE_CODE_DISABLE_WORKFLOWS=%s\\n' "$CLAUDE_CODE_DISABLE_WORKFLOWS" >> {args_file}
            for arg in "$@"; do
              if [ "$arg" = "--bare" ]; then
                echo "Not logged in · Please run /login" >&2
                exit 1
              fi
            done
            cat >/dev/null
            echo "OK"
            """,
        )
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "claude",
                "--orchestrator-family",
                "codex",
                "--out",
                str(out),
                "--prompt",
                "Reply exactly OK",
            ],
            cwd=str(tmp),
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["status"] == "ok"
        args = args_file.read_text(encoding="utf-8")
        assert "--system-prompt" in args
        assert "--disable-slash-commands" in args
        assert "non-interactive independent verifier" in args
        assert "cross-family verifier" not in args
        assert "launch subagents" in args
        assert args.count("Fabric MCP tools are not exposed") == 2
        assert args.count("Return only the file-backed verification result") == 2
        assert args.count("caller owns any Fabric correlation") == 2
        assert "CLAUDE_CODE_DISABLE_WORKFLOWS=1" in args
        assert "Read,Grep,Glob" in args.splitlines()
        assert "Bash" not in args.splitlines()
        assert "Edit" not in args.splitlines()
        assert out.read_text(encoding="utf-8").strip() == "OK"


def test_agy_direct_route_dispatches_json_sandbox_and_file_prompt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        env["CF_DISPATCH_AGY_ADD_DIR"] = str(allowed_one)
        out = tmp / "out.txt"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool", "agy",
                "--model", "gemini-3.7-flash",
                "--effort", "medium",
                "--add-dir", str(allowed_two),
                "--orchestrator-family", "codex",
                "--out", str(out),
                "--prompt", "Reply exactly AGY OK",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
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
        assert args[args.index("--output-format") + 1] == "json"
        assert "--sandbox" in args
        assert args[args.index("--model") + 1] == "gemini-3.7-flash"
        assert args[args.index("--effort") + 1] == "medium"
        assert args.count("--add-dir") == 2, args
        assert str(allowed_one) in args
        assert str(allowed_two) in args
        # agy has no file-backed prompt input, so the prompt is one argv value.
        assert args[args.index("--print") + 1] == "Reply exactly AGY OK"


def test_agy_task_class_uses_its_runtime_capability_producer():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "agy.args"
        write_executable(
            bin_dir / "agy",
            f"""#!/usr/bin/env bash
            if [ "$1" = "models" ]; then
              printf 'gemini-3.7-flash-high\ngemini-3.7-flash-medium\ngemini-3.7-flash-low\n'
              exit 0
            fi
            printf '%s\n' "$@" > {args_file}
            printf '%s\n' '{{"status":"SUCCESS","response":"AGY TASK OK"}}'
            """,
        )
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                "--orchestrator-family", "openai", "--task-class", "mechanical",
                "--role", "worker", "--risk-tier", "substantial",
                "--prompt", "Review", "--out", str(out),
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr + result.stdout
        assert record["status"] == "ok"
        assert record["route_alias"] == "scout"
        assert record["resolved_model"] == "gemini-3.7-flash"
        assert record["provider_family"] == "google"
        assert record["effort"] == "low"
        assert record["effort_capability_source"] == "runtime-model-catalog"
        assert record["risk_tier"] == "substantial"
        assert out.read_text(encoding="utf-8") == "AGY TASK OK"


def test_agy_explicit_effort_never_claims_an_unavailable_probe():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                "--orchestrator-family", "openai", "--model", "gemini-3.7-flash",
                "--effort", "medium", "--prompt", "Review", "--out", str(out),
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr + result.stdout
        assert record["status"] == "ok"
        assert record["effort"] == "medium"
        assert record["effort_capability_source"] == "provider-unverified"


def test_claude_task_class_runs_the_subscription_capability_canary():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
                printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"OK","modelUsage":{{"claude-opus-4-8":{{"inputTokens":1}}}}}}'
                exit 0
              fi
            done
            cat >/dev/null
            printf 'CLAUDE TASK OK\n'
            """,
        )
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "claude",
                "--orchestrator-family", "openai", "--task-class", "critical-review",
                "--role", "critical-review", "--risk-tier", "crucial",
                "--prompt", "Review", "--out", str(out),
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr + result.stdout
        assert record["status"] == "ok"
        assert record["route_alias"] == "flagship"
        assert record["resolved_model"] == "claude-opus-4-8"
        assert record["provider_family"] == "anthropic"
        assert record["effort"] == "high"
        assert record["effort_capability_source"] == "provider-unverified"
        assert record["risk_tier"] == "crucial"
        assert "--output-format json" in calls.read_text(encoding="utf-8")
        assert out.read_text(encoding="utf-8") == "CLAUDE TASK OK\n"


def test_agy_oversized_prompt_fails_closed_instead_of_truncating():
    """agy takes the prompt as one argv value, so ARG_MAX is a real ceiling.

    A brief silently clipped by the kernel would be reviewed as if complete,
    which is the same class of quiet wrongness as a denied read reported as
    SUCCESS. It must fail closed instead.
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "agy",
            """\
            #!/usr/bin/env bash
            printf '%s\\n' '{"status":"SUCCESS","response":"SHOULD NOT RUN"}'
            """,
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        out = tmp / "out.txt"
        big_prompt = tmp / "big-prompt.txt"
        big_prompt.write_text("x" * 200_000, encoding="utf-8")
        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "agy",
                "--model", "gemini-3.7-flash", "--effort", "low",
                "--orchestrator-family", "anthropic",
                "--out", str(out),
                # Via --prompt-file, not --prompt: Linux caps a single argv
                # string at 128 KiB, so passing the oversized prompt directly
                # would fail in this test's own exec before cf_dispatch ran.
                "--prompt-file", str(big_prompt),
            ],
            cwd=td, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "error"
        assert record["certification_eligible"] is False
        assert "SHOULD NOT RUN" not in out.read_text(encoding="utf-8")


def test_agy_success_with_empty_response_is_non_passing():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "agy",
                "--model", "gemini-3.1-pro-high",
                "--orchestrator-family", "codex",
                "--out", str(out), "--prompt", "Reply",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "empty_output"
        assert record["certification_eligible"] is False
        # The output carries the diagnostic rather than a review body. It must
        # never be empty, or the caller cannot tell a failed dispatch from a
        # dispatch that has not run.
        written = out.read_text(encoding="utf-8")
        assert "status=empty_output" in written
        assert "provider error:" not in written


def test_agy_success_envelope_with_nonzero_process_exit_is_failure():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                "--model", "gemini-3.7-flash", "--effort", "medium",
                "--orchestrator-family", "openai", "--out", str(out),
                "--prompt", "Review",
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "error"
        assert record["exit"] == 7
        assert record["certification_eligible"] is False
        assert "MUST NOT PASS" not in out.read_text(encoding="utf-8")
        written = out.read_text(encoding="utf-8")
        assert "status=error exit=7" in written
        assert "provider error:" not in written


def test_agy_success_envelope_with_error_never_publishes_response():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                "--model", "gemini-3.7-flash", "--effort", "medium",
                "--orchestrator-family", "openai", "--out", str(out),
                "--prompt", "Review",
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "auth_or_quota_error"
        assert record["certification_eligible"] is False
        written = out.read_text(encoding="utf-8")
        assert "MUST NOT PASS" not in written
        assert "provider error: quota exceeded" in written


def test_agy_success_envelope_with_null_error_publishes_response():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                "--model", "gemini-3.7-flash", "--effort", "medium",
                "--orchestrator-family", "openai", "--out", str(out),
                "--prompt", "Review",
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr + result.stdout
        assert record["status"] == "ok"
        assert out.read_text(encoding="utf-8") == "AGY NULL OK"


def test_agy_requires_one_well_typed_json_envelope():
    invalid_envelopes = (
        '{"status":"SUCCESS","response":"MUST NOT PASS","response":"duplicate"}',
        'provider noise\n{"status":"SUCCESS","response":"MUST NOT PASS"}\n',
        '{"status":"SUCCESS","response":7}',
    )
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        for index, envelope in enumerate(invalid_envelopes):
            out = tmp / f"out-{index}.txt"
            env["AGY_TEST_ENVELOPE"] = envelope
            result = subprocess.run(
                [
                    str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                    "--model", "gemini-3.7-flash", "--effort", "medium",
                    "--orchestrator-family", "openai", "--out", str(out),
                    "--prompt", "Review",
                ],
                cwd=tmp, env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )

            record = json.loads(result.stdout)
            assert result.returncode != 0
            assert record["status"] in {"empty_output", "invalid_envelope"}
            assert record["certification_eligible"] is False
            assert out.read_text(encoding="utf-8").startswith("agy dispatch failed:")


def test_agy_rejects_non_utf8_success_envelope():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                "--model", "gemini-3.7-flash", "--effort", "medium",
                "--orchestrator-family", "openai", "--out", str(out),
                "--prompt", "Review",
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "invalid_envelope"
        assert record["certification_eligible"] is False
        assert out.read_text(encoding="utf-8").startswith(
            "agy dispatch failed: status=invalid_envelope"
        )


def test_disabled_execution_routes_keep_configured_reason_and_never_launch_provider():
    cases = (
        (
            "kiro", "deepseek-v3.2", "kiro-cli", "CF_DISPATCH_ENABLE_KIRO",
            "Provider execution is dormant until one bounded ordinary Kiro "
            "invocation and safety boundary are verified.",
        ),
        (
            "opencode", "opencode/deepseek-v4-flash-free", "opencode", "",
            "Provider execution is unavailable because the direct dispatch owner "
            "has no verified OpenCode invocation or receipt contract.",
        ),
    )
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        for tool, model, executable, escape_flag, expected_reason in cases:
            invoked = tmp / f"{tool}.invoked"
            write_executable(
                bin_dir / executable,
                f"#!/usr/bin/env bash\ntouch {invoked}\nexit 99\n",
            )
            if escape_flag:
                env[escape_flag] = "1"
            out = tmp / f"{tool}.txt"
            result = subprocess.run(
                [
                    str(SCRIPT), "--intent", "ordinary", "--tool", tool,
                    "--model", model, "--alias", "scout", "--role", "worker",
                    "--prompt", "Review", "--out", str(out),
                ],
                cwd=tmp, env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )

            record = json.loads(result.stdout)
            assert result.returncode != 0
            assert record["status"] == "adapter_disabled"
            assert record["reason"] == expected_reason
            assert record["exit"] == 1
            assert record["read_only_guarantee"] == "none"
            assert record["certification_eligible"] is False
            assert not invoked.exists()


def test_disabled_capability_adapters_are_rejected_before_their_probe_runs():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "provenant",
            """#!/usr/bin/env bash
            printf '%s\n' '{"schema_version":1,"status":"adapter_disabled","reason":"configured off","adapter_enabled":false,"endpoint_provider":"disabled"}'
            exit 1
            """,
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"

        for tool, model in (("codex", "gpt-5.6-luna"), ("agy", "gemini-3.7-flash")):
            invoked = tmp / f"{tool}.invoked"
            write_executable(
                bin_dir / tool,
                f"#!/usr/bin/env bash\ntouch {invoked}\nexit 99\n",
            )
            out = tmp / f"{tool}.txt"
            result = subprocess.run(
                [
                    str(SCRIPT), "--intent", "ordinary", "--tool", tool,
                    "--model", model, "--alias", "workhorse", "--role", "worker",
                    "--prompt", "Review", "--out", str(out),
                ],
                cwd=tmp, env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )

            record = json.loads(result.stdout)
            assert result.returncode != 0
            assert record["status"] == "adapter_disabled"
            assert record["reason"] == "configured off"
            assert not invoked.exists()


def test_prompt_file_trailing_newlines_reach_stdin_adapter_byte_for_byte():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        received = tmp / "received.bin"
        write_executable(
            bin_dir / "codex",
            f"""#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
              exit 0
            fi
            cat > {received}
            printf 'OK\n'
            """,
        )
        prompt = tmp / "prompt.bin"
        prompt.write_bytes(b"Review exactly\n\n")
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "codex",
                "--orchestrator-family", "anthropic", "--alias", "workhorse",
                "--role", "worker", "--prompt-file", str(prompt), "--out", str(out),
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        assert result.returncode == 0, result.stderr + result.stdout
        assert received.read_bytes() == prompt.read_bytes()


def test_prompt_file_trailing_newlines_reach_argv_adapter_byte_for_byte():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "agy",
                "--orchestrator-family", "openai", "--model", "gemini-3.7-flash",
                "--effort", "medium", "--prompt-file", str(prompt), "--out", str(out),
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        assert result.returncode == 0, result.stderr + result.stdout
        assert received.read_bytes() == prompt.read_bytes()


def test_nul_prompt_file_is_rejected_before_provider_execution():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "invoked"
        write_executable(bin_dir / "codex", f"#!/usr/bin/env bash\ntouch {invoked}\n")
        prompt = tmp / "prompt.bin"
        prompt.write_bytes(b"A\0B\n")
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--intent", "ordinary", "--tool", "codex",
                "--orchestrator-family", "anthropic", "--prompt-file", str(prompt),
            ],
            cwd=tmp, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        assert result.returncode == 2
        assert "prompt contains unsupported NUL bytes" in result.stderr
        assert result.stdout == ""
        assert not invoked.exists()


def test_agy_failure_preserves_the_provider_reason_in_the_output():
    """agy reports failures in the stdout envelope, not on stderr.

    Classifying the status is not enough on its own: an exhausted quota and a
    revoked credential both land on auth_or_quota_error and want opposite
    responses, so the provider's own words have to survive into the output.
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "agy",
                "--model", "gemini-3.1-pro-high",
                "--orchestrator-family", "codex",
                "--out", str(out), "--prompt", "Reply",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "auth_or_quota_error"
        assert record["certification_eligible"] is False
        written = out.read_text(encoding="utf-8")
        assert "Individual quota reached" in written
        assert "Resets in 55m39s" in written


def test_agy_failure_with_null_error_uses_generic_reason_and_never_publishes_response():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{env['PATH']}"

        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "agy",
                "--model", "gemini-3.1-pro-high",
                "--orchestrator-family", "codex",
                "--out", str(out), "--prompt", "Reply",
            ],
            cwd=td, env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "error"
        written = out.read_text(encoding="utf-8")
        assert "SHOULD NOT PASS" not in written
        assert (
            "provider error: provider returned a non-success status without an error message"
            in written
        )
        assert "provider error: None" not in written


def test_agy_permission_denial_overrides_false_success_envelope():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "agy",
                "--model", "gemini-3.1-pro-high",
                "--orchestrator-family", "codex",
                "--out", str(out), "--prompt", "Reply",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "permission_denied"
        assert "read_file" in out.read_text(encoding="utf-8")


def test_default_failure_retains_only_the_declared_output_tempfile():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        temp_root = tmp / "tmp"
        temp_root.mkdir()
        env = fabric_free_env()
        env["TMPDIR"] = str(temp_root)
        result = subprocess.run(
            [str(SCRIPT), "--tool", "kiro", "--orchestrator-family", "codex", "--prompt", "Review"],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        output = Path(record["output_path"])
        assert result.returncode != 0
        assert output.exists()
        assert [path.resolve() for path in temp_root.iterdir()] == [output.resolve()]
        output.unlink()


def test_orchestrator_family_is_required():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [str(SCRIPT), "--tool", "claude", "--prompt", "Reply exactly OK"],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "orchestrator_family_required"
        assert record["cross_family"] is False


def test_same_family_cli_is_forbidden_when_family_declared():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "codex",
                "--prompt",
                "Reply exactly OK",
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
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
        provenant_stub="""#!/usr/bin/env bash
            printf '{\"status\":\"ok\",\"alias\":\"scout\",\"resolved_model\":\"haiku\",\"model_family\":\"anthropic\",\"endpoint_provider\":\"anthropic\",\"identity_source\":\"test\"}\n'
        """,
    )
    assert result.returncode == 0, result.output
    assert record["route_alias"] == "scout"


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
        provenant_stub="""#!/usr/bin/env bash
            printf '%s\n' '{"status":"ok","alias":"flagship","resolved_model":"opus|candidate","model_family":"anthropic","endpoint_provider":"anthropic","identity_source":"test-route","requested_effort":"high","effort":"high","effort_source":"test","effort_capability_source":"test-capability"}'
        """,
    )

    assert result.returncode == 0, result.output
    assert record["resolved_model"] == "opus|candidate"
    assert record["provider_family"] == "anthropic"
    assert record["endpoint_provider"] == "anthropic"
    assert record["identity_source"] == "test-route"
    assert record["effort"] == "high"
    assert output.strip() == "STRUCTURED ROUTE OK"


def test_cursor_model_provider_prevents_disguised_same_family_review():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "cursor-agent", "#!/usr/bin/env bash\necho OK\n")
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--model",
                "gpt-5.6-sol",
                "--orchestrator-family",
                "openai",
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["provider_family"] == "openai"
        assert record["status"] == "same_family_forbidden"
        assert record["cross_family"] is False


def test_cursor_distinct_model_records_adapter_and_provider_family():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "cursor.args"
        write_executable(
            bin_dir / "cursor-agent",
            f"#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > {args_file}\necho OK\n",
        )
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--model",
                "cursor-grok-4.5-high",
                "--orchestrator-family",
                "openai",
                "--out",
                str(out),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
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


def test_explicit_output_path_preserves_adapter_failure_diagnostics():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "cursor-agent",
            "#!/usr/bin/env bash\necho 'simulated adapter failure' >&2\nexit 9\n",
        )
        out = tmp / "review.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--model",
                "cursor-grok-4.5-high",
                "--orchestrator-family",
                "openai",
                "--out",
                str(out),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "error"
        assert record["output_path"] == str(out)
        assert "simulated adapter failure" in out.read_text(encoding="utf-8")


def test_unwritable_output_path_cannot_certify_success():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "codex",
            """#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{"models":[{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{"effort":"high"},{"effort":"max"},{"effort":"ultra"}]}]}'
              exit 0
            fi
            echo OK
            """,
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [str(SCRIPT), "--tool", "codex", "--orchestrator-family", "anthropic", "--out", str(tmp / "missing" / "out.txt"), "--prompt", "Review"],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "output_write_error"
        assert record["certification_eligible"] is False
        assert record["output_path"] == ""


def test_resolved_role_effort_reaches_codex_adapter_and_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "codex.args"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"high"}},{{"effort":"xhigh"}}]}}]}}'
              exit 0
            fi
            printf '%s\\n' "$@" > {args_file}
            cat >/dev/null
            echo OK
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--role",
                "critical-review",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["requested_effort"] == "max"
        assert record["effort"] == "xhigh"
        assert record["effort_capability_source"] == "runtime-model-catalog"
        assert record["resolved_model"] == "gpt-5.6-sol"
        assert record["catalog_model"] == ""
        assert record["model_selection"] == ""
        args = args_file.read_text(encoding="utf-8").splitlines()
        assert "-m" in args
        assert "gpt-5.6-sol" in args
        assert "service_tier=default" in args
        assert "model_reasoning_effort=xhigh" in args


def test_bare_codex_dispatch_defaults_to_workhorse_not_flagship():
    """A dispatch naming no alias, role or model must not land on the flagship.

    The default alias used to be flagship unconditionally, so an ordinary dispatch
    silently ran on the most expensive model in the family.
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "codex.args"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{{"effort":"medium"}},{{"effort":"high"}}]}},{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"high"}},{{"effort":"max"}}]}}]}}'
              exit 0
            fi
            printf '%s\\n' "$@" > {args_file}
            cat >/dev/null
            echo OK
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["route_alias"] == "workhorse"
        assert record["resolved_model"] == "gpt-5.6-luna"
        args = args_file.read_text(encoding="utf-8").splitlines()
        assert "gpt-5.6-luna" in args
        assert "gpt-5.6-sol" not in args
        assert "service_tier=default" in args


def test_critical_review_role_still_defaults_to_flagship():
    """The alias follows the role, so a critical review still reaches the flagship."""
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "codex.args"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{{"effort":"medium"}}]}},{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"max"}}]}}]}}'
              exit 0
            fi
            printf '%s\\n' "$@" > {args_file}
            cat >/dev/null
            echo OK
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--role",
                "critical-review",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["route_alias"] == "flagship"
        assert record["resolved_model"] == "gpt-5.6-sol"


def test_codex_capability_discovery_failure_blocks_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.exec-invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              echo "capability discovery unavailable" >&2
              exit 23
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["effort_capability_source"] == "runtime-discovery-failed"
        assert record["certification_eligible"] is False
        assert record["read_only_guarantee"] == "none"
        assert not invoked.exists()


def test_mixed_malformed_codex_capabilities_block_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.exec-invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"display_name":"missing slug"}},{{"slug":"gpt-5.6-terra","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
              exit 0
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["certification_eligible"] is False
        assert not invoked.exists()


def test_unrelated_codex_model_without_efforts_blocks_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.exec-invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-terra","supported_reasoning_levels":[]}}]}}'
              exit 0
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["certification_eligible"] is False
        assert not invoked.exists()


def test_duplicate_codex_discovery_member_blocks_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
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
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["certification_eligible"] is False
        assert not invoked.exists()


def test_codex_explicit_model_reaches_adapter_and_reports_runtime_failure():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
              exit 0
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--model",
                "gpt-5.6-sol",
                "--orchestrator-family",
                "anthropic",
                "--prompt",
                "Review",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "error"
        assert record["resolved_model"] == "gpt-5.6-sol"
        assert record["requested_model"] == "gpt-5.6-sol"
        assert record["catalog_model"] == ""
        assert record["model_selection"] == ""
        assert invoked.exists()


def test_interrupted_dispatch_cleans_internal_tempfiles():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
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
            cwd=td,
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


def test_broker_adapter_requires_resolvable_provider_family():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--orchestrator-family",
                "openai",
                "--prompt",
                "Review",
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "model_required_for_broker"
        assert record["cross_family"] is False


def test_manual_provider_override_is_not_supported():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "claude",
                "--provider-family",
                "google",
                "--orchestrator-family",
                "anthropic",
                "--prompt",
                "Review",
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert result.returncode == 2
        assert "unknown arg: --provider-family" in result.stderr


def test_invalid_orchestrator_family_fails_closed():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "claude",
                "--orchestrator-family",
                "Claude",
                "--prompt",
                "Reply exactly OK",
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
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


def test_chain_all_failed_uses_dispatch_schema():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--chain",
                "kiro copilot",
                "--orchestrator-family",
                "codex",
                "--prompt",
                "Reply exactly OK",
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["tool"] == "chain"
        assert record["status"] == "all_failed"
        assert record["read_only_guarantee"] == "none"


def test_run_dir_init_force_flag_only_creates_final_gate():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [str(RUN_DIR_SCRIPT), "--force"],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert result.returncode == 0, result.stderr
        run_dir = Path(result.stdout.strip())
        if not run_dir.is_absolute():
            run_dir = Path(td) / run_dir
        assert (run_dir / "FINAL_GATE.md").exists()
        receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text(encoding="utf-8"))
        assert receipt["status"] == "active"
        assert receipt["retention_policy"] == "capsule-plus-referenced-evidence"
        assert (run_dir / "traces" / "README.md").exists()
        gate = (run_dir / "FINAL_GATE.md").read_text(encoding="utf-8")
        for row in REQUIRED_GATE_ROWS:
            assert row in gate


def test_run_dir_init_force_does_not_clobber_existing_manifest():
    with tempfile.TemporaryDirectory() as td:
        run_dir = Path(td) / "existing"
        run_dir.mkdir()
        manifest = run_dir / "MANIFEST.md"
        manifest.write_text("KEEP\\n", encoding="utf-8")
        result = subprocess.run(
            [str(RUN_DIR_SCRIPT), str(run_dir), "--force"],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert result.returncode == 0, result.stderr
        assert manifest.read_text(encoding="utf-8") == "KEEP\\n"
        assert (run_dir / "FINAL_GATE.md").exists()
        assert (run_dir / "RUN_RECEIPT.json").exists()
        gate = (run_dir / "FINAL_GATE.md").read_text(encoding="utf-8")
        for row in REQUIRED_GATE_ROWS:
            assert row in gate


def test_non_git_fallback_routes_via_product_root_model_route():
    # From a non-git copy of the product tree, with no provenant on PATH,
    # dispatch must fall back to <product root>/scripts/model_route.py.
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        product = tmp / "product"
        shutil.copytree(
            SCRIPTS,
            product / "skills" / "orchestrate" / "scripts",
        )
        shutil.copytree(PRODUCT_ROOT / "config", product / "config")
        (product / "scripts").mkdir()
        for name in (
            "model_route.py",
            "model_route_catalog.py",
            "model_route_preferences.py",
            "worktree.py",
        ):
            shutil.copy2(PRODUCT_ROOT / "scripts" / name, product / "scripts" / name)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "claude",
            """\
            #!/usr/bin/env bash
            cat >/dev/null
            echo "OK"
            """,
        )
        write_executable(bin_dir / "python3", f'#!/bin/sh\nexec {sys.executable} "$@"\n')
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:/usr/bin:/bin"
        # cf_dispatch.sh appends $HOME/.local/bin and $HOME/bin to PATH;
        # point HOME at the sandbox so an installed provenant cannot leak in.
        env["HOME"] = str(tmp)
        instance = tmp / "instance"
        (instance / "config").mkdir(parents=True)
        for name in ("model-routing.json", "model-preferences.json"):
            shutil.copy2(product / "config" / name, instance / "config" / name)
        env["AGENT_FABRIC_INSTANCE_ROOT"] = str(instance)
        out = tmp / "out.txt"
        result = subprocess.run(
            [
                str(product / "skills" / "orchestrate" / "scripts" / "cf_dispatch.sh"),
                "--tool",
                "claude",
                "--orchestrator-family",
                "codex",
                "--role",
                "reviewer",
                "--out",
                str(out),
                "--prompt",
                "Reply exactly OK",
            ],
            cwd=str(tmp),
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["status"] == "ok"
        assert record["resolved_model"]
        assert out.read_text(encoding="utf-8").strip() == "OK"


if __name__ == "__main__":
    test_help_exits_cleanly()
    test_doctor_exits_cleanly()
    test_missing_option_value_is_clean_error()
    test_missing_prompt_file_is_clean_error()
    test_claude_oauth_fallback_after_bare_auth_failure()
    test_claude_oauth_fallback_uses_verifier_system_prompt()
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
    worktree = root / "worktree"
    worktree.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=worktree, check=True)
    return worktree.resolve()


def run_worktree_dispatch(tool, stub, worktree=None, extra_args=None, intent="ordinary"):
    """Dispatch one stubbed provider, optionally on the worktree writer route."""
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td).resolve()
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "provider.args"
        write_executable(bin_dir / tool, stub.format(args_file=args_file))
        out = tmp / "out.txt"
        env = fabric_free_env()
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

CODEX_ARGV_STUB = """\
    #!/usr/bin/env bash
    if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
      printf '{{"models":[{{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
      exit 0
    fi
    printf '%s\\n' "$@" >> {args_file}
    cat >/dev/null
    echo "OK"
"""


def test_claude_worktree_writer_route_runs_inside_the_owned_worktree():
    result, recorded, worktree = run_worktree_dispatch("claude", CLAUDE_ARGV_STUB, worktree="make")
    assert result.returncode == 0, result.output
    record = json.loads(result.output)
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
    assert "own exclusively for this run" in recorded
    assert "run commands and commit only inside that worktree" in recorded


def test_claude_read_only_route_remains_the_default():
    result, recorded, _ = run_worktree_dispatch("claude", CLAUDE_ARGV_STUB)
    assert result.returncode == 0, result.output
    record = json.loads(result.output)
    assert record["access_mode"] == "read_only"
    assert record["worktree"] == ""
    assert record["read_only_guarantee"] == "enforced"
    assert "--permission-mode\nplan" in recorded
    assert "--tools\nRead,Grep,Glob" in recorded
    assert "acceptEdits" not in recorded
    assert "--allowedTools" not in recorded
    assert "Bash" not in recorded


def test_codex_worktree_writer_route_uses_the_workspace_write_sandbox():
    result, recorded, worktree = run_worktree_dispatch("codex", CODEX_ARGV_STUB, worktree="make")
    assert result.returncode == 0, result.output
    record = json.loads(result.output)
    assert record["access_mode"] == "worktree_write"
    assert record["read_only_guarantee"] == "none"
    assert "-s\nworkspace-write" in recorded
    assert f"--cd\n{worktree}" in recorded
    assert "read-only" not in recorded
    # A linked worktree keeps its Git metadata outside the worktree root.
    assert "sandbox_workspace_write.writable_roots=" in recorded


def test_codex_read_only_route_keeps_the_read_only_sandbox():
    result, recorded, _ = run_worktree_dispatch("codex", CODEX_ARGV_STUB)
    assert result.returncode == 0, result.output
    record = json.loads(result.output)
    assert record["access_mode"] == "read_only"
    assert "-s\nread-only" in recorded
    assert "workspace-write" not in recorded


def test_worktree_writer_route_is_refused_for_assurance_intent():
    result, recorded, _ = run_worktree_dispatch(
        "claude", CLAUDE_ARGV_STUB, worktree="make", intent="assurance"
    )
    assert result.returncode == 2
    assert "requires --intent ordinary" in result.output
    assert recorded == ""


def test_worktree_writer_route_is_refused_for_unsupported_adapters():
    result, recorded, _ = run_worktree_dispatch("agy", CLAUDE_ARGV_STUB, worktree="make")
    assert result.returncode == 2
    assert "unsupported for adapter: agy" in result.output
    assert recorded == ""


def test_worktree_path_requires_the_writer_access_mode():
    with tempfile.TemporaryDirectory() as td:
        worktree = make_worktree(Path(td).resolve())
    result, recorded, _ = run_worktree_dispatch(
        "claude", CLAUDE_ARGV_STUB, extra_args=["--worktree", str(worktree)]
    )
    assert result.returncode == 2
    assert "--worktree requires --access-mode worktree_write" in result.output
    assert recorded == ""


def test_worktree_writer_route_rejects_a_path_that_is_not_a_worktree_root():
    with tempfile.TemporaryDirectory() as td:
        plain = Path(td).resolve() / "plain"
        plain.mkdir()
        result, recorded, _ = run_worktree_dispatch(
            "claude", CLAUDE_ARGV_STUB,
            extra_args=["--access-mode", "worktree_write", "--worktree", str(plain)],
        )
    assert result.returncode == 2
    assert "worktree" in result.output
    assert recorded == ""
