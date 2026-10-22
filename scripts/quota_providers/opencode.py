"""OpenCode Go usage from its local message stores, plus its own usage-limit errors.

Go limits are plan-wide dollar amounts over 5 hour, weekly and monthly windows. OpenCode
documents no usage endpoint or reset semantics, so spend here is a rolling local estimate
from this machine's `opencode.db` only: other hosts using the same account are not counted.
A recent `GoUsageLimitError` in the store is authoritative and marks that window exhausted.
Free zen models cost nothing and are ignored. Only message tables are read, never credentials.
"""

from __future__ import annotations

from contextlib import closing
from datetime import timedelta
import json
import sqlite3


ADAPTER = 'opencode'
PAID = ('opencode-go',)
# Calibrated against the Go dashboard (about $16 weekly, $32 monthly), not documented by OpenCode.
# Override with `limits.monthly_usd`; other plans need it.
PLAN_MONTHLY_USD = {'go': 32.0}
DEFAULT_PLAN = 'go'
# Window name, minutes and share of the monthly cap (5 hour 20%, weekly 50%, monthly 100%).
WINDOWS = (('five_hour', 5 * 60, 0.2), ('seven_day', 7 * 24 * 60, 0.5), ('thirty_day', 30 * 24 * 60, 1.0))
LIMIT_WINDOWS = {'weekly': 'seven_day', 'monthly': 'thirty_day'}
# Both stores hold assistant turns: v1 `message` and v2 `session_message` (opencode 2.0.30+).
# v2 also holds copies of migrated v1 rows (same id), which must count once.
STORES = (
    ('message', "json_extract(data,'$.providerID')", ' AND id NOT IN (SELECT id FROM session_message)'),
    ('session_message', "json_extract(data,'$.model.providerID')", " AND type='assistant'"),
)


def default_identity():
    from provider_identity import selected_identity
    return selected_identity('opencode')


def spend(db, now, providers):
    """Dollars per window over paid providers from both stores; None if neither is readable."""
    since = [int((now - timedelta(minutes=minutes)).timestamp() * 1000) for _, minutes, _ in WINDOWS]
    sums = ','.join("COALESCE(SUM(CASE WHEN time_created>=? THEN json_extract(data,'$.cost') ELSE 0 END),0)"
                    for _ in WINDOWS)
    marks = ','.join('?' * len(providers))
    totals, readable = [0.0] * len(WINDOWS), False
    for table, provider, extra in STORES:
        try:
            row = db.execute(f'SELECT {sums} FROM {table} WHERE time_created BETWEEN ? AND ?{extra} '
                             f'AND {provider} IN ({marks})',
                             (*since, min(since), int(now.timestamp() * 1000), *providers)).fetchone()
        except sqlite3.Error:
            continue
        readable = True
        totals = [total + float(value or 0) for total, value in zip(totals, row)]
    return totals if readable else None


def limit_errors(db, now, providers):
    """Latest GoUsageLimitError time (ms) per window name from recent failed assistant turns.

    A paid turn that later succeeded shows the provider accepted requests again (the limit
    was lifted or reset), so it supersedes earlier errors.
    """
    oldest = int((now - timedelta(minutes=WINDOWS[-1][1])).timestamp() * 1000)
    try:
        rows = db.execute("SELECT time_created, json_extract(data,'$.error.response.body') FROM session_message "
                          "WHERE type='assistant' AND json_extract(data,'$.error.type')='provider.quota' "
                          "AND time_created BETWEEN ? AND ? ORDER BY time_created", (oldest, int(now.timestamp() * 1000)))
        rows = rows.fetchall()
        marks = ','.join('?' * len(providers))
        accepted = db.execute("SELECT MAX(time_created) FROM session_message WHERE type='assistant' "
                              "AND json_extract(data,'$.error') IS NULL AND json_extract(data,'$.cost')>0 "
                              f"AND json_extract(data,'$.model.providerID') IN ({marks})", providers).fetchone()[0] or 0
    except sqlite3.Error:
        return {}
    latest = {}
    for created, body in rows:
        try:
            body = json.loads(body)
            if body['error']['type'] != 'GoUsageLimitError':
                continue
            name = str((body.get('metadata') or {}).get('limitName') or '').lower()
        except (TypeError, ValueError, KeyError):
            continue
        if created > accepted:
            latest[LIMIT_WINDOWS.get(name, 'five_hour')] = created
    return latest


def monthly_cap(account):
    limits = account.options.get('limits')
    try:
        cap = float(limits['monthly_usd'])
    except (KeyError, TypeError, ValueError):
        cap = PLAN_MONTHLY_USD.get(str(account.plan or DEFAULT_PLAN).strip().lower(), 0)
    return cap if cap > 0 else None


def fetch(account, io, now):
    database = account.identity_dir / 'opencode.db'
    providers = tuple(account.options.get('providers') or PAID)
    try:
        with closing(sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True)) as db:
            spent, errors = spend(db, now, providers), limit_errors(db, now, providers)
    except (sqlite3.Error, OSError):
        spent, errors = None, {}
    cap = monthly_cap(account)
    windows, unreported = [], []
    for index, (name, minutes, share) in enumerate(WINDOWS):
        used = spent[index] / (cap * share) * 100 if cap and spent is not None else None
        reset = None
        if name in errors and errors[name] + minutes * 60_000 > now.timestamp() * 1000:
            # The provider refused a request: exhausted, and at worst until the window has passed.
            used = max(used or 0, 100)
            reset = (errors[name] + minutes * 60_000)
        if used is None:
            unreported.append(name)
            continue
        windows.append({'name': name, 'raw_kind': 'local_spend_usd', 'used_percent': used,
                        'window_minutes': minutes, 'reset_at': reset})
    assumed = ', assuming Go plan' if account.plan is None else ''
    return {'windows': windows, 'unreported': unreported, 'plan': None,
            'source': 'local estimate: partial, this machine only (other hosts not counted); '
                      f'plan-wide caps calibrated, not documented{assumed}; GoUsageLimitError is authoritative'}
