"""Record parsers and file ingest for the local telemetry ledger.

Functions that need the ledger core take the loaded `telemetry` module, as
telemetry_costs does, so this module never imports it.
"""

from __future__ import annotations

from datetime import UTC, datetime
from functools import partial
import hashlib
import json
import os
import re
import sqlite3
from stat import S_ISREG


def claude_record(telemetry, db, row, state, path, offset, account=None, owner=None):
    telemetry.delivery_owner().compact(telemetry, db, 'claude', row, state, path, offset)
    message = row.get('message') or {}
    usage = message.get('usage')
    if row.get('type') != 'assistant' or not isinstance(usage, dict) or not message.get('id'):
        return
    reasoning = telemetry.number((usage.get('output_tokens_details') or {}).get('thinking_tokens'))
    output = telemetry.number(usage.get('output_tokens'))
    telemetry.put_usage(db, 'claude', message['id'], origin_path=path, owner=owner, session=row.get('sessionId'),
              adapter='claude', provider='anthropic', model=message.get('model'), observed_model=message.get('model'),
              effort=row.get('perTurnEffort'), account=account or row.get('account'),
              repository=row.get('cwd'), revision=row.get('gitRevision'), timestamp=row.get('timestamp'),
              input_tokens=telemetry.number(usage.get('input_tokens')),
              cache_read_tokens=telemetry.number(usage.get('cache_read_input_tokens')),
              cache_write_tokens=telemetry.number(usage.get('cache_creation_input_tokens')),
              output_tokens=max(0, output - (reasoning or 0)) if output is not None else None,
              reasoning_tokens=reasoning, usage_source='message.usage')


def codex_record(telemetry, db, row, state, path, offset, account=None, owner=None):
    telemetry.delivery_owner().compact(telemetry, db, 'codex', row, state, path, offset)
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
                'output_tokens', 'reasoning_output_tokens', 'total_tokens'} and telemetry.number(v) is not None}
            previous = state.get('total') or {}
            reset = any(telemetry.number(v) is not None and v < previous.get(k, 0) for k, v in total.items())
            delta = {k: max(0, v - (0 if reset else previous.get(k, 0)))
                     for k, v in total.items() if telemetry.number(v) is not None}
            state['total'] = total
            event_id = telemetry.identity(meta.get('session') or str(path), offset)
        else:
            delta = {k: v for k, v in (info.get('last_token_usage') or {}).items()
                     if k in {'input_tokens', 'cached_input_tokens', 'cache_write_input_tokens',
                              'output_tokens', 'reasoning_output_tokens', 'total_tokens'} and telemetry.number(v) is not None}
            event_id = telemetry.identity(meta.get('session') or str(path), timestamp, delta)
        if any(delta.values()):
            cached = delta.get('cached_input_tokens')
            written = delta.get('cache_write_input_tokens')
            reasoning = delta.get('reasoning_output_tokens')
            input_count, output = delta.get('input_tokens'), delta.get('output_tokens')
            telemetry.put_usage(db, 'codex', event_id, origin_path=path, owner=owner, **meta, adapter='codex', provider='openai',
                      timestamp=timestamp, observed_model=meta.get('model'),
                      input_tokens=max(0, input_count - (cached or 0) - (written or 0)) if input_count is not None else None,
                      cache_read_tokens=cached, cache_write_tokens=written,
                      output_tokens=max(0, output - (reasoning or 0)) if output is not None else None,
                      reasoning_tokens=reasoning, usage_source='token_count')
        limits = payload.get('rate_limits') or {}
        for window, value in limits.items():
            if isinstance(value, dict) and telemetry.number(value.get('used_percent')) is not None:
                reset_at = value.get('resets_at')
                if isinstance(reset_at, (int, float)):
                    reset_at = telemetry.iso(datetime.fromtimestamp(reset_at, UTC))
                telemetry.observation(db, 'codex', origin_path=str(path), provider='openai', adapter='codex', model=state.get('model'),
                            account=meta['account'], session=state.get('session'),
                            timestamp=timestamp, window=window, used_percent=value['used_percent'],
                            window_minutes=telemetry.number(value.get('window_minutes')), reset_at=reset_at)


# Only known event kinds and telemetry fields are persisted. Exporters also
# attach prompt, response, account and tool payloads to the same records.
OTEL_EVENT_ATTRIBUTES = {
    'user_prompt': frozenset({'prompt_length'}),
    'tool_decision': frozenset({'tool_name', 'decision'}),
}
OTEL_EVENT_ATTRIBUTES.update({f'codex.{name}': fields for name, fields in OTEL_EVENT_ATTRIBUTES.items()})
OTEL_ALLOWED_LOGS = frozenset({'api_request', 'codex.api_request', *OTEL_EVENT_ATTRIBUTES})
OTEL_STRING_ATTRIBUTES = frozenset({'model', 'effort', 'type', 'tool_name', 'decision'})
OTEL_BOOL_ATTRIBUTES = frozenset({'success'})
OTEL_NUMERIC_ATTRIBUTES = frozenset({
    'input_tokens', 'output_tokens', 'cache_read_tokens', 'cache_creation_tokens',
    'cache_creation_5m_tokens', 'cache_creation_1h_tokens', 'cost_usd', 'cost_usd_micros',
    'duration_ms', 'ttft_ms', 'prompt_length', 'response_length', 'tool_input_size_bytes',
    'tool_result_size_bytes', 'attempt', 'http.response.status_code',
})


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


def otel_stamp(telemetry, nanos, fallback):
    try:
        return telemetry.iso(datetime.fromtimestamp(int(nanos) / 1e9, UTC))
    except (TypeError, ValueError, OverflowError, OSError):
        return fallback


def otel_record(telemetry, db, row, state, path, offset):
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
                    if signal == 'logs':
                        raw_name = str(attributes.get('event.name') or 'log').lower()
                        if raw_name not in OTEL_ALLOWED_LOGS:
                            continue
                        name = re.sub(r'[^a-z0-9_.]', '_', raw_name)
                        kind = f'otel.{name}'
                    else:
                        kind, value = 'otel.metric', point.get('asInt', point.get('asDouble'))
                    if signal == 'logs' and raw_name in OTEL_EVENT_ATTRIBUTES:
                        allowed = OTEL_EVENT_ATTRIBUTES[raw_name]
                    else:
                        allowed = (OTEL_STRING_ATTRIBUTES | OTEL_BOOL_ATTRIBUTES |
                                   OTEL_NUMERIC_ATTRIBUTES)
                    kept = {key: value for key, value in attributes.items()
                            if key in allowed and (key in OTEL_STRING_ATTRIBUTES and isinstance(value, str) or
                                                   key in OTEL_BOOL_ATTRIBUTES and isinstance(value, bool) or
                                                   key in OTEL_NUMERIC_ATTRIBUTES and isinstance(value, (int, float)))}
                    if signal != 'logs':
                        kept.update(name=record.get('name'), value=float(value) if value is not None else None,
                                    temporality=(record.get('sum') or {}).get('aggregationTemporality'))
                    telemetry.put_event(db, kind, otel_stamp(telemetry, point.get('timeUnixNano'), row.get('received_at')),
                              f'{path}@{offset}#{index}', attrs=kept, session=attributes.get('session.id'),
                              model=attributes.get('model') if isinstance(attributes.get('model'), str) else None,
                              **linked)


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


PROVIDERS = {'claude': 'anthropic', 'codex': 'openai', 'cursor': 'cursor',
             'opencode': 'opencode', 'agy': 'google'}


def stamp_ms(value):
    try:
        return datetime.fromisoformat(str(value).replace('Z', '+00:00')).timestamp() * 1000
    except (TypeError, ValueError):
        return None


def ingest_document(telemetry, db, source, path):
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
        input_count = telemetry.number(usage.get('input_tokens'))
        cached = telemetry.number(usage.get('cache_read_tokens', usage.get('cached_input_tokens')))
        written = telemetry.number(usage.get('cache_write_tokens', row.get('cache_write_tokens')))
        output = telemetry.number(usage.get('output_tokens'))
        reasoning = telemetry.number(usage.get('reasoning_tokens', row.get('reasoning_tokens')))
        started, ended = stamp_ms(row.get('started_at')), stamp_ms(row.get('ended_at'))
        provider_ms = telemetry.number(((row.get('timing') or {}).get('phases') or {}).get('provider'))
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
                      status=row.get('status') or row.get('state'), error_class=telemetry.receipt_error_class(row),
                      task_class=row.get('task_class') or requested_route.get('task_class') or None,
                      route=requested_route.get('alias') or None,
                      duration_ms=round(ended - started) if started is not None and ended is not None and ended >= started else None,
                      provider_ms=round(provider_ms) if provider_ms is not None else None)
        telemetry.put_usage(db, source, telemetry.identity(str(path.resolve())), origin_path=path, **fields)
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
            telemetry.put_event(db, 'lane.attempt', fields['timestamp'], str(path), refresh=True,
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
                telemetry.observation(db, source, origin_path=str(path), **values)
                kind = ('cooldown' if source == 'cooldowns' else
                        f"limit.{values['status']}" if values['status'] in {'rate_limited', 'usage_limited'} else None)
                if kind:
                    telemetry.put_event(db, kind, values['timestamp'], str(path),
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


def ingest_opencode(telemetry, db, path, account=None, owner=None, inode=None):
    old = db.execute('SELECT state FROM cursors WHERE source=? AND path=?', ('opencode', str(path))).fetchone()
    saved = json.loads(old['state']) if old else {}
    marks = {'message': saved.get('max_updated', 0), **saved.get('tables', {})}
    pinned, stat = open_verified(path, inode)
    file_id = [stat.st_dev, stat.st_ino]
    with pinned, telemetry.readonly(path) as source:
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
                timestamp = telemetry.iso(datetime.fromtimestamp(stamp / 1000, UTC))
                telemetry.put_usage(db, 'opencode', message_id, origin_path=path, owner=owner, session=session, adapter='opencode',
                          provider=provider, model=model, timestamp=timestamp, repository=cwd or directory,
                          account=account or row.get('account'), input_tokens=telemetry.number(usage.get('input')),
                          output_tokens=telemetry.number(usage.get('output')), reasoning_tokens=telemetry.number(usage.get('reasoning')),
                          cache_read_tokens=telemetry.number(cache.get('read')), cache_write_tokens=telemetry.number(cache.get('write')),
                          reported_cost=telemetry.number(row.get('cost')), usage_source=f'{name}.tokens')
            marks[name] = watermark
    stat = path.stat()
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               ('opencode', str(path), stat.st_size, stat.st_mtime_ns, 0,
                json.dumps({'max_updated': marks.get('message', 0), 'tables': marks, 'file_id': file_id})))


def ingest(telemetry, db):
    with db:
        telemetry.attribute_accounts(db)
    owners_by_source = telemetry.owners()
    for source, paths in telemetry.source_paths().items():
        errors = 0
        # Fabric-rooted sources read the registry, so a bad registry marks only them as errors.
        if source in {'fabric', 'delivery'}:
            try:
                paths = telemetry.fabric_paths() if source == 'fabric' else telemetry.delivery_owner().paths(telemetry)
            except (OSError, ValueError, sqlite3.Error):
                errors += 1
        for path in paths:
            label, inode = paths[path] if isinstance(paths, dict) else (None, None)
            owner = {'account': label, 'owner': owners_by_source[source]} if label else {}
            try:
                with db:
                    if source == 'opencode':
                        ingest_opencode(telemetry, db, path, **owner, inode=inode)
                    elif source in {'fabric', 'route_health', 'cooldowns'}:
                        ingest_document(telemetry, db, source, path)
                    elif source == 'otel':
                        ingest_jsonl(db, source, path, partial(otel_record, telemetry))
                    elif source == 'delivery':
                        if path.name == 'RUN.json':
                            telemetry.delivery_owner().ingest_receipt(telemetry, db, path)
                        else:
                            telemetry.delivery_owner().prepare(db, path)
                            ingest_jsonl(db, source, path, partial(telemetry.delivery_owner().record, telemetry))
                    else:
                        ingest_jsonl(db, source, path, partial(
                            {'codex': codex_record, 'claude': claude_record}[source], telemetry, **owner), inode=inode)
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
                   (telemetry.now(), status, count, source))
        db.commit()
    return [dict(row) for row in db.execute('SELECT * FROM sources ORDER BY source')]
