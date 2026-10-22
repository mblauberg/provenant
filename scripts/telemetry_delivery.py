"""Ingest delivery sidecars and expose delivery-run telemetry."""

from __future__ import annotations

from collections import Counter
from collections import defaultdict
from datetime import UTC, datetime
import hashlib
import json
from pathlib import Path


def paths(telemetry):
    return sorted({path for name in ('RUN.json', 'repairs.jsonl', 'findings.jsonl')
                   for path in telemetry.fabric_paths(name)})


def _run_times(telemetry, receipt, run_dir):
    """Recorded lifecycle times, excluding authority expiry and other future limits."""
    times = []

    def visit(value):
        if isinstance(value, dict):
            for key, item in value.items():
                if key in {'authority', 'risk_override'}:
                    continue
                if key == 'at' or key == 'timestamp' or key.endswith('_at'):
                    try:
                        times.append(telemetry.since_time(item))
                    except (AttributeError, TypeError, ValueError):
                        pass
                elif isinstance(item, (dict, list)):
                    visit(item)
        elif isinstance(value, list):
            for item in value:
                visit(item)

    visit(receipt)
    if times:
        return min(times).isoformat(), max(times).isoformat()
    try:
        stamp = datetime.strptime(run_dir.name[:13], '%Y%m%d-%H%M').replace(tzinfo=UTC).isoformat()
        return stamp, None
    except ValueError:
        return None, None


def ingest_receipt(telemetry, db, path):
    """Ingest one RUN.json as a size/mtime-cursored document."""
    stat = path.stat()
    old = db.execute("SELECT size,mtime FROM cursors WHERE source='delivery' AND path=?",
                     (str(path),)).fetchone()
    if old and (old['size'], old['mtime']) == (stat.st_size, stat.st_mtime_ns):
        return
    receipt = json.loads(path.read_text())
    acceptance = receipt.get('delivery_acceptance')
    basis = acceptance.get('basis') if isinstance(acceptance, dict) else None
    if basis not in {'checks-and-review', 'project-landing', 'user'}:
        basis = None
    canonical_id = (receipt.get('software_delivery') or {}).get('canonical_artifact_id')
    artifact = next((item for item in receipt.get('artifacts', [])
                     if isinstance(item, dict) and item.get('id') == canonical_id), {})
    revision = artifact.get('git_revision') or {}
    commit = revision.get('commit') if isinstance(revision, dict) else None
    started, ended = _run_times(telemetry, receipt, path.parent)
    workspace = next(parent.parent for parent in path.parents if parent.name == '.agent-run')
    sources = [str((workspace / item).resolve())
               for item in (receipt.get('authority') or {}).get('allowed_source_paths', [])
               if item and Path(item) != Path('.')]
    db.execute('''INSERT INTO deliveries
        (run_dir,host,run_id,risk_tier,chair_family,repair_cycles,acceptance_basis,accepted,merge_commit,ingested_at,started_at,ended_at,source_paths)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
        ON CONFLICT(run_dir) DO UPDATE SET host=excluded.host,run_id=excluded.run_id,
        risk_tier=excluded.risk_tier,chair_family=excluded.chair_family,
        repair_cycles=excluded.repair_cycles,acceptance_basis=excluded.acceptance_basis,
        accepted=excluded.accepted,merge_commit=excluded.merge_commit,ingested_at=excluded.ingested_at,
        started_at=excluded.started_at,ended_at=excluded.ended_at,source_paths=excluded.source_paths''',
               (str(path.parent.resolve()), telemetry.host_id(), receipt.get('run_id'), receipt.get('risk_tier'),
                receipt.get('chair_family'), receipt.get('repair_cycles'), basis, int(basis is not None),
                commit, telemetry.now(), started, ended, json.dumps(sources)))
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               ('delivery', str(path), stat.st_size, stat.st_mtime_ns, stat.st_size, '{}'))


def cost_per_accepted(telemetry, db, since=None):
    """Cost estimate by receipt risk tier and each run's most-used harness digest."""
    boundary = telemetry.since_time(since) if since else None
    prices, free_patterns = telemetry.load_prices()
    adapters = telemetry.catalogue()
    telemetry.load_hosts(db)
    aliases = telemetry.host_aliases()
    current_host = telemetry.host_id()
    lanes = {str(Path(row['source_ref']).resolve()): json.loads(row['attrs'])
             for row in db.execute("SELECT source_ref,attrs FROM events WHERE kind='lane.attempt'")}
    usage = db.execute("""SELECT DISTINCT u.*, f.path FROM usage_files f JOIN usage u
                          ON u.source=f.source AND u.event_id=f.event_id AND u.host=f.host
                          WHERE u.source='fabric'""").fetchall()
    groups = defaultdict(lambda: {'runs': set(), 'accepted': set(), 'merged': set(),
                                  'known_cost': 0.0, 'usage_rows': 0, 'unpriced_rows': 0})
    for run in db.execute('SELECT * FROM deliveries ORDER BY run_dir').fetchall():
        if boundary and (not run['started_at'] or telemetry.since_time(run['started_at']) < boundary):
            continue
        merged = bool(run['merge_commit'] and db.execute(
            'SELECT 1 FROM changes WHERE merge_commit=? LIMIT 1', (run['merge_commit'],)).fetchone())
        sources = [Path(path).resolve() for path in json.loads(run['source_paths'] or '[]')]
        delivery_host = telemetry.same_host(run['host'], aliases)
        selected = {}
        for row in usage:
            canonical_host = telemetry.same_host(row['host'], aliases)
            if canonical_host != delivery_host:
                continue
            lane = lanes.get(str(Path(row['path']).resolve()), {})
            workspace, started = lane.get('workspace_root'), lane.get('started_at')
            if not workspace or not started or not run['started_at'] or not run['ended_at']:
                continue
            if not any(Path(workspace).resolve().is_relative_to(path) for path in sources):
                continue
            if not (telemetry.since_time(run['started_at']) <= telemetry.since_time(started)
                    <= telemetry.since_time(run['ended_at'])):
                continue
            key = (row['source'], row['event_id'], canonical_host)
            if key not in selected or row['host'] == current_host:
                selected[key] = row
        rows = list(selected.values())
        digests = Counter(lanes[str(Path(row['path']).resolve())].get('profile_digest') for row in rows)
        digest = min(digests, key=lambda name: (-digests[name], name or '')) if digests else None
        group = groups[(run['risk_tier'], digest)]
        _add_run(group, run, merged)
        group['usage_rows'] += len(rows)
        for row in rows:
            cost_row = dict(row)
            cost_row['model'] = telemetry.normalise_model(
                cost_row['adapter'], cost_row['provider'], cost_row['model'], adapters)
            price = telemetry.price_entry(cost_row, prices, free_patterns)
            cost = telemetry.estimated_cost(cost_row, prices, price)
            if cost is None:
                group['unpriced_rows'] += 1
            else:
                group['known_cost'] += cost
    report = []
    for (risk_tier, digest), group in sorted(groups.items(), key=lambda item: (item[0][0] or '', item[0][1] or '')):
        accepted = len(group['accepted'])
        cost = round(group['known_cost'], 9) if group['usage_rows'] else None
        report.append({'risk_tier': risk_tier, 'harness_digest': digest,
                       'runs': len(group['runs']), 'accepted': accepted, 'merged': len(group['merged']),
                       'cost_usd_lower_bound': cost, 'usage_rows': group['usage_rows'],
                       'unpriced_rows': group['unpriced_rows'],
                       'cost_per_accepted_usd_lower_bound':
                           round(group['known_cost'] / accepted, 9) if accepted and cost is not None else None})
    return report


def run_accepted(telemetry, db, since=None, as_json=False):
    report = cost_per_accepted(telemetry, db, since)
    if as_json:
        print(json.dumps(report, indent=2))
    else:
        print('risk_tier  harness_digest  runs  accepted  merged  cost_usd_lower_bound  usage_rows  unpriced_rows  cost_per_accepted_usd_lower_bound')
        for row in report:
            cost = row['cost_usd_lower_bound']
            shown_cost = f'{cost:.9f}' if cost is not None else '-'
            print(f"{row['risk_tier'] or '-'}  {row['harness_digest'] or '-'}  {row['runs']}  "
                  f"{row['accepted']}  {row['merged']}  {shown_cost}  {row['usage_rows']}  "
                  f"{row['unpriced_rows']}  {row['cost_per_accepted_usd_lower_bound']}")
    return 0


def _add_run(group, run, merged):
    group['runs'].add(run['run_dir'])
    if run['accepted']:
        group['accepted'].add(run['run_dir'])
    if merged:
        group['merged'].add(run['run_dir'])


def _review(run_dir, review_id):
    try:
        run = json.loads((run_dir / 'RUN.json').read_text())
    except (OSError, json.JSONDecodeError):
        run = {}
    return next((item for item in run.get('reviews', [])
                 if isinstance(item, dict) and item.get('id') == review_id), {})


def _update_event(db, kind, source_ref, attrs):
    db.execute('UPDATE events SET attrs=? WHERE kind=? AND source_ref=?',
               (json.dumps(attrs, sort_keys=True), kind, source_ref))


def prepare(db, path):
    """Re-read a sidecar when its owning receipt changes, including acceptance."""
    receipt = path.parent / 'RUN.json'
    try:
        stat = receipt.stat()
        signature = f'{stat.st_size}:{stat.st_mtime_ns}'
    except OSError:
        signature = 'missing'
    key = f'delivery_receipt:{path.parent}'
    previous = db.execute('SELECT value FROM meta WHERE key=?', (key,)).fetchone()
    if previous and previous['value'] != signature:
        for (saved_path,) in db.execute("SELECT path FROM cursors WHERE source='delivery'").fetchall():
            if Path(saved_path).parent == path.parent:
                db.execute('DELETE FROM cursors WHERE source=? AND path=?', ('delivery', saved_path))
    db.execute('INSERT OR REPLACE INTO meta VALUES (?,?)', (key, signature))


def record(telemetry, db, row, state, path, offset):
    """Parse one delivery sidecar row using the ledger's JSONL cursor."""
    source_ref = f'{Path(path).resolve()}#{offset}'
    if path.name == 'repairs.jsonl':
        cycle = row.get('cycle')
        failing = sorted(set(item for item in row.get('failing_before', []) if isinstance(item, str)))
        previous = state.get('previous')
        if previous:
            _update_event(db, 'repair.cycle', previous['source_ref'], {
                **previous['attrs'], 'failing_after_count': len(failing),
                'failing_after': failing[:8],
                'failing_after_hash': hashlib.sha256(json.dumps(failing, separators=(',', ':')).encode()).hexdigest()[:12],
            })
        full = {'repair_cycle': cycle, 'failing_before_count': len(failing),
                'failing_before': failing[:8],
                'failing_before_hash': hashlib.sha256(json.dumps(failing, separators=(',', ':')).encode()).hexdigest()[:12]}
        telemetry.put_event(db, 'repair.cycle', row.get('at'), source_ref, attrs=full)
        try:
            receipt = json.loads((path.parent / 'RUN.json').read_text())
        except (OSError, json.JSONDecodeError):
            receipt = {}
        if cycle == receipt.get('repair_cycles') and 'delivery_acceptance' in receipt:
            _update_event(db, 'repair.cycle', source_ref, {
                **full, 'failing_after_count': 0, 'failing_after': [],
                'failing_after_hash': hashlib.sha256(b'[]').hexdigest()[:12],
            })
        state['previous'] = {'source_ref': source_ref, 'attrs': full}
    elif path.name == 'findings.jsonl':
        review = _review(path.parent, row.get('review_id'))
        attrs = {key: row.get(key) for key in ('review_id', 'finding', 'disposition', 'severity', 'duplicate_of')
                 if row.get(key) is not None}
        lineage = {'reviewer_family': review.get('provider_family') or row.get('reviewer_family'),
                   'model': review.get('model') or row.get('model'),
                   'reviewer_id': review.get('reviewer_id') or row.get('reviewer_id')}
        attrs.update({key: value for key, value in lineage.items() if value is not None})
        telemetry.put_event(db, 'review.finding', row.get('at'), source_ref, attrs=attrs)
        _update_event(db, 'review.finding', source_ref, attrs)


def compact(telemetry, db, provider, row, state, path, offset):
    """Store metadata for a compaction boundary; never inspect Codex payload."""
    if provider == 'claude' and row.get('type') == 'system' and row.get('subtype') == 'compact_boundary':
        meta = row.get('compactMetadata') or {}
        attrs = {'trigger': meta.get('trigger'), 'pre_tokens': meta.get('preTokens'),
                 'post_tokens': meta.get('postTokens')}
        telemetry.put_event(db, 'session.compaction', row.get('timestamp'), f'{path}#{offset}',
                            attrs=attrs, session=row.get('sessionId'))
    elif provider == 'codex' and row.get('type') == 'compacted':
        telemetry.put_event(db, 'session.compaction', row.get('timestamp'), f'{path}#{offset}',
                            attrs={}, session=state.get('session'))


def report(telemetry, db, since=None, until=None):
    boundary = telemetry.since_time(since) if since else None
    end = telemetry.since_time(until) if until else None
    counts = Counter()
    rows = db.execute("SELECT DISTINCT usage.lane, events.event_id, events.ts FROM events JOIN usage "
                      "ON usage.session=events.session WHERE events.kind='session.compaction' "
                      "AND usage.lane IS NOT NULL")
    for lane, _event_id, stamp in rows:
        if boundary or end:
            if stamp is None:
                continue
            moment = telemetry.since_time(stamp)
            if boundary and moment < boundary or end and moment >= end:
                continue
        counts[lane] += 1
    return [{'lane': lane, 'compactions': count} for lane, count in sorted(counts.items())]
