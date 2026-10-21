"""OpenCode Go spend from its local message store, compared with limits the account configures.

OpenCode documents per-model dollar limits (5 hour, weekly, monthly) but offers no usage
endpoint or CLI read, so this is a local estimate: spend on this machine only, against
`limits` in the account's config. Without configured limits every window is unreported.
Free zen models cost nothing and are ignored.
"""

from __future__ import annotations

from contextlib import closing
from datetime import timedelta
import sqlite3


ADAPTER = 'opencode'
PAID = ('opencode-go',)
WINDOWS = (('five_hour', 5 * 60), ('seven_day', 7 * 24 * 60), ('thirty_day', 30 * 24 * 60))


def default_identity():
    from provider_identity import selected_identity
    return selected_identity('opencode')


def recent_cost(database, since, providers):
    """Dollars of paid-provider messages since `since`; None when the store cannot be read."""
    marks = ','.join('?' * len(providers))
    try:
        with closing(sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True)) as db:
            row = db.execute("SELECT COALESCE(SUM(json_extract(data,'$.cost')),0) FROM message "
                             f"WHERE time_created>=? AND json_extract(data,'$.providerID') IN ({marks})",
                             (int(since.timestamp() * 1000), *providers)).fetchone()
        return float(row[0])
    except (sqlite3.Error, OSError, TypeError, ValueError):
        return None


def limit_for(limits, key):
    try:
        value = float(limits[key])
    except (KeyError, TypeError, ValueError):
        return None
    return value if value > 0 else None


def fetch(account, io, now):
    limits = account.options.get('limits')
    limits = limits if isinstance(limits, dict) else {}
    providers = tuple(account.options.get('providers') or PAID)
    windows, unreported = [], []
    for key, minutes in WINDOWS:
        limit = limit_for(limits, key)
        spent = None if limit is None else recent_cost(
            account.identity_dir / 'opencode.db', now - timedelta(minutes=minutes), providers)
        if spent is None:
            unreported.append(key)
            continue
        windows.append({'name': key, 'raw_kind': 'local_spend_usd', 'used_percent': spent / limit * 100,
                        'window_minutes': minutes, 'reset_at': None})
    return {'windows': windows, 'unreported': unreported, 'plan': None,
            'source': 'local estimate: opencode.db spend against configured limits (this machine only)'}
