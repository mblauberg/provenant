"""Change markers, before/after comparison and the retro digest over the telemetry ledger.

Every function takes the loaded `telemetry` module, as telemetry_hosts does, so
the ledger owner stays the single source of row selection and cost rules.
"""

from __future__ import annotations

from collections import Counter
from datetime import UTC, timedelta
import json
import math
import os
import re
import subprocess


MIN_SAMPLE = 10  # a side with fewer lanes or usage rows than this is inconclusive, never a win
NOISE_SHARE = 0.10  # relative change at or below this is within noise (non-proportion metrics)
SYNC_LIMIT = 200  # merged PRs read per `changes --sync`; later runs continue from the newest marker
DEFAULT_WINDOW = '7d'
# metric, sample field, higher is better
METRICS = (('success_rate', 'lanes', True), ('retries_per_lane', 'lanes', False),
           ('latency_p50_s', 'lanes', False), ('cache_read_share', 'usage_rows', True),
           ('cost_usd_per_day', 'usage_rows', False), ('limit_events_per_day', 'usage_rows', False))
SHORT = {'success_rate': 'ok', 'retries_per_lane': 'retries', 'latency_p50_s': 'p50_s',
         'cache_read_share': 'cache', 'cost_usd_per_day': 'usd/d', 'limit_events_per_day': 'limits/d'}
MARK = {'improved': '+', 'regressed': '-', 'within_noise': '=', 'inconclusive': '?'}


class ChangeError(ValueError):
    """A change-marker or comparison failure whose message is safe to print."""


def iso(stamp):
    return stamp.astimezone(UTC).isoformat().replace('+00:00', 'Z')


def window(value):
    match = re.fullmatch(r'(\d+)([hdw])', value or '')
    if not match or int(match[1]) == 0:
        raise ChangeError(f'window must look like 24h, 7d or 2w, got {value!r}')
    return timedelta(hours=int(match[1]) * {'h': 1, 'd': 24, 'w': 168}[match[2]])


def git_environment():
    return {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}


def sync(telemetry, db):
    """Upsert merged PRs from `gh`, continuing from the newest synced merge date."""
    latest = db.execute("SELECT MAX(merged_at) FROM changes WHERE kind='pr'").fetchone()[0]
    command = ['gh', 'pr', 'list', '--state', 'merged', '--limit', str(SYNC_LIMIT),
               '--json', 'number,mergeCommit,mergedAt,title,labels']
    if latest:
        command += ['--search', f'merged:>={latest[:10]}']
    try:
        result = subprocess.run(command, cwd=telemetry.PRODUCT_ROOT, capture_output=True, text=True,
                                timeout=60, env=git_environment())
    except (OSError, subprocess.SubprocessError) as exc:
        raise ChangeError(f'gh pr list could not run ({type(exc).__name__}); install and authenticate gh') from None
    if result.returncode:
        raise ChangeError(f'gh pr list failed with exit {result.returncode}; run `gh auth status`')
    added = 0
    for row in json.loads(result.stdout):
        if not isinstance(row, dict) or not isinstance(row.get('number'), int) or not row.get('mergedAt'):
            continue
        labels = sorted(label.get('name') for label in row.get('labels') or () if isinstance(label, dict))
        added += db.execute('SELECT 1 FROM changes WHERE change_id=?', (f"pr:{row['number']}",)).fetchone() is None
        db.execute('INSERT INTO changes VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(change_id) DO UPDATE SET '
                   'merge_commit=excluded.merge_commit, merged_at=excluded.merged_at, title=excluded.title, '
                   'labels=excluded.labels',
                   (f"pr:{row['number']}", 'pr', row['number'], (row.get('mergeCommit') or {}).get('oid'),
                    iso(telemetry.since_time(row['mergedAt'])), row.get('title'), json.dumps(labels),
                    telemetry.now()))
    db.commit()
    return added


def add(telemetry, db, label, at):
    if not label or not label.strip():
        raise ChangeError('--add needs a non-empty label')
    try:
        stamp = iso(telemetry.since_time(at))
    except ValueError:
        raise ChangeError(f'--at must be an ISO date or timestamp, got {at!r}') from None
    change_id = 'manual:' + telemetry.identity(label, stamp)[:16]
    db.execute('INSERT OR IGNORE INTO changes VALUES (?,?,?,?,?,?,?,?)',
               (change_id, 'manual', None, None, stamp, label.strip(), '[]', telemetry.now()))
    db.commit()
    return change_id


def changes(db, since=None):
    query = 'SELECT * FROM changes' + (' WHERE merged_at >= ?' if since else '') + ' ORDER BY merged_at DESC, change_id'
    return [{**dict(row), 'labels': json.loads(row['labels'])} for row in db.execute(query, (since,) if since else ())]


def reference(change):
    return f"#{change['pr']}" if change.get('pr') is not None else change['title']


def pivot(telemetry, db, ref):
    """Resolve a PR number, merge revision or date to (change or None, pivot time)."""
    value = ref.strip()
    if re.fullmatch(r'#?\d{1,6}', value):
        row = db.execute('SELECT * FROM changes WHERE pr=?', (int(value.lstrip('#')),)).fetchone()
        if row is None:
            raise ChangeError(f'PR {value} has no change marker; run `provenant telemetry changes --sync`')
        return {**dict(row), 'labels': json.loads(row['labels'])}, telemetry.since_time(row['merged_at'])
    if re.fullmatch(r'[0-9a-fA-F]{7,40}', value):
        row = db.execute('SELECT * FROM changes WHERE merge_commit LIKE ? ORDER BY merged_at LIMIT 1',
                         (value.lower() + '%',)).fetchone()
        if row is not None:
            return {**dict(row), 'labels': json.loads(row['labels'])}, telemetry.since_time(row['merged_at'])
    try:
        return None, telemetry.since_time(value)
    except ValueError:
        pass
    if re.fullmatch(r'[\w][\w./^~-]*', value):
        try:
            result = subprocess.run(['git', '-C', str(telemetry.PRODUCT_ROOT), 'log', '-1', '--format=%cI', value, '--'],
                                    capture_output=True, text=True, timeout=10, env=git_environment())
            if result.returncode == 0 and result.stdout.strip():
                return None, telemetry.since_time(result.stdout.strip())
        except (OSError, subprocess.SubprocessError):
            pass
    raise ChangeError(f'cannot resolve {ref!r} as a PR number, revision or ISO date')


def side(telemetry, db, start, end):
    """Metrics for [start, end) from the ledger's own stats report."""
    report = telemetry.stats(db, iso(start), until=iso(end))
    lane = (report['lane_totals'] or [{}])[0]
    usage = (report['usage_totals'] or [{}])[0]
    lanes, rows = lane.get('lanes', 0), usage.get('rows', 0)
    days = max((end - start).total_seconds() / 86400, 1 / 24)
    limits = sum(row['events'] for row in report['limits_per_day'])
    cost = usage.get('known_cost_usd')
    return {'start': iso(start), 'end': iso(end), 'lanes': lanes, 'usage_rows': rows,
            'inconclusive': lanes < MIN_SAMPLE,
            'success_rate': lane.get('success_rate'),
            'retries_per_lane': round(lane['retries'] / lanes, 3) if lanes else None,
            'latency_p50_s': lane.get('latency_p50_s'),
            'cache_read_share': (round(usage['cache_share_read'] / usage['cache_share_input'], 3)
                                 if usage.get('cache_share_input') else None),
            'cost_usd': round(cost, 4) if cost is not None else None,
            'cost_usd_per_day': round(cost / days, 4) if rows else None,
            'missing_cost_rows': usage.get('missing_cost_rows', 0),
            'limit_events': limits, 'limit_events_per_day': round(limits / days, 3),
            'error_classes': lane.get('error_classes', {})}


def verdict(name, before, after, sample):
    nb, na = before[sample], after[sample]
    b, a = before[name], after[name]
    if nb < MIN_SAMPLE or na < MIN_SAMPLE or b is None or a is None:
        return 'inconclusive'
    if name == 'success_rate':  # two-proportion check: within two standard errors is noise
        pooled = (b * nb + a * na) / (nb + na)
        noise = abs(a - b) <= 2 * math.sqrt(pooled * (1 - pooled) * (1 / nb + 1 / na))
    else:
        noise = abs(a - b) <= NOISE_SHARE * max(abs(a), abs(b))
    if noise:
        return 'within_noise'
    return 'improved' if (a > b) == higher(name) else 'regressed'


def higher(name):
    return next(better for metric, _, better in METRICS if metric == name)


def compare(telemetry, db, ref, span=DEFAULT_WINDOW):
    change, at = pivot(telemetry, db, ref)
    report = compare_at(telemetry, db, change, at, span)
    return report if change else {**report, 'ref': ref}


def compare_at(telemetry, db, change, at, span=DEFAULT_WINDOW, *, now=None):
    width = window(span)
    current = now or telemetry.since_time(telemetry.now()).replace(microsecond=0)
    before = side(telemetry, db, at - width, at)
    after = side(telemetry, db, at, max(at, min(at + width, current)))
    return {'schema': 'provenant.telemetry.compare.v1', 'ref': reference(change) if change else iso(at),
            'change': change, 'pivot': iso(at), 'window': span, 'min_sample': MIN_SAMPLE,
            'before': before, 'after': after,
            'metrics': [{'metric': name, 'before': before[name], 'after': after[name],
                         'n_before': before[sample], 'n_after': after[sample], 'sample': sample,
                         'verdict': verdict(name, before, after, sample)}
                        for name, sample, _ in METRICS]}


def show(value, name=''):
    if value is None:
        return '-'
    if name in {'success_rate', 'cache_read_share'}:
        return f'{value:.0%}'
    return f'{value:.3g}' if isinstance(value, float) else str(value)


def render_compare(telemetry, report):
    change = report['change']
    title = f" {change['title'][:60]}" if change and change.get('pr') is not None else ''
    lines = [f"Compare {report['ref']}{title} at {report['pivot']}; window {report['window']} each side."]
    for name in ('before', 'after'):
        value = report[name]
        lines.append(f"{name:<6} {value['start']}..{value['end']} lanes={value['lanes']} "
                     f"usage_rows={telemetry.compact(value['usage_rows'])}"
                     + (f' inconclusive (n<{MIN_SAMPLE})' if value['inconclusive'] else ''))
    lines += telemetry.bounded_table(
        ('metric', 'before', 'after', 'n', 'verdict'),
        [(row['metric'], show(row['before'], row['metric']), show(row['after'], row['metric']),
          f"{row['n_before']}/{row['n_after']} {'lanes' if row['sample'] == 'lanes' else 'rows'}", row['verdict'])
         for row in report['metrics']], always=('before', 'after'))
    errors = Counter(report['after']['error_classes']).most_common(5)
    if errors:
        lines.append('after errors: ' + ', '.join(f'{name}:{count}' for name, count in errors))
    lines.append(f'Verdicts need n>={MIN_SAMPLE} on both sides; smaller samples are inconclusive, '
                 'never a win. Full report: --json')
    return '\n'.join(lines)


def attention(db, since):
    """Attention proxies already in the ledger; OTLP rows appear only when the collector ran."""
    counts = dict(db.execute("SELECT kind, COUNT(*) FROM events WHERE ts >= ? AND kind IN "
                             "('session.start','otel.user_prompt','otel.tool_decision') GROUP BY kind", (since,)))
    active = {}
    for row in db.execute("SELECT session, attrs FROM events WHERE kind='otel.metric' AND ts >= ?", (since,)):
        attrs = json.loads(row['attrs'])
        if attrs.get('name') != 'claude_code.active_time.total' or attrs.get('value') is None:
            continue
        key = (row['session'], attrs.get('type') or 'total')
        # Delta points add up; cumulative points (temporality 2) keep their latest maximum.
        active[key] = (max(active.get(key, 0), attrs['value']) if attrs.get('temporality') == 2
                       else active.get(key, 0) + attrs['value'])
    totals = Counter()
    for (_, kind), seconds in active.items():
        totals[kind] += seconds
    return {'sessions_started': counts.get('session.start', 0),
            'user_prompts': counts.get('otel.user_prompt'),
            'tool_decisions': counts.get('otel.tool_decision'),
            'active_time_s': {kind: round(seconds) for kind, seconds in sorted(totals.items())} or None}


def retro(telemetry, db, since=None, span=DEFAULT_WINDOW, *, now=None):
    current = now or telemetry.since_time(telemetry.now()).replace(microsecond=0)
    start = telemetry.since_time(since) if since else current - timedelta(days=7)
    overall = side(telemetry, db, start, current)
    report = telemetry.stats(db, iso(start))
    errors = Counter()
    for row in report['attempts']:
        if row['error_class']:
            errors[row['error_class']] += row['attempts']
    marked = changes(db, iso(start))
    return {'schema': 'provenant.telemetry.retro.v1', 'since': iso(start), 'until': iso(current),
            'window': span, 'overall': overall,
            'lanes_input_required': sum(row['lanes'] for row in report['lanes'] if row['status'] == 'input_required'),
            'changes': [{**change, 'compare': compare_at(telemetry, db, change, telemetry.since_time(change['merged_at']),
                                                         span, now=current)} for change in marked],
            'top_error_classes': dict(errors.most_common(5)),
            'attention': attention(db, iso(start)),
            'events_by_kind': dict(db.execute('SELECT kind, COUNT(*) FROM events WHERE ts >= ? '
                                              'GROUP BY kind ORDER BY COUNT(*) DESC, kind', (iso(start),)))}


def render_retro(telemetry, report, limit):
    overall = report['overall']
    lines = [f"Retro {report['since']}..{report['until']}: lanes={overall['lanes']} "
             f"ok={show(overall['success_rate'], 'success_rate')} usage_rows={telemetry.compact(overall['usage_rows'])} "
             f"usd={show(overall['cost_usd'])} limit_events={overall['limit_events']}"]
    if report['changes']:
        lines.append(f"Changes (newest first; {report['window']} each side; + improved, - regressed, "
                     f"= within noise, ? inconclusive n<{MIN_SAMPLE}):")
        for change in report['changes'][:limit]:
            metrics = {row['metric']: row for row in change['compare']['metrics']}
            summary = ' '.join(
                f"{SHORT[name]} {show(metrics[name]['before'], name)}->{show(metrics[name]['after'], name)}"
                f"{MARK[metrics[name]['verdict']]}"
                for name in ('success_rate', 'latency_p50_s', 'cost_usd_per_day', 'limit_events_per_day'))
            compared = change['compare']
            marker = f"#{change['pr']}" if change['pr'] is not None else 'marker'
            lines.append(f"  {marker} {change['merged_at'][:10]} {(change['title'] or '')[:40]}: "
                         f"{summary} n={compared['before']['lanes']}/{compared['after']['lanes']}")
        if len(report['changes']) > limit:
            lines.append(f"  {len(report['changes']) - limit} more; use --json")
    else:
        lines.append('No change markers in this window; run `provenant telemetry changes --sync`.')
    if report['top_error_classes']:
        lines.append('Top error classes (attempts): ' + ', '.join(
            f'{name}:{count}' for name, count in report['top_error_classes'].items()))
    seen = report['attention']
    parts = [f"sessions started {seen['sessions_started']}", f"lanes awaiting input {report['lanes_input_required']}"]
    if seen['user_prompts'] is not None:
        parts.append(f"user prompts {seen['user_prompts']}")
    if seen['tool_decisions'] is not None:
        parts.append(f"tool decisions {seen['tool_decisions']}")
    if seen['active_time_s']:
        parts.append('active ' + ' '.join(f'{kind} {seconds / 3600:.1f}h' for kind, seconds in seen['active_time_s'].items()))
    lines.append('Attention: ' + '; '.join(parts))
    if report['events_by_kind']:
        kinds = list(report['events_by_kind'].items())
        lines.append('Events: ' + ' '.join(f'{kind}:{count}' for kind, count in kinds[:limit])
                     + (f' (+{len(kinds) - limit} kinds)' if len(kinds) > limit else ''))
    lines.append('Detail: provenant stats --compare <PR#|rev|date> [--window 7d] [--json]')
    return '\n'.join(lines)


def render_changes(telemetry, rows, limit):
    lines = telemetry.bounded_table(
        ('merged_at', 'change', 'commit', 'labels', 'title'),
        [(row['merged_at'], reference(row) if row['pr'] is not None else 'manual', (row['merge_commit'] or '')[:10] or None,
          ','.join(row['labels'])[:30] or None, (row['title'] or '')[:60]) for row in rows], limit)
    if len(rows) > limit:
        lines.append(f'{len(rows) - limit} more; use --json')
    return '\n'.join(lines if rows else ['No change markers; run `provenant telemetry changes --sync`.'])
