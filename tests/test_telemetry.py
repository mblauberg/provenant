from pathlib import Path
import json
import os
import sqlite3
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def environment(tmp_path):
    home = tmp_path / 'home'
    home.mkdir(exist_ok=True)
    return {**os.environ, 'HOME': str(home),
            'AGENT_FABRIC_STATE_ROOT': str(tmp_path / 'state'),
            'AGENT_FABRIC_PRODUCT_ROOT': str(ROOT),
            'AGENT_FABRIC_STATE_DIRECTORY': str(tmp_path / 'state'),
            'FABRIC_COOLDOWNS_PATH': str(tmp_path / 'state/cooldowns.json'),
            'AGENT_FABRIC_ROUTE_HEALTH_PATH': str(tmp_path / 'state/route-health.json')}


def cli(tmp_path, *args):
    return subprocess.run([str(ROOT / 'scripts/provenant'), *args],
                          env=environment(tmp_path), cwd=tmp_path,
                          capture_output=True, text=True)


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


def jsonl(path, *rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('a') as stream:
        for row in rows:
            stream.write(json.dumps(row) + '\n')


def test_missing_sources_are_manifested_not_errors(tmp_path):
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 0, result.stderr
    result = cli(tmp_path, 'telemetry', 'sources', '--json')
    assert result.returncode == 0, result.stderr
    rows = json.loads(result.stdout)
    assert {row['source'] for row in rows} == {
        'fabric', 'route_health', 'cooldowns', 'claude', 'codex', 'opencode'}
    assert all(row['status'] == 'absent' and row['row_count'] == 0 for row in rows)
    assert all(row['parser_version'] == 2 and row['path_pattern'] for row in rows)
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('PRAGMA user_version').fetchone()[0] == 3


def test_claude_incremental_dedup_subagents_and_content_exclusion(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/session.jsonl'
    row = {'type': 'assistant', 'sessionId': 's1', 'cwd': '/repo',
           'timestamp': '2026-10-21T01:00:00Z', 'message': {
               'id': 'm1', 'model': 'claude-test', 'content': 'PRIVATE_MESSAGE',
               'usage': {'input_tokens': 10, 'output_tokens': 20,
                         'cache_read_input_tokens': 30, 'cache_creation_input_tokens': 40}}}
    jsonl(path, row)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    jsonl(path, row)
    sub = path.parent / 'session/subagents/agent-1.jsonl'
    jsonl(sub, {**row, 'message': {**row['message'], 'id': 'm2'}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        rows = db.execute('SELECT session,model,input_tokens,cache_read_tokens,cache_write_tokens,output_tokens FROM usage').fetchall()
        assert rows == [('s1', 'claude-test', 10, 30, 40, 20)] * 2
        cursors = db.execute('SELECT offset FROM cursors WHERE path=?', (str(path),)).fetchone()
        assert cursors[0] == path.stat().st_size
        assert 'PRIVATE_MESSAGE' not in '\n'.join(db.iterdump())
    assert 'PRIVATE_MESSAGE' not in (tmp_path / 'state/telemetry.sqlite3').read_bytes().decode(errors='ignore')


def test_codex_cumulative_usage_deltas_and_quota_windows_survive_append(tmp_path):
    path = tmp_path / 'home/.codex/sessions/day/rollout-test.jsonl'
    def event(total, percent):
        return {'type': 'event_msg', 'timestamp': '2026-10-21T02:00:00Z',
                'payload': {'type': 'token_count', 'info': {'total_token_usage': total},
                            'rate_limits': {'primary': {'used_percent': percent,
                                             'window_minutes': 300, 'resets_at': 1792600000}}}}
    total = {'input_tokens': 100, 'cached_input_tokens': 40,
             'output_tokens': 30, 'reasoning_output_tokens': 10}
    jsonl(path, {'type': 'session_meta', 'payload': {'id': 'cx', 'cwd': '/repo',
                  'creator_account_id': 'acct', 'git': {'commit_hash': 'abc'}}},
          {'type': 'turn_context', 'payload': {'model': 'test-sol', 'effort': 'high'}},
          event(total, 50), event(total, 50))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    jsonl(path, event({**total, 'input_tokens': 150, 'output_tokens': 50}, 60))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT SUM(input_tokens),SUM(cache_read_tokens),SUM(output_tokens),SUM(reasoning_tokens) FROM usage').fetchone() == (110, 40, 40, 10)
        assert db.execute('SELECT DISTINCT session,account,model,effort,revision FROM usage').fetchall() == [('cx', 'acct', 'test-sol', 'high', 'abc')]
        assert db.execute('SELECT used_percent,window_minutes FROM observations ORDER BY used_percent').fetchall() == [(50, 300), (60, 300)]


def test_fabric_projects_receipts_health_cooldowns_are_read_only_and_idempotent(tmp_path):
    state = tmp_path / 'state'
    state.mkdir()
    project = tmp_path / 'project'
    project.mkdir()
    with sqlite3.connect(state / 'fabric.sqlite3') as db:
        db.execute('CREATE TABLE agents(project TEXT)')
        db.execute('INSERT INTO agents VALUES (?)', (str(project),))
    path = project / '.agent-run/runs/20261021-1000-dispatch-test-abc/tasks/lane/attempt-001/attempt.json'
    receipt = {'schema': 'fabric.attempt.v1', 'run_id': 'run-1', 'task_id': 'lane',
               'attempt': 1, 'status': 'rate_limited', 'session_id': 's-f',
               'cwd': '/repo', 'ended_at': '2026-10-21T01:00:00Z',
               'provenance': {'requested': {'adapter': 'opencode', 'model': 'free-model'},
                              'resolved_model': 'free-model', 'effort_applied': 'high'},
               'context': {'input_tokens': 10, 'cached_input_tokens': 4,
                           'output_tokens': 2, 'cache_write_tokens': 3,
                           'reasoning_tokens': 1, 'usage_source': 'observed'},
               'error_class': 'provider_rate_limit', 'evidence': {'excerpt': 'PRIVATE_ERROR'}}
    write_json(path, receipt)
    write_json(state / 'route-health.json', {'schema': 'fabric.route-health.v1', 'routes': {
        'key': {'adapter': 'opencode', 'model': 'free-model', 'recent': [
            {'status': 'rate_limited', 'run_id': 'run-1', 'at': '2026-10-21T01:00:00Z'}]}}})
    write_json(state / 'cooldowns.json', {'cooldowns': {'opencode/free-model': {
        'adapter': 'opencode', 'model': 'free-model', 'account': 'free-account',
        'status': 'rate_limited', 'source_run': 'run-1',
        'recorded_at': '2026-10-21T01:00:00Z', 'cooling_until': '2026-10-21T02:00:00Z'}}})
    before = {p: p.read_bytes() for p in (path, state / 'route-health.json', state / 'cooldowns.json', state / 'fabric.sqlite3')}
    for _ in range(2):
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        assert db.execute('SELECT adapter,model,cache_write_tokens,reasoning_tokens,usage_source,error_class FROM usage').fetchall() == [('opencode', 'free-model', 3, 1, 'observed', 'provider_rate_limit')]
        assert db.execute('SELECT COUNT(*) FROM observations').fetchone()[0] == 2
        assert 'PRIVATE_ERROR' not in '\n'.join(db.iterdump())
    assert before == {p: p.read_bytes() for p in before}
    write_json(path, {**receipt, 'status': 'ok'})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        assert db.execute('SELECT status FROM usage').fetchall() == [('ok',)]


def test_opencode_updated_messages_use_readonly_sqlite_and_cursor(tmp_path):
    path = tmp_path / 'home/.local/share/opencode/opencode.db'
    path.parent.mkdir(parents=True)
    data = {'role': 'assistant', 'providerID': 'vendor', 'modelID': 'test-model',
            'time': {'created': 1792544400000}, 'path': {'cwd': '/repo'},
            'tokens': {'input': 10, 'output': 2, 'reasoning': 3,
                       'cache': {'read': 4, 'write': 5}}, 'cost': 0.7,
            'content': 'PRIVATE_OPENCODE'}
    with sqlite3.connect(path) as db:
        db.execute('CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT)')
        db.execute('INSERT INTO message VALUES (?,?,?,?,?)', ('m1','oc',1000,1000,json.dumps(data)))
    original = path.read_bytes()
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    assert path.read_bytes() == original
    with sqlite3.connect(path) as db:
        db.execute('UPDATE message SET data=?,time_updated=2000', (json.dumps({**data, 'tokens': {**data['tokens'], 'output': 9}}),))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT provider,model,input_tokens,output_tokens,reasoning_tokens,cache_read_tokens,cache_write_tokens,reported_cost FROM usage').fetchall() == [('vendor','test-model',10,9,3,4,5,0.7)]
        assert 'PRIVATE_OPENCODE' not in '\n'.join(db.iterdump())


def test_partial_jsonl_line_resumes_and_malformed_source_is_visible(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/partial.jsonl'
    row = {'type': 'assistant', 'sessionId': 's1', 'timestamp': '2026-10-21T01:00:00Z',
           'message': {'id': 'partial', 'model': 'test', 'usage': {'input_tokens': 7}}}
    path.parent.mkdir(parents=True)
    encoded = json.dumps(row)
    path.write_text(encoded[:20])
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with path.open('a') as stream:
        stream.write(encoded[20:] + '\n')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    jsonl(path, row)
    broken = path.parent / 'broken.jsonl'
    broken.write_text('PRIVATE_MALFORMED\n')
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 1
    assert 'PRIVATE_MALFORMED' not in result.stdout + result.stderr
    sources = json.loads(cli(tmp_path, 'telemetry', 'sources', '--json').stdout)
    assert next(row for row in sources if row['source'] == 'claude')['status'] == 'error'
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT SUM(input_tokens) FROM usage').fetchone()[0] == 7








def test_rewritten_jsonl_reconciles_removed_ids(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/s.jsonl'
    row = {'type':'assistant','sessionId':'s','message':{
        'id':'a','model':'test','usage':{'input_tokens':7}}}
    jsonl(path, row)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    path.write_text(json.dumps({**row, 'message':{**row['message'], 'id':'b'}}) + '\n')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT event_id,input_tokens FROM usage').fetchall() == [('b',7)]


def test_fabric_same_run_ids_across_projects_do_not_overwrite(tmp_path):
    state = tmp_path / 'state'
    state.mkdir()
    with sqlite3.connect(state / 'fabric.sqlite3') as db:
        db.execute('CREATE TABLE agents(project TEXT)')
        for name in ('one','two'):
            project = tmp_path / name
            db.execute('INSERT INTO agents VALUES (?)', (str(project),))
            write_json(project / '.agent-run/runs/run/tasks/lane/attempt-001/attempt.json', {
                'run_id':'run','task_id':'lane','attempt':1,'status':'ok','cwd':str(project),
                'provenance':{'requested':{'adapter':'codex'},'resolved_model':'test'},
                'context':{'input_tokens':7}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        assert db.execute('SELECT SUM(input_tokens),COUNT(*) FROM usage').fetchone() == (14,2)


def test_opencode_replacement_resets_cursor(tmp_path):
    path = tmp_path / 'home/.local/share/opencode/opencode.db'
    path.parent.mkdir(parents=True)
    def database(target, mid, stamp):
        with sqlite3.connect(target) as db:
            db.execute('CREATE TABLE message(id TEXT,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT)')
            db.execute('INSERT INTO message VALUES (?,?,?,?,?)', (mid,'s',stamp,stamp,
                json.dumps({'role':'assistant','tokens':{'input':7}})))
    database(path, 'old', 2000)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    replacement = path.with_suffix('.new')
    database(replacement, 'new', 1000)
    replacement.replace(path)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT event_id,input_tokens FROM usage').fetchall() == [('new',7)]


def test_codex_only_persists_allowlisted_usage_metadata(tmp_path):
    jsonl(tmp_path / 'home/.codex/sessions/day/rollout-test.jsonl',
          {'type':'session_meta','payload':{'id':'s','base_instructions':'PRIVATE_BASE'}},
          {'type':'event_msg','timestamp':'2026-10-21T01:00:00Z','payload':{
              'type':'token_count','info':{'total_token_usage':{
                  'input_tokens':10,'output_tokens':3,'unexpected':'PRIVATE_USAGE'}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert 'PRIVATE_' not in '\n'.join(db.iterdump())


def test_claude_duplicate_partial_usage_does_not_replace_complete_message(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/s.jsonl'
    row = {'type':'assistant','sessionId':'s','message':{
        'id':'m','model':'test','usage':{'input_tokens':10,'output_tokens':30}}}
    jsonl(path, row)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    jsonl(path, {**row,'message':{**row['message'],'usage':{'input_tokens':10,'output_tokens':3}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT output_tokens FROM usage').fetchall() == [(30,)]


def test_schema_migrates_existing_v1_observations(tmp_path):
    path = tmp_path / 'state/telemetry.sqlite3'
    path.parent.mkdir()
    with sqlite3.connect(path) as db:
        db.execute('CREATE TABLE observations(event_id TEXT PRIMARY KEY,source TEXT,host TEXT,provider TEXT,adapter TEXT,model TEXT,account TEXT,session TEXT,lane TEXT,run_id TEXT,timestamp TEXT,window TEXT,used_percent REAL,window_minutes REAL,reset_at TEXT,status TEXT)')
        db.execute('PRAGMA user_version=1')
    write_json(path.parent / 'cooldowns.json', {'cooldowns':{'x':{
        'adapter':'codex','model':'test','status':'usage_limited','source_run':'r',
        'recorded_at':'2026-10-21T01:00:00Z'}}})
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 0, result.stdout + result.stderr
    with sqlite3.connect(path) as db:
        assert db.execute('PRAGMA user_version').fetchone()[0] == 3
        assert db.execute('SELECT source,origin_path FROM observations').fetchone() == ('cooldowns', str(path.parent / 'cooldowns.json'))


def test_large_same_size_transcript_suffix_rewrite_is_reconciled(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/s.jsonl'
    jsonl(path, {'type':'user','content':'x'*5000}, {'type':'assistant','message':{
        'id':'old','model':'test','usage':{'input_tokens':10}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    path.write_text(path.read_text().replace('"old"','"new"'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT event_id FROM usage').fetchall() == [('new',)]












def test_unreadable_fabric_registry_does_not_block_other_sources(tmp_path):
    path = tmp_path / 'state/fabric.sqlite3'
    path.parent.mkdir()
    path.write_bytes(b'PRIVATE_CORRUPT_DB')
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type':'assistant','message':{'id':'m','model':'test','usage':{'input_tokens':7}}})
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 1
    assert 'PRIVATE_CORRUPT_DB' not in result.stdout + result.stderr
    rows = json.loads(cli(tmp_path, 'telemetry', 'sources', '--json').stdout)
    assert next(row for row in rows if row['source'] == 'fabric')['status'] == 'error'
    assert next(row for row in rows if row['source'] == 'claude')['row_count'] == 1




def test_populated_v1_rebuild_reconciles_rewrites_without_losing_other_files(tmp_path):
    directory = tmp_path / 'home/.claude/projects/repo'
    for mid in ('one','two'):
        jsonl(directory / f'{mid}.jsonl', {'type':'assistant','message':{
            'id':mid,'model':'test','usage':{'input_tokens':7}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute('DELETE FROM usage_files')
        db.execute("UPDATE cursors SET state='{}'")
        db.execute('PRAGMA user_version=1')
    path = directory / 'one.jsonl'
    path.write_text(path.read_text().replace('"one"','"new"'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT event_id FROM usage ORDER BY event_id').fetchall() == [('new',),('two',)]
    path.write_text(path.read_text().replace('"new"','"now"'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT event_id FROM usage ORDER BY event_id').fetchall() == [('now',),('two',)]


def test_legacy_rebuild_replaces_stale_usage_for_unchanged_message_id(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/s.jsonl'
    row = {'type':'assistant','message':{
        'id':'m','model':'test','usage':{'input_tokens':10,'output_tokens':30}}}
    jsonl(path, row)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute('DELETE FROM usage_files')
        db.execute("UPDATE cursors SET state='{}'")
        db.execute('PRAGMA user_version=1')
    row['message']['usage']['output_tokens'] = 3
    path.write_text(json.dumps(row) + '\n')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT output_tokens FROM usage').fetchall() == [(3,)]
