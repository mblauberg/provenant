"""Adaptive quota refresh policy: due times, reasons, backoff and config resolution."""

import builtins
from datetime import UTC, datetime, timedelta
import importlib.util
import os
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
NOW = datetime(2026, 10, 22, 12, 0, tzinfo=UTC)
WEEK = 7 * 24 * 60
spec = importlib.util.spec_from_file_location('quota_schedule_under_test', ROOT / 'scripts/quota_schedule.py')
schedule = importlib.util.module_from_spec(spec)
spec.loader.exec_module(schedule)


def iso(moment):
    return moment.isoformat().replace('+00:00', 'Z')


def ago(**delta):
    return NOW - timedelta(**delta)


def snap(attempted=None, windows=(), **fetch):
    return {'attempted_at': iso(attempted) if attempted else None, 'windows': list(windows),
            **({'fetch': fetch} if fetch else {})}


def win(used, minutes=300, reset_in=None, **extra):
    return {'name': 'w', 'used_percent': used, 'window_minutes': minutes,
            'reset_at': iso(NOW + reset_in) if reset_in is not None else None, **extra}


def plan(provider='claude', snapshot=None, settings=None, activity=None, **options):
    config, notes = schedule.resolve(provider, None, settings or {})
    return schedule.plan(provider=provider, account_id=f'{provider}/a', snapshot=snapshot, now=NOW, config=config,
                         activity_at=activity.timestamp() if activity else None, notes=notes, **options)


# (case, provider, snapshot, options, activity, reason, ttl, due)
CASES = [
    ('never fetched', 'claude', None, {}, None, 'first_fetch', 1800, True),
    ('idle claude, fetched 10m ago', 'claude', snap(ago(minutes=10)), {}, None, 'idle', 1800, False),
    ('idle claude, fetched 40m ago', 'claude', snap(ago(minutes=40)), {}, None, 'idle', 1800, True),
    ('active claude, fetched 6m ago', 'claude', snap(ago(minutes=6)), {}, ago(minutes=1), 'active', 300, True),
    ('active claude, fetched 2m ago', 'claude', snap(ago(minutes=2)), {}, ago(minutes=1), 'active', 300, False),
    ('activity only inside our own fetch', 'claude',
     snap(ago(minutes=6), last_duration_ms=3000), {}, ago(minutes=6) + timedelta(seconds=2), 'idle', 1800, False),
    ('activity outside the active window', 'claude', snap(ago(minutes=6)), {}, ago(hours=2), 'idle', 1800, False),
    ('near exhaustion halves the ttl', 'claude', snap(ago(minutes=20), [win(90, reset_in=timedelta(hours=3))]),
     {}, None, 'near_exhaustion', 900, True),
    ('projected run-out within the horizon', 'claude',
     snap(ago(minutes=5), [win(60, reset_in=timedelta(hours=4, minutes=30))]), {}, None, 'near_exhaustion', 900, False),
    ('battery doubles the ttl', 'claude', snap(ago(minutes=40)), {'power': 'battery'}, None, 'idle', 3600, False),
    ('low power quadruples the ttl', 'codex', snap(ago(minutes=40)), {'power': 'low'}, None, 'idle', 14400, False),
    ('low power skips an expensive provider', 'agy', snap(ago(hours=3)), {'power': 'low'}, None, 'low_power', 21600, False),
    ('manual ignores power', 'agy', snap(ago(minutes=1)), {'power': 'low', 'trigger': 'manual'}, None, 'idle', 7200, True),
    ('opencode active is clamped to its floor', 'opencode', snap(ago(minutes=3)),
     {'settings': {'providers': {'opencode': {'ttl_active_seconds': 10}}}}, ago(seconds=30), 'active', 60, True),
    ('exhausted until reset', 'claude', snap(ago(minutes=40), [win(100, reset_in=timedelta(hours=2))]),
     {}, None, 'exhausted_until_reset', 1800, False),
    ('reset passed since the last fetch', 'claude',
     snap(ago(minutes=10), [win(100, reset_in=timedelta(minutes=-1))]), {}, None, 'reset_passed', 1800, True),
    ('advisory windows never set the schedule', 'claude',
     snap(ago(minutes=10), [win(100, reset_in=timedelta(hours=2), advisory=True)]), {}, None, 'idle', 1800, False),
    ('panel uses the provider panel ttl', 'claude', snap(ago(minutes=3)), {'trigger': 'panel'}, None, 'idle', 1800, True),
    ('panel inside the panel ttl', 'claude', snap(ago(minutes=1)), {'trigger': 'panel'}, None, 'idle', 1800, False),
    ('manual inside its minimum age', 'claude', snap(ago(seconds=20)), {'trigger': 'manual'}, None, 'idle', 1800, False),
    ('event after the minimum interval', 'codex', snap(ago(seconds=90)), {'trigger': 'event'}, None, 'idle', 3600, True),
    ('wake inside the minimum interval', 'codex', snap(ago(seconds=30)), {'trigger': 'wake'}, None, 'idle', 3600, False),
    ('power only recomputes', 'claude', snap(ago(hours=3)), {'trigger': 'power'}, None, 'idle', 1800, False),
]


@pytest.mark.parametrize('case, provider, snapshot, options, activity, reason, ttl, due', CASES,
                         ids=[case[0] for case in CASES])
def test_plan(case, provider, snapshot, options, activity, reason, ttl, due):
    block, is_due = plan(provider, snapshot, activity=activity, **options)
    assert (block['reason'], block['ttl_seconds'], is_due) == (reason, ttl, due)
    assert set(block) >= {'next_due_at', 'reason', 'ttl_seconds', 'trigger', 'failures', 'backoff_until',
                          'error_kind', 'last_duration_ms', 'source_kind'}


def test_due_times_follow_ttl_with_bounded_jitter_and_reset_rules():
    block, _ = plan('claude', snap(ago(minutes=10)))
    offset = (schedule._time(block['next_due_at']) - ago(minutes=10)).total_seconds()
    assert 1800 * 0.9 <= offset <= 1800 * 1.1
    assert plan('claude', snap(ago(minutes=10)))[0] == block  # stable for one anchor
    exhausted, _ = plan('claude', snap(ago(minutes=40), [win(100, reset_in=timedelta(hours=2))]))
    assert schedule._time(exhausted['next_due_at']) == NOW + timedelta(hours=2, seconds=60)
    far, _ = plan('claude', snap(ago(minutes=40), [win(100, WEEK, reset_in=timedelta(days=5))]))
    assert (far['reason'], schedule._time(far['next_due_at'])) == ('exhausted_until_reset', ago(minutes=40) + timedelta(hours=6))
    passed, _ = plan('claude', snap(ago(minutes=10), [win(40, reset_in=timedelta(minutes=-1))]))
    assert schedule._time(passed['next_due_at']) == NOW - timedelta(minutes=1)
    # A reset already past at the last fetch does not keep the account due.
    stale, due = plan('claude', snap(ago(minutes=10), [win(40, reset_in=timedelta(hours=-1))]))
    assert (stale['reason'], due) == ('idle', False)


BACKOFF = {'failures': 2, 'backoff_until': iso(NOW + timedelta(minutes=5)), 'trigger': 'timer'}


@pytest.mark.parametrize('kind, options, activity, due', [
    ('network', {}, None, False),
    ('network', {'trigger': 'manual'}, None, True),
    ('rate_limited', {'trigger': 'manual'}, None, False),
    ('rate_limited', {'trigger': 'manual', 'ignore_backoff': True}, None, True),
    ('rate_limited', {'ignore_backoff': True}, None, False),   # judged on the plain schedule
    ('auth', {}, None, False),
    ('auth', {}, ago(minutes=1), True),       # signing in again clears an auth backoff
    ('missing', {'trigger': 'panel'}, None, False),
])
def test_backoff_wins_except_where_the_trigger_or_activity_clears_it(kind, options, activity, due):
    block, is_due = plan('claude', snap(ago(minutes=2), error_kind=kind, **BACKOFF), activity=activity, **options)
    assert is_due is due and block['failures'] == 2 and block['error_kind'] == kind
    if not activity:
        assert (block['reason'], block['next_due_at']) == ('backoff', BACKOFF['backoff_until'])


@pytest.mark.parametrize('kind, failures, retry_after, low, high', [
    ('network', 1, None, 108, 132),
    ('network', 2, None, 216, 264),
    ('other', 3, None, 432, 528),
    ('network', 9, None, 3240, 3960),           # capped at max_seconds before jitter
    ('rate_limited', 1, None, 108, 132),
    ('rate_limited', 1, 900, 900, 900),         # Retry-After wins over a shorter exponential wait
    ('rate_limited', 5, 30, 1728, 2112),        # and never shortens a longer one
    ('auth', 4, None, 3240, 3960),
    ('missing', 1, None, 77760, 95040),
])
def test_backoff_seconds_per_kind(kind, failures, retry_after, low, high):
    config, _ = schedule.resolve('claude')
    assert low <= schedule.backoff_seconds(kind, failures, retry_after, config, 'claude/a') <= high


def test_config_layers_resolve_in_order_and_invalid_values_fall_through_with_a_note():
    settings = {'schedule': {'min_interval_seconds': 90, 'jitter_fraction': 5, 'ttl_idle_seconds': 999,
                             'base_seconds': 1},
                'backoff': {'base_seconds': 60, 'max_seconds': 'x', 'min_interval_seconds': 5},
                'providers': {'claude': {'ttl_idle_seconds': 1200, 'ttl_active_seconds': -1}}}
    config, notes = schedule.resolve('claude', {'ttl_idle_seconds': 600}, settings)
    assert config['ttl_idle_seconds'] == 600                       # account block first
    assert config['ttl_active_seconds'] == 300                     # invalid provider value falls to POLICY
    assert config['min_interval_seconds'] == 90                    # quota.schedule
    assert config['jitter_fraction'] == 0.1 and config['max_seconds'] == 3600
    assert config['base_seconds'] == 60                            # backoff keys only from quota.backoff
    assert sorted(notes) == ['quota.backoff.max_seconds is invalid; using the default',
                             'quota.providers.claude.ttl_active_seconds is invalid; using the default',
                             'quota.schedule.jitter_fraction is invalid; using the default']
    codex, _ = schedule.resolve('codex', None, settings)
    assert codex['ttl_idle_seconds'] == 999 and codex['ttl_panel_seconds'] == 300
    unknown, _ = schedule.resolve('newprov')
    assert unknown == {**schedule.SCHEDULE, **schedule.BACKOFF, **schedule.DEFAULT_POLICY}
    swapped, notes = schedule.resolve('claude', None, {'schedule': {'min_interval_seconds': 900,
                                                                    'max_interval_seconds': 600}})
    assert (swapped['min_interval_seconds'], swapped['max_interval_seconds']) == (60, 21600) and notes
    block, _ = plan('claude', snap(ago(minutes=1)), settings={'schedule': {'max_parallel': 0}})
    assert block['notes'] == ['quota.schedule.max_parallel is invalid; using the default']


def touch(path, moment):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('')
    os.utime(path, (moment.timestamp(), moment.timestamp()))


def test_activity_is_stat_only_per_identity(tmp_path, monkeypatch):
    claude, codex, opencode, home = tmp_path / 'claude', tmp_path / 'codex', tmp_path / 'oc', tmp_path / 'home'
    touch(claude / 'history.jsonl', ago(hours=3))
    touch(claude / 'projects/p/session.jsonl', ago(minutes=2))
    day = NOW.astimezone()
    touch(codex / f'sessions/{day:%Y}/{day:%m}/{day:%d}/rollout.jsonl', ago(minutes=4))
    touch(opencode / 'opencode.db-wal', ago(minutes=5))
    touch(home / '.gemini/antigravity-cli/log/cli-1.log', ago(minutes=6))

    def refuse(*args, **kwargs):
        raise AssertionError('activity checks never open a file')
    monkeypatch.setattr(builtins, 'open', refuse)
    found = {name: schedule.activity(name, path, NOW, home) for name, path in (
        ('claude', claude), ('codex', codex), ('opencode', opencode), ('agy', tmp_path / 'keychain'),
        ('cursor', tmp_path / 'cursor'))}
    assert found['claude'] == pytest.approx(ago(minutes=2).timestamp())
    assert found['codex'] == pytest.approx(ago(minutes=4).timestamp())
    assert found['opencode'] == pytest.approx(ago(minutes=5).timestamp())
    assert found['agy'] is not None and found['cursor'] is None
    assert schedule.activity('claude', tmp_path / 'missing') is None
