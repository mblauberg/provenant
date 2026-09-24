"""Routes and receipts report the effort actually applied, never a made-up one."""

import importlib
import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

ROOT = Path(__file__).resolve().parents[1]
FIX = ROOT / "tests/fixtures/fabric-context"


def resolve(*args):
    result = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "model_route.py"), "resolve", *args],
        text=True, capture_output=True,
        env={**os.environ, "PATH": "", "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
             "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT)},
    )
    route = json.loads(result.stdout) if result.stdout else None
    assert result.returncode == 0, (route, result.stderr)
    assert route["status"] == "ok", route
    return route


def supervisor():
    return importlib.import_module("skills.orchestrate.scripts.provider_exec")


def planned(adapter, route, tmp_path, requested_effort=None):
    return supervisor().build_plan(adapter, route, "hello", cwd=tmp_path,
                                   requested_effort=requested_effort)


def effort_argv(plan):
    argv = plan["argv"]
    return [item for item in argv if item in {"--effort", "--variant"} or "reasoning_effort" in item]


@pytest.mark.parametrize("extra", [(), ("--effort", "high")])
def test_haiku_via_flagship_alias_sends_and_claims_no_effort(tmp_path, extra):
    route = resolve("--adapter", "claude", "--alias", "flagship", "--model", "haiku",
                    "--role", "worker", *extra)
    assert route["resolved_model"] == "haiku"
    assert route["requested_effort"] == "high"
    assert route["effort"] == route["effort_applied"] == ""
    assert any("effort high ignored" in note and "haiku" in note for note in route["notes"])
    assert "haiku" in route["effort_substitution"]
    assert route["effort_capability_source"] == "registry-no-effort-control"
    plan = planned("claude", route, tmp_path, requested_effort="high")
    assert effort_argv(plan) == []
    assert plan["effort"] == ""
    assert plan["route_label"] == "claude/haiku"
    assert any("effort high ignored" in warning and "haiku" in warning for warning in plan["warnings"])


def test_opus_with_high_effort_is_unchanged(tmp_path):
    route = resolve("--adapter", "claude", "--alias", "flagship", "--model", "opus",
                    "--role", "worker", "--effort", "high")
    assert route["effort"] == route["effort_applied"] == "high"
    assert not any("ignored" in note for note in route["notes"])
    plan = planned("claude", route, tmp_path, requested_effort="high")
    assert plan["argv"][plan["argv"].index("--effort") + 1] == "high"
    assert plan["route_label"] == "claude/opus@high"


def test_registered_claude_efforts_bound_what_is_sent(tmp_path):
    """A registry effort list, not the request, decides what reaches the CLI."""
    route = resolve("--adapter", "claude", "--alias", "flagship", "--model", "sonnet",
                    "--role", "worker", "--effort", "ultra")
    assert route["effort_applied"] == "max"
    assert "ultra" in route["effort_substitution"]
    plan = planned("claude", route, tmp_path, requested_effort="ultra")
    assert plan["argv"][plan["argv"].index("--effort") + 1] == "max"


@pytest.mark.parametrize("alias", [(), ("--alias", "flagship")])
def test_codex_unsupported_effort_records_the_clamped_value_sent(tmp_path, alias):
    route = resolve("--adapter", "codex", *alias, "--model", "gpt-6-luna",
                    "--role", "worker", "--effort", "ultra")
    assert route["effort_applied"] == "max"
    plan = planned("codex", route, tmp_path, requested_effort="ultra")
    assert 'model_reasoning_effort="max"' in plan["argv"]
    assert plan["route_label"] == "codex/gpt-6-luna@max"


def test_codex_without_effort_claims_no_default(tmp_path):
    route = resolve("--adapter", "codex", "--model", "gpt-6-luna", "--role", "worker")
    assert route["effort"] == route["effort_applied"] == ""
    plan = planned("codex", route, tmp_path)
    assert effort_argv(plan) == []
    assert plan["route_label"] == "codex/gpt-6-luna"


def test_agy_model_without_effort_control_ignores_effort(tmp_path):
    route = resolve("--adapter", "agy", "--model", "opus", "--role", "worker", "--effort", "high")
    assert route["resolved_model"] == "claude-opus-4-6-thinking"
    assert route["effort_applied"] == ""
    assert any("effort high ignored" in note and "claude-opus-4-6-thinking" in note
               for note in route["notes"])
    plan = planned("agy", route, tmp_path, requested_effort="high")
    assert effort_argv(plan) == []
    assert plan["route_label"] == "agy/claude-opus-4-6-thinking"


def test_agy_gemini_keeps_its_supported_effort(tmp_path):
    route = resolve("--adapter", "agy", "--model", "gemini", "--role", "worker", "--effort", "medium")
    assert route["effort_applied"] == "medium"
    plan = planned("agy", route, tmp_path, requested_effort="medium")
    assert plan["argv"][plan["argv"].index("--effort") + 1] == "medium"


@pytest.mark.parametrize("adapter,model", [
    ("opencode", "glm"),
    ("cursor", "auto"),
    ("cursor", "composer"),
])
def test_other_models_without_effort_control_ignore_effort(tmp_path, adapter, model):
    route = resolve("--adapter", adapter, "--model", model, "--role", "worker", "--effort", "high")
    assert route["effort_applied"] == ""
    assert any(note.startswith("effort high ignored: ") and note.endswith(" has no effort control")
               for note in route["notes"])
    plan = planned(adapter, route, tmp_path, requested_effort="high")
    assert effort_argv(plan) == []
    assert "@" not in plan["route_label"]


def test_fallback_candidate_carrying_an_effort_does_not_resend_it(tmp_path):
    """A fallback re-resolves explicitly, so the router is the one gate for every candidate."""
    routing = importlib.import_module("skills.orchestrate.scripts.exec_routing")
    plan = {"adapter": "claude", "model": "opus", "effort": "high",
            "route": {"fallback_candidates": [{"adapter": "claude", "model": "haiku"}]}}
    [candidate] = routing.candidates(plan, True, {})
    route = resolve("--adapter", candidate["adapter"], "--model", candidate["model"],
                    "--role", "worker", "--effort", candidate["effort"])
    assert route["effort_applied"] == ""
    assert effort_argv(planned("claude", route, tmp_path)) == []


def test_resume_of_an_old_receipt_does_not_resend_an_invented_effort(tmp_path):
    """Receipts written before this fix may record haiku@high; resume re-resolves it away."""
    route = resolve("--adapter", "claude", "--model", "claude-haiku-4-5-20251001",
                    "--role", "worker", "--effort", "high")
    assert route["effort_applied"] == ""
    assert effort_argv(planned("claude", route, tmp_path)) == []


def replay(tmp_path, adapter, route, events, requested_effort=None):
    stream = tmp_path / "stream.jsonl"
    stream.write_text("".join(json.dumps(event) + "\n" for event in events))
    plan = planned(adapter, route, tmp_path, requested_effort=requested_effort)
    plan["argv"] = [sys.executable, "-c", f"import sys; sys.stdout.write(open({str(stream)!r}).read())"]
    plan["grace_seconds"] = 0.1
    return supervisor().execute(plan, tmp_path / "result.md")


def events(name):
    return [json.loads(line) for line in (FIX / f"{name}.jsonl").read_text().splitlines()]


@pytest.fixture
def homes(tmp_path, monkeypatch):
    codex, claude = tmp_path / "codex-home", tmp_path / "claude-config"
    codex.mkdir()
    claude.mkdir()
    monkeypatch.setenv("CODEX_HOME", str(codex))
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude))
    return codex


def test_haiku_receipt_has_empty_applied_effort_and_one_note(tmp_path, homes):
    route = resolve("--adapter", "claude", "--alias", "flagship", "--model", "haiku", "--role", "worker")
    record = replay(tmp_path, "claude", route, events("claude-direct-haiku"), requested_effort="high")
    provenance = record["provenance"]
    assert provenance["effort_requested"] == "high"
    assert provenance["effort_applied"] == ""
    assert "@" not in provenance["line"]
    assert any("effort high ignored" in note and "haiku" in note for note in provenance["notes"])
    assert any("effort high ignored" in warning and "haiku" in warning for warning in record["warnings"])
    records = importlib.import_module("skills.orchestrate.scripts.fabric_records")
    text = records.render_digest({**record, "state": "terminal", "run_id": "mcp-haiku",
                                  "started_at": record.get("started_at"), "ended_at": record.get("ended_at")})
    assert "@" not in text
    assert "effort high ignored" in text


def rollout(codex, session, effort):
    day = codex / "sessions/2026/09/23"
    day.mkdir(parents=True)
    context = {"timestamp": "2026-09-23T05:24:01.000Z", "type": "turn_context",
               "payload": {"cwd": "/tmp", "model": "gpt-6-luna", "effort": effort, "summary": "auto"}}
    (day / f"rollout-2026-09-23T00-00-00-{session}.jsonl").write_text(
        json.dumps(context) + "\n" + (FIX / "codex-rollout.jsonl").read_text())


def test_codex_records_the_effort_it_reports_when_none_was_sent(tmp_path, homes):
    rollout(homes, "session-0001", "medium")
    route = resolve("--adapter", "codex", "--model", "gpt-6-luna", "--role", "worker")
    record = replay(tmp_path, "codex", route, events("codex"))
    provenance = record["provenance"]
    assert provenance["effort_applied"] == "medium"
    assert provenance["effort_observed_source"] == "codex:rollout.turn_context"
    assert provenance["line"].startswith("Route: codex/gpt-6-luna@medium ")


def test_codex_without_a_reported_effort_leaves_applied_empty(tmp_path, homes):
    route = resolve("--adapter", "codex", "--model", "gpt-6-luna", "--role", "worker")
    record = replay(tmp_path, "codex", route, events("codex"))
    provenance = record["provenance"]
    assert provenance["effort_applied"] == ""
    assert provenance["effort_observed_source"] is None
    assert provenance["line"].startswith("Route: codex/gpt-6-luna ")


def test_sent_effort_is_not_replaced_by_the_rollout(tmp_path, homes):
    rollout(homes, "session-0001", "medium")
    route = resolve("--adapter", "codex", "--model", "gpt-6-luna", "--role", "worker", "--effort", "high")
    record = replay(tmp_path, "codex", route, events("codex"), requested_effort="high")
    assert record["provenance"]["effort_applied"] == "high"
    assert record["provenance"]["effort_observed_source"] is None


@pytest.mark.parametrize(("observed_source", "expected"), [("codex:rollout.turn_context", None), (None, "medium")])
def test_resume_resends_only_an_effort_that_was_sent(tmp_path, observed_source, expected):
    dispatch = importlib.import_module("skills.orchestrate.scripts.dispatch_run")
    attempt = tmp_path / "run/tasks/dispatch-001/attempt-001"
    attempt.mkdir(parents=True)
    (attempt / "attempt.json").write_text(json.dumps({
        "run_id": "mcp-effort", "task_id": "dispatch-001", "attempt": 1, "state": "terminal", "status": "ok",
        "mode": "read_only", "worktree": None, "cwd": str(tmp_path), "session_id": "session-0001",
        "applied": {"sandbox": "read-only", "network": None, "add_dirs": []},
        "provenance": {"requested": {"adapter": "codex"}, "resolved_model": "gpt-6-luna",
                       "effort_applied": "medium", "effort_observed_source": observed_source},
    }))
    args = dispatch.argparse.Namespace(run_dir=tmp_path / "run", resume="mcp-effort", task_id=None,
                                       tool=None, model=None)
    try:
        dispatch.prepare_resume(args)
    except Exception:
        pass  # later relaunch checks need a real run; the effort is decided first
    assert args.effort == expected


def test_cursor_model_suffix_effort_is_recorded_as_sent(tmp_path):
    route = resolve("--adapter", "cursor", "--model", "grok-4.7", "--role", "worker", "--effort", "high")
    assert route["resolved_model"] == "grok-4.7-high"
    plan = planned("cursor", route, tmp_path, requested_effort="high")
    assert plan["effort"] == "high"
    assert plan["route_label"] == "cursor/grok-4.7-high@high"
    assert not any("does not expose effort control" in warning for warning in plan["warnings"])


@pytest.mark.parametrize(("extra", "model", "effort"), [
    (("--alias", "flagship", "--effort", "high"), "grok-4.7-high", "high"),
    (("--alias", "flagship", "--effort", "medium"), "grok-4.7-medium", "medium"),
    (("--alias", "flagship", "--effort", "ultra"), "grok-4.7-xhigh", "xhigh"),
    (("--alias", "flagship"), "grok-4.7-high", "high"),
])
def test_a_suffix_model_carries_the_effort_it_records_with_an_alias(extra, model, effort):
    """An alias with a model used to hit the adapter-wide model-id rule and refuse the route."""
    route = resolve("--adapter", "cursor", "--model", "grok-4.7", "--role", "worker", *extra)
    assert route["resolved_model"] == model
    assert route["effort_applied"] == effort


@pytest.mark.parametrize(("model", "extra", "resolved", "effort"), [
    ("grok-4.7-high", ("--effort", "medium"), "grok-4.7-medium", "medium"),
    ("grok-4.7-low", (), "grok-4.7-low", "low"),
])
def test_a_suffixed_id_is_never_suffixed_twice(model, extra, resolved, effort):
    route = resolve("--adapter", "cursor", "--alias", "flagship", "--model", model, "--role", "worker", *extra)
    assert route["resolved_model"] == resolved
    assert route["effort_applied"] == effort
