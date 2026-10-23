#!/usr/bin/env python3
"""Local, metadata-only provider usage ledger."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
from collections import Counter
from datetime import UTC, datetime
from functools import cache, partial
import json
import math
import os
from pathlib import Path
import re
import sqlite3
from stat import S_ISREG
import subprocess
import sys
from types import SimpleNamespace


SOURCES = ('fabric', 'route_health', 'cooldowns', 'claude', 'codex', 'opencode', 'otel', 'delivery')


def iso(moment):
    """A moment as ISO 8601 UTC with a Z suffix."""
    return moment.astimezone(UTC).isoformat().replace('+00:00', 'Z')


def now():
    return iso(datetime.now(UTC))


def state_root():
    return Path(os.environ.get('AGENT_FABRIC_STATE_ROOT',
                               str(Path.home() / '.local/state/agent-harness/fabric')))


_HOST = None


@cache
def host_identity():
    """The shared host id owner (scripts/host_identity.py)."""
    name = 'provenant_host_identity'
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name('host_identity.py'))
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]


def host_id():
    """This host's ledger id: a valid hosts.json `local_host`, otherwise the hostname's slug."""
    global _HOST
    if _HOST is None:
        root = Path(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT') or '~/.agents').expanduser()
        _HOST, error = host_identity().resolve(root / '.agent-fabric/hosts.json')
        if error:
            # Fabric hosts rejects the same file; both fall back to the hostname's slug.
            print(f'telemetry: invalid_config: {error}; using host id {_HOST}', file=sys.stderr)
    return _HOST


_ALIASES = None
_ALIAS_CONFIG_SIGNATURE = object()
# Ids this ledger has been written under, from its meta; `record_host` loads them on connect.
_LEDGER_IDS = frozenset()


def host_aliases():
    """This machine's earlier ids, excluding ids currently configured as peers."""
    global _ALIASES, _ALIAS_CONFIG_SIGNATURE
    identity = host_identity()
    path = identity.config_path()
    try:
        stat = path.stat()
        signature = (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns)
    except OSError:
        signature = None
    if _ALIASES is None or signature != _ALIAS_CONFIG_SIGNATURE:
        peers = frozenset(identity.peer_ids(path))
        _ALIASES = (frozenset(identity.aliases(host_id(), path)) | _LEDGER_IDS) - peers
        _ALIAS_CONFIG_SIGNATURE = signature
    return _ALIASES


def same_host(value, known=None):
    """This machine's rows read as one host, whichever earlier default stored them."""
    return host_identity().canonical(value, host_id(), host_aliases() if known is None else known)


def host_keys(host):
    """The stored host values a lookup for `host` must match; this machine's earlier ids included."""
    known = host_aliases()
    if same_host(host, known) != host_id():
        return [host]
    return sorted(known)


def host_clause(host):
    keys = host_keys(host)
    return f"host IN ({','.join('?' * len(keys))})", keys


def patterns():
    state = state_root()
    return {
        'fabric': '<current-and-registered-projects>/.agent-run/runs/**/attempt.json',
        'route_health': os.environ.get('AGENT_FABRIC_ROUTE_HEALTH_PATH', str(state / 'route-health.json')),
        'cooldowns': os.environ.get('FABRIC_COOLDOWNS_PATH', str(state / 'cooldowns.json')),
        **{source: ';'.join(str(root / folder / pattern) for root, _ in account_roots(source))
           for source, (_, folder, pattern) in IDENTITY_SOURCES.items()},
        'otel': str(state / 'otel/*.ndjson'),
        'delivery': '<current-and-registered-projects>/.agent-run/runs/**/{RUN.json,repairs.jsonl,findings.jsonl}',
    }


def connect():
    path = state_root() / 'telemetry.sqlite3'
    path.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path)
    db.row_factory = sqlite3.Row
    version = db.execute('PRAGMA user_version').fetchone()[0]
    if version not in (0, 1, 2, 3, 4):
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
            task_class TEXT, route TEXT, duration_ms INTEGER, provider_ms INTEGER,
            PRIMARY KEY(source,event_id,host));
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
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
        CREATE TABLE IF NOT EXISTS quota_events (
            provider TEXT NOT NULL, account TEXT NOT NULL, host TEXT NOT NULL, window TEXT NOT NULL,
            kind TEXT NOT NULL, observed_at TEXT NOT NULL, previous_observed_at TEXT,
            previous_used_percent REAL, used_percent REAL, previous_reset_at TEXT, reset_at TEXT,
            PRIMARY KEY(provider,account,host,window,observed_at));
        CREATE TABLE IF NOT EXISTS quota_series (
            provider TEXT NOT NULL, account TEXT NOT NULL, host TEXT NOT NULL, window TEXT NOT NULL,
            observed_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, source TEXT,
            used_percent REAL, window_minutes REAL, reset_at TEXT, plan TEXT, plan_detected TEXT,
            plan_tier TEXT, credit_balance REAL, reset_credits INTEGER,
            PRIMARY KEY(provider,account,host,window,observed_at));
        CREATE INDEX IF NOT EXISTS quota_series_account ON quota_series(provider,account,host,observed_at);
        CREATE TABLE IF NOT EXISTS events (
            event_id TEXT PRIMARY KEY, host TEXT NOT NULL, kind TEXT NOT NULL, ts TEXT,
            session TEXT, run_id TEXT, lane TEXT, task_id TEXT, pr INTEGER, provider TEXT,
            account TEXT, model TEXT, harness_rev TEXT, attrs TEXT NOT NULL DEFAULT '{}',
            source_ref TEXT);
        CREATE INDEX IF NOT EXISTS events_time ON events(ts);
        CREATE TABLE IF NOT EXISTS changes (
            change_id TEXT PRIMARY KEY, kind TEXT NOT NULL, pr INTEGER, merge_commit TEXT,
            merged_at TEXT NOT NULL, title TEXT, labels TEXT NOT NULL DEFAULT '[]',
            added_at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS harness_stamps (
            digest TEXT PRIMARY KEY, preset TEXT, knobs TEXT NOT NULL, first_seen TEXT);
        CREATE TABLE IF NOT EXISTS deliveries (
            run_dir TEXT PRIMARY KEY, host TEXT, run_id TEXT, risk_tier TEXT, chair_family TEXT,
            repair_cycles INTEGER, acceptance_basis TEXT, accepted INTEGER NOT NULL DEFAULT 0,
            merge_commit TEXT, ingested_at TEXT);
    ''')
    delivery_columns = {row[1] for row in db.execute('PRAGMA table_info(deliveries)')}
    if 'started_at' not in delivery_columns:
        db.execute('ALTER TABLE deliveries ADD COLUMN started_at TEXT')
    if db.execute("SELECT 1 FROM meta WHERE key='delivery_run_time_v2'").fetchone() is None:
        db.execute("DELETE FROM cursors WHERE source='delivery' AND path LIKE '%/RUN.json'")
        db.execute("INSERT OR IGNORE INTO meta VALUES ('delivery_run_time_v2', ?)", (now(),))
    observation_columns = {row[1] for row in db.execute('PRAGMA table_info(observations)')}
    if 'origin_path' not in observation_columns:
        db.execute('ALTER TABLE observations ADD COLUMN origin_path TEXT')
    usage_columns = {row[1] for row in db.execute('PRAGMA table_info(usage)')}
    for name, kind in (('run_id', 'TEXT'), ('task_id', 'TEXT'), ('attempt', 'INTEGER'),
                       ('task_class', 'TEXT'), ('route', 'TEXT'), ('duration_ms', 'INTEGER'),
                       ('provider_ms', 'INTEGER'), ('parent_run_id', 'TEXT'),
                       ('parent_task_id', 'TEXT'), ('parent_attempt', 'INTEGER'),
                       ('requested_model', 'TEXT'), ('resolved_model', 'TEXT'), ('observed_model', 'TEXT'),
                       ('delivery_run_id', 'TEXT')):
        if name not in usage_columns:
            db.execute(f'ALTER TABLE usage ADD COLUMN {name} {kind}')
    if db.execute("SELECT 1 FROM meta WHERE key='delivery_link_backfill'").fetchone() is None:
        db.execute("DELETE FROM cursors WHERE source='fabric'")
        db.execute("INSERT OR IGNORE INTO meta VALUES ('delivery_link_backfill', ?)", (now(),))
    if version in (1, 2):
        for source in SOURCES:
            paths = [row[0] for row in db.execute('SELECT path FROM cursors WHERE source=?', (source,))]
            if paths:
                db.execute('INSERT OR REPLACE INTO rebuilds VALUES (?,?)', (source, json.dumps(paths)))
                db.execute('DELETE FROM usage_files WHERE source=?', (source,))
                db.execute('DELETE FROM cursors WHERE source=?', (source,))
    if version in (1, 2, 3):
        # Version 4 reads route, task class and timing from receipts; re-read them in place.
        db.execute("DELETE FROM cursors WHERE source='fabric'")
    db.execute('PRAGMA user_version = 4')
    if db.execute("SELECT 1 FROM meta WHERE key='model_split'").fetchone() is None:
        # Re-read surviving receipts; pruned receipts keep unknown model components.
        db.execute("DELETE FROM cursors WHERE source='fabric'")
        db.execute("INSERT OR IGNORE INTO meta VALUES ('model_split', ?)", (now(),))
    migrate_event_ids(db)
    if db.execute("SELECT 1 FROM meta WHERE key='events_backfill'").fetchone() is None:
        # Events are additive tables (no version bump, so peers still read this ledger).
        # Re-read the small receipt and snapshot documents once so they backfill events.
        db.execute("DELETE FROM cursors WHERE source IN ('fabric','route_health','cooldowns')")
        db.execute("INSERT OR IGNORE INTO meta VALUES ('events_backfill', ?)", (now(),))
    if db.execute("SELECT 1 FROM meta WHERE key='session_events_backfill'").fetchone() is None:
        # A separate marker: ledgers backfilled by an earlier build may lack session events.
        # Transcripts are not re-read; sessions already in usage rows get their event here.
        backfill_session_events(db)
        db.execute("DELETE FROM meta WHERE key='events_usage_rowid'")  # the earlier, lossy watermark
        db.execute("INSERT OR IGNORE INTO meta VALUES ('session_events_backfill', ?)", (now(),))
    if db.execute("SELECT 1 FROM meta WHERE key='compaction_backfill'").fetchone() is None:
        db.execute("DELETE FROM cursors WHERE source IN ('claude','codex')")
        db.execute("INSERT OR IGNORE INTO meta VALUES ('compaction_backfill', ?)", (now(),))
    record_host(db)
    for source, pattern in patterns().items():
        db.execute('INSERT INTO sources(source,path_pattern,parser_version,status) VALUES (?,?,2,?) '
                   'ON CONFLICT(source) DO UPDATE SET path_pattern=excluded.path_pattern,parser_version=excluded.parser_version',
                   (source, pattern, 'not_ingested'))
    db.commit()
    return db


def load_hosts(db):
    """Read the ids this ledger was written under, so readers map them to this host.

    Works on a read-only connection. `host_id` is the single id an earlier build kept."""
    global _LEDGER_IDS, _ALIASES
    known = set()
    try:
        stored = db.execute("SELECT key, value FROM meta WHERE key IN ('host_id','host_ids')").fetchall()
    except sqlite3.Error:
        stored = []
    for key, value in stored:
        try:
            values = json.loads(value) if key == 'host_ids' else [value]
        except (TypeError, ValueError):
            values = []
        known.update(item for item in values if isinstance(item, str) and item)
    if frozenset(known) != _LEDGER_IDS:
        _LEDGER_IDS, _ALIASES = frozenset(known), None
    return known


def record_host(db):
    """Note the current id in the ledger's host history; rows are never relabelled.

    A rename leaves earlier rows under their old id. Readers map recorded ids to this host
    unless an id is currently configured as a peer."""
    known = load_hosts(db)
    if host_id() not in known:
        db.execute("INSERT OR REPLACE INTO meta VALUES ('host_ids', ?)", (json.dumps(sorted(known | {host_id()})),))
        load_hosts(db)


def number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and value >= 0 else None


def put_usage(db, source, event_id, origin_path, owner=None, **values):
    """Upsert one usage row; `owner` maps a file to its account for messages seen in several files."""
    values = {'source': source, 'event_id': event_id, 'host': host_id(), **values}
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
    if owner:
        (first,) = first_files(db, values['host'], (source, event_id))
        values['account'] = owner(first['path']) or values.get('account')
        db.execute('UPDATE usage SET account=? WHERE source=? AND event_id=? AND host=? AND account IS NOT ?',
                   (values['account'], source, event_id, values['host'], values['account']))
    if source != 'fabric' and values.get('session'):
        put_event(db, 'session.start', values.get('timestamp'), str(origin_path),
                  attrs={'source': source, 'adapter': values.get('adapter')},
                  **{key: values.get(key) for key in ('session', 'provider', 'account', 'model')})


def claude_record(db, row, state, path, offset, account=None, owner=None):
    delivery_owner().compact(this(), db, 'claude', row, state, path, offset)
    message = row.get('message') or {}
    usage = message.get('usage')
    if row.get('type') != 'assistant' or not isinstance(usage, dict) or not message.get('id'):
        return
    reasoning = number((usage.get('output_tokens_details') or {}).get('thinking_tokens'))
    output = number(usage.get('output_tokens'))
    put_usage(db, 'claude', message['id'], origin_path=path, owner=owner, session=row.get('sessionId'),
              adapter='claude', provider='anthropic', model=message.get('model'), observed_model=message.get('model'),
              effort=row.get('perTurnEffort'), account=account or row.get('account'),
              repository=row.get('cwd'), revision=row.get('gitRevision'), timestamp=row.get('timestamp'),
              input_tokens=number(usage.get('input_tokens')),
              cache_read_tokens=number(usage.get('cache_read_input_tokens')),
              cache_write_tokens=number(usage.get('cache_creation_input_tokens')),
              output_tokens=max(0, output - (reasoning or 0)) if output is not None else None,
              reasoning_tokens=reasoning, usage_source='message.usage')


def identity(*values):
    return hashlib.sha256(json.dumps(values, sort_keys=True).encode()).hexdigest()


def amount(value):
    """A finite non-negative number from a number or a decimal string, such as a credit balance."""
    try:
        value = float(value) if not isinstance(value, bool) else None
    except (TypeError, ValueError):
        return None
    return value if value is not None and math.isfinite(value) and value >= 0 else None


def observation(db, source, **values):
    values = {'source': source, 'host': host_id(), **values}
    values = {'event_id': identity(values), **values}
    db.execute(f"INSERT OR IGNORE INTO observations({','.join(values)}) "
               f"VALUES ({','.join('?' for _ in values)})", list(values.values()))


# Known event kinds. `kind` stays a free string: a new kind needs no migration,
# and OTLP log events keep their exporter's name as `otel.<event.name>`.
EVENT_KINDS = {
    'utility.call': 'validated utility operation attempt (route, usage and latency)',
    'lane.attempt': 'Fabric attempt reached a terminal status (attempt.json)',
    'repair.cycle': 'delivery repair cycle and failed gates before and after',
    'review.finding': 'delivery chair disposition of a review finding',
    'session.compaction': 'provider session context compaction boundary',
    'limit.rate_limited': 'provider rate limit (route health, cooldowns)',
    'limit.usage_limited': 'provider usage limit (route health, cooldowns)',
    'cooldown': 'route cooldown recorded (cooldowns.json)',
    'session.start': 'first ingested usage row of a Claude, Codex or OpenCode session',
    'plan.mismatch': 'an account\'s configured plan differs from its detected plan',
    'plan.change': 'an account\'s plan, detected plan or plan tier differs from its previous quota reading',
    'otel.user_prompt': 'OTLP log: a user prompt was submitted (length only)',
    'otel.api_request': 'OTLP log: one provider request with tokens, cost and duration',
    'otel.tool_decision': 'OTLP log: a tool permission decision',
    'otel.metric': 'OTLP metric data point (name, value, type)',
}
EVENT_FIELDS = ('session', 'run_id', 'lane', 'task_id', 'pr', 'provider', 'account', 'model', 'harness_rev')


def event_key(row):
    """An event's ID from fields stored in its own row, never a host or ledger namespace.

    Any ledger can therefore re-derive every ID: a relabel, an upgrade or a re-read
    source reconciles with the rows already there instead of duplicating them.
    """
    attrs = json.loads(row['attrs']) if isinstance(row['attrs'], str) else row['attrs']
    if row['kind'] == 'session.start':
        parts = (attrs.get('source'), row['session'])
    elif row['kind'] == 'lane.attempt':  # the receipt itself, so projects sharing run ids stay apart
        parts = (os.path.realpath(row['source_ref']),)
    elif row['kind'] == 'session.compaction':
        parts = ((row['session'], row['ts']) if row['session'] and row['ts'] else (row['source_ref'],))
    elif row['kind'] == 'plan.mismatch':
        parts = (row['provider'], row['account'], plan_key(attrs.get('plan')), plan_key(attrs.get('plan_detected')))
    elif row['kind'] in {'repair.cycle', 'review.finding'}:
        parts = (row['source_ref'],)
    else:  # snapshot and collector records (`path@offset#record`): their source plus their content
        parts = (row['source_ref'], row['ts'], *(row[name] for name in EVENT_FIELDS), attrs)
    return identity('event', row['kind'], *parts)


EVENT_IDS = 'row-v3'  # meta marker: every event ID is `event_key` of its row


def migrate_event_ids(db):
    """Re-key events from earlier builds once, under a write lock so concurrent openers cannot race."""
    marker = "SELECT value FROM meta WHERE key='event_ids'"
    if (db.execute(marker).fetchone() or [None])[0] == EVENT_IDS:
        return
    db.commit()
    db.execute('BEGIN IMMEDIATE')
    if (db.execute(marker).fetchone() or [None])[0] != EVENT_IDS:  # another opener may have finished
        # Collector refs from earlier builds lack the record position; rows of one line were
        # written in record order, so their rowid order restores it.
        position = Counter()
        for row in db.execute("SELECT rowid, source_ref FROM events WHERE kind LIKE 'otel.%' "
                              "AND source_ref NOT LIKE '%#%' ORDER BY rowid").fetchall():
            position[row['source_ref']] += 1
            db.execute('UPDATE events SET source_ref=? WHERE rowid=?',
                       (f"{row['source_ref']}#{position[row['source_ref']]}", row['rowid']))
        rekey_events(db)
        db.execute("DELETE FROM meta WHERE key='ledger_id'")
        db.execute("INSERT OR REPLACE INTO meta VALUES ('event_ids', ?)", (EVENT_IDS,))
    db.commit()


def rekey_events(db):
    """Move rows keyed by an earlier scheme to `event_key`; the earliest of any duplicates stays."""
    for row in db.execute('SELECT rowid, * FROM events ORDER BY rowid').fetchall():
        key = event_key(row)
        if key == row['event_id']:
            continue
        if db.execute('SELECT 1 FROM events WHERE event_id=?', (key,)).fetchone():
            db.execute('DELETE FROM events WHERE rowid=?', (row['rowid'],))
        else:
            db.execute('UPDATE events SET event_id=? WHERE rowid=?', (key, row['rowid']))


def put_event(db, kind, ts, source_ref, attrs=None, *, refresh=False, **fields):
    """Append one metadata event, optionally merging refreshed attrs into its existing history."""
    values = {'host': host_id(), 'kind': kind, 'ts': ts, 'source_ref': source_ref,
              'attrs': json.dumps({k: v for k, v in (attrs or {}).items() if v is not None}, sort_keys=True),
              **{name: fields.get(name) for name in EVENT_FIELDS}}
    values = {'event_id': event_key(values), **values}
    conflict = ('ON CONFLICT(event_id) DO UPDATE SET attrs=json_patch(events.attrs, excluded.attrs)'
                if refresh else 'ON CONFLICT(event_id) DO NOTHING')
    db.execute(f"INSERT INTO events({','.join(values)}) "
               f"VALUES ({','.join('?' for _ in values)}) {conflict}", list(values.values()))


# A reading's state per window; a row is appended only when one of these changes.
SERIES_STATE = ('used_percent', 'window_minutes', 'reset_at', 'plan', 'plan_detected', 'plan_tier',
                'credit_balance', 'reset_credits')
PLAN_FIELDS = ('plan', 'plan_detected', 'plan_tier')


def plan_key(value):
    return re.sub(r'[^a-z0-9]', '', str(value).lower()) if value else None


def put_quota_series(db, provider, account, host, observed_at, windows, source=None, **reading):
    """Append each window's reading when it differs from the one before it, else extend that row.

    `reading` holds the account-wide fields (plan, plan_detected, plan_tier, credit_balance,
    reset_credits). A plan field that changes between two known values emits `plan.change`.
    """
    if not observed_at:
        return
    hosts, keys = host_clause(host)
    latest = (f'SELECT * FROM quota_series WHERE provider=? AND account=? AND {hosts} {{}} AND observed_at<=? '
              'ORDER BY observed_at DESC LIMIT 1')
    before = db.execute(latest.format(''), (provider, account, *keys, observed_at)).fetchone()
    for window in windows:
        row = {name: window.get(name) for name in SERIES_STATE[:3]} | {
            name: reading.get(name) for name in SERIES_STATE[3:]}
        previous = db.execute(latest.format('AND window=?'), (provider, account, *keys, window['name'], observed_at)).fetchone()
        if previous and all(previous[name] == row[name] for name in SERIES_STATE):
            db.execute('UPDATE quota_series SET last_seen_at=MAX(last_seen_at,?) WHERE provider=? AND account=? '
                       'AND host=? AND window=? AND observed_at=?',
                       (observed_at, provider, account, previous['host'], window['name'], previous['observed_at']))
            continue
        db.execute('INSERT OR IGNORE INTO quota_series(provider,account,host,window,observed_at,last_seen_at,source,'
                   f"{','.join(SERIES_STATE)}) VALUES ({','.join('?' * (7 + len(SERIES_STATE)))})",
                   (provider, account, host, window['name'], observed_at, observed_at, source,
                    *(row[name] for name in SERIES_STATE)))
    # Spellings differ by source (`proLite`, `prolite`); an unknown value is never a change.
    configured, detected = plan_key(reading.get('plan')), plan_key(reading.get('plan_detected'))
    if configured and detected and configured != detected:
        put_event(db, 'plan.mismatch', observed_at, f'quota_series:{provider}/{account}@{host}',
                  attrs={'plan': reading['plan'], 'plan_detected': reading['plan_detected']},
                  provider=provider, account=account)
    changed = before and any(plan_key(before[name]) and plan_key(reading.get(name))
                             and plan_key(before[name]) != plan_key(reading[name])
                             for name in PLAN_FIELDS)
    if changed:
        put_event(db, 'plan.change', observed_at, f'quota_series:{provider}/{account}@{host}',
                  attrs={**{f'{name}_from': before[name] for name in PLAN_FIELDS},
                         **{f'{name}_to': reading.get(name) for name in PLAN_FIELDS}, 'source': source},
                  provider=provider, account=account)


def codex_record(db, row, state, path, offset, account=None, owner=None):
    delivery_owner().compact(this(), db, 'codex', row, state, path, offset)
    if row.get('type') == 'compacted':
        return
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
        meta['account'] = account or meta['account']
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
            put_usage(db, 'codex', event_id, origin_path=path, owner=owner, **meta, adapter='codex', provider='openai',
                      timestamp=timestamp, observed_model=meta.get('model'),
                      input_tokens=max(0, input_count - (cached or 0) - (written or 0)) if input_count is not None else None,
                      cache_read_tokens=cached, cache_write_tokens=written,
                      output_tokens=max(0, output - (reasoning or 0)) if output is not None else None,
                      reasoning_tokens=reasoning, usage_source='token_count')
        limits = payload.get('rate_limits') or {}
        for window, value in limits.items():
            if isinstance(value, dict) and number(value.get('used_percent')) is not None:
                reset_at = value.get('resets_at')
                if isinstance(reset_at, (int, float)):
                    reset_at = iso(datetime.fromtimestamp(reset_at, UTC))
                observation(db, 'codex', origin_path=str(path), provider='openai', adapter='codex', model=state.get('model'),
                            account=meta['account'], session=state.get('session'),
                            timestamp=timestamp, window=window, used_percent=value['used_percent'],
                            window_minutes=number(value.get('window_minutes')), reset_at=reset_at)


# OTLP attributes kept beside every numeric one; any other string may carry content.
OTEL_STRINGS = frozenset({'model', 'effort', 'type', 'tool_name', 'decision', 'source', 'success', 'status_code'})


def otel_attributes(items):
    values = {}
    for item in items or ():
        value = item.get('value') or {}
        if 'intValue' in value:
            values[item.get('key')] = int(value['intValue'])  # OTLP JSON sends int64 as a string
        elif 'doubleValue' in value:
            values[item.get('key')] = float(value['doubleValue'])
        elif 'boolValue' in value:
            values[item.get('key')] = bool(value['boolValue'])
        elif 'stringValue' in value:
            values[item.get('key')] = value['stringValue']
    return values


def otel_stamp(nanos, fallback):
    try:
        return iso(datetime.fromtimestamp(int(nanos) / 1e9, UTC))
    except (TypeError, ValueError, OverflowError, OSError):
        return fallback


def otel_record(db, row, state, path, offset):
    """One collector line becomes metadata events: log records by name, metric points by value."""
    body, signal = row.get('body') or {}, row.get('signal')
    resources = body.get('resourceLogs' if signal == 'logs' else 'resourceMetrics' if signal == 'metrics' else '') or ()
    index = 0  # each record's position on its line, so identical records keep their multiplicity
    for resource in resources:
        context = otel_attributes((resource.get('resource') or {}).get('attributes'))
        linked = {'run_id': context.get('provenant.run_id'), 'lane': context.get('provenant.lane'),
                  'harness_rev': context.get('provenant.harness_rev')}
        for scope in resource.get('scopeLogs' if signal == 'logs' else 'scopeMetrics') or ():
            for record in scope.get('logRecords' if signal == 'logs' else 'metrics') or ():
                points = ([record] if signal == 'logs' else
                          ((record.get('sum') or record.get('gauge') or {}).get('dataPoints') or ()))
                for point in points:
                    index += 1
                    attributes = otel_attributes(point.get('attributes'))
                    kept = {key: value for key, value in attributes.items()
                            if key in OTEL_STRINGS or isinstance(value, (int, float))}
                    if signal == 'logs':
                        name = re.sub(r'[^a-z0-9_.]', '_', str(attributes.get('event.name') or 'log').lower())
                        kind = f'otel.{name}'
                    else:
                        kind, value = 'otel.metric', point.get('asInt', point.get('asDouble'))
                        kept.update(name=record.get('name'), value=float(value) if value is not None else None,
                                    temporality=(record.get('sum') or {}).get('aggregationTemporality'))
                    put_event(db, kind, otel_stamp(point.get('timeUnixNano'), row.get('received_at')),
                              f'{path}@{offset}#{index}', attrs=kept, session=attributes.get('session.id'),
                              model=attributes.get('model') if isinstance(attributes.get('model'), str) else None,
                              **linked)


def backfill_session_events(db):
    """One `session.start` per session already in usage rows; later rows emit it on upsert."""
    for row in db.execute(
            "SELECT source, session, MIN(timestamp) AS ts, provider, account, model, adapter, "
            "(SELECT path FROM usage_files f WHERE f.source=u.source AND f.event_id=u.event_id "
            "AND f.host=u.host LIMIT 1) AS ref FROM usage u "
            "WHERE source!='fabric' AND session IS NOT NULL GROUP BY source, session").fetchall():
        put_event(db, 'session.start', row['ts'], row['ref'],
                  attrs={'source': row['source'], 'adapter': row['adapter']},
                  **{key: row[key] for key in ('session', 'provider', 'account', 'model')})


def clear_file(db, source, path):
    events = db.execute('SELECT event_id,host FROM usage_files WHERE source=? AND path=?',
                        (source, str(path))).fetchall()
    db.execute('DELETE FROM usage_files WHERE source=? AND path=?', (source, str(path)))
    for event_id, host in events:
        db.execute('DELETE FROM usage WHERE source=? AND event_id=? AND host=? AND NOT EXISTS '
                   '(SELECT 1 FROM usage_files f WHERE f.source=usage.source '
                   'AND f.event_id=usage.event_id AND f.host=usage.host)', (source, event_id, host))
    db.execute('DELETE FROM observations WHERE source=? AND origin_path=?', (source, str(path)))


def open_verified(path, inode=None):
    """Open a regular file without following a final symlink, as the inode seen at discovery when given.

    Discovery refuses links, but a file swapped for one before this open would otherwise be read.
    """
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    stat = os.fstat(fd)
    if not S_ISREG(stat.st_mode) or (inode and (stat.st_dev, stat.st_ino) != tuple(inode)):
        os.close(fd)
        raise OSError(f'{path} changed since discovery')
    return os.fdopen(fd, 'rb'), stat


def file_identity(stream, stat, length):
    digest = hashlib.sha256()
    stream.seek(0)
    remaining = length
    while remaining:
        chunk = stream.read(min(remaining, 1024 * 1024))
        if not chunk:
            break
        digest.update(chunk)
        remaining -= len(chunk)
    return [stat.st_dev, stat.st_ino, length, digest.hexdigest()]


def ingest_jsonl(db, source, path, parse, inode=None):
    stream, stat = open_verified(path, inode)
    with stream:
        ingest_stream(db, source, path, parse, stream, stat)


def ingest_stream(db, source, path, parse, stream, stat):
    old = db.execute('SELECT * FROM cursors WHERE source=? AND path=?', (source, str(path))).fetchone()
    state = json.loads(old['state']) if old else {}
    previous_file = state.get('_file')
    unchanged = old and old['size'] == stat.st_size and old['mtime'] == stat.st_mtime_ns
    current_id = [stat.st_dev, stat.st_ino]
    if unchanged and previous_file and previous_file[:2] == current_id:
        return
    rewritten = bool(old and (stat.st_size < old['size'] or
                     (previous_file and (previous_file[2] < old['size'] or
                      file_identity(stream, stat, previous_file[2]) != previous_file))))
    if rewritten:
        clear_file(db, source, path)
        state = {}
    offset = old['offset'] if old and not rewritten else 0
    stream.seek(offset)
    while stream.tell() < stat.st_size:
        line = stream.readline(stat.st_size - stream.tell())
        if not line.endswith(b'\n'):
            break
        row = json.loads(line)
        parse(db, row, state, path, offset)
        offset = stream.tell()
    state['_file'] = file_identity(stream, stat, stat.st_size)
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               (source, str(path), stat.st_size, stat.st_mtime_ns, offset, json.dumps(state)))


def readonly(path):
    return sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True)


@cache
def _fabric_roots(cwd, registry):
    projects = {Path(cwd)}
    registry = Path(registry) / 'fabric.sqlite3'
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
    return sorted(roots)


def fabric_roots():
    """Shared project and common-checkout artifact roots for Fabric-owned files."""
    registry = os.environ.get('AGENT_FABRIC_STATE_DIRECTORY',
                              str(Path.home() / '.local/state/agent-harness/fabric'))
    return _fabric_roots(str(Path.cwd()), registry)


def fabric_paths(name='attempt.json'):
    return sorted({path for root in fabric_roots() for path in root.glob(f'runs/**/{name}')
                   if path.is_file() and not path.is_symlink()})


# Native sources: the provider whose CLI identity directories hold them, and their place inside one.
IDENTITY_SOURCES = {'claude': ('claude', 'projects', '**/*.jsonl'),
                    'codex': ('codex', 'sessions', '**/rollout-*.jsonl'),
                    'opencode': ('opencode', '', 'opencode.db')}


@cache
def identity_owner():
    spec = importlib.util.spec_from_file_location('provenant_telemetry_identity', Path(__file__).with_name('provider_identity.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def account_roots(source):
    """(identity directory, account label) for each directory this source reads, in rank order.

    Configured `identity_dir` paths come first, then `aliases` (another directory signed in to
    the same account, such as the CLI default), then the provider default as `default`.
    """
    owner = identity_owner()
    provider = IDENTITY_SOURCES[source][0]
    default = owner.default_identity(provider)
    try:
        entries = owner.configured().get(provider) or {}
    except ValueError:
        entries = {}
    ranked = []
    for label, entry in sorted(entries.items()):
        entry = entry if isinstance(entry, dict) else {}
        ranked += [(1, alias, label) for alias in entry.get('aliases') or () if isinstance(alias, str)]
        ranked.append((0 if entry.get('identity_dir') else 2, entry.get('identity_dir') or default, label))
    ranked.append((3, default, 'default'))
    roots = {}
    for _, path, label in sorted(ranked, key=lambda item: item[0]):
        if path is not None:
            roots.setdefault(Path(path).expanduser(), label)
    return list(roots.items())


def file_owner(source, path, roots):
    """The account owning a file: the deepest identity directory holding it, ties by rank.

    Ingest and relabelling both use this, so every file has exactly one owner.
    """
    folder = IDENTITY_SOURCES[source][1]
    holders = [(len((root / folder).parts), -rank, label) for rank, (root, label) in enumerate(roots)
               if Path(path).is_relative_to(root / folder)]
    return max(holders)[2] if holders else None


def contained(path, root):
    """A regular file reached without any symlink below its identity directory.

    A link named like a transcript could otherwise make ingest open a credential file.
    """
    try:
        return (not path.is_symlink() and path.is_file() and
                path.resolve() == root.resolve() / path.relative_to(root))
    except (OSError, ValueError):
        return False


def native_paths(source):
    """Each file of a native source mapped to its owner's label and the inode seen at discovery."""
    _, folder, pattern = IDENTITY_SOURCES[source]
    roots = account_roots(source)
    found = {}
    for root, _ in roots:
        base = root / folder
        for path in sorted(base.glob(pattern)) if '*' in pattern else [base / pattern]:
            if path not in found and contained(path, root):
                stat = path.lstat()
                found[path] = (file_owner(source, path, roots), (stat.st_dev, stat.st_ino))
    return found


def first_files(db, host, event=None, *, earlier=False):
    """Each native message's first file by path. Its owner is the message's account in ingest and
    relabelling alike, so a message seen in several files never flips account with ingest order.
    `earlier` also matches this machine's rows stored under an earlier host id."""
    where, args = ('source=? AND event_id=?', event) if event else ('source IN (?,?,?)', IDENTITY_SOURCES)
    hosts, keys = host_clause(host) if earlier else ('host=?', [host])
    return db.execute(f'SELECT source, event_id, host, MIN(path) AS path FROM usage_files WHERE {hosts} AND {where} '
                      'GROUP BY source, event_id, host', (*keys, *args)).fetchall()


def owners():
    """Per native source, the cached file → account function shared by ingest and relabelling."""
    return {source: cache(partial(file_owner, source, roots=account_roots(source))) for source in IDENTITY_SOURCES}


def attribute_accounts(db):
    """Relabel this host's native rows by their file's owner whenever the identity directories change.

    Usage rows (through `usage_files`), Codex observations (`origin_path`) and `session.start`
    events (`source_ref`) all move together, so no table keeps a stale label.
    """
    roots = {source: account_roots(source) for source in IDENTITY_SOURCES}
    digest = identity([[source, str(root), label] for source in sorted(roots) for root, label in roots[source]])
    if (db.execute("SELECT value FROM meta WHERE key='account_roots'").fetchone() or [None])[0] == digest:
        return
    hosts, keys = host_clause(host_id())
    owner = owners()
    label = lambda source, path: owner[source](path)  # noqa: E731
    usage = [(label(row['source'], row['path']), row['source'], row['event_id'], row['host'])
             for row in first_files(db, host_id(), earlier=True)]
    db.executemany('UPDATE usage SET account=? WHERE source=? AND event_id=? AND host=? AND account IS NOT ?',
                   [(*row, row[0]) for row in usage if row[0]])
    for (path,) in db.execute(f"SELECT DISTINCT origin_path FROM observations WHERE source='codex' AND {hosts}",
                              keys).fetchall():
        if path and label('codex', path):
            db.execute(f"UPDATE observations SET account=? WHERE source='codex' AND {hosts} AND origin_path=?",
                       (label('codex', path), *keys, path))
    for row in db.execute(f"SELECT rowid, source_ref, attrs FROM events WHERE kind='session.start' AND {hosts}",
                          keys).fetchall():
        source = json.loads(row['attrs']).get('source')
        if source in IDENTITY_SOURCES and row['source_ref'] and label(source, row['source_ref']):
            db.execute('UPDATE events SET account=? WHERE rowid=?', (label(source, row['source_ref']), row['rowid']))
    db.execute("INSERT OR REPLACE INTO meta VALUES ('account_roots', ?)", (digest,))


def source_paths():
    configured = patterns()
    return {'fabric': [],
            'route_health': [Path(configured['route_health'])] if Path(configured['route_health']).is_file() else [],
            'cooldowns': [Path(configured['cooldowns'])] if Path(configured['cooldowns']).is_file() else [],
            # Native sources map each path to (account, inode); iterating them still yields paths.
            **{source: native_paths(source) for source in IDENTITY_SOURCES},
            'otel': sorted((state_root() / 'otel').glob('*.ndjson')),
            'delivery': []}


def this():
    """This module for sibling owners, also when a test loads it without registering it."""
    return sys.modules.get(__name__) or SimpleNamespace(**globals())


@cache
def delivery_owner():
    spec = importlib.util.spec_from_file_location('provenant_telemetry_delivery', Path(__file__).with_name('telemetry_delivery.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@cache
def costs_owner():
    spec = importlib.util.spec_from_file_location('provenant_telemetry_costs', Path(__file__).with_name('telemetry_costs.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@cache
def report_owner():
    spec = importlib.util.spec_from_file_location('provenant_telemetry_report', Path(__file__).with_name('telemetry_report.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PROVIDERS = {'claude': 'anthropic', 'codex': 'openai', 'cursor': 'cursor',
             'opencode': 'opencode', 'agy': 'google'}


def stamp_ms(value):
    try:
        return datetime.fromisoformat(str(value).replace('Z', '+00:00')).timestamp() * 1000
    except (TypeError, ValueError):
        return None


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
        started, ended = stamp_ms(row.get('started_at')), stamp_ms(row.get('ended_at'))
        provider_ms = number(((row.get('timing') or {}).get('phases') or {}).get('provider'))
        requested_route = row.get('requested_route') or {}
        parent = row.get('parent') or {}
        if not row.get('usage'):
            if input_count is not None:
                input_count = max(0, input_count - (cached or 0) - (written or 0))
            if output is not None and adapter in {'codex', 'claude', 'cursor'}:
                output = max(0, output - (reasoning or 0))
        fields = dict(session=row.get('session_id'), run_id=row.get('run_id'),
                      delivery_run_id=row.get('delivery_run_id'),
                      parent_run_id=parent.get('run_id'), parent_task_id=parent.get('task_id'),
                      parent_attempt=parent.get('attempt'),
                      task_id=row.get('task_id'), attempt=row.get('attempt'), lane='/'.join(str(row.get(k, '')) for k in ('run_id','task_id','attempt')),
                      graph_node=row.get('graph_node'), adapter=adapter,
                      provider=provenance.get('provider') or PROVIDERS.get(adapter, adapter),
                      model=provenance.get('observed_model') or provenance.get('resolved_model') or requested.get('model'),
                      requested_model=requested.get('model'), resolved_model=provenance.get('resolved_model'),
                      observed_model=provenance.get('observed_model'),
                      effort=provenance.get('effort_applied') or requested.get('effort'),
                      account=row.get('account') or provenance.get('account'),
                      repository=row.get('workspace_root') or row.get('cwd'),
                      revision=row.get('repository_revision'), timestamp=row.get('ended_at') or row.get('started_at'),
                      input_tokens=input_count, cache_read_tokens=cached, cache_write_tokens=written,
                      output_tokens=output, reasoning_tokens=reasoning,
                      usage_source=usage.get('usage_source') or row.get('usage_source') or usage.get('source'),
                      status=row.get('status') or row.get('state'), error_class=receipt_error_class(row),
                      task_class=row.get('task_class') or requested_route.get('task_class') or None,
                      route=requested_route.get('alias') or None,
                      duration_ms=round(ended - started) if started is not None and ended is not None and ended >= started else None,
                      provider_ms=round(provider_ms) if provider_ms is not None else None)
        put_usage(db, source, identity(str(path.resolve())), origin_path=path, **fields)
        if row.get('ended_at') and fields['status'] not in (None, 'running'):
            profile = row.get('profile') if isinstance(row.get('profile'), dict) else {}
            retry_of = row.get('retry_of')
            escalated_from = (f"{fields['run_id']}/{fields['task_id']}/{int(retry_of.removeprefix('attempt-'))}"
                              if row.get('escalation_reason') and isinstance(retry_of, str)
                              and re.fullmatch(r'attempt-\d+', retry_of) else None)
            if profile.get('digest') and isinstance(profile.get('knobs'), dict):
                db.execute('INSERT INTO harness_stamps(digest,preset,knobs,first_seen) VALUES (?,?,?,?) '
                           'ON CONFLICT(digest) DO UPDATE SET first_seen=MIN(harness_stamps.first_seen,excluded.first_seen)',
                           (profile['digest'], profile.get('preset'), json.dumps(profile['knobs'], sort_keys=True), fields['timestamp']))
            put_event(db, 'lane.attempt', fields['timestamp'], str(path), refresh=True,
                      # A receipt without a profile ran the default (balanced) harness profile.
                      attrs={**{key: fields[key] for key in ('adapter', 'effort', 'attempt', 'status', 'error_class',
                                                             'task_class', 'route', 'duration_ms')},
                             'profile': profile.get('preset'), 'profile_digest': profile.get('digest'),
                             'risk_tier': profile.get('risk_tier'), 'floored': profile.get('floored'),
                             'empty_patch': row.get('empty_patch'), 'retry_of': retry_of,
                             'escalation_reason': row.get('escalation_reason'), 'escalated_from': escalated_from,
                             **({'parent': parent} if parent else {}),
                             **({'children': [{key: child.get(key) for key in ('run_id', 'task_id', 'attempt', 'status')}
                                              for child in row['children']]} if row.get('children') else {})},
                      harness_rev=provenance.get('harness_rev'), **fields)
    else:
        entries = row.get('routes', {}) if source == 'route_health' else row.get('cooldowns', {})
        for entry in entries.values():
            for item in entry.get('recent', []) if source == 'route_health' else [entry]:
                adapter = entry.get('adapter')
                values = dict(adapter=adapter, provider=PROVIDERS.get(adapter, adapter),
                              model=entry.get('model'), account=entry.get('account'),
                              timestamp=item.get('at') or item.get('recorded_at'),
                              run_id=item.get('run_id') or item.get('source_run'),
                              status=item.get('status'), reset_at=item.get('cooling_until'))
                observation(db, source, origin_path=str(path), **values)
                kind = ('cooldown' if source == 'cooldowns' else
                        f"limit.{values['status']}" if values['status'] in {'rate_limited', 'usage_limited'} else None)
                if kind:
                    put_event(db, kind, values['timestamp'], str(path),
                              attrs={key: values[key] for key in ('adapter', 'status', 'reset_at')},
                              **{key: values[key] for key in ('run_id', 'provider', 'account', 'model')})
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               (source, str(path), stat.st_size, stat.st_mtime_ns, stat.st_size, '{}'))


# OpenCode 2.0.30 moved assistant turns from `message` to `session_message`, keeping ids.
# A turn in both tables is read only from `session_message`, so it has one usage row and source.
# Skipped rows come back with NULL data rather than being filtered, so they still advance the watermark.
OPENCODE_TABLES = {
    'message': ("SELECT id,session_id,time_created,time_updated,CASE WHEN {migrated} THEN NULL ELSE data END,NULL "
                "FROM message WHERE time_updated>=? ORDER BY time_updated,id"),
    'session_message': ("SELECT m.id,m.session_id,m.time_created,m.time_updated,"
                        "CASE WHEN m.type='assistant' THEN m.data END,s.directory "
                        "FROM session_message m LEFT JOIN session_v2 s ON s.id=m.session_id "
                        "WHERE m.time_updated>=? ORDER BY m.time_updated,m.id"),
}


def opencode_turn(row):
    """Provider, model and repository from either message layout; None for a non-assistant row."""
    model = row.get('model') if isinstance(row.get('model'), dict) else {}
    if row.get('role', 'assistant') != 'assistant':
        return None
    return (row.get('providerID') or model.get('providerID'), row.get('modelID') or model.get('id'),
            (row.get('path') or {}).get('cwd'))


def ingest_opencode(db, path, account=None, owner=None, inode=None):
    old = db.execute('SELECT state FROM cursors WHERE source=? AND path=?', ('opencode', str(path))).fetchone()
    saved = json.loads(old['state']) if old else {}
    marks = {'message': saved.get('max_updated', 0), **saved.get('tables', {})}
    pinned, stat = open_verified(path, inode)
    file_id = [stat.st_dev, stat.st_ino]
    with pinned, readonly(path) as source:
        source.execute('BEGIN')  # One snapshot for the watermarks and the rows they cover.
        present = {name for (name,) in source.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        # SQLite opens by name, so confirm the name still held the pinned file once it had.
        open_verified(path, file_id)[0].close()
        tables = [name for name in OPENCODE_TABLES if name in present]
        latest = {name: source.execute(f'SELECT MAX(time_updated) FROM {name}').fetchone()[0] or 0 for name in tables}
        if old and (saved.get('file_id') != file_id or any(latest[name] < marks.get(name, 0) for name in tables)):
            clear_file(db, 'opencode', path)
            marks = {}
        for name in tables:
            watermark = marks.get(name, 0)
            # Inclusive watermark covers messages updated during an earlier ingest.
            for message_id, session, created, updated, data, directory in source.execute(
                    OPENCODE_TABLES[name].format(migrated='id IN (SELECT id FROM session_message)'
                                                 if 'session_message' in present else '0'), (watermark,)):
                watermark = max(watermark, updated)
                if data is None:
                    continue
                row = json.loads(data)
                turn = opencode_turn(row)
                if turn is None:
                    continue
                provider, model, cwd = turn
                usage = row.get('tokens') or {}
                cache = usage.get('cache') or {}
                stamp = (row.get('time') or {}).get('created') or created
                timestamp = iso(datetime.fromtimestamp(stamp / 1000, UTC))
                put_usage(db, 'opencode', message_id, origin_path=path, owner=owner, session=session, adapter='opencode',
                          provider=provider, model=model, timestamp=timestamp, repository=cwd or directory,
                          account=account or row.get('account'), input_tokens=number(usage.get('input')),
                          output_tokens=number(usage.get('output')), reasoning_tokens=number(usage.get('reasoning')),
                          cache_read_tokens=number(cache.get('read')), cache_write_tokens=number(cache.get('write')),
                          reported_cost=number(row.get('cost')), usage_source=f'{name}.tokens')
            marks[name] = watermark
    stat = path.stat()
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               ('opencode', str(path), stat.st_size, stat.st_mtime_ns, 0,
                json.dumps({'max_updated': marks.get('message', 0), 'tables': marks, 'file_id': file_id})))


def ingest(db):
    with db:
        attribute_accounts(db)
    owners_by_source = owners()
    for source, paths in source_paths().items():
        errors = 0
        # Fabric-rooted sources read the registry, so a bad registry marks only them as errors.
        if source in {'fabric', 'delivery'}:
            try:
                paths = fabric_paths() if source == 'fabric' else delivery_owner().paths(this())
            except (OSError, ValueError, sqlite3.Error):
                errors += 1
        for path in paths:
            label, inode = paths[path] if isinstance(paths, dict) else (None, None)
            owner = {'account': label, 'owner': owners_by_source[source]} if label else {}
            try:
                with db:
                    if source == 'opencode':
                        ingest_opencode(db, path, **owner, inode=inode)
                    elif source in {'fabric', 'route_health', 'cooldowns'}:
                        ingest_document(db, source, path)
                    elif source == 'otel':
                        ingest_jsonl(db, source, path, otel_record)
                    elif source == 'delivery':
                        if path.name == 'RUN.json':
                            delivery_owner().ingest_receipt(this(), db, path)
                        else:
                            delivery_owner().prepare(db, path)
                            ingest_jsonl(db, source, path, partial(delivery_owner().record, this()))
                    else:
                        ingest_jsonl(db, source, path, partial(
                            {'codex': codex_record, 'claude': claude_record}[source], **owner), inode=inode)
            except (OSError, ValueError, TypeError, AttributeError, sqlite3.Error):
                errors += 1
        rebuild = db.execute('SELECT paths FROM rebuilds WHERE source=?', (source,)).fetchone()
        if rebuild and not errors and set(json.loads(rebuild['paths'])).issubset({str(path) for path in paths}):
            db.execute('DELETE FROM usage WHERE source=? AND NOT EXISTS '
                       '(SELECT 1 FROM usage_files f WHERE f.source=usage.source '
                       'AND f.event_id=usage.event_id AND f.host=usage.host)', (source,))
            db.execute('DELETE FROM observations WHERE source=? AND origin_path IS NULL', (source,))
            db.execute('DELETE FROM rebuilds WHERE source=?', (source,))
        if source == 'otel':
            count = db.execute("SELECT COUNT(*) FROM events WHERE kind LIKE 'otel.%'").fetchone()[0]
        elif source == 'delivery':
            count = db.execute("SELECT COUNT(*) FROM deliveries").fetchone()[0] + db.execute(
                "SELECT COUNT(*) FROM events WHERE kind IN ('repair.cycle','review.finding')").fetchone()[0]
        else:
            table = 'observations' if source in {'route_health', 'cooldowns'} else 'usage'
            count = db.execute(f'SELECT COUNT(*) FROM {table} WHERE source=?', (source,)).fetchone()[0]
        status = 'error' if errors else 'ok' if paths else 'absent'
        db.execute('UPDATE sources SET last_ingest=?,status=?,row_count=? WHERE source=?',
                   (now(), status, count, source))
        db.commit()
    return [dict(row) for row in db.execute('SELECT * FROM sources ORDER BY source')]


PRODUCT_ROOT = Path(__file__).resolve().parents[1]
PRICE_TABLE = PRODUCT_ROOT / 'config/telemetry-prices.json'
ERROR_CLASS_BY_STATUS = {
    'rate_limited': 'rate_limited', 'usage_limited': 'usage_limited',
    'auth_required': 'auth', 'model_unavailable': 'model_unavailable',
    'timed_out': 'timeout', 'startup_timeout': 'timeout', 'stalled': 'timeout',
    'permission_blocked': 'sandbox_denied', 'lost': 'provider_error', 'cancelled': 'cancelled',
}
UNCLASSIFIED_FAILURES = {'failed', 'quarantined', 'partial', 'blocked', 'rejected', 'interrupted',
                         'writer_changed', 'tool_missing'}


def receipt_error_class(row):
    """Keep legacy terminal receipts useful when they predate error_class."""
    if row.get('error_class'):
        return row['error_class']
    status = row.get('status') or row.get('state')
    if status in ERROR_CLASS_BY_STATUS:
        return ERROR_CLASS_BY_STATUS[status]
    # Only terminal failures get the generic class; queued or active lanes stay unclassified.
    return 'provider_error' if status in UNCLASSIFIED_FAILURES else None


def since_time(value):
    stamp = datetime.fromisoformat(value.replace('Z', '+00:00'))
    return stamp.replace(tzinfo=UTC) if stamp.tzinfo is None else stamp.astimezone(UTC)


@cache
def quota_owner():
    spec = importlib.util.spec_from_file_location('provenant_telemetry_quota', Path(__file__).with_name('provider_quota.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

@cache
def host_owner():
    spec = importlib.util.spec_from_file_location('provenant_telemetry_hosts', Path(__file__).with_name('telemetry_hosts.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

@cache
def changes_owner():
    spec = importlib.util.spec_from_file_location('provenant_telemetry_changes', Path(__file__).with_name('telemetry_changes.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def report_changes(db, args):
    """`changes`, `stats --compare` and `retro`; full reports with --json, compact text otherwise."""
    owner = changes_owner()
    span = args.window or owner.DEFAULT_WINDOW
    try:
        if args.compare:
            report = owner.compare(this(), db, args.compare, span)
            text = lambda: owner.render_compare(this(), report)
        elif args.command == 'retro':
            report = owner.retro(this(), db, args.since, span)
            text = lambda: owner.render_retro(this(), report, report_owner().ROW_LIMIT)
        else:
            if args.sync:
                _, pending, incomplete = owner.sync(this(), db)
                if pending:
                    print(f'telemetry: sync paused with {pending} time ranges left; run --sync again',
                          file=sys.stderr)
                if incomplete:
                    print(f'telemetry: {incomplete} one-second ranges exceed {owner.SEARCH_LIMIT} merges; '
                          'their change markers are incomplete', file=sys.stderr)
            elif args.add:
                owner.add(this(), db, args.add, args.at)
            report = owner.changes(db, args.since)
            text = lambda: owner.render_changes(this(), report, report_owner().ROW_LIMIT)
    except owner.ChangeError as exc:
        print(f'telemetry: {exc}', file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2) if args.json else text())
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('ingest', 'sources', 'stats', 'changes', 'retro', 'accepted'))
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--detail', action='store_true')
    parser.add_argument('--since', type=str)
    parser.add_argument('--hosts', nargs='+', help='all or configured host names (stats only)')
    parser.add_argument('--compare', metavar='PR|REV|DATE', help='before/after metrics around a change (stats only)')
    parser.add_argument('--window', help='each side of --compare, or retro compares: 24h, 7d, 2w')
    parser.add_argument('--sync', action='store_true', help='read merged PRs with gh (changes only)')
    parser.add_argument('--add', metavar='LABEL', help='manual change marker (changes only; needs --at)')
    parser.add_argument('--at', metavar='ISO', help='time of a manual marker')
    args = parser.parse_args(argv)
    for flag, commands in (('since', {'stats', 'changes', 'retro', 'accepted'}), ('hosts', {'stats'}), ('detail', {'stats'}),
                           ('compare', {'stats'}), ('window', {'stats', 'retro'}), ('sync', {'changes'}),
                           ('add', {'changes'}), ('at', {'changes'})):
        if getattr(args, flag) and args.command not in commands:
            parser.error(f"--{flag} applies only to {' or '.join(sorted(commands))}")
    if args.compare and (args.hosts or args.detail or args.since):
        parser.error('--compare takes only --window and --json')
    if args.window and args.command == 'stats' and not args.compare:
        parser.error('--window applies to --compare')
    if bool(args.add) != bool(args.at) or (args.add and args.sync):
        parser.error('--add needs --at, and not --sync')
    try:
        if args.since:
            since_time(args.since)
        with connect() as db:
            if args.command in {'changes', 'retro'} or args.compare:
                return report_changes(db, args)
            if args.command == 'stats':
                report = host_owner().collect(this(), db, args.since, args.hosts) if args.hosts else report_owner().stats(this(), db, args.since)
                if args.json:
                    print(json.dumps(report, indent=2))
                else:
                    report_owner().render_stats(this(), report, detail=args.detail)
                return 0
            if args.command == 'accepted':
                return delivery_owner().run_accepted(this(), db, args.since, args.json)
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
