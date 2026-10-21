"""Antigravity CLI quota pools from its print-mode `/usage` answer.

`agy -p /usage --output-format json` reads the account's quota summary without starting
an agent turn or spending quota; the CLI holds the credential, never this process. One
Keychain item serves every Google account agy signs in to, so the account is named by a
hash of the signed-in address that agy writes to its own log.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import tempfile


# Quota is report-only for agy: a provider-wide cooldown would also block the pool that
# still has headroom. Per-pool routing is the other half of #1062.
ADAPTER = None
TIMEOUT = 30.0
LOGS = 20
SIGNED_IN = re.compile(rb'OAuth: authenticated successfully as (\S+@\S+)')
UNITS = {'m': 1, 'h': 60, 'd': 1440, 'w': 10080}
NAMED = {'hourly': 60, 'daily': 1440, 'weekly': 10080}


def state_dir():
    return Path.home() / '.gemini/antigravity-cli'


def default_identity():
    from provider_identity import selected_identity
    # The login Keychain exists on every Mac; agy's state directory shows agy is in use.
    return selected_identity('agy') if state_dir().is_dir() else None


def fingerprint(email):
    return 'google-' + hashlib.sha256(email.strip().lower().encode()).hexdigest()[:12]


def _signed_in(path, limit):
    try:
        with open(path, 'rb') as stream:
            stream.seek(max(0, path.stat().st_size - limit))
            found = SIGNED_IN.findall(stream.read(limit))
    except OSError:
        return None
    return found[-1].decode('utf-8', 'replace') if found else None


def account_label(limit=2 * 1024 * 1024):
    """The signed-in account from agy's newest log that records one. Reads no credential."""
    for path in sorted((state_dir() / 'log').glob('cli-*.log'), reverse=True)[:LOGS]:
        email = _signed_in(path, limit)
        if email:
            return fingerprint(email)
    return None


def window_minutes(value):
    value = str(value or '').strip().lower()
    match = re.fullmatch(r'(\d+)\s*([mhdw])', value)
    return int(match[1]) * UNITS[match[2]] if match else NAMED.get(value)


def parse(payload, now=None):
    """One window per pool bucket, such as `gemini.weekly` or `claude.5h`."""
    data = (payload.get('command') or {}).get('data') if isinstance(payload.get('command'), dict) else None
    data = data if isinstance(data, dict) else {}
    groups = [(None, data.get('buckets'))] + [
        (re.sub(r'[^a-z0-9]+', '-', str(group.get('name') or '').lower()).strip('-').split('-')[0], group.get('buckets'))
        for group in data.get('groups') or [] if isinstance(group, dict)]
    windows, unreported = [], []
    for pool, buckets in groups:
        for bucket in buckets if isinstance(buckets, list) else []:
            if not isinstance(bucket, dict):
                continue
            name = f"{pool}.{bucket.get('window')}" if pool and bucket.get('window') else str(bucket.get('id') or '')
            remaining = bucket.get('remaining_fraction')
            if bucket.get('disabled') or not isinstance(remaining, (int, float)):
                unreported.append(name)
                continue
            windows.append({'name': name, 'raw_kind': bucket.get('id'), 'used_percent': (1 - remaining) * 100,
                            'window_minutes': window_minutes(bucket.get('window')), 'reset_at': bucket.get('reset_time')})
    return {'windows': windows, 'unreported': [name for name in unreported if name]}


def usage(io):
    """The `/usage` payload and the signed-in address from that run's own temporary log."""
    executable = io.executable('agy')
    if not executable:
        raise io.Unavailable('agy is not installed')
    with tempfile.TemporaryDirectory(prefix='provenant-agy-') as scratch:
        log = Path(scratch) / 'cli.log'
        try:
            result = io.run_process([executable, '-p', '/usage', '--output-format', 'json', '--log-file', str(log)],
                                    capture_output=True, text=True, timeout=TIMEOUT, check=False,
                                    cwd=scratch, stdin=io.DEVNULL)
        except (OSError, io.TimeoutExpired):
            raise io.Unavailable('agy quota request failed') from None
        email = _signed_in(log, io.MAX_RESPONSE_BYTES)
    if result.returncode != 0 or len(result.stdout or '') > io.MAX_RESPONSE_BYTES:
        raise io.Unavailable('agy quota request failed')
    try:
        payload = json.loads(result.stdout)
    except ValueError:
        raise io.Unavailable('agy returned an invalid quota response') from None
    if not isinstance(payload, dict) or payload.get('status') != 'SUCCESS':
        raise io.Unavailable('agy could not read quota; sign in to agy')
    return payload, email


def fetch(account, io, now):
    payload, email = usage(io)
    # A discovered account is named by its fingerprint; never file a reading under another one.
    if account.label.startswith('google-') and (not email or fingerprint(email) != account.label):
        raise io.Unavailable('agy is signed in to a different account than its log shows; run agy once')
    parsed = parse(payload, now)
    if not parsed['windows']:
        raise io.Unavailable('agy reported no quota')
    return {**parsed, 'source': 'agy print-mode /usage'}
