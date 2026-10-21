import importlib
import json
import os
import subprocess
import sys
from pathlib import Path
from datetime import UTC, datetime, timedelta
import pytest

FIX = Path(__file__).parent / "fixtures/fabric-v1"


def records():
    return importlib.import_module("skills.orchestrate.scripts.fabric_records")


@pytest.fixture(autouse=True)
def isolated_identity(tmp_path, monkeypatch):
    monkeypatch.setenv('HOME', str(tmp_path / 'home'))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    for name in ('CLAUDE_CONFIG_DIR', 'CODEX_HOME', 'XDG_DATA_HOME'):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(records().identity_resolver(), 'host_id', lambda: 'host-a')


def stored_routes(path):
    return {entry['adapter'] + '/' + entry['model']: entry
            for entry in json.loads(path.read_text())['cooldowns'].values()}


@pytest.mark.parametrize(('provider', 'environment'), [
    ('claude', 'CLAUDE_CONFIG_DIR'), ('codex', 'CODEX_HOME'), ('opencode', 'XDG_DATA_HOME')])
def test_unannotated_account_names_default_identity(tmp_path, monkeypatch, provider, environment):
    resolver = records().identity_resolver()
    monkeypatch.setenv(environment, str(tmp_path / 'alternate'))
    alternate = resolver.selected_identity(provider)
    config = tmp_path / 'instance/config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {provider: {
        'a': {}, 'b': {'identity_dir': str(alternate)}}}}))
    assert resolver.scope(provider)['account'] == 'b'
    assert resolver.scope(provider, default=True)['account'] == 'a'

    config.write_text(json.dumps({'accounts': {provider: {'a': {}}}}))
    assert resolver.scope(provider)['account'].startswith('path-')
    assert resolver.scope(provider, default=True)['account'] == 'a'

    config.write_text(json.dumps({'accounts': {provider: {
        'a': {}, 'b': {'identity_dir': str(resolver.default_identity(provider))}}}}))
    assert resolver.scope(provider, default=True)['account'] == 'b'


def test_cooldowns_follow_selected_claude_identity(tmp_path, monkeypatch):
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'a'))
    path = tmp_path / 'cooldowns.json'
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='rate_limited')
    row['provenance']['requested']['adapter'] = 'claude'
    records().write_cooldown(row, path=path)
    assert records().read_cooldowns(path)
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'b'))
    assert records().read_cooldowns(path) == {}
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'a'))
    assert records().read_cooldowns(path)


def test_digest_matches_shared_example_and_question():
    row = json.loads((FIX / "attempt.json").read_text())
    assert records().render_digest(row) == row["digest"]
    row.update(status="input_required", question="Target main or release/3?")
    assert (
        records().render_digest(row)
        == 'input_required mcp-a81f3c "Target main or release/3?" · reply: fabric_dispatch{resume:"mcp-a81f3c",prompt:"…"}'
    )


def test_cooldown_locked_merge_expiry_and_defaults(tmp_path):
    module = records()
    path = tmp_path / "cooldowns.json"
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="usage_limited", reset_at=None, retry_after=None)
    row["provenance"]["requested"]["adapter"] = "claude"
    row["provenance"]["resolved_model"] = "opus"
    at = datetime(2026, 9, 23, tzinfo=UTC)
    module.write_cooldown(row, path=path, at=at)
    assert stored_routes(path)["claude/claude-opus-5-5"]["cooling_until"] == "2026-09-23T01:00:00Z"
    row["provenance"]["resolved_model"] = "sonnet"
    row.update(status="rate_limited", retry_after=30)
    module.write_cooldown(row, path=path, at=at + timedelta(hours=2))
    assert set(stored_routes(path)) == {"claude/claude-sonnet-5-5"}
    assert (tmp_path / "cooldowns.lock").is_file()


def test_index_keeps_distinct_attempts_and_deduplicates_recovery(tmp_path):
    module = records()
    row = json.loads((FIX / "attempt.json").read_text())
    for attempt in (1, 1, 2):
        row["attempt"] = attempt
        module.append_index(row, tmp_path / "runs/example", root=tmp_path)
    entries = [
        json.loads(line)
        for line in (tmp_path / "runs/index.jsonl").read_text().splitlines()
    ]
    assert [item["attempt"] for item in entries] == [1, 2]
    assert all(item["line"] == row["provenance"]["line"] for item in entries)


def test_cooldown_writer_refuses_symlinked_store(tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir()
    linked = tmp_path / "linked"
    linked.symlink_to(outside, target_is_directory=True)
    row = json.loads((FIX / "attempt.json").read_text())
    row["status"] = "rate_limited"
    with pytest.raises((ValueError, OSError)):
        records().write_cooldown(row, path=linked / "cooldowns.json")
    assert list(outside.iterdir()) == []


def test_cooldown_normalizes_registered_id_and_preserves_origin(tmp_path, monkeypatch):
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(FIX.parents[2]))
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='rate_limited')
    row['provenance']['requested']['adapter'] = 'codex'
    row['provenance']['resolved_model'] = 'sol@high'
    path = tmp_path / 'cooldowns.json'
    records().write_cooldown(row, path=path)
    first = stored_routes(path)
    assert 'codex/gpt-6.1-sol' in first
    row['run_id'] = 'synthetic'
    row['evidence']['signature'] = 'cooldown_active'
    records().write_cooldown(row, path=path)
    assert stored_routes(path) == first


def test_account_usage_limit_cools_all_models(tmp_path):
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='usage_limited')
    row['provenance']['requested']['adapter'] = 'claude'
    row['evidence'].update(signature='usage_limited', excerpt="You've hit your weekly limit")
    path = tmp_path / 'cooldowns.json'
    records().write_cooldown(row, path=path)
    assert 'claude/*' in stored_routes(path)


def test_digest_result_is_openable_from_caller_and_fallback_shows_reset(tmp_path):
    row = json.loads((FIX / 'attempt.json').read_text())
    row['run_dir'] = str(tmp_path)
    assert str(tmp_path / row['paths']['result']) in records().render_digest(row)
    row.update(state='running', status=None)
    row['provenance']['fallback_from'] = {'attempt':1,'status':'usage_limited','route':'claude/opus','reset_at':'2026-09-23T14:00:00Z'}
    assert '(resets 2026-09-23T14:00:00Z)' in records().render_digest(row)


@pytest.mark.parametrize('model', ['grok', 'grok-4.7-high', 'grok-4.7@high'])
def test_cooldown_identity_strips_provider_effort_suffix(tmp_path, model):
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='rate_limited')
    row['provenance']['requested']['adapter'] = 'cursor'
    row['provenance']['resolved_model'] = model
    path = tmp_path / 'cooldowns.json'
    records().write_cooldown(row, path=path)
    assert set(stored_routes(path)) == {'cursor/grok-4.7'}


def test_cooldown_path_uses_state_root_between_override_and_default(tmp_path, monkeypatch):
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path))
    monkeypatch.delenv('FABRIC_COOLDOWNS_PATH', raising=False)
    assert records().cooldown_path() == tmp_path / 'cooldowns.json'
    override = tmp_path / 'explicit.json'
    monkeypatch.setenv('FABRIC_COOLDOWNS_PATH', str(override))
    assert records().cooldown_path() == override


def test_route_health_tracks_recent_outcomes_and_expires_stale_cooldowns(tmp_path, monkeypatch):
    module = records()
    monkeypatch.setenv("AGENT_FABRIC_ROUTE_HEALTH_PATH", str(tmp_path / "health.json"))
    monkeypatch.setenv("FABRIC_COOLDOWNS_PATH", str(tmp_path / "cooldowns.json"))
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="failed", task_class="review", run_id="run-1")
    row["provenance"]["requested"]["adapter"] = "codex"
    row["provenance"]["resolved_model"] = "gpt-6.1-sol"
    module.write_route_health(row, at=datetime(2026, 9, 23, tzinfo=UTC))
    row.update(status="ok", run_id="run-2")
    module.write_route_health(row, at=datetime(2026, 9, 24, tzinfo=UTC))
    row.update(status="rate_limited", run_id="run-3")
    module.write_route_health(row, at=datetime(2026, 9, 24, 1, tzinfo=UTC))
    cooldowns = tmp_path / "cooldowns.json"
    cooldowns.write_text(json.dumps({"schema": "fabric.cooldowns.v1", "cooldowns": {
        "codex/gpt-6.1-sol": {"cooling_until": "2026-09-23T01:00:00Z", "status": "rate_limited"},
    }}))
    health = module.read_route_health(at=datetime(2026, 9, 24, 1, tzinfo=UTC))
    item = health["codex|gpt-6.1-sol|review"]
    assert [entry["status"] for entry in item["recent"]] == ["rate_limited", "ok", "failed"]
    assert item["recent_failures"] == 1
    assert item["rate_limits"] == 1
    assert item["cooling_until"] == ""


@pytest.mark.parametrize("status,signature", [("permission_blocked", "permission_blocked"), ("ok", "exit_after_output")])
def test_route_health_keeps_sandbox_failures_and_output_warnings_neutral(tmp_path, status, signature):
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status=status, task_class="review")
    row["evidence"]["signature"] = signature
    path = tmp_path / "health.json"
    at = datetime(2026, 9, 24, tzinfo=UTC)
    records().write_route_health(row, path=path, at=at)
    health = records().read_route_health(path, cooldowns_path=tmp_path / "cooldowns.json", at=at)
    item = next(iter(health.values()))
    assert item["recent_failures"] == 0
    assert item["recent"][0]["status"] == status
    if signature == "exit_after_output":
        assert item["recent"][0]["warning"] == signature


def test_malformed_catalogue_keeps_cooldown_identity_and_warns(tmp_path, monkeypatch):
    from skills.orchestrate.scripts import exec_routing
    monkeypatch.setattr(exec_routing, 'snapshot', lambda: {'adapters': {'codex': {'models': [{}]}}})
    row = json.loads((FIX / 'attempt.json').read_text())
    row['status'] = 'rate_limited'
    row['provenance']['requested']['adapter'] = 'codex'
    row['provenance']['resolved_model'] = 'raw-model'
    path = tmp_path / 'cooldowns.json'
    records().write_cooldown(row, path=path)
    assert 'codex/raw-model' in stored_routes(path)
    assert any('catalogue' in warning for warning in row['warnings'])


def test_digest_surfaces_warnings_once_without_blocking():
    row = json.loads((FIX / "attempt.json").read_text())
    row["warnings"] = ["ultra unknown; ran at medium", "ultra unknown; ran at medium"]
    text = records().render_digest(row)
    assert text.startswith("ok ")
    assert text.endswith("\n  ! ultra unknown; ran at medium")


def test_digest_uses_first_meaningful_excerpt_line_when_fix_and_signature_are_empty():
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="failed", fix=None)
    row["evidence"].update(
        signature=None,
        excerpt="\n  error: invalid model selection\nsecond line is not the digest",
    )
    text = records().render_digest(row)
    assert "· error: invalid model selection" in text
    assert "second line is not the digest" not in text


def test_digest_bounds_excerpt_fallback_to_one_line():
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="failed", fix=None)
    row["evidence"].update(signature=None, excerpt="x" * 140 + "\nsecond line")
    detail = records().render_digest(row).split(" · ", 1)[1].split("\n", 1)[0]
    assert detail == "x" * 120


def test_agy_quota_cools_only_the_exhausted_model(tmp_path):
    """Antigravity meters each hosted model pool separately (2026-09-23: Claude
    Sonnet 4.6 hit "Individual quota reached" while Gemini answered in the same
    batch), so its quota message must not cool the whole adapter."""
    path = tmp_path / "cooldowns.json"
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="usage_limited", reset_at="2026-09-25T00:22:25Z", retry_after=None)
    row["provenance"]["requested"]["adapter"] = "agy"
    row["provenance"]["resolved_model"] = "claude-sonnet-4-6"
    row["evidence"] = {"signature": "usage_limited", "excerpt": "provider error: Individual quota reached. Resets in 41h56m11s."}
    at = datetime(2026, 9, 23, tzinfo=UTC)
    records().write_cooldown(row, path=path, at=at)
    assert set(stored_routes(path)) == {"agy/claude-sonnet-4-6"}
    row["provenance"]["requested"]["adapter"] = "claude"
    row["provenance"]["resolved_model"] = "opus"
    row["evidence"]["excerpt"] = "You've hit your session limit"
    records().write_cooldown(row, path=path, at=at)
    assert "claude/*" in stored_routes(path)


def test_quarantined_digest_keeps_result_and_review_action(tmp_path):
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='quarantined', landable=False, run_dir=str(tmp_path),
               quarantine={'reason': 'unverifiable: instruction snapshot unavailable',
                           'patches': [str(tmp_path / 'protected.patch')], 'restored': False})
    text = records().render_digest(row)
    assert text.startswith('quarantined ')
    assert str(tmp_path / row['paths']['result']) in text
    assert 'not landable' in text and 'review' in text
    assert row['quarantine']['reason'] in text
    assert row['quarantine']['patches'][0] in text


def test_quarantined_route_does_not_count_as_provider_failure(tmp_path):
    row = json.loads((FIX / 'attempt.json').read_text())
    row['status'] = 'quarantined'
    path = tmp_path / 'route-health.json'
    at = datetime.now(UTC)
    records().write_route_health(row, path=path, at=at)
    health = records().read_route_health(path, cooldowns_path=tmp_path / 'cooldowns.json', at=at)
    assert next(iter(health.values()))['recent_failures'] == 0


@pytest.mark.parametrize("status", ["ok", "partial", "failed", "input_required"])
def test_only_success_clears_its_own_cooldown(tmp_path, status):
    path = tmp_path / "cooldowns.json"
    at = datetime(2026, 9, 23, tzinfo=UTC)
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="rate_limited")
    row["provenance"]["requested"]["adapter"] = "agy"
    for model in ("claude-sonnet-4-6", "gemini-3.5-flash", "*"):
        row["provenance"]["resolved_model"] = model
        records().write_cooldown(row, path=path, at=at)
    row["provenance"].update(resolved_model="claude-sonnet-4-6", observed_model="claude-sonnet-4-6")
    row.update(status=status)
    records().write_cooldown(row, path=path, at=at + timedelta(minutes=1))
    remaining = records().read_cooldowns(path, at)
    assert ("agy/claude-sonnet-4-6" in remaining) is (status != "ok")
    assert {"agy/gemini-3.5-flash", "agy/*"} <= remaining.keys()


def test_substituted_success_clears_only_the_model_that_answered(tmp_path):
    path = tmp_path / "cooldowns.json"
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="rate_limited")
    row["provenance"]["requested"]["adapter"] = "claude"
    for model in ("opus", "sonnet"):
        row["provenance"]["resolved_model"] = model
        records().write_cooldown(row, path=path)
    row.update(status="ok")
    row["provenance"].update(resolved_model="opus", observed_model="sonnet")
    records().write_cooldown(row, path=path)
    assert set(records().read_cooldowns(path)) == {"claude/claude-opus-5-5"}


def test_cooldown_expires_after_its_window_and_reprobe_rearms_it(tmp_path):
    path = tmp_path / "cooldowns.json"
    at = datetime(2026, 9, 23, tzinfo=UTC)
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="usage_limited", reset_at=None, retry_after=24 * 3600)
    row["provenance"]["requested"]["adapter"] = "opencode"
    row["provenance"]["resolved_model"] = "opencode-go/glm-5"
    records().write_cooldown(row, path=path, at=at)
    assert "opencode/opencode-go/glm-5" in records().read_cooldowns(path, at + timedelta(hours=23))
    assert not records().read_cooldowns(path, at + timedelta(hours=25))
    records().write_cooldown(row, path=path, at=at + timedelta(hours=25))  # failed probe re-arms
    assert "opencode/opencode-go/glm-5" in records().read_cooldowns(path, at + timedelta(hours=26))


@pytest.mark.parametrize("model,expected", [
    ("opencode-go/glm-5", "opencode/opencode-go/glm-5"),
    ("opencode/some-free-model", "opencode/some-free-model"),
])
def test_opencode_usage_limit_stays_scoped_to_its_model(tmp_path, model, expected):
    path = tmp_path / "cooldowns.json"
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="usage_limited", reset_at=None, retry_after=None)
    row["evidence"]["excerpt"] = "Monthly usage limit reached"
    row["provenance"]["requested"]["adapter"] = "opencode"
    row["provenance"]["resolved_model"] = model
    records().write_cooldown(row, path=path)
    keys = set(records().read_cooldowns(path))
    assert keys and "opencode/*" not in keys


def limited(adapter, model, at, path, status="usage_limited", excerpt="usage limit reached"):
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status=status, reset_at=None, retry_after=None)
    row["evidence"] = {"signature": "quota", "excerpt": excerpt}
    row["provenance"]["requested"]["adapter"] = adapter
    row["provenance"].update(resolved_model=model, observed_model=None)
    records().write_cooldown(row, path=path, at=at)


def timestamp(value):
    return records().timestamp(value)


def succeed(adapter, model, started_at, path, at):
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="ok")
    if started_at is None:
        row.pop("started_at", None)
    else:
        row["started_at"] = started_at
    row["provenance"]["requested"]["adapter"] = adapter
    row["provenance"].update(resolved_model=model, observed_model=model)
    records().write_cooldown(row, path=path, at=at)
    return records().read_cooldowns(path, at)


def seeded(tmp_path, at):
    path = tmp_path / "cooldowns.json"
    limited("claude", "opus", at, path)
    limited("claude", "sonnet", at, path, "rate_limited", "429")
    limited("codex", "*", at, path)
    limited("codex", "gpt-5.5", at, path, "rate_limited", "429")
    return path


def test_reset_selectors_remove_only_what_they_name(tmp_path):
    at = datetime.now(UTC)
    path = seeded(tmp_path, at)
    claude = sorted(key for key in records().read_cooldowns(path) if key.startswith("claude/"))
    assert len(claude) == 2 and "claude/*" in claude
    model_key = next(key for key in claude if key != "claude/*")
    # The alias is canonicalised the way write_cooldown keyed it.
    assert records().clear_cooldowns("claude/sonnet", path) == [model_key]
    assert records().clear_cooldowns("claude", path) == ["claude/*"]
    assert set(records().read_cooldowns(path)) == {"codex/*", "codex/gpt-5.5"}
    assert records().clear_cooldowns("gemini", path) == []
    assert records().clear_cooldowns(None, path) == ["codex/*", "codex/gpt-5.5"]
    assert records().read_cooldowns(path) == {}
    assert records().clear_cooldowns(None, path) == []


def test_success_after_account_limit_clears_it_for_claude(tmp_path):
    at = datetime(2026, 9, 23, 10, 0, tzinfo=UTC)
    path = tmp_path / "cooldowns.json"
    limited("claude", "*", at, path)
    assert "claude/*" in records().read_cooldowns(path, at)
    later = at + timedelta(minutes=5)
    assert "claude/*" not in succeed("claude", "sonnet", timestamp(later), path, later)


def test_success_started_before_account_limit_keeps_it(tmp_path):
    at = datetime(2026, 9, 23, 10, 0, tzinfo=UTC)
    path = tmp_path / "cooldowns.json"
    limited("claude", "*", at, path)
    later = at + timedelta(minutes=5)
    started = timestamp(at - timedelta(minutes=1))
    assert "claude/*" in succeed("claude", "sonnet", started, path, later)


def test_success_without_start_time_keeps_account_limit(tmp_path):
    at = datetime(2026, 9, 23, 10, 0, tzinfo=UTC)
    path = tmp_path / "cooldowns.json"
    limited("claude", "*", at, path)
    assert "claude/*" in succeed("claude", "sonnet", None, path, at + timedelta(minutes=5))


def test_model_quota_adapter_success_keeps_its_wildcard(tmp_path):
    at = datetime(2026, 9, 23, 10, 0, tzinfo=UTC)
    path = tmp_path / "cooldowns.json"
    limited("agy", "*", at, path, "rate_limited", "429")
    later = at + timedelta(seconds=5)
    assert "agy/*" in succeed("agy", "gemini-3.5-flash", timestamp(later), path, later)


@pytest.mark.parametrize("selector", ["", " ", "--json", "-x", "/sonnet"])
def test_reset_rejects_a_malformed_selector_without_clearing_anything(tmp_path, selector):
    path = seeded(tmp_path, datetime.now(UTC))
    before = records().read_cooldowns(path)
    with pytest.raises(ValueError):
        records().clear_cooldowns(selector, path)
    assert records().read_cooldowns(path) == before


def test_reset_command_prints_removed_keys_and_exits_zero(tmp_path):
    at = datetime.now(UTC)
    path = seeded(tmp_path, at)
    env = {**os.environ, "FABRIC_COOLDOWNS_PATH": str(path)}
    command = [sys.executable, str(Path(__file__).parents[1] / "scripts" / "model_route.py"), "reset"]
    removed = subprocess.run([*command, "codex", "--json"], env=env, capture_output=True, text=True)
    assert removed.returncode == 0
    assert json.loads(removed.stdout)["removed"] == ["codex/*", "codex/gpt-5.5"]
    plain = subprocess.run([*command, "codex"], env=env, capture_output=True, text=True)
    assert plain.returncode == 0
    assert plain.stdout.strip() == "no cooldowns matched"
    assert "claude/*" in records().read_cooldowns(path)


@pytest.mark.parametrize("status", ["rate_limited", "usage_limited"])
def test_limit_outcomes_never_become_empty_output_failures(tmp_path, monkeypatch, status):
    from scripts import model_route
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status=status, task_class="mechanical")
    row["evidence"]["signature"] = "empty_output"
    row["provenance"]["requested"]["adapter"] = "codex"
    row["provenance"]["resolved_model"] = "gpt-6.1-sol"
    path = tmp_path / "route-health.json"
    at = datetime.now(UTC)
    records().write_route_health(row, path=path, at=at)
    health = records().read_route_health(path, cooldowns_path=tmp_path / "cooldowns.json", at=at)
    item = next(iter(health.values()))
    assert item["recent"][0]["status"] == status
    assert item["recent_failures"] == 0
    assert item["rate_limits"] == 1
    assert not model_route._recent_failed_task_class_models("codex", "mechanical")


@pytest.mark.parametrize('adapter,switch', [
    (adapter, switch) for adapter in ('claude', 'codex', 'agy', 'cursor', 'opencode', 'kiro')
    for switch in ('account', 'host') if adapter != 'agy' or switch == 'host'
])
def test_account_and_host_switches_preserve_cooldowns_and_health(tmp_path, monkeypatch, adapter, switch):
    from scripts import model_route
    from skills.orchestrate.scripts import exec_routing

    resolver = records().identity_resolver()
    first = resolver.selected_identity(adapter)
    second = tmp_path / 'alternate'
    config = tmp_path / 'instance/config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {adapter: {
        'a': {'identity_dir': str(first)}, 'b': {'identity_dir': str(second)}}}}))
    path = tmp_path / 'cooldowns.json'
    health = tmp_path / 'route-health.json'
    monkeypatch.setenv('FABRIC_COOLDOWNS_PATH', str(path))
    monkeypatch.setenv('PROVENANT_QUOTA', '0')
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='rate_limited')
    row['provenance']['requested']['adapter'] = adapter
    row['provenance']['resolved_model'] = 'test-model'
    records().write_cooldown(row, path=path)
    records().write_route_health(row, path=health)
    original = records().read_cooldowns(path)
    assert next(iter(original.values()))['account'] == 'a'
    assert exec_routing.cooling(adapter, 'test-model')
    assert model_route._cooling(adapter, 'test-model', model_route._cooldowns({}), {})
    if switch == 'host':
        monkeypatch.setattr(resolver, 'host_id', lambda: 'host-b')
    elif adapter in ('claude', 'codex'):
        monkeypatch.setenv({'claude': 'CLAUDE_CONFIG_DIR', 'codex': 'CODEX_HOME'}[adapter], str(second))
    else:
        # These CLIs use their platform/default directory; alternate HOME selects it.
        monkeypatch.setenv('HOME', str(tmp_path / 'other-home'))
        second = resolver.selected_identity(adapter)
        config.write_text(json.dumps({'accounts': {adapter: {
            'a': {'identity_dir': str(first)}, 'b': {'identity_dir': str(second)}}}}))
    assert records().read_cooldowns(path) == {}
    assert exec_routing.cooling(adapter, 'test-model') is None
    assert not model_route._cooling(adapter, 'test-model', model_route._cooldowns({}), {})
    assert not next(iter(records().read_route_health(health, cooldowns_path=path).values()))['cooling_until']
    # Success in B must leave A's limit intact; a new B limit must coexist with it.
    row.update(status='ok')
    records().write_cooldown(row, path=path)
    row.update(status='rate_limited')
    records().write_cooldown(row, path=path)
    assert records().read_cooldowns(path)
    assert len(json.loads(path.read_text())['cooldowns']) == 2
    monkeypatch.setattr(resolver, 'host_id', lambda: 'host-a')
    monkeypatch.setenv('HOME', str(tmp_path / 'home'))
    for name in ('CLAUDE_CONFIG_DIR', 'CODEX_HOME'):
        monkeypatch.delenv(name, raising=False)
    assert records().read_cooldowns(path) == original
    assert exec_routing.cooling(adapter, 'test-model')
    assert model_route._cooling(adapter, 'test-model', model_route._cooldowns({}), {})


def test_legacy_null_scope_migrates_once_to_default_identity_not_selected_alternate(tmp_path, monkeypatch):
    resolver = records().identity_resolver()
    config = tmp_path / 'instance/config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {'main': {'identity_dir': '~/.claude'}}}}))
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'alternate'))
    path = tmp_path / 'cooldowns.json'
    until = timestamp(datetime.now(UTC) + timedelta(hours=1))
    path.write_text(json.dumps({'schema': 'fabric.cooldowns.v1', 'cooldowns': {
        'claude/*': {'account': None, 'cooling_until': until}}}))
    assert records().read_cooldowns(path) == {}
    entry, = json.loads(path.read_text())['cooldowns'].values()
    assert (entry['account'], entry['host']) == ('main', 'host-a')
    migrated = path.read_bytes()
    assert (tmp_path / 'cooldowns.lock').exists()
    monkeypatch.delenv('CLAUDE_CONFIG_DIR')
    assert records().read_cooldowns(path)['claude/*']['account'] == 'main'
    assert path.read_bytes() == migrated
    monkeypatch.setattr(resolver, 'host_id', lambda: 'host-b')
    assert records().read_cooldowns(path) == {}
    assert path.read_bytes() == migrated


def test_new_unknown_scope_only_matches_unknown_and_never_migrates(tmp_path, monkeypatch):
    resolver = records().identity_resolver()
    monkeypatch.setattr(resolver, 'selected_identity', lambda adapter: None)
    path = tmp_path / 'cooldowns.json'
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='rate_limited', account=None, host='host-a')
    row['provenance']['requested']['adapter'] = 'claude'
    records().write_cooldown(row, path=path)
    assert records().read_cooldowns(path)
    before = path.read_bytes()
    monkeypatch.setattr(resolver, 'selected_identity', resolver.default_identity)
    assert records().read_cooldowns(path) == {}
    assert path.read_bytes() == before


def test_reset_account_cli_keeps_other_accounts_and_hosts(tmp_path, monkeypatch):
    resolver = records().identity_resolver()
    path = tmp_path / 'cooldowns.json'
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='rate_limited')
    row['provenance']['requested']['adapter'] = 'codex'
    row['provenance']['resolved_model'] = 'gpt-6.1-sol'
    for account, host in [('a', 'host-a'), ('b', 'host-a'), ('a', 'host-b')]:
        row.update(account=account, host=host)
        records().write_cooldown(row, path=path)
    hosts = tmp_path / 'instance/.agent-fabric/hosts.json'
    hosts.parent.mkdir(parents=True)
    hosts.write_text(json.dumps({'schema_version': 1, 'local_host': 'host-a'}))
    command = [sys.executable, str(Path(__file__).parents[1] / 'scripts/model_route.py'), 'reset',
               'codex/sol', '--account', 'a', '--json']
    run = subprocess.run(command, env={**os.environ, 'FABRIC_COOLDOWNS_PATH': str(path)}, capture_output=True, text=True)
    assert run.returncode == 0, run.stderr
    assert json.loads(run.stdout)['removed'] == ['codex/gpt-6.1-sol']
    assert {(row['account'], row['host']) for row in json.loads(path.read_text())['cooldowns'].values()} == {
        ('b', 'host-a'), ('a', 'host-b')}
    assert records().clear_cooldowns('codex', path) == ['codex/gpt-6.1-sol']
    assert not json.loads(path.read_text())['cooldowns']


def test_rate_limit_backoff_survives_expiry_is_scoped_and_resets_on_success(tmp_path, monkeypatch):
    path = tmp_path / "cooldowns.json"
    at = datetime.now(UTC)
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="rate_limited", reset_at=None, retry_after=None, account="a", host="host-a")
    row["provenance"]["requested"]["adapter"] = "codex"
    row["provenance"]["resolved_model"] = "gpt-6.1-sol"
    row["provenance"]["observed_model"] = None
    for delay in (30, 60, 120, 240, 480, 900, 900):
        records().write_cooldown(row, path=path, at=at)
        entries = list(json.loads(path.read_text())["cooldowns"].values())
        entry = next(entry for entry in entries if entry["account"] == "a" and entry["host"] == "host-a")
        assert (records().parse_time(entry["cooling_until"]) - at).total_seconds() == delay
        at += timedelta(seconds=delay + 1)
        assert not records().read_cooldowns(path, at)
    for account, host, model in (("b", "host-a", "gpt-6.1-sol"), ("a", "host-b", "gpt-6.1-sol"),
                                 ("a", "host-a", "gpt-6-astra")):
        other = {**row, "account": account, "host": host,
                 "provenance": {**row["provenance"], "resolved_model": model}}
        records().write_cooldown(other, path=path, at=at)
        entry = next(entry for entry in json.loads(path.read_text())["cooldowns"].values()
                     if (entry["account"], entry["host"], entry["model"]) == (account, host, model))
        assert (records().parse_time(entry["cooling_until"]) - at).total_seconds() == 30
    records().write_cooldown({**row, "status": "ok"}, path=path, at=at)
    records().write_cooldown(row, path=path, at=at)
    entry = next(entry for entry in json.loads(path.read_text())["cooldowns"].values()
                 if (entry["account"], entry["host"], entry["model"]) == ("a", "host-a", "gpt-6.1-sol"))
    assert (records().parse_time(entry["cooling_until"]) - at).total_seconds() == 30


def test_cooldown_read_without_lock_preserves_file_and_live_limit(tmp_path, monkeypatch):
    path = tmp_path / "cooldowns.json"
    at = datetime.now(UTC)
    entry = {"adapter": "codex", "model": "gpt-6.1-sol", "status": "usage_limited",
             "cooling_until": records().timestamp(at + timedelta(hours=1))}
    path.write_text(json.dumps({"cooldowns": {"codex/gpt-6.1-sol": entry}}))
    before = path.read_bytes()
    original = records().store_open
    def read_only(path, flags, create_parents=False):
        if flags & os.O_CREAT:
            raise PermissionError("read-only state")
        return original(path, flags, create_parents)
    monkeypatch.setattr(records(), "store_open", read_only)
    assert "codex/gpt-6.1-sol" in records().read_cooldowns(path, at)
    assert path.read_bytes() == before
    assert not path.with_name("cooldowns.lock").exists()


def test_invalid_account_config_warns_once(tmp_path, monkeypatch, capsys):
    resolver = records().identity_resolver()
    monkeypatch.setattr(resolver, "_ACCOUNT_WARNING_EMITTED", False, raising=False)
    config = tmp_path / "instance/config/telemetry.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({"accounts": {"codex": []}}))
    assert resolver.scope("codex")["account"] is None
    assert resolver.scope("claude")["account"] is None
    lines = capsys.readouterr().err.splitlines()
    assert len(lines) == 1
    assert "telemetry accounts must map provider" in lines[0]


def test_concurrent_rate_limits_double_under_the_cooldown_lock(tmp_path):
    from concurrent.futures import ThreadPoolExecutor
    path = tmp_path / "cooldowns.json"
    at = datetime.now(UTC)
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="rate_limited", reset_at=None, retry_after=None)
    with records().locked(path.with_name("cooldowns.lock")):
        pass
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda _: records().write_cooldown(row, path=path, at=at), range(4)))
    entry, = json.loads(path.read_text())["cooldowns"].values()
    assert entry["backoff_seconds"] == 240
    assert (records().parse_time(entry["cooling_until"]) - at).total_seconds() == 240


def test_rate_limit_config_and_provider_window_are_both_honoured(tmp_path):
    config = tmp_path / "instance/config/model-routing.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({"rate_limit_pacing": {"base_delay_seconds": 2, "cap_seconds": 3}}))
    path = tmp_path / "cooldowns.json"
    at = datetime.now(UTC)
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="rate_limited", reset_at=None, retry_after=0.1)
    records().write_cooldown(row, path=path, at=at)
    entry, = records().read_cooldowns(path, at).values()
    assert (records().parse_time(entry["cooling_until"]) - at).total_seconds() == 2
    row["reset_at"] = records().timestamp(at + timedelta(seconds=20))
    records().write_cooldown(row, path=path, at=at)
    entry, = records().read_cooldowns(path, at).values()
    assert entry["backoff_seconds"] == 3
    assert (records().parse_time(entry["cooling_until"]) - at).total_seconds() == 20
