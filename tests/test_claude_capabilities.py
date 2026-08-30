import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace

import pytest


ROOT = Path(__file__).resolve().parents[1]
PATH = ROOT / "skills" / "orchestrate" / "scripts" / "claude_capabilities.py"


def load_module():
    spec = importlib.util.spec_from_file_location("claude_capabilities", PATH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fake_claude(
    tmp_path, *, auth_method="claude.ai", model_usage=None, is_error=False,
    effort_warning=False, stderr_warning=False,
):
    path = tmp_path / "claude"
    path.write_text(f'''#!/usr/bin/env python3
import json
import sys
if sys.argv[1:3] == ["auth", "status"]:
    print(json.dumps({{
        "loggedIn": True, "authMethod": {auth_method!r}, "subscriptionType": "pro",
        "email": "secret@example.com", "orgId": "secret-org"
    }}))
else:
    required = ["-p", "--safe-mode", "--no-session-persistence", "--permission-mode", "plan", "--tools", "", "--model", "opus", "--effort", "medium", "--output-format", "json"]
    assert all(item in sys.argv[1:] for item in required)
    if {effort_warning!r}:
        print("Warning: Unknown --effort value 'medium' - ignoring it and using the default effort.", file=sys.stderr)
        print("Valid values: low, medium, high, xhigh, max.", file=sys.stderr)
    if {stderr_warning!r}:
        print("provider warning", file=sys.stderr)
    print(json.dumps({{
        "type": "result", "subtype": "success", "is_error": {is_error!r},
        "result": "OK", "modelUsage": {model_usage or {
            'claude-haiku-4-5-20251001': {'inputTokens': 1},
            'claude-opus-4-8': {'inputTokens': 1},
        }!r}
    }}))
''')
    path.chmod(0o700)
    return path


def test_subscription_canary_emits_scrubbed_runtime_capability(tmp_path):
    module = load_module()
    output = tmp_path / "capabilities.json"
    executable = fake_claude(tmp_path)

    assert module.main([
        "--out", str(output), "--claude-bin", str(executable),
        "--alias", "opus", "--effort", "medium",
    ]) == 0

    snapshot = json.loads(output.read_text())
    assert snapshot["source"] == "claude subscription canary"
    assert snapshot["provenance"] == {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    assert snapshot["models"] == {
        "opus": {
            "resolved_model": "claude-opus-4-8",
            "requested_effort": "medium",
            "effort_verified": False,
        }
    }
    encoded = output.read_text()
    assert "secret@example.com" not in encoded
    assert "secret-org" not in encoded


@pytest.mark.parametrize(
    "kwargs",
    [
        {"auth_method": "apiKey"},
        {"model_usage": {"claude-opus-4-8": {}, "claude-opus-4-9": {}}},
        {"is_error": True},
    ],
)
def test_subscription_canary_rejects_unproven_or_ambiguous_results(tmp_path, kwargs):
    module = load_module()
    output = tmp_path / "capabilities.json"
    executable = fake_claude(tmp_path, **kwargs)

    assert module.main([
        "--out", str(output), "--claude-bin", str(executable),
        "--alias", "opus", "--effort", "medium",
    ]) == 1
    assert not output.exists()


def test_subscription_canary_rejects_unknown_effort_warning(tmp_path):
    module = load_module()
    output = tmp_path / "capabilities.json"
    executable = fake_claude(tmp_path, effort_warning=True)

    assert module.main([
        "--out", str(output), "--claude-bin", str(executable),
        "--alias", "opus", "--effort", "medium",
    ]) == 1
    assert not output.exists()


def test_subscription_canary_ignores_unrelated_stderr_warning(tmp_path):
    module = load_module()
    output = tmp_path / "capabilities.json"
    executable = fake_claude(tmp_path, stderr_warning=True)

    assert module.main([
        "--out", str(output), "--claude-bin", str(executable),
        "--alias", "opus", "--effort", "medium",
    ]) == 0

    assert json.loads(output.read_text())["models"]["opus"]["resolved_model"] == "claude-opus-4-8"


def test_unknown_effort_warning_is_read_from_stderr(monkeypatch):
    module = load_module()
    payload = json.dumps({"ok": True})
    warning = "Warning: Unknown --effort value"
    result = SimpleNamespace(
        returncode=0,
        output=payload,
        stdout=payload,
        stderr=warning,
        timed_out=False,
    )
    monkeypatch.setattr(module, "run_bounded", lambda *args, **kwargs: result)

    with pytest.raises(ValueError, match="Claude CLI rejected the requested effort"):
        module.run_json(["claude"], 2)


def test_nonzero_exit_reports_stderr(monkeypatch):
    module = load_module()
    result = SimpleNamespace(
        returncode=9,
        output="",
        stdout="",
        stderr="claude failed",
        timed_out=False,
    )
    monkeypatch.setattr(module, "run_bounded", lambda *args, **kwargs: result)

    with pytest.raises(ValueError, match="command exited 9: claude failed"):
        module.run_json(["claude"], 2)


def test_nonzero_provider_stdout_keeps_only_scrubbed_policy_reason(monkeypatch):
    module = load_module()
    result = SimpleNamespace(
        returncode=1,
        output="",
        stdout=(
            "Your organization has disabled Claude subscription access for "
            "Claude Code; contact secret@example.com in org secret-org"
        ),
        stderr="",
        timed_out=False,
    )
    monkeypatch.setattr(module, "run_bounded", lambda *args, **kwargs: result)

    with pytest.raises(
        ValueError,
        match="command exited 1: provider access denied; subscription access disabled by organisation policy",
    ) as error:
        module.run_json(["claude"], 2)

    assert "secret@example.com" not in str(error.value)
    assert "secret-org" not in str(error.value)


def test_ultra_effort_is_rejected_before_claude_subprocess(monkeypatch, tmp_path):
    module = load_module()

    def fail_bounded(*args, **kwargs):
        pytest.fail("claude subprocess should not be called")

    monkeypatch.setattr(module, "run_bounded", fail_bounded)
    with pytest.raises(SystemExit) as exc_info:
        module.main([
            "--out", str(tmp_path / "capabilities.json"),
            "--alias", "opus", "--effort", "ultra",
        ])

    assert exc_info.value.code != 0
