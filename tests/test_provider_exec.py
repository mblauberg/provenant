"""Provider supervisor gates use fixture programs only, never model calls."""

import importlib
import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "skills/orchestrate/scripts"


def test_plan_only_resolves_without_launch(tmp_path):
    provider = tmp_path / "claude"
    provider.write_text('#!/bin/sh\ntouch "' + str(tmp_path / "launched") + '"\n')
    provider.chmod(0o755)
    env = {
        **os.environ,
        "PATH": str(tmp_path) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
    }
    result = subprocess.run(
        [
            str(SCRIPTS / "cf_dispatch.sh"),
            "--intent",
            "ordinary",
            "--tool",
            "claude",
            "--alias",
            "workhorse",
            "--prompt",
            "Hello",
            "--plan-only",
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    plan = json.loads(result.stdout)
    assert plan["schema"] == "fabric.exec-plan.v1"
    assert "--bare" not in plan["argv"]
    assert "--no-session-persistence" not in plan["argv"]
    assert "--session-id" in plan["argv"]
    assert "--permission-prompts" in plan["argv"]
    assert "stream-json" in plan["argv"]
    assert not (tmp_path / "launched").exists()


def supervisor():
    return importlib.import_module("skills.orchestrate.scripts.provider_exec")


@pytest.mark.parametrize(
    "adapter", ["claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"]
)
@pytest.mark.parametrize(
    "text,status",
    [
        ("You've hit your usage limit", "usage_limited"),
        ("Individual quota reached", "usage_limited"),
        ("rate limit exceeded; HTTP 429", "rate_limited"),
        ("Login expired; not logged in", "auth_required"),
        ("model not found", "model_unavailable"),
        ("permission denied", "permission_blocked"),
    ],
)
def test_failure_signatures(adapter, text, status):
    parsed = supervisor().parse_output(adapter, "", text, 1)
    assert parsed["status"] == status
    assert parsed["signature"]
    assert len(parsed["excerpt"]) <= 200


@pytest.mark.parametrize(
    "fixture", sorted((ROOT / "tests/fixtures/fabric-v1").glob("provider-*.json"))
)
def test_provider_golden(fixture):
    data = json.loads(fixture.read_text())
    for case in [data, *data.get("cases", [])]:
        result = supervisor().parse_output(
            case["adapter"], case["stdout"], case.get("stderr", ""), case["exit"]
        )
        for key, value in case["expected"].items():
            assert result[key] == value


def test_structured_result_and_question_take_precedence_over_prose():
    events = "\n".join(
        map(
            json.dumps,
            [
                {
                    "type": "system",
                    "subtype": "init",
                    "session_id": "s1",
                    "model": "opus",
                },
                {
                    "type": "result",
                    "is_error": False,
                    "result": "```\nQUESTION: Target main or release/3?\n```",
                },
            ],
        )
    )
    parsed = supervisor().parse_output("claude", events)
    assert parsed["status"] == "input_required"
    assert parsed["question"] == "Target main or release/3?"
    assert parsed["session_id"] == "s1"
    assert parsed["observed_model"] == "opus"


def test_reconnecting_is_nonfatal_and_quota_discussion_is_not_an_error():
    events = "\n".join(
        map(
            json.dumps,
            [
                {"type": "error", "message": "Reconnecting… 1/5"},
                {
                    "type": "item.completed",
                    "item": {
                        "type": "agent_message",
                        "text": "Explain quota exceeded errors",
                    },
                },
                {"type": "turn.completed"},
            ],
        )
    )
    assert supervisor().parse_output("codex", events)["status"] == "ok"


def test_reset_evidence_uses_provider_time_units():
    from datetime import UTC, datetime

    at = datetime(2026, 9, 23, 1, tzinfo=UTC)
    assert (
        supervisor().parse_output(
            "claude",
            json.dumps(
                {
                    "type": "rate_limit_event",
                    "rate_limit_info": {"status": "rejected", "resetsAt": 1790157600},
                }
            ),
            "",
            1,
            at=at,
        )["reset_at"]
        == "2026-09-23T10:00:00Z"
    )
    assert (
        supervisor().parse_output(
            "agy", "", "RESOURCE_EXHAUSTED Resets in 167h", 3, at=at
        )["reset_at"]
        == "2026-09-30T00:00:00Z"
    )
    assert (
        supervisor().parse_output(
            "codex", "", "Usage limit; try again at 2:00 AM", 1, at=at
        )["reset_at"]
        == "2026-09-23T02:00:00Z"
    )


def fixture_plan(tmp_path, code, adapter="codex", **controls):
    mod = supervisor()
    plan = mod.build_plan(
        adapter,
        {
            "resolved_model": "fixture",
            "model_family": "openai",
            "endpoint_provider": "openai",
            "effort": "high",
        },
        "hello",
        cwd=tmp_path,
        **controls,
    )
    plan["argv"] = [sys.executable, "-u", "-c", code]
    plan["grace_seconds"] = 0.1
    return plan


@pytest.mark.parametrize(
    "adapter", ["claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"]
)
def test_all_adapters_stall_with_durable_diagnostics(tmp_path, adapter):
    plan = fixture_plan(
        tmp_path,
        "import time; time.sleep(30)",
        adapter,
        idle_seconds=0.2,
        timeout_seconds=3,
    )
    record = supervisor().execute(
        plan,
        tmp_path / "result.md",
        env={**os.environ, "CF_DISPATCH_ENABLE_COPILOT": "1"},
    )
    assert record["status"] == "stalled"
    assert record["exit"] is not None
    assert "idle_warning" in (tmp_path / "events.jsonl").read_text()
    assert record["evidence"]["signature"] == "idle_watchdog"


def test_cursor_terminal_event_closes_open_stdin_and_reaps_descendants(tmp_path):
    code = """import json,os,stat,subprocess,sys,time
assert stat.S_ISFIFO(os.fstat(0).st_mode)
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
print(json.dumps({'type':'system','subtype':'init','model':'grok','session_id':'cursor-1'}))
print(json.dumps({'type':'result','result':'DONE','is_error':False}))
time.sleep(30)
"""
    plan = fixture_plan(tmp_path, code, "cursor", timeout_seconds=2, idle_seconds=1)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"
    assert record["session_id"] == "cursor-1"
    assert record["provenance"]["observed_model"] == "grok"
    assert record["provenance"]["identity"] == "observed"


def test_wall_timeout_is_distinct_from_idle(tmp_path):
    plan = fixture_plan(
        tmp_path, "import time; time.sleep(30)", idle_seconds=3, timeout_seconds=0.2
    )
    assert supervisor().execute(plan, tmp_path / "result.md")["status"] == "timed_out"


def test_observed_substitution_cannot_certify_the_resolved_family(tmp_path):
    plan = fixture_plan(
        tmp_path,
        "import json; print(json.dumps({'type':'system','subtype':'init','model':'other-vendor'})); "
        "print(json.dumps({'type':'result','result':'DONE','is_error':False}))",
        "claude",
        intent="assurance",
        orchestrator_family="anthropic",
    )
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["provenance"]["family"] == "unknown"
    assert not record["cross_family"]
    assert not record["certification_eligible"]


def test_preface_env_and_credential_path_controls(tmp_path):
    plan = fixture_plan(
        tmp_path,
        "import os,json; print(json.dumps({k:os.environ.get(k) for k in ['PROVENANT_ROUTE','PROVENANT_RUN_ID','PROVENANT_CHAIR','AGENT_FABRIC_SEAT']}))",
        run_id="mcp-123",
        chair="chair-seat",
    )
    # Plain text is a supported compatibility response.
    plan["argv"][-1] = (
        "import os; print('|'.join(os.environ.get(k,'') for k in ['PROVENANT_ROUTE','PROVENANT_RUN_ID','PROVENANT_CHAIR','AGENT_FABRIC_SEAT']))"
    )
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert (
        tmp_path / "result.md"
    ).read_text().strip() == "codex/fixture@high|mcp-123|chair-seat|"
    assert plan["prompt"].startswith("You are codex/fixture@high via Fabric.")
    secrets = tmp_path / ".ssh"
    secrets.mkdir()
    assert str(secrets) not in fixture_plan(tmp_path, "", add_dirs=[secrets])['applied']['add_dirs']
    with pytest.raises(ValueError, match="read-only"):
        fixture_plan(tmp_path, "", sandbox="workspace-write")


@pytest.mark.parametrize("adapter", ["claude", "codex"])
def test_owner_contract_and_resume_keeps_same_run(tmp_path, adapter):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    executable = bindir / adapter
    executable.write_text("""#!/usr/bin/env python3
import json,sys
if sys.argv[1:3]==['debug','models']:
 print(json.dumps({'models':[{'slug':'gpt-6-luna','supported_reasoning_levels':[{'effort':'high'}]}]}));sys.exit()
prompt=sys.stdin.read()
if 'claude' in sys.argv[0]:
 print(json.dumps({'type':'system','subtype':'init','model':'claude-sonnet-4-6','session_id':'fixture-session'}))
 print(json.dumps({'type':'result','result':'DONE' if '--resume' in sys.argv else '```\\nQUESTION: Which branch?\\n```','is_error':False}))
else:
 print(json.dumps({'type':'thread.started','thread_id':'fixture-session'}))
 print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'DONE' if 'resume' in sys.argv else '```\\nQUESTION: Which branch?\\n```'}}))
 print(json.dumps({'type':'turn.completed'}))
""")
    executable.chmod(0o755)
    env = {
        **os.environ,
        "PATH": str(bindir) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
        "FABRIC_COOLDOWNS_PATH": str(tmp_path / "cooldowns.json"),
    }
    run = Path(
        subprocess.check_output(
            [
                str(SCRIPTS / "run_dir_init.sh"),
                "--kind",
                "dispatch",
                "--slug",
                "resume",
            ],
            cwd=tmp_path,
            text=True,
        ).strip()
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Choose a branch")
    first = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--task-id",
            "task-1",
            "--tool",
            adapter,
            "--alias",
            "workhorse",
            "--role",
            "worker",
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    path = run / "tasks/task-1/attempt-001/attempt.json"
    assert path.exists(), first.stdout + first.stderr
    row = json.loads(path.read_text())
    assert row["schema"] == "fabric.attempt.v1"
    assert row["status"] == "input_required"
    assert row["session_id"] == "fixture-session"
    prompt.write_text("main")
    second = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--resume",
            row["run_id"],
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert second.returncode == 0, second.stdout + second.stderr
    row2 = json.loads((run / "tasks/task-1/attempt-002/attempt.json").read_text())
    assert row2["status"] == "ok"
    assert row2["run_id"] == row["run_id"]
    assert row2["session_id"] == "fixture-session"
    third = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--resume",
            row["run_id"],
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert third.returncode == 0, third.stdout + third.stderr
    row3 = json.loads((run / "tasks/task-1/attempt-003/attempt.json").read_text())
    assert row3["status"] == "ok" and row3["session_id"] == row2["session_id"]
    assert len((tmp_path / ".agent-run/runs/index.jsonl").read_text().splitlines()) == 3


def test_fallback_training_requires_explicit_opt_in(tmp_path):
    mod = importlib.import_module("skills.orchestrate.scripts.exec_routing")
    plan = fixture_plan(tmp_path, "")
    plan["requested_model"] = None
    plan["route"].update(
        alias="workhorse",
        candidates=["fixture", "paid", "training", "opencode/free-free"],
    )
    catalogue = {"models": {"training": {"trains_on_prompts": True}}, "adapters": {}}
    assert [item["model"] for item in mod.candidates(plan, True, catalogue)] == ["paid"]
    assert [item["model"] for item in mod.candidates(plan, "any", catalogue)] == [
        "paid",
        "training",
        "free-free",
    ]
    assert mod.candidates(plan, False, catalogue) == []


@pytest.mark.parametrize("explicit", [False, True])
def test_usage_limit_falls_back_as_attempt_two(tmp_path, explicit):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    cli = bindir / "claude"
    cli.write_text("""#!/usr/bin/env python3
import json,sys
sys.stdin.read()
model=sys.argv[sys.argv.index('--model')+1]
print(json.dumps({'type':'result','is_error':model=='opus','result':"You've hit your usage limit" if model=='opus' else 'DONE'}))
sys.exit(1 if model=='opus' else 0)
""")
    cli.chmod(0o755)
    fallback_cli = bindir / 'opencode'
    fallback_cli.write_text("#!/usr/bin/env python3\nimport json\nprint(json.dumps({'type':'text','part':{'text':'DONE'}}))\n")
    fallback_cli.chmod(0o755)
    env = {
        **os.environ,
        "PATH": str(bindir) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
        "FABRIC_COOLDOWNS_PATH": str(tmp_path / "cooldowns.json"),
    }
    run = Path(
        subprocess.check_output(
            [str(SCRIPTS / "run_dir_init.sh"), "--kind", "dispatch"],
            cwd=tmp_path,
            text=True,
        ).strip()
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply DONE")
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--tool",
            "claude",
            "--model" if explicit else "--alias",
            "opus" if explicit else "workhorse",
            "--fallback", "true",
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    rows = [
        json.loads(path.read_text())
        for path in sorted((run / "tasks").glob("*/attempt-*/attempt.json"))
    ]
    assert [row["status"] for row in rows] == ["usage_limited", "ok"]
    assert len({row["run_id"] for row in rows}) == 1
    assert rows[1]["provenance"]["fallback_from"]["status"] == "usage_limited"
    assert json.loads((run / "RUN_RECEIPT.json").read_text())["status"] == "succeeded"
    assert (
        json.loads((tmp_path / "cooldowns.json").read_text())["cooldowns"][
            "claude/*"
        ]["source_run"]
        == rows[0]["run_id"]
    )


def test_codex_observed_model_comes_from_rollout_turn_context(tmp_path):
    sessions = tmp_path / "codex/sessions/2026/09/23"
    sessions.mkdir(parents=True)
    (sessions / "rollout-time-thread-123.jsonl").write_text(
        json.dumps({"type": "turn_context", "payload": {"model": "gpt-6-luna"}}) + "\n"
    )
    code = "import json; print(json.dumps({'type':'thread.started','thread_id':'thread-123'})); print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'OK'}})); print(json.dumps({'type':'turn.completed'}))"
    plan = fixture_plan(tmp_path, code)
    record = supervisor().execute(
        plan,
        tmp_path / "result.md",
        env={**os.environ, "CODEX_HOME": str(tmp_path / "codex")},
    )
    assert record["provenance"]["observed_model"] == "gpt-6-luna"
    assert record["provenance"]["observed_source"] == "codex:rollout.turn_context.model"
    assert record["provenance"]["identity"] == "observed"
    assert "mismatch" in record["warnings"][0]


def test_opencode_observed_model_comes_from_export(tmp_path):
    cli = tmp_path / "opencode"
    cli.write_text(
        '#!/bin/sh\nif [ "$1" = export ]; then echo \'{"messages":[{"info":{"providerID":"opencode-go","modelID":"deepseek-v4.1-flash"}}]}\'; fi\n'
    )
    cli.chmod(0o755)
    plan = fixture_plan(
        tmp_path,
        "import json; print(json.dumps({'type':'text','sessionID':'oc-1','part':{'text':'OK'}}))",
        "opencode",
    )
    record = supervisor().execute(
        plan,
        tmp_path / "result.md",
        env={**os.environ, "PATH": str(tmp_path) + ":" + os.environ["PATH"]},
    )
    assert record["provenance"]["observed_model"] == "opencode-go/deepseek-v4.1-flash"
    assert record["provenance"]["observed_source"] == "opencode:export.modelID"


def test_capture_is_bounded_but_result_is_complete(tmp_path, monkeypatch):
    module = supervisor()
    path = tmp_path / "events.jsonl"
    capture = module.BoundedCapture(path, limit=100)
    capture.write(b"A" * 80)
    capture.write(b"B" * 80)
    capture.close()
    assert path.read_bytes() == b"A" * 50 + b"B" * 50


def test_linked_writer_automatically_grants_git_metadata(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", str(repo)], check=True, capture_output=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
        check=True,
        capture_output=True,
    )
    lane = tmp_path / "lane"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "-b", "lane", str(lane)],
        check=True,
        capture_output=True,
    )
    plan = supervisor().build_plan(
        "codex",
        {"resolved_model": "fixture"},
        "hello",
        mode="worktree_write",
        worktree=lane,
        network=False,
    )
    assert str(repo / ".git") in plan["applied"]["add_dirs"]
    assert plan["applied"]["network"] is False
    assert "sandbox_workspace_write.network_access=false" in plan["argv"]
    assert "--ephemeral" not in plan["argv"]


def test_plan_only_honors_read_only_cwd_inside_workspace(tmp_path):
    child = tmp_path / "nested"
    child.mkdir()
    cli = tmp_path / "claude"
    cli.write_text("#!/bin/sh\nexit 99\n")
    cli.chmod(0o755)
    env = {
        **os.environ,
        "PATH": str(tmp_path) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
    }
    result = subprocess.run(
        [
            str(SCRIPTS / "cf_dispatch.sh"),
            "--tool",
            "claude",
            "--intent",
            "ordinary",
            "--prompt",
            "hello",
            "--cwd",
            str(child),
            "--plan-only",
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    assert json.loads(result.stdout)["cwd"] == str(child)


def test_unsupported_controls_are_not_claimed_as_applied(tmp_path):
    plan = supervisor().build_plan(
        "cursor",
        {"resolved_model": "fixture", "effort": "high"},
        "hello",
        cwd=tmp_path,
        requested_effort="high",
        network=False,
    )
    assert plan["effort"] == ""
    assert plan["applied"]["network"] is None
    assert plan["warnings"]
    plan = supervisor().build_plan(
        "codex",
        {"resolved_model": "fixture"},
        "hello",
        cwd=tmp_path,
        mode="worktree_write",
        sandbox="full",
        network=False,
    )
    assert plan["applied"]["network"] is None
    assert plan["applied"]["guarantee"] != "enforced"


def test_kiro_acp_stream_captures_text_session_and_model():
    events = [
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "kiro-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "DONE"},
                },
            },
        },
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "kiro-1",
                "update": {
                    "sessionUpdate": "config_options_update",
                    "configOptions": [
                        {"category": "model", "currentValue": "kiro-auto"}
                    ],
                },
            },
        },
        {"jsonrpc": "2.0", "id": 1, "result": {"stopReason": "end_turn"}},
    ]
    parsed = supervisor().parse_output("kiro", "\n".join(map(json.dumps, events)))
    assert parsed["text"] == "DONE"
    assert parsed["session_id"] == "kiro-1"
    assert parsed["observed_model"] == "kiro-auto"
    assert parsed["terminal"] is True


def test_kiro_enforcement_requires_fresh_version_bound_negative_probe(tmp_path):
    from datetime import UTC, datetime, timedelta

    route = {
        "resolved_model": "auto",
        "cli_version": "2.23.0",
        "read_only_probe": {
            "cli_version": "2.23.0",
            "checked_at": datetime.now(UTC).isoformat(),
            "attempted_write": True,
            "permission_denied": True,
            "file_created": False,
        },
    }
    assert (
        supervisor().build_plan("kiro", route, "hello", cwd=tmp_path)["applied"][
            "guarantee"
        ]
        == "enforced"
    )
    route["read_only_probe"]["cli_version"] = "old"
    assert (
        supervisor().build_plan("kiro", route, "hello", cwd=tmp_path)["applied"][
            "guarantee"
        ]
        == "prompt_only"
    )
    route["read_only_probe"].update(
        cli_version="2.23.0",
        checked_at=(datetime.now(UTC) - timedelta(days=2)).isoformat(),
    )
    assert (
        supervisor().build_plan("kiro", route, "hello", cwd=tmp_path)["applied"][
            "guarantee"
        ]
        == "prompt_only"
    )


def test_writer_watchdog_does_not_count_its_own_warning_as_progress(tmp_path):
    plan = fixture_plan(
        tmp_path,
        "import time; time.sleep(30)",
        mode="worktree_write",
        idle_seconds=1.2,
        timeout_seconds=3.5,
    )
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "stalled"

@pytest.mark.parametrize('api_key', [False, True])
def test_claude_plan_isolates_settings_and_mcp(tmp_path, monkeypatch, api_key):
    monkeypatch.delenv('ANTHROPIC_BASE_URL', raising=False)
    monkeypatch.delenv('ANTHROPIC_AUTH_TOKEN', raising=False)
    monkeypatch.delenv('ANTHROPIC_API_KEY', raising=False)
    if api_key:
        monkeypatch.setenv('ANTHROPIC_API_KEY', 'fixture-key')
    plan = supervisor().build_plan('claude', {'resolved_model': 'opus'}, 'hello', cwd=tmp_path)
    assert ('--bare' if api_key else '--safe-mode') in plan['argv']
    assert '--strict-mcp-config' in plan['argv']
    assert 'fixture-key' not in ' '.join(plan['argv'])

@pytest.mark.parametrize('adapter', ['claude', 'codex', 'opencode'])
def test_successful_answer_survives_tool_permission_diagnostic(adapter):
    parsed = supervisor().parse_output(adapter, '{"type":"result","result":"DONE"}', 'ls: /root: Permission denied', 0)
    assert parsed['status'] == 'ok'


def test_question_example_in_middle_of_answer_does_not_suspend():
    parsed = supervisor().parse_output('claude', json.dumps({'type':'result','result':'Example:\n```\nQUESTION: Which?\n```\nEnd of explanation.'}))
    assert parsed['status'] == 'ok'


@pytest.mark.parametrize('directory', ['.config', 'Library', '.local/share', '.local'])
def test_add_dirs_deny_ancestors_of_credential_stores(tmp_path, monkeypatch, directory):
    monkeypatch.setenv('HOME', str(tmp_path))
    target = tmp_path / directory
    target.mkdir(parents=True)
    plan = supervisor().build_plan('codex', {}, 'hello', cwd=tmp_path, add_dirs=[target])
    assert str(target) not in plan['applied']['add_dirs']
    assert any('credential' in warning for warning in plan['warnings'])


def test_opencode_does_not_claim_unsupported_add_dirs(tmp_path):
    plan = supervisor().build_plan('opencode', {}, 'hello', cwd=tmp_path, add_dirs=[tmp_path])
    assert plan['applied']['add_dirs'] == []
    assert 'additional directories unsupported by opencode' in plan['warnings']


@pytest.mark.parametrize('directory', [
    '.gemini', '.cursor', '.kiro', '.docker', '.kube', '.config/opencode',
    '.config/gh', '.config/gcloud', '.aws', '.ssh', '.gnupg', '.netrc',
    '.npmrc', '.local/share/opencode/auth.json',
])
def test_add_dirs_warns_and_drops_credential_stores(tmp_path, monkeypatch, directory):
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)
    target = tmp_path / directory
    target.mkdir(parents=True)
    safe = tmp_path / 'source'
    safe.mkdir()
    plan = supervisor().build_plan('codex', {}, 'hello', cwd=tmp_path, add_dirs=[target, safe])
    assert str(target) not in plan['applied']['add_dirs']
    assert str(safe) in plan['applied']['add_dirs']
    assert any('credential' in warning for warning in plan['warnings'])


def test_failure_classification_uses_diagnostics_not_answer_prose():
    parsed = supervisor().parse_output('opencode', 'Explain the rate limit algorithm', '', 1)
    assert parsed['status'] == 'failed'
    assert supervisor().parse_output('claude', '', '529 overloaded', 1)['status'] == 'rate_limited'


def test_kiro_tool_echo_does_not_override_observed_model():
    output = '\n'.join(map(json.dumps, [
        {'type':'system', 'model':'actual'},
        {'type':'tool_result','data':{'model':'echoed'}},
        {'type':'result','result':'DONE'},
    ]))
    assert supervisor().parse_output('kiro', output)['observed_model'] == 'actual'


def test_capture_rollover_writes_linear_bytes_and_preserves_terminal_result(tmp_path):
    mod = supervisor()
    capture = mod.BoundedCapture(tmp_path / 'capture', limit=1000)
    class Meter:
        def __init__(self, file):
            self.file, self.written = file, 0
        def write(self, data):
            self.written += len(data)
            return self.file.write(data)
        def __getattr__(self, name):
            return getattr(self.file, name)
    meter = Meter(capture.file)
    capture.file = meter
    for _ in range(1000):
        capture.write(b'x' * 100)
    capture.close()
    assert meter.written <= 102000
    assert (tmp_path / 'capture').stat().st_size == 1000


def test_stream_capture_retains_early_semantic_events_with_bounded_storage(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
    code = '''import json
print(json.dumps({'type':'system','model':'observed','session_id':'retained'}))
print(json.dumps({'type':'result','result':'DONE'}))
for i in range(10000): print(json.dumps({'type':'telemetry','data':'x'*100}))
'''
    plan = fixture_plan(tmp_path, code, 'claude')
    record = mod.execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'ok'
    assert record['session_id'] == 'retained'
    assert (tmp_path / 'result.md').read_text() == 'DONE'
    assert (tmp_path / 'events.jsonl').stat().st_size <= 2048


def test_plan_uses_router_applied_effort(tmp_path):
    plan = supervisor().build_plan('opencode', {'resolved_model':'fixture', 'effort':'high', 'effort_applied':'medium'}, 'hello', cwd=tmp_path)
    assert plan['effort'] == 'medium'
    assert plan['argv'][plan['argv'].index('--variant') + 1] == 'medium'


def test_fallback_uses_router_candidates_and_parses_explicit_routes():
    mod = importlib.import_module('skills.orchestrate.scripts.exec_routing')
    plan = {'adapter':'claude','model':'opus','effort':'high','route':{'fallback_candidates':[{'adapter':'codex','model':'gpt-6-sol','effort_applied':'medium'}]}}
    assert mod.candidates(plan, True, {}) == [{'adapter':'codex','model':'gpt-6-sol','effort':'medium'}]
    assert mod.candidates(plan, ['codex/gpt-6-sol@low'], {}) == [{'adapter':'codex','model':'gpt-6-sol','effort':'low'}]


@pytest.mark.parametrize('policy', ['yes', 'codex/gpt-6-sol', 3, {}, [''], [3], [{'adapter': [], 'model': 'sol'}]])
def test_invalid_fallback_rejected_during_preflight(tmp_path, monkeypatch, policy):
    mod = importlib.import_module('skills.orchestrate.scripts.dispatch_run')
    monkeypatch.chdir(tmp_path)
    result = mod.preflight_tasks([{'id':'one','adapter':'claude','model':'opus','prompt':'hello','fallback':policy}])
    assert result['status'] == 'rejected'
    assert result['error'] == 'fallback_invalid'


def test_cancel_allows_provider_session_flush(tmp_path):
    code = '''import signal,time
from pathlib import Path
def stop(*args):
    time.sleep(.3)
    Path('flushed').write_text('saved')
    raise SystemExit(0)
signal.signal(signal.SIGTERM, stop)
Path('ready').touch()
time.sleep(30)
'''
    plan = fixture_plan(tmp_path, code, timeout_seconds=5)
    record = supervisor().execute(plan, tmp_path / 'result.md', cancelled=lambda: (tmp_path / 'ready').exists())
    assert record['status'] == 'cancelled'
    assert (tmp_path / 'flushed').read_text() == 'saved'


def test_writer_progress_includes_files_after_ten_thousand(tmp_path):
    mod = supervisor()
    directory = tmp_path / 'source'
    directory.mkdir()
    for number in range(10001):
        (directory / str(number)).touch()
    before = mod._workspace_stamp(tmp_path)
    # Modify the last entry in the same traversal order used by the watchdog.
    last = list(os.walk(directory))[0][2][-1]
    (directory / last).write_text('changed')
    assert mod._workspace_stamp(tmp_path) != before


def test_stream_rollover_preserves_unclassified_structured_failure(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
    code = '''import json
print(json.dumps({'type':'result','is_error':True,'result':'backend exploded'}))
for i in range(1000): print(json.dumps({'type':'telemetry','data':'x'*100}))
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'failed'


def test_explicit_route_fallback_policy_reaches_router(tmp_path, monkeypatch):
    mod = importlib.import_module('skills.orchestrate.scripts.dispatch_run')
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))
    result = mod.preflight_tasks([{'id':'one','adapter':'claude','model':'opus','prompt':'hello','fallback':True}])
    assert result['status'] == 'validated'
    assert result['routes'][0]['fallback_candidates']


def test_oversized_plain_result_is_partial_and_not_certifiable(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
    plan = fixture_plan(tmp_path, "print('a' * 10000)", intent='assurance', orchestrator_family='anthropic')
    record = mod.execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'partial'
    assert not record['certification_eligible']
    assert any('truncated' in warning for warning in record['warnings'])
    assert (tmp_path / 'result.md').stat().st_size <= 2048


def test_oversized_split_result_cannot_certify_preliminary_text(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
    code = '''import json,sys,time
print(json.dumps({'type':'assistant','message':{'content':'preliminary'}}), flush=True)
result = json.dumps({'type':'result','result':'a'*100000})+'\\n'
for offset in range(0, len(result), 1024):
    sys.stdout.write(result[offset:offset+1024]); sys.stdout.flush(); time.sleep(.001)
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'partial'
    assert any('truncated' in warning for warning in record['warnings'])


def test_malformed_router_fallback_does_not_crash_finished_attempt():
    mod = importlib.import_module('skills.orchestrate.scripts.exec_routing')
    plan = {'adapter':'claude','model':'opus','route':{'fallback_candidates':[None, {'adapter':'future','model':'x'}, {'adapter':'codex','model':'sol'}]}}
    assert mod.candidates(plan, True, {}) == [{'adapter':'codex','model':'sol','effort':None}]


@pytest.mark.parametrize('rollover', [False, True])
def test_successful_terminal_result_supersedes_transient_retry_error(tmp_path, monkeypatch, rollover):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048 if rollover else 20*1024*1024)
    code = '''import json
print(json.dumps({'type':'api_retry','error':'529 overloaded'}))
for i in range(1000): print(json.dumps({'type':'telemetry','data':'x'*100}))
print(json.dumps({'type':'result','is_error':False,'result':'DONE'}))
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'ok'


@pytest.mark.parametrize('rollover', [False, True])
def test_retry_success_never_erases_a_separate_hard_failure(tmp_path, monkeypatch, rollover):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048 if rollover else 20*1024*1024)
    code = '''import json
print(json.dumps({'type':'error','error':'permission denied'}))
print(json.dumps({'type':'api_retry','error':'529 overloaded'}))
for i in range(1000): print(json.dumps({'type':'telemetry','data':'x'*100}))
print(json.dumps({'type':'result','is_error':False,'result':'DONE'}))
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'permission_blocked'


@pytest.mark.parametrize('raw,models,expected', [(None, {}, []), (3, {}, []), ([{'adapter':'codex','model':'sol'}], None, ['sol']), ([{'adapter':'codex','model':'sol'}], {'sol':None}, ['sol'])])
def test_malformed_router_candidate_containers_are_tolerated(raw, models, expected):
    mod = importlib.import_module('skills.orchestrate.scripts.exec_routing')
    plan = {'adapter':'claude','model':'opus','route':{'fallback_candidates':raw}}
    assert [item['model'] for item in mod.candidates(plan, True, {'models':models})] == expected
