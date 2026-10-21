#!/usr/bin/env python3
"""Provider-reported quota per account: windows, pace, reset events, credits and routing input.

Accounts come from configuration; quota sources come from one module per provider under
`quota_providers/`. Nothing here knows a plan, a limit or a window length: every window,
limit and reset time comes from the provider's own response. Credentials are read
in-process only to call that provider's endpoint, and never reach the cache, the ledger,
logs or stdout. Pace and window handling are adapted from token-meter (MIT, Galileo Agent
Labs); see THIRD_PARTY_NOTICES.md.
"""

from __future__ import annotations

import argparse
from collections import namedtuple
from contextlib import contextmanager
from datetime import UTC, datetime, timedelta
import importlib.util
import fcntl
import json
import math
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from time import monotonic
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, target):
        return None


# Authenticated quota requests must stay at the provider's exact endpoint.
urlopen = build_opener(_NoRedirect()).open


SCHEMA = 'provenant.quota.v1'
REPORT_SCHEMA = 'provenant.quota.report.v1'
HERE = Path(__file__).resolve().parent
# Adding a provider is one module exposing ADAPTER, default_identity() and
# fetch(account, io, now), plus one line here.
REGISTRY = {
    'claude': HERE / 'quota_providers/claude.py',
    'codex': HERE / 'quota_providers/codex.py',
    'cursor': HERE / 'quota_providers/cursor.py',
}
CACHE_SECONDS = 300
HTTP_TIMEOUT = 8.0
PROCESS_TIMEOUT = 10.0
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
PACE_TOLERANCE = 4      # percentage points either side of even use
EARLY_SHARE = 0.03      # too little of a window has passed to judge its pace
RESET_NOISE = 5         # a smaller drop is reporting noise, not a granted reset
NEAR_EXHAUSTED = 95
WAIT_HOURS = 6
SEVERITY = ('exhausted', 'deficit', 'on_pace', 'reserve', 'early', 'not_started', 'unknown', 'unreported')
LABEL = re.compile(r'[a-z0-9][a-z0-9._-]{0,63}')
DISABLED = {'0', 'false', 'no', 'off'}

PIPE, DEVNULL, TimeoutExpired = subprocess.PIPE, subprocess.DEVNULL, subprocess.TimeoutExpired
run_process = subprocess.run
open_process = subprocess.Popen
executable = shutil.which


class Unavailable(Exception):
    """A provider could not report quota. The message is fixed text, never a credential."""


# A non-secret label, the CLI identity directory, and whether that is the CLI's default.
Account = namedtuple('Account', 'provider label identity_dir active plan priority', defaults=(False, None, None))


class _Io:
    """What provider modules may use; resolved at call time so tests can replace it."""

    def __getattr__(self, name):
        return globals()[name]


IO = _Io()
_TELEMETRY = None
_LOADED = {}


# Time and numbers ---------------------------------------------------------------------------

def utcnow():
    return datetime.now(UTC)


def iso(moment):
    return moment.astimezone(UTC).isoformat().replace('+00:00', 'Z') if moment else None


def number(value):
    if isinstance(value, bool):
        return None
    try:
        value = float(value)
    except (TypeError, ValueError):
        return None
    return value if math.isfinite(value) else None


def parse_time(value):
    seconds = number(value)
    if seconds is not None:
        if seconds > 10_000_000_000:
            seconds /= 1000
        return datetime.fromtimestamp(seconds, UTC) if seconds > 0 else None
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        moment = datetime.fromisoformat(value.strip())
    except ValueError:
        return None
    return moment if moment.tzinfo else moment.replace(tzinfo=UTC)


def duration(seconds):
    seconds = max(0, int(seconds or 0))
    minutes, hours = seconds // 60, seconds // 3600
    if seconds < 3600:
        return f'{minutes}m'
    if hours < 48:
        return f'{hours}h' + (f' {minutes % 60}m' if minutes % 60 else '')
    return f'{hours // 24}d' + (f' {hours % 24}h' if hours % 24 else '')


def window_length(minutes):
    return duration(minutes * 60) if minutes else '-'


# Configuration, registry and accounts --------------------------------------------------------

def _telemetry():
    global _TELEMETRY
    if _TELEMETRY is None:
        spec = importlib.util.spec_from_file_location('provenant_telemetry_for_quota', HERE / 'telemetry.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _TELEMETRY = module
    return _TELEMETRY


def telemetry_host():
    return _telemetry().host_id()


def state_root():
    return _telemetry().state_root()


def config():
    root = Path(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT') or '~/.agents').expanduser()
    try:
        value = json.loads((root / 'config/telemetry.json').read_text())
    except (OSError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def settings():
    value = config().get('quota')
    return value if isinstance(value, dict) else {}


def enabled():
    if os.environ.get('PROVENANT_QUOTA', '').strip().lower() in DISABLED:
        return False
    return settings().get('enabled', True) is not False


def cache_seconds():
    return max(CACHE_SECONDS, int(number(settings().get('cache_seconds')) or CACHE_SECONDS))


def register(name, module):
    """Register a quota source at runtime; the module needs the same three names."""
    _LOADED[name] = module


def provider(name):
    if name not in _LOADED and name in REGISTRY:
        spec = importlib.util.spec_from_file_location(f'provenant_quota_{name}', REGISTRY[name])
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _LOADED[name] = module
    return _LOADED.get(name)


def registered():
    return sorted(set(REGISTRY) | set(_LOADED))


def same_path(left, right):
    try:
        return Path(left).expanduser().resolve() == Path(right).expanduser().resolve()
    except OSError:
        return False


def configured():
    value = config().get('accounts') or {}
    if not isinstance(value, dict) or not all(isinstance(entries, dict) for entries in value.values()):
        raise ValueError('telemetry accounts must map provider to {label: {identity_dir}}')
    for entries in value.values():
        for label in entries:
            if not LABEL.fullmatch(str(label)):
                raise ValueError(f'account label {label!r} must be a short lowercase name, never an email or token')
    return value


def accounts():
    """Configured accounts; a provider with no configuration gets its local default, if any."""
    listed = configured()
    found = []
    for name in sorted(set(registered()) | set(listed)):
        module = provider(name)
        default = module.default_identity() if module else None
        if name not in listed:
            if default is not None and Path(default).exists():
                found.append(Account(name, 'default', Path(default), True))
            continue
        for label, entry in sorted(listed[name].items()):
            entry = entry if isinstance(entry, dict) else {}
            if entry.get('enabled') is False:
                continue
            if not entry.get('identity_dir') and default is None:
                raise ValueError(f'{name}/{label} requires identity_dir')
            identity = Path(entry.get('identity_dir') or default).expanduser()
            priority = number(entry.get('priority'))
            found.append(Account(name, label, identity, default is not None and same_path(identity, default),
                                 str(entry['plan']) if entry.get('plan') else None,
                                 int(priority) if priority is not None else None))
    return found


# Provider I/O for quota modules -------------------------------------------------------------

def http_json(url, headers):
    request = Request(url, headers=headers, method='GET')
    try:
        with urlopen(request, timeout=HTTP_TIMEOUT) as response:
            raw = response.read(MAX_RESPONSE_BYTES + 1)
    except HTTPError as exc:
        code = exc.code
        exc.close()
        if code in (401, 403):
            raise Unavailable('provider authentication needs a refresh') from None
        if code == 429:
            raise Unavailable('provider quota service is rate limited') from None
        raise Unavailable(f'provider quota request failed (HTTP {code})') from None
    except (URLError, TimeoutError, OSError):
        raise Unavailable('provider quota request could not connect') from None
    if len(raw) > MAX_RESPONSE_BYTES:
        raise Unavailable('provider quota response was too large')
    try:
        value = json.loads(raw.decode('utf-8'))
    except (UnicodeDecodeError, ValueError):
        raise Unavailable('provider returned an invalid quota response') from None
    if not isinstance(value, dict):
        raise Unavailable('provider returned an invalid quota response')
    return value


def ledger_windows(adapter, now):
    """The latest rate-limit observation per window that this host's ledger already holds."""
    try:
        with _telemetry().connect() as db:
            rows = db.execute('SELECT window,used_percent,window_minutes,reset_at,timestamp FROM observations '
                              'WHERE adapter=? AND host=? AND used_percent IS NOT NULL '
                              'ORDER BY timestamp DESC LIMIT 500', (adapter, telemetry_host())).fetchall()
    except (OSError, ValueError, sqlite3.Error):
        rows = []
    latest = {}
    for row in rows:
        latest.setdefault(row['window'], row)
    windows = [{'name': name, 'raw_kind': name, 'used_percent': row['used_percent'],
                'window_minutes': row['window_minutes'], 'reset_at': row['reset_at']}
               for name, row in sorted(latest.items())]
    observed = max((row['timestamp'] for row in latest.values()), default=None)
    return {'windows': windows, 'unreported': [], 'observed_at': iso(parse_time(observed))}


# Snapshots, cache and reset events ----------------------------------------------------------

def normalise_window(raw):
    name = str((raw or {}).get('name') or '').strip()
    used = number((raw or {}).get('used_percent'))
    if not name or used is None:
        return None
    minutes = number(raw.get('window_minutes'))
    reset, start = parse_time(raw.get('reset_at')), parse_time(raw.get('window_start'))
    if minutes is None and start and reset and reset > start:
        minutes = (reset - start).total_seconds() / 60
    if minutes is not None and minutes <= 0:
        minutes = None
    if reset and minutes:
        start = reset - timedelta(minutes=minutes)
    if minutes is not None and minutes.is_integer():
        minutes = int(minutes)
    return {'name': name, 'raw_kind': str(raw['raw_kind']) if raw.get('raw_kind') is not None else None,
            'used_percent': round(max(0.0, min(100.0, used)), 2), 'window_minutes': minutes,
            'reset_at': iso(reset), 'window_start': iso(start)}


def cache_path(provider_name, label, host=None):
    host = host or telemetry_host()
    return state_root() / 'quota' / f'{provider_name}--{label}--{host}.json'


def load_snapshot(provider_name, label):
    try:
        value = json.loads(cache_path(provider_name, label).read_text())
    except (OSError, ValueError):
        # Read the previous two-part filename only when its host matches this host.
        try:
            value = json.loads((state_root() / 'quota' / f'{provider_name}--{label}.json').read_text())
        except (OSError, ValueError):
            return None
    return value if isinstance(value, dict) and value.get('schema') == SCHEMA and value.get('host') == telemetry_host() else None


@contextmanager
def locked(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('a') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(stream, fcntl.LOCK_UN)


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, prefix=path.name + '.', delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(json.dumps(value, indent=2, sort_keys=True) + '\n')
        temporary.replace(path)
    finally:
        if temporary:
            temporary.unlink(missing_ok=True)


def save_snapshot(value):
    atomic_json(cache_path(value['provider'], value['account'], value['host']), value)


def snapshots():
    values = []
    for path in sorted((state_root() / 'quota').glob('*--*.json')):
        try:
            value = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        if isinstance(value, dict) and value.get('schema') == SCHEMA:
            values.append(value)
    return values


def blank(account, status, error=None, now=None):
    return {'schema': SCHEMA, 'provider': account.provider, 'account': account.label, 'plan': account.plan,
            'host': telemetry_host(), 'active': account.active, 'priority': account.priority,
            'observed_at': None, 'attempted_at': iso(now) if now else None, 'source': None,
            'status': status, 'error': error, 'windows': [], 'unreported': [], 'credits': None}


def classify_reset(previous, current, previous_observed, observed):
    """Name what happened between two readings of one window, from the readings alone."""
    before, after = parse_time(previous.get('reset_at')), parse_time(current.get('reset_at'))
    drop = (number(previous.get('used_percent')) or 0) - (number(current.get('used_percent')) or 0)
    observed = parse_time(observed) if isinstance(observed, str) else observed
    kind = None
    if after is None:
        kind = 'not_started' if before is not None or drop > RESET_NOISE else None
    elif before is None:
        kind = 'window_started'
    elif observed and observed >= before:
        kind = 'natural'
    else:
        minutes = number(current.get('window_minutes')) or number(previous.get('window_minutes')) or 0
        moved = abs((after - before).total_seconds()) > max(300, minutes * 0.6)
        if drop > RESET_NOISE:
            restarted = minutes and abs((after - observed).total_seconds() - minutes * 60) <= max(300, minutes * 6)
            kind = 'granted_window_restarted' if moved and after > before and restarted else \
                'window_moved' if moved else 'granted_date_kept'
        elif moved:
            kind = 'window_moved'
    if kind is None:
        return None
    return {'kind': kind, 'previous_used_percent': previous.get('used_percent'),
            'used_percent': current.get('used_percent'), 'previous_reset_at': previous.get('reset_at'),
            'reset_at': current.get('reset_at')}


def record_events(previous, current):
    if not previous or not previous.get('windows'):
        return []
    before = {row['name']: row for row in previous['windows']}
    events = []
    for row in current['windows']:
        if row['name'] in before:
            event = classify_reset(before[row['name']], row, previous.get('observed_at'), current['observed_at'])
            if event:
                events.append({**event, 'window': row['name']})
    if not events:
        return events
    try:
        with _telemetry().connect() as db:
            db.executemany(
                'INSERT OR IGNORE INTO quota_events(provider,account,host,window,kind,observed_at,'
                'previous_observed_at,previous_used_percent,used_percent,previous_reset_at,reset_at) '
                'VALUES (?,?,?,?,?,?,?,?,?,?,?)',
                [(current['provider'], current['account'], current['host'], event['window'], event['kind'],
                  current['observed_at'], previous.get('observed_at'), event['previous_used_percent'],
                  event['used_percent'], event['previous_reset_at'], event['reset_at']) for event in events])
    except (OSError, ValueError, sqlite3.Error):
        pass  # The ledger is advisory here; a quota reading never fails on it.
    return events


def fetch(account, now):
    module = provider(account.provider)
    if module is None:
        raise Unavailable(f'no quota source is registered for {account.provider}')
    return module.fetch(account, IO, now)


def refresh(account, now=None, force=False):
    """The account's snapshot, fetched at most once per cache period unless forced."""
    if not enabled():
        return _refresh(account, now, force)
    with locked(cache_path(account.provider, account.label).with_suffix('.lock')):
        return _refresh(account, now, force)


def _refresh(account, now=None, force=False):
    now = now or utcnow()
    if not enabled():
        return load_snapshot(account.provider, account.label) or blank(account, 'disabled')
    if provider(account.provider) is None:
        return blank(account, 'unreported', f'no quota source is registered for {account.provider}')
    previous = load_snapshot(account.provider, account.label)
    attempted = parse_time((previous or {}).get('attempted_at'))
    if previous and not force and attempted and (now - attempted).total_seconds() < cache_seconds():
        return {**previous, 'active': account.active, 'priority': account.priority,
                'plan': account.plan or previous.get('plan')}
    try:
        result = fetch(account, now)
    except Unavailable as exc:
        error = str(exc)
    except Exception as exc:  # A provider module bug must not break quota or routing.
        error = f'quota source failed ({type(exc).__name__})'
    else:
        windows, unreported = [], [str(name) for name in result.get('unreported') or []]
        for raw in result.get('windows') or []:
            row = normalise_window(raw)
            if row:
                windows.append(row)
            elif isinstance(raw, dict) and raw.get('name'):
                unreported.append(str(raw['name']))
        names = {row['name'] for row in windows}
        # A window this account reported before and not now is unreported, never 0%.
        unreported += [row['name'] for row in (previous or {}).get('windows', [])
                       if row['name'] not in names and row['name'] not in unreported]
        plan = account.plan or result.get('plan')
        value = {**blank(account, 'ok' if windows else 'unreported', now=now),
                 'plan': str(plan) if plan else None, 'observed_at': result.get('observed_at') or iso(now),
                 'source': result.get('source'), 'windows': windows, 'unreported': unreported,
                 'credits': result.get('credits') if isinstance(result.get('credits'), dict) else None}
        record_events(previous, value)
        save_snapshot(value)
        return value
    value = {**(previous or blank(account, 'unavailable')), 'attempted_at': iso(now), 'error': error,
             'status': 'stale' if previous and previous.get('windows') else 'unavailable',
             'active': account.active, 'priority': account.priority}
    save_snapshot(value)
    return value


# Pace, credits and the report ---------------------------------------------------------------

def pace(window, now):
    """Compare provider-reported use with even use across the reported window."""
    used = number(window.get('used_percent'))
    if used is None:
        return {'state': 'unreported', 'summary': 'unreported; missing does not mean 0%'}
    actual = max(0.0, min(100.0, used))
    reset, minutes = parse_time(window.get('reset_at')), number(window.get('window_minutes'))
    result = {'state': 'unknown', 'expected_used_percent': None, 'delta_percent': None,
              'will_last_to_reset': None, 'runs_out_at': None, 'headroom_percent': round(100 - actual, 1),
              'expiring_headroom_percent': 0, 'summary': ''}
    remaining = (reset - now).total_seconds() if reset else None
    if reset is not None and remaining <= 0:
        return {**result, 'state': 'not_started', 'headroom_percent': 100,
                'summary': f'reset passed at {iso(reset)}; awaiting a fresh reading'}
    if reset is None or minutes and remaining > minutes * 60:
        return {**result, 'state': 'not_started', 'headroom_percent': 100, 'summary': 'window starts on first use'}
    if actual >= 100:
        return {**result, 'state': 'exhausted', 'will_last_to_reset': False, 'runs_out_at': iso(now),
                'summary': 'exhausted' + (f'; resets in {duration(remaining)}' if reset else '')}
    if not minutes or remaining > minutes * 60:
        return {**result, 'summary': f'resets in {duration(remaining)}; window length unreported'}
    elapsed = minutes * 60 - remaining
    expected = elapsed / (minutes * 60) * 100
    delta = actual - expected
    state = 'deficit' if delta > PACE_TOLERANCE else 'reserve' if delta < -PACE_TOLERANCE else 'on_pace'
    lead = f'{abs(delta):.0f}% in {state}' if state != 'on_pace' else 'on pace'
    rate = actual / elapsed if elapsed > 0 else 0
    run_out = (100 - actual) / rate if rate > 0 else None
    lasts = run_out is None or run_out >= remaining
    projected = min(100.0, actual + rate * remaining)
    result.update(expected_used_percent=round(expected, 1), delta_percent=round(delta, 1), will_last_to_reset=lasts,
                  runs_out_at=None if lasts else iso(now + timedelta(seconds=run_out)),
                  expiring_headroom_percent=round(100 - projected, 1) if lasts else 0)
    if elapsed / (minutes * 60) < EARLY_SHARE:
        return {**result, 'state': 'early', 'summary': f'window just started; resets in {duration(remaining)}'}
    tail = 'lasts until reset' if lasts else f'runs out in {duration(run_out)}'
    return {**result, 'state': state, 'summary': f'{lead}; {tail}'}


def governing(rows):
    """The window that constrains the account most."""
    return min(rows, key=lambda row: (SEVERITY.index(row['pace']['state']), -row['used_percent']), default=None)


def credits():
    try:
        value = json.loads((state_root() / 'quota/credits.json').read_text())
    except (OSError, ValueError):
        return {}
    result = {}
    for key, entry in value.items() if isinstance(value, dict) else []:
        parts = key.split('/')
        if isinstance(entry, dict) and len(parts) in (2, 3):
            result[tuple(parts) if len(parts) == 3 else (*parts, telemetry_host())] = entry
    return result


def change_credits(provider_name, label, delta):
    if not LABEL.fullmatch(provider_name) or not LABEL.fullmatch(label):
        raise ValueError('provider and account must be short lowercase labels')
    path = state_root() / 'quota/credits.json'
    with locked(path.with_suffix('.lock')):
        current = {'/'.join(key): entry for key, entry in credits().items()}
        key = f'{provider_name}/{label}/{telemetry_host()}'
        count = int((current.get(key) or {}).get('reset_credits') or 0) + delta
        if count < 0:
            raise ValueError(f'{key} has no banked reset credit to use')
        current[key] = {'reset_credits': count, 'updated_at': iso(utcnow())}
        atomic_json(path, current)
        return count


def credit_advice(rows, banked, now):
    """Advice only: Provenant never redeems a reset itself."""
    count = int(number((banked or {}).get('reset_credits')) or 0)
    if not count:
        return {'action': 'none', 'reset_credits': 0}
    tightest = max(rows, key=lambda row: (row['used_percent'], row['window_minutes'] or 0), default=None)
    advice = {'reset_credits': count, 'source': (banked or {}).get('source', 'manual'),
              'window': tightest and tightest['name']}
    reset = parse_time(tightest and tightest['reset_at'])
    if tightest is None or tightest['used_percent'] < NEAR_EXHAUSTED:
        return {**advice, 'action': 'hold', 'reason': 'redeem a reset only at or near exhaustion'}
    wait = number(settings().get('credit_wait_hours')) or WAIT_HOURS
    left = (reset - now).total_seconds() if reset else None
    if left is not None and left <= wait * 3600:
        return {**advice, 'action': 'wait', 'reason': f'the natural reset is in {duration(left)}'}
    return {**advice, 'action': 'redeem',
            'reason': 'the natural reset is ' + (f'{duration(left)} away' if left else 'unreported') +
                      '; prefer a reset that keeps the reset date (see quota_events)'}


def freshest(values):
    """One snapshot per (provider, account): the most recent reading from any host."""
    best = {}
    for value in values:
        key = (value.get('provider'), value.get('account'))
        if key not in best or (value.get('observed_at') or '') > (best[key].get('observed_at') or ''):
            best[key] = value
    return [best[key] for key in sorted(best, key=lambda key: tuple(str(part) for part in key))]


def describe(value, banked, now):
    rows = [{**row, 'pace': pace(row, now)} for row in value.get('windows') or []]
    worst = governing(rows)
    unreported = list(value.get('unreported') or [])
    dated = [row for row in rows if row.get('reset_at') and row.get('window_minutes')]
    longest = max(dated, key=lambda row: row['window_minutes'], default=None)
    return {key: value.get(key) for key in ('provider', 'account', 'plan', 'host', 'active', 'priority',
                                            'observed_at', 'attempted_at', 'status', 'error', 'source')} | {
        'state': worst['pace']['state'] if worst else 'unreported',
        'summary': f"{worst['name']}: {worst['pace']['summary']}" if worst else value.get('error') or 'unreported',
        'windows': rows, 'unreported': unreported,
        'coverage_note': f"{', '.join(unreported)} not reported by {value.get('provider')}; missing does not mean 0%"
                         if unreported else '',
        'credits': value.get('credits') or banked, 'credit_advice': credit_advice(rows, value.get('credits') or banked, now),
        'use_it_or_lose_it': {'window': longest['name'], 'reset_at': longest['reset_at'],
                              'unused_percent': longest['pace']['headroom_percent'],
                              'expiring_headroom_percent': longest['pace']['expiring_headroom_percent']}
                             if longest else None}


def report(values, now=None):
    now = now or utcnow()
    banked = credits()
    rows = [describe(value, banked.get((value.get('provider'), value.get('account'), telemetry_host())), now)
            for value in freshest(values)]
    preference = {}
    for row in rows:
        preference.setdefault(row['provider'], []).append(row)

    def order(row):
        # Spend the account whose longest window resets soonest first, so its headroom is not lost.
        longest = max(row['windows'], key=lambda window: window['window_minutes'] or 0, default=None)
        return (row['state'] == 'exhausted', row['priority'] if row['priority'] is not None else 100,
                (longest or {}).get('reset_at') or '9999', row['account'])
    return {'schema': REPORT_SCHEMA, 'generated_at': iso(now), 'host': telemetry_host(), 'enabled': enabled(),
            'providers': registered(), 'accounts': rows,
            'placement_preference': {name: [row['account'] for row in sorted(group, key=order)]
                                     for name, group in sorted(preference.items())}}


def gather(now=None, force=False):
    now = now or utcnow()
    return [refresh(account, now, force) for account in accounts()]


# Routing input -------------------------------------------------------------------------------

def route_states(now=None):
    """Pace state per provider from the cached snapshot of its active account. Never fetches."""
    now = now or utcnow()
    if not enabled():
        return {}
    try:
        current = {(account.provider, account.label): account for account in accounts()}
    except ValueError:
        return {}
    states = {}
    for value in freshest(snapshots()):
        name, label = value.get('provider'), value.get('account')
        account = current.get((name, label))
        if not account or not account.active or value.get('status') not in ('ok', 'stale'):
            continue
        module = provider(name)
        row = describe(value, None, now)
        exhausted = [window['reset_at'] for window in row['windows'] if window['pace']['state'] == 'exhausted']
        states[name] = {'adapter': getattr(module, 'ADAPTER', None), 'account': label, 'state': row['state'],
                        'summary': row['summary'], 'observed_at': value.get('observed_at'),
                        'cooling_until': max(exhausted) if exhausted and all(exhausted) else None}
    return states


def apply_to_cooldowns(records, now=None):
    """Add cooldowns for exhausted providers; drop stale usage cooldowns where the provider has headroom."""
    now = now or utcnow()
    try:
        states = route_states(now)
    except Exception:  # Routing must never fail on quota.
        return records
    result = dict(records)
    for state in states.values():
        adapter = state['adapter']
        if not adapter:
            continue
        observed = parse_time(state['observed_at'])
        until = parse_time(state['cooling_until'])
        if observed and state['state'] in ('exhausted', 'reserve', 'on_pace', 'not_started'):
            for key, record in records.items():
                recorded = parse_time((record or {}).get('recorded_at'))
                if (str(key).split('/', 1)[0] == adapter and record.get('status') in ('usage_limited', 'quota_exhausted')
                        and recorded and observed > recorded):
                    result.pop(key, None)
        if state['state'] == 'exhausted' and until and until > now:
            key = f'{adapter}/*'
            if str((result.get(key) or {}).get('cooling_until', '')) < state['cooling_until']:
                result[key] = {'adapter': adapter, 'model': '*', 'account': state['account'],
                               'status': 'quota_exhausted', 'cooling_until': state['cooling_until'],
                               'recorded_at': state['observed_at'], 'source': 'provider quota'}
    return result


# Command line --------------------------------------------------------------------------------

def render(value):
    table = _telemetry().table
    lines = [f"Provider quota on {value['host']}" + ('' if value['enabled'] else ' (fetching disabled)')]
    rows = []
    for row in value['accounts']:
        head = (row['provider'], row['account'] + ('*' if row['active'] else ''), row['plan'] or '-')
        for window in row['windows']:
            rows.append((*head, window['name'], f"{window['used_percent']:g}%", window_length(window['window_minutes']),
                         (window['reset_at'] or '-')[:16].replace('T', ' '), window['pace']['state'], window['pace']['summary']))
        for name in row['unreported']:
            rows.append((*head, name, '-', '-', '-', 'unreported', 'missing does not mean 0%'))
        if not row['windows'] and not row['unreported']:
            rows.append((*head, '-', '-', '-', '-', row['status'], row['error'] or ''))
    lines += table(('provider', 'account', 'plan', 'window', 'used', 'length', 'resets', 'pace', 'detail'), rows)
    for row in value['accounts']:
        advice = row['credit_advice']
        if advice['action'] != 'none':
            lines.append(f"{row['provider']}/{row['account']}: {advice['reset_credits']} reset credit(s); "
                         f"{advice['action']}: {advice['reason']}")
    lines.append('* active account (the CLI default identity); routing reads only cached readings')
    print('\n'.join(lines))


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv[:2] == ['credit', 'list']:
        parser = argparse.ArgumentParser(prog='provenant quota credit list')
        parser.add_argument('--json', action='store_true')
        args = parser.parse_args(argv[2:])
        rows = [dict(provider=name, account=account, host=host, **entry)
                for (name, account, host), entry in sorted(credits().items()) if host == telemetry_host()]
        if args.json:
            print(json.dumps(rows, indent=2))
        else:
            for row in rows:
                print(f"{row['provider']}/{row['account']}@{row['host']}: {row['reset_credits']} reset credit(s)")
        return 0
    if argv[:1] == ['credit']:
        parser = argparse.ArgumentParser(prog='provenant quota credit',
                                         description='Record a banked reset credit by hand. Advice only.')
        parser.add_argument('provider')
        parser.add_argument('--account', default='default')
        parser.add_argument('action', choices=('add', 'use'))
        args = parser.parse_args(argv[1:])
        try:
            count = change_credits(args.provider, args.account, 1 if args.action == 'add' else -1)
        except (OSError, ValueError) as exc:
            print(f'quota: {exc}', file=sys.stderr)
            return 1
        print(f'{args.provider}/{args.account}: {count} reset credit(s)')
        return 0
    parser = argparse.ArgumentParser(prog='provenant quota', description=__doc__.split('\n\n')[0])
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--refresh', action='store_true', help='fetch now, ignoring the cache period')
    args = parser.parse_args(argv)
    try:
        value = report(gather(force=args.refresh))
    except (OSError, ValueError) as exc:
        print(f'quota: {exc}', file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(value, indent=2))
    else:
        render(value)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
