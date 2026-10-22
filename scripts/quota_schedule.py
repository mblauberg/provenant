"""When each quota account is next due for a fetch, and why. Pure policy: no I/O beyond stat.

`plan()` turns one account's cached snapshot, its identity activity, the trigger and the power
state into a `schedule` block. `backoff_seconds()` sets the wait after a failed fetch.
`resolve()` reads configuration: the account `quota` block, then `quota.providers.<name>`,
then `quota.schedule` and `quota.backoff`, then the provider POLICY below, then the defaults.
An invalid value falls through to the next layer and adds a note.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
import hashlib
import math
import os
from pathlib import Path


SCHEDULE = {
    'enabled': True,
    'min_interval_seconds': 60,
    'max_interval_seconds': 21600,
    'active_window_seconds': 900,
    'jitter_fraction': 0.1,
    'coalesce_seconds': 120,
    'near_exhaustion_percent': 85,
    'near_exhaustion_factor': 0.5,
    'runs_out_horizon_factor': 2,
    'reset_grace_seconds': 60,
    'exhausted_recheck_seconds': 21600,
    'battery_factor': 2,
    'low_power_factor': 4,
    'low_power_skip_costs': ['expensive'],
    'manual_min_age_seconds': 30,
    'event_debounce_seconds': 30,
    'max_parallel': 3,
}
BACKOFF = {'base_seconds': 120, 'max_seconds': 3600, 'auth_seconds': 3600, 'missing_seconds': 86400}
COSTS = ('cheap', 'moderate', 'expensive')
# Per-provider defaults. A solo provider never fetches alongside another account.
POLICY = {
    'claude': {'cost': 'moderate', 'ttl_active_seconds': 300, 'ttl_idle_seconds': 1800, 'ttl_panel_seconds': 120},
    'codex': {'cost': 'moderate', 'ttl_active_seconds': 900, 'ttl_idle_seconds': 3600, 'ttl_panel_seconds': 300},
    'cursor': {'cost': 'cheap', 'ttl_active_seconds': 1800, 'ttl_idle_seconds': 3600, 'ttl_panel_seconds': 600},
    'opencode': {'cost': 'cheap', 'ttl_active_seconds': 300, 'ttl_idle_seconds': 3600, 'ttl_panel_seconds': 60},
    'agy': {'cost': 'expensive', 'solo': True, 'ttl_active_seconds': 1200, 'ttl_idle_seconds': 7200,
            'ttl_panel_seconds': 900},
}
DEFAULT_POLICY = {'cost': 'moderate', 'solo': False, 'ttl_active_seconds': 900, 'ttl_idle_seconds': 3600,
                  'ttl_panel_seconds': 300}
TRIGGERS = ('timer', 'panel', 'manual', 'event', 'wake', 'power')
POWER = ('ac', 'battery', 'low')
ERROR_KINDS = ('rate_limited', 'network', 'auth', 'missing', 'other')
OWN_WINDOW_SLACK = 5  # seconds after our own fetch whose identity writes are ours, not the user's


def _seconds(value):
    return not isinstance(value, bool) and isinstance(value, (int, float)) and math.isfinite(value) and value >= 0


def _positive(value):
    return _seconds(value) and value > 0


VALID = {
    'enabled': lambda value: isinstance(value, bool),
    'solo': lambda value: isinstance(value, bool),
    'cost': lambda value: value in COSTS,
    'jitter_fraction': lambda value: _seconds(value) and value < 1,
    'near_exhaustion_percent': lambda value: _seconds(value) and value <= 100,
    'near_exhaustion_factor': lambda value: _positive(value) and value <= 1,
    'battery_factor': lambda value: _positive(value) and value >= 1,
    'low_power_factor': lambda value: _positive(value) and value >= 1,
    'low_power_skip_costs': lambda value: isinstance(value, list) and all(item in COSTS for item in value),
    'max_parallel': lambda value: type(value) is int and value >= 1,
}
POSITIVE = {'min_interval_seconds', 'max_interval_seconds', 'runs_out_horizon_factor', 'base_seconds', 'max_seconds',
            'ttl_active_seconds', 'ttl_idle_seconds', 'ttl_panel_seconds'}


def _valid(key, value):
    check = VALID.get(key) or (_positive if key in POSITIVE else _seconds)
    return check(value)


def resolve(provider, account_quota=None, settings=None):
    """(config, notes) for one account; every key comes from the first layer that sets it validly."""
    settings = settings if isinstance(settings, dict) else {}
    providers = settings.get('providers') if isinstance(settings.get('providers'), dict) else {}
    layers = [('account quota', account_quota), (f'quota.providers.{provider}', providers.get(provider)),
              ('quota.schedule', settings.get('schedule')), ('quota.backoff', settings.get('backoff'))]
    defaults = {**SCHEDULE, **BACKOFF, **DEFAULT_POLICY, **POLICY.get(provider, {})}
    config, notes = {}, []
    for key, default in defaults.items():
        for name, layer in layers:
            if not isinstance(layer, dict) or key not in layer:
                continue
            if name == 'quota.schedule' and key in BACKOFF or name == 'quota.backoff' and key not in BACKOFF:
                continue
            if _valid(key, layer[key]):
                config[key] = layer[key]
                break
            notes.append(f'{name}.{key} is invalid; using the default')
        config.setdefault(key, default)
    if config['max_interval_seconds'] < config['min_interval_seconds']:
        notes.append('max_interval_seconds is below min_interval_seconds; using the defaults')
        config['min_interval_seconds'], config['max_interval_seconds'] = (
            SCHEDULE['min_interval_seconds'], SCHEDULE['max_interval_seconds'])
    return config, notes


# Activity: stat only, per identity directory -------------------------------------------------

def _mtime(path):
    try:
        return os.stat(path).st_mtime
    except OSError:
        return None


def _newest_files(directory, suffix, limit):
    """Paths of `suffix` files in the `limit` most recently changed subdirectories."""
    try:
        with os.scandir(directory) as entries:
            folders = sorted(((entry.stat().st_mtime, entry.path) for entry in entries if entry.is_dir()),
                             reverse=True)[:limit]
    except OSError:
        return []
    found = []
    for _, folder in folders:
        try:
            with os.scandir(folder) as entries:
                found += [entry.path for entry in entries if entry.name.endswith(suffix)]
        except OSError:
            continue
    return found


def activity_paths(provider, identity_dir, now=None, home=None):
    """Files whose change means the user used this identity. Nothing here is a credential."""
    root = Path(identity_dir).expanduser()
    if provider == 'claude':
        return [root / 'history.jsonl', *_newest_files(root / 'projects', '.jsonl', 4)]
    if provider == 'codex':
        today = (now or datetime.now(UTC)).astimezone()
        paths = []
        for day in (today, today - timedelta(days=1)):
            folder = root / 'sessions' / f'{day:%Y}' / f'{day:%m}' / f'{day:%d}'
            try:
                with os.scandir(folder) as entries:
                    paths += [entry.path for entry in entries]
            except OSError:
                continue
        return paths
    if provider == 'opencode':
        return [root / 'opencode.db-wal']
    if provider == 'agy':
        return [Path(home or Path.home()) / '.gemini/antigravity-cli/log']
    return []


def activity(provider, identity_dir, now=None, home=None):
    """The latest activity time (epoch seconds) for this identity, or None."""
    return max(filter(None, map(_mtime, activity_paths(provider, identity_dir, now, home))), default=None)


# Planning ------------------------------------------------------------------------------------

def _time(value):
    if not isinstance(value, str) or not value:
        return None
    try:
        moment = datetime.fromisoformat(value)
    except ValueError:
        return None
    return moment if moment.tzinfo else moment.replace(tzinfo=UTC)


def _iso(moment):
    return moment.astimezone(UTC).isoformat().replace('+00:00', 'Z') if moment else None


def _number(value):
    return value if not isinstance(value, bool) and isinstance(value, (int, float)) and math.isfinite(value) else None


def jitter(seed, fraction):
    """A stable offset in [-fraction, fraction] for one account and one anchor time."""
    digest = int.from_bytes(hashlib.sha256(seed.encode()).digest()[:8], 'big')
    return (digest / 2 ** 64 * 2 - 1) * fraction


def backoff_seconds(kind, failures, retry_after, config, seed=''):
    """Seconds to wait after `failures` consecutive failures of this kind."""
    if kind == 'missing':
        wait = config['missing_seconds']
    elif kind == 'auth':
        wait = config['auth_seconds']
    else:
        wait = min(config['max_seconds'], config['base_seconds'] * 2 ** max(0, min(failures, 32) - 1))
    wait *= 1 + jitter(f'{seed}|{failures}', config['jitter_fraction'])
    if kind == 'rate_limited' and _number(retry_after) is not None:
        wait = max(wait, retry_after)
    return wait


def _sets_state(row):
    return not row.get('advisory') and not (row.get('limit_amount') and not row.get('window_minutes'))


def _windows(snapshot):
    return [row for row in (snapshot or {}).get('windows') or []
            if isinstance(row, dict) and _number(row.get('used_percent')) is not None and _sets_state(row)]


def _near(rows, now, ttl, config):
    """Near exhaustion: use at or above the threshold, or a run-out projected within the horizon."""
    for row in rows:
        used = row['used_percent']
        if used >= 100:
            continue
        if used >= config['near_exhaustion_percent']:
            return True
        reset, minutes = _time(row.get('reset_at')), _number(row.get('window_minutes'))
        if not reset or not minutes or used <= 0:
            continue
        remaining = (reset - now).total_seconds()
        elapsed = minutes * 60 - remaining
        if remaining <= 0 or elapsed <= 0:
            continue
        runs_out = (100 - used) / (used / elapsed)
        if runs_out < remaining and runs_out <= config['runs_out_horizon_factor'] * ttl:
            return True
    return False


def plan(*, provider, account_id, snapshot, now, config, trigger='timer', power='ac', activity_at=None,
         ignore_backoff=False, notes=()):
    """The account's schedule block, plus `due`: whether this trigger should fetch it now."""
    snapshot = snapshot or {}
    state = snapshot.get('fetch') if isinstance(snapshot.get('fetch'), dict) else {}
    attempted = _time(snapshot.get('attempted_at'))
    failures = int(_number(state.get('failures')) or 0)
    kind = state.get('error_kind') if state.get('error_kind') in ERROR_KINDS else None
    backoff = _time(state.get('backoff_until')) if failures else None
    duration = (_number(state.get('last_duration_ms')) or 0) / 1000
    seen = datetime.fromtimestamp(activity_at, UTC) if _number(activity_at) is not None else None
    own = attempted and seen and attempted <= seen <= attempted + timedelta(seconds=duration + OWN_WINDOW_SLACK)
    active = bool(seen and not own and (now - seen).total_seconds() <= config['active_window_seconds'])
    since_fetch = bool(seen and attempted and not own and seen > attempted)

    base = config['ttl_active_seconds'] if active else config['ttl_idle_seconds']
    reason = 'active' if active else 'idle'
    rows = _windows(snapshot)
    factor = 1
    if _near(rows, now, base, config):
        factor, reason = config['near_exhaustion_factor'], 'near_exhaustion'
    if trigger != 'manual':
        factor *= {'battery': config['battery_factor'], 'low': config['low_power_factor']}.get(power, 1)
    ttl = min(config['max_interval_seconds'], max(config['min_interval_seconds'], base * factor))
    if power == 'low' and trigger != 'manual' and config['cost'] in config['low_power_skip_costs']:
        ttl, reason = config['max_interval_seconds'], 'low_power'

    if attempted is None:
        next_due, reason = now, 'first_fetch'
    else:
        next_due = attempted + timedelta(seconds=ttl * (1 + jitter(f'{account_id}|{_iso(attempted)}',
                                                                   config['jitter_fraction'])))
        exhausted = [_time(row.get('reset_at')) for row in rows if row['used_percent'] >= 100]
        if exhausted and all(reset and reset > now for reset in exhausted):
            next_due = min(max(exhausted) + timedelta(seconds=config['reset_grace_seconds']),
                           attempted + timedelta(seconds=config['exhausted_recheck_seconds']))
            reason = 'exhausted_until_reset'
        passed = [reset for reset in map(_time, (row.get('reset_at') for row in rows))
                  if reset and attempted < reset <= now]
        if passed:
            next_due, reason = min(passed), 'reset_passed'
    # Backoff wins; identity activity after a failed sign-in clears an auth backoff. A manual
    # trigger passes any backoff but a rate limit, and --ignore-backoff passes all of them:
    # both are then judged on the plain schedule.
    blocked, plain = False, next_due
    if backoff and not (kind == 'auth' and since_fetch):
        next_due, reason = backoff, 'backoff'
        blocked = backoff > now and not ignore_backoff and not (trigger == 'manual' and kind != 'rate_limited')
    elif backoff:
        next_due = plain = now

    age = (now - attempted).total_seconds() if attempted else math.inf
    coalesce = min(config['coalesce_seconds'], 0.25 * ttl)
    timer_due = (min(next_due, plain) - now).total_seconds() <= coalesce
    due = not blocked and {
        'timer': timer_due,
        'panel': timer_due or age >= config['ttl_panel_seconds'],
        'manual': timer_due or age >= config['manual_min_age_seconds'],
        'event': timer_due or age >= max(config['min_interval_seconds'], config['event_debounce_seconds']),
        'wake': timer_due or age >= config['min_interval_seconds'],
    }.get(trigger, False)
    block = {'next_due_at': _iso(next_due), 'reason': reason, 'ttl_seconds': round(ttl),
             'trigger': state.get('trigger'), 'failures': failures, 'backoff_until': _iso(backoff),
             'error_kind': kind if failures else None, 'last_duration_ms': state.get('last_duration_ms'),
             'source_kind': state.get('source_kind') or ('fetch' if attempted else None)}
    if notes:
        block['notes'] = list(notes)
    return block, due
