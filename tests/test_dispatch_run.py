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
from types import SimpleNamespace

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


def load_secret_scan_module():
    import importlib
    load_dispatch_module()
    return importlib.import_module('secret_scan')


@pytest.mark.parametrize('name,positive,placeholder', [
    ('PEM private key', '-----BEGIN RSA PRIVATE KEY-----\n' + 'A' * 64 + '\n-----END RSA PRIVATE KEY-----',
     '-----BEGIN RSA PRIVATE KEY-----\nEXAMPLE\n-----END RSA PRIVATE KEY-----'),
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


def test_secret_scan_add_dirs_includes_ignored_regular_files_and_marks_budget_exceeded(tmp_path, monkeypatch):
    scan = load_secret_scan_module()
    subprocess.run(['git', 'init', '-q'], cwd=tmp_path, check=True)
    (tmp_path / '.gitignore').write_text('ignored.txt\n.env\n')
    (tmp_path / 'ignored.txt').write_text('AKIA' + 'A' * 16)
    (tmp_path / '.env').write_text('rk_live_' + 'a' * 24)
    (tmp_path / 'tracked.txt').write_text('AKIA' + 'B' * 16)
    (tmp_path / 'extra.txt').write_text('ghp_' + 'a' * 30)
    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert {finding.name for finding in result.findings} == {'AWS access key ID', 'GitHub token', 'Stripe live key'}
    assert {Path(finding.path).name for finding in result.findings} == {
        'ignored.txt', '.env', 'tracked.txt', 'extra.txt'
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


def test_secret_scan_skips_binary_and_untracked_system_dirs_but_refuses_oversized_files(tmp_path):
    scan = load_secret_scan_module()
    secret = b'AKIA' + b'A' * 16
    (tmp_path / 'binary.dat').write_bytes(b'\0' + secret)
    (tmp_path / 'large.txt').write_bytes(secret + b'a' * scan.MAX_FILE_BYTES)
    for name in ('node_modules', '.git'):
        directory = tmp_path / name
        directory.mkdir()
        (directory / 'secret.txt').write_bytes(secret)
    result = scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)])
    assert result.findings == []
    assert result.budget_exceeded


def test_secret_scan_skips_git_metadata_file(tmp_path):
    scan = load_secret_scan_module()
    (tmp_path / '.git').write_text('AKIA' + 'A' * 16)
    assert scan.scan_inputs(b'hello', '<prompt>', [str(tmp_path)]).findings == []


def test_secret_scan_avoids_generic_credential_words_and_unprefixed_jwt():
    scan = load_secret_scan_module()
    content = b'password=' + b'a' * 60 + b'\n' + b'a' * 12 + b'.' + b'b' * 12 + b'.' + b'c' * 32
    assert scan.scan_bytes(content, '<prompt>') == []


def test_terminal_attempt_carries_nested_owner_spared_count(tmp_path: Path) -> None:
    module = load_dispatch_module()
    args = SimpleNamespace(tool="codex", alias="workhorse", model="fixture", effort="low",
                           task_id="task-1", access_mode="read_only", worktree=None,
                           _last_plan={})
    legacy = {"started_at": "2026-09-23T00:00:00Z", "finished_at": "2026-09-23T00:00:01Z",
              "outcome": "ok", "status": "ok", "result": "result.md",
              "attempt_path": "tasks/task-1/attempt-001", "requested_route": {}}
    row = module.terminal_contract(args, tmp_path, legacy,
                                   {"status": "ok", "spared": 2}, 1,
                                   tmp_path / "tasks/task-1/attempt-001")
    assert row["spared"] == 2


def test_prepare_resume_restores_previous_workspace_root(tmp_path: Path):
    module = load_dispatch_module()
    run_dir = make_run(tmp_path, "resume-root")
    attempt_dir = run_dir / "tasks/task-1/attempt-001"
    attempt_dir.mkdir(parents=True)
    workspace = tmp_path / "recorded-workspace"
    workspace.mkdir()
    previous = {
        "run_id": "resume-me", "task_id": "task-1", "attempt": 1, "state": "terminal",
        "status": "ok", "mode": "read_only", "cwd": str(workspace / "src"), "worktree": None,
        "workspace": {"root": str(workspace)}, "session_id": "saved-session",
        "provenance": {"requested": {"adapter": "codex"}, "resolved_model": "fixture", "effort_applied": ""},
        "applied": {"sandbox": "read-only", "network": None, "add_dirs": []},
        "paths": {"events": None}, "requested_route": {},
    }
    (attempt_dir / "attempt.json").write_text(json.dumps(previous), encoding="utf-8")
    args = SimpleNamespace(
        run_dir=run_dir, resume="resume-me", task_id=None, tool=None, model=None, effort=None,
        access_mode=None, worktree=None, provider_cwd=None, sandbox=None, network=None,
        add_dirs=[], resume_session=None, fallback=None, context_ceiling=None,
        intent="ordinary", orchestrator_family="", role="worker", risk_tier="",
        model_override_tier="", reviewer_id="", preface=True,
    )

    module.prepare_resume(args)

    assert args.workspace_root == workspace


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
          printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
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
    assert receipt["status"] == "ok"
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


@pytest.mark.parametrize("legacy", [False, True])
@pytest.mark.parametrize("floor", ['"invalid"', "-1", "101"])
def test_invalid_memory_floor_retains_failed_attempt_with_fix(tmp_path, monkeypatch, legacy, floor):
    run_dir = make_run(tmp_path, f"bad-floor-{legacy}-{floor}")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(bin_dir / "codex", '''#!/usr/bin/env bash
if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
  printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
  exit 0
fi
exit 99
''')
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/bad-floor/attempt-001/attempt.json").read_text())
    state = json.loads((run_dir / "tasks/bad-floor/attempt-001/attempt.json").read_text())
    assert attempt["status"] == state["status"] == "failed"
    assert state["fix"] == "Set .agents/fabric-policy.json memory_floor_percent to numbers from 0 to 100."
    assert "memory_floor_percent" in attempt["process_error"]


@pytest.mark.parametrize("legacy", [False, True])
def test_memory_wait_expiry_is_typed_and_visible(tmp_path, monkeypatch, legacy):
    run_dir = make_run(tmp_path, f"memory-expiry-{legacy}")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("hello\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(bin_dir / "codex", '''#!/usr/bin/env bash
if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
  printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
  exit 0
fi
exit 99
''')
    monkeypatch.setenv("PATH", f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    module = load_dispatch_module()
    monkeypatch.setattr(module.memory_admission, "available_memory_mb", lambda: (640, 16384))
    monkeypatch.setenv("FABRIC_MEMORY_WAIT_SECONDS", "0")
    monkeypatch.chdir(tmp_path)
    if legacy:
        adapter = tmp_path / "adapter"
        write_success_adapter(adapter)
        module.CF_DISPATCH = adapter
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "memory-expiry", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    assert module.dispatch(args) == 1
    attempt = json.loads((run_dir / "dispatch/tasks/memory-expiry/attempt-001/attempt.json").read_text())
    state = json.loads((run_dir / "tasks/memory-expiry/attempt-001/attempt.json").read_text())
    assert attempt["status"] == state["status"] == "failed"
    assert attempt["outcome"] == state["error"] == "memory_unavailable"
    assert "lower the mode's memory_floor_percent" in state["fix"]
    assert "memory_unavailable" in state["digest"]


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
    result = module.preflight_tasks([
        {"id": "model-only", "adapter": "codex", "model": "gpt-6-luna", "prompt": "hi"},
        {"id": "explicit-both", "adapter": "codex", "alias": "workhorse", "model": "gpt-6-luna", "prompt": "hi"},
    ])

    model_only, explicit_both = result["routes"]
    resolve_commands = [command for command in commands if len(command) > 2 and command[2] == "resolve"]
    assert len(resolve_commands) == 2
    assert "--alias" not in resolve_commands[0]
    assert "--model" in resolve_commands[0]
    assert model_only["notes"] == []
    assert "--alias" in resolve_commands[1]
    assert explicit_both["notes"] == ["alias and model both supplied; model won"]


def test_explicit_model_receipt_does_not_record_an_implied_alias(tmp_path):
    mod = load_dispatch_module()
    args = SimpleNamespace(
        tool="opencode", alias="flagship", model="mimo", alias_supplied=False, effort=None,
        task_id="dispatch-001", access_mode="read_only", worktree=None,
        _phase_timings={}, reviewer_id=None, risk_tier=None,
        model_override_tier=None,
    )
    row = mod.contract_row(args, tmp_path, 1, tmp_path / "attempt", {}, "now")
    assert row["provenance"]["requested"]["alias"] == ""


def test_explicit_model_and_alias_are_both_recorded_in_receipt(tmp_path):
    mod = load_dispatch_module()
    args = SimpleNamespace(
        tool="opencode", alias="workhorse", model="gpt-6-luna", alias_supplied=True,
        effort=None, task_id="dispatch-001", access_mode="read_only", worktree=None,
        _phase_timings={}, reviewer_id=None, risk_tier=None,
        model_override_tier=None,
    )
    row = mod.contract_row(args, tmp_path, 1, tmp_path / "attempt", {}, "now")
    assert row["provenance"]["requested"]["alias"] == "workhorse"


def test_workspace_identity_keeps_root_and_records_provider_cwd(tmp_path):
    mod = load_dispatch_module()
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    identity = mod.workspace_identity(root, cwd)
    assert identity["cwd"] == str(cwd)
    assert identity["root"] == str(root)


def test_workspace_identity_canonicalizes_fallback_root(tmp_path):
    mod = load_dispatch_module()
    real_root = tmp_path / "workspace"
    (real_root / "sub").mkdir(parents=True)
    workspace_link = tmp_path / "workspace-link"
    workspace_link.symlink_to(real_root, target_is_directory=True)

    identity = mod.workspace_identity(workspace_link, workspace_link / "sub")

    assert identity["root"] == str(real_root.resolve())
    assert identity["cwd"] == str((real_root / "sub").resolve())


def test_workspace_identity_keeps_canonical_caller_root_inside_git_checkout(tmp_path, monkeypatch):
    mod = load_dispatch_module()
    repo = tmp_path / "repo"
    workspace = repo / "workspace"
    provider_cwd = workspace / "sub"
    provider_cwd.mkdir(parents=True)
    monkeypatch.setattr(
        mod.subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(
            args[0], 0,
            f"{repo}\n{'a' * 40}\n" if args[0][1:3] == ["rev-parse", "--show-toplevel"] else "",
            "",
        ),
    )

    identity = mod.workspace_identity(workspace, provider_cwd)

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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "reviewer",
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
    module.reconcile_manifest(run_dir)


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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "reviewer",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "reviewer",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)

    assert module.dispatch(args) == (0 if status == "ok" else 1)
    attempt = json.loads((run_dir / "dispatch/tasks/case/attempt-001/attempt.json").read_text())
    assert attempt["status"] == status
    assert attempt["outcome"] == outcome
    if status == "ok":
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
          printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
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
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(ROOT)

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
          printf '{{"models":[{{"slug":"gpt-6-luna","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
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
          printf '{"models":[{"slug":"gpt-6-sol","supported_reasoning_levels":[{"effort":"high"}]},{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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

    with pytest.raises(module.AttemptEvidenceError, match="successful adapter receipt is invalid"):
        module.reconcile_manifest(run_dir)


@pytest.mark.parametrize("exit_code", [False, 0.0])
def test_reentry_rejects_non_integer_zero_exit(tmp_path: Path, monkeypatch, exit_code: object) -> None:
    """Re-entry requires the exact terminal exit proof before repairing a manifest."""
    run_dir = make_run(tmp_path, "reentry-invalid-exit")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("revalidate retained success\n", encoding="utf-8")
    adapter = tmp_path / "success-adapter"
    write_success_adapter(adapter)
    module = load_dispatch_module()
    monkeypatch.setattr(module, "CF_DISPATCH", adapter)
    monkeypatch.chdir(tmp_path)
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "reentry", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    assert module.dispatch(args) == 0
    attempt = run_dir / "dispatch/tasks/reentry/attempt-001/attempt.json"
    record = json.loads(attempt.read_text(encoding="utf-8"))
    record["process"]["exit_code"] = exit_code
    attempt.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    attempt.with_name("attempt.sha256").write_text(
        "sha256:" + hashlib.sha256(attempt.read_bytes()).hexdigest() + "  attempt.json\n",
        encoding="utf-8",
    )

    with pytest.raises(module.AttemptEvidenceError, match="successful attempt does not prove exit 0"):
        module.reconcile_manifest(run_dir)


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
          printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
          exit 0
        fi
        sleep 10
        """,
    )
    env = os.environ.copy()
    env["PROVENANT_PREFLIGHT_ROUTES"] = json.dumps({"timeout": {"adapter": "codex", "alias": "workhorse", "resolved_model": "gpt-6-luna", "effort": "high"}})
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
          printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
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
        original = module.write_owned
        fired = False
        def publish(run_dir, path, content):
            global fired
            original(run_dir, path, content)
            if path == run_dir / "dispatch/tasks/late/attempt-001/attempt.json" and not fired:
                fired = True
                os.kill(os.getpid(), signal.SIGTERM)
        module.write_owned = publish
        raise SystemExit(module.dispatch(module.parser().parse_args(sys.argv[1:])))
    """), encoding="utf-8")
    result = subprocess.run([
        sys.executable, str(driver), "--run-dir", str(run_dir), "--task-id", "late",
        "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--adapter", "codex", "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
    assert next_record["status"] == "ok"


def test_attempt_rows_are_accepted_by_existing_finalizer(tmp_path: Path) -> None:
    run_dir = make_run(tmp_path, "finalizer")
    prompt = tmp_path / "prompt.md"
    prompt.write_text("finalizer\n", encoding="utf-8")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(bin_dir / "codex", """#!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"high"}]}]}'
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
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "blocked", "--adapter", "opencode",
        "--prompt-file", "private/prompt.md", "--model", "opencode/mimo-v2.6-flash-free",
        "--role", "worker",
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
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "blocked", "--adapter", "opencode",
        "--prompt-file", "prompt.md", "--add-dir", "private",
        "--model", "opencode/mimo-v2.6-flash-free", "--role", "worker",
    ])
    assert module.dispatch(args) == 2
    assert "protected path" in str(json.loads(capsys.readouterr().out))
    assert not (run_dir / "dispatch/tasks/blocked/attempt-001/prompt.md").exists()


def test_hard_linked_prompt_reports_typed_custody_error(tmp_path: Path, monkeypatch, capsys) -> None:
    run_dir = make_run(tmp_path, "hardlink-typed")
    source = tmp_path / "prompt.md"
    source.write_text("secret boundary\n", encoding="utf-8")
    linked = tmp_path / "linked-prompt.md"
    linked.hardlink_to(source)
    module = load_dispatch_module()
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "hardlink-typed", "--adapter", "codex",
        "--prompt-file", str(linked), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 2
    assert json.loads(capsys.readouterr().out)["status"] == "prompt_hard_link_denied"


def test_missing_run_receipt_reports_missing_custody(tmp_path: Path, monkeypatch, capsys) -> None:
    run_dir = make_run(tmp_path, "missing-receipt")
    (run_dir / "RUN_RECEIPT.json").unlink()
    prompt = tmp_path / "prompt.md"
    prompt.write_text("missing receipt\n", encoding="utf-8")
    module = load_dispatch_module()
    args = module.parser().parse_args([
        "--run-dir", str(run_dir), "--task-id", "missing-receipt", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
    ])
    monkeypatch.chdir(tmp_path)
    assert module.dispatch(args) == 2
    assert json.loads(capsys.readouterr().out)["status"] == "run_custody_missing"


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
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(ROOT)
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(ROOT)
    env["PATH"] = f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}"
    return subprocess.run(
        [str(SCRIPT), "--run-dir", str(run_dir), "--task-id", task_id, "--adapter", adapter,
         "--prompt-file", str(prompt), "--orchestrator-family", "openai",
         *([] if adapter == "opencode" else ["--alias", "workhorse"]),
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
    result = run_writer_dispatch(tmp_path, run_dir, prompt, "--access-mode", "worktree_write",
                                 "--worktree", str(worktree))
    receipt = json.loads(result.stdout.splitlines()[-1])
    assert result.returncode in {0, 1}, result.stderr + result.stdout
    assert receipt["status"] != "invalid_worktree"
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
         "--prompt-file", str(prompt), "--orchestrator-family", "openai", "--alias", "workhorse",
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
        "--alias", "workhorse", "--role", "worker",
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

    lease = module.acquire_worktree_lease(worktree)
    try:
        with pytest.raises(module.WorktreeLeaseError, match="another writer"):
            module.acquire_worktree_lease(worktree)
        result = run_writer_dispatch(
            tmp_path, run_dir, prompt, "--access-mode", "worktree_write", "--worktree", str(worktree),
            adapter=adapter,
        )
        assert result.returncode != 0
        assert json.loads(result.stdout)["status"] == "worktree_busy"
    finally:
        module.release_worktree_lease(lease)

    assert module.acquire_worktree_lease(worktree) is not None


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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
        "--timeout", "900",
    ])
    command = module.build_command(args, prompt, tmp_path / "result.md")
    assert "--timeout-seconds" in command
    passed = int(command[command.index("--timeout-seconds") + 1])
    assert passed == 895
    assert 0 < passed < args.timeout_seconds
    # A short deadline still leaves the provider a positive whole second.
    assert module.provider_timeout_seconds(2.0) == 1
    assert module.provider_timeout_seconds(0.25) == 1


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
         "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
        "--prompt-file", str(prompt), "--alias", "workhorse", "--role", "worker",
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
            {'id': 'missing', 'adapter': 'claude', 'alias': 'workhorse', 'prompt_file': 'absent.md'},
            {'id': 'broker', 'adapter': 'opencode', 'alias': 'workhorse', 'prompt': 'hello'},
        ]}), text=True, capture_output=True,
        env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': str(ROOT)},
    )
    record = json.loads(result.stdout)
    assert record['status'] == 'rejected'
    # OpenCode now resolves its catalogue default model, so only the missing prompt is rejected.
    assert {error['error'] for error in record['errors']} == {'prompt_unavailable'}
    assert all(error['fix'] for error in record['errors'])
    assert not (tmp_path / '.agent-run').exists()


def test_secret_in_inline_prompt_is_rejected_before_preflight_route(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    module = load_dispatch_module()
    result = module.preflight_tasks([{
        'id': 'task-1', 'adapter': 'claude', 'prompt': 'Bearer ' +
        'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.' + 'a' * 43,
    }])
    assert result['status'] == 'rejected'
    assert result['error'] == 'secret_detected'
    assert 'Bearer JWT' in result['fix']
    assert 'Bearer eyJ' not in json.dumps(result)


def test_preflight_secret_file_rejects_and_explicit_override_accepts(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))
    module = load_dispatch_module()
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('hello\n' + 'AKIA' + 'A' * 16)
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'workhorse', 'prompt_file': str(prompt)}
    rejected = module.preflight_tasks([task])
    assert rejected['error'] == 'secret_detected'
    assert f'{prompt}:2' in rejected['fix']
    assert module.preflight_tasks([{**task, 'allow_secrets': True}])['status'] == 'validated'


def test_preflight_secret_in_add_dirs_rejects_and_override_accepts(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))
    directory = tmp_path / 'shared'
    directory.mkdir()
    (directory / 'source.txt').write_text('rk_live_' + 'a' * 24)
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'workhorse',
            'prompt': 'review shared files', 'add_dirs': [str(directory)]}
    module = load_dispatch_module()
    rejected = module.preflight_tasks([task])
    assert rejected['error'] == 'secret_detected'
    assert 'Stripe live key' in rejected['fix']
    assert module.preflight_tasks([{**task, 'allow_secrets': True}])['status'] == 'validated'


def test_preflight_and_dispatch_refuse_scan_budget_overrun_unless_overridden(tmp_path, monkeypatch, capsys):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))
    scan = load_secret_scan_module()
    monkeypatch.setattr(scan, 'MAX_FILES', 0)
    directory = tmp_path / 'shared'
    directory.mkdir()
    (directory / 'source.txt').write_text('ordinary content')
    task = {'id': 'task-1', 'adapter': 'claude', 'alias': 'workhorse',
            'prompt': 'review shared files', 'add_dirs': [str(directory)]}
    module = load_dispatch_module()

    rejected = module.preflight_tasks([task])
    assert rejected['status'] == 'rejected'
    assert rejected['error'] == 'secret_scan_budget_exceeded'
    assert 'Narrow the prompt or additional directories' in rejected['fix']
    assert module.preflight_tasks([{**task, 'allow_secrets': True}])['status'] == 'validated'

    run_dir = make_run(tmp_path, 'scan-budget')
    adapter = tmp_path / 'adapter'
    write_success_adapter(adapter)
    module.CF_DISPATCH = adapter
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('review shared files')
    args = module.parser().parse_args(['--run-dir', str(run_dir), '--adapter', 'codex',
        '--prompt-file', str(prompt), '--alias', 'workhorse', '--role', 'worker',
        '--add-dir', str(directory)])
    assert module.dispatch(args) == 2
    refused = json.loads(capsys.readouterr().out.splitlines()[-1])
    assert refused['status'] == 'rejected'
    assert refused['error'] == 'secret_scan_budget_exceeded'
    assert 'allow_secrets: true and explain why in the prompt' in refused['fix']

    args.allow_secrets = True
    assert module.dispatch(args) == 0


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
        '--prompt-file', str(prompt), '--alias', 'workhorse', '--role', 'worker'])
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
        '--prompt-file', str(prompt), '--alias', 'workhorse', '--role', 'worker'])
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
        '--prompt-stdin', '--alias', 'workhorse', '--role', 'worker', '--access-mode', mode])
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
        env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': str(ROOT), 'PROBE_COUNTER': str(counter),
             'PATH': str(bin_dir) + os.pathsep + os.environ['PATH']})
    record = json.loads(result.stdout)
    assert record['status'] == 'validated', record
    assert len(record['routes']) == 3
    assert not counter.exists()


@pytest.mark.parametrize('owner', ['dispatch', 'batch'])
@pytest.mark.parametrize('instance', ['configured', 'missing', 'unset'])
def test_provider_does_not_inherit_chair_fabric_environment(tmp_path, owner, instance):
    policy = tmp_path / '.agents/fabric-policy.json'
    policy.parent.mkdir()
    policy.write_text('{"memory_floor_percent":{"read_only":0}}')
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
           'AGENT_FABRIC_INSTANCE_ROOT': str(ROOT), 'AGENT_FABRIC_PRODUCT_ROOT': str(ROOT),
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
    if owner == 'dispatch':
        command = [str(SCRIPT), '--run-dir', str(run_dir), '--adapter', 'codex',
                   '--prompt-file', str(prompt), '--alias', 'workhorse', '--role', 'worker']
    else:
        manifest = tmp_path / 'tasks.json'
        manifest.write_text(json.dumps({'schema_version': 1, 'tasks': [
            {'id': 'isolated', 'adapter': 'codex', 'prompt_file': str(prompt),
             'alias': 'workhorse', 'role': 'worker'}]}))
        command = [str(SCRIPT.with_name('batch_run.py')), '--run-dir', str(run_dir), '--manifest', str(manifest)]
    result = subprocess.run(command, cwd=tmp_path, env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stdout + result.stderr
    run_id = json.loads((run_dir / 'RUN_RECEIPT.json').read_text())['run_id']
    assert json.loads(capture.read_text()) == {"PROVENANT_RUN_ID": run_id}
    assert (tmp_path / 'provider-instance.txt').read_text() == expected_instance
    assert not (tmp_path / 'chair-state').exists()
    scratch = Path((tmp_path / 'provider-tmp.txt').read_text())
    assert scratch.is_dir()  # Full provider diagnostics now live in the attempt, not a removed tmp directory.
    assert json.loads((run_dir / 'RUN_RECEIPT.json').read_text())['status'] == 'ok'


def real_owner_fixture(tmp_path, monkeypatch, code):
    """Exercise cf_dispatch --plan-only and the production supervisor, no owner stub."""
    bindir = tmp_path / 'provider-bin'
    bindir.mkdir()
    write_executable(bindir / 'claude', '#!/usr/bin/env python3\n' + code)
    monkeypatch.setenv('PATH', str(bindir) + os.pathsep + os.environ['PATH'])
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))
    monkeypatch.setenv('FABRIC_COOLDOWNS_PATH', str(tmp_path / 'cooldowns.json'))
    monkeypatch.delenv('PROVENANT_RUN_TOKEN', raising=False)
    run = Path(subprocess.check_output([str(INIT), '--kind', 'dispatch'], cwd=tmp_path, text=True).strip())
    prompt = tmp_path / 'caller-prompt.md'
    prompt.write_text('hello')
    command = [sys.executable, str(SCRIPT), '--run-dir', str(run), '--adapter', 'claude',
               '--model', 'opus', '--prompt-file', str(prompt), '--fallback', 'false']
    return run, prompt, command


def test_normal_attempt_records_reaped_new_session_child(tmp_path, monkeypatch):
    pid_path = tmp_path / "leftover.pid"
    code = f'''import json, pathlib, subprocess, sys
sys.stdin.read()
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                         start_new_session=True, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))
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


def isolate_fabric_plan_env(monkeypatch):
    for key in tuple(os.environ):
        if key == 'PROVENANT_RUN_ID' or key.startswith('AGENT_FABRIC_'):
            monkeypatch.delenv(key, raising=False)
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))


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
        with pytest.raises(mod.WorktreeLeaseError):
            mod.acquire_worktree_lease(worktree)
        if stop == 'SIGTERM':
            process.send_signal(signal.SIGTERM)
        elif stop == 'marker':
            mod.create_cancellation_marker(run, run / 'dispatch/tasks/dispatch-001/attempt-001')
        stdout, stderr = process.communicate(timeout=10)
        row = json.loads((run / 'tasks/dispatch-001/attempt-001/attempt.json').read_text())
        assert row['status'] == ('timed_out' if stop == 'timeout' else 'cancelled'), stdout + stderr
        assert row['evidence']['exit'] is not None
        pids = json.loads((worktree / 'provider-pids').read_text())
        for pid in pids:
            with pytest.raises(ProcessLookupError):
                os.kill(pid, 0)
        lease = mod.acquire_worktree_lease(worktree)
        mod.release_worktree_lease(lease)
        receipt = json.loads((run / 'RUN_RECEIPT.json').read_text())
        assert receipt['status'] == ('failed' if stop == 'timeout' else 'cancelled')
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
    isolate_fabric_plan_env(monkeypatch)
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
        'if "opus" in sys.argv: print("rate limit", file=sys.stderr); sys.exit(1)\n'
        'print(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch)
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


def test_finalize_phase_includes_receipt_publication(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json\nsys.stdin.read()\nprint(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch)
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


@pytest.mark.parametrize('adapter, model, effort', [
    ('claude', 'opus', None), ('codex', 'gpt-6-luna', 'low'),
])
def test_fabric_fast_plan_matches_shell_for_explicit_model(tmp_path, monkeypatch, adapter, model, effort):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch,
        'import sys,json\nsys.stdin.read()\nprint(json.dumps({"type":"result","result":"DONE"}))\n',
    )
    isolate_fabric_plan_env(monkeypatch)
    monkeypatch.chdir(tmp_path)
    if adapter == 'codex':
        write_executable(tmp_path / 'provider-bin/codex', '#!/bin/sh\nexit 0\n')
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.tool = adapter
    args.model = model
    args.effort = effort
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    result = run / 'result.md'
    shell = subprocess.run(
        [*mod.build_command(args, prompt, result), '--plan-only'],
        cwd=tmp_path, env=mod.routing_environment(), capture_output=True, text=True, check=True,
    )
    planned = mod.fast_fabric_plan(args, prompt, result, tmp_path)
    assert planned is not None
    expected = json.loads(shell.stdout)
    for value in (planned, expected):
        session = value.pop('session_id', None)
        value['argv'] = [arg.replace(session, '<session>') if session else arg
                         for arg in value['argv']]
    assert planned == expected


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
    isolate_fabric_plan_env(monkeypatch)
    monkeypatch.chdir(tmp_path)
    if adapter == 'codex':
        write_executable(tmp_path / 'provider-bin/codex', '#!/bin/sh\nexit 0\n')
    prompt.write_bytes(content.encode())
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.tool, args.model, args.effort = adapter, model, effort
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    result = run / 'result.md'
    shell = subprocess.run([*mod.build_command(args, prompt, result), '--plan-only'],
                           cwd=tmp_path, env=mod.routing_environment(), capture_output=True, text=True)
    planned = mod.fast_fabric_plan(args, prompt, result, tmp_path)
    if planned is None:
        return  # The shell planner owns routes outside the narrow fast path.
    assert shell.returncode == 0, shell.stderr
    expected = json.loads(shell.stdout)
    for value in (planned, expected):
        session = value.pop('session_id', None)
        value['argv'] = [arg.replace(session, '<session>') if session else arg for arg in value['argv']]
    assert planned == expected


@pytest.mark.parametrize('name', [
    'CF_DISPATCH_IDLE_SECONDS', 'CF_DISPATCH_AGY_ADD_DIR', 'CF_DISPATCH_ENABLE_KIRO',
    'CF_DISPATCH_ENABLE_COPILOT', 'CF_DISPATCH_CURSOR_MODEL', 'CF_DISPATCH_KIRO_MODEL',
    'CF_DISPATCH_COPILOT_MODEL', 'CF_DISPATCH_OPENCODE_MODEL', 'CF_DISPATCH_ENDPOINT',
    'CF_DISPATCH_CODEX_NETWORK', 'CF_DISPATCH_AGY_SANDBOX',
])
def test_fabric_fast_plan_delegates_shell_environment_knobs(tmp_path, monkeypatch, name):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv(name, '1')
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    assert mod.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


@pytest.mark.parametrize('variant', ['agy_add_dir', 'resume', 'fallback'])
def test_fabric_fast_plan_delegates_valid_shell_only_options(tmp_path, monkeypatch, variant):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch)
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
                           cwd=tmp_path, env=mod.routing_environment(), capture_output=True, text=True)
    assert shell.returncode == 0, shell.stderr
    expected = json.loads(shell.stdout)
    assert expected['adapter'] == 'claude'
    assert mod.fast_fabric_plan(args, prompt, result, tmp_path) is None
    if variant == 'agy_add_dir':
        assert str(extra) in expected['applied']['add_dirs']
    elif variant == 'resume':
        assert 'existing-session' in expected['argv']


def test_fabric_fast_plan_delegates_missing_tmpdir(tmp_path, monkeypatch):
    """cf_dispatch.sh rejects a TMPDIR that is not a directory; the fast path must not accept it."""
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    monkeypatch.setenv('TMPDIR', str(tmp_path / 'missing'))
    assert mod.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


def test_fabric_fast_plan_guard_covers_shell_environment_reads():
    import re
    mod = load_dispatch_module()
    shell = (ROOT / 'skills/orchestrate/scripts/cf_dispatch.sh').read_text()
    read_names = set(re.findall(r'\$\{?(CF_DISPATCH_[A-Z0-9_]+)', shell))
    assert read_names == mod.FAST_PLAN_SHELL_ENV_READS


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
        mod.parse_fast_route_json(raw)


def test_fabric_fast_route_validation_tracks_shell_fields():
    import re
    mod = load_dispatch_module()
    shell = (ROOT / 'skills/orchestrate/scripts/cf_dispatch.sh').read_text()
    parser = shell.split('parse_route_json() {', 1)[1].split("PY\n", 1)[0]
    required = parser.split('for key in (', 1)[1].split(')', 1)[0]
    fields = parser.split('keys = (', 1)[1].split(')', 1)[0]
    assert set(re.findall(r'"([a-z_]+)"', required)) == set(mod.FAST_PLAN_ROUTE_REQUIRED)
    assert set(re.findall(r'"([a-z_]+)"', fields)) == set(mod.FAST_PLAN_ROUTE_FIELDS)


@pytest.mark.parametrize('failure', [
    ImportError('module unavailable'), SyntaxError('bad module'),
    subprocess.CalledProcessError(1, ['router']), subprocess.TimeoutExpired(['router'], 1),
    SystemExit(2),
])
def test_fabric_fast_plan_delegates_import_and_router_failures(tmp_path, monkeypatch, failure):
    run, prompt, command = real_owner_fixture(tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n')
    isolate_fabric_plan_env(monkeypatch)
    monkeypatch.chdir(tmp_path)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS

    def fail_import(*_args, **_kwargs):
        raise failure

    monkeypatch.setattr(mod.importlib.util, 'spec_from_file_location', fail_import)
    assert mod.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


@pytest.mark.parametrize('adapter, idle', [('agy', None), ('claude', '0.5')])
def test_fabric_fast_plan_delegates_routes_needing_shell_checks(tmp_path, monkeypatch, adapter, idle):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n',
    )
    monkeypatch.chdir(tmp_path)
    if idle is not None:
        monkeypatch.setenv('CF_DISPATCH_IDLE_SECONDS', idle)
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.tool = adapter
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    assert mod.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


def test_fabric_fast_plan_rejects_invalid_git_context(tmp_path, monkeypatch):
    run, prompt, command = real_owner_fixture(
        tmp_path, monkeypatch, 'import sys\nsys.stdin.read()\n',
    )
    monkeypatch.chdir(tmp_path)
    (tmp_path / '.git').symlink_to(tmp_path / 'missing-git')
    mod = load_dispatch_module()
    args = mod.parser().parse_args(command[2:])
    args.timeout_seconds = mod.DEFAULT_TIMEOUT_SECONDS
    assert mod.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


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
    assert mod.fast_fabric_plan(args, prompt, run / 'result.md', tmp_path) is None


@pytest.mark.parametrize('previous_status', ['interrupted', 'timed_out', 'cancelled', 'stalled'])
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
    if previous_status == 'interrupted':
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
    resumed = subprocess.run([sys.executable, str(SCRIPT), '--run-dir', str(run), '--resume', previous['run_id'],
                              '--prompt-file', str(prompt)], cwd=tmp_path, capture_output=True, text=True)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    row = json.loads((run / 'tasks/dispatch-001/attempt-002/attempt.json').read_text())
    assert row['status'] == 'ok'
    assert 'resumed_by_relaunch' in row['provenance']['notes']
    assert 'resume: relaunched' in row['warnings']


def test_incomplete_writer_without_claude_session_returns_typed_fix(tmp_path):
    mod = load_dispatch_module()
    run = Path(subprocess.check_output([str(INIT), '--kind', 'dispatch'], cwd=tmp_path, text=True).strip())
    row = json.loads((ROOT / 'tests/fixtures/fabric-v1/attempt.json').read_text())
    row.update(run_id=run.name, status='timed_out', mode='worktree_write', cwd=str(tmp_path),
               worktree=str(tmp_path), session_id='generated-but-unobserved')
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


def test_planner_stopped_by_signal_is_interrupted_not_rejected():
    mod = load_dispatch_module()
    stopped = subprocess.CompletedProcess([], -15, stdout="", stderr="Terminated: 15")
    assert mod.planner_result(stopped)["status"] == "interrupted"
    broken = subprocess.CompletedProcess([], 0, stdout="not json", stderr="")
    assert mod.planner_result(broken) == {"status": "rejected", "fix": "route planner returned invalid JSON"}


def test_planner_capability_statuses_become_typed_failures_with_fixes():
    mod = load_dispatch_module()
    row = mod.router_failure("capability_model_unavailable", "codex")
    assert row["status"] == "model_unavailable"
    assert row["fix"].startswith("choose a registered model:") and "gpt-6-luna" in row["fix"]
    assert row["evidence"]["signature"] == "capability_model_unavailable"
    assert mod.router_failure("capability_discovery_failed", "codex")["status"] == "failed"


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
    row = mod.router_failure(status, "codex")
    assert row["status"] == expected
    assert row["fix"] and row["evidence"]["signature"] == status


def test_planner_crash_signal_is_not_an_interruption():
    mod = load_dispatch_module()
    crashed = mod.planner_result(subprocess.CompletedProcess([], -11, stdout="", stderr=""))
    assert crashed["status"] == "failed" and "signal 11" in crashed["fix"]


def test_provider_exec_failures_are_not_router_refusals():
    mod = load_dispatch_module()
    assert mod.route_refusal({"status": "output_write_error", "provenance": {"line": "Route: x"}}, "codex") is None
    assert mod.route_refusal({"status": "ok"}, "codex") is None
    assert mod.route_refusal({"schema": "fabric.exec-plan.v1", "status": "odd"}, "codex") is None
    assert mod.route_refusal({"status": "unknown_alias"}, "codex")["status"] == "rejected"
