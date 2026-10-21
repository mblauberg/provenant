#!/usr/bin/env python3
"""Local, metadata-only provider usage ledger."""

from __future__ import annotations

import argparse
import hashlib
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
        'fabric': '<registered-projects>/.agent-run/runs/**/attempt.json',
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
    if version not in (0, 1):
        db.close()
        raise ValueError(f'unsupported telemetry schema version: {version}')
    db.executescript('''
        CREATE TABLE IF NOT EXISTS sources (
            source TEXT PRIMARY KEY, path_pattern TEXT NOT NULL,
            parser_version INTEGER NOT NULL, last_ingest TEXT,
            row_count INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS usage (
            source TEXT NOT NULL, event_id TEXT NOT NULL, host TEXT NOT NULL,
            session TEXT, lane TEXT, graph_node TEXT, adapter TEXT, provider TEXT,
            model TEXT, effort TEXT, account TEXT, repository TEXT, revision TEXT,
            timestamp TEXT, input_tokens INTEGER, cache_read_tokens INTEGER,
            cache_write_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
            usage_source TEXT, reported_cost REAL, status TEXT, error_class TEXT,
            PRIMARY KEY(source,event_id,host));
        CREATE INDEX IF NOT EXISTS usage_time ON usage(timestamp);
        CREATE TABLE IF NOT EXISTS cursors (
            source TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
            mtime INTEGER NOT NULL, offset INTEGER NOT NULL, state TEXT NOT NULL,
            PRIMARY KEY(source,path));
        CREATE TABLE IF NOT EXISTS observations (
            event_id TEXT PRIMARY KEY, source TEXT NOT NULL, host TEXT NOT NULL,
            provider TEXT, adapter TEXT, model TEXT, account TEXT, session TEXT,
            lane TEXT, run_id TEXT, timestamp TEXT, window TEXT, used_percent REAL,
            window_minutes REAL, reset_at TEXT, status TEXT);
        PRAGMA user_version = 1;
    ''')
    for source, pattern in patterns().items():
        db.execute('INSERT INTO sources(source,path_pattern,parser_version,status) VALUES (?,?,1,?) '
                   'ON CONFLICT(source) DO UPDATE SET path_pattern=excluded.path_pattern',
                   (source, pattern, 'not_ingested'))
    db.commit()
    return db


def number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and value >= 0 else None


def put_usage(db, source, event_id, **values):
    values = {'source': source, 'event_id': event_id, 'host': socket.gethostname(), **values}
    columns = list(values)
    update = ','.join(f'{key}=excluded.{key}' for key in columns if key not in {'source', 'event_id', 'host'})
    db.execute(f"INSERT INTO usage({','.join(columns)}) VALUES ({','.join('?' for _ in columns)}) "
               f'ON CONFLICT(source,event_id,host) DO UPDATE SET {update}', list(values.values()))


def claude_record(db, row, state, path, offset):
    message = row.get('message') or {}
    usage = message.get('usage')
    if row.get('type') != 'assistant' or not isinstance(usage, dict) or not message.get('id'):
        return
    reasoning = number((usage.get('output_tokens_details') or {}).get('thinking_tokens'))
    output = number(usage.get('output_tokens'))
    put_usage(db, 'claude', message['id'], session=row.get('sessionId'),
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
            previous = state.get('total') or {}
            reset = any(number(v) is not None and v < previous.get(k, 0) for k, v in total.items())
            delta = {k: max(0, v - (0 if reset else previous.get(k, 0)))
                     for k, v in total.items() if number(v) is not None}
            state['total'] = total
            event_id = identity(meta.get('session') or str(path), offset)
        else:
            delta = info.get('last_token_usage') or {}
            event_id = identity(meta.get('session') or str(path), timestamp, delta)
        if any(delta.values()):
            cached = delta.get('cached_input_tokens')
            written = delta.get('cache_write_input_tokens')
            reasoning = delta.get('reasoning_output_tokens')
            input_count, output = delta.get('input_tokens'), delta.get('output_tokens')
            put_usage(db, 'codex', event_id, **meta, adapter='codex', provider='openai',
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
                observation(db, 'codex', provider='openai', adapter='codex', model=state.get('model'),
                            account=state.get('account'), session=state.get('session'),
                            timestamp=timestamp, window=window, used_percent=value['used_percent'],
                            window_minutes=number(value.get('window_minutes')), reset_at=reset_at)


def ingest_jsonl(db, source, path, parse):
    stat = path.stat()
    old = db.execute('SELECT * FROM cursors WHERE source=? AND path=?', (source, str(path))).fetchone()
    if old and old['size'] == stat.st_size and old['mtime'] == stat.st_mtime_ns:
        return
    offset = old['offset'] if old and stat.st_size >= old['size'] else 0
    # A same-size rewrite must be reparsed; append-only sources retain their context.
    if old and stat.st_size == old['size']:
        offset = 0
    state = json.loads(old['state']) if old and offset else {}
    with path.open('rb') as stream:
        stream.seek(offset)
        while line := stream.readline():
            if not line.endswith(b'\n'):
                break
            row = json.loads(line)
            parse(db, row, state, path, offset)
            offset = stream.tell()
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
    return {'fabric': fabric_paths(),
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
        put_usage(db, source, identity(row.get('run_id'), row.get('task_id'), row.get('attempt')),
                  session=row.get('session_id'), lane='/'.join(str(row.get(k, '')) for k in ('run_id','task_id','attempt')),
                  graph_node=row.get('graph_node'), adapter=adapter,
                  provider=provenance.get('provider') or PROVIDERS.get(adapter, adapter),
                  model=provenance.get('observed_model') or provenance.get('resolved_model') or requested.get('model'),
                  effort=provenance.get('effort_applied') or requested.get('effort'),
                  account=row.get('account') or provenance.get('account'),
                  repository=row.get('workspace_root') or row.get('cwd'),
                  revision=row.get('repository_revision'), timestamp=row.get('ended_at') or row.get('started_at'),
                  input_tokens=number(usage.get('input_tokens')),
                  cache_read_tokens=number(usage.get('cache_read_tokens', usage.get('cached_input_tokens'))),
                  cache_write_tokens=number(usage.get('cache_write_tokens', row.get('cache_write_tokens'))),
                  output_tokens=number(usage.get('output_tokens')),
                  reasoning_tokens=number(usage.get('reasoning_tokens', row.get('reasoning_tokens'))),
                  usage_source=usage.get('usage_source') or row.get('usage_source') or usage.get('source'),
                  status=row.get('status') or row.get('state'), error_class=row.get('error_class'))
    else:
        entries = row.get('routes', {}) if source == 'route_health' else row.get('cooldowns', {})
        for entry in entries.values():
            for item in entry.get('recent', []) if source == 'route_health' else [entry]:
                adapter = entry.get('adapter')
                observation(db, source, adapter=adapter, provider=PROVIDERS.get(adapter, adapter),
                            model=entry.get('model'), account=entry.get('account'),
                            timestamp=item.get('at') or item.get('recorded_at'),
                            run_id=item.get('run_id') or item.get('source_run'),
                            status=item.get('status'), reset_at=item.get('cooling_until'))
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               (source, str(path), stat.st_size, stat.st_mtime_ns, stat.st_size, '{}'))


def ingest_opencode(db, path):
    old = db.execute('SELECT state FROM cursors WHERE source=? AND path=?', ('opencode', str(path))).fetchone()
    watermark = json.loads(old['state']).get('max_updated', 0) if old else 0
    with readonly(path) as source:
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
            put_usage(db, 'opencode', message_id, session=session, adapter='opencode',
                      provider=row.get('providerID'), model=row.get('modelID'),
                      timestamp=timestamp, repository=(row.get('path') or {}).get('cwd'),
                      account=row.get('account'), input_tokens=number(usage.get('input')),
                      output_tokens=number(usage.get('output')), reasoning_tokens=number(usage.get('reasoning')),
                      cache_read_tokens=number(cache.get('read')), cache_write_tokens=number(cache.get('write')),
                      reported_cost=number(row.get('cost')), usage_source='message.tokens')
    stat = path.stat()
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               ('opencode', str(path), stat.st_size, stat.st_mtime_ns, 0, json.dumps({'max_updated': watermark})))


def ingest(db):
    for source, paths in source_paths().items():
        errors = 0
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
        table = 'observations' if source in {'route_health', 'cooldowns'} else 'usage'
        count = db.execute(f'SELECT COUNT(*) FROM {table} WHERE source=?', (source,)).fetchone()[0]
        status = 'error' if errors else 'ok' if paths else 'absent'
        db.execute('UPDATE sources SET last_ingest=?,status=?,row_count=? WHERE source=?',
                   (now(), status, count, source))
        db.commit()
    return [dict(row) for row in db.execute('SELECT * FROM sources ORDER BY source')]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('ingest', 'sources'))
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(argv)
    try:
        with connect() as db:
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
