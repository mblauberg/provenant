#!/usr/bin/env python3
"""Local, metadata-only provider usage ledger."""

from __future__ import annotations

import argparse
import hashlib
from collections import Counter
from datetime import UTC, datetime
import json
import os
from pathlib import Path
import sqlite3
import socket
import subprocess
import sys


SOURCES = ('fabric', 'route_health', 'cooldowns', 'claude', 'codex', 'opencode')


def now():
    return datetime.now(UTC).isoformat().replace('+00:00', 'Z')


def state_root():
    return Path(os.environ.get('AGENT_FABRIC_STATE_ROOT',
                               str(Path.home() / '.local/state/agent-harness/fabric')))


def patterns():
    home, state = Path.home(), state_root()
    return {
        'fabric': '<current-and-registered-projects>/.agent-run/{runs/**,mcp-*/**}/attempt.json',
        'route_health': os.environ.get('AGENT_FABRIC_ROUTE_HEALTH_PATH', str(state / 'route-health.json')),
        'cooldowns': os.environ.get('FABRIC_COOLDOWNS_PATH', str(state / 'cooldowns.json')),
        'claude': str(home / '.claude/projects/**/*.jsonl'),
        'codex': str(home / '.codex/sessions/**/rollout-*.jsonl'),
        'opencode': str(home / '.local/share/opencode/opencode.db'),
    }


def connect():
    path = state_root() / 'telemetry.sqlite3'
    path.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path)
    db.row_factory = sqlite3.Row
    version = db.execute('PRAGMA user_version').fetchone()[0]
    if version not in (0, 1, 2, 3):
        db.close()
        raise ValueError(f'unsupported telemetry schema version: {version}')
    db.executescript('''
        CREATE TABLE IF NOT EXISTS rebuilds (
            source TEXT PRIMARY KEY, paths TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS sources (
            source TEXT PRIMARY KEY, path_pattern TEXT NOT NULL,
            parser_version INTEGER NOT NULL, last_ingest TEXT,
            row_count INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS usage (
            source TEXT NOT NULL, event_id TEXT NOT NULL, host TEXT NOT NULL,
            session TEXT, lane TEXT, run_id TEXT, task_id TEXT, attempt INTEGER,
            graph_node TEXT, adapter TEXT, provider TEXT,
            model TEXT, effort TEXT, account TEXT, repository TEXT, revision TEXT,
            timestamp TEXT, input_tokens INTEGER, cache_read_tokens INTEGER,
            cache_write_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
            usage_source TEXT, reported_cost REAL, status TEXT, error_class TEXT,
            PRIMARY KEY(source,event_id,host));
        CREATE TABLE IF NOT EXISTS usage_files (
            source TEXT NOT NULL, event_id TEXT NOT NULL, host TEXT NOT NULL, path TEXT NOT NULL,
            PRIMARY KEY(source,event_id,host,path));
        CREATE INDEX IF NOT EXISTS usage_time ON usage(timestamp);
        CREATE TABLE IF NOT EXISTS cursors (
            source TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
            mtime INTEGER NOT NULL, offset INTEGER NOT NULL, state TEXT NOT NULL,
            PRIMARY KEY(source,path));
        CREATE TABLE IF NOT EXISTS observations (
            event_id TEXT PRIMARY KEY, source TEXT NOT NULL, host TEXT NOT NULL,
            provider TEXT, adapter TEXT, model TEXT, account TEXT, session TEXT,
            origin_path TEXT, lane TEXT, run_id TEXT, timestamp TEXT, window TEXT, used_percent REAL,
            window_minutes REAL, reset_at TEXT, status TEXT);

    ''')
    observation_columns = {row[1] for row in db.execute('PRAGMA table_info(observations)')}
    if 'origin_path' not in observation_columns:
        db.execute('ALTER TABLE observations ADD COLUMN origin_path TEXT')
    usage_columns = {row[1] for row in db.execute('PRAGMA table_info(usage)')}
    for name, kind in (('run_id', 'TEXT'), ('task_id', 'TEXT'), ('attempt', 'INTEGER')):
        if name not in usage_columns:
            db.execute(f'ALTER TABLE usage ADD COLUMN {name} {kind}')
    if version in (1, 2):
        for source in SOURCES:
            paths = [row[0] for row in db.execute('SELECT path FROM cursors WHERE source=?', (source,))]
            if paths:
                db.execute('INSERT OR REPLACE INTO rebuilds VALUES (?,?)', (source, json.dumps(paths)))
                db.execute('DELETE FROM usage_files WHERE source=?', (source,))
                db.execute('DELETE FROM cursors WHERE source=?', (source,))
    db.execute('PRAGMA user_version = 3')
    for source, pattern in patterns().items():
        db.execute('INSERT INTO sources(source,path_pattern,parser_version,status) VALUES (?,?,2,?) '
                   'ON CONFLICT(source) DO UPDATE SET path_pattern=excluded.path_pattern,parser_version=excluded.parser_version',
                   (source, pattern, 'not_ingested'))
    db.commit()
    return db


def number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and value >= 0 else None


def put_usage(db, source, event_id, origin_path, **values):
    values = {'source': source, 'event_id': event_id, 'host': socket.gethostname(), **values}
    columns = list(values)
    update = ','.join(f'{key}=excluded.{key}' for key in columns if key not in {'source', 'event_id', 'host'})
    condition = (' WHERE COALESCE(excluded.output_tokens,0)+COALESCE(excluded.reasoning_tokens,0) '
                 '>= COALESCE(usage.output_tokens,0)+COALESCE(usage.reasoning_tokens,0) '
                 'OR NOT EXISTS (SELECT 1 FROM usage_files f WHERE f.source=usage.source '
                 'AND f.event_id=usage.event_id AND f.host=usage.host)'
                 if source == 'claude' else '')
    db.execute(f"INSERT INTO usage({','.join(columns)}) VALUES ({','.join('?' for _ in columns)}) "
               f'ON CONFLICT(source,event_id,host) DO UPDATE SET {update}{condition}', list(values.values()))
    db.execute('INSERT OR IGNORE INTO usage_files VALUES (?,?,?,?)',
               (source, event_id, values['host'], str(origin_path)))


def claude_record(db, row, state, path, offset):
    message = row.get('message') or {}
    usage = message.get('usage')
    if row.get('type') != 'assistant' or not isinstance(usage, dict) or not message.get('id'):
        return
    reasoning = number((usage.get('output_tokens_details') or {}).get('thinking_tokens'))
    output = number(usage.get('output_tokens'))
    put_usage(db, 'claude', message['id'], origin_path=path, session=row.get('sessionId'),
              adapter='claude', provider='anthropic', model=message.get('model'),
              effort=row.get('perTurnEffort'), account=row.get('account'),
              repository=row.get('cwd'), revision=row.get('gitRevision'), timestamp=row.get('timestamp'),
              input_tokens=number(usage.get('input_tokens')),
              cache_read_tokens=number(usage.get('cache_read_input_tokens')),
              cache_write_tokens=number(usage.get('cache_creation_input_tokens')),
              output_tokens=max(0, output - (reasoning or 0)) if output is not None else None,
              reasoning_tokens=reasoning, usage_source='message.usage')


def identity(*values):
    return hashlib.sha256(json.dumps(values, sort_keys=True).encode()).hexdigest()


def observation(db, source, **values):
    values = {'source': source, 'host': socket.gethostname(), **values}
    values = {'event_id': identity(values), **values}
    db.execute(f"INSERT OR IGNORE INTO observations({','.join(values)}) "
               f"VALUES ({','.join('?' for _ in values)})", list(values.values()))


def codex_record(db, row, state, path, offset):
    payload = row.get('payload') or {}
    if row.get('type') == 'session_meta':
        state.update(session=payload.get('id') or payload.get('session_id'),
                     repository=payload.get('cwd'), account=payload.get('creator_account_id'),
                     revision=(payload.get('git') or {}).get('commit_hash'))
    elif row.get('type') == 'turn_context':
        state.update(model=payload.get('model'), effort=payload.get('effort'),
                     repository=payload.get('cwd') or state.get('repository'))
    elif row.get('type') == 'event_msg' and payload.get('type') == 'token_count':
        meta = {key: state.get(key) for key in ('session', 'repository', 'account', 'revision', 'model', 'effort')}
        timestamp = row.get('timestamp')
        info = payload.get('info') or {}
        total = info.get('total_token_usage')
        if isinstance(total, dict):
            total = {k: v for k, v in total.items() if k in {
                'input_tokens', 'cached_input_tokens', 'cache_write_input_tokens',
                'output_tokens', 'reasoning_output_tokens', 'total_tokens'} and number(v) is not None}
            previous = state.get('total') or {}
            reset = any(number(v) is not None and v < previous.get(k, 0) for k, v in total.items())
            delta = {k: max(0, v - (0 if reset else previous.get(k, 0)))
                     for k, v in total.items() if number(v) is not None}
            state['total'] = total
            event_id = identity(meta.get('session') or str(path), offset)
        else:
            delta = {k: v for k, v in (info.get('last_token_usage') or {}).items()
                     if k in {'input_tokens', 'cached_input_tokens', 'cache_write_input_tokens',
                              'output_tokens', 'reasoning_output_tokens', 'total_tokens'} and number(v) is not None}
            event_id = identity(meta.get('session') or str(path), timestamp, delta)
        if any(delta.values()):
            cached = delta.get('cached_input_tokens')
            written = delta.get('cache_write_input_tokens')
            reasoning = delta.get('reasoning_output_tokens')
            input_count, output = delta.get('input_tokens'), delta.get('output_tokens')
            put_usage(db, 'codex', event_id, origin_path=path, **meta, adapter='codex', provider='openai',
                      timestamp=timestamp,
                      input_tokens=max(0, input_count - (cached or 0) - (written or 0)) if input_count is not None else None,
                      cache_read_tokens=cached, cache_write_tokens=written,
                      output_tokens=max(0, output - (reasoning or 0)) if output is not None else None,
                      reasoning_tokens=reasoning, usage_source='token_count')
        limits = payload.get('rate_limits') or {}
        for window, value in limits.items():
            if isinstance(value, dict) and number(value.get('used_percent')) is not None:
                reset_at = value.get('resets_at')
                if isinstance(reset_at, (int, float)):
                    reset_at = datetime.fromtimestamp(reset_at, UTC).isoformat().replace('+00:00', 'Z')
                observation(db, 'codex', origin_path=str(path), provider='openai', adapter='codex', model=state.get('model'),
                            account=state.get('account'), session=state.get('session'),
                            timestamp=timestamp, window=window, used_percent=value['used_percent'],
                            window_minutes=number(value.get('window_minutes')), reset_at=reset_at)


def clear_file(db, source, path):
    events = db.execute('SELECT event_id,host FROM usage_files WHERE source=? AND path=?',
                        (source, str(path))).fetchall()
    db.execute('DELETE FROM usage_files WHERE source=? AND path=?', (source, str(path)))
    for event_id, host in events:
        db.execute('DELETE FROM usage WHERE source=? AND event_id=? AND host=? AND NOT EXISTS '
                   '(SELECT 1 FROM usage_files f WHERE f.source=usage.source '
                   'AND f.event_id=usage.event_id AND f.host=usage.host)', (source, event_id, host))
    db.execute('DELETE FROM observations WHERE source=? AND origin_path=?', (source, str(path)))


def file_identity(path, length):
    stat = path.stat()
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        remaining = length
        while remaining:
            chunk = stream.read(min(remaining, 1024 * 1024))
            if not chunk:
                break
            digest.update(chunk)
            remaining -= len(chunk)
    return [stat.st_dev, stat.st_ino, length, digest.hexdigest()]


def ingest_jsonl(db, source, path, parse):
    stat = path.stat()
    old = db.execute('SELECT * FROM cursors WHERE source=? AND path=?', (source, str(path))).fetchone()
    state = json.loads(old['state']) if old else {}
    previous_file = state.get('_file')
    unchanged = old and old['size'] == stat.st_size and old['mtime'] == stat.st_mtime_ns
    current_id = [stat.st_dev, stat.st_ino]
    if unchanged and previous_file and previous_file[:2] == current_id:
        return
    rewritten = bool(old and (stat.st_size < old['size'] or
                     (previous_file and (previous_file[2] < old['size'] or
                      file_identity(path, previous_file[2]) != previous_file))))
    if rewritten:
        clear_file(db, source, path)
        state = {}
    offset = old['offset'] if old and not rewritten else 0
    with path.open('rb') as stream:
        stream.seek(offset)
        while stream.tell() < stat.st_size:
            line = stream.readline(stat.st_size - stream.tell())
            if not line.endswith(b'\n'):
                break
            row = json.loads(line)
            parse(db, row, state, path, offset)
            offset = stream.tell()
    state['_file'] = file_identity(path, stat.st_size)
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               (source, str(path), stat.st_size, stat.st_mtime_ns, offset, json.dumps(state)))


def readonly(path):
    return sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True)


def fabric_paths():
    projects = {Path.cwd()}
    registry = Path(os.environ.get('AGENT_FABRIC_STATE_DIRECTORY',
                    str(Path.home() / '.local/state/agent-harness/fabric'))) / 'fabric.sqlite3'
    if registry.is_file():
        with readonly(registry) as source:
            projects.update(Path(row[0]) for row in source.execute('SELECT DISTINCT project FROM agents'))
    roots = set()
    for project in projects:
        try:
            result = subprocess.run(['git', '-C', str(project), 'rev-parse', '--path-format=absolute',
                                     '--git-common-dir'], capture_output=True, text=True, timeout=5,
                                    env={k: v for k, v in os.environ.items() if not k.startswith('GIT_')})
            root = Path(result.stdout.strip()).parent if result.returncode == 0 else project
        except (OSError, subprocess.SubprocessError):
            root = project
        roots.add(root / '.agent-run')
        roots.add(project / '.agent-run')
    return sorted({path for root in roots for pattern in ('runs/**/attempt.json', 'mcp-*/**/attempt.json')
                   for path in root.glob(pattern)})


def source_paths():
    home, configured = Path.home(), patterns()
    return {'fabric': [],
            'route_health': [Path(configured['route_health'])] if Path(configured['route_health']).is_file() else [],
            'cooldowns': [Path(configured['cooldowns'])] if Path(configured['cooldowns']).is_file() else [],
            'claude': sorted((home / '.claude/projects').glob('**/*.jsonl')),
            'codex': sorted((home / '.codex/sessions').glob('**/rollout-*.jsonl')),
            'opencode': [Path(configured['opencode'])] if Path(configured['opencode']).is_file() else []}


PROVIDERS = {'claude': 'anthropic', 'codex': 'openai', 'cursor': 'cursor',
             'opencode': 'opencode', 'agy': 'google'}


def ingest_document(db, source, path):
    stat = path.stat()
    old = db.execute('SELECT * FROM cursors WHERE source=? AND path=?', (source, str(path))).fetchone()
    if old and old['size'] == stat.st_size and old['mtime'] == stat.st_mtime_ns:
        return
    row = json.loads(path.read_text())
    if source == 'fabric':
        provenance = row.get('provenance') or {}
        requested = provenance.get('requested') or {}
        adapter = requested.get('adapter')
        usage = row.get('usage') or row.get('context') or {}
        input_count = number(usage.get('input_tokens'))
        cached = number(usage.get('cache_read_tokens', usage.get('cached_input_tokens')))
        written = number(usage.get('cache_write_tokens', row.get('cache_write_tokens')))
        output = number(usage.get('output_tokens'))
        reasoning = number(usage.get('reasoning_tokens', row.get('reasoning_tokens')))
        if not row.get('usage'):
            if input_count is not None:
                input_count = max(0, input_count - (cached or 0) - (written or 0))
            if output is not None and adapter in {'codex', 'claude', 'cursor'}:
                output = max(0, output - (reasoning or 0))
        put_usage(db, source, identity(str(path.resolve())), origin_path=path,
                  session=row.get('session_id'), run_id=row.get('run_id'),
                  task_id=row.get('task_id'), attempt=row.get('attempt'), lane='/'.join(str(row.get(k, '')) for k in ('run_id','task_id','attempt')),
                  graph_node=row.get('graph_node'), adapter=adapter,
                  provider=provenance.get('provider') or PROVIDERS.get(adapter, adapter),
                  model=provenance.get('observed_model') or provenance.get('resolved_model') or requested.get('model'),
                  effort=provenance.get('effort_applied') or requested.get('effort'),
                  account=row.get('account') or provenance.get('account'),
                  repository=row.get('workspace_root') or row.get('cwd'),
                  revision=row.get('repository_revision'), timestamp=row.get('ended_at') or row.get('started_at'),
                  input_tokens=input_count, cache_read_tokens=cached, cache_write_tokens=written,
                  output_tokens=output, reasoning_tokens=reasoning,
                  usage_source=usage.get('usage_source') or row.get('usage_source') or usage.get('source'),
                  status=row.get('status') or row.get('state'), error_class=row.get('error_class'))
    else:
        entries = row.get('routes', {}) if source == 'route_health' else row.get('cooldowns', {})
        for entry in entries.values():
            for item in entry.get('recent', []) if source == 'route_health' else [entry]:
                adapter = entry.get('adapter')
                observation(db, source, origin_path=str(path), adapter=adapter, provider=PROVIDERS.get(adapter, adapter),
                            model=entry.get('model'), account=entry.get('account'),
                            timestamp=item.get('at') or item.get('recorded_at'),
                            run_id=item.get('run_id') or item.get('source_run'),
                            status=item.get('status'), reset_at=item.get('cooling_until'))
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               (source, str(path), stat.st_size, stat.st_mtime_ns, stat.st_size, '{}'))


def ingest_opencode(db, path):
    old = db.execute('SELECT state FROM cursors WHERE source=? AND path=?', ('opencode', str(path))).fetchone()
    saved = json.loads(old['state']) if old else {}
    watermark = saved.get('max_updated', 0)
    stat = path.stat()
    file_id = [stat.st_dev, stat.st_ino]
    with readonly(path) as source:
        latest = source.execute('SELECT MAX(time_updated) FROM message').fetchone()[0] or 0
        if old and (saved.get('file_id') != file_id or latest < watermark):
            clear_file(db, 'opencode', path)
            watermark = 0
        # Inclusive watermark covers messages updated during an earlier ingest.
        for message_id, session, created, updated, data in source.execute(
                'SELECT id,session_id,time_created,time_updated,data FROM message '
                'WHERE time_updated>=? ORDER BY time_updated,id', (watermark,)):
            row = json.loads(data)
            watermark = max(watermark, updated)
            if row.get('role') != 'assistant':
                continue
            usage = row.get('tokens') or {}
            cache = usage.get('cache') or {}
            stamp = (row.get('time') or {}).get('created') or created
            timestamp = datetime.fromtimestamp(stamp / 1000, UTC).isoformat().replace('+00:00', 'Z')
            put_usage(db, 'opencode', message_id, origin_path=path, session=session, adapter='opencode',
                      provider=row.get('providerID'), model=row.get('modelID'),
                      timestamp=timestamp, repository=(row.get('path') or {}).get('cwd'),
                      account=row.get('account'), input_tokens=number(usage.get('input')),
                      output_tokens=number(usage.get('output')), reasoning_tokens=number(usage.get('reasoning')),
                      cache_read_tokens=number(cache.get('read')), cache_write_tokens=number(cache.get('write')),
                      reported_cost=number(row.get('cost')), usage_source='message.tokens')
    stat = path.stat()
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               ('opencode', str(path), stat.st_size, stat.st_mtime_ns, 0, json.dumps({'max_updated': watermark, 'file_id': file_id})))


def ingest(db):
    for source, paths in source_paths().items():
        errors = 0
        if source == 'fabric':
            try:
                paths = fabric_paths()
            except (OSError, ValueError, sqlite3.Error):
                errors += 1
        for path in paths:
            try:
                with db:
                    if source == 'opencode':
                        ingest_opencode(db, path)
                    elif source in {'fabric', 'route_health', 'cooldowns'}:
                        ingest_document(db, source, path)
                    else:
                        ingest_jsonl(db, source, path, codex_record if source == 'codex' else claude_record)
            except (OSError, ValueError, TypeError, AttributeError, sqlite3.Error):
                errors += 1
        rebuild = db.execute('SELECT paths FROM rebuilds WHERE source=?', (source,)).fetchone()
        if rebuild and not errors and set(json.loads(rebuild['paths'])).issubset({str(path) for path in paths}):
            db.execute('DELETE FROM usage WHERE source=? AND NOT EXISTS '
                       '(SELECT 1 FROM usage_files f WHERE f.source=usage.source '
                       'AND f.event_id=usage.event_id AND f.host=usage.host)', (source,))
            db.execute('DELETE FROM observations WHERE source=? AND origin_path IS NULL', (source,))
            db.execute('DELETE FROM rebuilds WHERE source=?', (source,))
        table = 'observations' if source in {'route_health', 'cooldowns'} else 'usage'
        count = db.execute(f'SELECT COUNT(*) FROM {table} WHERE source=?', (source,)).fetchone()[0]
        status = 'error' if errors else 'ok' if paths else 'absent'
        db.execute('UPDATE sources SET last_ingest=?,status=?,row_count=? WHERE source=?',
                   (now(), status, count, source))
        db.commit()
    return [dict(row) for row in db.execute('SELECT * FROM sources ORDER BY source')]


TOKEN_FIELDS = ('input_tokens', 'cache_read_tokens', 'cache_write_tokens', 'output_tokens', 'reasoning_tokens')
PRICE_TABLE = Path(__file__).resolve().parents[1] / 'config/telemetry-prices.json'


def estimated_cost(row, prices):
    entry = (prices.get(f"{row['adapter']}/{row['model']}") or
             prices.get(f"{row['adapter']}/{row.get('provider')}/{row['model']}") or {})
    rates = entry.get('usd_per_million') or {}
    total = 0.0
    for token, rate in (('input_tokens', 'input'), ('cache_read_tokens', 'cached_input'),
                        ('cache_write_tokens', 'cache_write'), ('output_tokens', 'output'),
                        ('reasoning_tokens', 'output')):
        count = row[token]
        price = number(rates.get(rate))
        if count is None or (count > 0 and price is None):
            return None
        total += count * (price or 0) / 1_000_000
    return total


def aggregate(rows, keys, prices):
    groups = {}
    for row in rows:
        key = tuple(row.get(k) for k in keys)
        if key not in groups:
            groups[key] = {**dict(zip(keys, key)), **{field: 0 for field in TOKEN_FIELDS},
                           'rows': 0, 'missing_cost_rows': 0, 'estimated_cost_usd': 0.0,
                           'reported_cost_usd': None, 'cost_basis': 'api_equivalent_estimate',
                           'missing_token_rows': 0,
                           'missing_token_fields': dict.fromkeys(TOKEN_FIELDS, 0)}
        group = groups[key]
        group['rows'] += 1
        group['missing_token_rows'] += int(any(row[field] is None for field in TOKEN_FIELDS))
        for field in TOKEN_FIELDS:
            group[field] += row[field] or 0
            group['missing_token_fields'][field] += int(row[field] is None)
        cost = estimated_cost(row, prices)
        if cost is None:
            group['missing_cost_rows'] += 1
        else:
            group['estimated_cost_usd'] += cost
        if row['reported_cost'] is not None:
            group['reported_cost_usd'] = (group['reported_cost_usd'] or 0) + row['reported_cost']
    for group in groups.values():
        inputs = sum(group[field] for field in TOKEN_FIELDS[:3])
        group['cache_read_share'] = (group['cache_read_tokens'] / inputs
                                     if inputs and not any(group['missing_token_fields'][field]
                                                           for field in TOKEN_FIELDS[:3]) else None)
        for field in TOKEN_FIELDS:
            if group['missing_token_fields'][field] == group['rows']:
                group[field] = None
        if group['missing_cost_rows']:
            group['estimated_cost_usd'] = None
    return sorted(groups.values(), key=lambda row: tuple(str(row[k] or '') for k in keys))


def since_time(value):
    stamp = datetime.fromisoformat(value.replace('Z', '+00:00'))
    return stamp.replace(tzinfo=UTC) if stamp.tzinfo is None else stamp.astimezone(UTC)


def stats(db, since=None, prices=None):
    if prices is None:
        prices = json.loads(PRICE_TABLE.read_text())['models']
    boundary = since_time(since) if since else None
    def included(row):
        return not boundary or (row['timestamp'] is not None and since_time(row['timestamp']) >= boundary)
    all_usage = [dict(row) for row in db.execute('SELECT * FROM usage')]
    selected = [row for row in all_usage if included(row)]
    native = {(row['adapter'], row['session']) for row in selected if row['source'] != 'fabric' and row['session']}
    usage = [row for row in selected if row['source'] != 'fabric' or (row['adapter'], row['session']) not in native]
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
    receipt_outcomes = set()
    for row in selected:
        if row['source'] == 'fabric' and row['status'] in {'rate_limited', 'usage_limited'}:
            run = row['run_id']
            receipt_outcomes.add((row['adapter'], row['model'], run, row['status']))
            day = since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
            limits[(day, row['provider'], row['account'], row['status'])] += 1
    seen = set()
    for item in db.execute('SELECT * FROM observations ORDER BY source'):
        row = dict(item)
        if not included(row) or row['status'] not in {'rate_limited', 'usage_limited'}:
            continue
        if (row['adapter'], row['model'], row['run_id'], row['status']) in receipt_outcomes:
            continue
        day = since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
        key = (row['adapter'], row['model'], row['run_id'] or row['timestamp'], row['status'], day)
        if key not in seen:
            seen.add(key)
            limits[(day, row['provider'], row['account'], row['status'])] += 1
    return {'schema_version': 1, 'since': since,
            'usage': aggregate(usage, ('provider', 'adapter', 'model'), prices),
            'usage_per_day': aggregate(usage, ('day', 'provider', 'adapter', 'account'), prices),
            'lanes': [{'status': status, 'error_class': error, 'lanes': count}
                      for (status, error), count in sorted(lanes.items(), key=str)],
            'attempts': [{'status': status, 'error_class': error, 'attempts': count}
                         for (status, error), count in sorted(attempts.items(), key=str)],
            'limits_per_day': [dict(zip(('day','provider','account','status','events'), (*key, count)))
                               for key, count in sorted(limits.items(), key=str)],
            'quota_observations': [dict(row) for row in db.execute('SELECT * FROM observations WHERE used_percent IS NOT NULL') if included(row)],
            'receipt_usage_rows_excluded': len(selected) - len(usage),
            'pending_source_rebuilds': [row[0] for row in db.execute('SELECT source FROM rebuilds ORDER BY source')]}


def render_stats(report):
    print('Costs: API-equivalent estimates in USD; missing prices or usage remain missing.')
    for row in report['usage']:
        cost = row['estimated_cost_usd']
        share = row['cache_read_share']
        print(f"{row['provider']}/{row['model']}: " +
              ' '.join(f"{field}={row[field] if row[field] is not None else 'missing'}" for field in TOKEN_FIELDS) +
              f" cache_read_share={share if share is not None else 'missing'} "
              f"estimated_cost_usd={cost if cost is not None else 'missing'}")
    for category in ('lanes', 'attempts', 'limits_per_day', 'usage_per_day', 'quota_observations'):
        print(category + ':')
        for row in report[category]:
            print('  ' + json.dumps(row, sort_keys=True))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('ingest', 'sources', 'stats'))
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--since', type=str)
    args = parser.parse_args(argv)
    try:
        if args.since:
            since_time(args.since)
        if args.since and args.command != 'stats':
            parser.error('--since applies only to stats')
        with connect() as db:
            if args.command == 'stats':
                report = stats(db, args.since)
                if args.json:
                    print(json.dumps(report, indent=2))
                else:
                    render_stats(report)
                return 0
            rows = ingest(db) if args.command == 'ingest' else [
                dict(row) for row in db.execute('SELECT * FROM sources ORDER BY source')]
        if args.json:
            print(json.dumps(rows, indent=2))
        else:
            for row in rows:
                print(f"{row['source']}: {row['status']} rows={row['row_count']} "
                      f"parser={row['parser_version']} last={row['last_ingest'] or '-'} "
                      f"{row['path_pattern']}")
        return int(args.command == 'ingest' and any(row['status'] == 'error' for row in rows))
    except (OSError, ValueError, sqlite3.Error) as exc:
        print(f'telemetry: {type(exc).__name__}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
