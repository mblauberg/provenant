"""Ingest delivery sidecars and expose delivery-run telemetry."""

from __future__ import annotations

from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import subprocess


def paths(telemetry):
    projects = {Path.cwd()}
    registry = Path(os.environ.get('AGENT_FABRIC_STATE_DIRECTORY', str(telemetry.state_root()))) / 'fabric.sqlite3'
    if registry.is_file():
        with telemetry.readonly(registry) as source:
            projects.update(Path(row[0]) for row in source.execute('SELECT DISTINCT project FROM agents'))
    roots = set()
    for project in projects:
        try:
            result = subprocess.run(['git', '-C', str(project), 'rev-parse', '--path-format=absolute',
                                     '--git-common-dir'], capture_output=True, text=True, timeout=5,
                                    env={key: value for key, value in os.environ.items() if not key.startswith('GIT_')})
            root = Path(result.stdout.strip()).parent if result.returncode == 0 else project
        except (OSError, subprocess.SubprocessError):
            root = project
        roots.update((root / '.agent-run', project / '.agent-run'))
    return sorted(path for root in roots for name in ('repairs.jsonl', 'findings.jsonl')
                  for path in root.glob(f'runs/**/{name}')
                  if path.is_file() and not path.is_symlink())


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
    source_ref = f'{path}#{offset}'
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
    rows = db.execute("SELECT usage.lane, events.ts FROM events JOIN usage "
                      "ON usage.session=events.session WHERE events.kind='session.compaction' "
                      "AND usage.lane IS NOT NULL")
    for lane, stamp in rows:
        if boundary or end:
            if stamp is None:
                continue
            moment = telemetry.since_time(stamp)
            if boundary and moment < boundary or end and moment >= end:
                continue
        counts[lane] += 1
    return [{'lane': lane, 'compactions': count} for lane, count in sorted(counts.items())]
