import importlib
import json
from pathlib import Path
from datetime import UTC, datetime, timedelta
import pytest

FIX = Path(__file__).parent / "fixtures/fabric-v1"


def records():
    return importlib.import_module("skills.orchestrate.scripts.fabric_records")


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
    data = json.loads(path.read_text())
    assert data["cooldowns"]["claude/claude-opus-5-5"]["cooling_until"] == "2026-09-23T01:00:00Z"
    row["provenance"]["resolved_model"] = "sonnet"
    row.update(status="rate_limited", retry_after=30)
    module.write_cooldown(row, path=path, at=at + timedelta(hours=2))
    assert set(json.loads(path.read_text())["cooldowns"]) == {"claude/sonnet"}
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
    first = json.loads(path.read_text())['cooldowns']
    assert 'codex/gpt-6-sol' in first
    row['run_id'] = 'synthetic'
    row['evidence']['signature'] = 'cooldown_active'
    records().write_cooldown(row, path=path)
    assert json.loads(path.read_text())['cooldowns'] == first


def test_account_usage_limit_cools_all_models(tmp_path):
    row = json.loads((FIX / 'attempt.json').read_text())
    row.update(status='usage_limited')
    row['provenance']['requested']['adapter'] = 'claude'
    row['evidence'].update(signature='usage_limited', excerpt="You've hit your weekly limit")
    path = tmp_path / 'cooldowns.json'
    records().write_cooldown(row, path=path)
    assert 'claude/*' in json.loads(path.read_text())['cooldowns']


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
    assert set(json.loads(path.read_text())['cooldowns']) == {'cursor/grok-4.7'}


def test_cooldown_path_uses_state_root_between_override_and_default(tmp_path, monkeypatch):
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path))
    monkeypatch.delenv('FABRIC_COOLDOWNS_PATH', raising=False)
    assert records().cooldown_path() == tmp_path / 'cooldowns.json'
    override = tmp_path / 'explicit.json'
    monkeypatch.setenv('FABRIC_COOLDOWNS_PATH', str(override))
    assert records().cooldown_path() == override


def test_malformed_catalogue_keeps_cooldown_identity_and_warns(tmp_path, monkeypatch):
    from skills.orchestrate.scripts import exec_routing
    monkeypatch.setattr(exec_routing, 'snapshot', lambda: {'adapters': {'codex': {'models': [{}]}}})
    row = json.loads((FIX / 'attempt.json').read_text())
    row['status'] = 'rate_limited'
    row['provenance']['requested']['adapter'] = 'codex'
    row['provenance']['resolved_model'] = 'raw-model'
    path = tmp_path / 'cooldowns.json'
    records().write_cooldown(row, path=path)
    assert 'codex/raw-model' in json.loads(path.read_text())['cooldowns']
    assert any('catalogue' in warning for warning in row['warnings'])


def test_digest_surfaces_warnings_once_without_blocking():
    row = json.loads((FIX / "attempt.json").read_text())
    row["warnings"] = ["ultra unknown; ran at medium", "ultra unknown; ran at medium"]
    text = records().render_digest(row)
    assert text.startswith("ok ")
    assert text.endswith("\n  ! ultra unknown; ran at medium")


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
    assert set(json.loads(path.read_text())["cooldowns"]) == {"agy/claude-sonnet-4-6"}
    row["provenance"]["requested"]["adapter"] = "claude"
    row["provenance"]["resolved_model"] = "opus"
    row["evidence"]["excerpt"] = "You've hit your session limit"
    records().write_cooldown(row, path=path, at=at)
    assert "claude/*" in json.loads(path.read_text())["cooldowns"]
