#!/usr/bin/env python3
"""Provider-reported quota per account: windows, pace, reset events, credits and routing input.

Accounts come from configuration; quota sources come from one module per provider under
`quota_providers/`. Nothing here knows a plan, a limit or a window length: every window,
limit and reset time comes from the provider's own response. Credentials are read
in-process only to call that provider's endpoint, and never reach the cache,
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



_OPENER = []


def urlopen(request, timeout):
    """Open without following redirects: authenticated quota requests stay at the provider's endpoint.

    urllib.request loads on first use, so a tick with nothing due never pays for it."""
    if not _OPENER:
        from urllib.request import HTTPRedirectHandler, build_opener

        class NoRedirect(HTTPRedirectHandler):
            def redirect_request(self, request, response, code, message, headers, target):
                return None
        _OPENER.append(build_opener(NoRedirect()).open)
    return _OPENER[0](request, timeout=timeout)


SCHEMA = 'provenant.quota.v1'
REPORT_SCHEMA = 'provenant.quota.report.v1'
HERE = Path(__file__).resolve().parent
# Adding a provider is one module exposing ADAPTER, default_identity() and
# fetch(account, io, now), plus one line here. A module whose one credential store
# serves several accounts may add account_label() to name the signed-in one locally, and
# fingerprinted(label) so accounts seen before are kept as signed out. A module whose
# windows fall into independent pools may add window_pool(name).
REGISTRY = {
    'agy': HERE / 'quota_providers/agy.py',
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
NEAR_EXHAUSTED = 95
WAIT_HOURS = 6
# How routing treats an account whose allowance is spent but whose credits keep it usable (`quota.credits_policy`).
CREDITS_POLICIES = ('last_resort', 'normal', 'never')
UNKNOWN_RESET_SECONDS = 3600  # How long a refused allowance cools when no reset time is known.
SEVERITY = ('exhausted', 'unknown', 'unreported', 'deficit', 'ahead_of_pace', 'on_pace', 'reserve', 'early', 'not_started')
_identity_spec = importlib.util.spec_from_file_location('provider_identity', HERE / 'provider_identity.py')
identity = sys.modules.get('provider_identity')
if identity is None:
    identity = importlib.util.module_from_spec(_identity_spec)
    sys.modules['provider_identity'] = identity
    _identity_spec.loader.exec_module(identity)
LABEL = identity.LABEL
DISABLED = {'0', 'false', 'no', 'off'}
schedule = sys.modules.get('quota_schedule')
if schedule is None:
    _schedule_spec = importlib.util.spec_from_file_location('quota_schedule', HERE / 'quota_schedule.py')
    schedule = importlib.util.module_from_spec(_schedule_spec)
    sys.modules['quota_schedule'] = schedule
    _schedule_spec.loader.exec_module(schedule)
quota_display = sys.modules.get('quota_display')
if quota_display is None:
    _display_spec = importlib.util.spec_from_file_location('quota_display', HERE / 'quota_display.py')
    quota_display = importlib.util.module_from_spec(_display_spec)
    sys.modules['quota_display'] = quota_display
    _display_spec.loader.exec_module(quota_display)
RETRY_AFTER_CAP = 86400
TRACE_RETENTION_DAYS = 30
TRACE_TEXT = 200
# Anything token-shaped is scrubbed from trace text: bearer values, addresses and long opaque runs.
SECRETISH = re.compile(r'(?i)bearer\s+\S+|[\w.+-]+@[\w-]+\.[\w.-]+|[A-Za-z0-9_\-+/=.]{24,}')

PIPE, DEVNULL, TimeoutExpired = subprocess.PIPE, subprocess.DEVNULL, subprocess.TimeoutExpired
run_process = subprocess.run
open_process = subprocess.Popen
executable = shutil.which


class Unavailable(Exception):
    """A provider could not report quota. The message is fixed text, never a credential.

    `kind` is one of quota_schedule.ERROR_KINDS and sets the backoff; `retry_after` is the
    provider's own wait in seconds, `retry_at` its deadline when it named a date, and `status`
    the HTTP status, when known."""

    def __init__(self, message, kind='other', retry_after=None, status=None, retry_at=None):
        super().__init__(message)
        self.kind = kind if kind in schedule.ERROR_KINDS else 'other'
        self.retry_after = retry_after
        self.status = status
        self.retry_at = retry_at


# A non-secret label, the CLI identity directory, and whether that is the CLI's default. A signed-out account
# is one this host read before through a credential store that is now signed in to another; it is never fetched.
class Account(namedtuple('Account', 'provider label identity_dir active plan priority options signed_out')):
    __slots__ = ()

    def __new__(cls, provider, label, identity_dir, active=False, plan=None, priority=None, options=None,
                signed_out=False):
        return super().__new__(cls, provider, label, identity_dir, active, plan, priority,
                               options if options is not None else {}, signed_out)


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
    def readonly(path):
        return sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True)

    @staticmethod
    def memo(name, account, seconds, produce):
        return memo(name, account, seconds, produce)


IO = _Io()
_LOADED = {}


# Time and numbers ---------------------------------------------------------------------------

def utcnow():
    return datetime.now(UTC)


iso = schedule.iso


number = schedule.number
parse_time = schedule.parse_time


def duration(seconds):
    seconds = max(0, int(seconds or 0))
    minutes, hours = seconds // 60, seconds // 3600
    if seconds < 3600:
        return f'{minutes}m'
    if hours < 48:
        return f'{hours}h' + (f' {minutes % 60}m' if minutes % 60 else '')
    return f'{hours // 24}d' + (f' {hours % 24}h' if hours % 24 else '')


# Configuration, registry and accounts --------------------------------------------------------

def telemetry_host():
    return identity.host_id()


def local_hosts():
    """This machine's current id, then the earlier ids its stores may hold (raw hostname, slug, 'local')."""
    return identity.host_aliases(telemetry_host())


def same_host(value):
    return identity.same_host(value, telemetry_host())


def state_root():
    return Path(os.environ.get('AGENT_FABRIC_STATE_ROOT', str(Path.home() / '.local/state/agent-harness/fabric')))


config = identity.config


def settings():
    value = config().get('quota')
    return value if isinstance(value, dict) else {}


def setting(key, default=None):
    """One `quota.<key>` setting, or `default` when it is not configured."""
    return settings().get(key, default)


def setting_map(key):
    """A `quota.<key>` setting that must be an object; anything else reads as empty."""
    value = setting(key)
    return value if isinstance(value, dict) else {}


def enabled():
    if os.environ.get('PROVENANT_QUOTA', '').strip().lower() in DISABLED:
        return False
    return setting('enabled', True) is not False


def cache_seconds():
    return max(CACHE_SECONDS, int(number(setting('cache_seconds')) or CACHE_SECONDS))


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


def accounts(provider_name=None):
    """Configured accounts; a provider with no configuration gets its local default, if any."""
    listed = configured()
    found = []
    for name in sorted({provider_name} if provider_name else set(registered()) | set(listed)):
        module = provider(name)
        default = module.default_identity() if module else identity.selected_identity(name)
        shared = getattr(module, 'fingerprinted', None)  # one credential store serves several accounts
        signed = getattr(module, 'account_label', lambda: None)() if module and (shared or name not in listed) else None
        if name not in listed:
            if default is not None and Path(default).exists():
                label = signed or identity.account_key(name, default, entries={})
                found.append(Account(name, label, Path(default), True))
                found += [Account(name, other, Path(default), signed_out=True)
                          for other in remembered(name, shared, Path(default)) if other != label]
            continue
        active_label = identity.account_key(name, default, listed[name])
        configured_default = identity.default_identity(name)
        enabled = {label: entry if isinstance(entry, dict) else {} for label, entry in listed[name].items()
                   if not isinstance(entry, dict) or entry.get('enabled') is not False}
        dirs = {}
        for label, entry in sorted(enabled.items()):
            if not entry.get('identity_dir') and configured_default is None:
                raise ValueError(f'{name}/{label} requires identity_dir')
            dirs[label] = Path(entry.get('identity_dir') or configured_default).expanduser()
        # On a shared store, an alias such as `work` names whichever account it first read: its fingerprint, from
        # the entry or the alias's last reading. A fingerprint label is its own.
        bound = {label: label if shared(label) else bound_fingerprint(name, label, dirs[label], entry, shared)
                 for label, entry in enabled.items()} if shared else {}
        claimed = signed in bound.values()
        for label, entry in sorted(enabled.items()):
            priority = number(entry.get('priority'))
            if shared:
                # Current only while its own account is signed in; an unbound alias takes an unclaimed sign-in.
                mine = bound[label] == signed if bound[label] else signed is None or not claimed
                claimed = claimed or (bound[label] is None and mine and signed is not None)
                away = not mine
                active = default is not None and mine and (signed is not None or label == active_label)
            else:
                away, active = False, default is not None and label == active_label
            # An entry's own `fingerprint` passes through as a deliberate binding; _refresh resolves the rest.
            options = {key: value for key, value in entry.items()
                       if key not in ('identity_dir', 'enabled', 'plan', 'priority', 'display_name')}
            found.append(Account(name, label, dirs[label], active, str(entry['plan']) if entry.get('plan') else None,
                                 int(priority) if priority is not None else None, options, away))
        # Configured entries only name accounts, so the signed-in one and those seen before still appear under
        # their fingerprints, unless an entry already stands for them.
        if shared and enabled and default is not None and Path(default).exists():
            taken = set(bound.values()) | ({signed} if claimed else set())
            found += [Account(name, label, Path(default), label == signed, signed_out=label != signed)
                      for label in sorted({signed} - {None} | set(remembered(name, shared, Path(default))))
                      if label not in listed[name] and label not in taken]
    return found


def bound_fingerprint(name, label, identity_dir, entry, shared):
    """The account an alias on a shared store stands for: `fingerprint` in its entry, else its last reading's."""
    for candidate in (entry.get('fingerprint'), (load_snapshot(name, label, identity_dir) or {}).get('fingerprint')):
        if isinstance(candidate, str) and shared(candidate):
            return candidate
    return None


def expected_fingerprint(account, previous):
    """The account a reading from a shared store must come from: the entry's `fingerprint`, which deliberately
    binds or rebinds an alias; a fingerprint label; else the alias's snapshot, reloaded under the refresh lock so
    two concurrent refreshes never both claim an unbound alias. None for an unbound alias or an unshared store."""
    shared = getattr(provider(account.provider), 'fingerprinted', None)
    if not shared:
        return None
    for candidate in (account.options.get('fingerprint'), account.label, (previous or {}).get('fingerprint')):
        if isinstance(candidate, str) and shared(candidate):
            return candidate
    return None


def remembered(name, shared, identity_dir, now=None):
    """Fingerprinted accounts this host has a snapshot for under one shared identity, such as agy's one Keychain
    item: signing in to another account leaves their last reading listed as signed out rather than forgotten.

    Fingerprints hash the account itself, never the host, so a later merge of peers' readings dedupes by label.
    With `quota.remembered.forget_after_days`, an account unseen that long is no longer listed."""
    if shared is None:
        return []
    template = cache_path(name, '*', identity_dir=identity_dir)
    prefix, suffix = template.name.split('*')
    days = number(setting_map('remembered').get('forget_after_days'))
    cutoff = (now or utcnow()) - timedelta(days=days) if days and days > 0 else None
    labels = []
    for path in template.parent.glob(template.name):
        label = path.name[len(prefix):-len(suffix)]
        if not shared(label):
            continue
        if cutoff is not None:
            try:
                seen = parse_time(json.loads(path.read_text()).get('observed_at'))
            except (OSError, ValueError, AttributeError):
                seen = None
            if seen is None or seen < cutoff:
                continue
        labels.append(label)
    return sorted(labels)


def forget(account_key):
    """Remove a signed-out account's remembered snapshot on this host. A signed-in account is never forgotten."""
    name, _, label = str(account_key).partition('/')
    match = next((account for account in accounts() if (account.provider, account.label) == (name, label)), None)
    if match is None:
        raise ValueError(f'{account_key} is not a known account on this host')
    if not match.signed_out:
        raise ValueError(f'{account_key} is signed in; only a signed-out account can be forgotten')
    path = cache_path(name, label, identity_dir=match.identity_dir)
    with locked(path.with_suffix('.lock')):
        path.unlink(missing_ok=True)
    path.with_suffix('.lock').unlink(missing_ok=True)
    named = isinstance((configured().get(name) or {}).get(label), dict)
    return (f'{account_key}: forgotten' +
            ('; it stays listed while telemetry.json names it' if named else ''))


# Provider I/O for quota modules -------------------------------------------------------------

def http_json(url, headers):
    from urllib.request import Request
    request = Request(url, headers=headers, method='GET')
    try:
        with urlopen(request, timeout=HTTP_TIMEOUT) as response:
            raw = response.read(MAX_RESPONSE_BYTES + 1)
    except HTTPError as exc:
        code = exc.code
        header = exc.headers.get('Retry-After') if exc.headers else None
        wait, deadline = retry_after(header), retry_deadline(header)
        exc.close()
        if code in (401, 403):
            raise Unavailable('provider authentication needs a refresh', 'auth', status=code) from None
        if code == 429:
            raise Unavailable('provider quota service is rate limited', 'rate_limited', wait, code, deadline) from None
        raise Unavailable(f'provider quota request failed (HTTP {code})',
                          'network' if code >= 500 else 'other', wait, code, deadline) from None
    except (URLError, TimeoutError, OSError):
        raise Unavailable('provider quota request could not connect', 'network') from None
    if len(raw) > MAX_RESPONSE_BYTES:
        raise Unavailable('provider quota response was too large')
    try:
        value = json.loads(raw.decode('utf-8'))
    except (UnicodeDecodeError, ValueError):
        raise Unavailable('provider returned an invalid quota response') from None
    if not isinstance(value, dict):
        raise Unavailable('provider returned an invalid quota response')
    return value


def retry_after(value, now=None):
    """Seconds from a Retry-After header (seconds or an HTTP date), capped at a day; None if unusable."""
    if not isinstance(value, str) or not value.strip():
        return None
    seconds = number(value.strip())
    if seconds is None:
        from email.utils import parsedate_to_datetime
        try:
            moment = parsedate_to_datetime(value.strip())
        except (TypeError, ValueError, IndexError):
            return None
        moment = moment if moment.tzinfo else moment.replace(tzinfo=UTC)
        seconds = (moment - (now or utcnow())).total_seconds()
    return round(min(RETRY_AFTER_CAP, max(0.0, seconds)), 1)


def retry_deadline(value):
    """The moment a Retry-After HTTP date names, kept absolute; None for a wait in seconds."""
    if not isinstance(value, str) or not value.strip() or number(value.strip()) is not None:
        return None
    from email.utils import parsedate_to_datetime
    try:
        moment = parsedate_to_datetime(value.strip())
    except (TypeError, ValueError, IndexError):
        return None
    return moment if moment.tzinfo else moment.replace(tzinfo=UTC)


def memo(name, account, seconds, produce):
    """A small non-secret value kept per identity until it is `seconds` old or the identity's activity
    changes. An empty value is never kept."""
    stamp = schedule.activity(account.provider, account.identity_dir)
    digest = hashlib.sha256(str(Path(account.identity_dir).expanduser()).encode()).hexdigest()[:16]
    path = state_root() / 'quota/memo' / f'{name}-{digest}.json'
    moment = utcnow().timestamp()
    try:
        kept = json.loads(path.read_text())
        if (isinstance(kept, dict) and kept.get('stamp') == stamp and
                0 <= moment - (number(kept.get('at')) or 0) < seconds):
            return kept['value']
    except (OSError, ValueError, KeyError):
        pass
    value = produce()
    if value:
        try:
            atomic_json(path, {'value': value, 'at': moment, 'stamp': stamp})
        except OSError:
            pass
    return value



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
            **({'advisory': True} if raw.get('advisory') is True else {}),
            **({'pool': raw['pool']} if _code(raw.get('pool'), r'[a-z0-9][a-z0-9._-]{0,31}') else {})}


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
    value = None
    # A snapshot written under an earlier host id is still this host's.
    for host in local_hosts():
        try:
            value = json.loads(cache_path(provider_name, label, host, identity_dir).read_text())
            break
        except (OSError, ValueError):
            continue
    if value is None:
        # Unbound legacy snapshots cannot establish which identity supplied them.
        if identity_dir is not None:
            return None
        # Read the previous two-part filename only when its host matches this host.
        try:
            value = json.loads((state_root() / 'quota' / f'{provider_name}--{label}.json').read_text())
        except (OSError, ValueError):
            return None
    if not (isinstance(value, dict) and value.get('schema') == SCHEMA
            and same_host(value.get('host')) == telemetry_host()):
        return None
    return {**value, 'host': telemetry_host()}



class ClaudeAccountsExhaustedError(ValueError):
    """Every candidate has a cached, still-binding usage limit."""


def claude_account_exhausted(value, now):
    """Known five-hour/weekly exhaustion binds even with errors, credits or advisory windows."""
    return any(row.get('name') in ('five_hour', 'seven_day')
               and number(row.get('used_percent')) is not None
               and number(row['used_percent']) >= 100 and not reset_passed(row, now)
               for row in (value or {}).get('windows', []))


def allowance_runway_covered(value, window, budget, now, banked=None):
    """Whether allowance and usable reserves cover the estimate through the natural reset."""
    if budget.get('will_last_to_reset') is True:
        return True
    reset, runs_out = parse_time(window.get('reset_at')), parse_time(budget.get('runs_out_at'))
    headroom = budget.get('headroom_percent') or 0
    if not reset or not runs_out or runs_out <= now or not headroom:
        return False
    counted = value.get('credits') or banked or {}
    resets = normalise_resets(value.get('resets')) or normalise_resets({'available': counted.get('reset_credits')})
    extra = (runs_out - now).total_seconds() * 100 / headroom * (resets['available'] if resets else 0)
    gap = (reset - runs_out).total_seconds() - extra
    if gap <= 0:
        return True
    credit = credit_draw(value, True)
    if not credit or (parse_time(credit.get('expires_at')) or reset) < reset:
        return False
    if credit['unlimited']:
        return True
    drawing = credit_draw(value, allowance_spent(value, {'pace': budget}))
    rate = burn_rate(value, credit) if drawing else None
    floor = number(setting('credit_floor')) or 0
    return bool(rate and (credit['balance'] - floor) / rate * 3600 >= gap)


def choose_claude_account(snapshots, now):
    """Prefer covered weekly runway, then unknown readings, then the smallest shortage.

    Exhausted accounts cannot launch. Among covered accounts, spend projected
    weekly waste first, then prefer Pro, then labels. Shortages sort by pace delta,
    then labels; unknown readings sort by labels.
    """
    ranked = []
    banked = credits()
    for label, value in snapshots.items():
        windows = {row.get('name'): row for row in (value or {}).get('windows', [])}
        short, weekly = windows.get('five_hour'), windows.get('seven_day')
        # A known limit still binds when the other window is missing or the last fetch failed.
        if claude_account_exhausted(value, now):
            continue
        if (not value or value.get('status') != 'ok' or not short or not weekly
                or any(number(row.get('used_percent')) is None for row in (short, weekly))):
            ranked.append(((1, 0, 0, label), label))
            continue
        weekly = {**weekly, 'used_percent': 0} if reset_passed(weekly, now) else weekly
        budget = pace(weekly, now)
        if budget['state'] in ('unknown', 'unreported'):
            score = (1, 0, 0, label)
        elif budget.get('will_last_to_reset') is False and not allowance_runway_covered(
                value, weekly, budget, now, banked.get(('claude', label, telemetry_host()))):
            score = (2, max(0, budget.get('delta_percent') or 0), 0, label)
        else:
            score = (0, -budget.get('expiring_headroom_percent', 0), value.get('plan') != 'pro', label)
        ranked.append((score, label))
    if snapshots and not ranked:
        raise ClaudeAccountsExhaustedError('claude_accounts_exhausted: all Claude accounts have an exhausted window')
    return min(ranked)[1] if ranked else None


def open_private(path, mode='a'):
    """Open for appending, or reading with mode 'r', never through a symlink; created 0600."""
    flags = os.O_RDONLY if mode == 'r' else os.O_WRONLY | os.O_CREAT | os.O_APPEND
    return os.fdopen(os.open(path, flags | os.O_NOFOLLOW, 0o600), mode)


@contextmanager
def locked(path, blocking=True):
    """Hold an exclusive lock on `path`; without `blocking`, yield False if another holder has it."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open_private(path) as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        except BlockingIOError:
            yield False
            return
        try:
            yield True
        finally:
            fcntl.flock(stream, fcntl.LOCK_UN)


def replace_private(path, text, mtime=None):
    """Replace `path` with `text` through an exclusive 0600 temporary; a planted name is never followed."""
    descriptor, name = tempfile.mkstemp(dir=path.parent, prefix=path.name + '.')
    try:
        with os.fdopen(descriptor, 'w') as stream:
            stream.write(text)
        if mtime is not None:
            os.utime(name, (mtime, mtime))
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    replace_private(path, json.dumps(value, indent=2, sort_keys=True) + '\n')


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
            values.append({**value, 'host': same_host(value.get('host'))})
    return values


def blank(account, status, error=None, now=None):
    return {'schema': SCHEMA, 'provider': account.provider, 'account': account.label, 'plan': account.plan,
            'host': telemetry_host(), 'active': account.active, 'priority': account.priority,
            'observed_at': None, 'attempted_at': iso(now) if now else None, 'source': None,
            'status': status, 'error': error, 'windows': [], 'unreported': [], 'credits': None}


def fetch(account, now):
    module = provider(account.provider)
    if module is None:
        raise Unavailable(f'no quota source is registered for {account.provider}')
    return module.fetch(account, IO, now)


def account_id(account):
    return f'{account.provider}/{account.label}'


def scheduled(account, previous, now, trigger='timer', power='ac', ignore_backoff=False):
    """(schedule block, due) for one account from its cached snapshot. Never fetches.

    With `quota.schedule.enabled` false, the account is due once its last attempt is older than
    `cache_seconds`, exactly as before the schedule existed."""
    config, notes = schedule.resolve(account.provider, account.options.get('quota'), settings())
    if config['enabled']:
        return schedule.plan(provider=account.provider, account_id=account_id(account), snapshot=previous,
                             now=now, config=config, trigger=trigger, power=power, ignore_backoff=ignore_backoff,
                             activity_at=schedule.activity(account.provider, account.identity_dir, now), notes=notes)
    attempted = parse_time((previous or {}).get('attempted_at'))
    due = not previous or not attempted or (now - attempted).total_seconds() >= cache_seconds()
    state = (previous or {}).get('fetch') or {}
    return {'next_due_at': iso(attempted + timedelta(seconds=cache_seconds()) if attempted else now),
            'reason': 'cache_seconds', 'ttl_seconds': cache_seconds(), 'trigger': state.get('trigger'),
            'failures': state.get('failures') or 0, 'backoff_until': None, 'error_kind': state.get('error_kind'),
            'last_duration_ms': state.get('last_duration_ms'), 'source_kind': 'fetch' if attempted else None,
            **({'notes': notes} if notes else {})}, due and trigger != 'power'


def cached(account, previous, block=None):
    return {**previous, 'active': account.active, 'priority': account.priority,
            'plan': account.plan or previous.get('plan'), **({'schedule': block} if block else {})}


def refresh(account, now=None, force=False, trigger='timer', power='ac', ignore_backoff=False):
    """The account's snapshot, fetched only when its schedule says it is due, unless forced."""
    if not enabled():
        return _refresh(account, now, force)
    with locked(cache_path(account.provider, account.label, identity_dir=account.identity_dir).with_suffix('.lock')):
        return _refresh(account, now, force, trigger, power, ignore_backoff)


def _refresh(account, now=None, force=False, trigger='timer', power='ac', ignore_backoff=False):
    now = now or utcnow()
    if not enabled():
        previous = load_snapshot(account.provider, account.label, account.identity_dir) or blank(account, 'disabled')
        return cached(account, previous)
    if provider(account.provider) is None:
        return blank(account, 'unreported', f'no quota source is registered for {account.provider}')
    previous = load_snapshot(account.provider, account.label, account.identity_dir)
    if account.signed_out:  # never fetched: the shared store would answer for the signed-in account
        return {**cached(account, previous or blank(account, 'signed_out')), 'status': 'signed_out', 'error': None}
    block, due = scheduled(account, previous, now, trigger, power, ignore_backoff)
    if previous and not force and not due:
        return cached(account, previous, block)
    started, kind, wait, deadline = monotonic(), None, None, None
    try:
        result = fetch(account, now)
        # A shared store answers for whoever is signed in now; never file that under another account, and never
        # file a reading that cannot say whose it is.
        if getattr(provider(account.provider), 'fingerprinted', None):
            expected, read = expected_fingerprint(account, previous), result.get('fingerprint')
            if not isinstance(read, str):
                raise Unavailable(f'{account.provider} did not say which account it read for {account.label}', 'auth')
            if expected and read != expected:
                raise Unavailable(f'{account.provider} is signed in to a different account than {account.label}', 'auth')
    except Unavailable as exc:
        # Provider text is fixed, but nothing token-shaped may reach the snapshot either.
        error, kind, wait = SECRETISH.sub('[redacted]', str(exc)), exc.kind, exc.retry_after
        deadline = exc.retry_at
    except Exception as exc:  # A provider module bug must not break quota or routing.
        error, kind = f'quota source failed ({type(exc).__name__})', 'other'
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
                 **{key: extra for key, extra in extras.items() if extra},
                 **({'fingerprint': result['fingerprint']} if isinstance(result.get('fingerprint'), str) else {})}
        value['fetch'] = fetch_state(account, previous, now, trigger, monotonic() - started)
        save_snapshot(value, account.identity_dir)
        trace(value, trigger, value['status'], None, value['fetch']['last_duration_ms'], None, None)
        return {**value, 'schedule': scheduled(account, value, now, trigger, power)[0]}
    value = {**(previous or blank(account, 'unavailable')), 'attempted_at': iso(now), 'error': error,
             'status': 'stale' if previous and previous.get('windows') else 'unavailable',
             'active': account.active, 'priority': account.priority,
             'fetch': fetch_state(account, previous, now, trigger, monotonic() - started, kind, wait, deadline)}
    save_snapshot(value, account.identity_dir)
    trace(value, trigger, 'error', kind, value['fetch']['last_duration_ms'], wait, error)
    return {**value, 'schedule': scheduled(account, value, now, trigger, power)[0]}


def fetch_state(account, previous, now, trigger, elapsed, kind=None, wait=None, deadline=None):
    """What the schedule needs from this attempt; a failure extends the account's backoff.

    The backoff runs from when the attempt finished, so a slow request never shortens a
    provider's Retry-After; a Retry-After date stays the deadline it named, capped at a day."""
    state = {'trigger': trigger, 'source_kind': 'fetch', 'last_duration_ms': round(elapsed * 1000),
             'failures': 0, 'error_kind': None, 'backoff_until': None, 'retry_after_s': None}
    if kind is None:
        return state
    before = (previous or {}).get('fetch') or {}
    failures = int(number(before.get('failures')) or 0) + 1
    config, _ = schedule.resolve(account.provider, account.options.get('quota'), settings())
    finished = now + timedelta(seconds=elapsed)
    seconds = schedule.backoff_seconds(kind, failures, None if deadline else wait, config, account_id(account))
    until = finished + timedelta(seconds=seconds)
    if deadline:
        until = max(until, min(deadline, finished + timedelta(seconds=RETRY_AFTER_CAP)))
    return {**state, 'failures': failures, 'error_kind': kind, 'retry_after_s': wait, 'backoff_until': iso(until)}


# Fetch trace ---------------------------------------------------------------------------------

def quota_dir():
    return state_root() / 'quota'


def scrub(text):
    """Trace text without anything token-shaped, at most TRACE_TEXT characters."""
    return SECRETISH.sub('[redacted]', str(text))[:TRACE_TEXT] if text else None


def trace(value, trigger, outcome, kind, duration_ms, wait, error):
    """Append one line per fetch attempt to fetches.jsonl. Never a credential or a raw body."""
    line = {'at': value.get('attempted_at'), 'provider': value.get('provider'), 'account': value.get('account'),
            'trigger': trigger, 'outcome': outcome, 'error_kind': kind, 'duration_ms': duration_ms,
            'retry_after_s': wait, 'error': scrub(error)}
    path = quota_dir() / 'fetches.jsonl'
    try:
        with locked(path.with_suffix('.lock')), open_private(path) as stream:
            stream.write(json.dumps(line, sort_keys=True) + '\n')
        # Every writer prunes, so `quota` and `quota --refresh` stay bounded too; gated daily.
        prune_trace(parse_time(line['at']) or utcnow())
    except OSError:
        pass  # The trace is advisory; a quota reading never fails on it.


def trace_lines(since=None):
    try:
        with open_private(quota_dir() / 'fetches.jsonl', 'r') as stream:
            text = stream.read()
    except OSError:
        return []
    rows = []
    for raw in text.splitlines():
        try:
            row = json.loads(raw)
        except ValueError:
            continue
        moment = parse_time(row.get('at')) if isinstance(row, dict) else None
        if moment and (since is None or moment >= since):
            rows.append(row)
    return rows


def prune_trace(now, retention_days=None):
    """Drop trace lines older than `quota.trace.retention_days`, at most once a day."""
    if retention_days is None:
        configured = setting_map('trace')
        retention_days = number(configured.get('retention_days'))
        retention_days = retention_days if retention_days and retention_days > 0 else TRACE_RETENTION_DAYS
    path, marker = quota_dir() / 'fetches.jsonl', quota_dir() / 'fetches.pruned'
    try:
        if now.timestamp() - marker.stat().st_mtime < 86400:
            return False
    except OSError:
        pass
    with locked(path.with_suffix('.lock')):
        kept = trace_lines(now - timedelta(days=retention_days))
        # Exclusive 0600 temporaries replace the trace and the marker; a planted name is never followed.
        if path.exists() or path.is_symlink():
            replace_private(path, ''.join(json.dumps(row, sort_keys=True) + '\n' for row in kept))
        replace_private(marker, '', now.timestamp())
    return True


def percentile(values, share):
    values = sorted(values)
    return values[min(len(values) - 1, int(share * len(values)))] if values else None


def trace_summary(since):
    groups = {}
    for row in trace_lines(since):
        group = groups.setdefault((str(row.get('provider')), str(row.get('account'))), {
            'provider': row.get('provider'), 'account': row.get('account'), 'fetches': 0, 'ok': 0,
            'failures': {}, 'durations': [], 'last_at': None})
        group['fetches'] += 1
        if row.get('outcome') == 'error':
            kind = row.get('error_kind') or 'other'
            group['failures'][kind] = group['failures'].get(kind, 0) + 1
        else:
            group['ok'] += 1
        if number(row.get('duration_ms')) is not None:
            group['durations'].append(row['duration_ms'])
        group['last_at'] = max(group['last_at'] or '', row.get('at') or '') or None
    accounts = []
    for _, group in sorted(groups.items()):
        durations = group.pop('durations')
        accounts.append({**group, 'p50_ms': percentile(durations, 0.5), 'max_ms': max(durations, default=None)})
    return {'since': iso(since), 'fetches': sum(row['fetches'] for row in accounts),
            'failures': sum(sum(row['failures'].values()) for row in accounts), 'accounts': accounts}


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
    if remaining is not None and remaining <= 0:
        return {**result, 'summary': f'reset passed at {iso(reset)}; awaiting a fresh reading'}
    if reset is None:
        return {**result, 'summary': 'reset time unreported; pace unknown'}
    if not minutes:
        return {**result, 'summary': f'resets in {duration(remaining)}; window length unreported'}
    if remaining > minutes * 60 + CLOCK_SKEW_SECONDS:
        return {**result, 'summary': 'reset time exceeds the reported window; pace unknown'}
    remaining = min(remaining, minutes * 60)
    elapsed = minutes * 60 - remaining
    expected = elapsed / (minutes * 60) * 100
    delta = actual - expected
    state = 'ahead_of_pace' if delta > PACE_TOLERANCE else 'reserve' if delta < -PACE_TOLERANCE else 'on_pace'
    lead = 'ahead of pace' if state == 'ahead_of_pace' else f'{abs(delta):.0f}% in reserve' if state == 'reserve' else 'on pace'
    rate = actual / elapsed if elapsed > 0 else 0
    run_out = (100 - actual) / rate if rate > 0 else None
    lasts = run_out is None or run_out >= remaining
    projected = min(100.0, actual + rate * remaining)
    result.update(expected_used_percent=round(expected, 1), delta_percent=round(delta, 1), will_last_to_reset=lasts,
                  runs_out_at=None if lasts else iso(now + timedelta(seconds=run_out)),
                  expiring_headroom_percent=round(100 - projected, 1) if lasts else 0)
    if elapsed / (minutes * 60) < EARLY_SHARE:
        return {**result, 'state': 'early', 'summary': f'window just started; resets in {duration(remaining)}'}
    tail = 'allowance lasts until reset' if lasts else f'allowance lasts about {duration(run_out)} at this rate'
    return {**result, 'state': state, 'summary': f'{lead}; {tail}'}


def sets_state(row):
    """An untimed budget, such as cloud session credits, or a window its provider marks advisory is shown but
    never sets the account's state: its pace is always unknown."""
    return not row.get('advisory') and not (row.get('limit_amount') and not row.get('window_minutes'))


def governing(rows):
    """The window that constrains the account most."""
    return min(filter(sets_state, rows), key=lambda row: (SEVERITY.index(row['pace']['state']), -row['used_percent']),
               default=None)


def pools(rows):
    """Windows by pool, each with the unpooled windows that bind every pool; one group when nothing is pooled."""
    names = sorted({row['pool'] for row in rows if row.get('pool')})
    return {name: [row for row in rows if row.get('pool') in (name, None)] for name in names} or {None: rows}


def back_at(rows):
    """When a pool's exhaustion ends: its exhausted window that resets last, or None if none is exhausted."""
    spent = [row for row in rows if sets_state(row) and row['pace']['state'] == 'exhausted']
    return max(spent, key=lambda row: row.get('reset_at') or '9999', default=None)


def binding(rows):
    """The window that binds the account. Pools are independent, so the account is usable while any pool is:
    the most constrained usable pool binds, and when every pool is exhausted, the pool back soonest binds."""
    groups = list(pools(rows).values())
    live = [worst for worst in map(governing, groups) if worst and worst['pace']['state'] != 'exhausted']
    if live:
        return governing(live)
    spent = [window for window in map(back_at, groups) if window]
    return min(spent, key=lambda row: row.get('reset_at') or '9999', default=None) or governing(rows)


def reset_passed(row, now):
    reset = parse_time(row.get('reset_at'))
    return bool(reset and reset <= now - timedelta(seconds=CLOCK_SKEW_SECONDS))


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
                result[(*parts[:2], same_host(parts[2]))] = entry
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
        earlier = [f'{provider_name}/{label}/{host}' for host in local_hosts()[1:]]
        if key not in current:
            # Keep a count banked under an earlier host id or the generic key where it is.
            key = next((name for name in [*earlier, generic] if name in current), key)
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
    wait = number(setting('credit_wait_hours')) or WAIT_HOURS
    left = (reset - now).total_seconds() if reset else None
    if left is not None and left <= wait * 3600:
        return {**advice, 'action': 'wait', 'reason': f'the natural reset is in {duration(left)}'}
    return {**advice, 'action': 'redeem',
            'reason': 'the natural reset is ' + (f'{duration(left)} away' if left else 'unreported') +
                      '; prefer a reset that keeps the reset date'}


def freshest(values):
    """One snapshot per (provider, account): the most recent reading from any host."""
    best = {}
    for value in values:
        key = (value.get('provider'), value.get('account'))
        if key not in best or (value.get('observed_at') or '') > (best[key].get('observed_at') or ''):
            best[key] = value
    return [best[key] for key in sorted(best, key=lambda key: tuple(str(part) for part in key))]


def credits_policy():
    value = setting('credits_policy')
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
    floor = number(setting('credit_floor')) or 0
    return next((entry for entry in normalise_usage_credits(value.get('usage_credits')) or []
                 if entry['unlimited'] or entry['balance'] > floor), None)


def burn_rate(value, entry):
    """Credits spent per hour since the balance began falling, from successive readings."""
    anchor = value.get('credit_anchor') or {}
    start, end = parse_time(anchor.get('observed_at')), parse_time(value.get('observed_at'))
    spent = (number(anchor.get('balance')) or 0) - (entry['balance'] or 0)
    hours = (end - start).total_seconds() / 3600 if start and end else 0
    return round(spent / hours, 2) if spent > 0 and hours > 0 else None


def local_activity(found=None):
    """The latest local use per account id, from identity file times; a signed-out account has none, since the
    shared store's activity belongs to whoever is signed in."""
    try:
        found = accounts() if found is None else found
    except (OSError, ValueError):
        return {}
    return {account_id(account): schedule.activity(account.provider, account.identity_dir)
            for account in found if not account.signed_out}


def gather(now=None, force=False):
    """Every account, each fetched if due. Without `now`, each attempt is timed when it starts."""
    return [refresh(account, now, force) for account in accounts()]


# Tick: fetch only the due accounts and keep report.json current -------------------------------

def report_path():
    return quota_dir() / 'report.json'


def current_report():
    try:
        value = json.loads(report_path().read_text())
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) and value.get('schema') == REPORT_SCHEMA else None


def plans(now, trigger='timer', power='ac', ignore_backoff=False):
    """(account, cached value, due) per account, from cached snapshots only."""
    rows = []
    for account in accounts():
        previous = load_snapshot(account.provider, account.label, account.identity_dir)
        # Phase 4 hook: passive readers (rollouts, refusals, cooldowns) would update `previous` here.
        if not enabled() or provider(account.provider) is None or account.signed_out:
            rows.append((account, _refresh(account, now), False))
            continue
        block, due = scheduled(account, previous, now, trigger, power, ignore_backoff)
        rows.append((account, cached(account, previous or blank(account, 'unavailable'), block), due))
    return rows


def tick(trigger='timer', power='ac', ignore_backoff=False):
    """Fetch the due accounts under a non-blocking refresh lock. None means another tick holds it."""
    with locked(quota_dir() / 'refresh.lock', blocking=False) as held:
        return _tick(trigger, power, ignore_backoff) if held else None


def _tick(trigger, power, ignore_backoff):
    now = utcnow()
    rows = plans(now, trigger, power, ignore_backoff)
    values = {account_id(account): value for account, value, _ in rows}
    activity = local_activity([account for account, _, _ in rows])
    due = [account for account, _, is_due in rows if is_due]
    if not due:
        value = quota_display.report(globals(), list(values.values()), now, activity=activity)
        written = current_report()
        # The fast path writes only when the earliest due time or the headline moved, or no report exists yet.
        if written is None or any(written.get(key) != value[key] for key in ('next_due_at', 'headline')):
            atomic_json(report_path(), value)
        return value
    from concurrent.futures import ThreadPoolExecutor, as_completed
    import threading
    guard, refreshing, kinds = threading.Lock(), {account_id(account) for account in due}, []

    def publish():
        atomic_json(report_path(), quota_display.report(globals(), list(values.values()), utcnow(), refreshing, activity))

    def run(account):
        try:
            value = refresh(account, None, False, trigger, power, ignore_backoff)
        except (OSError, ValueError):
            value = values[account_id(account)]
        with guard:
            values[account_id(account)] = value
            refreshing.discard(account_id(account))
            kinds.append((value.get('fetch') or {}).get('error_kind'))
            publish()

    with guard:
        publish()
    config, _ = schedule.resolve('', None, settings())
    solo = [account for account in due if schedule.resolve(account.provider, account.options.get('quota'),
                                                            settings())[0]['solo']]
    try:
        with ThreadPoolExecutor(max_workers=config['max_parallel']) as pool:
            for future in as_completed([pool.submit(run, account) for account in due if account not in solo]):
                future.result()
        for account in solo:  # An expensive solo provider never runs alongside another fetch.
            run(account)
    finally:  # Even after a crash, readers never see a fetch left marked as running.
        with guard:
            value = quota_display.report(globals(), list(values.values()), utcnow(), activity=activity)
            if kinds and len(kinds) == len(due) and all(kind == 'network' for kind in kinds):
                value['offline'] = True  # every fetch failed to connect
            atomic_json(report_path(), value)
    return value


# Routing input -------------------------------------------------------------------------------

def refused_until(worst, value, now):
    """When a refused allowance may return: the governing window's reset, else a bounded wait.

    The provider refuses ordinary use below 100%. With no future reset known, the account cools for
    `quota.unknown_reset_cooldown_seconds` (default an hour) from the reading, then the next reading decides."""
    reset = parse_time(worst.get('reset_at'))
    if reset and reset > now:
        return worst['reset_at']
    seconds = number(setting('unknown_reset_cooldown_seconds'))
    observed = parse_time(value.get('observed_at')) or now
    return iso(observed + timedelta(seconds=seconds if seconds and seconds > 0 else UNKNOWN_RESET_SECONDS))


def route_states(now=None, *, selected_accounts=None):
    """Pace state per provider from the cached snapshot of its active account. Never fetches."""
    now = now or utcnow()
    if not enabled():
        return {}
    try:
        candidates = ([account for name in selected_accounts for account in accounts(name)]
                      if selected_accounts is not None else accounts())
        current = {(account.provider, account.label): account for account in candidates}
    except ValueError:
        return {}
    states = {}
    for (name, label), account in current.items():
        value = load_snapshot(name, label, account.identity_dir)
        active = label == selected_accounts.get(name) if selected_accounts is not None else account.active
        if not active or not value or value.get('status') not in ('ok', 'stale'):
            continue
        module = provider(name)
        banked = credits().get((name, label, telemetry_host()))
        row = quota_display.describe(globals(), value, banked, now)
        current = [window for window in row['windows'] if not window.get('reset_passed')]
        worst = binding(current)
        headroom = bool(worst and (worst['pace'].get('will_last_to_reset') is True
                                  or worst['used_percent'] < NEAR_EXHAUSTED))
        exhausted = [window['reset_at'] for window in current
                     if window['pace']['state'] == 'exhausted' and sets_state(window)]
        if row['state'] == 'exhausted' and (not exhausted or
                all(exhausted) and max(parse_time(reset) for reset in exhausted) <= now):
            exhausted = [refused_until(binding(current) or binding(row['windows']), value, now)]
        states[name] = {'adapter': getattr(module, 'ADAPTER', None), 'covers': list(getattr(module, 'COVERS', ())),
                        'account': label, 'host': telemetry_host(), 'state': row['state'], 'has_headroom': headroom,
                        'summary': row['summary'], 'observed_at': value.get('observed_at'),
                        'cooling_until': max(exhausted) if exhausted and all(exhausted) else None,
                        # Pools pick a last-resort adapter only when nothing with allowance qualifies.
                        **({'last_resort': True} if row['state'] == 'credit_only' and
                           credits_policy() == 'last_resort' else {})}
    return states


def apply_to_cooldowns(records, now=None, *, selected_accounts=None, include_exhausted=True):
    """Add cooldowns for exhausted providers; drop stale usage cooldowns where the provider has headroom."""
    now = now or utcnow()
    try:
        states = route_states(now, selected_accounts=selected_accounts)
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
        if observed and (drawing or state['has_headroom'] and
                         state['state'] in ('ahead_of_pace', 'reserve', 'on_pace', 'early', 'not_started')):
            for key, record in records.items():
                recorded = parse_time((record or {}).get('recorded_at'))
                if (identity.matches(record, state) and str(key).split('/', 1)[0] == adapter and record.get('status') in ('usage_limited', 'quota_exhausted')
                        and recorded and observed > recorded and (not drawing or record.get('source') == 'provider quota')):
                    result.pop(key, None)
        if include_exhausted and state['state'] == 'exhausted' and until and until > now:
            key = f'{adapter}/*'
            record = result.get(key) or {}
            recorded = parse_time(record.get('recorded_at'))
            if (str(record.get('cooling_until', '')) < state['cooling_until'] or
                    record.get('status') in ('usage_limited', 'quota_exhausted') and
                    observed and recorded and observed > recorded):
                result[key] = {'adapter': adapter, 'model': '*', 'account': state['account'], 'host': state['host'],
                               'status': 'quota_exhausted', 'cooling_until': state['cooling_until'],
                               'recorded_at': state['observed_at'], 'source': 'provider quota',
                               **({'covers': state['covers']} if state['covers'] else {})}
    return result


# Command line --------------------------------------------------------------------------------

def since_time(text, now):
    """`24h`, `30m`, `7d` or an ISO time."""
    match = re.fullmatch(r'(\d+)\s*([mhd])', str(text).strip().lower())
    if match:
        return now - timedelta(minutes=int(match[1]) * {'m': 1, 'h': 60, 'd': 1440}[match[2]])
    moment = parse_time(text)
    if moment is None:
        raise ValueError(f'--since {text!r} is not a duration such as 24h or an ISO time')
    return moment


def schedule_main(argv):
    parser = argparse.ArgumentParser(prog='provenant quota schedule',
                                     description='Each account\'s schedule from cached readings. Never fetches.')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(argv)
    rows = [{'provider': account.provider, 'account': account.label, 'schedule': value.get('schedule')}
            for account, value, _ in plans(utcnow())]
    if args.json:
        print(json.dumps(rows, indent=2))
        return 0
    lines = quota_display.table(('provider', 'account', 'next due', 'reason', 'ttl', 'failures', 'error'), [
        (row['provider'], row['account'], (block.get('next_due_at') or '-')[:19].replace('T', ' '),
         block.get('reason') or '-', duration(block.get('ttl_seconds')), str(block.get('failures') or 0),
         block.get('error_kind') or '-') for row in rows for block in [row['schedule'] or {}]])
    print('\n'.join(lines))
    return 0


def trace_main(argv):
    parser = argparse.ArgumentParser(prog='provenant quota trace', description='Summarise recorded fetch attempts.')
    parser.add_argument('--since', default='24h')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(argv)
    try:
        value = trace_summary(since_time(args.since, utcnow()))
    except ValueError as exc:
        parser.error(str(exc))
    if args.json:
        print(json.dumps(value, indent=2))
        return 0
    lines = [f"Quota fetches since {value['since']}: {value['fetches']} ({value['failures']} failed)"]
    lines += quota_display.table(('provider', 'account', 'fetches', 'failures', 'p50', 'max', 'last'), [
        (row['provider'], row['account'], str(row['fetches']),
         ', '.join(f'{kind} {count}' for kind, count in sorted(row['failures'].items())) or '-',
         f"{row['p50_ms']}ms" if row['p50_ms'] is not None else '-',
         f"{row['max_ms']}ms" if row['max_ms'] is not None else '-', (row['last_at'] or '-')[:19].replace('T', ' '))
        for row in value['accounts']])
    print('\n'.join(lines))
    return 0


def tick_main(argv):
    parser = argparse.ArgumentParser(prog='provenant quota tick',
                                     description='Fetch only the accounts that are due and write report.json.')
    parser.add_argument('--trigger', choices=schedule.TRIGGERS, default='timer')
    parser.add_argument('--power', choices=schedule.POWER, default='ac', help='supplied by the caller')
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--ignore-backoff', action='store_true')
    args = parser.parse_args(argv)
    try:
        value = tick(args.trigger, args.power, args.ignore_backoff)
        # Another tick holds the lock: show what it last wrote and leave the fetching to it.
        value = value or current_report() or quota_display.report(globals(), [row for _, row, _ in plans(utcnow())])
    except (OSError, ValueError) as exc:
        print(f'quota: {exc}', file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(value, indent=2))
        return 0
    for row in value['accounts']:
        block = row.get('schedule') or {}
        print(f"{row['provider']}/{row['account']}: {row['status']}; next {block.get('next_due_at') or '-'} "
              f"({block.get('reason') or '-'})")
    return 0


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv[:1] == ['tick']:
        return tick_main(argv[1:])
    if argv[:1] == ['schedule']:
        return schedule_main(argv[1:])
    if argv[:1] == ['trace']:
        return trace_main(argv[1:])
    if argv[:1] == ['path']:
        parser = argparse.ArgumentParser(prog='provenant quota path')
        parser.add_argument('--json', action='store_true')
        args = parser.parse_args(argv[1:])
        paths = {'state_dir': str(quota_dir()), 'report': str(report_path()),
                 'trace': str(quota_dir() / 'fetches.jsonl'), 'lock': str(quota_dir() / 'refresh.lock')}
        print(json.dumps(paths, indent=2) if args.json else paths['state_dir'])
        return 0
    if argv[:1] == ['forget']:
        parser = argparse.ArgumentParser(prog='provenant quota forget',
                                         description='Remove a signed-out account\'s remembered reading on this host.')
        parser.add_argument('account', help='<provider>/<label>, as `provenant quota` lists it')
        parser.add_argument('--json', action='store_true', help='print the report without the account')
        args = parser.parse_args(argv[1:])
        try:
            message = forget(args.account)
            value = quota_display.report(globals(), [row for _, row, _ in plans(utcnow())]) if args.json else None
            if value is not None:
                atomic_json(report_path(), value)
        except (OSError, ValueError) as exc:
            print(f'quota: {exc}', file=sys.stderr)
            return 1
        print(json.dumps(value, indent=2) if args.json else message)
        return 0
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
        value = quota_display.report(globals(), gather(force=args.refresh))
    except (OSError, ValueError) as exc:
        print(f'quota: {exc}', file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(value, indent=2))
    else:
        quota_display.render(globals(), value, args.verbose)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
