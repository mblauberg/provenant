"""Cursor plan usage from its usage-summary endpoint and the local signed-in session.

Adapted from token-meter (MIT, Galileo Agent Labs); see THIRD_PARTY_NOTICES.md.
"""

from __future__ import annotations

import base64
import json
from pathlib import Path
import re
import sqlite3
import sys


ADAPTER = 'cursor'
SUMMARY_URL = 'https://cursor.com/api/usage-summary'


def default_identity():
    home = Path.home()
    if sys.platform == 'darwin':
        return home / 'Library/Application Support/Cursor/User/globalStorage'
    return home / '.config/Cursor/User/globalStorage'


def session(database, now, io=None):
    """The access token and user id from Cursor's state database, opened read-only."""
    unavailable = io.Unavailable if io else ValueError
    try:
        with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as db:
            row = db.execute("SELECT value FROM ItemTable WHERE key='cursorAuth/accessToken' LIMIT 1").fetchone()
    except sqlite3.Error:
        raise unavailable('Cursor is not signed in locally') from None
    token = row[0] if row else None
    if isinstance(token, (bytes, memoryview)):
        token = bytes(token).decode('utf-8', 'replace')
    if not isinstance(token, str) or not token.strip():
        raise unavailable('Cursor is not signed in locally')
    try:
        claims = token.split('.')[1]
        claims = json.loads(base64.urlsafe_b64decode(claims + '=' * (-len(claims) % 4)))
        user = str(claims.get('sub') or '').split('|')[-1]
        expires = float(claims['exp'])
    except (IndexError, KeyError, TypeError, ValueError):
        raise unavailable('Cursor local session is invalid') from None
    if not re.fullmatch(r'[A-Za-z0-9._-]+', user):
        raise unavailable('Cursor local session is invalid')
    if expires <= now.timestamp() + 60:
        raise unavailable('Cursor session needs a refresh; open Cursor once')
    return token, user


def parse(payload, now=None):
    individual = payload.get('individualUsage') if isinstance(payload.get('individualUsage'), dict) else {}
    team = payload.get('teamUsage') if isinstance(payload.get('teamUsage'), dict) else {}
    windows = []
    for kind, value in (('plan', individual.get('plan')), ('overall', individual.get('overall')),
                        ('pooled', team.get('pooled'))):
        if not isinstance(value, dict):
            continue
        try:
            used, limit = float(value['used']), float(value['limit'])
        except (KeyError, TypeError, ValueError):
            continue
        if limit > 0:
            windows.append({'name': 'billing_cycle', 'raw_kind': kind, 'used_percent': used / limit * 100,
                            'window_minutes': None, 'reset_at': payload.get('billingCycleEnd'),
                            'window_start': payload.get('billingCycleStart')})
            break
    return {'windows': windows, 'unreported': [] if windows else ['billing_cycle'],
            'plan': payload.get('membershipType') or payload.get('limitType')}


def fetch(account, io, now):
    token, user = session(account.identity_dir / 'state.vscdb', now, io)
    payload = io.http_json(SUMMARY_URL, {'Accept': 'application/json',
                                         'Cookie': f'WorkosCursorSessionToken={user}%3A%3A{token}'})
    return {**parse(payload, now), 'source': 'Cursor usage-summary endpoint'}
