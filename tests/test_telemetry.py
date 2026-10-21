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


def test_stats_since_json_tokens_unknown_cost_and_daily_limits(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/session.jsonl'
    def row(mid, stamp):
        return {'type': 'assistant', 'sessionId': 's', 'timestamp': stamp,
                'message': {'id': mid, 'model': 'test-model', 'usage': {
                    'input_tokens': 10, 'cache_read_input_tokens': 30,
                    'cache_creation_input_tokens': 10, 'output_tokens': 20,
                    'output_tokens_details': {'thinking_tokens': 5}}}}
    jsonl(path, row('old', '2026-10-20T01:00:00Z'), row('new', '2026-10-21T01:00:00Z'))
    state = tmp_path / 'state'
    write_json(state / 'cooldowns.json', {'cooldowns': {'opencode/free': {
        'adapter': 'opencode', 'model': 'free', 'status': 'rate_limited',
        'recorded_at': '2026-10-21T01:00:00Z', 'source_run': 'r'}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    result = cli(tmp_path, 'stats', '--since', '2026-10-21', '--json')
    assert result.returncode == 0, result.stderr
    stats = json.loads(result.stdout)
    usage = stats['usage'][0]
    assert (usage['provider'], usage['model']) == ('anthropic', 'test-model')
    assert {k: usage[k] for k in ('input_tokens','cache_read_tokens','cache_write_tokens','output_tokens','reasoning_tokens')} == {
        'input_tokens': 10, 'cache_read_tokens': 30, 'cache_write_tokens': 10,
        'output_tokens': 15, 'reasoning_tokens': 5}
    assert usage['cache_read_share'] == 0.6
    assert usage['estimated_cost_usd'] is None
    assert usage['missing_cost_rows'] == 1
    assert usage['cost_basis'] == 'api_equivalent_estimate'
    assert stats['limits_per_day'] == [{'day': '2026-10-21', 'provider': 'opencode', 'account': None, 'status': 'rate_limited', 'events': 1}]
    assert len(stats['usage_per_day']) == 1
    assert stats['usage_per_day'][0]['day'] == '2026-10-21'
    assert stats['usage_per_day'][0]['input_tokens'] == 10
    plain = cli(tmp_path, 'stats', '--since', '2026-10-21')
    assert plain.returncode == 0 and 'missing' in plain.stdout


def test_stats_prices_preserve_missingness_and_bill_reasoning_once(tmp_path):
    from scripts import telemetry
    row = {'adapter': 'claude', 'model': 'synthetic', 'input_tokens': 1_000_000,
           'cache_read_tokens': 2_000_000, 'cache_write_tokens': 3_000_000,
           'output_tokens': 4_000_000, 'reasoning_tokens': 5_000_000}
    prices = {'claude/synthetic': {'usd_per_million': {
        'input': 2, 'cached_input': 0.2, 'cache_write': 3, 'output': 10}}}
    assert telemetry.estimated_cost(row, prices) == 101.4
    assert telemetry.estimated_cost({**row, 'cache_write_tokens': None}, prices) is None
    assert telemetry.estimated_cost(row, {'claude/synthetic': {'usd_per_million': {
        'input': 2, 'cached_input': 0.2, 'cache_write': None, 'output': 10}}}) is None
    assert telemetry.estimated_cost({**row, 'cache_write_tokens': 0}, {
        'claude/synthetic': {'usd_per_million': {
            'input': 2, 'cached_input': 0.2, 'cache_write': None, 'output': 10}}}) == 92.4


def test_stats_do_not_double_count_receipts_with_native_session_usage(tmp_path):
    project = tmp_path
    path = project / '.agent-run/runs/run/tasks/lane/attempt-001/attempt.json'
    write_json(path, {'run_id':'r','task_id':'t','attempt':1,'session_id':'s',
                     'status':'rate_limited','error_class':'throttle',
                     'ended_at':'2026-10-21T01:00:00Z',
                     'provenance': {'requested':{'adapter':'claude'}, 'resolved_model':'test'},
                     'context':{'input_tokens':100,'output_tokens':20}})
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type':'assistant','sessionId':'s','timestamp':'2026-10-21T01:00:00Z',
        'message':{'id':'m','model':'test','usage':{'input_tokens':100,'output_tokens':20}}})
    write_json(tmp_path / 'state/route-health.json', {'routes':{'key':{
        'adapter':'claude','model':'test','recent':[{'status':'rate_limited','run_id':'r','at':'2026-10-21T01:00:01Z'}]}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    result = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    assert result['usage'][0]['input_tokens'] == 100
    assert result['receipt_usage_rows_excluded'] == 1
    assert result['lanes'] == [{'status':'rate_limited','error_class':'throttle','lanes':1}]
    assert sum(row['events'] for row in result['limits_per_day']) == 1


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


def test_since_dedup_uses_only_native_usage_in_requested_period(tmp_path):
    write_json(tmp_path / '.agent-run/runs/run/tasks/lane/attempt-001/attempt.json', {
        'run_id':'r','task_id':'t','attempt':1,'session_id':'s','status':'ok',
        'ended_at':'2026-10-21T01:00:00Z',
        'provenance':{'requested':{'adapter':'claude'},'resolved_model':'test'},
        'context':{'input_tokens':100}})
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type':'assistant','sessionId':'s','timestamp':'2026-10-20T01:00:00Z',
        'message':{'id':'m','model':'test','usage':{'input_tokens':7}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--since', '2026-10-21', '--json').stdout)
    assert report['usage'][0]['input_tokens'] == 100
    assert report['receipt_usage_rows_excluded'] == 0


def test_opencode_price_lookup_keeps_provider_namespace():
    from scripts import telemetry
    row = {'adapter':'opencode','provider':'opencode-go','model':'synthetic',
           'input_tokens':1_000_000,'cache_read_tokens':0,'cache_write_tokens':0,
           'output_tokens':0,'reasoning_tokens':0}
    assert telemetry.estimated_cost(row, {'opencode/opencode-go/synthetic': {
        'usd_per_million':{'input':2}}}) == 2


def test_lanes_report_latest_attempt_and_preserve_attempt_outcomes(tmp_path):
    for attempt, status in ((1,'failed'), (2,'ok')):
        write_json(tmp_path / f'.agent-run/runs/run/tasks/lane/attempt-{attempt:03d}/attempt.json', {
            'run_id':'r','task_id':'lane','attempt':attempt,'status':status,
            'error_class':'test_failure' if status == 'failed' else None,
            'ended_at':'2026-10-21T01:00:00Z', 'cwd':'/repo',
            'provenance':{'requested':{'adapter':'codex'},'resolved_model':'test'}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    assert report['lanes'] == [{'status':'ok','error_class':None,'lanes':1}]
    assert report['attempts'] == [{'status':'failed','error_class':'test_failure','attempts':1},
                                  {'status':'ok','error_class':None,'attempts':1}]


def test_unknown_token_fields_are_not_reported_as_zero(tmp_path):
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type':'assistant','timestamp':'2026-10-21T01:00:00Z',
        'message':{'id':'m','model':'test','usage':{'input_tokens':10}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    usage = report['usage'][0]
    assert usage['output_tokens'] is None
    assert usage['cache_read_share'] is None
    assert usage['missing_token_fields']['output_tokens'] == 1
    assert usage['adapter'] == 'claude'


def test_fabric_context_totals_are_normalised_to_disjoint_token_buckets(tmp_path):
    write_json(tmp_path / '.agent-run/runs/run/tasks/lane/attempt-001/attempt.json', {
        'run_id':'r','task_id':'lane','attempt':1,'status':'ok',
        'ended_at':'2026-10-21T01:00:00Z',
        'provenance':{'requested':{'adapter':'codex'},'resolved_model':'test'},
        'context':{'input_tokens':100,'cached_input_tokens':40,'cache_write_tokens':10,
                   'output_tokens':30,'reasoning_tokens':10}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    row = report['usage'][0]
    assert (row['input_tokens'], row['cache_read_tokens'], row['cache_write_tokens'],
            row['output_tokens'], row['reasoning_tokens']) == (50,40,10,20,10)


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


def test_limit_dedup_keeps_distinct_models_in_the_same_run(tmp_path):
    write_json(tmp_path / '.agent-run/runs/run/tasks/lane/attempt-001/attempt.json', {
        'run_id':'r','task_id':'lane','attempt':1,'status':'rate_limited',
        'ended_at':'2026-10-21T01:00:00Z',
        'provenance':{'requested':{'adapter':'codex'},'resolved_model':'one'}})
    write_json(tmp_path / 'state/route-health.json', {'routes':{model:{
        'adapter':'codex','model':model,'recent':[{'run_id':'r','status':'rate_limited',
        'at':'2026-10-21T01:00:00Z'}]} for model in ('one','two')}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    assert sum(row['events'] for row in report['limits_per_day']) == 2


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
