"""Typed utility contracts at the provider boundary."""
import json
from pathlib import Path
import sqlite3

import pytest


from tests.support.front_door import front_door_binary

def test_utility_retries_then_escalates_without_downgrading_privacy(tmp_path, monkeypatch):
    import utility
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    calls = []
    def provider(prompt, route, confidential):
        calls.append((route, confidential))
        return ('["bad"]' if len(calls) < 3 else '["yes", "no"]', {'route': route})
    assert utility.execute('classify', 'one\ntwo', labels=['yes', 'no'], provider=provider) == ['yes', 'no']
    assert calls == [('utility', True), ('utility', True), ('junior', True)]
    db = sqlite3.connect(tmp_path / 'state/telemetry.sqlite3')
    attrs = [json.loads(row[0]) for row in db.execute("select attrs from events where kind='utility.call'")]
    assert len(attrs) == 3
    assert [row['escalated'] for row in attrs] == [False, False, True]
    assert all(row['verb'] == 'classify' and row['latency_ms'] >= 0 for row in attrs)


def test_utility_plain_code_and_schema_validation(tmp_path, monkeypatch):
    import utility
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    def never(*args):
        pytest.fail('deterministic input must not call a model')
    assert utility.execute('classify', '["YES", "no"]', labels=['yes', 'no'], provider=never) == ['yes', 'no']
    schema = {'type': 'object', 'required': ['count'], 'properties': {'count': {'type': 'integer', 'minimum': 0}}, 'additionalProperties': False}
    assert utility.execute('extract', '{"count": 3}', schema=schema, provider=never) == {'count': 3}
    calls = []
    def provider(prompt, route, confidential):
        calls.append(route)
        return ('{"count": -1}' if len(calls) < 3 else '{"count": 2}', {})
    assert utility.execute('extract', 'two items', schema=schema, provider=provider) == {'count': 2}
    assert calls == ['utility', 'utility', 'junior']
    with pytest.raises(ValueError):
        utility.execute('extract', '{}', schema={'type': 'made-up'}, provider=never)


def test_digest_validates_line_pointers_and_bounds_the_answer(tmp_path, monkeypatch):
    import utility
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    calls = []
    def provider(prompt, route, confidential):
        calls.append(route)
        return (json.dumps({'answer': 'Found it', 'lines': [99] if len(calls) == 1 else [2]}), {})
    assert utility.execute('digest', 'first\nsecond', question='Where?', provider=provider) == {'answer': 'Found it', 'lines': [2]}
    assert calls == ['utility', 'utility']


def test_cli_help_matches_frozen_text(tmp_path, monkeypatch):
    import subprocess
    root = Path(__file__).resolve().parents[1]
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(root))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    for verb in ('digest', 'classify', 'extract'):
        done = subprocess.run([str(front_door_binary()), 'help', verb], capture_output=True, text=True)
        assert done.returncode == 0
        assert done.stdout == (root / f'tools/front-door/src/help/{verb}.txt').read_text()


@pytest.mark.parametrize('timeout_seconds', [None, 20])
def test_provider_path_uses_router_and_supervisor_and_records_usage(tmp_path, monkeypatch, timeout_seconds):
    import os
    import utility
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    cli = bin_dir / 'claude'
    cli.write_text('''#!/usr/bin/env python3
import json, sys
if '--version' in sys.argv:
    print('2.1.236'); raise SystemExit(0)
if '--help' in sys.argv:
    print('--model --effort --safe-mode --bare'); raise SystemExit(0)
sys.stdin.read()
print(json.dumps({'type':'system','subtype':'init','model':'claude-haiku-5-5','session_id':'utility-test'}))
print(json.dumps({'type':'result','subtype':'success','result':'["yes"]','is_error':False,
                  'usage': {'input_tokens': 10, 'output_tokens': 2}, 'total_cost_usd': 0.001}))
''')
    cli.chmod(0o755)
    monkeypatch.setenv('PATH', str(bin_dir) + os.pathsep + os.environ['PATH'])
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(Path(__file__).resolve().parents[1]))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.setenv('ANTHROPIC_API_KEY', 'synthetic-test-key')
    monkeypatch.setenv('PROVENANT_NO_OS_CONFINEMENT', '1')
    monkeypatch.chdir(tmp_path)
    import subprocess
    subprocess.run(['git', 'init', '-q', str(tmp_path)], check=True)
    assert utility.execute('classify', 'some item', labels=['yes', 'no'], timeout_seconds=timeout_seconds) == ['yes']
    db = sqlite3.connect(tmp_path / 'state/telemetry.sqlite3')
    model, attributes = db.execute("select model, attrs from events where kind='utility.call'").fetchone()
    assert model == 'claude/claude-haiku-5-5'
    attrs = json.loads(attributes)
    assert attrs['route'] == 'utility' and attrs['model'] == 'claude/claude-haiku-5-5'
    assert attrs['tokens']['input_tokens'] == 10 and attrs['cost_usd'] == 0.001


def test_cli_refuses_protected_input_from_another_repository(tmp_path, monkeypatch):
    import subprocess
    import utility
    caller, other = tmp_path / 'caller', tmp_path / 'other'
    for root in (caller, other):
        root.mkdir()
        subprocess.run(['git', 'init', '-q', str(root)], check=True)
    policy = other / '.agents/fabric-policy.json'
    policy.parent.mkdir()
    policy.write_text(json.dumps({'protected_paths': ['private']}))
    source = other / 'private/evidence.txt'
    source.parent.mkdir()
    source.write_text('protected evidence')
    monkeypatch.chdir(caller)
    with pytest.raises(ValueError, match='protected'):
        utility.read_input(source)


def test_single_attempt_does_not_retry_or_escalate(tmp_path, monkeypatch):
    import utility
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    calls = []
    def provider(prompt, route, confidential):
        calls.append(route)
        return ('invalid JSON', {})
    with pytest.raises(ValueError):
        utility.execute('digest', 'some output', provider=provider, retry=False)
    assert calls == ['utility']


@pytest.mark.parametrize('failure', [sqlite3.OperationalError, RuntimeError])
def test_telemetry_failure_does_not_fail_the_call(monkeypatch, failure):
    import utility
    def broken():
        raise failure('telemetry unavailable')
    monkeypatch.setattr(utility.telemetry, 'connect', broken)
    assert utility.execute('classify', 'yes', labels=['yes', 'no']) == ['yes']
    def provider(prompt, route, confidential):
        contract = json.loads(prompt.split('\n', 1)[1])
        assert contract['items'] == ['some input'] and 'source' not in contract
        return ('["yes"]', {})
    assert utility.execute('classify', 'some input', labels=['yes', 'no'], provider=provider) == ['yes']
