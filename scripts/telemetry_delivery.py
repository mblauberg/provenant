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


def _run_timestamp(telemetry, receipt, mtime):
    """Earliest receipt timestamp, or RUN.json mtime when none is recorded."""
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
        return min(times).isoformat()
    return datetime.fromtimestamp(mtime, UTC).isoformat()


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
    started = _run_timestamp(telemetry, receipt, stat.st_mtime)
    db.execute('''INSERT INTO deliveries
        (run_dir,host,run_id,risk_tier,chair_family,repair_cycles,acceptance_basis,accepted,merge_commit,ingested_at,started_at)
        VALUES (?,?,?,?,?,?,?,?,?,?,?)
        ON CONFLICT(run_dir) DO UPDATE SET host=excluded.host,run_id=excluded.run_id,
        risk_tier=excluded.risk_tier,chair_family=excluded.chair_family,
        repair_cycles=excluded.repair_cycles,acceptance_basis=excluded.acceptance_basis,
        accepted=excluded.accepted,merge_commit=excluded.merge_commit,ingested_at=excluded.ingested_at,
        started_at=excluded.started_at''',
               (str(path.parent.resolve()), telemetry.host_id(), receipt.get('run_id'), receipt.get('risk_tier'),
                receipt.get('chair_family'), receipt.get('repair_cycles'), basis, int(basis is not None),
                commit, telemetry.now(), started))
    db.execute('INSERT OR REPLACE INTO cursors VALUES (?,?,?,?,?,?)',
               ('delivery', str(path), stat.st_size, stat.st_mtime_ns, stat.st_size, '{}'))


def cost_per_accepted(telemetry, db, since=None):
    """Delivery counts by risk tier; lane cost and harness digest are unlinked."""
    boundary = telemetry.since_time(since) if since else None
    groups = defaultdict(lambda: {'runs': set(), 'accepted': set(), 'merged': set()})
    for run in db.execute('SELECT * FROM deliveries ORDER BY run_dir').fetchall():
        if boundary and (not run['started_at'] or telemetry.since_time(run['started_at']) < boundary):
            continue
        merged = bool(run['merge_commit'] and db.execute(
            'SELECT 1 FROM changes WHERE merge_commit=? LIMIT 1', (run['merge_commit'],)).fetchone())
        group = groups[(run['risk_tier'], None)]
        _add_run(group, run, merged)
    report = []
    for (risk_tier, digest), group in sorted(groups.items(), key=lambda item: (item[0][0] or '', item[0][1] or '')):
        report.append({'risk_tier': risk_tier, 'harness_digest': digest,
                       'runs': len(group['runs']), 'accepted': len(group['accepted']),
                       'merged': len(group['merged']), 'cost_usd_lower_bound': None,
                       'usage_rows': 0, 'unpriced_rows': 0,
                       'cost_per_accepted_usd_lower_bound': None, 'cost_basis': 'unlinked'})
    return report


def run_accepted(telemetry, db, since=None, as_json=False):
    report = cost_per_accepted(telemetry, db, since)
    if as_json:
        print(json.dumps(report, indent=2))
    else:
        print('risk_tier  harness_digest  runs  accepted  merged  cost_usd_lower_bound  usage_rows  unpriced_rows  cost_per_accepted_usd_lower_bound  cost_basis')
        for row in report:
            cost = row['cost_usd_lower_bound']
            shown_cost = f'{cost:.9f}' if cost is not None else '-'
            print(f"{row['risk_tier'] or '-'}  {row['harness_digest'] or '-'}  {row['runs']}  "
                  f"{row['accepted']}  {row['merged']}  {shown_cost}  {row['usage_rows']}  "
                  f"{row['unpriced_rows']}  {row['cost_per_accepted_usd_lower_bound']}  {row['cost_basis']}")
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
