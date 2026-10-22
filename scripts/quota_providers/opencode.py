"""OpenCode Go usage from its local message stores, plus its own usage-limit errors.

Go limits are plan-wide dollar amounts over 5 hour, weekly and monthly windows. OpenCode
documents no usage endpoint or reset semantics, so spend here is a rolling local estimate
from this machine's `opencode.db` only: other hosts using the same account are not counted.
A recent `GoUsageLimitError` in the store is authoritative and marks that window exhausted.
Free zen models cost nothing and are ignored. Only message tables are read, never credentials.
"""

from __future__ import annotations

from contextlib import closing
from calendar import monthrange
from datetime import UTC, timedelta
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
    ('message', "json_extract(data,'$.providerID')", ''),
    ('session_message', "json_extract(data,'$.model.providerID')", " AND type='assistant'"),
)
# A per-row primary-key probe; `NOT IN (SELECT id ...)` would read every v2 id first.
SEEN_IN_V2 = ' AND NOT EXISTS (SELECT 1 FROM session_message AS v2 WHERE v2.id=message.id)'
# v1 `message` has no time index, only (session_id, time_created, id). Each of its session ids,
# read from that index alone, gives a time-range search, so no message body outside the window
# is read. The message times stay the filter: a session's own time_updated can be older than its
# messages.
SESSION_RANGES = ' AND session_id IN (SELECT DISTINCT session_id FROM message)'



def default_identity():
    from provider_identity import selected_identity
    return selected_identity('opencode')


def add_months(anchor, count):
    """The anchor `count` calendar months on, its day clamped to the month's length."""
    year, month = divmod(anchor.year * 12 + anchor.month - 1 + count, 12)
    return anchor.replace(year=year, month=month + 1, day=min(anchor.day, monthrange(year, month + 1)[1]))


def cycles(now, anchor, weekly=None):
    """(name, start, end, minutes) per window; end is None while a window rolls.

    With a subscription anchor the weekly and monthly windows are fixed cycles from it; the
    weekly cycle may have its own anchor. The 5 hour window always rolls.
    """
    rolling = [(name, now - timedelta(minutes=minutes), None, minutes) for name, minutes, _ in WINDOWS]
    if anchor is None:
        return rolling
    weekly = weekly or anchor
    weeks = (now - weekly) // timedelta(days=7)
    week = weekly + timedelta(days=7 * weeks)
    count = (now.year - anchor.year) * 12 + now.month - anchor.month
    count -= add_months(anchor, count) > now
    month = add_months(anchor, count)
    fixed = {'seven_day': (week, week + timedelta(days=7)), 'thirty_day': (month, add_months(anchor, count + 1))}
    return [(name, *fixed[name], (fixed[name][1] - fixed[name][0]).total_seconds() / 60) if name in fixed
            else (name, start, end, minutes) for name, start, end, minutes in rolling]


def spend(db, now, providers, starts):
    """Dollars per window over paid providers from both stores; None if neither is readable."""
    since = [int(start.timestamp() * 1000) for start in starts]
    sums = ','.join("COALESCE(SUM(CASE WHEN time_created>=? THEN json_extract(data,'$.cost') ELSE 0 END),0)"
                    for _ in WINDOWS)
    marks = ','.join('?' * len(providers))
    totals, readable = [0.0] * len(WINDOWS), False
    names = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    for table, provider, extra in STORES:
        if table == 'message':
            columns = {row[1] for row in db.execute('PRAGMA table_info(message)')}
            extra = SESSION_RANGES if 'session_id' in columns else ''
            extra += SEEN_IN_V2 if 'session_message' in names else ''
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
    oldest = int((now - timedelta(days=32)).timestamp() * 1000)
    try:
        rows = db.execute("SELECT time_created, json_extract(data,'$.error.response.body') FROM session_message "
                          "WHERE type='assistant' AND json_extract(data,'$.error.type')='provider.quota' "
                          "AND time_created BETWEEN ? AND ? ORDER BY time_created", (oldest, int(now.timestamp() * 1000)))
        rows = rows.fetchall()
        marks = ','.join('?' * len(providers))
        accepted = db.execute("SELECT MAX(time_created) FROM session_message WHERE type='assistant' "
                              "AND json_extract(data,'$.error') IS NULL AND json_extract(data,'$.cost')>0 "
                              f"AND time_created BETWEEN ? AND ? AND json_extract(data,'$.model.providerID') IN ({marks})",
                              (oldest, int(now.timestamp() * 1000), *providers)).fetchone()[0] or 0
    except sqlite3.Error:
        return {}
    latest = {}
    for created, body in rows:
        try:
            body = json.loads(body)
            if body['error']['type'] != 'GoUsageLimitError':
                continue
            metadata = body.get('metadata')
            name = str(metadata.get('limitName') or '').lower() if isinstance(metadata, dict) else ''
        except (AttributeError, TypeError, ValueError, KeyError):
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


def anchor_of(account, io, key='cycle_anchor'):
    limits = account.options.get('limits')
    anchor = io.parse_time(limits.get(key)) if isinstance(limits, dict) else None
    return anchor.astimezone(UTC) if anchor else None


def fetch(account, io, now):
    database = account.identity_dir / 'opencode.db'
    providers = tuple(account.options.get('providers') or PAID)
    anchor = anchor_of(account, io)
    frames = cycles(now, anchor, anchor_of(account, io, 'weekly_anchor'))
    try:
        with closing(sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True)) as db:
            spent = spend(db, now, providers, [start for _, start, _, _ in frames])
            errors = limit_errors(db, now, providers)
    except (sqlite3.Error, OSError):
        spent, errors = None, {}
    cap = monthly_cap(account)
    windows, unreported = [], []
    for (name, start, end, minutes), (_, _, share), dollars in zip(
            frames, WINDOWS, spent or [None] * len(WINDOWS)):
        used = dollars / (cap * share) * 100 if cap and dollars is not None else None
        reset = int(end.timestamp() * 1000) if end else None
        failed = errors.get(name)
        if failed and failed >= start.timestamp() * 1000:
            # The provider refused a request: exhausted until the cycle ends, or at worst a window on.
            used = max(used or 0, 100)
            reset = reset or failed + minutes * 60_000
        if used is None:
            unreported.append(name)
            continue
        windows.append({'name': name, 'raw_kind': 'local_spend_usd', 'used_percent': used,
                        'window_minutes': minutes, 'reset_at': reset})
    assumed = ', assuming Go plan' if account.plan is None else ''
    cycle = 'fixed cycles from the configured anchor' if anchor else 'rolling windows, resets estimated'
    return {'windows': windows, 'unreported': unreported, 'plan': None,
            'source': 'local estimate: partial, this machine only (other hosts not counted); '
                      f'{cycle}; plan-wide caps calibrated, not documented{assumed}; '
                      'GoUsageLimitError is authoritative'}
