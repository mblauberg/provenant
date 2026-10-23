"""Shared fixtures and builders for provider quota and display tests."""

from datetime import UTC, datetime
import json
from pathlib import Path

import pytest

from tests.support.modules import load_script


ROOT = Path(__file__).resolve().parents[2]
NOW = datetime.now(UTC).replace(microsecond=0)


@pytest.fixture
def quota(tmp_path, monkeypatch, request):
    home = tmp_path / 'home'
    home.mkdir()
    monkeypatch.setenv('HOME', str(home))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(home / '.agents'))
    for name in ('PROVENANT_QUOTA', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME', 'XDG_DATA_HOME'):
        monkeypatch.delenv(name, raising=False)
    module_name = {'test_provider_quota.py': 'provider_quota_under_test',
                   'test_quota_display.py': 'provider_quota_display_under_test'}[request.path.name]
    module = load_script(ROOT / 'scripts/provider_quota.py', module_name)

    def refuse(*args, **kwargs):
        raise AssertionError('tests never reach a real provider, CLI or credential store')
    for name in ('run_process', 'open_process', 'urlopen', 'executable'):
        monkeypatch.setattr(module, name, refuse)
    monkeypatch.setattr(module, 'telemetry_host', lambda: 'studio')
    return module


def configure(tmp_path, value):
    path = tmp_path / 'home/.agents/config/telemetry.json'
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


def window(used, minutes, reset_in=None, name='seven_day', now=NOW, **extra):
    return {'name': name, 'raw_kind': name, 'used_percent': used, 'window_minutes': minutes,
            'reset_at': (now + reset_in).isoformat().replace('+00:00', 'Z') if reset_in is not None else None,
            **extra}


def snapshot(quota, provider='codex', windows=(), observed=NOW, account='default', host='studio', **extra):
    return {'schema': quota.SCHEMA, 'provider': provider, 'account': account, 'plan': None, 'host': host,
            'active': True, 'priority': None, 'observed_at': observed.isoformat().replace('+00:00', 'Z'),
            'attempted_at': observed.isoformat().replace('+00:00', 'Z'), 'source': 'test', 'status': 'ok',
            'error': None, 'unreported': [], 'credits': None,
            'windows': [quota.normalise_window(row) for row in windows], **extra}
