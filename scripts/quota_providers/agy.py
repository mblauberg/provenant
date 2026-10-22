"""Antigravity CLI quota pools from its print-mode `/usage` answer.

`agy -p /usage --output-format json` reads the account's quota summary without starting
an agent turn or spending quota; the CLI holds the credential, never this process. One
Keychain item serves every Google account agy signs in to, so the account is named by a
hash of the signed-in address that agy writes to its own log.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
from pathlib import Path
import re
import tempfile
import threading


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
    # The identity is the login Keychain on macOS and the state directory elsewhere; either way
    # agy's state directory shows agy is in use.
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
    folder = state_dir() / 'log'
    try:  # Names sort by time; comparing strings, not paths, keeps a large log folder cheap.
        names = [name for name in os.listdir(folder) if name.startswith('cli-') and name.endswith('.log')]
    except OSError:
        names = []
    for name in sorted(names, reverse=True)[:LOGS]:
        email = _signed_in(folder / name, limit)
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
            # A boolean, NaN or out-of-range fraction would read as a false 0% or 100%.
            valid = isinstance(remaining, (int, float)) and not isinstance(remaining, bool) and \
                0 <= remaining <= 1 and math.isfinite(remaining)
            if bucket.get('disabled') or not valid:
                unreported.append(name)
                continue
            windows.append({'name': name, 'raw_kind': bucket.get('id'), 'used_percent': (1 - remaining) * 100,
                            'window_minutes': window_minutes(bucket.get('window')), 'reset_at': bucket.get('reset_time')})
    return {'windows': windows, 'unreported': [name for name in unreported if name]}


def usage(io):
    """The `/usage` payload and the signed-in address from that run's own temporary log."""
    executable = io.executable('agy')
    if not executable:
        raise io.Unavailable('agy is not installed', 'missing')
    with tempfile.TemporaryDirectory(prefix='provenant-agy-') as scratch:
        log = Path(scratch) / 'cli.log'
        try:
            process = io.open_process([executable, '-p', '/usage', '--output-format', 'json', '--log-file', str(log)],
                                      stdin=io.DEVNULL, stdout=io.PIPE, stderr=io.DEVNULL, cwd=scratch)
        except OSError:
            raise io.Unavailable('agy quota request failed') from None
        # The deadline kills the CLI, which ends the bounded read below.
        deadline = threading.Timer(TIMEOUT, process.kill)
        deadline.start()
        try:
            raw = process.stdout.read(io.MAX_RESPONSE_BYTES + 1)
            if len(raw) > io.MAX_RESPONSE_BYTES:
                process.kill()
            code = process.wait()
        except OSError:
            process.kill()
            raise io.Unavailable('agy quota request failed') from None
        finally:
            deadline.cancel()
            process.stdout.close()
        email = _signed_in(log, io.MAX_RESPONSE_BYTES)
    if code != 0 or len(raw) > io.MAX_RESPONSE_BYTES:
        raise io.Unavailable('agy quota request failed')
    try:
        payload = json.loads(raw.decode('utf-8'))
    except (UnicodeDecodeError, ValueError):
        raise io.Unavailable('agy returned an invalid quota response') from None
    if not isinstance(payload, dict) or payload.get('status') != 'SUCCESS':
        raise io.Unavailable('agy could not read quota; sign in to agy', 'auth')
    return payload, email


def fetch(account, io, now):
    payload, email = usage(io)
    # A discovered account is named by its fingerprint; never file a reading under another one.
    if account.label.startswith('google-') and (not email or fingerprint(email) != account.label):
        raise io.Unavailable('agy is signed in to a different account than its log shows; run agy once', 'auth')
    parsed = parse(payload, now)
    if not parsed['windows']:
        raise io.Unavailable('agy reported no quota')
    return {**parsed, 'source': 'agy print-mode /usage'}
