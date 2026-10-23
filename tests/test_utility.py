"""Typed utility contracts at the provider boundary."""
import json
from pathlib import Path

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


def test_cli_plain_code_contracts_and_help(tmp_path, monkeypatch, capsys):
    import os
    import subprocess
    import sys
    root = Path(__file__).resolve().parents[1]
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(root))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    items = tmp_path / 'items.txt'
    items.write_text('yes\nno\n')
    done = subprocess.run([str(front_door_binary()), 'classify', '--labels', 'yes,no', str(items)],
                          capture_output=True, text=True, env=os.environ)
    assert done.returncode == 0, done.stderr
    assert json.loads(done.stdout) == ['yes', 'no']
    for verb in ('digest', 'classify', 'extract'):
        done = subprocess.run([str(front_door_binary()), 'help', verb], capture_output=True, text=True)
        assert done.returncode == 0
        assert done.stdout == (root / f'tools/front-door/src/help/{verb}.txt').read_text()


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
