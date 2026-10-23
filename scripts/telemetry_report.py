"""The `telemetry stats` report: outcomes, peer summaries and the text tables.

Functions that need the ledger core take the loaded `telemetry` module, as
telemetry_hosts does, so this module never imports it.
"""

from __future__ import annotations

from collections import Counter
from datetime import timedelta
import itertools
import math
import statistics


SUCCESS = {'ok', 'succeeded'}


def percentile(values, share):
    values = sorted(values)
    return values[max(0, math.ceil(share * len(values)) - 1)] if values else None


OUTCOME_KEYS = ('adapter', 'model', 'effort', 'task_class', 'route')


def outcomes(rows):
    """Route quality per adapter/model@effort, task class and route from Fabric attempts."""
    keys = OUTCOME_KEYS
    groups = {}
    latest = {}
    for row in rows:
        key = tuple(row.get(k) or None for k in keys)
        group = groups.setdefault(key, {'attempts': 0, 'errors': Counter(), 'latency': [], 'speed': []})
        group['attempts'] += 1
        if row['status'] not in SUCCESS and row['error_class']:
            group['errors'][row['error_class']] += 1
        if row['duration_ms'] is not None:
            group['latency'].append(row['duration_ms'] / 1000)
        produced = (row['output_tokens'] or 0) + (row['reasoning_tokens'] or 0)
        if row['provider_ms'] and (row['output_tokens'] is not None or row['reasoning_tokens'] is not None):
            group['speed'].append(produced / (row['provider_ms'] / 1000))
        lane = (key, row['repository'], row['run_id'], row['task_id'])
        if lane not in latest or (row['attempt'] or 0) > (latest[lane]['attempt'] or 0):
            latest[lane] = row
    for (key, *_), row in latest.items():
        group = groups[key]
        group['lanes'] = group.get('lanes', 0) + 1
        group['succeeded'] = group.get('succeeded', 0) + int(row['status'] in SUCCESS)
        group['retries'] = group.get('retries', 0) + max(0, (row['attempt'] or 1) - 1)
    report = []
    for key, group in sorted(groups.items(), key=lambda item: tuple(str(v or '') for v in item[0])):
        lanes = group.get('lanes', 0)
        report.append({**dict(zip(keys, key)), 'lanes': lanes, 'succeeded': group.get('succeeded', 0),
                       'success_rate': round(group.get('succeeded', 0) / lanes, 3) if lanes else None,
                       'attempts': group['attempts'], 'retries': group.get('retries', 0),
                       'error_classes': dict(sorted(group['errors'].items())),
                       'latency_samples': len(group['latency']),
                       'latency_p50_s': percentile(group['latency'], 0.5),
                       'latency_p90_s': percentile(group['latency'], 0.9),
                       'output_tokens_per_s': round(statistics.median(group['speed']), 2) if group['speed'] else None})
    return report


def stats(telemetry, db, since=None, prices=None, *, summary=False, until=None):
    """Ledger report for [since, until); either bound may be omitted."""
    costs = telemetry.costs_owner()
    free_patterns = {}
    if prices is None:
        prices, free_patterns = costs.load_prices(telemetry)
    boundary = telemetry.since_time(since) if since else None
    end = telemetry.since_time(until) if until else None
    def included(row):
        if not boundary and not end:
            return True
        stamp = telemetry.since_time(row['timestamp']) if row['timestamp'] is not None else None
        return stamp is not None and (not boundary or stamp >= boundary) and (not end or stamp < end)
    adapters = costs.catalogue(telemetry)
    excluded = Counter()
    selected, unattributed = [], []
    # A day of margin keeps offset timestamps; `included` applies the exact bounds.
    where, parameters, order, cell_order, order_by = '', {}, "printf('%020d', rowid)", "printf('%020d', ledger_rowid)", 'rowid'
    if boundary:
        # The old full-row query was walked in index order here; ties between attempts keep that order.
        where, parameters['start'] = ' AND timestamp >= :start', (boundary - timedelta(days=1)).date().isoformat()
        # Fields are joined with \x01, below any timestamp character, so a prefix sorts first.
        order = "printf('%s' || char(1) || '%020d', COALESCE(timestamp,''), rowid)"
        cell_order = "printf('%s' || char(1) || '%020d', COALESCE(timestamp,''), ledger_rowid)"
        order_by = 'timestamp, rowid'
    current = telemetry.host_id()
    telemetry.load_hosts(db)
    aliases = telemetry.host_aliases()
    rows = {}
    def attribute(row):
        row['model_raw'] = row['model']
        row['model'] = costs.normalise_model(row['adapter'], row['provider'], row['model'], adapters)
        return costs.classify(row)
    # Receipts stay row by row (few, and lanes need whole rows); native sources group in SQL.
    for item in db.execute(f"SELECT {order} AS first_row, * FROM usage WHERE source='fabric'" + where + ' ORDER BY ' + order_by, parameters):
        row = dict(item)
        stored, row['host'] = row['host'], telemetry.same_host(row['host'], aliases)
        key = (row['source'], row['event_id'], row['host'])
        # One event stored under an earlier and the current id counts once, as the current row.
        if key not in rows or stored == current:
            rows[key] = row
    for row in rows.values():
        if not included(row):
            continue
        reason = attribute(row)
        if reason == 'test_fixture':
            excluded[reason] += 1
            continue
        if reason == 'unattributed':
            unattributed.append(row)
        selected.append(row)
    attributed = [row for row in selected if costs.classify(row) is None]
    cells, native_rows = costs.usage_cells(db, where, parameters, summary, boundary, end, bool(attributed),
                                     cell_order, current, aliases)
    cells = [cell for cell in cells if included(cell)]
    for cell in cells:
        cell['reason'] = attribute(cell)
        if cell['reason'] == 'test_fixture':
            excluded[cell['reason']] += cell['rows']
    cells = [cell for cell in cells if cell['reason'] != 'test_fixture']
    native = set()
    for row in native_rows:
        if included(row) and attribute(row) is None:
            native.add((row['adapter'], row['session']))
    usage = [row for row in attributed if row['source'] != 'fabric' or (row['adapter'], row['session']) not in native]
    for row in usage:
        row['day'] = telemetry.since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
    usage_groups = [cell for cell in cells if cell['reason'] is None]
    for cell in usage_groups:
        cell['day'] = telemetry.since_time(cell['timestamp']).date().isoformat() if cell['timestamp'] else None
    def groups(keys):
        # Groups enter `combine` in the order their first row had, so tied sort keys keep that order.
        ordered = itertools.chain(((tuple(row['first_row'].split('\x01')), costs.unit(telemetry, row, keys, prices, free_patterns)) for row in usage),
                                  ((tuple(cell['first_row'].split('\x01')), costs.cell_unit(telemetry, cell, keys, prices, free_patterns)) for cell in usage_groups))
        return costs.combine((group for _, group in sorted(ordered, key=lambda pair: pair[0])), keys)
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
        if stamp and (key not in last_limits or telemetry.since_time(stamp) > telemetry.since_time(last_limits[key])):
            last_limits[key] = stamp
    receipt_outcomes = set()
    for row in selected:
        if row['source'] == 'fabric' and row['status'] in {'rate_limited', 'usage_limited'}:
            remember_limit(row)
            run = row['run_id']
            receipt_outcomes.add((row['adapter'], row['model_raw'], run, row['status']))
            day = telemetry.since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
            limits[(day, row['provider'], row['account'], row['status'])] += 1
    seen = set()
    for item in db.execute('SELECT * FROM observations ORDER BY source'):
        row = dict(item)
        row['host'] = telemetry.same_host(row['host'], aliases)
        if not included(row) or row['status'] not in {'rate_limited', 'usage_limited'}:
            continue
        remember_limit(row)
        if (row['adapter'], row['model'], row['run_id'], row['status']) in receipt_outcomes:
            continue
        day = telemetry.since_time(row['timestamp']).date().isoformat() if row['timestamp'] else None
        key = (row['adapter'], row['model'], row['run_id'] or row['timestamp'], row['status'], day)
        if key not in seen:
            seen.add(key)
            limits[(day, row['provider'], row['account'], row['status'])] += 1
    windows = {}
    for item in db.execute('SELECT * FROM observations WHERE used_percent IS NOT NULL ORDER BY timestamp'):
        row = dict(item)
        row['host'] = telemetry.same_host(row['host'], aliases)
        if included(row):
            windows[(row['provider'], row['account'], row['window'])] = row
    missing = Counter()
    for row in unattributed:
        group = missing.setdefault((row['source'], row['adapter']), {'rows': 0, 'input_tokens': 0, 'statuses': Counter()})
        group['rows'] += 1
        group['input_tokens'] += row['input_tokens'] or 0
        if row['status']:
            group['statuses'][row['status']] += 1
    for cell in cells:
        if cell['reason'] == 'unattributed':
            group = missing.setdefault((cell['source'], cell['adapter']), {'rows': 0, 'input_tokens': 0, 'statuses': Counter()})
            group['rows'] += cell['rows']
            group['input_tokens'] += cell['input_tokens']
            if cell['status']:
                group['statuses'][cell['status']] += cell['rows']
    report = {'schema_version': 2, 'since': since, 'host': telemetry.host_id(),
            'usage': costs.finalise(groups(('provider', 'adapter', 'model')), ('provider', 'adapter', 'model')),
            'usage_per_day': costs.finalise(groups(('day', 'provider', 'adapter', 'account')), ('day', 'provider', 'adapter', 'account')),
            'usage_by_parent': costs.aggregate(telemetry, [row for row in attributed if row['source'] == 'fabric' and row.get('parent_run_id')],
                                         ('parent_run_id', 'parent_task_id', 'parent_attempt'), prices, free_patterns),
            'outcomes': outcomes([row for row in attributed if row['source'] == 'fabric']),
            # One overall group each, for before/after comparison; percentiles are never averaged.
            'lane_totals': outcomes([{**row, **dict.fromkeys(OUTCOME_KEYS)}
                                     for row in attributed if row['source'] == 'fabric']),
            'usage_totals': [{**group, 'known_cost_usd': costs.micro(group['known_cost_usd']),
                              'reported_cost_usd': costs.micro(group['reported_cost_usd'])} for group in groups(())],
            'lanes': [{'status': status, 'error_class': error, 'lanes': count}
                      for (status, error), count in sorted(lanes.items(), key=str)],
            'attempts': [{'status': status, 'error_class': error, 'attempts': count}
                         for (status, error), count in sorted(attempts.items(), key=str)],
            'limits_per_day': [dict(zip(('day','provider','account','status','events'), (*key, count)))
                               for key, count in sorted(limits.items(), key=str)],
            'last_limit_events': [dict(host=host, provider=provider, account=account, timestamp=stamp,
                                       age_seconds=max(0, round((telemetry.since_time(telemetry.now()) - telemetry.since_time(stamp)).total_seconds())))
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
        # Transport to the host merge: full precision, rounded once after the merge.
        report['session_usage'] = groups(keys)
    report['compactions_per_lane'] = telemetry.delivery_owner().report(telemetry, db, since, until)
    return report



def peer_summary(telemetry, db, since=None, host=None):
    """Versioned aggregate payload. No event rows, paths, prompts or credential material."""
    report = stats(telemetry, db, since, summary=True)
    if host:
        report['host'] = host
        for row in report['session_usage']:
            row['host'] = host
    report['schema'] = 'provenant.telemetry.summary.v1'
    report['quota_snapshots'] = [public_snapshot(telemetry, {**value, 'host': report['host'] if host else telemetry.host_id()})
                                 for value in telemetry.quota_owner().snapshots() if telemetry.same_host(value.get('host')) == telemetry.host_id()]
    report['quota_observations'] = [
        {key: row.get(key) for key in ('host', 'provider', 'account', 'window', 'used_percent',
                                     'window_minutes', 'reset_at', 'timestamp')}
        for row in report['quota_observations']]
    return report




def public_snapshot(telemetry, value):
    """Only documented quota metadata may cross the peer boundary."""
    result = {key: value[key] for key in ('schema', 'provider', 'account', 'host', 'plan', 'active', 'priority',
                                         'observed_at', 'attempted_at', 'status', 'error', 'source', 'unreported')
              if key in value}
    quota = telemetry.quota_owner()
    result['windows'] = [{**{key: row[key] for key in ('name', 'used_percent', 'window_minutes', 'reset_at',
                                                     'raw_kind', 'window_start') if key in row},
                          **quota.window_amounts(row), **({'advisory': True} if row.get('advisory') is True else {})}
                         for row in value.get('windows', []) if isinstance(row, dict)]
    for key, extra in (('usage_credits', quota.normalise_usage_credits(value.get('usage_credits'))),
                       ('resets', quota.normalise_resets(value.get('resets'))),
                       ('allowance', quota.normalise_allowance(value.get('allowance')))):
        if extra:
            result[key] = extra
    credits = value.get('credits')
    if isinstance(credits, dict) and type(credits.get('reset_credits')) is int and credits['reset_credits'] >= 0:
        result['credits'] = {'reset_credits': credits['reset_credits']}
        if credits.get('source') in ('manual', 'provider'):
            result['credits']['source'] = credits['source']
    return result


def compact(value):
    if value is None:
        return '-'
    if isinstance(value, float) and not value.is_integer():
        return f'{value:.2f}'
    for size, suffix in ((1e9, 'B'), (1e6, 'M'), (1e3, 'k')):
        if abs(value) >= size:
            return f'{value / size:.3g}{suffix}'
    return str(int(value))


def table(headers, rows):
    cells = [[str(cell) for cell in row] for row in rows]
    widths = [max([len(str(header)), *(len(row[i]) for row in cells)]) for i, header in enumerate(headers)]
    return [' '.join(str(cell).ljust(width) for cell, width in zip(row, widths)).rstrip()
            for row in [headers, *cells]]


ROW_LIMIT = 12  # rows shown per table before `N more; use --detail/--json`


def _nullable_compact(value):
    return compact(value) if value is not None else None


def bounded_table(headers, rows, limit=None, always=()):
    """Render a table of nullable cells; columns missing in every row are dropped.

    None prints as '-' and callers report an elided tail beyond `limit`, so no
    identifier, status or next-step hint is lost. Columns in `always` stay even
    when empty, because their absence is itself the signal.
    """
    shown = rows if limit is None else rows[:limit]
    keep = [index for index, name in enumerate(headers)
            if name in always or any(row[index] is not None for row in rows)]
    cells = [[('-' if row[index] is None else str(row[index])) for index in keep] for row in shown]
    return table(keep and [headers[index] for index in keep] or list(headers), cells)


def render_stats(telemetry, report, detail=False):
    """Summary tables plus pointer lines by default; `--detail` adds the per-account ones."""
    limit = None if detail else ROW_LIMIT
    lines = [f"Usage on {report['host']}" + (f" since {report['since']}" if report['since'] else '') +
             ': costs are API-equivalent USD estimates; - marks missing usage or prices.']
    lines += bounded_table(
        ('provider', 'adapter', 'model', 'rows', 'input', 'cache_read', 'cache_write',
         'output', 'reasoning', 'cache%', 'cost_usd', 'billing'),
        [(row['provider'] or '-', row['adapter'], row['model'], row['rows'],
          *(compact(row[field]) if row[field] is not None else None for field in telemetry.costs_owner().TOKEN_FIELDS),
          None if row['cache_read_share'] is None else f"{row['cache_read_share']:.0%}",
          None if row['estimated_cost_usd'] is None else f"{row['estimated_cost_usd']:.2f}",
          row['billing'] or '-') for row in report['usage']], limit, always=('cost_usd',))
    if not detail and len(report['usage']) > ROW_LIMIT:
        lines.append(f"{len(report['usage']) - ROW_LIMIT} more; use --detail/--json")
    outcomes = [(f"{row['adapter']}/{row['model']}" + (f"@{row['effort']}" if row['effort'] else ''),
                 row['task_class'] or '-', row['route'] or '-', row['lanes'],
                 None if row['success_rate'] is None else f"{row['success_rate']:.0%}", row['retries'],
                 ','.join(f'{name}:{count}' for name, count in row['error_classes'].items()) or '-',
                 *map(_nullable_compact, (row['latency_p50_s'], row['latency_p90_s'],
                                          row['output_tokens_per_s'])))
                for row in report['outcomes']]
    if outcomes:
        lines += ['', 'Outcomes by route (latest attempt per lane):']
        lines += bounded_table(
            ('route', 'task_class', 'alias', 'lanes', 'ok', 'retries', 'errors',
             'p50_s', 'p90_s', 'out_tok/s'), outcomes, limit)
        if not detail and len(outcomes) > ROW_LIMIT:
            lines.append(f"{len(outcomes) - ROW_LIMIT} more; use --detail/--json")
    last_limits = [(row['provider'] or '-', row['account'] or 'unattributed', row['timestamp'], f"{row['age_seconds']}s ago")
                   for row in report.get('last_limit_events', [])]
    sections = [('Lanes', [(row['status'], row['error_class'] or '-', row['lanes']) for row in report['lanes']]),
                ('Compactions per lane', [(row['lane'], row['compactions'], row.get('host', '-'))
                                          for row in report.get('compactions_per_lane', [])]),
                ('Limit events', [(row['day'] or '-', row['provider'] or '-', row['status'], row['events'])
                                  for row in report['limits_per_day'][-14:]]),
                ('Last limit events', last_limits if detail else last_limits[:ROW_LIMIT])]
    # Quota observations stay behind `--detail` in the default summary view.
    if detail:
        sections.append(('Latest quota observations', [(row['provider'] or '-', row['window'], f"{row['used_percent']:g}%",
                                                        row['reset_at'] or '-', row['timestamp'] or '-')
                                                       for row in report['quota_observations']]))
    for title, rows in sections:
        if rows:
            lines += ['', f'{title}:', *('  ' + '  '.join(str(cell) for cell in row) for row in rows)]
    if not detail and len(last_limits) > ROW_LIMIT:
        lines.append(f"{len(last_limits) - ROW_LIMIT} more limit events; use --detail/--json")
    if not detail and report['quota_observations']:
        lines.append(f"{len(report['quota_observations'])} quota observations; use --detail/--json")
    if report['unattributed'] or report['excluded_rows']:
        lines += ['', 'Not attributed to a model: ' + '; '.join(
            f"{row['source']}/{row['adapter'] or '-'} rows={row['rows']}" for row in report['unattributed']) +
            ''.join(f'; excluded {reason}={count}' for reason, count in sorted(report['excluded_rows'].items()))]
    for host in report.get('hosts', []):
        lines.append(f"host {host['host']}: {host['status']}" + (f" ({host['reason']})" if host.get('reason') else ''))
    print('\n'.join(lines))
