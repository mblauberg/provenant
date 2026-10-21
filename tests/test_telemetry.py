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
            'AGENT_FABRIC_INSTANCE_ROOT': str(home / '.agents'),
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
        'fabric', 'route_health', 'cooldowns', 'claude', 'codex', 'opencode', 'otel'}
    assert all(row['status'] == 'absent' and row['row_count'] == 0 for row in rows)
    assert all(row['parser_version'] == 2 and row['path_pattern'] for row in rows)
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("PRAGMA user_version").fetchone()[0] == 4


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
        assert db.execute("PRAGMA user_version").fetchone()[0] == 4
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


def test_lost_attempt_is_reported_as_a_provider_error(tmp_path):
    write_json(tmp_path / '.agent-run/runs/run/tasks/lane/attempt-001/attempt.json', {
        'run_id':'r','task_id':'lane','attempt':1,'state':'terminal','status':'lost',
        'error_class':'provider_error','ended_at':'2026-10-21T01:00:00Z','cwd':'/repo',
        'provenance':{'requested':{'adapter':'codex'},'resolved_model':'test'}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    assert report['lanes'] == [{'status':'lost','error_class':'provider_error','lanes':1}]
    assert report['attempts'] == [{'status':'lost','error_class':'provider_error','attempts':1}]


def test_cancelled_error_class_survives_ingestion_and_reporting(tmp_path):
    write_json(tmp_path / '.agent-run/runs/run/tasks/lane/attempt-001/attempt.json', {
        'run_id':'r','task_id':'lane','attempt':1,'state':'terminal','status':'cancelled',
        'error_class':'cancelled','ended_at':'2026-10-21T01:00:00Z','cwd':'/repo',
        'provenance':{'requested':{'adapter':'codex'},'resolved_model':'test'}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    result = cli(tmp_path, 'stats', '--json')
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report['lanes'] == [{'status':'cancelled','error_class':'cancelled','lanes':1}]
    assert report['attempts'] == [{'status':'cancelled','error_class':'cancelled','attempts':1}]


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


def receipt(project, name, attempt=1, **values):
    path = project / f'.agent-run/runs/{name}/tasks/lane/attempt-{attempt:03d}/attempt.json'
    write_json(path, {'run_id': name, 'task_id': 'lane', 'attempt': attempt, 'cwd': '/repo',
                      'ended_at': '2026-10-21T01:00:00Z', **values})
    return path


def route(adapter, model, effort=None):
    return {'requested': {'adapter': adapter, 'model': model, 'effort': effort or ''},
            'resolved_model': model, 'effort_applied': effort or ''}


def test_stats_normalise_models_against_the_catalogue_and_keep_raw_values(tmp_path):
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
        'message': {'id': 'm', 'model': 'claude-haiku-4-5-20251001', 'usage': {'input_tokens': 7}}})
    receipt(tmp_path, 'agy', status='ok', provenance=route('agy', 'gemini-3.8-flash-high', 'high'))
    receipt(tmp_path, 'cursor', status='ok', provenance=route('cursor', 'Grok 4.7 256K High'))
    receipt(tmp_path, 'claude', status='ok', provenance=route('claude', 'opus'))
    receipt(tmp_path, 'sonnet5', status='ok', provenance=route('claude', 'claude-sonnet-5'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    models = {(row['adapter'], row['model']): row['model_raw'] for row in report['usage']}
    assert models == {('claude', 'claude-haiku-4-5'): ['claude-haiku-4-5-20251001'],
                      ('agy', 'gemini-3.8-flash'): ['gemini-3.8-flash-high'],
                      ('cursor', 'grok-4.7'): ['Grok 4.7 256K High'],
                      ('claude', 'claude-opus-5-5'): ['opus'],
                      # A versioned spelling is a distinct model, never folded onto a newer id.
                      ('claude', 'claude-sonnet-5'): ['claude-sonnet-5']}


def test_opencode_native_and_receipt_spellings_share_one_model_key(tmp_path):
    from scripts import telemetry
    catalogue = telemetry.catalogue()
    assert telemetry.normalise_model('opencode', 'opencode-go', 'deepseek-v4.1-flash', catalogue) == 'opencode-go/deepseek-v4.1-flash'
    assert telemetry.normalise_model('opencode', 'opencode', 'opencode-go/deepseek-v4.1-flash', catalogue) == 'opencode-go/deepseek-v4.1-flash'
    assert telemetry.normalise_model('opencode', 'openrouter', 'z-ai/glm-5.2:free', catalogue) == 'openrouter/z-ai/glm-5.2:free'
    assert telemetry.normalise_model('codex', 'openai', 'gpt-6-sol', catalogue) == 'gpt-6-sol'
    assert telemetry.normalise_model('codex', 'openai', None, catalogue) is None


def test_fixture_rows_are_excluded_and_unattributed_rows_reported_separately(tmp_path):
    receipt(tmp_path, 'fixture', status='ok', provenance=route('codex', 'fixture'), context={'input_tokens': 5})
    receipt(tmp_path, 'missing', status='model_unavailable', provenance=route('opencode', 'opencode/nonexistent-model'))
    receipt(tmp_path, 'bare', status='cancelled', context={'input_tokens': 3})
    receipt(tmp_path, 'real', status='ok', provenance=route('codex', 'gpt-6-luna'), context={'input_tokens': 9})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    assert [(row['adapter'], row['model']) for row in report['usage']] == [('codex', 'gpt-6-luna')]
    assert report['excluded_rows'] == {'test_fixture': 2}
    assert report['unattributed'] == [{'source': 'fabric', 'adapter': None, 'rows': 1,
                                       'input_tokens': 3, 'statuses': {'cancelled': 1}}]
    assert all(row['model'] not in {'fixture', 'opencode/nonexistent-model'} for row in report['outcomes'])
    assert sum(row['attempts'] for row in report['attempts']) == 2


def test_cache_read_share_counts_only_rows_with_every_input_field(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/s.jsonl'
    jsonl(path, {'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
                 'message': {'id': 'a', 'model': 'claude-opus-5-5', 'usage': {
                     'input_tokens': 10, 'cache_read_input_tokens': 30, 'cache_creation_input_tokens': 10}}},
          {'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
           'message': {'id': 'b', 'model': 'claude-opus-5-5', 'usage': {'input_tokens': 100}}})
    jsonl(tmp_path / 'home/.codex/sessions/day/rollout-x.jsonl',
          {'type': 'session_meta', 'payload': {'id': 'cx'}},
          {'type': 'turn_context', 'payload': {'model': 'gpt-6-luna'}},
          {'type': 'event_msg', 'timestamp': '2026-10-21T01:00:00Z', 'payload': {
              'type': 'token_count', 'info': {'total_token_usage': {
                  'input_tokens': 100, 'cached_input_tokens': 40, 'cache_write_input_tokens': 0,
                  'output_tokens': 1}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    shares = {row['adapter']: row['cache_read_share'] for row in report['usage']}
    # Codex input already includes cached tokens; the ledger stores the uncached remainder.
    assert shares == {'claude': 0.6, 'codex': 0.4}
    claude = next(row for row in report['usage'] if row['adapter'] == 'claude')
    assert claude['cache_share_rows'] == 1 and claude['rows'] == 2


def test_outcomes_group_route_quality_with_retries_errors_latency_and_throughput(tmp_path):
    common = {'task_class': 'review', 'requested_route': {'alias': 'workhorse', 'task_class': 'review'},
              'provenance': route('codex', 'gpt-6.1-sol', 'high'), 'started_at': '2026-10-21T01:00:00Z'}
    receipt(tmp_path, 'one', 1, **common, status='failed', error_class='provider_timeout',
            ended_at='2026-10-21T01:00:30Z', timing={'phases': {'provider': 30000}})
    receipt(tmp_path, 'one', 2, **common, status='ok', ended_at='2026-10-21T01:00:10Z',
            timing={'phases': {'provider': 10000}}, context={'output_tokens': 100})
    receipt(tmp_path, 'two', 1, **common, status='ok', ended_at='2026-10-21T01:00:20Z',
            timing={'phases': {'provider': 20000}}, context={'output_tokens': 100})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    assert report['outcomes'] == [{
        'adapter': 'codex', 'model': 'gpt-6.1-sol', 'effort': 'high', 'task_class': 'review',
        'route': 'workhorse', 'lanes': 2, 'succeeded': 2, 'success_rate': 1.0, 'attempts': 3,
        'retries': 1, 'error_classes': {'provider_timeout': 1},
        'latency_p50_s': 20.0, 'latency_p90_s': 30.0, 'output_tokens_per_s': 7.5}]
    table = cli(tmp_path, 'stats').stdout
    assert 'codex/gpt-6.1-sol@high' in table and '100%' in table


def test_price_table_is_dated_sourced_and_marks_free_and_subscription_routes(tmp_path):
    table = json.loads((ROOT / 'config/telemetry-prices.json').read_text())
    assert table['schema_version'] == 2
    for key, entry in table['models'].items():
        assert entry['source'] in table['sources'], key
        assert entry['date'] and entry['price_status'] in {'verified', 'unverified', 'free'}, key
        assert entry['billing'] in {'api', 'subscription', 'free'}, key
        if entry['price_status'] == 'verified':
            assert all(isinstance(entry['usd_per_million'][field], (int, float))
                       for field in ('input', 'cached_input', 'output')), key
    assert table['models']['claude/claude-opus-5-5']['usd_per_million'] == {
        'input': 4, 'cached_input': 0.2, 'cache_write': 5, 'output': 20}
    receipt(tmp_path, 'free', status='ok', provenance=route('opencode', 'opencode/mimo-v2.6-flash-free'),
            context={'input_tokens': 1000, 'output_tokens': 10, 'cached_input_tokens': 0})
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
        'message': {'id': 'm', 'model': 'claude-opus-5-5', 'usage': {
            'input_tokens': 1_000_000, 'cache_read_input_tokens': 0,
            'cache_creation_input_tokens': 0, 'output_tokens': 0}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    usage = {row['adapter']: row for row in json.loads(cli(tmp_path, 'stats', '--json').stdout)['usage']}
    assert (usage['opencode']['billing'], usage['opencode']['estimated_cost_usd']) == ('free', 0)
    assert (usage['claude']['billing'], usage['claude']['estimated_cost_usd']) == ('subscription', 4)


def test_stats_default_output_is_a_compact_table(tmp_path):
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
        'message': {'id': 'm', 'model': 'claude-opus-5-5', 'usage': {'input_tokens': 1234567}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    lines = cli(tmp_path, 'stats').stdout.splitlines()
    header = next(line for line in lines if line.split()[:3] == ['provider', 'adapter', 'model'])
    row = lines[lines.index(header) + 1].split()
    assert row[:4] == ['anthropic', 'claude', 'claude-opus-5-5', '1']
    assert '1.23M' in row and row[-2:] == ['-', 'subscription']
    assert not any(line.lstrip().startswith('{') for line in lines)


def test_fabric_discovery_ignores_legacy_mcp_run_roots(tmp_path):
    write_json(tmp_path / '.agent-run/mcp-old/tasks/lane/attempt-001/attempt.json', {
        'run_id': 'old', 'task_id': 'lane', 'attempt': 1, 'status': 'ok',
        'provenance': route('codex', 'gpt-6-luna')})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    sources = {row['source']: row for row in json.loads(cli(tmp_path, 'telemetry', 'sources', '--json').stdout)}
    assert sources['fabric']['row_count'] == 0 and 'mcp-' not in sources['fabric']['path_pattern']


def test_ledger_rows_carry_the_configured_host_id_without_duplicates(tmp_path):
    hosts = tmp_path / 'home/.agents/.agent-fabric/hosts.json'
    write_json(hosts, {'schema_version': 1, 'local_host': 'runner-ci'})
    path = tmp_path / 'home/.claude/projects/repo/s.jsonl'
    jsonl(path, {'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
                 'message': {'id': 'one', 'model': 'claude-opus-5-5', 'usage': {'input_tokens': 7}}})
    receipt(tmp_path, 'run', status='ok', provenance=route('codex', 'gpt-6-luna'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        original_host = db.execute('SELECT host FROM usage LIMIT 1').fetchone()[0]
        assert original_host == 'runner-ci'
        db.execute('INSERT INTO quota_events(provider,account,host,window,kind,observed_at) VALUES (?,?,?,?,?,?)',
                   ('codex', 'work', original_host, 'custom', 'natural', '2026-10-21T12:00:00Z'))
    write_json(hosts, {'schema_version': 1, 'local_host': 'studio'})
    jsonl(path, {'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
                 'message': {'id': 'two', 'model': 'claude-opus-5-5', 'usage': {'input_tokens': 7}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT DISTINCT host FROM usage').fetchall() == [('studio',)]
        assert db.execute('SELECT COUNT(*) FROM usage').fetchone()[0] == 3
        assert db.execute('SELECT DISTINCT host FROM usage_files').fetchall() == [('studio',)]
        assert db.execute('SELECT DISTINCT host FROM quota_events').fetchall() == [('studio',)]


def test_host_id_prefers_the_instance_config_over_home_and_hostname(tmp_path, monkeypatch):
    import importlib.util
    spec = importlib.util.spec_from_file_location('telemetry_host_precedence', ROOT / 'scripts/telemetry.py')
    telemetry = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(telemetry)
    monkeypatch.setenv('HOME', str(tmp_path / 'home'))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setattr(telemetry.socket, 'gethostname', lambda: 'runner-ci')
    write_json(tmp_path / 'home/.agents/.agent-fabric/hosts.json',
               {'schema_version': 1, 'local_host': 'home-host'})
    write_json(tmp_path / 'instance/.agent-fabric/hosts.json',
               {'schema_version': 1, 'local_host': 'studio'})
    assert telemetry.host_id() == 'studio'


def test_peer_stats_are_versioned_aggregates_and_read_only(tmp_path):
    path = tmp_path / 'home/.claude/projects/repo/session.jsonl'
    for index in range(2):
        jsonl(path, {'type': 'assistant', 'sessionId': 'same-session', 'timestamp': '2026-10-21T01:00:00Z',
                     'message': {'id': f'm{index}', 'model': 'claude-test', 'content': 'PRIVATE_CONTENT',
                                 'usage': {'input_tokens': 10, 'output_tokens': 20}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    database = tmp_path / 'state/telemetry.sqlite3'
    before = database.read_bytes()
    write_json(tmp_path / 'state/quota/fake--work.json', {
        'schema': 'provenant.quota.v1', 'provider': 'fake', 'account': 'work',
        'host': __import__('socket').gethostname(), 'access_token': 'PRIVATE_TOKEN',
        'credits': {'reset_credits': 1, 'credential': 'PRIVATE_TOKEN', 'source': {'token': 'PRIVATE_TOKEN'}},
        'windows': [{'name': 'custom', 'used_percent': 20, 'credential': 'PRIVATE_TOKEN'}]})
    result = subprocess.run([str(ROOT / 'scripts/provenant'), 'peer'],
                            input=json.dumps({'protocol_version': 1, 'verb': 'telemetry.stats',
                                              'params': {'schema_version': 1, 'since': '2026-10-21'}}),
                            env=environment(tmp_path), capture_output=True, text=True)
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads(result.stdout)['result']
    assert summary['schema'] == 'provenant.telemetry.summary.v1'
    groups = summary['session_usage']
    assert len(groups) == 1 and groups[0]['session'] == 'same-session'
    assert groups[0]['rows'] == 2 and groups[0]['input_tokens'] == 20
    assert 'PRIVATE_CONTENT' not in result.stdout and 'event_id' not in result.stdout and 'origin_path' not in result.stdout
    assert 'PRIVATE_TOKEN' not in result.stdout
    assert summary['quota_snapshots'][0]['windows'][0] == {'name': 'custom', 'used_percent': 20}
    assert database.read_bytes() == before


def test_stats_hosts_all_merges_peer_usage_and_survives_old_and_offline_peers(tmp_path):
    local = tmp_path / 'home/.claude/projects/repo/local.jsonl'
    jsonl(local, {'type': 'assistant', 'sessionId': 'shared-session', 'timestamp': '2026-10-21T01:00:00Z',
                  'message': {'id': 'local', 'model': 'claude-test', 'usage': {'input_tokens': 10, 'output_tokens': 20}}})
    config = tmp_path / 'home/.agents/.agent-fabric/hosts.json'
    write_json(config, {'schema_version': 1, 'local_host': 'studio', 'peers': {
        'peer': {'ssh_destination': 'peer'}, 'offline': {'ssh_destination': 'offline'},
        'old': {'ssh_destination': 'old'}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    # The transport is a fixture; it never reaches SSH or a provider.
    shim = tmp_path / 'ssh-fixture'
    shim.write_text('''#!/usr/bin/env python3
import json, sys
request = json.load(sys.stdin)
name = sys.argv[-2]
if name == 'offline':
    sys.exit(255)
if request['verb'] == 'hello':
    print(json.dumps({'protocol_version': 1, 'ok': True, 'result': {'protocol_version': 1, 'host': name}}))
elif name == 'old':
    print(json.dumps({'protocol_version': 1, 'ok': False, 'error': {'code': 'unknown_verb', 'message': 'old peer'}}))
    sys.exit(1)
else:
    print(json.dumps({'protocol_version': 1, 'ok': True, 'result': json.load(open(__file__ + '.json'))}))
''')
    shim.chmod(0o755)
    # Use the real aggregate encoder, then represent another host with the same session ID.
    import importlib.util
    spec = importlib.util.spec_from_file_location('telemetry_host_test', ROOT / 'scripts/telemetry.py')
    telemetry = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(telemetry)
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.row_factory = sqlite3.Row
        summary = telemetry.peer_summary(db, host='peer')
    Path(str(shim) + '.json').write_text(json.dumps(summary))
    env = {**environment(tmp_path), 'AGENT_FABRIC_SSH_PROGRAM': str(shim),
           'AGENT_FABRIC_INSTANCE_ROOT': str(tmp_path / 'home/.agents')}
    result = subprocess.run([str(ROOT / 'scripts/provenant'), 'stats', '--hosts', 'all', '--json'],
                            env=env, capture_output=True, text=True)
    assert result.returncode == 0, result.stdout + result.stderr
    report = json.loads(result.stdout)
    assert report['partial'] is True
    assert report['usage'][0]['input_tokens'] == 20
    statuses = {row['host']: row for row in report['hosts']}
    assert statuses['peer']['status'] == 'ok'
    assert statuses['offline']['status'] == 'partial' and statuses['offline']['reason'] == 'unreachable'
    assert statuses['old']['status'] == 'partial' and statuses['old']['reason'] == 'unknown_verb'
    # Repeated selectors must not count one (host, source, session) twice.
    result = subprocess.run([str(ROOT / 'scripts/provenant'), 'stats', '--hosts', 'peer', 'peer', '--json'],
                            env=env, capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)['usage'][0]['input_tokens'] == 10


def test_peer_merge_deduplicates_sessions_and_uses_freshest_account_quota(tmp_path, monkeypatch):
    import importlib.util
    def load(name, filename):
        spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / filename)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    telemetry = load('merge_telemetry_test', 'telemetry.py')
    federation = load('merge_federation_test', 'telemetry_hosts.py')
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    with telemetry.connect() as db:
        first = telemetry.peer_summary(db, host='studio')
    second = {**first, 'host': 'peer'}
    def quota(host, observed, used):
        return {'schema': 'provenant.quota.v1', 'provider': 'claude', 'account': 'work', 'host': host,
                'observed_at': observed, 'windows': [{'name': 'custom', 'used_percent': used,
                'window_minutes': 300, 'reset_at': None}]}
    first['quota_snapshots'] = [quota('studio', '2026-10-21T12:00:00Z', 40)]
    second['quota_snapshots'] = [quota('peer', '2026-10-21T13:00:00Z', 60)]
    merged = federation.merge(telemetry, [first, second, second], [], None)
    assert [(row['host'], row['windows'][0]['used_percent']) for row in merged['quota']['accounts']] == [('peer', 60)]


def test_malformed_peer_is_partial_instead_of_breaking_local_stats(tmp_path, monkeypatch):
    import importlib.util
    def load(name, filename):
        spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / filename)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    telemetry = load('bad_peer_telemetry', 'telemetry.py')
    federation = load('bad_peer_federation', 'telemetry_hosts.py')
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    with telemetry.connect() as db:
        good = telemetry.peer_summary(db, host='peer')
        hosts = federation.hosts_owner()
        monkeypatch.setattr(hosts, 'load_config', lambda: hosts.HostsConfig(telemetry.host_id(), {'peer': object()}))
        response = {**good, 'excluded_rows': []}
        monkeypatch.setattr(hosts.PeerClient, 'call', lambda *args, **kwargs: {'ok': True, 'result': response})
        report = federation.collect(telemetry, db, None, ['all'])
        assert report['partial'] and report['hosts'][-1]['reason'] == 'bad_response'
        # A malformed aggregate must not become a string token count.
        response = {**good, 'session_usage': [{'host': 'peer', 'source': 'claude', 'session': 's',
                    'day': None, 'provider': 'anthropic', 'adapter': 'claude', 'account': None,
                    'model': 'test', 'rows': '1'}]}
        report = federation.collect(telemetry, db, None, ['all'])
        assert report['partial'] and report['hosts'][-1]['reason'] == 'bad_response'
        response = {**good, 'quota_observations': [{'used_percent': 'bad'}]}
        report = federation.collect(telemetry, db, None, ['all'])
        assert report['partial'] and report['hosts'][-1]['reason'] == 'bad_response'
        local_quota = {'schema': 'provenant.quota.v1', 'provider': 'codex', 'account': 'default',
                       'host': telemetry.host_id(), 'observed_at': '2026-10-21T12:00:00Z', 'windows': []}
        write_json(tmp_path / 'state/quota/codex--default.json', local_quota)
        response = {**good, 'quota_snapshots': [{**local_quota, 'host': 'peer', 'observed_at': 5}]}
        report = federation.collect(telemetry, db, None, ['all'])
        assert report['partial'] and report['hosts'][-1]['reason'] == 'bad_response'
        response = {**good, 'quota_snapshots': [{**local_quota, 'host': 'peer',
                    'observed_at': '2026-10-21T13:00:00Z', 'windows': [{'name': 'custom', 'used_percent': 30, 'window_minutes': 300,
                                 'reset_at': None, 'credential': 'PRIVATE_TOKEN'}],
                    'credits': {'reset_credits': 1, 'source': 'provider', 'credential': 'PRIVATE_TOKEN'}}]}
        report = federation.collect(telemetry, db, None, ['all'])
        assert 'PRIVATE_TOKEN' not in json.dumps(report)


def test_unreadable_peer_ledger_returns_a_typed_error(tmp_path):
    state = tmp_path / 'state'
    state.mkdir()
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        db.execute('PRAGMA user_version=4')
    result = subprocess.run([str(ROOT / 'scripts/provenant'), 'peer'],
                            input=json.dumps({'protocol_version': 1, 'verb': 'telemetry.stats',
                                              'params': {'schema_version': 1}}),
                            env=environment(tmp_path), capture_output=True, text=True)
    assert result.returncode == 1 and result.stderr == ''
    assert json.loads(result.stdout)['error']['code'] == 'telemetry_unavailable'


def test_stats_reports_time_since_last_limit_per_account_and_host(tmp_path):
    receipt(tmp_path, 'limit-old', status='usage_limited', account='work',
            ended_at='2026-10-20T01:00:00Z', provenance=route('codex', 'gpt-6-luna'))
    receipt(tmp_path, 'limit-new', status='rate_limited', account='work',
            ended_at='2026-10-21T02:00:00Z', provenance=route('codex', 'gpt-6-luna'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    limit = report['last_limit_events'][0]
    assert (limit['provider'], limit['account'], limit['timestamp']) == ('openai', 'work', '2026-10-21T02:00:00Z')
    assert limit['host'] == report['host'] and limit['age_seconds'] >= 0
    assert 'Last limit events' in cli(tmp_path, 'stats').stdout


def test_telemetry_reuses_loaded_quota_and_host_owners():
    from scripts import telemetry

    assert telemetry.quota_owner() is telemetry.quota_owner()
    assert telemetry.host_owner() is telemetry.host_owner()


def otlp_line(signal, body):
    return {'received_at': '2026-10-21T03:00:00Z', 'signal': signal, 'body': body}


def otlp_value(value):
    return {'intValue': str(value)} if isinstance(value, int) else {'stringValue': value}


def otlp_attributes(**values):
    return [{'key': key.replace('__', '.'), 'value': otlp_value(value)} for key, value in values.items()]


def test_events_ledger_is_append_only_metadata_with_harness_revision(tmp_path):
    state = tmp_path / 'state'
    receipt(tmp_path, 'run-e', 1, status='ok', session_id='lane-session',
            provenance={**route('codex', 'gpt-6.1-sol', 'high'), 'harness_rev': 'abc123'},
            evidence={'excerpt': 'PRIVATE_ERROR'})
    receipt(tmp_path, 'run-r', 1, status='running', ended_at=None, provenance=route('codex', 'gpt-6.1-sol'))
    write_json(state / 'route-health.json', {'routes': {'key': {'adapter': 'codex', 'model': 'gpt-6.1-sol', 'recent': [
        {'status': 'usage_limited', 'run_id': 'run-e', 'at': '2026-10-21T01:00:00Z'},
        {'status': 'ok', 'run_id': 'run-e', 'at': '2026-10-21T01:00:01Z'}]}}})
    write_json(state / 'cooldowns.json', {'cooldowns': {'codex/gpt-6.1-sol': {
        'adapter': 'codex', 'model': 'gpt-6.1-sol', 'account': 'work', 'status': 'usage_limited',
        'recorded_at': '2026-10-21T01:00:00Z', 'cooling_until': '2026-10-21T05:00:00Z'}}})
    jsonl(tmp_path / 'home/.claude/projects/repo/session.jsonl', *(
        {'type': 'assistant', 'sessionId': 'chair', 'timestamp': f'2026-10-21T0{index}:00:00Z',
         'message': {'id': f'm{index}', 'model': 'claude-test', 'content': 'PRIVATE_MESSAGE',
                     'usage': {'input_tokens': 1, 'output_tokens': 1}}} for index in (2, 1)))
    resource = {'attributes': otlp_attributes(provenant__run_id='run-e', user__email='PRIVATE_EMAIL')}
    jsonl(state / 'otel/logs-20261021.ndjson', otlp_line('logs', {'resourceLogs': [{
        'resource': resource, 'scopeLogs': [{'logRecords': [{
            'timeUnixNano': '1792544400000000000',
            'attributes': otlp_attributes(event__name='api_request', session__id='chair', model='claude-test',
                                          input_tokens=12, prompt='PRIVATE_PROMPT')}]}]}]}))
    jsonl(state / 'otel/metrics-20261021.ndjson', otlp_line('metrics', {'resourceMetrics': [{
        'resource': resource, 'scopeMetrics': [{'metrics': [{
            'name': 'claude_code.active_time.total', 'sum': {'aggregationTemporality': 1, 'dataPoints': [
                {'asDouble': 90.0, 'attributes': otlp_attributes(type='user', session__id='chair')},
                {'asDouble': 30.0, 'attributes': otlp_attributes(type='cli', session__id='chair')}]}}]}]}]}))
    for _ in range(2):
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        db.row_factory = sqlite3.Row
        events = [dict(row) for row in db.execute('SELECT * FROM events ORDER BY kind, ts')]
        dump = '\n'.join(db.iterdump())
    assert [row['kind'] for row in events] == [
        'cooldown', 'lane.attempt', 'limit.usage_limited', 'otel.api_request', 'otel.metric', 'otel.metric',
        'session.start']
    lane = events[1]
    assert (lane['run_id'], lane['task_id'], lane['session'], lane['harness_rev']) == ('run-e', 'lane', 'lane-session', 'abc123')
    assert json.loads(lane['attrs'])['status'] == 'ok' and lane['source_ref'].endswith('attempt-001/attempt.json')
    assert all(row['harness_rev'] is None for row in events if row['kind'] != 'lane.attempt')
    api = events[3]
    assert (api['session'], api['run_id'], api['model']) == ('chair', 'run-e', 'claude-test')
    assert json.loads(api['attrs']) == {'input_tokens': 12, 'model': 'claude-test'}
    assert '@' in api['source_ref'] and api['ts'].startswith('2026-')
    session = events[-1]
    assert (session['session'], session['ts']) == ('chair', '2026-10-21T01:00:00Z')
    assert session['source_ref'].endswith('session.jsonl')
    for private in ('PRIVATE_ERROR', 'PRIVATE_MESSAGE', 'PRIVATE_PROMPT', 'PRIVATE_EMAIL'):
        assert private not in dump
    sources = {row['source']: row for row in json.loads(cli(tmp_path, 'telemetry', 'sources', '--json').stdout)}
    assert sources['otel']['status'] == 'ok' and sources['otel']['row_count'] == 3
    # An older ledger without events re-reads receipts and snapshots once to backfill them.
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        db.execute('DELETE FROM events')
        db.execute("DELETE FROM meta WHERE key IN ('events_backfill', 'events_usage_rowid')")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        assert {row[0] for row in db.execute('SELECT kind FROM events')} >= {
            'lane.attempt', 'limit.usage_limited', 'cooldown', 'session.start'}
        assert db.execute('SELECT COUNT(*) FROM usage').fetchone()[0] == 4  # re-reading adds no usage rows


def fake_gh(tmp_path, *pages):
    """A `gh` stub on PATH: each call prints the next page and logs its arguments."""
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir(exist_ok=True)
    for index, page in enumerate(pages):
        (bin_dir / f'page-{index}.json').write_text(json.dumps(page))
    script = bin_dir / 'gh'
    script.write_text(f'''#!/usr/bin/env python3
import json, pathlib, sys
log = pathlib.Path({str(bin_dir / 'calls.jsonl')!r})
calls = log.read_text().splitlines() if log.exists() else []
with log.open('a') as stream:
    stream.write(json.dumps(sys.argv[1:]) + '\\n')
page = pathlib.Path({str(bin_dir)!r}) / f'page-{{min(len(calls), {len(pages) - 1})}}.json'
print(page.read_text())
''')
    script.chmod(0o755)
    return bin_dir


def cli_with(tmp_path, bin_dir, *args):
    env = environment(tmp_path)
    env['PATH'] = os.pathsep.join((str(bin_dir), env['PATH']))
    return subprocess.run([str(ROOT / 'scripts/provenant'), *args], env=env, cwd=tmp_path,
                          capture_output=True, text=True)


def pull(number, merged_at, title='Change', labels=()):
    return {'number': number, 'mergeCommit': {'oid': f'{number:07d}abcdef'}, 'mergedAt': merged_at,
            'title': title, 'labels': [{'name': name} for name in labels]}


def test_changes_sync_is_incremental_idempotent_and_takes_manual_markers(tmp_path):
    first = [pull(10, '2026-10-01T00:00:00Z', 'Lighter review ladder', ['area:review'])]
    second = [pull(10, '2026-10-01T00:00:00Z', 'Lighter review ladder (renamed)', ['area:review']),
              pull(11, '2026-10-05T12:00:00Z', 'Quota-aware routing', ['area:routing'])]
    bin_dir = fake_gh(tmp_path, first, second)
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'changes', '--sync').returncode == 0
    result = cli_with(tmp_path, bin_dir, 'telemetry', 'changes', '--sync', '--json')
    assert result.returncode == 0, result.stderr
    rows = json.loads(result.stdout)
    assert [(row['pr'], row['title'], row['labels']) for row in rows] == [
        (11, 'Quota-aware routing', ['area:routing']), (10, 'Lighter review ladder (renamed)', ['area:review'])]
    assert rows[0]['merge_commit'] == '0000011abcdef' and rows[0]['merged_at'] == '2026-10-05T12:00:00Z'
    calls = [json.loads(line) for line in (bin_dir / 'calls.jsonl').read_text().splitlines()]
    assert calls[0][:4] == ['pr', 'list', '--state', 'merged'] and '--limit' in calls[0]
    assert '--search' not in calls[0] and calls[1][-2:] == ['--search', 'merged:>=2026-10-01']
    added = cli_with(tmp_path, bin_dir, 'telemetry', 'changes', '--add', 'switched default reviewer',
                     '--at', '2026-10-03')
    assert added.returncode == 0, added.stderr
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'changes', '--add', 'switched default reviewer',
                    '--at', '2026-10-03').returncode == 0
    text = cli_with(tmp_path, bin_dir, 'telemetry', 'changes').stdout
    assert text.count('switched default reviewer') == 1 and '#11' in text and 'manual' in text
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'changes', '--add', 'x').returncode == 2
    failing = tmp_path / 'failing'
    failing.mkdir()
    (failing / 'gh').write_text('#!/bin/sh\necho PRIVATE_GH_OUTPUT >&2\nexit 4\n')
    (failing / 'gh').chmod(0o755)
    result = cli_with(tmp_path, failing, 'telemetry', 'changes', '--sync')
    assert result.returncode == 1 and 'exit 4' in result.stderr and 'PRIVATE_GH_OUTPUT' not in result.stderr


def lanes_around(project, prefix, count, ok, day):
    for index in range(count):
        receipt(project, f'{prefix}-{index:02d}', 1, status='ok' if index < ok else 'failed',
                error_class=None if index < ok else 'provider_timeout',
                provenance=route('codex', 'gpt-6.1-sol', 'high'),
                started_at=f'{day}T01:00:00Z', ended_at=f'{day}T01:00:{10 + index:02d}Z',
                context={'input_tokens': 100, 'cached_input_tokens': 50, 'cache_write_tokens': 0,
                         'output_tokens': 10, 'reasoning_tokens': 0})


def test_stats_compare_reports_sample_sizes_and_never_calls_a_small_side_a_win(tmp_path):
    bin_dir = fake_gh(tmp_path, [pull(20, '2026-10-10T00:00:00Z', 'Evidence-based acceptance')])
    lanes_around(tmp_path, 'before', 12, 6, '2026-10-08')
    lanes_around(tmp_path, 'after', 3, 3, '2026-10-11')
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'ingest').returncode == 0
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'changes', '--sync').returncode == 0
    result = cli_with(tmp_path, bin_dir, 'stats', '--compare', '20', '--window', '7d', '--json')
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report['pivot'] == '2026-10-10T00:00:00Z' and report['change']['pr'] == 20
    assert (report['before']['lanes'], report['after']['lanes']) == (12, 3)
    assert report['before']['success_rate'] == 0.5 and report['after']['success_rate'] == 1.0
    assert report['after']['inconclusive'] is True and report['before']['inconclusive'] is False
    metrics = {row['metric']: row for row in report['metrics']}
    assert metrics['success_rate']['verdict'] == 'inconclusive'
    assert (metrics['success_rate']['n_before'], metrics['success_rate']['n_after']) == (12, 3)
    assert report['before']['error_classes'] == {'provider_timeout': 6}
    assert report['before']['cache_read_share'] == 0.5
    text = cli_with(tmp_path, bin_dir, 'stats', '--compare', '#20')
    assert text.returncode == 0, text.stderr
    assert len(text.stdout.encode()) < 4096
    assert 'inconclusive (n<10)' in text.stdout and '12/3 lanes' in text.stdout and 'improved' not in text.stdout
    # A large enough after side gets a verdict; a date pivots the same way.
    lanes_around(tmp_path, 'later', 12, 12, '2026-10-12')
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli_with(tmp_path, bin_dir, 'stats', '--compare', '2026-10-10', '--json').stdout)
    metrics = {row['metric']: row for row in report['metrics']}
    assert report['change'] is None and report['after']['lanes'] == 15
    assert metrics['success_rate']['verdict'] == 'improved'
    assert metrics['cache_read_share']['verdict'] == 'within_noise'
    missing = cli_with(tmp_path, bin_dir, 'stats', '--compare', '999')
    assert missing.returncode == 1 and 'changes --sync' in missing.stderr
    assert cli_with(tmp_path, bin_dir, 'stats', '--compare', '20', '--window', 'soon').returncode == 1
    assert cli_with(tmp_path, bin_dir, 'stats', '--window', '7d').returncode == 2


def test_retro_is_a_compact_digest_of_changes_errors_and_attention(tmp_path):
    pulls = [pull(100 + index, f'2026-10-{10 + index // 2:02d}T{index % 2 * 12:02d}:00:00Z',
                  'A fairly long pull request title that will be truncated in the digest') for index in range(15)]
    bin_dir = fake_gh(tmp_path, pulls)
    lanes_around(tmp_path, 'lane', 12, 9, '2026-10-12')
    receipt(tmp_path, 'waiting', 1, status='input_required', provenance=route('codex', 'gpt-6.1-sol'),
            ended_at='2026-10-12T02:00:00Z')
    jsonl(tmp_path / 'home/.claude/projects/repo/session.jsonl', {
        'type': 'assistant', 'sessionId': 'chair', 'timestamp': '2026-10-12T01:00:00Z',
        'message': {'id': 'm1', 'model': 'claude-test', 'usage': {'input_tokens': 1, 'output_tokens': 1}}})
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'ingest').returncode == 0
    assert cli_with(tmp_path, bin_dir, 'telemetry', 'changes', '--sync').returncode == 0
    result = cli_with(tmp_path, bin_dir, 'retro', '--since', '2026-10-01')
    assert result.returncode == 0, result.stderr
    assert len(result.stdout.encode()) < 4096
    assert '#114' in result.stdout and '3 more; use --json' in result.stdout
    assert 'Top error classes (attempts): provider_timeout:3' in result.stdout
    assert 'sessions started 1' in result.stdout and 'lanes awaiting input 1' in result.stdout
    report = json.loads(cli_with(tmp_path, bin_dir, 'retro', '--since', '2026-10-01', '--json').stdout)
    assert len(report['changes']) == 15 and report['changes'][0]['compare']['metrics']
    assert report['events_by_kind']['lane.attempt'] == 13
