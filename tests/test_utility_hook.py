"""Oversized Bash output is optional, private and fail open."""
import json
import subprocess
from pathlib import Path


def test_hook_off_by_default_then_saves_and_replaces_bash_output(tmp_path, monkeypatch):
    import utility_output_hook as hook
    monkeypatch.chdir(tmp_path)
    subprocess.run(['git', 'init', '-q', str(tmp_path)], check=True)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    payload = {'hook_event_name': 'PostToolUse', 'tool_name': 'Bash', 'cwd': str(tmp_path),
               'tool_input': {'command': 'gh run view 123 --log'},
               'tool_response': {'stdout': 'build failed\n' * 2000, 'stderr': '', 'interrupted': False}}
    calls = []
    def digest(verb, source, **kwargs):
        calls.append(kwargs)
        return {'answer': 'Build failed.', 'lines': [1]}
    assert hook.handle(payload, digest=digest) is None
    monkeypatch.setenv('PROVENANT_KNOB_UTILITY_DIGEST_TOOL_OUTPUT', 'true')
    result = hook.handle(payload, digest=digest)['hookSpecificOutput']
    assert calls[0]['confidential'] is True
    assert result['hookEventName'] == 'PostToolUse'
    assert 'Build failed.' in result['additionalContext']
    replacement = result['updatedToolOutput']
    assert replacement['interrupted'] is False and len(replacement['stdout']) < 1000
    saved = list((tmp_path / '.agent-run/runs').glob('*/output.txt'))
    assert len(saved) == 1 and saved[0].read_text() == payload['tool_response']['stdout']
    assert str(saved[0]) in replacement['stdout']
    assert saved[0].stat().st_mode & 0o777 == 0o600


def test_hook_errors_and_protected_sources_leave_original_untouched(tmp_path, monkeypatch):
    import utility_output_hook as hook
    monkeypatch.chdir(tmp_path)
    subprocess.run(['git', 'init', '-q', str(tmp_path)], check=True)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setenv('PROVENANT_KNOB_UTILITY_DIGEST_TOOL_OUTPUT', 'true')
    payload = {'hook_event_name': 'PostToolUse', 'tool_name': 'Bash', 'cwd': str(tmp_path),
               'tool_input': {'command': 'cat large.log'}, 'tool_response': {'stdout': 'x' * 30000, 'stderr': ''}}
    (tmp_path / 'large.log').write_text(payload['tool_response']['stdout'])
    original = json.dumps(payload)
    def broken(*args, **kwargs):
        raise RuntimeError('provider unavailable')
    assert hook.handle(payload, digest=broken) is None
    def never(*args, **kwargs):
        raise AssertionError('protected output reached utility')
    private = tmp_path / '.agents/fabric-policy.json'
    private.parent.mkdir()
    private.write_text(json.dumps({'protected_paths': ['private']}))
    payload['tool_input']['command'] = 'cat private/evidence.txt'
    assert hook.handle(payload, digest=never) is None
    payload['tool_input']['command'] = 'cat large.log'
    assert json.dumps(payload) == original
    # Unknown provenance in a project with protected paths is conservatively skipped too.
    assert hook.handle(payload, digest=never) is None


def test_hook_skips_opaque_commands_and_external_protected_sources(tmp_path, monkeypatch):
    import utility_output_hook as hook
    monkeypatch.chdir(tmp_path)
    subprocess.run(['git', 'init', '-q', str(tmp_path)], check=True)
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setenv('PROVENANT_KNOB_UTILITY_DIGEST_TOOL_OUTPUT', 'true')
    other = tmp_path / 'other'
    other.mkdir()
    subprocess.run(['git', 'init', '-q', str(other)], check=True)
    policy = other / '.agents/fabric-policy.json'
    policy.parent.mkdir()
    policy.write_text(json.dumps({'protected_paths': ['private']}))
    source = other / 'private/evidence.txt'
    source.parent.mkdir()
    source.write_text('private data')
    payload = {'hook_event_name': 'PostToolUse', 'tool_name': 'Bash', 'cwd': str(tmp_path),
               'tool_response': {'stdout': 'x' * 30000, 'stderr': ''}}
    called = []
    def digest(*args, **kwargs):
        called.append(True)
        return {'answer': 'should have skipped', 'lines': [1]}
    for command in ['python3 -c \'print(open(".env").read() * 1000)\'',
                    f'cat {source}', 'sh -c "cat .env"', 'pytest', 'cat safe.log; cat .env']:
        payload['tool_input'] = {'command': command}
        assert hook.handle(payload, digest=digest) is None
    assert called == []
