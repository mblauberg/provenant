"""Native provider quota: registry, accounts, pace, reset events, credits and routing input (#1030)."""

import base64
from datetime import UTC, datetime, timedelta
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
from types import SimpleNamespace

import pytest


ROOT = Path(__file__).resolve().parents[1]
NOW = datetime(2026, 10, 22, 12, 0, tzinfo=UTC)
WEEK = 7 * 24 * 60
TOKEN = 'SECRET-TOKEN-VALUE'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def quota(tmp_path, monkeypatch):
    home = tmp_path / 'home'
    home.mkdir()
    monkeypatch.setenv('HOME', str(home))
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(home / '.agents'))
    for name in ('PROVENANT_QUOTA', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME'):
        monkeypatch.delenv(name, raising=False)
    module = load('provider_quota_under_test', ROOT / 'scripts/provider_quota.py')

    def refuse(*args, **kwargs):
        raise AssertionError('tests never reach a real provider, CLI or credential store')
    for name in ('run_process', 'open_process', 'urlopen', 'executable'):
        monkeypatch.setattr(module, name, refuse)
    monkeypatch.setattr(module, 'telemetry_host', lambda: 'studio')
    return module


def configure(tmp_path, value):
    path = tmp_path / 'home/.agents/config/telemetry.json'
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


def iso(moment):
    return moment.isoformat().replace('+00:00', 'Z')


def window(used, minutes, reset_in=None, name='seven_day'):
    return {'name': name, 'raw_kind': name, 'used_percent': used, 'window_minutes': minutes,
            'reset_at': iso(NOW + reset_in) if reset_in is not None else None}


def snapshot(quota, provider='codex', windows=(), observed=NOW, account='default', host='studio', **extra):
    value = {'schema': quota.SCHEMA, 'provider': provider, 'account': account, 'plan': None, 'host': host,
             'active': True, 'priority': None, 'observed_at': iso(observed), 'attempted_at': iso(observed),
             'source': 'test', 'status': 'ok', 'error': None, 'unreported': [], 'credits': None,
             'windows': [quota.normalise_window(row) for row in windows], **extra}
    if account == 'default' and quota.provider(provider):
        quota.provider(provider).default_identity().mkdir(parents=True, exist_ok=True)
    configured = next((row for row in quota.accounts() if (row.provider, row.label) == (provider, account)), None)
    identity = configured.identity_dir if configured else quota.provider(provider).default_identity()
    quota.save_snapshot(value, identity)
    return value


# Pace ---------------------------------------------------------------------------------------

def test_codex_45_percent_over_pace_is_a_deficit_that_runs_out_in_about_18_hours(quota):
    pace = quota.pace(window(64.2, WEEK, timedelta(hours=135.7)), NOW)
    assert pace['state'] == 'deficit'
    assert pace['delta_percent'] == pytest.approx(45, abs=0.5)
    assert pace['will_last_to_reset'] is False
    runs_out = quota.parse_time(pace['runs_out_at'])
    assert (runs_out - NOW).total_seconds() / 3600 == pytest.approx(18, abs=0.5)
    assert pace['expiring_headroom_percent'] == 0


def test_reserve_on_pace_exhausted_and_not_started_states(quota):
    reserve = quota.pace(window(3, 300, timedelta(hours=2), 'five_hour'), NOW)
    assert reserve['state'] == 'reserve' and reserve['will_last_to_reset'] is True
    # 3% after 3 of 5 hours projects 5% at reset, so 95% of the window expires unused.
    assert reserve['expiring_headroom_percent'] == pytest.approx(95)
    assert quota.pace(window(60, 300, timedelta(hours=2)), NOW)['state'] == 'on_pace'
    exhausted = quota.pace(window(100, 300, timedelta(hours=2)), NOW)
    assert exhausted['state'] == 'exhausted' and exhausted['runs_out_at'] == iso(NOW)
    started_on_use = quota.pace(window(0, 300, None), NOW)
    assert started_on_use == {**started_on_use, 'state': 'not_started', 'expected_used_percent': None,
                              'headroom_percent': 100, 'expiring_headroom_percent': 0}
    passed = quota.pace(window(100, 300, timedelta(hours=-1)), NOW)
    assert passed['state'] == 'exhausted' and passed['headroom_percent'] == 0


def test_missing_windows_are_unreported_never_zero(quota, monkeypatch):
    replies = iter([{'windows': [window(12, 300, timedelta(hours=1), 'five_hour'), window(20, WEEK, timedelta(days=2))]},
                    {'windows': [window(14, 300, timedelta(hours=1), 'five_hour')], 'unreported': ['seven_day_opus']}])
    monkeypatch.setattr(quota, 'fetch', lambda account, now: next(replies))
    account = quota.Account('claude', 'default', Path('/nowhere'), True)
    quota.refresh(account, NOW, force=True)
    latest = quota.refresh(account, NOW + timedelta(minutes=10), force=True)
    assert latest['unreported'] == ['seven_day_opus', 'seven_day']
    row = quota.report([latest], NOW)['accounts'][0]
    assert [window['name'] for window in row['windows']] == ['five_hour']
    assert 'missing does not mean 0%' in row['coverage_note']
    assert quota.provider('claude').parse({'five_hour': {'utilization': 1}, 'seven_day': None})['unreported'] == ['seven_day']


# Provider modules ---------------------------------------------------------------------------

def test_registry_names_one_module_per_provider_with_the_common_interface(quota):
    for name in ('claude', 'codex', 'cursor'):
        module = quota.provider(name)
        assert module.ADAPTER == name and callable(module.fetch) and callable(module.default_identity)
    assert quota.provider('agy') is None


def test_codex_payload_windows_carry_provider_names_lengths_and_reset_credits(quota):
    payload = {'rateLimits': {'planType': 'pro', 'primary': {'usedPercent': 20, 'windowDurationMins': 300,
                                                             'resetsAt': int((NOW + timedelta(hours=2)).timestamp())},
                              'secondary': {'usedPercent': 50, 'windowDurationMins': 10080,
                                            'resetsAt': int((NOW + timedelta(days=3)).timestamp())},
                              'resetCredits': 1},
               'rateLimitsByLimitId': {'gpt-spark': {'primary': {'usedPercent': 5, 'windowDurationMins': 60}}}}
    parsed = quota.provider('codex').parse(payload, NOW)
    assert parsed['plan'] == 'pro' and parsed['credits'] == {'reset_credits': 1, 'source': 'provider'}
    rows = [quota.normalise_window(row) for row in parsed['windows']]
    assert [(row['name'], row['used_percent'], row['window_minutes']) for row in rows] == [
        ('primary', 20, 300), ('secondary', 50, 10080), ('gpt-spark.primary', 5, 60)]
    assert rows[1]['window_start'] == iso(NOW + timedelta(days=3) - timedelta(days=7))


def test_codex_app_server_speaks_json_rpc_and_never_inherits_a_token(quota, monkeypatch, tmp_path):
    sent = []

    class Process:
        def __init__(self, command, **kwargs):
            sent.append((command, kwargs['env'].get('CODEX_HOME')))
            self.stdin = io.StringIO()
            self.stdout = io.StringIO('\n'.join(json.dumps(line) for line in (
                {'id': 1, 'result': {}}, {'method': 'noise'},
                {'id': 2, 'result': {'rateLimits': {'primary': {'usedPercent': 7}}}})) + '\n')
        def poll(self):
            return 0
        def wait(self, timeout=None):
            return 0
    monkeypatch.setattr(quota, 'open_process', Process)
    monkeypatch.setattr(quota, 'executable', lambda name: '/bin/' + name)
    account = quota.Account('codex', 'default', tmp_path / 'codex-home', True)
    assert quota.provider('codex').app_server(account, quota.IO, timeout=1) == {'rateLimits': {'primary': {'usedPercent': 7}}}
    assert sent == [(['/bin/codex', '-s', 'read-only', '-a', 'untrusted', 'app-server'], str(tmp_path / 'codex-home'))]


def test_codex_falls_back_to_rollout_rate_limits_already_in_the_ledger(quota, monkeypatch, tmp_path):
    db = load('telemetry_for_quota', ROOT / 'scripts/telemetry.py').connect()
    for used, stamp in ((40, NOW - timedelta(hours=2)), (45, NOW - timedelta(hours=1))):
        db.execute('INSERT INTO observations(event_id,source,host,provider,adapter,timestamp,window,used_percent,window_minutes,reset_at) '
                   'VALUES (?,?,?,?,?,?,?,?,?,?)', (f'e{used}', 'codex', 'studio', 'openai', 'codex', iso(stamp),
                                                     'secondary', used, 10080, iso(NOW + timedelta(days=2))))
    db.commit()
    monkeypatch.setattr(quota, 'executable', lambda name: None)
    account = quota.Account('codex', 'default', tmp_path / 'home/.codex', True)
    value = quota.refresh(account, NOW, force=True)
    assert value['source'] == 'codex rollout rate-limit events'
    assert value['observed_at'] == iso(NOW - timedelta(hours=1))
    assert [(row['name'], row['used_percent']) for row in value['windows']] == [('secondary', 45)]
    other = quota.refresh(quota.Account('codex', 'work', tmp_path / 'codex-work', False), NOW, force=True)
    assert other['status'] == 'unavailable' and other['windows'] == []


def test_claude_usage_endpoint_uses_the_token_in_process_only(quota, monkeypatch, tmp_path, capsys):
    identity = tmp_path / 'claude-b'
    identity.mkdir()
    (identity / '.credentials.json').write_text(json.dumps({'claudeAiOauth': {
        'accessToken': TOKEN, 'expiresAt': int((NOW + timedelta(hours=1)).timestamp() * 1000),
        'scopes': ['user:profile'], 'subscriptionType': 'max'}}))
    monkeypatch.setattr(quota.provider('claude'), 'status', lambda account, io: {'loggedIn': True,
                                                                                 'email': 'owner@example.com'})
    seen = {}

    class Response(io.BytesIO):
        def __enter__(self):
            return self
        def __exit__(self, *args):
            return False

    def opener(request, timeout):
        seen.update(url=request.full_url, headers=dict(request.header_items()))
        return Response(json.dumps({
            'five_hour': {'utilization': 3, 'resets_at': iso(NOW + timedelta(hours=4))},
            'seven_day': {'utilization': 20, 'resets_at': iso(NOW + timedelta(days=4))},
            'seven_day_opus': {'utilization': 30, 'resets_at': iso(NOW + timedelta(days=4))},
            'limits': [{'kind': 'weekly_scoped', 'percent': 10, 'group': 'seven_day',
                        'scope': {'model': {'id': 'fable'}}, 'resets_at': iso(NOW + timedelta(days=4))}]}).encode())
    monkeypatch.setattr(quota, 'urlopen', opener)
    result = quota.refresh(quota.Account('claude', 'max-b', identity, False), NOW, force=True)
    assert seen['url'] == 'https://api.anthropic.com/api/oauth/usage'
    assert seen['headers']['Authorization'] == 'Bearer ' + TOKEN
    assert seen['headers']['Anthropic-beta'] == 'oauth-2025-04-20'
    assert result['plan'] == 'max' and result['account'] == 'max-b' and result['host'] == 'studio'
    assert [(row['name'], row['window_minutes']) for row in result['windows']] == [
        ('five_hour', 300), ('seven_day', WEEK), ('seven_day_opus', WEEK), ('weekly_scoped:fable', WEEK)]
    stored = quota.cache_path('claude', 'max-b', identity_dir=identity).read_text()
    for secret in (TOKEN, 'owner@example.com'):
        assert secret not in stored and secret not in json.dumps(result)
    assert TOKEN not in capsys.readouterr().out


def test_claude_refuses_expiring_credentials_without_calling_the_endpoint(quota, monkeypatch, tmp_path):
    identity = tmp_path / 'claude-c'
    identity.mkdir()
    (identity / '.credentials.json').write_text(json.dumps({'claudeAiOauth': {
        'accessToken': TOKEN, 'expiresAt': int((NOW + timedelta(seconds=30)).timestamp() * 1000)}}))
    monkeypatch.setattr(quota.provider('claude'), 'status', lambda account, io: {'loggedIn': True})
    value = quota.refresh(quota.Account('claude', 'c', identity, False), NOW, force=True)
    assert value['status'] == 'unavailable' and 'refresh' in value['error'] and TOKEN not in json.dumps(value)


def test_cursor_session_cookie_comes_from_the_local_jwt_and_cycle_dates(quota, tmp_path, monkeypatch):
    claims = base64.urlsafe_b64encode(json.dumps({'sub': 'auth0|user_42', 'exp': int((NOW + timedelta(days=1)).timestamp())}).encode()).decode().rstrip('=')
    jwt = f'header.{claims}.signature'
    database = tmp_path / 'state ? fixture.vscdb'
    with sqlite3.connect(database) as db:
        db.execute('CREATE TABLE ItemTable(key TEXT, value TEXT)')
        db.execute('INSERT INTO ItemTable VALUES (?,?)', ('cursorAuth/accessToken', jwt))
    cursor = quota.provider('cursor')
    connect = sqlite3.connect
    opened = []
    def track(*args, **kwargs):
        connection = connect(*args, **kwargs)
        opened.append(connection)
        return connection
    monkeypatch.setattr(cursor.sqlite3, 'connect', track)
    assert cursor.session(database, NOW) == (jwt, 'user_42')
    with pytest.raises(sqlite3.ProgrammingError):
        opened[0].execute('SELECT 1')
    parsed = cursor.parse({'membershipType': 'pro', 'individualUsage': {'plan': {'used': 250, 'limit': 1000}},
                           'billingCycleStart': iso(NOW - timedelta(days=10)),
                           'billingCycleEnd': iso(NOW + timedelta(days=20))}, NOW)
    row = quota.normalise_window(parsed['windows'][0])
    assert parsed['plan'] == 'pro'
    assert (row['name'], row['used_percent'], row['window_minutes']) == ('billing_cycle', 25, 30 * 24 * 60)


def test_a_registered_provider_module_extends_quota_and_routing_without_core_changes(quota, tmp_path):
    identity = tmp_path / 'fake-home'
    identity.mkdir()
    calls = []
    fake = SimpleNamespace(
        ADAPTER='fake', default_identity=lambda: identity,
        fetch=lambda account, io, now: calls.append(account) or {
            'plan': 'team', 'source': 'fake endpoint',
            'windows': [{'name': 'monthly_tokens', 'raw_kind': 'tokens', 'used_percent': 100,
                         'window_minutes': 30 * 24 * 60, 'reset_at': iso(now + timedelta(days=2))}]})
    quota.register('fakeprov', fake)
    configure(tmp_path, {'accounts': {'fakeprov': {'team': {'identity_dir': str(identity), 'priority': 1,
                                                         'organisation': 'org-42', 'pool': {'id': 'shared'}}}}})
    account, = quota.accounts()
    assert (account.provider, account.label, account.active) == ('fakeprov', 'team', True)
    assert account.options == {'organisation': 'org-42', 'pool': {'id': 'shared'}}
    report = quota.report(quota.gather(NOW), NOW)
    row = report['accounts'][0]
    assert (row['provider'], row['plan'], row['state'], row['windows'][0]['name']) == ('fakeprov', 'team', 'exhausted', 'monthly_tokens')
    assert quota.apply_to_cooldowns({}, NOW)['fake/*']['cooling_until'] == iso(NOW + timedelta(days=2))
    assert len(calls) == 1 and calls[0].options == account.options
    assert 'organisation' not in json.dumps(report)
    assert quota.Account('fakeprov', 'default', identity).options == {}


# Cache, config and accounts ------------------------------------------------------------------

def test_cache_is_reused_within_ttl_and_disabled_quota_never_fetches(quota, monkeypatch):
    calls = []
    monkeypatch.setattr(quota, 'fetch', lambda account, now: calls.append(account) or {
        'windows': [window(10, 300, timedelta(hours=1))], 'plan': None, 'source': 'test'})
    account = quota.Account('codex', 'default', Path('/nowhere'), True)
    quota.refresh(account, NOW)
    quota.refresh(account, NOW + timedelta(minutes=4))
    assert len(calls) == 1
    quota.refresh(account, NOW + timedelta(minutes=6))
    assert len(calls) == 2
    monkeypatch.setenv('PROVENANT_QUOTA', '0')
    assert quota.refresh(account, NOW + timedelta(hours=1), force=True)['attempted_at'] == iso(NOW + timedelta(minutes=6))
    assert len(calls) == 2


@pytest.mark.parametrize(('provider_name', 'environment'), [('claude', 'CLAUDE_CONFIG_DIR'), ('codex', 'CODEX_HOME')])
def test_cache_and_routing_are_bound_to_the_selected_identity(quota, monkeypatch, tmp_path, provider_name, environment):
    first_identity = quota.provider(provider_name).default_identity()
    first_identity.mkdir(parents=True)
    calls = []
    monkeypatch.setattr(quota, 'fetch', lambda account, now: calls.append(account.identity_dir) or {
        'windows': [window(10 if account.identity_dir == first_identity else 100, 300, timedelta(hours=2))]})
    first, = quota.gather(NOW)
    assert first['windows'][0]['used_percent'] == 10
    second_identity = tmp_path / 'other-identity'
    second_identity.mkdir()
    monkeypatch.setenv(environment, str(second_identity))
    assert quota.route_states(NOW) == {}
    second, = quota.gather(NOW + timedelta(minutes=1))
    assert second['windows'][0]['used_percent'] == 100
    assert quota.route_states(NOW)[provider_name]['state'] == 'exhausted'
    monkeypatch.delenv(environment)
    cached, = quota.gather(NOW + timedelta(minutes=2))
    assert cached['windows'][0]['used_percent'] == 10
    assert quota.route_states(NOW)[provider_name]['state'] == 'reserve'
    assert calls == [first_identity, second_identity]


def test_accounts_come_from_config_and_default_to_one_discovered_identity(quota, tmp_path):
    assert quota.accounts() == []
    (tmp_path / 'home/.claude').mkdir()
    assert [(row.provider, row.label, row.active) for row in quota.accounts()] == [('claude', 'default', True)]
    configure(tmp_path, {'quota': {'enabled': False}, 'accounts': {
        'claude': {'max-a': {'identity_dir': '~/.claude', 'plan': 'Max 20x', 'priority': 1},
                   'max-b': {'identity_dir': '~/.claude-b'},
                   'retired': {'identity_dir': '~/.claude-old', 'enabled': False}},
        'agy': {'work': {'identity_dir': '~/.gemini'}}}})
    assert quota.enabled() is False
    found = {(row.provider, row.label): row for row in quota.accounts()}
    assert set(found) == {('claude', 'max-a'), ('claude', 'max-b'), ('agy', 'work')}
    assert found[('claude', 'max-a')].active and found[('claude', 'max-a')].plan == 'Max 20x'
    assert found[('claude', 'max-a')].priority == 1
    assert found[('claude', 'max-b')].identity_dir == tmp_path / 'home/.claude-b' and not found[('claude', 'max-b')].active
    configure(tmp_path, {'accounts': {'claude': {'owner@example.com': {'identity_dir': '~/.claude'}}}})
    with pytest.raises(ValueError, match='label'):
        quota.accounts()


# Reset events -------------------------------------------------------------------------------

@pytest.mark.parametrize(('previous', 'current', 'observed', 'kind'), [
    ((80, timedelta(hours=1)), (5, timedelta(days=7, hours=1)), NOW + timedelta(hours=2), 'natural'),
    ((80, timedelta(days=3)), (10, timedelta(days=3)), NOW + timedelta(hours=1), 'granted_date_kept'),
    ((80, timedelta(days=3)), (2, timedelta(days=7, hours=1)), NOW + timedelta(hours=1), 'granted_window_restarted'),
    ((40, timedelta(days=3)), (41, timedelta(days=4)), NOW + timedelta(hours=1), 'window_moved'),
    ((40, timedelta(days=3)), (0, None), NOW + timedelta(hours=1), 'not_started'),
    ((0, None), (3, timedelta(days=7)), NOW + timedelta(hours=1), 'window_started'),
    ((40, timedelta(days=3)), (41, timedelta(days=3)), NOW + timedelta(hours=1), None),
    # A local clock beyond the previous reset is not proof that the provider rolled its window.
    ((80, timedelta(days=3)), (10, timedelta(days=3)), NOW + timedelta(days=4), 'granted_date_kept'),
    ((40, timedelta(days=3)), (41, timedelta(days=3)), NOW + timedelta(days=4), None),
    # A large gap from the previous provider reset is not a natural rollover.
    ((80, timedelta(days=3)), (2, timedelta(days=14)), NOW + timedelta(hours=1), 'window_moved'),
    # A thirty-minute mismatch is not a window restarted at this observation.
    ((80, timedelta(days=3)), (2, timedelta(days=7, minutes=30)), NOW + timedelta(hours=1), 'window_moved'),
    ((80, timedelta(days=3)), (2, timedelta(days=7, minutes=58)), NOW + timedelta(hours=1), 'granted_window_restarted'),
])
def test_consecutive_observations_classify_reset_events(quota, previous, current, observed, kind):
    before = window(previous[0], WEEK, previous[1])
    after = window(current[0], WEEK, current[1])
    event = quota.classify_reset(before, after, NOW, observed)
    assert (event or {}).get('kind') == kind


def test_reset_events_are_recorded_in_the_ledger_and_pace_follows_the_latest_snapshot(quota, monkeypatch, tmp_path):
    replies = iter([[window(80, WEEK, timedelta(days=3))], [window(10, WEEK, timedelta(days=3))]])
    monkeypatch.setattr(quota, 'fetch', lambda account, now: {'windows': next(replies), 'source': 'test'})
    account = quota.Account('codex', 'default', Path('/nowhere'), True)
    quota.refresh(account, NOW, force=True)
    latest = quota.refresh(account, NOW + timedelta(hours=1), force=True)
    with sqlite3.connect(tmp_path / 'state/telemetry.sqlite3') as db:
        rows = db.execute('SELECT provider,account,host,window,kind,previous_used_percent,used_percent FROM quota_events').fetchall()
    assert rows == [('codex', 'default', 'studio', 'seven_day', 'granted_date_kept', 80, 10)]
    assert quota.report([latest], NOW + timedelta(hours=1))['accounts'][0]['windows'][0]['pace']['state'] == 'reserve'


# Credits and use-it-or-lose-it ----------------------------------------------------------------

def test_manual_reset_credits_and_redeem_advice(quota, capsys):
    assert quota.main(['credit', 'codex', 'add']) == 0
    assert quota.main(['credit', 'codex', '--account', 'default', 'add']) == 0
    assert quota.main(['credit', 'codex', 'use']) == 0
    assert quota.credits()[('codex', 'default', 'studio')]['reset_credits'] == 1
    assert quota.main(['credit', 'codex', '--account', 'spare', 'use']) == 1

    def advice(used, reset_in):
        return quota.report([snapshot(quota, 'codex', [window(used, WEEK, reset_in)])], NOW)['accounts'][0]['credit_advice']
    assert advice(40, timedelta(days=3))['action'] == 'hold'
    assert advice(100, timedelta(hours=2))['action'] == 'wait'
    assert advice(100, timedelta(days=3))['action'] == 'redeem'
    with pytest.raises(SystemExit):
        quota.main(['credit', 'codex', 'redeem'])


@pytest.mark.parametrize('host_specific', [False, True])
def test_credit_updates_preserve_host_agnostic_keys_and_prefer_exact_host(quota, monkeypatch, host_specific):
    path = quota.state_root() / 'quota/credits.json'
    current = {'claude/main/peer': {'reset_credits': 7}}
    if host_specific:
        current['claude/main/studio'] = {'reset_credits': 3}
    # Put the generic record last to also verify host-specific read precedence.
    current['claude/main'] = {'reset_credits': 2}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(current))
    assert quota.credits()[('claude', 'main', 'studio')]['reset_credits'] == (3 if host_specific else 2)
    assert quota.change_credits('claude', 'main', 1) == (4 if host_specific else 3)
    updated = json.loads(path.read_text())
    assert set(updated) == set(current)
    assert updated['claude/main/peer'] == current['claude/main/peer']
    if host_specific:
        assert updated['claude/main'] == current['claude/main']
    assert quota.credits()[('claude', 'main', 'studio')]['reset_credits'] == (4 if host_specific else 3)
    monkeypatch.setattr(quota, 'telemetry_host', lambda: 'another-host')
    assert quota.credits()[('claude', 'main', 'another-host')]['reset_credits'] == (2 if host_specific else 3)


def test_use_it_or_lose_it_prefers_the_account_whose_reset_comes_sooner(quota):
    soon = snapshot(quota, 'claude', [window(20, WEEK, timedelta(days=1))], account='max-a')
    later = snapshot(quota, 'claude', [window(20, WEEK, timedelta(days=5))], account='max-b')
    report = quota.report([later, soon], NOW)
    rows = {row['account']: row for row in report['accounts'] if row['provider'] == 'claude'}
    assert rows['max-a']['use_it_or_lose_it']['expiring_headroom_percent'] > 0
    assert rows['max-a']['use_it_or_lose_it']['reset_at'] == iso(NOW + timedelta(days=1))
    assert report['placement_preference']['claude'] == ['max-a', 'max-b']


def test_peer_readings_of_one_account_keep_the_freshest(quota):
    old = snapshot(quota, 'claude', [window(10, WEEK, timedelta(days=2))], observed=NOW - timedelta(hours=2), host='workshop')
    new = snapshot(quota, 'claude', [window(30, WEEK, timedelta(days=2))], observed=NOW - timedelta(minutes=1))
    rows = quota.report([new, old], NOW)['accounts']
    assert [(row['host'], row['windows'][0]['used_percent']) for row in rows] == [('studio', 30)]


# Routing input -------------------------------------------------------------------------------

def test_reserve_snapshot_lifts_a_stale_claude_usage_cooldown(quota):
    cooldowns = {'claude/*': {'account': 'default', 'host': 'studio', 'adapter': 'claude', 'model': '*', 'status': 'usage_limited',
                              'recorded_at': iso(NOW - timedelta(hours=1)), 'cooling_until': '2026-10-22T22:00:00Z'},
                 'claude/opus': {'account': 'default', 'host': 'studio', 'adapter': 'claude', 'model': 'opus', 'status': 'rate_limited',
                                 'recorded_at': iso(NOW - timedelta(hours=1)), 'cooling_until': '2026-10-22T22:00:00Z'}}
    snapshot(quota, 'claude', [window(3, 300, timedelta(hours=2), 'five_hour'), window(20, WEEK, timedelta(days=4))],
             observed=NOW - timedelta(minutes=5))
    assert quota.route_states(NOW)['claude']['state'] == 'reserve'
    assert quota.apply_to_cooldowns(cooldowns, NOW) == {'claude/opus': cooldowns['claude/opus']}
    snapshot(quota, 'claude', [window(3, 300, timedelta(hours=2), 'five_hour')], observed=NOW - timedelta(hours=2))
    assert quota.apply_to_cooldowns(cooldowns, NOW) == cooldowns


@pytest.mark.parametrize('windows', [
    [window(100, 300)],
    [window(25, 300)],
    [window(25, 300), window(3, 300, timedelta(hours=2), 'five_hour')],
])
def test_indefinite_quota_never_evicts_an_existing_usage_cooldown(quota, windows):
    cooldowns = {'claude/*': {'account': 'default', 'host': 'studio', 'status': 'usage_limited',
                              'recorded_at': iso(NOW - timedelta(hours=1)),
                              'cooling_until': iso(NOW + timedelta(hours=6))}}
    snapshot(quota, 'claude', windows)
    assert quota.apply_to_cooldowns(cooldowns, NOW) == cooldowns


def test_early_headroom_clears_stale_usage_cooldowns_after_reset(quota):
    cooldowns = {'claude/*': {'account': 'default', 'host': 'studio', 'status': 'usage_limited',
                              'recorded_at': iso(NOW - timedelta(hours=1)),
                              'cooling_until': iso(NOW + timedelta(hours=6))}}
    snapshot(quota, 'claude', [window(1, 300, timedelta(hours=4, minutes=59))])
    assert quota.route_states(NOW)['claude']['state'] == 'early'
    assert quota.apply_to_cooldowns(cooldowns, NOW) == {}


def test_both_claude_accounts_exhausted_block_routing_until_reset_then_lift(quota, tmp_path):
    reset = datetime(2026, 10, 28, 22, 0, tzinfo=UTC)  # Thursday 08:00 AEST
    configure(tmp_path, {'accounts': {'claude': {'max-a': {'identity_dir': '~/.claude'},
                                                 'max-b': {'identity_dir': '~/.claude-b'}}}})
    for label, active in (('max-a', True), ('max-b', False)):
        snapshot(quota, 'claude', [window(100, WEEK, reset - NOW), window(40, 300, timedelta(hours=2), 'five_hour')],
                 account=label, active=active, plan='max')
    cooled = quota.apply_to_cooldowns({}, NOW)
    assert cooled == {'claude/*': {**cooled['claude/*'], 'account': 'max-a', 'status': 'quota_exhausted',
                                   'cooling_until': iso(reset)}}
    assert quota.report(quota.snapshots(), NOW)['placement_preference']['claude'] == ['max-a', 'max-b']
    assert quota.apply_to_cooldowns({}, reset + timedelta(minutes=1)) == {}


def test_exhausted_window_cools_the_provider_until_reset_and_deficit_is_report_only(quota):
    snapshot(quota, 'codex', [window(100, 300, timedelta(hours=3), 'primary')])
    cooled = quota.apply_to_cooldowns({}, NOW)
    assert cooled['codex/*']['cooling_until'] == iso(NOW + timedelta(hours=3))
    assert cooled['codex/*']['status'] == 'quota_exhausted'
    snapshot(quota, 'codex', [window(64.2, WEEK, timedelta(hours=135.7), 'secondary')])
    assert quota.apply_to_cooldowns({}, NOW) == {}
    state = quota.route_states(NOW)['codex']
    assert state['state'] == 'deficit' and 'runs out' in state['summary']


def test_inactive_removed_and_disabled_accounts_do_not_touch_routing(quota, monkeypatch, tmp_path):
    snapshot(quota, 'codex', [window(100, 300, timedelta(hours=3))], account='spare', active=False)
    assert quota.apply_to_cooldowns({}, NOW) == {}
    snapshot(quota, 'codex', [window(100, 300, timedelta(hours=3))], account='old')
    configure(tmp_path, {'accounts': {'codex': {'work': {'identity_dir': '~/.codex'}}}})
    assert quota.apply_to_cooldowns({}, NOW) == {}
    configure(tmp_path, {})
    snapshot(quota, 'codex', [window(100, 300, timedelta(hours=3))])
    monkeypatch.setenv('PROVENANT_QUOTA', '0')
    assert quota.apply_to_cooldowns({}, NOW) == {}


# CLI ----------------------------------------------------------------------------------------

def test_quota_cli_reports_config_accounts_from_cache_without_fetching(tmp_path):
    home, state = tmp_path / 'home', tmp_path / 'state'
    (home / '.codex').mkdir(parents=True)
    (state / 'quota').mkdir(parents=True)
    observed = datetime.now(UTC)
    module = load('quota_cli_fixture', ROOT / 'scripts/provider_quota.py')
    cache_name = module.cache_path('codex', 'default', 'studio', home / '.codex').name
    (state / 'quota' / cache_name).write_text(json.dumps({
        'schema': 'provenant.quota.v1', 'provider': 'codex', 'account': 'default', 'plan': 'pro', 'host': 'studio',
        'active': True, 'priority': None, 'observed_at': iso(observed), 'attempted_at': iso(observed),
        'source': 'test', 'status': 'ok', 'error': None, 'unreported': [], 'credits': None,
        'windows': [{'name': 'secondary', 'raw_kind': 'secondary', 'used_percent': 55, 'window_minutes': WEEK,
                     'reset_at': iso(observed + timedelta(days=3)), 'window_start': None}]}))
    hosts = home / '.agents/.agent-fabric/hosts.json'
    hosts.parent.mkdir(parents=True)
    hosts.write_text(json.dumps({'schema_version': 1, 'local_host': 'studio'}))
    config = home / '.agents/config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'agy': {'work': {'identity_dir': '~/.gemini'}},
                                               'claude': {'max-b': {'identity_dir': '~/.claude-b', 'plan': 'Max'}}}}))
    # Fetching is disabled and no provider CLI is reachable, so nothing can touch a credential.
    tools = [directory for directory in os.environ['PATH'].split(os.pathsep)
             if not any(shutil.which(name, path=directory) for name in ('claude', 'codex'))]
    env = {**os.environ, 'HOME': str(home), 'AGENT_FABRIC_STATE_ROOT': str(state),
           'AGENT_FABRIC_INSTANCE_ROOT': str(home / '.agents'), 'CODEX_HOME': str(home / '.codex'),
           'CLAUDE_CONFIG_DIR': str(home / '.claude'), 'PATH': os.pathsep.join(tools), 'PROVENANT_QUOTA': '0'}
    result = subprocess.run([str(ROOT / 'scripts/provenant'), 'quota', '--json'], env=env, capture_output=True, text=True, check=False)
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report['schema'] == 'provenant.quota.report.v1' and report['enabled'] is False
    rows = {(row['provider'], row['account']): row for row in report['accounts']}
    assert set(rows) == {('agy', 'work'), ('claude', 'max-b'), ('codex', 'default')}
    codex = rows[('codex', 'default')]
    assert (codex['plan'], codex['host'], codex['state']) == ('pro', 'studio', 'on_pace')
    assert rows[('claude', 'max-b')]['status'] == 'disabled' and rows[('claude', 'max-b')]['plan'] == 'Max'
    assert rows[('agy', 'work')]['status'] == 'disabled'
    table = subprocess.run([str(ROOT / 'scripts/provenant'), 'quota'], env=env, capture_output=True, text=True, check=False)
    assert table.returncode == 0 and 'codex' in table.stdout and 'max-b' in table.stdout


def test_cache_is_keyed_by_provider_account_and_host(quota, monkeypatch):
    first = snapshot(quota, windows=[window(10, WEEK, timedelta(days=3))])
    monkeypatch.setattr(quota, 'telemetry_host', lambda: 'peer')
    assert quota.load_snapshot('codex', 'default', quota.provider('codex').default_identity()) is None
    second = snapshot(quota, host='peer', windows=[window(90, WEEK, timedelta(days=3))])
    assert quota.load_snapshot('codex', 'default', quota.provider('codex').default_identity())['host'] == 'peer'
    assert len(quota.snapshots()) == 2
    assert quota.freshest([first, second])[0]['host'] in ('studio', 'peer')


def test_provider_reset_overrides_later_usage_cooldown_and_identity_switch_rechecks_accounts(quota, tmp_path, monkeypatch):
    configure(tmp_path, {'accounts': {'claude': {'a': {'identity_dir': '~/.claude'},
                                               'b': {'identity_dir': '~/.claude-b'}}}})
    snapshot(quota, 'claude', [window(100, WEEK, timedelta(days=2))], account='a')
    snapshot(quota, 'claude', [window(3, 300, timedelta(hours=2))], account='b', active=False)
    cooldown = {'claude/*': {'account': 'a', 'host': 'studio', 'status': 'usage_limited', 'recorded_at': iso(NOW - timedelta(hours=1)),
                             'cooling_until': iso(NOW + timedelta(days=5))}}
    assert quota.apply_to_cooldowns(cooldown, NOW)['claude/*']['cooling_until'] == iso(NOW + timedelta(days=2))
    assert quota.apply_to_cooldowns(cooldown, NOW + timedelta(days=2, minutes=1)) == cooldown
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'home/.claude-b'))
    assert quota.route_states(NOW)['claude']['account'] == 'b'
    assert {row['account']: row['active'] for row in quota.gather(NOW)} == {'a': False, 'b': True}
    monkeypatch.setenv('PROVENANT_QUOTA', '0')
    assert {row['account']: row['active'] for row in quota.gather(NOW)} == {'a': False, 'b': True}
    monkeypatch.delenv('PROVENANT_QUOTA')
    assert quota.route_states(NOW)['claude']['state'] == 'reserve'
    assert quota.apply_to_cooldowns(cooldown, NOW) == cooldown  # account a is retained after switching to b


@pytest.mark.parametrize('reset_in', [None, timedelta(hours=8), timedelta(hours=-1)])
def test_exhaustion_takes_precedence_over_missing_or_inconsistent_timing(quota, reset_in):
    state = quota.pace(window(100, 300, reset_in), NOW)
    assert state['state'] == 'exhausted' and state['headroom_percent'] == 0


@pytest.mark.parametrize('used', [0, 96])
def test_reset_just_beyond_window_is_clamped_without_losing_used_quota(quota, used):
    state = quota.pace(window(used, 300, timedelta(hours=5, minutes=2)), NOW)
    assert state['state'] == 'early' and state['headroom_percent'] == 100 - used
    assert state['expected_used_percent'] == 0


@pytest.mark.parametrize(('used', 'expected'), [(0, 'not_started'), (25, 'unknown'), (100, 'exhausted')])
def test_cursor_missing_billing_cycle_end_preserves_reported_usage(quota, used, expected):
    parsed = quota.provider('cursor').parse({'individualUsage': {'plan': {'used': used, 'limit': 100}}})
    state = quota.pace(quota.normalise_window(parsed['windows'][0]), NOW)
    assert state['state'] == expected and state['headroom_percent'] == 100 - used


def test_positive_usage_with_a_far_future_or_passed_reset_has_unknown_pace(quota):
    for reset_in in (timedelta(hours=8), timedelta(hours=-1)):
        state = quota.pace(window(25, 300, reset_in), NOW)
        assert state['state'] == 'unknown' and state['headroom_percent'] == 75


def test_credit_list_is_local_advice_and_does_not_fetch(quota, capsys):
    quota.main(['credit', 'codex', '--account', 'work', 'add'])
    capsys.readouterr()
    assert quota.main(['credit', 'list', '--json']) == 0
    rows = json.loads(capsys.readouterr().out)
    assert rows == [{'provider': 'codex', 'account': 'work', 'host': 'studio', 'reset_credits': 1,
                     'updated_at': rows[0]['updated_at']}]


def test_http_quota_never_forwards_credentials_on_redirect(monkeypatch):
    import http.client
    from email.message import Message
    opened = []
    class Connection(http.client.HTTPSConnection):
        debuglevel = 0
        def __init__(self, host, **kwargs):
            opened.append(host)
            self.sock = None
        def set_debuglevel(self, value):
            pass
        def request(self, *args, **kwargs):
            pass
        def close(self):
            pass
        def getresponse(self):
            response = io.BytesIO(b'{}')
            response.status = 302 if len(opened) == 1 else 200
            response.reason = 'Found'
            response.msg = Message()
            response.msg['Location'] = 'https://untrusted.invalid/steal'
            response.code = response.status
            headers = response.msg
            response.info = lambda: headers
            return response
    monkeypatch.setattr(http.client, 'HTTPSConnection', Connection)
    monkeypatch.setenv('no_proxy', '*')
    module = load('quota_redirect_test', ROOT / 'scripts/provider_quota.py')
    with pytest.raises(module.Unavailable):
        module.http_json('https://api.anthropic.com/api/oauth/usage', {'Authorization': 'Bearer ' + TOKEN})
    assert opened == ['api.anthropic.com']


def test_codex_quota_has_one_deadline_and_reaps_a_stubborn_child(quota, monkeypatch):
    ticks = iter(range(100))
    monkeypatch.setattr(quota, 'monotonic', lambda: next(ticks), raising=False)
    monkeypatch.setattr(quota, 'executable', lambda name: '/fake/codex')
    stopped = []
    class Process:
        stdin = io.StringIO()
        stdout = io.StringIO('\n'.join(json.dumps({'method': 'noise'}) for _ in range(10)) + '\n')
        def poll(self):
            return None
        def terminate(self):
            stopped.append('terminate')
        def kill(self):
            stopped.append('kill')
        def wait(self, timeout=None):
            if 'kill' not in stopped:
                raise subprocess.TimeoutExpired('codex', timeout)
            stopped.append('reaped')
    monkeypatch.setattr(quota, 'open_process', lambda *args, **kwargs: Process())
    with pytest.raises(quota.Unavailable, match='timed out'):
        quota.provider('codex').app_server(quota.Account('codex', 'default', Path('/fake'), True), quota.IO, timeout=2)
    assert stopped == ['terminate', 'kill', 'reaped']
    assert Process.stdin.closed and Process.stdout.closed


def test_unused_future_window_is_full_and_use_it_or_lose_it_includes_unused_fraction(quota):
    future = window(0, 300, timedelta(hours=8))
    assert quota.pace(future, NOW)['state'] == 'not_started'
    assert quota.pace(future, NOW)['headroom_percent'] == 100
    value = quota.report([snapshot(quota, windows=[window(64.2, WEEK, timedelta(hours=135.7))])], NOW)
    expiring = value['accounts'][0]['use_it_or_lose_it']
    assert expiring['unused_percent'] == pytest.approx(35.8)
    assert expiring['expiring_headroom_percent'] == 0


def test_concurrent_reads_share_one_cached_fetch_and_credit_updates_are_not_lost(quota):
    from concurrent.futures import ThreadPoolExecutor
    import threading
    import time
    calls = []
    barrier = threading.Barrier(4)
    def fetch(account, io, now):
        calls.append(account.label)
        time.sleep(0.02)
        return {'windows': [window(25, 300, timedelta(hours=2))]}
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None, fetch=fetch))
    account = quota.Account('fake', 'work', Path('/fixture'), True)
    def read(_):
        barrier.wait()
        return quota.refresh(account, NOW)
    with ThreadPoolExecutor(max_workers=4) as pool:
        assert all(row['status'] == 'ok' for row in pool.map(read, range(4)))
        list(pool.map(lambda _: quota.change_credits('fake', 'work', 1), range(20)))
    assert calls == ['work']
    assert quota.credits()[('fake', 'work', 'studio')]['reset_credits'] == 20
    assert quota.cache_path('fake', 'work', identity_dir=account.identity_dir).stat().st_mode & 0o777 == 0o600


def test_configured_provider_without_a_default_requires_an_identity_directory(quota, tmp_path):
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None))
    configure(tmp_path, {'accounts': {'fake': {'work': {}}}})
    with pytest.raises(ValueError, match='identity_dir'):
        quota.accounts()


@pytest.mark.parametrize('account,host', [('other', 'studio'), ('default', 'peer'), (None, 'studio')])
def test_quota_headroom_cannot_clear_another_scope(quota, account, host):
    snapshot(quota, 'claude', [window(3, 300, timedelta(hours=2))])
    cooldown = {'claude/*': {'account': account, 'host': host, 'status': 'usage_limited',
                             'recorded_at': iso(NOW - timedelta(hours=1)),
                             'cooling_until': iso(NOW + timedelta(hours=4))}}
    assert quota.apply_to_cooldowns(cooldown, NOW) == cooldown


def test_unconfigured_selected_identity_uses_same_path_token_in_quota_and_cooldowns(quota, monkeypatch, tmp_path):
    identity = tmp_path / 'private-email@example.test'
    identity.mkdir()
    monkeypatch.setenv('CODEX_HOME', str(identity))
    monkeypatch.setattr(quota, 'fetch', lambda account, now: {'windows': [window(100, 300, timedelta(hours=2))]})
    account, = quota.accounts()
    assert account.label.startswith('path-') and 'email' not in account.label
    quota.refresh(account, NOW)
    cooldown = quota.apply_to_cooldowns({}, NOW)['codex/*']
    assert cooldown['account'] == quota.identity.scope('codex')['account']
    assert cooldown['host'] == 'studio'


def test_agy_identity_is_the_os_login_keychain_even_with_another_home(quota, monkeypatch, tmp_path):
    first = quota.identity.selected_identity('agy')
    monkeypatch.setenv('HOME', str(tmp_path / 'another-home'))
    monkeypatch.setenv('AGY_CONFIG_DIR', str(tmp_path / 'another-config'))
    assert quota.identity.selected_identity('agy') == first
    assert quota.identity.scope('agy')['account'] == 'default'
