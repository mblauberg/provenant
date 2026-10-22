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
import hashlib
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
    'opencode': HERE / 'quota_providers/opencode.py',
}
CACHE_SECONDS = 300
HTTP_TIMEOUT = 8.0
PROCESS_TIMEOUT = 10.0
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
PACE_TOLERANCE = 4      # percentage points either side of even use
EARLY_SHARE = 0.03      # too little of a window has passed to judge its pace
CLOCK_SKEW_SECONDS = 300
RESET_NOISE = 5         # a smaller drop is reporting noise, not a granted reset
NEAR_EXHAUSTED = 95
WAIT_HOURS = 6
# How routing treats an account whose allowance is spent but whose credits keep it usable (`quota.credits_policy`).
CREDITS_POLICIES = ('last_resort', 'normal', 'never')
UNKNOWN_RESET_SECONDS = 3600  # How long a refused allowance cools when no reset time is known.
# Providers name some windows by codename; `quota.window_labels` in telemetry.json adds or overrides labels.
WINDOW_LABELS = {'iguana_necktie': 'Cloud session credits'}
SEVERITY = ('exhausted', 'unknown', 'unreported', 'deficit', 'on_pace', 'reserve', 'early', 'not_started')
_identity_spec = importlib.util.spec_from_file_location('provider_identity', HERE / 'provider_identity.py')
identity = sys.modules.get('provider_identity')
if identity is None:
    identity = importlib.util.module_from_spec(_identity_spec)
    sys.modules['provider_identity'] = identity
    _identity_spec.loader.exec_module(identity)
LABEL = identity.LABEL
DISABLED = {'0', 'false', 'no', 'off'}

PIPE, DEVNULL, TimeoutExpired = subprocess.PIPE, subprocess.DEVNULL, subprocess.TimeoutExpired
run_process = subprocess.run
open_process = subprocess.Popen
executable = shutil.which


class Unavailable(Exception):
    """A provider could not report quota. The message is fixed text, never a credential."""


# A non-secret label, the CLI identity directory, and whether that is the CLI's default.
class Account(namedtuple('Account', 'provider label identity_dir active plan priority options')):
    __slots__ = ()

    def __new__(cls, provider, label, identity_dir, active=False, plan=None, priority=None, options=None):
        return super().__new__(cls, provider, label, identity_dir, active, plan, priority,
                               options if options is not None else {})


class _Io:
    """Explicit provider I/O; functions resolve at call time for boundary replacement."""

    Unavailable = Unavailable
    TimeoutExpired = TimeoutExpired
    PROCESS_TIMEOUT = PROCESS_TIMEOUT
    MAX_RESPONSE_BYTES = MAX_RESPONSE_BYTES
    PIPE = PIPE
    DEVNULL = DEVNULL

    @staticmethod
    def executable(name):
        return executable(name)

    @staticmethod
    def run_process(*args, **kwargs):
        return run_process(*args, **kwargs)

    @staticmethod
    def open_process(*args, **kwargs):
        return open_process(*args, **kwargs)

    @staticmethod
    def monotonic():
        return monotonic()

    @staticmethod
    def parse_time(value):
        return parse_time(value)

    @staticmethod
    def http_json(url, headers):
        return http_json(url, headers)

    @staticmethod
    def ledger_windows(adapter, now):
        return ledger_windows(adapter, now)


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
    return identity.host_id()


def state_root():
    return _telemetry().state_root()


config = identity.config


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


same_path = identity.same_path
configured = identity.configured


def accounts():
    """Configured accounts; a provider with no configuration gets its local default, if any."""
    listed = configured()
    found = []
    for name in sorted(set(registered()) | set(listed)):
        module = provider(name)
        default = module.default_identity() if module else identity.selected_identity(name)
        if name not in listed:
            if default is not None and Path(default).exists():
                found.append(Account(name, identity.account_key(name, default, entries={}), Path(default), True))
            continue
        active_label = identity.account_key(name, default, listed[name])
        configured_default = identity.default_identity(name)
        for label, entry in sorted(listed[name].items()):
            entry = entry if isinstance(entry, dict) else {}
            if entry.get('enabled') is False:
                continue
            if not entry.get('identity_dir') and configured_default is None:
                raise ValueError(f'{name}/{label} requires identity_dir')
            identity_dir = Path(entry.get('identity_dir') or configured_default).expanduser()
            priority = number(entry.get('priority'))
            found.append(Account(name, label, identity_dir, default is not None and label == active_label,
                                 str(entry['plan']) if entry.get('plan') else None,
                                 int(priority) if priority is not None else None,
                                 {key: value for key, value in entry.items()
                                  if key not in ('identity_dir', 'enabled', 'plan', 'priority')}))
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
            'reset_at': iso(reset), 'window_start': iso(start), **window_amounts(raw),
            **({'advisory': True} if raw.get('advisory') is True else {})}


def _code(value, pattern):
    return value if isinstance(value, str) and re.fullmatch(pattern, value) else None


def window_amounts(raw):
    """The amounts behind a budget window, such as dollars of cloud session credit, when reported."""
    used, limit = number(raw.get('used_amount')), number(raw.get('limit_amount'))
    if used is None or limit is None or limit <= 0:
        return {}
    return {'used_amount': round(used, 2), 'limit_amount': round(limit, 2),
            'currency': _code(raw.get('currency'), r'[A-Z]{3}')}


def normalise_usage_credits(raw):
    """Credit balances the provider reports; a zero, missing or malformed balance is left out."""
    rows = []
    for entry in raw if isinstance(raw, list) else []:
        if not isinstance(entry, dict):
            continue
        balance, unlimited = number(entry.get('balance')), entry.get('unlimited') is True
        if not unlimited and (balance is None or balance <= 0):
            continue
        rows.append({'kind': _code(entry.get('kind'), r'[a-z][a-z0-9_]{0,31}') or 'credits',
                     'balance': None if unlimited else round(balance, 2), 'unlimited': unlimited,
                     'currency': _code(entry.get('currency'), r'[A-Z]{3}'),
                     'unit': _code(entry.get('unit'), r'[a-z][a-z_]{0,15}'),
                     'expires_at': iso(parse_time(entry.get('expires_at'))), 'source': 'provider'})
    return rows or None


def normalise_allowance(raw):
    """Whether the provider still allows ordinary included use, and whether a spend control or depleted
    credits stop credit use; each is unknown when unreported."""
    raw = raw if isinstance(raw, dict) else {}
    value = {key: raw[key] for key in ('ordinary_usage_allowed', 'spend_control_reached')
             if isinstance(raw.get(key), bool)}
    reached = _code(raw.get('limit_reached_type'), r'[a-z][a-z_]{0,63}')
    return {**value, **({'limit_reached_type': reached} if reached else {})} or None


def credit_anchor(previous, credits, observed):
    """Where the current run of falling balance began, so a burn rate spans the whole run."""
    balance = credits[0]['balance'] if credits else None
    if balance is None:
        return None
    last = normalise_usage_credits((previous or {}).get('usage_credits'))
    before, anchor = last[0]['balance'] if last else None, (previous or {}).get('credit_anchor') or {}
    if before is None or balance >= before:
        return {'balance': balance, 'observed_at': observed}
    if (number(anchor.get('balance')) or 0) >= before and parse_time(anchor.get('observed_at')):
        return anchor
    return {'balance': before, 'observed_at': previous.get('observed_at')}


def normalise_resets(raw):
    """Banked resets available now; zero or an unreported count is left out, never shown as zero."""
    if not isinstance(raw, dict) or type(raw.get('available')) is not int or raw['available'] <= 0:
        return None
    expiries = raw.get('expires_at') if isinstance(raw.get('expires_at'), list) else []
    return {'available': raw['available'], 'expires_at': sorted(filter(None, map(iso, map(parse_time, expiries)))),
            'source': raw.get('source') if raw.get('source') in ('manual', 'provider') else 'provider'}


def cache_path(provider_name, label, host=None, identity_dir=None):
    host = host or telemetry_host()
    identity = ''
    if identity_dir is not None:
        resolved = str(Path(identity_dir).expanduser().resolve())
        identity = '--' + hashlib.sha256(resolved.encode()).hexdigest()[:16]
    return state_root() / 'quota' / f'{provider_name}--{label}--{host}{identity}.json'


def load_snapshot(provider_name, label, identity_dir=None):
    try:
        value = json.loads(cache_path(provider_name, label, identity_dir=identity_dir).read_text())
    except (OSError, ValueError):
        # Unbound legacy snapshots cannot establish which identity supplied them.
        if identity_dir is not None:
            return None
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


def save_snapshot(value, identity_dir=None):
    atomic_json(cache_path(value['provider'], value['account'], value['host'], identity_dir), value)


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
    minutes = number(current.get('window_minutes')) or number(previous.get('window_minutes')) or 0
    start = after - timedelta(minutes=minutes) if after and minutes else None
    kind = None
    if after is None:
        kind = 'not_started' if before is not None or drop > RESET_NOISE else None
    elif before is None:
        kind = 'window_started'
    elif start and after > before and abs((start - before).total_seconds()) <= CLOCK_SKEW_SECONDS:
        kind = 'natural'
    else:
        moved = abs((after - before).total_seconds()) > CLOCK_SKEW_SECONDS
        if drop > RESET_NOISE:
            restarted = start and observed and abs((start - observed).total_seconds()) <= CLOCK_SKEW_SECONDS
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
    with locked(cache_path(account.provider, account.label, identity_dir=account.identity_dir).with_suffix('.lock')):
        return _refresh(account, now, force)


def _refresh(account, now=None, force=False):
    now = now or utcnow()
    if not enabled():
        previous = load_snapshot(account.provider, account.label, account.identity_dir) or blank(account, 'disabled')
        return {**previous, 'active': account.active, 'priority': account.priority,
                'plan': account.plan or previous.get('plan')}
    if provider(account.provider) is None:
        return blank(account, 'unreported', f'no quota source is registered for {account.provider}')
    previous = load_snapshot(account.provider, account.label, account.identity_dir)
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
        observed = result.get('observed_at') or iso(now)
        balance = normalise_usage_credits(result.get('usage_credits'))
        extras = {'usage_credits': balance, 'resets': normalise_resets(result.get('resets')),
                  'allowance': normalise_allowance(result.get('allowance')),
                  'credit_anchor': credit_anchor(previous, balance, observed)}
        value = {**blank(account, 'ok' if windows else 'unreported', now=now),
                 'plan': str(plan) if plan else None, 'observed_at': observed,
                 'source': result.get('source'), 'windows': windows, 'unreported': unreported,
                 'credits': result.get('credits') if isinstance(result.get('credits'), dict) else None,
                 **{key: extra for key, extra in extras.items() if extra}}
        record_events(previous, value)
        save_snapshot(value, account.identity_dir)
        return value
    value = {**(previous or blank(account, 'unavailable')), 'attempted_at': iso(now), 'error': error,
             'status': 'stale' if previous and previous.get('windows') else 'unavailable',
             'active': account.active, 'priority': account.priority}
    save_snapshot(value, account.identity_dir)
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
    if actual >= 100:
        return {**result, 'state': 'exhausted', 'will_last_to_reset': False, 'runs_out_at': iso(now),
                'summary': 'exhausted' + (f'; resets in {duration(remaining)}' if remaining and remaining > 0 else '')}
    if actual == 0 and (reset is None or remaining <= 0 or
                        minutes and remaining > minutes * 60 + CLOCK_SKEW_SECONDS):
        return {**result, 'state': 'not_started', 'summary': 'window starts on first use'}
    if reset is None:
        return {**result, 'summary': 'reset time unreported; pace unknown'}
    if remaining <= 0:
        return {**result, 'summary': f'reset passed at {iso(reset)}; awaiting a fresh reading'}
    if not minutes:
        return {**result, 'summary': f'resets in {duration(remaining)}; window length unreported'}
    if remaining > minutes * 60 + CLOCK_SKEW_SECONDS:
        return {**result, 'summary': 'reset time exceeds the reported window; pace unknown'}
    remaining = min(remaining, minutes * 60)
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


def sets_state(row):
    """An untimed budget, such as cloud session credits, or a window its provider marks advisory is shown but
    never sets the account's state: its pace is always unknown."""
    return not row.get('advisory') and not (row.get('limit_amount') and not row.get('window_minutes'))


def governing(rows):
    """The window that constrains the account most."""
    return min(filter(sets_state, rows), key=lambda row: (SEVERITY.index(row['pace']['state']), -row['used_percent']),
               default=None)


def credits(*, raw=False):
    try:
        value = json.loads((state_root() / 'quota/credits.json').read_text())
    except (OSError, ValueError):
        return {}
    result = {}
    for key, entry in value.items() if isinstance(value, dict) else []:
        parts = key.split('/')
        if isinstance(entry, dict) and len(parts) in (2, 3):
            if raw:
                result[key] = entry
            elif len(parts) == 3:
                result[tuple(parts)] = entry
            else:
                result.setdefault((*parts, telemetry_host()), entry)
    return result


def change_credits(provider_name, label, delta):
    if not LABEL.fullmatch(provider_name) or not LABEL.fullmatch(label):
        raise ValueError('provider and account must be short lowercase labels')
    path = state_root() / 'quota/credits.json'
    with locked(path.with_suffix('.lock')):
        current = credits(raw=True)
        key = f'{provider_name}/{label}/{telemetry_host()}'
        generic = f'{provider_name}/{label}'
        if key not in current and generic in current:
            key = generic
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


def window_labels():
    configured = settings().get('window_labels')
    configured = configured if isinstance(configured, dict) else {}
    return {**WINDOW_LABELS, **{str(key): value for key, value in configured.items() if isinstance(value, str)}}


def credits_policy():
    value = settings().get('credits_policy')
    return value if value in CREDITS_POLICIES else CREDITS_POLICIES[0]


def allowance_spent(value, worst):
    """Whether the plan allowance is gone: a window is exhausted, or the provider refuses ordinary use."""
    allowance = value.get('allowance') or {}
    return bool(worst) and (worst['pace']['state'] == 'exhausted' or allowance.get('ordinary_usage_allowed') is False)


def credit_draw(value, spent):
    """The credit that keeps an account usable once its allowance is spent, or None.

    Only the latest reading's balance counts, and only above `quota.credit_floor`; a spend control or
    depleted credits stop it. The `never` policy leaves such an account exhausted."""
    allowance = value.get('allowance') or {}
    if (not spent or credits_policy() == 'never' or allowance.get('spend_control_reached') is True
            or 'credits_depleted' in allowance.get('limit_reached_type', '')):
        return None
    floor = number(settings().get('credit_floor')) or 0
    return next((entry for entry in normalise_usage_credits(value.get('usage_credits')) or []
                 if entry['unlimited'] or entry['balance'] > floor), None)


def burn_rate(value, entry):
    """Credits spent per hour since the balance began falling, from successive readings."""
    anchor = value.get('credit_anchor') or {}
    start, end = parse_time(anchor.get('observed_at')), parse_time(value.get('observed_at'))
    spent = (number(anchor.get('balance')) or 0) - (entry['balance'] or 0)
    hours = (end - start).total_seconds() / 3600 if start and end else 0
    return round(spent / hours, 2) if spent > 0 and hours > 0 else None


def summary(value, worst, spent, drawing, rate):
    if not worst:
        return value.get('error') or 'unreported'
    if spent and not drawing and worst['pace']['state'] != 'exhausted':
        return f"{worst['name']}: allowance spent; {value.get('provider')} refuses ordinary use"
    if not drawing:
        return f"{worst['name']}: {worst['pace']['summary']}"
    return (f"{worst['name']}: allowance spent; drawing on credits, {credit_text(drawing)}"
            + (f', about {rate:,.0f}/h' if rate else ''))


def describe(value, banked, now):
    labels = window_labels()
    rows = [{**row, 'pace': pace(row, now), **({'label': labels[row['name']]} if labels.get(row['name']) else {})}
            for row in value.get('windows') or []]
    worst = governing(rows)
    spent = allowance_spent(value, worst)
    drawing = credit_draw(value, spent)
    rate = burn_rate(value, drawing) if drawing else None
    unreported = list(value.get('unreported') or [])
    dated = [row for row in rows if row.get('reset_at') and row.get('window_minutes')]
    longest = max(dated, key=lambda row: row['window_minutes'], default=None)
    return {key: value.get(key) for key in ('provider', 'account', 'plan', 'host', 'active', 'priority',
                                            'observed_at', 'attempted_at', 'status', 'error', 'source')} | {
        'state': 'credit_only' if drawing else 'exhausted' if spent else worst['pace']['state'] if worst else 'unreported',
        'summary': summary(value, worst, spent, drawing, rate),
        'windows': rows, 'unreported': unreported,
        'coverage_note': f"{', '.join(unreported)} not reported by {value.get('provider')}; missing does not mean 0%"
                         if unreported else '',
        'credits': value.get('credits') or banked, 'credit_advice': credit_advice(rows, value.get('credits') or banked, now),
        'use_it_or_lose_it': {'window': longest['name'], 'reset_at': longest['reset_at'],
                              'unused_percent': longest['pace']['headroom_percent'],
                              'expiring_headroom_percent': longest['pace']['expiring_headroom_percent']}
                             if longest else None} | optional_credits(value, banked) | (
        {'drawing_credits': True, 'credit_burn_per_hour': rate} if drawing else {})


def optional_credits(value, banked):
    """Usage credits and banked resets, present only when an account has some."""
    counted = value.get('credits') or banked or {}
    resets = (normalise_resets(value.get('resets')) or
              normalise_resets({'available': counted.get('reset_credits'), 'source': counted.get('source', 'manual')}))
    extras = {'usage_credits': normalise_usage_credits(value.get('usage_credits')), 'resets': resets}
    return {key: extra for key, extra in extras.items() if extra}


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
        spare = row['state'] == 'credit_only' and credits_policy() == 'last_resort'
        return (row['state'] == 'exhausted', spare, row['priority'] if row['priority'] is not None else 100,
                (longest or {}).get('reset_at') or '9999', row['account'])
    return {'schema': REPORT_SCHEMA, 'generated_at': iso(now), 'host': telemetry_host(), 'enabled': enabled(),
            'providers': registered(), 'accounts': rows,
            'placement_preference': {name: [row['account'] for row in sorted(group, key=order)]
                                     for name, group in sorted(preference.items())}}


def gather(now=None, force=False):
    now = now or utcnow()
    return [refresh(account, now, force) for account in accounts()]


# Routing input -------------------------------------------------------------------------------

def refused_until(worst, value, now):
    """When a refused allowance may return: the governing window's reset, else a bounded wait.

    The provider refuses ordinary use below 100%. With no future reset known, the account cools for
    `quota.unknown_reset_cooldown_seconds` (default an hour) from the reading, then the next reading decides."""
    reset = parse_time(worst.get('reset_at'))
    if reset and reset > now:
        return worst['reset_at']
    seconds = number(settings().get('unknown_reset_cooldown_seconds'))
    observed = parse_time(value.get('observed_at')) or now
    return iso(observed + timedelta(seconds=seconds if seconds and seconds > 0 else UNKNOWN_RESET_SECONDS))


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
    for (name, label), account in current.items():
        value = load_snapshot(name, label, account.identity_dir)
        if not account.active or not value or value.get('status') not in ('ok', 'stale'):
            continue
        module = provider(name)
        row = describe(value, None, now)
        exhausted = [window['reset_at'] for window in row['windows']
                     if window['pace']['state'] == 'exhausted' and sets_state(window)]
        if row['state'] == 'exhausted' and not exhausted:
            exhausted = [refused_until(governing(row['windows']), value, now)]
        states[name] = {'adapter': getattr(module, 'ADAPTER', None), 'account': label, 'host': telemetry_host(), 'state': row['state'],
                        'summary': row['summary'], 'observed_at': value.get('observed_at'),
                        'cooling_until': max(exhausted) if exhausted and all(exhausted) else None,
                        # Pools pick a last-resort adapter only when nothing with allowance qualifies.
                        **({'last_resort': True} if row['state'] == 'credit_only' and
                           credits_policy() == 'last_resort' else {})}
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
        # An account drawing on credits adds no cooldown and clears only the ones quota set. A real
        # refusal stays until it expires or a reading shows allowance again: credits may be switched off.
        drawing = state['state'] == 'credit_only'
        if observed and (drawing or state['state'] in ('reserve', 'on_pace', 'early', 'not_started')):
            for key, record in records.items():
                recorded = parse_time((record or {}).get('recorded_at'))
                if (identity.matches(record, state) and str(key).split('/', 1)[0] == adapter and record.get('status') in ('usage_limited', 'quota_exhausted')
                        and recorded and observed > recorded and (not drawing or record.get('source') == 'provider quota')):
                    result.pop(key, None)
        if state['state'] == 'exhausted' and until and until > now:
            key = f'{adapter}/*'
            record = result.get(key) or {}
            recorded = parse_time(record.get('recorded_at'))
            if (str(record.get('cooling_until', '')) < state['cooling_until'] or
                    record.get('status') in ('usage_limited', 'quota_exhausted') and
                    observed and recorded and observed > recorded):
                result[key] = {'adapter': adapter, 'model': '*', 'account': state['account'], 'host': state['host'],
                               'status': 'quota_exhausted', 'cooling_until': state['cooling_until'],
                               'recorded_at': state['observed_at'], 'source': 'provider quota'}
    return result


# Command line --------------------------------------------------------------------------------

def day(stamp):
    moment = parse_time(stamp).astimezone()
    return f'{moment.day} {moment:%b}'


def amount(value, currency):
    return f'${value:,.0f}' if currency == 'USD' else f'{value:,.0f}' + (f' {currency}' if currency else '')


def used_text(window):
    """`$136 of $250` for a budget window, otherwise the reported percentage."""
    if window.get('limit_amount'):
        currency = window.get('currency')
        return f"{amount(window['used_amount'], currency)} of {amount(window['limit_amount'], currency)}"
    return f"{window['used_percent']:g}%"


def credit_text(entry):
    text = 'unlimited credits' if entry['unlimited'] else \
        f"{amount(entry['balance'], entry['currency'])} credit" if entry['currency'] else \
        f"{entry['balance']:,.0f} {entry['unit'] or 'credits'}"
    return text + ' available' + (f" (expires {day(entry['expires_at'])})" if entry['expires_at'] else '')


def reset_text(resets):
    count, expiries = resets['available'], resets['expires_at']
    lead = 'next expires' if len(expiries) > 1 else 'expires'
    return f"{count} banked reset{'s' if count != 1 else ''}" + (f' ({lead} {day(expiries[0])})' if expiries else '')


def repeats(window, windows):
    """A `limits` entry (`session:1`) that restates a named window (`five_hour`): same length, use and reset."""
    def stamp(row):
        return parse_time(row['reset_at'])
    return ':' in window['name'] and any(
        ':' not in other['name'] and other['window_minutes'] == window['window_minutes'] and
        other['used_percent'] == window['used_percent'] and stamp(other) and stamp(window) and
        abs((stamp(other) - stamp(window)).total_seconds()) <= 60 for other in windows)


def render(value, verbose=False):
    table = _telemetry().table
    lines = [f"Provider quota on {value['host']}" + ('' if value['enabled'] else ' (fetching disabled)')]
    rows = []
    for row in value['accounts']:
        head = (row['provider'], row['account'] + ('*' if row['active'] else ''), row['plan'] or '-')
        for window in row['windows']:
            if repeats(window, row['windows']):
                continue
            pace_state, detail = window['pace']['state'], window['pace']['summary']
            if window.get('limit_amount') and not window['window_minutes']:  # a dollar budget has no pace
                pace_state = '-'
                detail = f"{amount(window['limit_amount'] - window['used_amount'], window.get('currency'))} left; " + \
                    detail.removesuffix('; window length unreported')
            rows.append((*head, window.get('label') or window['name'], used_text(window),
                         window_length(window['window_minutes']),
                         (window['reset_at'] or '-')[:16].replace('T', ' '), pace_state, detail))
        if not row['windows']:
            rows.append((*head, '-', '-', '-', '-', row['status'], row['error'] or ''))
    lines += table(('provider', 'account', 'plan', 'window', 'used', 'length', 'resets', 'pace', 'detail'), rows)
    # A plan lacks many optional windows, so absence alone is not news: list them only on request.
    for row in value['accounts'] if verbose else []:
        if row['unreported']:
            lines.append(f"{row['provider']}/{row['account']}: {len(row['unreported'])} window(s) not reported "
                         "(missing does not mean 0%)")
    # Credits and banked resets appear only for an account that has some.
    for row in value['accounts']:
        parts = [credit_text(entry) for entry in row.get('usage_credits') or []]
        parts += [reset_text(row['resets'])] if row.get('resets') else []
        advice = row['credit_advice']
        parts += [f"{advice['action']}: {advice['reason']}"] if advice['action'] != 'none' else []
        if parts:
            lines.append(f"{row['provider']}/{row['account']}: " + '; '.join(parts))
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
    parser.add_argument('--verbose', action='store_true', help='also list windows the provider did not report')
    args = parser.parse_args(argv)
    try:
        value = report(gather(force=args.refresh))
    except (OSError, ValueError) as exc:
        print(f'quota: {exc}', file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(value, indent=2))
    else:
        render(value, args.verbose)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
