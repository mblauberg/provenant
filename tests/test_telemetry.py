from datetime import UTC, datetime, timedelta
from functools import partial
from pathlib import Path
import contextlib
import io
import itertools
import json
import os
import sqlite3
import subprocess
import sys
import types
from unittest.mock import patch

import pytest
from tests.support.modules import load_script


from tests.support.front_door import front_door_binary

ROOT = Path(__file__).resolve().parents[1]


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
        # A rename never rewrites the ledger: earlier rows keep their id and read as this host.
        assert sorted(db.execute('SELECT host, COUNT(*) FROM usage GROUP BY host')) == [('runner-ci', 2), ('studio', 1)]
        assert db.execute('SELECT DISTINCT host FROM quota_events').fetchall() == [('runner-ci',)]


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


def cli(tmp_path, *args, scripts_path=True):
    arguments = list(args[1:] if args and args[0] == 'telemetry' else args)
    stdout, stderr = io.StringIO(), io.StringIO()
    original_cwd = Path.cwd()
    previous_module = sys.modules.get('telemetry_cli_under_test')
    try:
        os.chdir(tmp_path)
        with patch.dict(os.environ, environment(tmp_path)), \
                patch.object(sys, 'path', ([str(ROOT / 'scripts'), *sys.path] if scripts_path else
                                           [path for path in sys.path if path != str(ROOT / 'scripts')])), \
                contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_cli_under_test', register=True)
            code = telemetry.main(arguments)
    finally:
        if previous_module is None:
            sys.modules.pop('telemetry_cli_under_test', None)
        else:
            sys.modules['telemetry_cli_under_test'] = previous_module
        os.chdir(original_cwd)
    return subprocess.CompletedProcess([str(front_door_binary()), *args], code,
                                        stdout.getvalue(), stderr.getvalue())


def cli_process(tmp_path, *args):
    return subprocess.run([str(front_door_binary()), *args],
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
        'fabric', 'route_health', 'cooldowns', 'claude', 'codex', 'opencode', 'otel', 'delivery'}
    assert all(row['status'] == 'absent' and row['row_count'] == 0 for row in rows)
    assert all(row['parser_version'] == 2 and row['path_pattern'] for row in rows)
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("PRAGMA user_version").fetchone()[0] == 4


def test_sources_query_reads_a_read_only_ledger(tmp_path):
    seeded = cli(tmp_path, 'telemetry', 'ingest')
    assert seeded.returncode == 0, seeded.stderr
    ledger = tmp_path / 'state/telemetry.sqlite3'
    directory_mode = ledger.parent.stat().st_mode & 0o777
    file_mode = ledger.stat().st_mode & 0o777
    try:
        ledger.chmod(0o444)
        ledger.parent.chmod(0o555)
        result = cli(tmp_path, 'telemetry', 'sources', '--json')
        stats = cli(tmp_path, 'telemetry', 'stats', '--json')
    finally:
        ledger.parent.chmod(directory_mode)
        ledger.chmod(file_mode)
    assert result.returncode == 0, result.stderr
    rows = json.loads(result.stdout)
    assert len(rows) == 8
    assert {row['source'] for row in rows} == {
        'fabric', 'route_health', 'cooldowns', 'claude', 'codex', 'opencode', 'otel', 'delivery'}
    assert stats.returncode == 0, stats.stderr
    assert isinstance(json.loads(stats.stdout), dict)


def test_stats_auto_ingest_runs_when_a_source_is_stale_but_skips_fresh_sources(tmp_path):
    from datetime import UTC, datetime, timedelta

    stamp = datetime.now(UTC).isoformat()
    receipt(tmp_path, 'first', status='ok', ended_at=stamp)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    receipt(tmp_path, 'second', status='ok', ended_at=stamp)
    fresh = cli(tmp_path, 'stats', '--json')
    assert fresh.returncode == 0, fresh.stderr
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("SELECT COUNT(*) FROM usage WHERE source='fabric'").fetchone()[0] == 1
        db.execute("UPDATE sources SET last_ingest=? WHERE source='fabric'",
                   ((datetime.now(UTC) - timedelta(hours=2)).isoformat(),))
    stale = cli(tmp_path, 'stats', '--json')
    assert stale.returncode == 0, stale.stderr
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("SELECT COUNT(*) FROM usage WHERE source='fabric'").fetchone()[0] == 2


def test_read_only_connection_restores_the_query_lock_wait_after_opening(tmp_path):
    receipt(tmp_path, 'opened', status='ok', ended_at=datetime.now(UTC).isoformat())
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_readonly_under_test')
    with patch.dict(os.environ, environment(tmp_path)):
        db = telemetry.connect(readonly=True)
    try:
        wait = db.execute('PRAGMA busy_timeout').fetchone()[0]
    finally:
        db.close()
    assert wait == telemetry.SQLITE_BUSY_TIMEOUT_SECONDS * 1000


def test_auto_ingest_failure_warns_but_query_returns_available_ledger(tmp_path):
    from datetime import UTC, datetime, timedelta

    receipt(tmp_path, 'available', status='ok', ended_at=datetime.now(UTC).isoformat())
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    bad = tmp_path / 'state/otel/broken.ndjson'
    bad.parent.mkdir(parents=True)
    bad.write_text('{ malformed\n')
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute("UPDATE sources SET last_ingest=? WHERE source='otel'",
                   ((datetime.now(UTC) - timedelta(hours=2)).isoformat(),))
    result = cli(tmp_path, 'stats', '--json')
    assert result.returncode == 0
    assert json.loads(result.stdout)['schema_version'] == 2
    assert 'auto-ingest failed' in result.stderr


def test_auto_ingest_interval_accepts_fractional_seconds(tmp_path):
    from datetime import UTC, datetime, timedelta

    receipt(tmp_path, 'fractional', status='ok', ended_at=datetime.now(UTC).isoformat())
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    receipt(tmp_path, 'fractional-refresh', status='ok', ended_at=datetime.now(UTC).isoformat())
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute("UPDATE sources SET last_ingest=? WHERE source='fabric'",
                   ((datetime.now(UTC) - timedelta(seconds=1)).isoformat(),))
    with patch.dict(os.environ, {'PROVENANT_TELEMETRY_AUTO_INGEST_SECONDS': '0.5'}):
        result = cli(tmp_path, 'stats', '--json')
    assert result.returncode == 0, result.stderr
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("SELECT COUNT(*) FROM usage WHERE source='fabric'").fetchone()[0] == 2


def test_retro_and_changes_auto_ingest_stale_sources(tmp_path):
    stamp = datetime.now(UTC) - timedelta(minutes=4)

    receipt(tmp_path, 'before-retro', status='ok', ended_at=stamp.isoformat())
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    receipt(tmp_path, 'from-retro', status='ok', ended_at=(stamp + timedelta(minutes=1)).isoformat())
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute("UPDATE sources SET last_ingest=? WHERE source='fabric'",
                   ((datetime.now(UTC) - timedelta(hours=2)).isoformat(),))
    retro = cli(tmp_path, 'retro', '--since', (stamp - timedelta(days=1)).isoformat(), '--json')
    assert retro.returncode == 0, retro.stderr
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("SELECT COUNT(*) FROM usage WHERE run_id='from-retro'").fetchone() == (1,)

    receipt(tmp_path, 'from-changes', status='ok', ended_at=(stamp + timedelta(minutes=2)).isoformat())
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute("UPDATE sources SET last_ingest=? WHERE source='fabric'",
                   ((datetime.now(UTC) - timedelta(hours=2)).isoformat(),))
    changes = cli(tmp_path, 'telemetry', 'changes', '--json')
    assert changes.returncode == 0, changes.stderr
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("SELECT COUNT(*) FROM usage WHERE run_id='from-changes'").fetchone() == (1,)


def test_delivery_receipt_fields_migration_rereads_unchanged_receipts(tmp_path):
    run = tmp_path / '.agent-run/runs/delivery-migration/RUN.json'
    write_json(run, {'run_id': 'delivery-migration', 'risk_tier': 'substantial',
                     'writer_model': 'gpt-6-luna',
                     'reviews': [{'id': 'review-1', 'role': 'targeted', 'status': 'failed',
                                  'provider_family': 'anthropic', 'model': 'claude-sonnet',
                                  'lenses': ['privacy']}]})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        assert db.execute("SELECT 1 FROM meta WHERE key='delivery_run_time_v2'").fetchone()
        db.execute("UPDATE deliveries SET writer_model=NULL, review_verdicts='[]'")
        db.execute("DELETE FROM meta WHERE key='delivery_receipt_fields_v1'")
    result = cli(tmp_path, 'stats', '--json')
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    delivery = next(row for row in report['deliveries'] if row['run_id'] == 'delivery-migration')
    assert delivery['writer_model'] == 'gpt-6-luna'
    assert delivery['review_verdicts'][0]['verdict'] == 'failed'


def test_ingest_owner_ignores_cwd_module_shadowing(tmp_path):
    shadow = tmp_path / 'telemetry_ingest.py'
    shadow.write_text('raise AssertionError("cwd module was imported")\n')
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_owner_path_test')
    original_cwd = Path.cwd()
    telemetry.ingest_owner.cache_clear()
    try:
        os.chdir(tmp_path)
        with patch.object(sys, 'path', [str(tmp_path), *sys.path]):
            owner = telemetry.ingest_owner()
        assert Path(owner.__file__).resolve() == (ROOT / 'scripts/telemetry_ingest.py').resolve()
    finally:
        telemetry.ingest_owner.cache_clear()
        os.chdir(original_cwd)


def test_otel_and_delivery_sources_ingest_safe_usage_and_structured_review_fields(tmp_path):
    from datetime import UTC, datetime

    stamp = datetime.now(UTC)
    nanos = str(int(stamp.timestamp() * 1_000_000_000))
    jsonl(tmp_path / 'state/otel/logs-current.ndjson', otlp_line('logs', {'resourceLogs': [{
        'resource': {'attributes': []}, 'scopeLogs': [{'logRecords': [
            {'timeUnixNano': nanos,
             'attributes': otlp_attributes(**{'event.name': 'api_request', 'model': 'gpt-6-luna',
                                               'input_tokens': 17, 'output_tokens': 4,
                                               'mystery_count': 991, 'prompt_text': 'PRIVATE_PROMPT',
                                               'tool_output': 'PRIVATE_TOOL'})},
            {'timeUnixNano': nanos,
             'attributes': otlp_attributes(**{'event.name': 'user_prompt', 'prompt_length': 23,
                                               'prompt_text': 'PRIVATE_PROMPT'})},
            {'timeUnixNano': nanos,
             'attributes': otlp_attributes(**{'event.name': 'tool_decision', 'tool_name': 'Bash',
                                               'decision': 'allow', 'tool_input': 'PRIVATE_TOOL'})},
            {'timeUnixNano': nanos,
             'attributes': otlp_attributes(**{'event.name': 'codex.user_prompt', 'prompt_length': 5,
                                               'prompt': 'PRIVATE_PROMPT'})},
            {'timeUnixNano': nanos,
             'attributes': otlp_attributes(**{'event.name': 'codex.tool_decision', 'tool_name': 'shell',
                                               'decision': 'approved', 'call_id': 'PRIVATE_CALL'})}]}]}]}))
    metric_point = {'asDouble': 2.0, 'timeUnixNano': nanos,
                    'attributes': otlp_attributes(session='s1', **{'user.email': 'private@example.test'})}
    metric = {'name': 'claude_code.active_time.total',
              'sum': {'aggregationTemporality': 1, 'dataPoints': [metric_point]}}
    metric_body = {'resourceMetrics': [{'resource': {'attributes': []},
                                        'scopeMetrics': [{'metrics': [metric]}]}]}
    jsonl(tmp_path / 'state/otel/metrics-current.ndjson', otlp_line('metrics', metric_body))
    run = tmp_path / '.agent-run/runs/delivery-fixture'
    receipt_data = json.loads((ROOT / 'skills/deliver/templates/RUN.template.json').read_text())
    receipt_data.update({
        'run_id': 'delivery-fixture', 'risk_tier': 'substantial', 'chair_family': 'openai',
        'writer_model': 'gpt-6-luna', 'reviews': [
            {'id': 'review-a', 'role': 'targeted', 'status': 'pass', 'provider_family': 'anthropic',
             'model': 'claude-sonnet', 'lenses': ['correctness']},
            {'id': 'review-b', 'role': 'targeted', 'status': 'failed', 'provider_family': 'openai',
             'model': 'gpt-review', 'lenses': ['privacy']}],
    })
    shape = load_script(ROOT / 'skills/deliver/scripts/delivery_run_shape.py', 'delivery_run_shape_test')
    assert shape.shape_errors(receipt_data) == []
    write_json(run / 'RUN.json', receipt_data)
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 0, result.stderr
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    delivery = next(row for row in report['deliveries'] if row['run_id'] == 'delivery-fixture')
    assert delivery['writer_model'] == 'gpt-6-luna'
    assert [row['verdict'] for row in delivery['review_verdicts']] == ['pass', 'failed']
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        events = dict(db.execute("SELECT kind,attrs FROM events WHERE kind LIKE 'otel.%'"))
        assert json.loads(events['otel.user_prompt'])['prompt_length'] == 23
        assert json.loads(events['otel.tool_decision'])['tool_name'] == 'Bash'
        assert json.loads(events['otel.tool_decision'])['decision'] == 'allow'
        assert json.loads(events['otel.metric'])['name'] == 'claude_code.active_time.total'
        attrs = events['otel.api_request']
    assert 'PRIVATE_PROMPT' not in attrs and 'PRIVATE_TOOL' not in attrs
    assert 'prompt_text' not in attrs and 'tool_output' not in attrs and 'mystery_count' not in attrs
    assert 'prompt_text' not in events['otel.user_prompt'] and 'tool_input' not in events['otel.tool_decision']
    assert json.loads(events['otel.codex.user_prompt']) == {'prompt_length': 5}
    assert json.loads(events['otel.codex.tool_decision']) == {'tool_name': 'shell', 'decision': 'approved'}
    assert 'user.email' not in events['otel.metric'] and 'private@example.test' not in events['otel.metric']
    assert json.loads(attrs)['input_tokens'] == 17
    retro = cli(tmp_path, 'retro', '--since', stamp.isoformat(), '--json')
    assert retro.returncode == 0, retro.stderr
    attention = json.loads(retro.stdout)['attention']
    assert attention['user_prompts'] == 2
    assert attention['tool_decisions'] == 2


@pytest.mark.parametrize('version,message', [
    (3, 'telemetry: telemetry schema version 3 needs migration; run provenant telemetry ingest'),
    (9, 'telemetry: unsupported telemetry schema version: 9'),
], ids=['older', 'newer'])
def test_query_reports_schema_version_mismatch(tmp_path, version, message):
    seeded = cli(tmp_path, 'telemetry', 'ingest')
    assert seeded.returncode == 0, seeded.stderr
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        db.execute(f'PRAGMA user_version = {version}')
    result = cli(tmp_path, 'telemetry', 'sources', '--json')
    assert result.returncode == 1
    assert result.stderr.strip() == message


@pytest.mark.parametrize('create_ledger, expected_tables', [(False, None), (True, [])],
                         ids=['missing-ledger', 'uninitialised-ledger'])
def test_query_reports_missing_or_uninitialised_ledger_without_creating_schema(tmp_path, create_ledger,
                                                                              expected_tables):
    (tmp_path / 'state').mkdir()
    ledger = tmp_path / 'state/telemetry.sqlite3'
    if create_ledger:
        with sqlite3.connect(ledger):
            pass
    result = cli(tmp_path, 'telemetry', 'sources', '--json')
    assert result.returncode == 0, result.stderr
    assert result.stderr.strip() == 'telemetry: ledger not initialised; reporting an empty ledger'
    assert all(row['row_count'] == 0 for row in json.loads(result.stdout))
    if expected_tables is None:
        assert not ledger.exists()
    else:
        with sqlite3.connect(ledger) as db:
            assert db.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall() == expected_tables


def test_delivery_sidecars_ingest_repair_transitions_and_finding_lineage(tmp_path):
    run = tmp_path / '.agent-run/runs/20261022-1200-delivery-test-a1b2c3'
    run.mkdir(parents=True)
    (run / 'RUN.json').write_text(json.dumps({
        'repair_cycles': 2,
        'reviews': [],
    }))
    jsonl(run / 'repairs.jsonl',
          {'cycle': 1, 'at': '2026-10-22T00:00:00Z', 'failing_before': ['a', 'b']},
          {'cycle': 2, 'at': '2026-10-22T00:01:00Z', 'failing_before': ['b']})
    jsonl(run / 'findings.jsonl', *[
        {'review_id': 'review-1', 'finding': f'f-{index}', 'disposition': disposition,
         'reviewer_family': 'fallback-family', 'model': 'fallback-model', 'reviewer_id': 'fallback-reviewer',
         'at': f'2026-10-22T00:0{index}:00Z'}
        for index, disposition in enumerate(('confirmed', 'false_positive', 'reviewer_bias'), start=1)
    ])
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 0, result.stdout + result.stderr
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        rows = [json.loads(attrs) for (attrs,) in db.execute(
            "SELECT attrs FROM events WHERE kind='repair.cycle' ORDER BY ts")]
        findings = [json.loads(attrs) for (attrs,) in db.execute(
            "SELECT attrs FROM events WHERE kind='review.finding' ORDER BY ts")]
    assert rows[0]['failing_after_count'] == 1
    assert rows[0]['failing_after_hash'] == rows[1]['failing_before_hash']
    assert [item['disposition'] for item in findings] == ['confirmed', 'false_positive', 'reviewer_bias']
    assert all(item['reviewer_family'] == 'fallback-family' and item['model'] == 'fallback-model'
               for item in findings)
    receipt = json.loads((run / 'RUN.json').read_text())
    receipt['delivery_acceptance'] = {'basis': 'checks-and-review', 'evidence_ids': ['checks']}
    receipt['reviews'] = [{'id': 'review-1', 'provider_family': 'anthropic', 'model': 'claude-test',
                           'reviewer_id': 'reviewer'}]
    (run / 'RUN.json').write_text(json.dumps(receipt))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        final = json.loads(db.execute("SELECT attrs FROM events WHERE kind='repair.cycle' ORDER BY ts DESC LIMIT 1").fetchone()[0])
        findings = [json.loads(attrs) for (attrs,) in db.execute(
            "SELECT attrs FROM events WHERE kind='review.finding' ORDER BY ts")]
    assert final['failing_after_count'] == 0
    assert final['failing_after'] == []
    assert len(findings) == 3
    assert all(item['reviewer_family'] == 'anthropic' and item['model'] == 'claude-test' for item in findings)
    sources = {item['source']: item for item in json.loads(cli(tmp_path, 'telemetry', 'sources', '--json').stdout)}
    assert sources['delivery']['row_count'] == 6


def test_provider_compaction_rows_store_only_metadata(tmp_path):
    env = environment(tmp_path)
    home = Path(env['HOME'])
    claude = home / '.claude/projects/project/session.jsonl'
    codex = home / '.codex/sessions/2026/rollout-test.jsonl'
    claude_rows = [
        {'type': 'system', 'subtype': 'compact_boundary', 'sessionId': 'claude-session',
         'timestamp': '2026-10-22T00:00:00Z',
         'compactMetadata': {'trigger': 'auto', 'preTokens': 100, 'postTokens': 20}},
        *[{'type': 'assistant', 'sessionId': 'claude-session', 'timestamp': f'2026-10-22T00:0{index}:00Z',
           'message': {'id': f'message-{index}', 'model': 'claude-test',
                       'usage': {'input_tokens': 1, 'output_tokens': 1}}}
          for index in range(1, 4)],
    ]
    jsonl(claude, *claude_rows)
    duplicate_claude = home / '.claude-michael/projects/project/session.jsonl'
    jsonl(duplicate_claude, *claude_rows)
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'claude': {
        'michael': {'identity_dir': str(home / '.claude-michael')}}}})
    jsonl(codex,
          {'type': 'session_meta', 'payload': {'id': 'codex-session'}},
          {'type': 'compacted', 'timestamp': '2026-10-22T00:02:00Z',
           'payload': {'message': 'never store this', 'replacement_history': [{'secret': 'never store'}]}})
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 0, result.stderr
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        rows = db.execute("SELECT session, attrs FROM events WHERE kind='session.compaction' ORDER BY session").fetchall()
    assert len(rows) == 2
    assert rows[0][0] == 'claude-session'
    assert json.loads(rows[0][1]) == {'post_tokens': 20, 'pre_tokens': 100, 'trigger': 'auto'}
    assert rows[1][0] == 'codex-session'
    assert 'never store' not in json.dumps(rows)
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute("UPDATE usage SET lane='lane-a' WHERE session='claude-session'")
        assert db.execute("SELECT COUNT(*) FROM usage WHERE session='claude-session'").fetchone()[0] == 3
    summary = json.loads(cli(tmp_path, 'telemetry', 'stats', '--json').stdout)
    bounded = json.loads(cli(tmp_path, 'telemetry', 'stats', '--json', '--since', '2026-10-22T00:01:00Z').stdout)
    assert summary['compactions_per_lane'] == [{'lane': 'lane-a', 'compactions': 1}]
    assert bounded['compactions_per_lane'] == []
    assert 'Compactions per lane:' in cli(tmp_path, 'telemetry', 'stats').stdout


def test_delivery_discovery_follows_linked_worktree_git_common_dir(tmp_path):
    primary = tmp_path / 'primary'
    primary.mkdir()
    subprocess.run(['git', 'init', '-q', str(primary)], check=True)
    subprocess.run(['git', '-C', str(primary), 'config', 'user.email', 'test@example.com'], check=True)
    subprocess.run(['git', '-C', str(primary), 'config', 'user.name', 'Test'], check=True)
    (primary / 'tracked').write_text('tracked\n')
    subprocess.run(['git', '-C', str(primary), 'add', 'tracked'], check=True)
    subprocess.run(['git', '-C', str(primary), 'commit', '-qm', 'init'], check=True)
    worktree = tmp_path / 'linked'
    subprocess.run(['git', '-C', str(primary), 'worktree', 'add', '-qb', 'test/linked', str(worktree)], check=True)
    run = primary / '.agent-run/runs/20261022-1200-delivery-test-a1b2c3'
    run.mkdir(parents=True)
    jsonl(run / 'repairs.jsonl', {'cycle': 1, 'at': '2026-10-22T00:00:00Z', 'failing_before': ['lint']})
    result = cli(worktree, 'telemetry', 'ingest')
    assert result.returncode == 0, result.stdout + result.stderr
    with sqlite3.connect(worktree / 'state/telemetry.sqlite3') as db:
        assert db.execute("SELECT COUNT(*) FROM events WHERE kind='repair.cycle'").fetchone()[0] == 1


def test_accepted_uses_receipt_basis_canonical_artifact_and_synced_merge(tmp_path):
    root = tmp_path / '.agent-run/runs'
    merge_commit = 'b' * 40
    for name, basis, canonical_commit in [
            ('checks', 'checks-and-review', merge_commit),
            ('landing', 'project-landing', 'c' * 40),
            ('user', 'user', None), ('unaccepted', None, None),
            ('invalid', 'ok', None)]:
        receipt = {'run_id': name, 'risk_tier': 'substantial',
                   'software_delivery': {'canonical_artifact_id': 'source'},
                   'artifacts': [{'id': 'source', 'git_revision': {'commit': canonical_commit}},
                                 {'id': 'other', 'git_revision': {'commit': merge_commit}}]}
        if basis:
            receipt['delivery_acceptance'] = {'basis': basis}
        write_json(root / name / 'RUN.json', receipt)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        db.execute("INSERT INTO changes(change_id,kind,pr,merge_commit,merged_at,title,labels,added_at) "
                   "VALUES ('merge:1','pull_request',1,?,'2026-10-22T00:00:00Z','merged','[]',"
                   "'2026-10-22T00:00:00Z')", (merge_commit,))
    report = json.loads(cli(tmp_path, 'telemetry', 'accepted', '--json').stdout)
    assert len(report) == 1
    assert (report[0]['runs'], report[0]['accepted'], report[0]['merged']) == (5, 3, 1)


def test_accepted_cost_is_null_and_unlinked_even_with_matching_lanes(tmp_path):
    root = tmp_path / '.agent-run/runs'
    write_json(root / 'delivery/RUN.json', {
        'run_id': 'delivery', 'risk_tier': 'routine',
        'delivery_acceptance': {'basis': 'user'},
        'authority': {'allowed_source_paths': ['.worktrees/shared']},
        'evidence': [{'started_at': '2026-10-22T00:00:00Z',
                      'finished_at': '2026-10-22T00:02:00Z'}]})
    write_json(root / 'dispatch/dispatch/tasks/task/attempt-001/attempt.json', {
        'run_id': 'dispatch', 'task_id': 'task', 'attempt': 1, 'status': 'ok',
        'workspace': {'root': str(tmp_path / '.worktrees/shared')},
        'started_at': '2026-10-22T00:00:00Z', 'ended_at': '2026-10-22T00:01:00Z',
        'provenance': {'requested': {'adapter': 'claude', 'model': 'opus'}, 'provider': 'anthropic'},
        'profile': {'preset': 'balanced', 'digest': 'a' * 64, 'knobs': {}},
        'usage': {'input_tokens': 1000, 'output_tokens': 2000}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'telemetry', 'accepted', '--json').stdout)
    assert report == [{'risk_tier': 'routine', 'harness_digest': None,
                       'runs': 1, 'accepted': 1, 'merged': 0, 'usage_rows': 0,
                       'unpriced_rows': 0, 'cost_usd_lower_bound': None,
                       'cost_per_accepted_usd_lower_bound': None, 'cost_basis': 'unlinked'}]
    table = cli(tmp_path, 'telemetry', 'accepted')
    assert table.returncode == 0 and 'unlinked' in table.stdout


def test_accepted_cost_joins_sibling_lanes_by_delivery_id_once_across_host_aliases(tmp_path):
    root = tmp_path / '.agent-run/runs'
    usage = dict(input_tokens=1_000_000, output_tokens=0, cache_read_tokens=0,
                 cache_write_tokens=0, reasoning_tokens=0)
    for name, attempt, delivery in [('writer', 1, 'delivery-a'), ('reviewer', 1, 'delivery-a'),
                                     ('retried', 1, 'delivery-b'), ('retried', 2, 'delivery-b'),
                                     ('unaccepted', 1, 'delivery-c'), ('unrelated', 1, None)]:
        receipt(tmp_path, name, attempt=attempt, status='ok',
                started_at='2026-10-21T00:00:00Z',
                retry_of='attempt-001' if attempt == 2 else None,
                provenance=route('agy', 'claude-opus-4-6-thinking'), usage=usage,
                **({'delivery_run_id': delivery} if delivery else {}))
    receipt(tmp_path, 'unpriced', status='ok', delivery_run_id='delivery-a',
            provenance=route('agy', 'unknown-model'), usage=usage)
    # The writer exists before RUN.json; authority and timestamps cannot supply this join.
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    for name in ('delivery-a', 'delivery-b', 'delivery-c'):
        write_json(root / name / 'RUN.json', {
            'run_id': name, 'risk_tier': 'substantial',
            'authority': {'allowed_source_paths': ['.']},
            'evidence': [{'started_at': '2026-10-22T00:00:00Z'}],
            **({'delivery_acceptance': {'basis': 'checks-and-review'}} if name != 'delivery-c' else {})})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        host = db.execute('SELECT host FROM usage LIMIT 1').fetchone()[0]
        db.execute("INSERT OR REPLACE INTO meta VALUES ('host_ids', ?)", (json.dumps([host, 'earlier-host']),))
        columns = [row[1] for row in db.execute('PRAGMA table_info(usage)')]
        values = ','.join("'earlier-host'" if column == 'host' else column for column in columns)
        db.execute(f'INSERT INTO usage SELECT {values} FROM usage')
        assert db.execute('PRAGMA user_version').fetchone() == (4,)
    report = json.loads(cli(tmp_path, 'telemetry', 'accepted', '--since', '2026-10-22', '--json').stdout)
    assert len(report) == 1
    row = report[0]
    assert (row['runs'], row['accepted'], row['usage_rows'], row['unpriced_rows']) == (3, 2, 6, 1)
    assert row['cost_basis'] == 'linked'
    assert row['cost_usd_lower_bound'] == 25
    assert row['cost_per_accepted_usd_lower_bound'] == 12.5


def test_delivery_link_migration_rereads_surviving_receipts_once(tmp_path):
    receipt(tmp_path, 'kept', status='ok', delivery_run_id='delivery:kept')
    pruned = receipt(tmp_path, 'pruned', status='ok', delivery_run_id='delivery:pruned')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    pruned.unlink()
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        db.execute('ALTER TABLE usage DROP COLUMN delivery_run_id')
        db.execute("DELETE FROM meta WHERE key='delivery_link_backfill'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT run_id,delivery_run_id FROM usage ORDER BY run_id').fetchall() == [
            ('kept', 'delivery:kept'), ('pruned', None)]
        assert db.execute('PRAGMA user_version').fetchone() == (4,)
        db.execute("UPDATE usage SET delivery_run_id='sentinel' WHERE run_id='kept'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute("SELECT delivery_run_id FROM usage WHERE run_id='kept'").fetchone() == ('sentinel',)


def test_delivery_link_backfill_uses_marker_when_column_already_exists(tmp_path):
    kept = receipt(tmp_path, 'kept', status='ok', delivery_run_id='delivery:kept')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        db.execute("UPDATE usage SET delivery_run_id=NULL WHERE run_id='kept'")
        db.execute("DELETE FROM meta WHERE key='delivery_link_backfill'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute("SELECT delivery_run_id FROM usage WHERE run_id='kept'").fetchone() == ('delivery:kept',)
        assert db.execute("SELECT 1 FROM meta WHERE key='delivery_link_backfill'").fetchone() is not None
        assert db.execute("UPDATE usage SET delivery_run_id='sentinel' WHERE run_id='kept'").rowcount == 1
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute("SELECT delivery_run_id FROM usage WHERE run_id='kept'").fetchone() == ('sentinel',)


def test_accepted_since_uses_run_time_after_backfill_and_receipt_update(tmp_path):
    root = tmp_path / '.agent-run/runs'
    old = root / '20261022-1200-delivery-old-a1b2c3/RUN.json'
    write_json(old, {'run_id': 'old', 'risk_tier': 'routine',
                     'authority': {'expires_at': '2019-01-01T00:00:00Z'},
                     'evidence': [{'started_at': '2020-01-01T00:00:00Z',
                                   'finished_at': '2020-01-01T00:02:00Z'}]})
    # Receipt mtimes deliberately disagree with the folder stamps in both directions.
    for name, stamp in [('20200101-1200-delivery-current-a1b2c3', 1792627200),
                        ('20261022-1201-delivery-stale-a1b2c3', 1577836800)]:
        path = root / name / 'RUN.json'
        write_json(path, {'run_id': name,
                          'risk_tier': 'routine' if 'current' in name else 'substantial'})
        os.utime(path, (stamp, stamp))
    for accepted in (False, True):
        receipt = json.loads(old.read_text())
        if accepted:
            receipt['delivery_acceptance'] = {'basis': 'user', 'recorded_at': '2026-10-22T00:00:00Z'}
            write_json(old, receipt)
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
        report = json.loads(cli(tmp_path, 'telemetry', 'accepted', '--since', '2026-01-01', '--json').stdout)
        assert [(row['risk_tier'], row['runs'], row['accepted']) for row in report] == [('routine', 1, 0)]
        # Existing ledgers need unchanged receipts re-read under the corrected fallback.
        with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
            db.execute("DELETE FROM meta WHERE key='delivery_run_time_v2'")
            db.execute('UPDATE deliveries SET started_at=NULL')
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
        report = json.loads(cli(tmp_path, 'telemetry', 'accepted', '--since', '2026-01-01', '--json').stdout)
        assert sum(row['runs'] for row in report) == 1
        assert sum(row['accepted'] for row in report) == 0


def test_accepted_groups_tiers_with_unknown_digest_without_double_counting(tmp_path):
    root = tmp_path / '.agent-run/runs'
    for name, tier, accepted in [('one', 'routine', True), ('two', 'routine', False),
                                 ('three', 'substantial', True)]:
        receipt = {'run_id': name, 'risk_tier': tier}
        if accepted:
            receipt['delivery_acceptance'] = {'basis': 'checks-and-review'}
        write_json(root / name / 'RUN.json', receipt)
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'telemetry', 'accepted', '--json').stdout)
    assert [(row['risk_tier'], row['harness_digest'], row['runs'], row['accepted'])
            for row in report] == [('routine', None, 2, 1), ('substantial', None, 1, 1)]


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
        assert db.execute('SELECT DISTINCT session,account,model,effort,revision FROM usage').fetchall() == [('cx', 'default', 'test-sol', 'high', 'abc')]
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


def test_opencode_v2_session_messages_are_ingested_once_alongside_migrated_messages(tmp_path):
    path = tmp_path / 'home/.local/share/opencode/opencode.db'
    path.parent.mkdir(parents=True)
    legacy = {'role': 'assistant', 'providerID': 'vendor', 'modelID': 'old-model', 'path': {'cwd': '/repo'},
              'time': {'created': 1792544400000}, 'tokens': {'input': 10, 'output': 2}, 'cost': 0.5}
    def turn(model, output, stamp):
        return json.dumps({'time': {'created': stamp}, 'model': {'providerID': 'vendor', 'id': model},
                           'tokens': {'input': 10, 'output': output, 'reasoning': 1, 'cache': {'read': 4, 'write': 0}},
                           'cost': 0.25, 'content': ['PRIVATE_V2']})
    with sqlite3.connect(path) as db:
        db.execute('CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT)')
        db.execute('CREATE TABLE session_v2(id TEXT PRIMARY KEY,directory TEXT NOT NULL)')
        db.execute('CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,seq INTEGER,'
                   'time_created INTEGER,time_updated INTEGER,data TEXT)')
        db.execute('INSERT INTO session_v2 VALUES (?,?)', ('s', '/repo'))
        # A pre-2.0.30 turn exists in both tables after migration; later turns only in session_message.
        db.execute('INSERT INTO message VALUES (?,?,?,?,?)', ('m1', 's', 1000, 1000, json.dumps(legacy)))
        db.executemany('INSERT INTO session_message VALUES (?,?,?,?,?,?,?)', [
            ('m1', 's', 'assistant', 1, 1000, 1000, json.dumps({**json.loads(turn('old-model', 2, 1792544400000))})),
            ('u2', 's', 'user', 2, 2000, 2000, json.dumps({'text': 'PRIVATE_PROMPT'})),
            ('m3', 's', 'assistant', 3, 3000, 3000, turn('new-model', 7, 1792630800000))])
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(path) as db:
        db.executemany('INSERT INTO session_message VALUES (?,?,?,?,?,?,?)', [
            ('m4', 's', 'assistant', 4, 4000, 4000, turn('new-model', 5, 1792630900000)),
            ('u5', 's', 'user', 5, 5000, 5000, json.dumps({'text': 'PRIVATE_PROMPT'}))])
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        # Skipped rows (migrated, or not assistant turns) still advance each table's watermark,
        # so a fully migrated `message` table is not rescanned on every ingest.
        state = json.loads(db.execute("SELECT state FROM cursors WHERE source='opencode'").fetchone()[0])
        assert state['tables'] == {'message': 1000, 'session_message': 5000}
        assert db.execute('SELECT event_id,provider,model,repository,output_tokens,cache_read_tokens,usage_source '
                          'FROM usage ORDER BY event_id').fetchall() == [
            ('m1', 'vendor', 'old-model', '/repo', 2, 4, 'session_message.tokens'),
            ('m3', 'vendor', 'new-model', '/repo', 7, 4, 'session_message.tokens'),
            ('m4', 'vendor', 'new-model', '/repo', 5, 4, 'session_message.tokens')]
        dump = '\n'.join(db.iterdump())
        assert 'PRIVATE_V2' not in dump and 'PRIVATE_PROMPT' not in dump


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


def test_stats_prices_preserve_missingness_and_bill_reasoning_once(tmp_path):
    from scripts import telemetry
    row = {'adapter': 'claude', 'model': 'synthetic', 'input_tokens': 1_000_000,
           'cache_read_tokens': 2_000_000, 'cache_write_tokens': 3_000_000,
           'output_tokens': 4_000_000, 'reasoning_tokens': 5_000_000}
    prices = {'claude/synthetic': {'usd_per_million': {
        'input': 2, 'cached_input': 0.2, 'cache_write': 3, 'output': 10}}}
    assert telemetry.costs_owner().estimated_cost(telemetry, row, prices) == 101.4
    assert telemetry.costs_owner().estimated_cost(telemetry, {**row, 'cache_write_tokens': None}, prices) is None
    assert telemetry.costs_owner().estimated_cost(telemetry, row, {'claude/synthetic': {'usd_per_million': {
        'input': 2, 'cached_input': 0.2, 'cache_write': None, 'output': 10}}}) is None
    assert telemetry.costs_owner().estimated_cost(telemetry, {**row, 'cache_write_tokens': 0}, {
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
    assert telemetry.costs_owner().estimated_cost(telemetry, row, {'opencode/opencode-go/synthetic': {
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


def test_legacy_terminal_failures_receive_error_classes(tmp_path):
    receipt(tmp_path, 'failed', status='failed', provenance=route('opencode', 'free-model'))
    receipt(tmp_path, 'limited', status='rate_limited', provenance=route('opencode', 'free-model'))
    receipt(tmp_path, 'queued', status='queued', provenance=route('opencode', 'free-model'))
    receipt(tmp_path, 'explicit', status='failed', error_class='auth', provenance=route('opencode', 'free-model'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    assert sorted(report['lanes'], key=lambda row: (row['status'], row['error_class'] or '')) == [
        {'status':'failed','error_class':'auth','lanes':1},
        {'status':'failed','error_class':'provider_error','lanes':1},
        {'status':'queued','error_class':None,'lanes':1},
        {'status':'rate_limited','error_class':'rate_limited','lanes':1},
    ]


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
                      'ended_at': datetime.now(UTC).isoformat(), **values})
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
    catalogue = telemetry.costs_owner().catalogue(telemetry)
    assert telemetry.costs_owner().normalise_model('opencode', 'opencode-go', 'deepseek-v4.1-flash', catalogue) == 'opencode-go/deepseek-v4.1-flash'
    assert telemetry.costs_owner().normalise_model('opencode', 'opencode', 'opencode-go/deepseek-v4.1-flash', catalogue) == 'opencode-go/deepseek-v4.1-flash'
    assert telemetry.costs_owner().normalise_model('opencode', 'openrouter', 'z-ai/glm-5.2:free', catalogue) == 'openrouter/z-ai/glm-5.2:free'
    assert telemetry.costs_owner().normalise_model('codex', 'openai', 'gpt-6-sol', catalogue) == 'gpt-6-sol'
    assert telemetry.costs_owner().normalise_model('codex', 'openai', None, catalogue) is None


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
    common = {'task_class': 'review', 'requested_route': {'alias': 'senior', 'task_class': 'review'},
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
        'route': 'senior', 'lanes': 2, 'succeeded': 2, 'success_rate': 1.0, 'attempts': 3,
        'retries': 1, 'error_classes': {'provider_timeout': 1},
        'known_cost_usd': 0, 'missing_cost_rows': 3, 'cost_per_success_usd': None,
        'latency_samples': 3, 'latency_p50_s': 20.0, 'latency_p90_s': 30.0, 'output_tokens_per_s': 7.5}]
    table = cli(tmp_path, 'stats').stdout
    assert 'codex/gpt-6.1-sol@high' in table and '100%' in table


def test_ingest_records_classifier_stage_confidence_and_shadow_answers(tmp_path):
    receipt(tmp_path, 'classified', status='ok', task_class='review', classified_by='rules',
            classifier_confidence=0.9, shadow_answers={'model': {'class': 'review', 'confidence': 0.7}},
            requested_route={'alias': 'principal', 'task_class': 'review'})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        row = db.execute('SELECT classified_by,classifier_confidence,shadow_answers FROM usage').fetchone()
    assert row == ('rules', 0.9, '{"model": {"class": "review", "confidence": 0.7}}')


def test_classifier_metadata_backfills_v4_receipts_once(tmp_path):
    kept = receipt(tmp_path, 'kept', status='ok', classified_by='rules', classifier_confidence=0.9,
                   shadow_answers={'model': None})
    pruned = receipt(tmp_path, 'pruned', status='ok')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    pruned.unlink()
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        for column in ('classified_by', 'classifier_confidence', 'shadow_answers'):
            db.execute(f'ALTER TABLE usage DROP COLUMN {column}')
        db.execute("DELETE FROM meta WHERE key='classifier_metadata_backfill'")
        db.execute('PRAGMA user_version = 4')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT run_id, classified_by, classifier_confidence, shadow_answers '
                          'FROM usage ORDER BY run_id').fetchall() == [
            ('kept', 'rules', 0.9, '{"model": null}'), ('pruned', None, None, None)]
        assert db.execute('PRAGMA user_version').fetchone() == (4,)
        cursor = db.execute("SELECT * FROM cursors WHERE path=?", (str(kept),)).fetchone()
        db.execute("UPDATE usage SET classified_by='sentinel' WHERE run_id='kept'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute("SELECT classified_by FROM usage WHERE run_id='kept'").fetchone() == ('sentinel',)
        assert db.execute("SELECT * FROM cursors WHERE path=?", (str(kept),)).fetchone() == cursor


def test_outcomes_report_api_cost_per_success_and_unknown_costs(tmp_path):
    def usage(input_tokens=1_000_000, output_tokens=1_000_000):
        return {'input_tokens': input_tokens, 'cache_read_tokens': 0, 'cache_write_tokens': 0,
                'output_tokens': output_tokens}

    # A retry is a separate API attempt and contributes to cost, while success
    # is counted once for the lane's latest attempt.
    for attempt, status in ((1, 'failed'), (2, 'ok')):
        receipt(tmp_path, 'priced-retry', attempt, status=status,
                provenance=route('codex', 'gpt-6-luna'), context=usage())
    receipt(tmp_path, 'priced-single', status='ok', provenance=route('codex', 'gpt-6-luna'),
            context=usage())
    receipt(tmp_path, 'unpriced', status='ok', provenance=route('codex', 'unlisted-model'),
            context=usage())
    receipt(tmp_path, 'failed-only', status='failed', provenance=route('codex', 'gpt-6-luna'),
            requested_route={'task_class': 'failed-only'}, context=usage())

    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    report = json.loads(cli(tmp_path, 'stats', '--json').stdout)
    outcomes = {(row['model'], row['task_class'], row['route']): row for row in report['outcomes']}
    priced = outcomes[('gpt-6-luna', None, None)]
    assert priced['attempts'] == 3 and priced['succeeded'] == 2
    assert priced['known_cost_usd'] == pytest.approx(1.8)
    assert priced['missing_cost_rows'] == 0
    assert priced['cost_per_success_usd'] == pytest.approx(0.9)
    unpriced = outcomes[('unlisted-model', None, None)]
    assert unpriced['missing_cost_rows'] == 1 and unpriced['cost_per_success_usd'] is None
    failed = outcomes[('gpt-6-luna', 'failed-only', None)]
    assert failed['succeeded'] == 0 and failed['cost_per_success_usd'] is None

    table = cli(tmp_path, 'stats').stdout
    assert 'cost_usd' in table and '$/ok' in table
    assert '1.80' in table and '0.90' in table


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


def test_host_id_prefers_the_instance_config_over_home_and_hostname(tmp_path, monkeypatch):
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_host_precedence')
    monkeypatch.setenv('HOME', str(tmp_path / 'home'))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setattr(__import__('socket'), 'gethostname', lambda: 'runner-ci')
    write_json(tmp_path / 'home/.agents/.agent-fabric/hosts.json',
               {'schema_version': 1, 'local_host': 'home-host'})
    write_json(tmp_path / 'instance/.agent-fabric/hosts.json',
               {'schema_version': 1, 'local_host': 'studio'})
    assert telemetry.host_id() == 'studio'


def test_peer_merge_deduplicates_sessions_and_uses_freshest_account_quota(tmp_path, monkeypatch):
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'merge_telemetry_test')
    federation = load_script(ROOT / 'scripts/telemetry_hosts.py', 'merge_federation_test')
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    with telemetry.connect() as db:
        first = telemetry.report_owner().peer_summary(telemetry, db, host='studio')
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
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'bad_peer_telemetry')
    federation = load_script(ROOT / 'scripts/telemetry_hosts.py', 'bad_peer_federation')
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    with telemetry.connect() as db:
        good = telemetry.report_owner().peer_summary(telemetry, db, host='peer')
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
    received = datetime.now(UTC) - timedelta(hours=1)
    return {'received_at': received.strftime('%Y-%m-%dT%H:%M:%SZ'), 'signal': signal, 'body': body}


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
    assert (session['session'], session['ts']) == ('chair', '2026-10-21T02:00:00Z')  # first ingested row
    assert session['source_ref'].endswith('session.jsonl')
    for private in ('PRIVATE_ERROR', 'PRIVATE_MESSAGE', 'PRIVATE_PROMPT', 'PRIVATE_EMAIL'):
        assert private not in dump
    sources = {row['source']: row for row in json.loads(cli(tmp_path, 'telemetry', 'sources', '--json').stdout)}
    assert sources['otel']['status'] == 'ok' and sources['otel']['row_count'] == 3
    # An older ledger without events re-reads receipts and snapshots once to backfill them.
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        db.execute('DELETE FROM events')
        db.execute("DELETE FROM meta WHERE key IN ('events_backfill', 'session_events_backfill')")
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
    return subprocess.run([str(front_door_binary()), *args], env=env, cwd=tmp_path,
                          capture_output=True, text=True)


def pull(number, merged_at, title='Change', labels=()):
    return {'number': number, 'mergeCommit': {'oid': f'{number:07d}abcdef'}, 'mergedAt': merged_at,
            'title': title, 'labels': [{'name': name} for name in labels]}


def lanes_around(project, prefix, count, ok, day):
    for index in range(count):
        receipt(project, f'{prefix}-{index:02d}', 1, status='ok' if index < ok else 'failed',
                error_class=None if index < ok else 'provider_timeout',
                provenance=route('codex', 'gpt-6.1-sol', 'high'),
                started_at=f'{day}T01:00:00Z', ended_at=f'{day}T01:00:{10 + index:02d}Z',
                context={'input_tokens': 100, 'cached_input_tokens': 50, 'cache_write_tokens': 0,
                         'output_tokens': 10, 'reasoning_tokens': 0})


def test_session_start_survives_transcript_replacement(tmp_path):
    """Regression: a rewritten, smaller transcript must still announce its new session."""
    path = tmp_path / 'home/.claude/projects/repo/session.jsonl'
    def message(session, index):
        return {'type': 'assistant', 'sessionId': session, 'timestamp': f'2026-10-21T0{index}:00:00Z',
                'message': {'id': f'{session}-m{index}', 'model': 'claude-test',
                            'usage': {'input_tokens': 1, 'output_tokens': 1}}}
    jsonl(path, message('s1', 1), message('s1', 2), message('s1', 3))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    path.write_text(json.dumps(message('s2', 4)) + '\n')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        sessions = dict(db.execute("SELECT session, ts FROM events WHERE kind='session.start'"))
    assert sessions == {'s1': '2026-10-21T01:00:00Z', 's2': '2026-10-21T04:00:00Z'}


def test_event_ids_survive_a_host_rename(tmp_path):
    """Regression: renaming the host and re-reading sources must not duplicate events."""
    hosts = tmp_path / 'home/.agents/.agent-fabric/hosts.json'
    write_json(hosts, {'schema_version': 1, 'local_host': 'studio'})
    path = receipt(tmp_path, 'run-h', 1, status='usage_limited', account='work',
                   provenance=route('codex', 'gpt-6.1-sol'))
    write_json(tmp_path / 'state/cooldowns.json', {'cooldowns': {'codex/gpt-6.1-sol': {
        'adapter': 'codex', 'model': 'gpt-6.1-sol', 'account': 'work', 'status': 'usage_limited',
        'recorded_at': '2026-10-21T01:00:00Z', 'cooling_until': '2026-10-21T05:00:00Z'}}})
    session = tmp_path / 'home/.claude/projects/repo/session.jsonl'
    def message(index):
        return {'type': 'assistant', 'sessionId': 'chair', 'timestamp': f'2026-10-21T0{index}:00:00Z',
                'message': {'id': f'm{index}', 'model': 'claude-test',
                            'usage': {'input_tokens': 1, 'output_tokens': 1}}}
    jsonl(session, message(1))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    database = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(database) as db:
        before = sorted(db.execute('SELECT kind, COUNT(*) FROM events GROUP BY kind'))
    assert {kind for kind, _ in before} >= {'lane.attempt', 'session.start', 'cooldown'}
    write_json(hosts, {'schema_version': 1, 'local_host': 'renamed'})
    path.write_text(path.read_text() + '\n')
    os.utime(path, (2_000_000_000, 2_000_000_000))
    jsonl(session, message(2))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(database) as db:
        assert sorted(db.execute('SELECT kind, COUNT(*) FROM events GROUP BY kind')) == before


def compare_lanes(project, prefix, count, day, model='gpt-6.1-sol', timed=None, cached=None):
    """Lanes on one day; only the first `timed` carry a start time, the first `cached` full cache fields."""
    for index in range(count):
        context = {'input_tokens': 100, 'output_tokens': 10, 'reasoning_tokens': 0}
        if cached is None or index < cached:
            context.update(cached_input_tokens=50, cache_write_tokens=0)
        start = {} if timed is not None and index >= timed else {'started_at': f'{day}T01:00:00Z'}
        receipt(project, f'{prefix}-{index:02d}', 1, status='ok', provenance=route('codex', model, 'high'),
                ended_at=f'{day}T01:00:{10 + index:02d}Z', context=context, **start)


def compare_report(tmp_path, pivot='2026-10-10'):
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    result = cli(tmp_path, 'stats', '--compare', pivot, '--window', '7d', '--json')
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    return report, {row['metric']: row for row in report['metrics']}


def test_compare_never_reads_unpriced_rows_as_savings(tmp_path):
    """Regression: an unpriced model's $0 must not make cost/day look improved."""
    compare_lanes(tmp_path, 'before', 12, '2026-10-08')
    compare_lanes(tmp_path, 'after', 10, '2026-10-11')
    compare_lanes(tmp_path, 'unpriced', 4, '2026-10-11', model='unpriced-model')
    report, metrics = compare_report(tmp_path)
    cost = metrics['cost_usd_per_day']
    assert (cost['n_before'], cost['n_after'], cost['sample']) == (12, 10, 'priced rows')
    assert report['after']['cost_usd_per_day'] < 0.9 * report['before']['cost_usd_per_day']
    assert cost['verdict'] == 'inconclusive'
    assert (report['before']['cost_coverage'], report['after']['cost_coverage']) == (1.0, 0.714)
    assert report['after']['missing_cost_rows'] == 4
    text = cli(tmp_path, 'stats', '--compare', '2026-10-10').stdout
    assert '12/10 priced rows' in text


def test_compare_counts_the_observations_behind_each_metric(tmp_path):
    """Regression: latency and cache n are timed attempts and complete rows, not lanes or usage rows."""
    compare_lanes(tmp_path, 'before', 12, '2026-10-08')
    compare_lanes(tmp_path, 'after', 12, '2026-10-11', timed=1, cached=3)
    report, metrics = compare_report(tmp_path)
    assert (metrics['success_rate']['n_before'], metrics['success_rate']['n_after']) == (12, 12)
    assert metrics['success_rate']['verdict'] == 'within_noise'
    latency, cache = metrics['latency_p50_s'], metrics['cache_read_share']
    assert (latency['n_before'], latency['n_after'], latency['sample']) == (12, 1, 'timed attempts')
    assert (cache['n_before'], cache['n_after'], cache['sample']) == (12, 3, 'complete rows')
    assert latency['after'] is not None and cache['after'] is not None
    assert latency['verdict'] == cache['verdict'] == 'inconclusive'
    assert report['after']['samples']['limit_events_per_day'] == 12


FILTERING_GH = """#!{python}
import json, pathlib, re, sys
from datetime import datetime
bin_dir = pathlib.Path({bin_dir!r})
with (bin_dir / 'calls.jsonl').open('a') as stream:
    stream.write(json.dumps(sys.argv[1:]) + '\\n')
args = sys.argv[1:]
limit = int(args[args.index('--limit') + 1])
search = args[args.index('--search') + 1] if '--search' in args else ''
when = lambda value: datetime.fromisoformat(value.replace('Z', '+00:00'))
rows = json.loads((bin_dir / 'prs.json').read_text())
for op, bound in re.findall(r'merged:(>=|<=)(\\S+)', search):
    rows = [row for row in rows if when(row['mergedAt']) == when(bound)
            or (when(row['mergedAt']) > when(bound)) == (op == '>=')]
print(json.dumps(sorted(rows, key=lambda row: row['mergedAt'])[:limit]))
"""


def filtering_gh(tmp_path, pulls):
    """A `gh` stub that honours --limit and merged:>= / merged:<=, returning the OLDEST matches first,
    so a full page silently drops the newest PRs as an unsorted search can."""
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir(exist_ok=True)
    (bin_dir / 'prs.json').write_text(json.dumps(pulls))
    script = bin_dir / 'gh'
    script.write_text(FILTERING_GH.format(python=sys.executable, bin_dir=str(bin_dir)))
    script.chmod(0o755)
    return bin_dir


def hourly_pulls(count, start='2026-09-01T00:00:00'):  # timebomb: allow sync reads SYNC_FLOOR..now, a past date never expires
    from datetime import datetime, timedelta
    first = datetime.fromisoformat(start)
    return [pull(1 + index, (first + timedelta(hours=index)).isoformat() + 'Z') for index in range(count)]


def test_capped_changes_sync_resumes_its_pending_ranges(tmp_path, monkeypatch):
    """Regression: a sweep stopped by the query cap or a gh failure resumes without losing PRs."""
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'sync_telemetry_test')
    owner = load_script(ROOT / 'scripts/telemetry_changes.py', 'sync_changes_test')
    monkeypatch.setattr(owner, 'SYNC_LIMIT', 5)
    monkeypatch.setattr(owner, 'SYNC_PAGES', 2)
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    bin_dir = filtering_gh(tmp_path, hourly_pulls(12))
    monkeypatch.setenv('PATH', os.pathsep.join((str(bin_dir), os.environ['PATH'])))
    with telemetry.connect() as db:
        added, pending, _ = owner.sync(telemetry, db)
        assert pending > 0 and added < 12
        (bin_dir / 'gh').rename(bin_dir / 'gh-real')
        (bin_dir / 'gh').write_text('#!/bin/sh\nexit 3\n')
        (bin_dir / 'gh').chmod(0o755)
        try:
            owner.sync(telemetry, db)
        except owner.ChangeError:
            pass
        else:
            raise AssertionError('a failing gh must raise')
        (bin_dir / 'gh-real').replace(bin_dir / 'gh')
        state = json.loads(db.execute("SELECT value FROM meta WHERE key='changes_sync'").fetchone()[0])
        assert len(state['pending']) == pending and state['through'] is None
        runs = 1
        while pending:
            added, pending, _ = owner.sync(telemetry, db)
            runs += 1
            assert runs < 20
        assert db.execute("SELECT COUNT(*) FROM changes WHERE kind='pr'").fetchone()[0] == 12
        state = json.loads(db.execute("SELECT value FROM meta WHERE key='changes_sync'").fetchone()[0])
        assert state['pending'] == [] and state['through']


def test_fit_keeps_whole_lines_within_the_byte_budget():
    owner = load_script(ROOT / 'scripts/telemetry_changes.py', 'fit_changes_test')
    assert owner.fit(['short', 'lines']) == 'short\nlines'
    text = owner.fit([f'{index} ' + '🚀' * 200 for index in range(10)])
    assert len(text.encode()) <= owner.BUDGET and text.endswith('lines omitted to fit 4 KB; use --json')
    assert all(line.startswith(str(index)) for index, line in enumerate(text.splitlines()[:-1]))


def use_environment(tmp_path, monkeypatch):
    for key, value in environment(tmp_path).items():
        if key != 'PATH':
            monkeypatch.setenv(key, value)
    monkeypatch.chdir(tmp_path)  # fabric receipts are discovered from the working directory


def test_peer_outcomes_carry_latency_samples_and_old_peers_still_validate(tmp_path, monkeypatch):
    """Regression: the per-metric latency n must cross the peer boundary without rejecting summaries."""
    use_environment(tmp_path, monkeypatch)
    receipt(tmp_path, 'timed', 1, status='ok', started_at='2026-10-21T00:59:00Z',
            provenance=route('codex', 'gpt-6.1-sol'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'peer_outcome_telemetry')
    hosts = load_script(ROOT / 'scripts/telemetry_hosts.py', 'peer_outcome_hosts')
    with telemetry.connect() as db:
        summary = telemetry.report_owner().peer_summary(telemetry, db, host='peer')
    assert summary['outcomes'] and summary['outcomes'][0]['latency_samples'] == 1
    rendered = io.StringIO()
    with contextlib.redirect_stdout(rendered):
        telemetry.report_owner().render_stats(telemetry, summary)
    assert 'Outcomes by route' in rendered.getvalue()
    assert 'cost_usd' in rendered.getvalue() and '$/ok' in rendered.getvalue()
    hosts.validate(telemetry, json.loads(json.dumps(summary)), 'peer')
    old = json.loads(json.dumps(summary))
    for row in old['outcomes']:
        del row['latency_samples']
    hosts.validate(telemetry, old, 'peer')
    bad = json.loads(json.dumps(summary))
    bad['outcomes'][0]['latency_samples'] = 'many'
    try:
        hosts.validate(telemetry, bad, 'peer')
    except ValueError:
        pass
    else:
        raise AssertionError('a non-count latency_samples must be rejected')


def test_ledgers_backfilled_before_session_events_get_them_once(tmp_path):
    """Regression: an existing `events_backfill` marker must not skip the session.start repair."""
    jsonl(tmp_path / 'home/.claude/projects/repo/session.jsonl', {
        'type': 'assistant', 'sessionId': 'missed', 'timestamp': '2026-10-21T01:00:00Z',
        'message': {'id': 'm1', 'model': 'claude-test', 'usage': {'input_tokens': 1, 'output_tokens': 1}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    database = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(database) as db:  # the shape an earlier build left: marker set, session event missing
        db.execute("DELETE FROM events WHERE kind='session.start'")
        db.execute("DELETE FROM meta WHERE key='session_events_backfill'")
        assert db.execute("SELECT 1 FROM meta WHERE key='events_backfill'").fetchone()
    for _ in range(2):
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(database) as db:
        assert db.execute("SELECT session FROM events WHERE kind='session.start'").fetchall() == [('missed',)]


def test_saturated_one_second_ranges_page_further_and_stay_marked_incomplete(tmp_path, monkeypatch):
    """Regression: more merges in one second than a page holds must not be silently dropped."""
    use_environment(tmp_path, monkeypatch)
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'saturated_telemetry')
    owner = load_script(ROOT / 'scripts/telemetry_changes.py', 'saturated_changes')
    monkeypatch.setattr(owner, 'SYNC_LIMIT', 5)
    same = [pull(700 + index, '2026-10-05T12:00:00Z', f'Batch {index}') for index in range(7)]
    bin_dir = filtering_gh(tmp_path, [*same, *hourly_pulls(3)])
    monkeypatch.setenv('PATH', os.pathsep.join((str(bin_dir), os.environ['PATH'])))
    monkeypatch.setattr(owner, 'SEARCH_LIMIT', 10)
    with telemetry.connect() as db:
        added, pending, incomplete = owner.sync(telemetry, db)
        assert (added, pending, incomplete) == (10, 0, 0)
    # When even the search cap is full, the range is kept and reported as incomplete.
    (tmp_path / 'state/telemetry.sqlite3').unlink()
    monkeypatch.setattr(owner, 'SEARCH_LIMIT', 6)
    with telemetry.connect() as db:
        added, pending, incomplete = owner.sync(telemetry, db)
        assert (pending, incomplete) == (0, 1)
        state = json.loads(db.execute("SELECT value FROM meta WHERE key='changes_sync'").fetchone()[0])
        low, high = state['incomplete'][0]
        assert low <= '2026-10-05T12:00:00Z' <= high
        assert owner.sync(telemetry, db)[2] == 1  # it stays marked on later syncs


def test_fit_keeps_whole_long_lines_that_fit():
    """Regression: a line that fits is never clipped; lines that do not are counted, not cut."""
    owner = load_script(ROOT / 'scripts/telemetry_changes.py', 'fit_whole_changes')
    long = 'x' * 1500
    assert owner.fit(['head', long, 'tail']) == f'head\n{long}\ntail'
    text = owner.fit(['head', 'y' * 5000, 'tail'])
    assert text == 'head\ntail\n1 lines omitted to fit 4 KB; use --json'


def test_upgrade_reconciles_events_from_relabelled_and_mixed_namespaces(tmp_path):
    """Regression: event rows keyed under any earlier namespace, including a host label changed
    before the upgrade, reconcile by their stored identity instead of duplicating on re-ingest."""
    hosts = tmp_path / 'home/.agents/.agent-fabric/hosts.json'
    write_json(hosts, {'schema_version': 1, 'local_host': 'studio'})
    receipt(tmp_path, 'run-m', 1, status='ok', provenance=route('codex', 'gpt-6.1-sol'))
    write_json(tmp_path / 'state/cooldowns.json', {'cooldowns': {'codex/gpt-6.1-sol': {
        'adapter': 'codex', 'model': 'gpt-6.1-sol', 'account': 'work', 'status': 'usage_limited',
        'recorded_at': '2026-10-21T01:00:00Z', 'cooling_until': '2026-10-21T05:00:00Z'}}})
    jsonl(tmp_path / 'home/.claude/projects/repo/session.jsonl', {
        'type': 'assistant', 'sessionId': 'chair', 'timestamp': '2026-10-21T01:00:00Z',
        'message': {'id': 'm1', 'model': 'claude-test', 'usage': {'input_tokens': 1, 'output_tokens': 1}}})
    jsonl(tmp_path / 'state/otel/metrics-20261021.ndjson', otlp_line('metrics', {'resourceMetrics': [{
        'resource': {'attributes': []}, 'scopeMetrics': [{'metrics': [{
            'name': 'claude_code.active_time.total', 'sum': {'aggregationTemporality': 1, 'dataPoints': [
                {'asDouble': 90.0, 'timeUnixNano': '1792544400000000000',
                 'attributes': otlp_attributes(type='user', session__id='chair')},
                {'asDouble': 30.0, 'timeUnixNano': '1792544400000000000',
                 'attributes': otlp_attributes(type='cli', session__id='chair')}]}}]}]}]}))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    database = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(database) as db:
        truth = sorted(db.execute('SELECT kind, COUNT(*) FROM events GROUP BY kind'))
        assert dict(truth) == {'cooldown': 1, 'lane.attempt': 1, 'otel.metric': 2, 'session.start': 1}
        # Rewrite the ledger as an earlier build left it: IDs hashed under two old host labels
        # (one changed before the upgrade), a duplicate from that relabel, and no re-key marker.
        rows = db.execute('SELECT rowid, kind FROM events ORDER BY rowid').fetchall()
        for position, (rowid, kind) in enumerate(rows):
            label = 'old-studio' if position % 2 else 'older-name'
            db.execute('UPDATE events SET event_id=? WHERE rowid=?', (f'{label}:{kind}:{rowid}', rowid))
        db.execute("INSERT INTO events SELECT 'relabel-dup', host, kind, ts, session, run_id, lane, task_id, pr, "
                   "provider, account, model, harness_rev, attrs, source_ref FROM events WHERE kind='lane.attempt'")
        db.execute("DELETE FROM meta WHERE key IN ('event_ids', 'ledger_id')")
        db.execute("UPDATE meta SET value='old-studio' WHERE key='host_id'")
        db.execute('DELETE FROM cursors')  # every source is read again
    write_json(hosts, {'schema_version': 1, 'local_host': 'renamed'})
    for _ in range(2):
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(database) as db:
        assert sorted(db.execute('SELECT kind, COUNT(*) FROM events GROUP BY kind')) == truth


def test_a_full_two_second_range_splits_before_it_is_called_saturated(tmp_path, monkeypatch):
    """Regression: [T, T+1s] holds two timestamps; split it before reading deeper or warning."""
    use_environment(tmp_path, monkeypatch)
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'two_second_telemetry')
    owner = load_script(ROOT / 'scripts/telemetry_changes.py', 'two_second_changes')
    monkeypatch.setattr(owner, 'SYNC_LIMIT', 5)
    monkeypatch.setattr(owner, 'SEARCH_LIMIT', 6)
    pulls = [pull(800 + index, f'2026-10-05T12:00:0{index % 2}Z', f'Burst {index}') for index in range(8)]
    bin_dir = filtering_gh(tmp_path, pulls)
    monkeypatch.setenv('PATH', os.pathsep.join((str(bin_dir), os.environ['PATH'])))
    with telemetry.connect() as db:
        state = {'through': None, 'sweep': '2026-10-06T00:00:00Z', 'incomplete': [],
                 'pending': [['2026-10-05T12:00:00Z', '2026-10-05T12:00:01Z']]}
        db.execute("INSERT INTO meta VALUES ('changes_sync', ?)", (json.dumps(state),))
        added, pending, incomplete = owner.sync(telemetry, db)
        assert (added, pending, incomplete) == (8, 0, 0)


def test_lane_attempts_in_different_projects_with_the_same_run_ids_stay_distinct(tmp_path):
    """Regression: the receipt's own location is part of a lane.attempt's identity."""
    other = tmp_path / 'other-project'
    registry = tmp_path / 'state/fabric.sqlite3'
    registry.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(registry) as db:
        db.execute('CREATE TABLE agents (project TEXT)')
        db.execute('INSERT INTO agents VALUES (?)', (str(other),))
    for project in (tmp_path, other):
        receipt(project, 'run-same', 1, status='ok', provenance=route('codex', 'gpt-6.1-sol'))
    for _ in range(2):
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        refs = [row[0] for row in db.execute("SELECT source_ref FROM events WHERE kind='lane.attempt'")]
        assert len(refs) == 2 and len(set(refs)) == 2
        db.execute("DELETE FROM meta WHERE key='event_ids'")  # a later re-key keeps both
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute("SELECT COUNT(*) FROM events WHERE kind='lane.attempt'").fetchone()[0] == 2


def test_identical_collector_records_on_one_line_keep_their_multiplicity(tmp_path):
    """Regression: each record's position on its collector line is part of its identity."""
    point = {'asDouble': 5.0, 'timeUnixNano': '1792544400000000000',
             'attributes': otlp_attributes(type='user', session__id='chair')}
    otel = tmp_path / 'state/otel/metrics-20261021.ndjson'
    jsonl(otel, otlp_line('metrics', {'resourceMetrics': [{'resource': {'attributes': []}, 'scopeMetrics': [
        {'metrics': [{'name': 'claude_code.active_time.total',
                      'sum': {'aggregationTemporality': 1, 'dataPoints': [point, point, point]}}]}]}]}))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    database = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(database) as db:
        assert db.execute("SELECT COUNT(*) FROM events WHERE kind='otel.metric'").fetchone()[0] == 3
        # An earlier build stored refs without the record position; the re-key restores it in order.
        db.execute("UPDATE events SET source_ref=substr(source_ref, 1, instr(source_ref, '#') - 1), "
                   "event_id='legacy-' || rowid WHERE kind='otel.metric'")
        db.execute("DELETE FROM meta WHERE key='event_ids'")
        db.execute("DELETE FROM cursors WHERE source='otel'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(database) as db:
        assert db.execute("SELECT COUNT(*) FROM events WHERE kind='otel.metric'").fetchone()[0] == 3


CONCURRENT_UPGRADE = """
import importlib.util, sys, time
spec = importlib.util.spec_from_file_location('upgrade', sys.argv[1])
telemetry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(telemetry)
original, slept = telemetry.event_key, []
def slow(row):
    if not slept:  # widen the window between reading old rows and re-keying them
        slept.append(1)
        time.sleep(1.0)
    return original(row)
telemetry.event_key = slow
telemetry.connect().close()
"""


def test_concurrent_upgrades_re_key_events_once_without_losing_rows(tmp_path):
    """Regression: two processes opening an old ledger together must not delete each other's re-keyed rows."""
    receipt(tmp_path, 'run-c', 1, status='ok', provenance=route('codex', 'gpt-6.1-sol'))
    write_json(tmp_path / 'state/cooldowns.json', {'cooldowns': {'codex/gpt-6.1-sol': {
        'adapter': 'codex', 'model': 'gpt-6.1-sol', 'account': 'work', 'status': 'usage_limited',
        'recorded_at': '2026-10-21T01:00:00Z', 'cooling_until': '2026-10-21T05:00:00Z'}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    database = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(database) as db:
        truth = sorted(db.execute('SELECT kind, COUNT(*) FROM events GROUP BY kind'))
        db.execute("UPDATE events SET event_id='legacy-' || rowid")
        db.execute("DELETE FROM meta WHERE key='event_ids'")
    script = tmp_path / 'upgrade.py'
    script.write_text(CONCURRENT_UPGRADE)
    env = environment(tmp_path)
    workers = [subprocess.Popen([sys.executable, str(script), str(ROOT / 'scripts/telemetry.py')], env=env,
                                cwd=tmp_path, stderr=subprocess.PIPE, text=True) for _ in range(2)]
    for worker in workers:
        _, stderr = worker.communicate(timeout=60)
        assert worker.returncode == 0, stderr
    with sqlite3.connect(database) as db:
        assert sorted(db.execute('SELECT kind, COUNT(*) FROM events GROUP BY kind')) == truth


def test_ledger_from_main_format_replays_collector_lines_with_their_record_positions(tmp_path):
    """A ledger written by main (no events table, no collector source) upgrades and replays
    [A, A, B] on one line as three events, keyed by each record's own position."""
    a = {'asDouble': 5.0, 'timeUnixNano': '1792544400000000000',
         'attributes': otlp_attributes(type='user', session__id='chair')}
    b = {**a, 'asDouble': 7.0}
    jsonl(tmp_path / 'state/otel/metrics-20261021.ndjson', otlp_line('metrics', {'resourceMetrics': [{
        'resource': {'attributes': []}, 'scopeMetrics': [{'metrics': [{
            'name': 'claude_code.active_time.total',
            'sum': {'aggregationTemporality': 1, 'dataPoints': [a, a, b]}}]}]}]}))
    jsonl(tmp_path / 'home/.claude/projects/repo/session.jsonl', {
        'type': 'assistant', 'sessionId': 'chair', 'timestamp': '2026-10-21T01:00:00Z',
        'message': {'id': 'm1', 'model': 'claude-test', 'usage': {'input_tokens': 1, 'output_tokens': 1}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    database = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(database) as db:  # reduce the ledger to what main writes
        for table in ('events', 'changes'):
            db.execute(f'DROP TABLE {table}')
        db.execute("DELETE FROM meta WHERE key IN ('event_ids', 'events_backfill', 'session_events_backfill')")
        db.execute("DELETE FROM cursors WHERE source='otel'")
        db.execute("DELETE FROM sources WHERE source='otel'")
    for _ in range(2):
        assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(database) as db:
        rows = db.execute("SELECT source_ref, attrs FROM events WHERE kind='otel.metric' ORDER BY source_ref").fetchall()
        assert [ref.rsplit('#', 1)[1] for ref, _ in rows] == ['1', '2', '3']
        assert [json.loads(attrs)['value'] for _, attrs in rows] == [5.0, 5.0, 7.0]
        assert db.execute("SELECT COUNT(*) FROM events WHERE kind='session.start'").fetchone()[0] == 1


def claude_turn(session, message, stamp='2026-10-21T01:00:00Z'):
    return {'type': 'assistant', 'sessionId': session, 'timestamp': stamp,
            'message': {'id': message, 'model': 'claude-test', 'usage': {'input_tokens': 1, 'output_tokens': 1}}}


def test_claude_sessions_in_every_identity_dir_carry_their_account_and_relabel_on_alias(tmp_path):
    home = tmp_path / 'home'
    jsonl(home / '.claude/projects/repo/a.jsonl', claude_turn('a', 'ma'))
    jsonl(home / '.claude-michael/projects/repo/m.jsonl', claude_turn('m', 'mm'))
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'claude': {
        'michael': {'identity_dir': str(home / '.claude-michael'), 'plan': 'Max'}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT session,account FROM usage ORDER BY session').fetchall() == [
            ('a', 'default'), ('m', 'michael')]
    # Declaring the default directory as another sign-in of `anese` relabels rows already stored.
    jsonl(home / '.claude-anese/projects/repo/n.jsonl', claude_turn('n', 'mn'))
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'claude': {
        'anese': {'identity_dir': str(home / '.claude-anese'), 'aliases': [str(home / '.claude')]},
        'michael': {'identity_dir': str(home / '.claude-michael')}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT session,account FROM usage ORDER BY session').fetchall() == [
            ('a', 'anese'), ('m', 'michael'), ('n', 'anese')]
    sources = {row['source']: row for row in json.loads(cli(tmp_path, 'telemetry', 'sources', '--json').stdout)}
    assert '.claude-anese/projects' in sources['claude']['path_pattern'] and sources['claude']['row_count'] == 3


def test_codex_rows_observations_and_session_events_relabel_together(tmp_path):
    home = tmp_path / 'home'
    path = home / '.codex/sessions/day/rollout-a.jsonl'
    jsonl(path, {'type': 'session_meta', 'payload': {'id': 'cx', 'creator_account_id': 'opaque-id'}},
          {'type': 'event_msg', 'timestamp': '2026-10-21T01:00:00Z', 'payload': {
              'type': 'token_count', 'info': {'last_token_usage': {'input_tokens': 10}},
              'rate_limits': {'primary': {'used_percent': 5, 'window_minutes': 300}, 'plan_type': 'plus'}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    ledger = tmp_path / 'state/telemetry.sqlite3'
    def accounts():
        with sqlite3.connect(ledger) as db:
            return {table: {row[0] for row in db.execute(query)} for table, query in (
                ('usage', 'SELECT account FROM usage'), ('observations', 'SELECT account FROM observations'),
                ('session', "SELECT account FROM events WHERE kind='session.start'"))}
    assert accounts() == dict.fromkeys(('usage', 'observations', 'session'), {'default'})  # never the provider id
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'codex': {
        'work': {'identity_dir': str(home / '.codex-work'), 'aliases': [str(home / '.codex')]}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0  # the file is unchanged, so only relabelling moves it
    assert accounts() == dict.fromkeys(('usage', 'observations', 'session'), {'work'})


def test_a_nested_identity_directory_owns_its_files_in_ingest_and_relabelling(tmp_path):
    home = tmp_path / 'home'
    jsonl(home / '.claude/projects/repo/outer.jsonl', claude_turn('outer', 'mo'))
    jsonl(home / '.claude/projects/nested/projects/repo/inner.jsonl', claude_turn('inner', 'mi'))
    outer = {'outer': {'identity_dir': str(home / '.claude')}}
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'claude': outer}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT session, account FROM usage ORDER BY session').fetchall() == [
            ('inner', 'outer'), ('outer', 'outer')]
    # Declaring the nested directory moves only its files, although neither file changed.
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'claude': {
        **outer, 'inner': {'identity_dir': str(home / '.claude/projects/nested')}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT session, account FROM usage ORDER BY session').fetchall() == [
            ('inner', 'inner'), ('outer', 'outer')]


def test_transcript_names_that_link_elsewhere_are_never_opened(tmp_path):
    home = tmp_path / 'home'
    secret = home / '.codex/auth.json'
    secret.parent.mkdir(parents=True)
    secret.write_text('{"tokens": {"access_token": "SECRET-TOKEN-VALUE"}}\n')
    (home / '.codex/sessions/day').mkdir(parents=True)
    (home / '.codex/sessions/day/rollout-link.jsonl').symlink_to(secret)
    elsewhere = tmp_path / 'elsewhere'
    jsonl(elsewhere / 'repo/s.jsonl', claude_turn('linked', 'ml'))
    (home / '.claude/projects').mkdir(parents=True)
    (home / '.claude/projects/linked').symlink_to(elsewhere / 'repo')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT COUNT(*) FROM usage').fetchone() == (0,)
        assert db.execute('SELECT COUNT(*) FROM cursors').fetchone() == (0,)


def test_lane_events_carry_the_harness_profile_when_the_receipt_has_one(tmp_path):
    stamp = datetime.now(UTC).replace(microsecond=0).isoformat().replace('+00:00', 'Z')
    receipt(tmp_path, 'run-p', 1, status='ok', provenance=route('codex', 'gpt-6.1-sol'),
            ended_at=stamp,
            profile={'preset': 'thorough', 'digest': 'd1', 'knobs': {'reviewers': 3},
                     'risk_tier': 'substantial', 'floored': ['reviewers']})
    receipt(tmp_path, 'run-q', 1, status='ok', provenance=route('codex', 'gpt-6.1-sol'))
    receipt(tmp_path, 'run-r', status='ok', ended_at=stamp,
            profile={'preset': 'thorough', 'digest': 'd1',
                                                  'knobs': {'reviewers': 3}})
    receipt(tmp_path, 'broken', status='ok', profile={'digest': 'd2'})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        rows = dict(db.execute("SELECT run_id, attrs FROM events WHERE kind='lane.attempt'").fetchall())
        stamps = db.execute('SELECT digest, preset, knobs, first_seen FROM harness_stamps').fetchall()
    assert len(stamps) == 1
    assert stamps[0][:2] == ('d1', 'thorough')
    assert json.loads(stamps[0][2]) == {'reviewers': 3}
    assert stamps[0][3] == stamp
    assert json.loads(rows['run-p'])['risk_tier'] == 'substantial'
    assert json.loads(rows['run-p'])['floored'] == ['reviewers']
    assert (json.loads(rows['run-p'])['profile'], json.loads(rows['run-p'])['profile_digest']) == ('thorough', 'd1')
    assert 'profile' not in json.loads(rows['run-q'])  # absent means the default (balanced) profile


def test_a_message_in_several_identity_dirs_keeps_one_owner_across_ingest_and_relabelling(tmp_path):
    home = tmp_path / 'home'
    # One resumed message in two accounts' transcripts; its owner is the first file by path.
    jsonl(home / '.claude/projects/repo/a.jsonl', claude_turn('s', 'shared'))
    jsonl(home / '.claude-michael/projects/repo/b.jsonl', claude_turn('s', 'shared'))
    accounts = {'michael': {'identity_dir': str(home / '.claude-michael')}}
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'claude': accounts}})
    ledger = tmp_path / 'state/telemetry.sqlite3'
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT account FROM usage').fetchall() == [('michael',)]
    write_json(home / '.agents/config/telemetry.json', {'accounts': {'claude': {
        **accounts, 'pro': {'identity_dir': str(home / '.claude-pro')}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT account FROM usage').fetchall() == [('michael',)]


def test_a_transcript_swapped_for_a_link_after_discovery_is_never_read(tmp_path, monkeypatch):
    home = tmp_path / 'home'
    for name, value in environment(tmp_path).items():
        monkeypatch.setenv(name, value)
    monkeypatch.chdir(tmp_path)
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_swap_test')
    transcript = home / '.claude/projects/repo/s.jsonl'
    jsonl(transcript, claude_turn('s', 'real'))
    database = home / '.local/share/opencode/opencode.db'
    database.parent.mkdir(parents=True)
    def opencode(target, message):
        with sqlite3.connect(target) as db:
            db.execute('CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,'
                       'time_updated INTEGER,data TEXT)')
            db.execute('INSERT INTO message VALUES (?,?,?,?,?)', (message, 's', 1000, 1000, json.dumps(
                {'role': 'assistant', 'tokens': {'input': 1}})))
    opencode(database, 'real-oc')
    discovered = {source: telemetry.native_paths(source) for source in ('claude', 'opencode')}
    secret = home / '.claude/secret.jsonl'
    jsonl(secret, claude_turn('s', 'SECRET_MESSAGE'))
    opencode(home / 'other.db', 'SECRET_OC')
    for target, path in ((secret, transcript), (home / 'other.db', database)):
        link = path.with_name('swap')
        link.symlink_to(target)
        link.replace(path)
    monkeypatch.setattr(telemetry, 'native_paths', lambda source: discovered.get(source, {}))
    db = telemetry.connect()
    telemetry.ingest_owner().ingest(telemetry, db)
    assert db.execute('SELECT COUNT(*) FROM usage').fetchone()[0] == 0
    assert {row['source']: row['status'] for row in db.execute(
        "SELECT source, status FROM sources WHERE source IN ('claude','opencode')")} == {
            'claude': 'error', 'opencode': 'error'}
    # A plain file of another inode under the discovered name is refused too.
    transcript.unlink()
    jsonl(transcript, claude_turn('s', 'REPLACED'))
    telemetry.ingest_owner().ingest(telemetry, db)
    assert db.execute("SELECT COUNT(*) FROM usage WHERE source='claude'").fetchone()[0] == 0


def test_nested_lineage_reaches_usage_events_and_parent_stats(tmp_path):
    project = tmp_path / 'project'
    project.mkdir()
    state = tmp_path / 'state'
    state.mkdir()
    with sqlite3.connect(state / 'fabric.sqlite3') as db:
        db.execute('CREATE TABLE agents(project TEXT)')
        db.execute('INSERT INTO agents VALUES (?)', (str(project),))
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 2, 'run_dir': str(project)}
    path = project / '.agent-run/runs/20261022-1000-dispatch-child-abc/tasks/junior/attempt-001/attempt.json'
    write_json(path, {'schema': 'fabric.attempt.v1', 'run_id': 'mcp-child', 'task_id': 'junior',
                     'attempt': 1, 'parent': parent, 'children': [{'run_id': 'mcp-grandchild', 'task_id': 'bulk',
                         'attempt': 1, 'status': 'failed', 'error': 'PRIVATE_CHILD_ERROR'}],
                     'status': 'ok', 'state': 'terminal',
                     'cwd': str(project), 'ended_at': '2026-10-22T01:00:00Z',
                     'provenance': {'requested': {'adapter': 'codex'}, 'resolved_model': 'gpt-6.1-sol'},
                     'usage': {'input_tokens': 10, 'cache_read_tokens': 0, 'cache_write_tokens': 0,
                               'output_tokens': 5, 'reasoning_tokens': 0}})
    result = cli(tmp_path, 'telemetry', 'ingest')
    assert result.returncode == 0, result.stderr
    with sqlite3.connect(state / 'telemetry.sqlite3') as db:
        assert db.execute('SELECT parent_run_id,parent_task_id,parent_attempt FROM usage').fetchall() == [
            ('mcp-parent', 'writer', 2)]
        attrs = json.loads(db.execute("SELECT attrs FROM events WHERE kind='lane.attempt'").fetchone()[0])
        assert attrs['parent'] == parent
        assert attrs['children'] == [{'run_id': 'mcp-grandchild', 'task_id': 'bulk', 'attempt': 1, 'status': 'failed'}]
        assert 'PRIVATE_CHILD_ERROR' not in '\n'.join(db.iterdump())
    result = cli(tmp_path, 'stats', '--json')
    assert result.returncode == 0, result.stderr
    groups = json.loads(result.stdout)['usage_by_parent']
    assert [(row['parent_run_id'], row['parent_task_id'], row['parent_attempt'], row['output_tokens'])
            for row in groups] == [('mcp-parent', 'writer', 2, 5)]


def test_rows_under_earlier_host_defaults_merge_into_one_host_without_a_rewrite(tmp_path, monkeypatch):
    """Raw-hostname and 'local' rows from earlier versions read as this host; the ledger keeps them."""
    use_environment(tmp_path, monkeypatch)
    monkeypatch.setattr(__import__('socket'), 'gethostname', lambda: 'Anes-MacBook-Pro.local')
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_host_aliases')
    session = tmp_path / 'home/.claude/projects/repo/session.jsonl'
    def message(name):
        return {'type': 'assistant', 'sessionId': 'chair', 'timestamp': '2026-10-21T01:00:00Z',
                'message': {'id': name, 'model': 'claude-test', 'usage': {'input_tokens': 10, 'output_tokens': 1}}}
    jsonl(session, message('raw'), message('fallback'))
    with telemetry.connect() as db:
        telemetry.ingest_owner().ingest(telemetry, db)
    database = tmp_path / 'state/telemetry.sqlite3'
    # Stand in for rows written by earlier versions: the raw hostname, and fabric's old 'local'.
    with sqlite3.connect(database) as db:
        for table in ('usage', 'usage_files'):
            db.execute(f"UPDATE {table} SET host='Anes-MacBook-Pro.local' WHERE event_id LIKE '%raw%'")
            db.execute(f"UPDATE {table} SET host='local' WHERE event_id LIKE '%fallback%'")
        db.execute("UPDATE meta SET value='Anes-MacBook-Pro.local' WHERE key='host_id'")
    jsonl(session, message('slug'))
    with telemetry.connect() as db:
        telemetry.ingest_owner().ingest(telemetry, db)
        db.row_factory = sqlite3.Row
        report = telemetry.report_owner().stats(telemetry, db, summary=True)
    with sqlite3.connect(database) as db:
        stored = sorted(db.execute('SELECT host, COUNT(*) FROM usage GROUP BY host'))
    assert stored == [('Anes-MacBook-Pro.local', 1), ('anes-macbook-pro', 1), ('local', 1)]
    assert report['host'] == 'anes-macbook-pro'
    groups = report['session_usage']
    assert [(row['host'], row['session'], row['rows'], row['input_tokens']) for row in groups] == [
        ('anes-macbook-pro', 'chair', 3, 30)]


def test_long_lived_telemetry_refreshes_peer_aliases_after_hosts_config_changes(tmp_path, monkeypatch):
    use_environment(tmp_path, monkeypatch)
    monkeypatch.setattr(__import__('socket'), 'gethostname', lambda: 'Box.local')
    config = tmp_path / 'home/.agents/.agent-fabric/hosts.json'
    write_json(config, {'schema_version': 1, 'local_host': 'studio', 'peers': {}})
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_live_peer_aliases')
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl',
          {'type': 'assistant', 'sessionId': 'chair', 'timestamp': '2026-10-21T01:00:00Z',
           'message': {'id': 'one', 'model': 'claude-test', 'usage': {'input_tokens': 10}}})
    with telemetry.connect() as db:
        telemetry.ingest_owner().ingest(telemetry, db)
        db.execute("UPDATE usage SET host='box'")
        db.execute("UPDATE usage_files SET host='box'")
        db.row_factory = sqlite3.Row
        before = telemetry.report_owner().stats(telemetry, db, summary=True)
        assert [(row['host'], row['rows']) for row in before['session_usage']] == [('studio', 1)]
        write_json(config, {'schema_version': 1, 'local_host': 'studio',
                            'peers': {'box': {'ssh_destination': 'box'}}})
        after = telemetry.report_owner().stats(telemetry, db, summary=True)
    assert [(row['host'], row['rows']) for row in after['session_usage']] == [('box', 1)]


def ssh_tripwire(tmp_path):
    marker = tmp_path / 'ssh-called'
    shim = tmp_path / 'ssh-tripwire'
    shim.write_text(f'#!/bin/sh\necho called > {marker}\necho "ssh must not run" >&2\nexit 99\n')
    shim.chmod(0o755)
    return shim, marker


def test_a_renamed_host_never_rewrites_the_ledger_even_when_its_old_id_is_now_a_peer(tmp_path, monkeypatch):
    """This ledger once wrote as `box`; `box` is now a peer and this host is `studio`."""
    use_environment(tmp_path, monkeypatch)
    config = tmp_path / 'home/.agents/.agent-fabric/hosts.json'
    write_json(config, {'schema_version': 1, 'local_host': 'box'})
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl',
          {'type': 'assistant', 'sessionId': 'chair', 'timestamp': '2026-10-21T01:00:00Z',
           'message': {'id': 'one', 'model': 'claude-test', 'usage': {'input_tokens': 10, 'output_tokens': 1}}})
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_rename_box')
    with telemetry.connect() as db:
        telemetry.ingest_owner().ingest(telemetry, db)
    database = tmp_path / 'state/telemetry.sqlite3'
    tables = ('usage', 'usage_files', 'observations', 'quota_events', 'quota_series', 'events')
    with sqlite3.connect(database) as db:
        # The same event also stored under the new id, as a re-read after the rename would.
        db.execute("INSERT INTO usage SELECT source,event_id,'studio',"
                   + ','.join(column for (_, column, *_) in db.execute('PRAGMA table_info(usage)')
                              if column not in {'source', 'event_id', 'host'})
                   + " FROM usage WHERE host='box'")
        before = {table: sorted(db.execute(f'SELECT * FROM {table}')) for table in tables}
    assert len(before['usage']) == 2
    write_json(config, {'schema_version': 1, 'local_host': 'studio', 'peers': {'box': {'ssh_destination': 'box'}}})
    telemetry = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_rename_studio')
    with telemetry.connect() as db:
        telemetry.ingest_owner().ingest(telemetry, db)
        db.row_factory = sqlite3.Row
        report = telemetry.report_owner().stats(telemetry, db, summary=True)
    assert telemetry.same_host('box') == 'box'
    with sqlite3.connect(database) as db:
        assert {table: sorted(db.execute(f'SELECT * FROM {table}')) for table in tables} == before
    assert report['host'] == 'studio'
    assert sorted((row['host'], row['rows'], row['input_tokens']) for row in report['session_usage']) == [
        ('box', 1, 10), ('studio', 1, 10)]


# Golden check for the SQL-grouped `stats`: the row-by-row implementation it replaced is kept
# here as the oracle and both must report the same for one ledger.
def legacy_stats(db, since=None, prices=None, *, summary=False, until=None):
    """Ledger report for [since, until); either bound may be omitted."""
    free_patterns = {}
    if prices is None:
        prices, free_patterns = load_prices()
    boundary = since_time(since) if since else None
    end = since_time(until) if until else None
    def included(row):
        if not boundary and not end:
            return True
        stamp = since_time(row['timestamp']) if row['timestamp'] is not None else None
        return stamp is not None and (not boundary or stamp >= boundary) and (not end or stamp < end)
    adapters = catalogue()
    excluded = Counter()
    selected, unattributed = [], []
    # A day of margin keeps offset timestamps; `included` applies the exact bounds.
    query, parameters = 'SELECT * FROM usage', ()
    if boundary:
        query, parameters = query + ' WHERE timestamp >= ?', ((boundary - timedelta(days=1)).date().isoformat(),)
    for item in db.execute(query, parameters):
        row = dict(item)
        if not included(row):
            continue
        row['model_raw'] = row['model']
        row['model'] = normalise_model(row['adapter'], row['provider'], row['model'], adapters)
        reason = classify(row)
        if reason == 'test_fixture':
            excluded[reason] += 1
            continue
        if reason == 'unattributed':
            unattributed.append(row)
        selected.append(row)
    attributed = [row for row in selected if classify(row) is None]
    native = {(row['adapter'], row['session']) for row in attributed if row['source'] != 'fabric' and row['session']}
    usage = [row for row in attributed if row['source'] != 'fabric' or (row['adapter'], row['session']) not in native]
    for row in usage:
        row['day'] = since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
    latest = {}
    attempts = Counter()
    for row in selected:
        if row['source'] != 'fabric':
            continue
        attempts[(row['status'], row['error_class'])] += 1
        key = (row['repository'], row['run_id'], row['task_id'])
        if key not in latest or (row['attempt'] or 0) > (latest[key]['attempt'] or 0):
            latest[key] = row
    lanes = Counter((row['status'], row['error_class']) for row in latest.values())
    limits = Counter()
    last_limits = {}
    def remember_limit(row):
        key = (row['host'], row['provider'], row['account'])
        stamp = row.get('timestamp')
        if stamp and (key not in last_limits or since_time(stamp) > since_time(last_limits[key])):
            last_limits[key] = stamp
    receipt_outcomes = set()
    for row in selected:
        if row['source'] == 'fabric' and row['status'] in {'rate_limited', 'usage_limited'}:
            remember_limit(row)
            run = row['run_id']
            receipt_outcomes.add((row['adapter'], row['model_raw'], run, row['status']))
            day = since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
            limits[(day, row['provider'], row['account'], row['status'])] += 1
    seen = set()
    for item in db.execute('SELECT * FROM observations ORDER BY source'):
        row = dict(item)
        if not included(row) or row['status'] not in {'rate_limited', 'usage_limited'}:
            continue
        remember_limit(row)
        if (row['adapter'], row['model'], row['run_id'], row['status']) in receipt_outcomes:
            continue
        day = since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
        key = (row['adapter'], row['model'], row['run_id'] or row['timestamp'], row['status'], day)
        if key not in seen:
            seen.add(key)
            limits[(day, row['provider'], row['account'], row['status'])] += 1
    windows = {}
    for item in db.execute('SELECT * FROM observations WHERE used_percent IS NOT NULL ORDER BY timestamp'):
        row = dict(item)
        if included(row):
            windows[(row['provider'], row['account'], row['window'])] = row
    missing = Counter()
    for row in unattributed:
        group = missing.setdefault((row['source'], row['adapter']), {'rows': 0, 'input_tokens': 0, 'statuses': Counter()})
        group['rows'] += 1
        group['input_tokens'] += row['input_tokens'] or 0
        if row['status']:
            group['statuses'][row['status']] += 1
    report = {'schema_version': 2, 'since': since, 'host': host_id(),
            'usage': aggregate(usage, ('provider', 'adapter', 'model'), prices, free_patterns),
            'usage_per_day': aggregate(usage, ('day', 'provider', 'adapter', 'account'), prices, free_patterns),
            'usage_by_parent': aggregate([row for row in attributed if row['source'] == 'fabric' and row.get('parent_run_id')],
                                         ('parent_run_id', 'parent_task_id', 'parent_attempt'), prices, free_patterns),
            'outcomes': outcomes([row for row in attributed if row['source'] == 'fabric'],
                                 this(), costs, prices, free_patterns),
            # One overall group each, for before/after comparison; percentiles are never averaged.
            'lane_totals': outcomes([row for row in attributed if row['source'] == 'fabric'],
                                    this(), costs, prices, free_patterns, outcome_keys=()),
            'usage_totals': combine((unit(row, (), prices, free_patterns) for row in usage), ()),
            'lanes': [{'status': status, 'error_class': error, 'lanes': count}
                      for (status, error), count in sorted(lanes.items(), key=str)],
            'attempts': [{'status': status, 'error_class': error, 'attempts': count}
                         for (status, error), count in sorted(attempts.items(), key=str)],
            'limits_per_day': [dict(zip(('day','provider','account','status','events'), (*key, count)))
                               for key, count in sorted(limits.items(), key=str)],
            'last_limit_events': [dict(host=host, provider=provider, account=account, timestamp=stamp,
                                       age_seconds=max(0, round((since_time(now()) - since_time(stamp)).total_seconds())))
                                  for (host, provider, account), stamp in sorted(last_limits.items(), key=str)],
            'quota_observations': sorted(windows.values(), key=lambda row: tuple(str(row[k] or '') for k in ('provider', 'account', 'window'))),
            'excluded_rows': dict(excluded),
            'unattributed': [{'source': source, 'adapter': adapter, 'rows': group['rows'],
                              'input_tokens': group['input_tokens'], 'statuses': dict(sorted(group['statuses'].items()))}
                             for (source, adapter), group in sorted(missing.items(), key=lambda item: str(item[0]))],
            'receipt_usage_rows_excluded': len(attributed) - len(usage),
            'pending_source_rebuilds': [row[0] for row in db.execute('SELECT source FROM rebuilds ORDER BY source')]}
    if summary:
        keys = ('host', 'source', 'session', 'day', 'provider', 'adapter', 'account', 'model')
        report['session_usage'] = combine((unit(row, keys, prices, free_patterns) for row in usage), keys)
    # Not SQL-grouped: the delivery owner's count, tested on its own.
    report['compactions_per_lane'] = delivery_owner().report(this(), db, since, until)
    return report


def load_telemetry(tmp_path, monkeypatch):
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.setenv('HOME', str(tmp_path / 'home'))
    module = load_script(ROOT / 'scripts/telemetry.py', 'telemetry_golden')
    # The oracle runs against the module's own helpers.
    costs, report = module.costs_owner(), module.report_owner()
    names = {**vars(module), 'itertools': itertools, 'timedelta': timedelta,
             'load_prices': partial(costs.load_prices, module), 'catalogue': partial(costs.catalogue, module),
             'aggregate': partial(costs.aggregate, module), 'unit': partial(costs.unit, module),
             'normalise_model': costs.normalise_model, 'classify': costs.classify, 'combine': costs.combine,
             'costs': costs,
             'outcomes': report.outcomes, 'OUTCOME_KEYS': report.OUTCOME_KEYS}
    module.legacy_stats = types.FunctionType(legacy_stats.__code__, names, argdefs=legacy_stats.__defaults__)
    module.legacy_stats.__kwdefaults__ = legacy_stats.__kwdefaults__
    return module


def fixture_ledger(db):
    import random
    rng = random.Random(7)
    models = [('claude', 'anthropic', 'claude-opus-4-8'), ('claude', 'anthropic', 'opus'),
              ('codex', 'openai', 'gpt-5.5'), ('codex', 'openai', 'gpt-5.5-high'),
              ('opencode', 'opencode', 'free'), ('claude', 'anthropic', '<synthetic>'),
              ('claude', 'anthropic', 'fixture'), ('codex', 'openai', 'nonexistent-model'),
              ('claude', 'anthropic', 'unpriced-model'), (None, None, None), ('claude', 'anthropic', ' ')]
    stamps = ['2026-10-19T23:59:59Z', '2026-10-20T00:00:00Z', '2026-10-20T13:00:00.250Z',
              '2026-10-20T23:30:00-05:00', '2026-10-21T08:00:00+10:00', '2026-10-21T12:00:00+00:00',
              '2026-10-21T23:59:59Z', '2026-10-22T00:00:00Z', '2026-10-23T09:00:00Z', '2026-10-21 12:00:00', '2026-10-21T12:00:00', '2026-10-21T12:00:00Z', None]
    tokens = (None, 0, 5, 1200)
    rows = []
    for index in range(600):
        source = rng.choice(['claude', 'codex', 'opencode', 'fabric', 'fabric'])
        adapter, provider, model = rng.choice(models)
        rows.append({'source': source, 'event_id': f'e{index}', 'host': rng.choice(['h1', 'h2']),
                     'session': rng.choice([None, '', 's1', 's2', 's3']), 'run_id': f'r{rng.randrange(8)}',
                     'task_id': f't{rng.randrange(3)}', 'attempt': rng.choice([None, 1, 2, 3]),
                     'repository': 'repo', 'adapter': adapter, 'provider': provider, 'model': model,
                     'effort': rng.choice([None, 'high']), 'account': rng.choice([None, '', 'a', 'b']),
                     'timestamp': rng.choice(stamps),
                     **{field: rng.choice(tokens) for field in (
                         'input_tokens', 'cache_read_tokens', 'cache_write_tokens', 'output_tokens', 'reasoning_tokens')},
                     'reported_cost': rng.choice([None, None, 0.25, 1.5]),
                     'status': rng.choice([None, 'ok', 'succeeded', 'failed', 'rate_limited', 'usage_limited']),
                     'error_class': rng.choice([None, 'timeout']), 'task_class': rng.choice([None, 'code']),
                     'route': rng.choice([None, 'x']), 'duration_ms': rng.choice([None, 900, 4000]),
                     'provider_ms': rng.choice([None, 700, 3000])})
    for row in rows:
        db.execute(f"INSERT INTO usage({','.join(row)}) VALUES ({','.join('?' for _ in row)})", tuple(row.values()))
    db.commit()


def serialised(report, telemetry):
    # `session_usage` is full-precision transport to the host merge; compare it at the output edge.
    # The legacy oracle predates the output-edge rounding of `usage_totals`.
    report.pop('deliveries', None)  # Delivery receipt fields were added after the row-by-row oracle.
    for row in report.get('session_usage', []) + report['usage_totals']:
        row['known_cost_usd'] = telemetry.costs_owner().micro(row['known_cost_usd'])
        row['reported_cost_usd'] = telemetry.costs_owner().micro(row['reported_cost_usd'])
    for event in report['last_limit_events']:
        event.pop('age_seconds')  # depends on the clock
    return json.dumps(report, indent=2)


def test_sql_grouped_stats_match_the_row_by_row_oracle(tmp_path, monkeypatch):
    telemetry = load_telemetry(tmp_path, monkeypatch)
    with telemetry.connect() as db:
        fixture_ledger(db)
        for kwargs in ({}, {'summary': True}, {'since': '2026-10-20'}, {'since': '2026-10-20T12:00:00Z'},
                       {'until': '2026-10-22'}, {'since': '2026-10-20', 'until': '2026-10-22T06:00:00+00:00'},
                       {'since': '2026-10-21T00:00:00+10:00', 'summary': True}):
            new = telemetry.report_owner().stats(telemetry, db, **kwargs)
            old = telemetry.legacy_stats(db, **kwargs)
            assert new['usage'] and new['unattributed'] and new['excluded_rows'], kwargs
            assert serialised(new, telemetry) == serialised(old, telemetry), kwargs


def test_host_merge_rounds_costs_once_after_summing(tmp_path, monkeypatch):
    telemetry = load_telemetry(tmp_path, monkeypatch)
    hosts = load_script(ROOT / 'scripts/telemetry_hosts.py', 'telemetry_hosts_golden')
    def group(session):
        row = telemetry.costs_owner().unit(telemetry, {'adapter': 'codex', 'provider': 'openai', 'model': 'gpt-5.5', 'day': '2026-10-21',
                              'account': None, 'input_tokens': 1, 'cache_read_tokens': 0, 'cache_write_tokens': 0,
                              'output_tokens': 0, 'reasoning_tokens': 0, 'reported_cost': 4e-7, 'model_raw': 'gpt-5.5'},
                             (), {}, {})
        return {**row, 'known_cost_usd': 4e-7, 'missing_cost_rows': 0, 'host': 'h1', 'source': 'codex', 'session': session,
                'provider': 'openai', 'adapter': 'codex', 'model': 'gpt-5.5', 'day': '2026-10-21', 'account': None}
    summary = {'host': 'h1', 'session_usage': [group('a'), group('b')], 'excluded_rows': {},
               'receipt_usage_rows_excluded': 0, 'pending_source_rebuilds': [],
               'compactions_per_lane': [{'lane': 'lane-a', 'compactions': 2}]}
    monkeypatch.setattr(hosts.quota_display, 'report', lambda quota, values: {})
    merged = hosts.merge(telemetry, [summary], [{'host': 'h1', 'status': 'ok'}], None)
    assert merged['usage'][0]['estimated_cost_usd'] == 1e-6
    assert merged['usage'][0]['reported_cost_usd'] == 1e-6
    assert merged['usage_per_day'][0]['estimated_cost_usd'] == 1e-6
    assert merged['compactions_per_lane'] == [{'lane': 'lane-a', 'compactions': 2, 'host': 'h1'}]


def test_receipt_models_stay_distinct_and_native_models_are_observed(tmp_path):
    receipt(tmp_path, 'observed', status='ok', provenance={
        'requested': {'adapter': 'claude', 'model': 'opus'},
        'resolved_model': 'claude-opus-x', 'observed_model': 'claude-sonnet-y'})
    receipt(tmp_path, 'resolved', status='ok', provenance={
        'requested': {'adapter': 'claude', 'model': 'opus'}, 'resolved_model': 'claude-opus-x'})
    jsonl(tmp_path / 'home/.claude/projects/repo/s.jsonl', {
        'type': 'assistant', 'sessionId': 's', 'timestamp': '2026-10-21T01:00:00Z',
        'message': {'id': 'm', 'model': 'claude-native', 'usage': {'input_tokens': 7}}})
    jsonl(tmp_path / 'home/.codex/sessions/day/rollout-models.jsonl',
          {'type': 'session_meta', 'payload': {'id': 'c'}},
          {'type': 'turn_context', 'payload': {'model': 'codex-native'}},
          {'type': 'event_msg', 'payload': {'type': 'token_count',
           'info': {'total_token_usage': {'input_tokens': 5}}}})
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        assert db.execute('SELECT run_id, requested_model, resolved_model, observed_model, model '
                          "FROM usage WHERE source='fabric' ORDER BY run_id").fetchall() == [
            ('observed', 'opus', 'claude-opus-x', 'claude-sonnet-y', 'claude-sonnet-y'),
            ('resolved', 'opus', 'claude-opus-x', None, 'claude-opus-x')]
        assert db.execute('SELECT source, requested_model, resolved_model, observed_model, model '
                          "FROM usage WHERE source!='fabric' ORDER BY source").fetchall() == [
            ('claude', None, None, 'claude-native', 'claude-native'),
            ('codex', None, None, 'codex-native', 'codex-native')]
        assert db.execute('PRAGMA user_version').fetchone() == (4,)


def test_receipt_fields_refresh_without_replacing_event_history(tmp_path):
    path = receipt(tmp_path, 'run-refresh', status='ok', empty_patch=True,
                   retry_of='attempt-001', escalation_reason='gate_failed',
                   profile={'preset': 'thorough', 'digest': 'd1', 'knobs': {},
                            'risk_tier': 'substantial', 'floored': []})
    receipt(tmp_path, 'changed', status='ok', empty_patch=False, retry_of='attempt-001')
    receipt(tmp_path, 'unknown', status='ok')
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        rows = {run: json.loads(attrs) for run, attrs in db.execute(
            "SELECT run_id, attrs FROM events WHERE kind='lane.attempt'")}
        assert rows['run-refresh']['empty_patch'] is True
        assert rows['changed']['empty_patch'] is False
        assert 'empty_patch' not in rows['unknown']
        assert rows['run-refresh']['retry_of'] == 'attempt-001'
        assert rows['run-refresh']['escalation_reason'] == 'gate_failed'
        assert rows['run-refresh']['escalated_from'] == 'run-refresh/lane/1'
        assert 'escalated_from' not in rows['changed']
        before = db.execute("SELECT event_id,ts FROM events WHERE run_id='run-refresh'").fetchone()
        # Simulate an older event with existing history but no new receipt attrs.
        db.execute("UPDATE events SET attrs=? WHERE run_id='run-refresh'", ('{"retained":true}',))
        db.execute("DELETE FROM cursors WHERE source='fabric'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        event_id, ts, attrs = db.execute(
            "SELECT event_id,ts,attrs FROM events WHERE run_id='run-refresh'").fetchone()
        assert (event_id, ts) == before
        assert json.loads(attrs) == {**rows['run-refresh'], 'retained': True}
        assert db.execute("SELECT COUNT(*) FROM events WHERE kind='lane.attempt'").fetchone() == (3,)


def test_model_split_migration_rereads_only_surviving_receipts_once(tmp_path):
    kept = receipt(tmp_path, 'kept', status='ok', provenance=route('claude', 'opus'),
                   empty_patch=True, profile={'preset': 'balanced', 'digest': 'd1', 'knobs': {}})
    pruned = receipt(tmp_path, 'pruned', status='ok', provenance=route('claude', 'sonnet'))
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    pruned.unlink()
    ledger = tmp_path / 'state/telemetry.sqlite3'
    with sqlite3.connect(ledger) as db:
        for column in ('requested_model', 'resolved_model', 'observed_model'):
            db.execute(f'ALTER TABLE usage DROP COLUMN {column}')
        db.execute("DELETE FROM meta WHERE key='model_split'")
        db.execute('DELETE FROM harness_stamps')
        db.execute("UPDATE events SET attrs='{}'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute('SELECT run_id, requested_model, resolved_model, observed_model, model '
                          'FROM usage ORDER BY run_id').fetchall() == [
            ('kept', 'opus', 'opus', None, 'opus'), ('pruned', None, None, None, 'sonnet')]
        assert json.loads(db.execute("SELECT attrs FROM events WHERE run_id='kept'").fetchone()[0])['empty_patch'] is True
        assert db.execute('SELECT COUNT(*) FROM harness_stamps').fetchone() == (1,)
        assert db.execute('PRAGMA user_version').fetchone() == (4,)
        cursor = db.execute("SELECT * FROM cursors WHERE path=?", (str(kept),)).fetchone()
        # A one-time migration must not keep resetting the Fabric cursor on connect.
        db.execute("UPDATE usage SET requested_model='sentinel' WHERE run_id='kept'")
    assert cli(tmp_path, 'telemetry', 'ingest').returncode == 0
    with sqlite3.connect(ledger) as db:
        assert db.execute("SELECT requested_model FROM usage WHERE run_id='kept'").fetchone() == ('sentinel',)
        assert db.execute("SELECT * FROM cursors WHERE path=?", (str(kept),)).fetchone() == cursor
