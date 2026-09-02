"""Capability discovery for every configured provider, through one module.

The per-provider expectations are the ones the three separate suites asserted
before the module was folded together, so a changed snapshot fails here.
"""

import importlib.util
import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest


ROOT = Path(__file__).resolve().parents[1]
PATH = ROOT / "skills" / "orchestrate" / "scripts" / "capabilities.py"


def load_module():
    spec = importlib.util.spec_from_file_location("capabilities", PATH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    # dataclasses resolves annotations through sys.modules, so a module loaded
    # straight off a path has to be registered before it executes.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


MODULE = load_module()


def write_executable(path: Path, body: str) -> Path:
    path.write_text(body)
    path.chmod(0o700)
    return path


def fake_codex(tmp_path, *, stderr="", catalogue=None):
    payload = catalogue or (
        '{"models":[{"slug":"gpt-5.6-sol",'
        '"supported_reasoning_levels":[{"effort":"high"}]}]}'
    )
    return write_executable(
        tmp_path / "codex",
        "#!/usr/bin/env python3\n"
        f"import sys; sys.stderr.write({stderr!r}); "
        f"sys.stdout.write({payload!r})",
    )


def fake_agy(tmp_path, *, stderr="", listing="gemini-3.1-pro-high\ngemini-3.1-pro-low\n"):
    return write_executable(
        tmp_path / "agy",
        "#!/usr/bin/env python3\n"
        f"import sys; sys.stderr.write({stderr!r}); "
        f"sys.stdout.write({listing!r})",
    )


def fake_claude(
    tmp_path, *, auth_method="claude.ai", model_usage=None, is_error=False,
    effort_warning=False, stderr_warning=False,
):
    return write_executable(tmp_path / "claude", f'''#!/usr/bin/env python3
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


# provider, fake executable factory, extra route arguments, expected models,
# label the failing-run diagnostic must name.
PROVIDERS = [
    pytest.param(
        "codex",
        fake_codex,
        [],
        {"gpt-5.6-sol": {"resolved_model": "gpt-5.6-sol", "supported_efforts": ["high"]}},
        "codex debug models",
        id="codex",
    ),
    pytest.param(
        "agy",
        fake_agy,
        [],
        {
            "gemini-3.1-pro": {
                "resolved_model": "gemini-3.1-pro",
                "supported_efforts": ["low", "high"],
                "dispatchable_ids": ["gemini-3.1-pro-high", "gemini-3.1-pro-low"],
            }
        },
        "agy models",
        id="agy",
    ),
    pytest.param(
        "claude",
        fake_claude,
        ["--alias", "opus", "--effort", "medium"],
        {
            "opus": {
                "resolved_model": "claude-opus-4-8",
                "requested_effort": "medium",
                "effort_verified": False,
            }
        },
        "command",
        id="claude",
    ),
]


@pytest.mark.parametrize("provider,fake,route,expected,label", PROVIDERS)
def test_discovery_reports_the_documented_capabilities(
    tmp_path, provider, fake, route, expected, label
):
    output = tmp_path / "capabilities.json"

    assert MODULE.main(
        [provider, "--out", str(output), "--bin", str(fake(tmp_path)), *route]
    ) == 0

    assert json.loads(output.read_text())["models"] == expected


@pytest.mark.parametrize("provider,fake,route,expected,label", PROVIDERS)
def test_discovery_ignores_an_unrelated_stderr_warning(
    tmp_path, provider, fake, route, expected, label
):
    output = tmp_path / "capabilities.json"
    kwargs = {"stderr_warning": True} if provider == "claude" else {"stderr": "provider warning\n"}

    assert MODULE.main(
        [provider, "--out", str(output), "--bin", str(fake(tmp_path, **kwargs)), *route]
    ) == 0

    assert json.loads(output.read_text())["models"] == expected


@pytest.mark.parametrize("provider,fake,route,expected,label", PROVIDERS)
def test_nonzero_exit_reports_scrubbed_stderr(
    tmp_path, capsys, provider, fake, route, expected, label
):
    executable = write_executable(
        tmp_path / provider,
        "#!/usr/bin/env python3\n"
        f"import sys; sys.stderr.write('{provider} failed\\n'); sys.exit(7)",
    )
    output = tmp_path / "capabilities.json"

    assert MODULE.main(
        [provider, "--out", str(output), "--bin", str(executable), *route]
    ) == 1

    assert f"{label} exited 7: {provider} failed" in capsys.readouterr().err
    assert not output.exists()


@pytest.mark.parametrize("provider", ["agy", "codex"])
def test_a_catalogue_provider_prints_to_stdout_without_an_out_path(tmp_path, capsys, provider):
    fake = {"agy": fake_agy, "codex": fake_codex}[provider]

    assert MODULE.main([provider, "--bin", str(fake(tmp_path))]) == 0

    assert json.loads(capsys.readouterr().out)["source"] in {"agy models", "codex debug models"}


def test_claude_requires_the_route_it_proves_and_an_output_path(tmp_path):
    for argv in (
        ["claude", "--alias", "opus", "--effort", "medium"],
        ["claude", "--out", str(tmp_path / "c.json"), "--alias", " ", "--effort", "medium"],
        ["claude", "--out", str(tmp_path / "c.json"), "--alias", "opus"],
    ):
        with pytest.raises(SystemExit) as exc_info:
            MODULE.main(argv)
        assert exc_info.value.code != 0


def test_a_catalogue_provider_rejects_route_arguments(tmp_path):
    with pytest.raises(SystemExit) as exc_info:
        MODULE.main(["codex", "--alias", "opus"])
    assert exc_info.value.code != 0


def test_agy_accepts_the_current_tab_separated_model_listing():
    snapshot = MODULE.normalise_agy(
        "gemini-3.7-flash-high\tGemini 3.7 Flash (High)\n"
        "gemini-3.7-flash-medium\tGemini 3.7 Flash (Medium)\n"
        "gemini-3.7-flash-low\tGemini 3.7 Flash (Low)\n"
        "claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n"
    )

    assert snapshot["models"]["gemini-3.7-flash"] == {
        "resolved_model": "gemini-3.7-flash",
        "supported_efforts": ["low", "medium", "high"],
        "dispatchable_ids": [
            "gemini-3.7-flash-high",
            "gemini-3.7-flash-low",
            "gemini-3.7-flash-medium",
        ],
    }
    assert snapshot["effortless_models"] == ["claude-opus-4-6-thinking"]


def test_codex_keeps_model_specific_efforts_only():
    value = MODULE.normalise_codex({"models": [
        {"slug": "gpt-5.6-sol", "supported_reasoning_levels": [{"effort": "ultra"}, {"effort": "max"}]},
        {"slug": "gpt-5.6-luna", "supported_reasoning_levels": [{"effort": "max"}]},
    ]})
    assert value["models"]["gpt-5.6-sol"]["supported_efforts"] == ["ultra", "max"]
    assert value["models"]["gpt-5.6-luna"]["supported_efforts"] == ["max"]


@pytest.mark.parametrize(
    "malformed",
    [
        "not a model entry",
        {"slug": 7, "supported_reasoning_levels": []},
        {"slug": "", "supported_reasoning_levels": []},
        {"slug": "gpt-malformed"},
        {"slug": "gpt-malformed", "supported_reasoning_levels": {}},
        {"slug": "gpt-malformed", "supported_reasoning_levels": ["high"]},
        {"slug": "gpt-malformed", "supported_reasoning_levels": [{"effort": 7}]},
        {"slug": "gpt-malformed", "supported_reasoning_levels": [{"effort": " "}]},
    ],
)
def test_codex_rejects_an_entire_mixed_payload_for_any_malformed_entry(malformed):
    with pytest.raises(ValueError):
        MODULE.normalise_codex({
            "models": [
                {"slug": "gpt-5.6-sol", "supported_reasoning_levels": [{"effort": "high"}]},
                malformed,
            ]
        })


@pytest.mark.parametrize(
    "catalogue",
    [
        {},
        {"models": []},
        {"models": [{"display_name": "missing slug"}]},
        {"models": [{"slug": "gpt-empty", "supported_reasoning_levels": []}]},
        {
            "models": [
                {"slug": "GPT-Duplicate", "supported_reasoning_levels": [{"effort": "high"}]},
                {"slug": "gpt-duplicate", "supported_reasoning_levels": [{"effort": "max"}]},
            ]
        },
    ],
)
def test_codex_rejects_empty_malformed_and_casefolded_duplicate_catalogues(catalogue):
    with pytest.raises(ValueError):
        MODULE.normalise_codex(catalogue)


@pytest.mark.parametrize(
    "raw",
    [
        '{"models":[{"slug":"gpt-a","supported_reasoning_levels":[{"effort":"high"}]}],'
        '"models":[{"slug":"gpt-b","supported_reasoning_levels":[{"effort":"max"}]}]}',
        '{"models":[{"slug":"gpt-a","slug":"gpt-b",'
        '"supported_reasoning_levels":[{"effort":"high"}]}]}',
        '{"models":[{"slug":"gpt-a","supported_reasoning_levels":[{"effort":"high"}],'
        '"supported_reasoning_levels":[{"effort":"max"}]}]}',
        '{"models":[{"slug":"gpt-a",'
        '"supported_reasoning_levels":[{"effort":"high","effort":"max"}]}]}',
    ],
)
def test_discovery_rejects_duplicate_json_members_before_normalisation(tmp_path, monkeypatch, raw):
    result = SimpleNamespace(returncode=0, output=raw, stdout=raw, stderr="", timed_out=False)
    monkeypatch.setattr(MODULE, "run_bounded", lambda *args, **kwargs: result)
    output = tmp_path / "capabilities.json"

    assert MODULE.main(["codex", "--out", str(output)]) == 1
    assert not output.exists()


def test_claude_canary_emits_scrubbed_runtime_provenance(tmp_path):
    output = tmp_path / "capabilities.json"

    assert MODULE.main([
        "claude", "--out", str(output), "--bin", str(fake_claude(tmp_path)),
        "--alias", "opus", "--effort", "medium",
    ]) == 0

    encoded = output.read_text()
    snapshot = json.loads(encoded)
    assert snapshot["source"] == "claude subscription canary"
    assert snapshot["provenance"] == {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    assert "secret@example.com" not in encoded
    assert "secret-org" not in encoded


@pytest.mark.parametrize(
    "kwargs",
    [
        {"auth_method": "apiKey"},
        {"model_usage": {"claude-opus-4-8": {}, "claude-opus-4-9": {}}},
        {"is_error": True},
        {"effort_warning": True},
    ],
)
def test_claude_canary_rejects_unproven_or_ambiguous_results(tmp_path, kwargs):
    output = tmp_path / "capabilities.json"

    assert MODULE.main([
        "claude", "--out", str(output), "--bin", str(fake_claude(tmp_path, **kwargs)),
        "--alias", "opus", "--effort", "medium",
    ]) == 1
    assert not output.exists()


def test_claude_reads_the_unknown_effort_warning_from_stderr(monkeypatch):
    payload = json.dumps({"ok": True})
    result = SimpleNamespace(
        returncode=0,
        output=payload,
        stdout=payload,
        stderr="Warning: Unknown --effort value",
        timed_out=False,
    )
    monkeypatch.setattr(MODULE, "run_bounded", lambda *args, **kwargs: result)

    with pytest.raises(ValueError, match="Claude CLI rejected the requested effort"):
        MODULE.claude_json(["claude"], 2)


def test_a_failing_provider_stdout_keeps_only_a_scrubbed_policy_reason(monkeypatch):
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
    monkeypatch.setattr(MODULE, "run_bounded", lambda *args, **kwargs: result)

    with pytest.raises(
        ValueError,
        match="command exited 1: provider access denied; subscription access disabled by organisation policy",
    ) as error:
        MODULE.run_cli(["claude"], 2)

    assert "secret@example.com" not in str(error.value)
    assert "secret-org" not in str(error.value)


def test_an_effort_outside_the_table_is_rejected_before_any_subprocess(monkeypatch, tmp_path):
    def fail_bounded(*args, **kwargs):
        pytest.fail("no provider subprocess should be started")

    monkeypatch.setattr(MODULE, "run_bounded", fail_bounded)

    with pytest.raises(SystemExit) as exc_info:
        MODULE.main([
            "claude", "--out", str(tmp_path / "capabilities.json"),
            "--alias", "opus", "--effort", "ultra",
        ])
    assert exc_info.value.code != 0
