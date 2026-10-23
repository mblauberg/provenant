"""Native provider quota: registry, accounts, pace, reset events, credits and routing input (#1030)."""

import base64
from datetime import UTC, datetime, timedelta
import hashlib
import importlib.util
import io
import json
import os
import stat
import sys
from pathlib import Path
import shutil
import sqlite3
import subprocess
from types import SimpleNamespace

import pytest

import quota_display
from tests.support.quota import NOW, configure, quota, snapshot as build_snapshot, window


from tests.support.front_door import front_door_binary

ROOT = Path(__file__).resolve().parents[1]
WEEK = 7 * 24 * 60
TOKEN = 'SECRET-TOKEN-VALUE'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def iso(moment):
    return moment.isoformat().replace('+00:00', 'Z')


def snapshot(quota, provider='codex', windows=(), observed=NOW, account='default', host='studio', **extra):
    value = build_snapshot(quota, provider, windows, observed, account, host, **extra)
    if account == 'default' and quota.provider(provider):
        quota.provider(provider).default_identity().mkdir(parents=True, exist_ok=True)
    configured = next((row for row in quota.accounts() if (row.provider, row.label) == (provider, account)), None)
    identity = configured.identity_dir if configured else quota.provider(provider).default_identity()
    quota.save_snapshot(value, identity)
    return value


# Pace ---------------------------------------------------------------------------------------

def test_above_even_pace_with_allowance_left_is_ahead_of_pace(quota):
    pace = quota.pace(window(64.2, WEEK, timedelta(hours=135.7)), NOW)
    assert pace['state'] == 'ahead_of_pace'
    assert pace['delta_percent'] == pytest.approx(45, abs=0.5)
    assert pace['will_last_to_reset'] is False
    runs_out = quota.parse_time(pace['runs_out_at'])
    assert (runs_out - NOW).total_seconds() / 3600 == pytest.approx(18, abs=0.5)
    assert pace['expiring_headroom_percent'] == 0
    assert 'ahead of pace' in pace['summary'] and 'allowance lasts about' in pace['summary']
    assert 'runs out' not in pace['summary'] and 'deficit' not in pace['summary']


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
    row = quota_display.report(vars(quota), [latest], NOW)['accounts'][0]
    assert [window['name'] for window in row['windows']] == ['five_hour']
    assert 'missing does not mean 0%' in row['coverage_note']
    assert quota.provider('claude').parse({'five_hour': {'utilization': 1}, 'seven_day': None})['unreported'] == ['seven_day']


def test_render_lists_unreported_windows_only_when_verbose_one_line_per_account(quota, monkeypatch, capsys):
    monkeypatch.setattr(quota, 'fetch', lambda account, now: {
        'windows': [window(14, 300, timedelta(hours=1), 'five_hour')], 'unreported': ['seven_day_opus', 'seven_day_sonnet']})
    account = quota.Account('claude', 'max-a', Path('/nowhere'), True)
    shown = quota_display.report(vars(quota), [quota.refresh(account, NOW, force=True)], NOW)
    quota_display.render(vars(quota), shown)
    assert 'not reported' not in capsys.readouterr().out
    quota_display.render(vars(quota), shown, verbose=True)
    out = capsys.readouterr().out
    assert 'seven_day_opus' not in out and '5-hour' in out
    assert 'claude/max-a: 2 window(s) not reported (missing does not mean 0%)' in out


def test_render_hides_repeated_limit_windows_and_reads_a_dollar_budget_plainly(quota, monkeypatch, capsys):
    scoped = window(50, WEEK, timedelta(days=2), 'weekly_scoped:fable')
    budget = {'name': 'Cloud session credits', 'used_percent': 54, 'window_minutes': None,
              'reset_at': (NOW + timedelta(days=14)).isoformat(), 'used_amount': 136, 'limit_amount': 250, 'currency': 'USD'}
    monkeypatch.setattr(quota, 'fetch', lambda account, now: {
        'windows': [window(14, 300, timedelta(hours=1), 'five_hour'), window(14, 300, timedelta(hours=1), 'session:1'),
                    scoped, budget]})
    account = quota.Account('claude', 'max-a', Path('/nowhere'), True)
    value = quota_display.report(vars(quota), [quota.refresh(account, NOW, force=True)], NOW)
    quota_display.render(vars(quota), value)
    out = capsys.readouterr().out
    assert 'session:1' not in out and 'Weekly (Fable)' in out and 'window length unreported' not in out
    assert '$114 left; resets in 14d' in out and 'unknown' not in out
    # The restated window is dropped from the report itself, so no reader counts it twice.
    assert 'session:1' not in {w['name'] for w in value['accounts'][0]['windows']}


# Provider modules ---------------------------------------------------------------------------

def test_registry_names_one_module_per_provider_with_the_common_interface(quota):
    for name in ('claude', 'codex', 'cursor', 'opencode'):
        module = quota.provider(name)
        assert module.ADAPTER == name and callable(module.fetch) and callable(module.default_identity)
    agy = quota.provider('agy')
    # agy pools are per model family, so its quota never sets a provider-wide cooldown.
    assert agy.ADAPTER is None and callable(agy.fetch) and callable(agy.account_label)


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
    # codex-cli 0.159 rejects `-a untrusted` and exits before answering.
    assert sent == [(['/bin/codex', '-s', 'read-only', '-a', 'never', 'app-server'], str(tmp_path / 'codex-home'))]


def test_codex_reports_its_credit_balance_and_banked_resets_with_expiry(quota):
    # The shape `account/rateLimits/read` returned from codex-cli 0.159, identifiers removed.
    expires = int((NOW + timedelta(days=16)).timestamp())
    payload = {'rateLimits': {'primary': {'usedPercent': 77, 'windowDurationMins': WEEK},
                              'credits': {'hasCredits': True, 'unlimited': False, 'balance': '27406.2146797500'},
                              'planType': 'prolite'},
               'rateLimitResetCredits': {'availableCount': 1, 'credits': [
                   {'id': 'opaque', 'resetType': 'codexRateLimits', 'status': 'available', 'grantedAt': 1,
                    'expiresAt': expires, 'title': 'Full reset'},
                   {'id': 'spent', 'resetType': 'codexRateLimits', 'status': 'redeemed', 'grantedAt': 1,
                    'expiresAt': expires + 1}]}}
    parsed = quota.provider('codex').parse(payload, NOW)
    assert parsed['credits'] == {'reset_credits': 1, 'source': 'provider'}
    assert quota.normalise_allowance(parsed['allowance']) is None
    allowed = quota.provider('codex').parse({**payload, 'ordinaryUsageAllowed': True,
                                             'rateLimits': {**payload['rateLimits'], 'spendControlReached': False}})
    assert quota.normalise_allowance(allowed['allowance']) == {'ordinary_usage_allowed': True,
                                                               'spend_control_reached': False}
    assert quota.normalise_resets(parsed['resets']) == {'available': 1, 'expires_at': [iso(NOW + timedelta(days=16))],
                                                        'source': 'provider'}
    assert quota.normalise_usage_credits(parsed['usage_credits']) == [{
        'kind': 'credits', 'balance': 27406.21, 'unlimited': False, 'currency': None, 'unit': 'credits',
        'expires_at': None, 'source': 'provider'}]
    # Rollout events spell the balance in snake case; no balance and no count mean nothing to show.
    assert quota.provider('codex').parse({'rate_limits': {}, 'credits': None})['usage_credits'] is None
    empty = quota.provider('codex').parse({'rateLimits': {'credits': {'has_credits': False, 'balance': '0'}},
                                           'rateLimitResetCredits': {'availableCount': 0, 'credits': None}})
    assert empty['usage_credits'] is None and quota.normalise_resets(empty['resets']) is None


def test_claude_usage_endpoint_uses_the_token_in_process_only(quota, monkeypatch, tmp_path, capsys):
    identity = tmp_path / 'claude-b'
    identity.mkdir()
    (identity / '.credentials.json').write_text(json.dumps({'claudeAiOauth': {
        'accessToken': TOKEN, 'expiresAt': int((NOW + timedelta(hours=1)).timestamp() * 1000),
        'scopes': ['user:profile'], 'subscriptionType': 'max', 'rateLimitTier': 'default_claude_max_20x'}}))
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


def test_claude_dollar_windows_and_usage_credit_balance(quota):
    claude = quota.provider('claude')
    parsed = claude.parse({'iguana_necktie': {'utilization': 54.5, 'resets_at': iso(NOW + timedelta(days=14)),
                                              'limit_dollars': 250, 'used_dollars': 136.29385},
                           'five_hour': {'utilization': 10, 'limit_dollars': None, 'used_dollars': None},
                           'spend': {'used': {'amount_minor': 0, 'currency': 'USD', 'exponent': 2}, 'balance': None}})
    rows = {row['name']: quota.normalise_window(row) for row in parsed['windows']}
    assert (rows['iguana_necktie']['used_amount'], rows['iguana_necktie']['limit_amount']) == (136.29, 250)
    assert rows['iguana_necktie']['currency'] == 'USD' and 'limit_amount' not in rows['five_hour']
    # Every account seen so far reports a null balance, so none is shown.
    assert parsed['usage_credits'] is None
    balance = claude.usage_credits({'balance': {'amount_minor': 4250, 'currency': 'USD', 'exponent': 2}})
    assert quota.normalise_usage_credits(balance)[0] == {
        'kind': 'usage_credits', 'balance': 42.5, 'unlimited': False, 'currency': 'USD', 'unit': None,
        'expires_at': None, 'source': 'provider'}


def claude_limit(kind, group, percent, resets_at, active=True, scope=None):
    return {'kind': kind, 'group': group, 'percent': percent, 'severity': 'normal', 'resets_at': resets_at,
            'scope': scope, 'is_active': active}


def test_claude_limits_take_the_length_of_the_named_window_they_repeat(quota, monkeypatch):
    # Live anese and pro shapes: `limits` repeat named windows without a length.
    session_reset, weekly_reset, budget_reset, pro_reset = (iso(NOW + timedelta(hours=3)),
                                                              iso(NOW + timedelta(days=6)),
                                                              iso(NOW + timedelta(days=14)),
                                                              iso(NOW + timedelta(days=3)))
    budget = {'utilization': 54.51754, 'resets_at': budget_reset, 'limit_dollars': 250,
              'used_dollars': 136.29385, 'remaining_dollars': 113.70615}
    anese = {'five_hour': {'utilization': 10.0, 'resets_at': session_reset},
             'seven_day': {'utilization': 3.0, 'resets_at': weekly_reset},
             'iguana_necktie': budget, 'seven_day_opus': None,
             'limits': [claude_limit('session', 'session', 10, session_reset),
                        claude_limit('weekly_all', 'weekly', 3, weekly_reset, active=False),
                        claude_limit('weekly_scoped', 'weekly', 0, weekly_reset, active=False,
                                     scope={'model': {'id': 'fable', 'display_name': 'Fable'}, 'surface': None})]}
    pro = {'five_hour': {'utilization': 0.0, 'resets_at': None},
           'seven_day': {'utilization': 48.0, 'resets_at': pro_reset},
           'limits': [claude_limit('session', 'session', 0, None, active=False),
                      claude_limit('weekly_all', 'weekly', 48, pro_reset)]}
    claude = quota.provider('claude')
    lengths = {name: {row['name']: row['window_minutes'] for row in claude.parse(payload)['windows']}
               for name, payload in (('anese', anese), ('pro', pro))}
    assert lengths['anese'] == {'five_hour': 300, 'seven_day': WEEK, 'iguana_necktie': None, 'session:1': 300}
    assert lengths['pro'] == {'five_hour': 300, 'seven_day': WEEK, 'weekly_all:2': WEEK}
    replies = {'anese': anese, 'pro': pro}
    monkeypatch.setattr(quota, 'fetch', lambda account, _now: claude.parse(replies[account.label]))
    values = [quota.refresh(quota.Account('claude', label, Path('/nowhere') / label, False), NOW, force=True)
              for label in replies]
    states = {row['account']: row['state'] for row in quota_display.report(vars(quota), values, NOW)['accounts']}
    assert states == {'anese': 'reserve', 'pro': 'reserve'}
    # An entry that repeats no named window keeps no length, is shown, and never sets state.
    unmatched = {**pro, 'limits': [claude_limit('weekly_all', 'weekly', 90, iso(NOW + timedelta(days=4)))]}
    row = next(row for row in claude.parse(unmatched)['windows'] if row['name'] == 'weekly_all:1')
    assert row['window_minutes'] is None and row['advisory'] is True
    replies['pro'] = unmatched
    value = quota.refresh(quota.Account('claude', 'pro', Path('/nowhere/pro'), False), NOW, force=True)
    account = quota_display.report(vars(quota), [value], NOW)['accounts'][0]
    assert 'weekly_all:1' in {window['name'] for window in account['windows']} and account['state'] == 'reserve'


def test_claude_refuses_expiring_credentials_without_calling_the_endpoint(quota, monkeypatch, tmp_path):
    identity = tmp_path / 'claude-c'
    identity.mkdir()
    (identity / '.credentials.json').write_text(json.dumps({'claudeAiOauth': {
        'accessToken': TOKEN, 'expiresAt': int((NOW + timedelta(seconds=30)).timestamp() * 1000)}}))
    monkeypatch.setattr(quota.provider('claude'), 'status', lambda account, io: {'loggedIn': True})
    value = quota.refresh(quota.Account('claude', 'c', identity, False), NOW, force=True)
    assert value['status'] == 'unavailable' and 'refresh' in value['error'] and TOKEN not in json.dumps(value)


@pytest.mark.parametrize('name, suffixed', [('.claude', False), ('.claude-b', True)])
def test_claude_keychain_item_follows_the_config_directory(quota, monkeypatch, tmp_path, name, suffixed):
    identity = tmp_path / 'home' / name
    identity.mkdir()
    claude = quota.provider('claude')
    monkeypatch.setattr(claude, 'status', lambda account, io: {'loggedIn': True})
    monkeypatch.setattr(claude.sys, 'platform', 'darwin')
    seen = []

    def security(argv, **kwargs):
        seen.append(argv)
        return subprocess.CompletedProcess(argv, 44, b'', b'')
    monkeypatch.setattr(quota, 'run_process', security)
    value = quota.refresh(quota.Account('claude', 'x', identity, False), NOW, force=True)
    digest = hashlib.sha256(str(identity).encode()).hexdigest()[:8]
    expected = 'Claude Code-credentials' + (f'-{digest}' if suffixed else '')
    assert [argv[argv.index('-s') + 1] for argv in seen] == [expected]
    assert value['status'] == 'unavailable'


def test_claude_on_linux_reads_only_the_credentials_file_and_never_calls_security(quota, monkeypatch, tmp_path):
    claude = quota.provider('claude')
    monkeypatch.setattr(claude, 'status', lambda account, io: {'loggedIn': True})
    monkeypatch.setattr(claude.sys, 'platform', 'linux')
    calls = []
    monkeypatch.setattr(quota, 'run_process', lambda argv, **kwargs: calls.append(argv))
    missing = tmp_path / 'claude-missing'
    missing.mkdir()
    value = quota.refresh(quota.Account('claude', 'x', missing, False), NOW, force=True)
    assert value['status'] == 'unavailable' and value['error'] == 'Claude credentials are not available'
    present = tmp_path / 'claude-present'
    present.mkdir()
    (present / '.credentials.json').write_text(json.dumps({'claudeAiOauth': {
        'accessToken': TOKEN, 'expiresAt': int((NOW + timedelta(seconds=30)).timestamp() * 1000)}}))
    value = quota.refresh(quota.Account('claude', 'y', present, False), NOW, force=True)
    assert value['status'] == 'unavailable' and 'refresh' in value['error'] and TOKEN not in json.dumps(value)
    assert calls == []


def test_claude_keychain_service_matches_a_known_claude_code_name(quota):
    # Claude Code's item for CLAUDE_CONFIG_DIR=/Users/user/.claude-pro, observed on a real Mac.
    assert quota.provider('claude').keychain_service(Path('/Users/user/.claude-pro')) == 'Claude Code-credentials-90930409'


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
    assert cursor.session(database, NOW, quota.IO) == (jwt, 'user_42')
    with pytest.raises(sqlite3.ProgrammingError):
        opened[0].execute('SELECT 1')
    parsed = cursor.parse({'membershipType': 'pro', 'individualUsage': {'plan': {'used': 250, 'limit': 1000}},
                           'billingCycleStart': iso(NOW - timedelta(days=10)),
                           'billingCycleEnd': iso(NOW + timedelta(days=20))}, NOW)
    row = quota.normalise_window(parsed['windows'][0])
    assert parsed['plan'] == 'pro'
    assert (row['name'], row['used_percent'], row['window_minutes']) == ('billing_cycle', 25, 30 * 24 * 60)


def opencode_account(quota, tmp_path, v1=(), v2=(), plan=None, **options):
    """Rows are (age, cost, provider) for v1 `message` and v2 `session_message`; errors are (age, body)."""
    home = tmp_path / 'oc'
    home.mkdir(exist_ok=True)
    ms = lambda age: int((NOW - age).timestamp() * 1000)
    with sqlite3.connect(home / 'opencode.db') as db:
        db.execute('CREATE TABLE IF NOT EXISTS message (id text, time_created integer, data text)')
        db.execute('CREATE TABLE IF NOT EXISTS session_message (id text, type text, time_created integer, data text)')
        for age, cost, provider in v1:
            db.execute('INSERT INTO message VALUES (?,?,?)', (f'a{age}{cost}', ms(age),
                                                              json.dumps({'providerID': provider, 'cost': cost})))
        for age, cost, provider in v2:
            data = {'model': {'providerID': provider, 'id': 'm'}, 'cost': cost}
            if isinstance(cost, str):
                data = {'model': {'providerID': provider}, 'error': {'type': 'provider.quota',
                                                                      'response': {'body': cost}}}
            db.execute('INSERT INTO session_message VALUES (?,?,?,?)',
                       (f'b{age}{provider}{len(str(cost))}', 'assistant', ms(age), json.dumps(data)))
    return quota.Account('opencode', 'go', home, True, plan=plan, options=options)


def opencode_rows(quota, account):
    fetched = quota.provider('opencode').fetch(account, quota.IO, NOW)
    return {w['name']: w for w in fetched['windows']}, fetched


LIMIT_ERROR = json.dumps({'error': {'type': 'GoUsageLimitError'}, 'metadata': {'limitName': 'weekly'}})
hour, day = timedelta(hours=1), timedelta(days=1)


def test_opencode_sums_both_message_stores_against_plan_wide_calibrated_caps(quota, tmp_path):
    v1 = ((hour, 0.5, 'opencode-go'), (3 * day, 2.0, 'opencode-go'), (40 * day, 99, 'opencode-go'),
          (hour, 5, 'opencode'), (-2 * hour, 50, 'opencode-go'))
    v2 = ((hour, 1.1, 'opencode-go'), (3 * day, 2.4, 'opencode-go'), (20 * day, 8.0, 'opencode-go'),
          (hour, 7, 'opencode'))
    windows, fetched = opencode_rows(quota, opencode_account(quota, tmp_path, v1, v2))
    # Go: $32 monthly, $16 weekly (50%), $6.4 for 5 hours (20%).
    assert {name: w['used_percent'] for name, w in windows.items()} == pytest.approx(
        {'five_hour': 1.6 / 6.4 * 100, 'seven_day': 6.0 / 16 * 100, 'thirty_day': 14.0 / 32 * 100})
    assert (windows['five_hour']['window_minutes'], windows['seven_day']['window_minutes']) == (300, WEEK)
    assert fetched['unreported'] == [] and windows['seven_day']['reset_at'] is None
    assert 'this machine only' in fetched['source'] and 'assuming Go plan' in fetched['source']


def test_opencode_counts_a_row_copied_into_both_stores_once(quota, tmp_path):
    account = opencode_account(quota, tmp_path, v1=((hour, 3.2, 'opencode-go'),))
    with sqlite3.connect(account.identity_dir / 'opencode.db') as db:
        db.execute("INSERT INTO session_message SELECT id,'assistant',time_created,"
                   "json_object('model',json_object('providerID','opencode-go'),'cost',3.2) FROM message")
    windows, _ = opencode_rows(quota, account)
    assert windows['thirty_day']['used_percent'] == pytest.approx(10.0)


def test_opencode_monthly_cap_is_configurable_and_other_plans_need_it(quota, tmp_path):
    v2 = ((hour, 4.0, 'opencode-go'),)
    windows, _ = opencode_rows(quota, opencode_account(quota, tmp_path, v2=v2, limits={'monthly_usd': 80}))
    assert windows['thirty_day']['used_percent'] == pytest.approx(5.0)
    other = quota.Account('opencode', 'plus', tmp_path / 'oc', True, plan='Go Plus')
    windows, fetched = opencode_rows(quota, other)
    assert windows == {} and fetched['unreported'] == ['five_hour', 'seven_day', 'thirty_day']


def test_opencode_usage_limit_error_marks_that_window_exhausted_until_it_has_passed(quota, tmp_path):
    account = opencode_account(quota, tmp_path, v2=((2 * day, LIMIT_ERROR, 'opencode-go'),
                                                    (3 * day, 1.0, 'opencode-go')))
    windows, _ = opencode_rows(quota, account)
    assert windows['seven_day']['used_percent'] == 100
    assert quota.parse_time(windows['seven_day']['reset_at']) == NOW - 2 * day + 7 * day
    assert windows['thirty_day']['used_percent'] < 10 and windows['thirty_day']['reset_at'] is None
    (tmp_path / 'old').mkdir()
    old = opencode_account(quota, tmp_path / 'old', v2=((8 * day, LIMIT_ERROR, 'opencode-go'),))
    windows, _ = opencode_rows(quota, old)
    assert windows['seven_day']['used_percent'] == 0


def test_opencode_error_is_superseded_by_a_later_successful_paid_turn(quota, tmp_path):
    account = opencode_account(quota, tmp_path, v2=((2 * day, LIMIT_ERROR, 'opencode-go'),
                                                    (day, 0.5, 'opencode-go')))
    windows, _ = opencode_rows(quota, account)
    assert windows['seven_day']['used_percent'] < 10 and windows['seven_day']['reset_at'] is None


@pytest.mark.parametrize('anchor, now, month, week', [
    ('2026-01-31T00:00:00Z', '2026-03-05T12:00:00Z', ('2026-02-28', '2026-03-31'), ('2026-02-28', '2026-03-07')),
    ('2026-01-31T00:00:00Z', '2026-03-31T00:00:00Z', ('2026-03-31', '2026-04-30'), ('2026-03-28', '2026-04-04')),
    ('2027-12-31T06:00:00Z', '2028-02-29T07:00:00Z', ('2028-02-29', '2028-03-31'), ('2028-02-25', '2028-03-03')),
    ('2026-10-18T00:00:00Z', '2026-10-22T00:40:00Z', ('2026-10-18', '2026-11-18'), ('2026-10-18', '2026-10-25')),
    ('2026-10-18T00:00:00Z', '2026-12-31T23:59:00Z', ('2026-12-18', '2027-01-18'), ('2026-12-27', '2027-01-03')),
])
def test_opencode_fixed_cycles_follow_the_anchor_through_short_months(quota, anchor, now, month, week):
    opencode = quota.provider('opencode')
    when = quota.parse_time(now)
    frames = {name: (start, end) for name, start, end, _ in opencode.cycles(when, quota.parse_time(anchor))}
    day = lambda moment: moment.date().isoformat()
    assert tuple(map(day, frames['thirty_day'])) == month and tuple(map(day, frames['seven_day'])) == week
    assert frames['five_hour'] == (when - timedelta(hours=5), None)
    assert all(start <= when < end for start, end in (frames['thirty_day'], frames['seven_day']))


def test_opencode_weekly_anchor_offsets_only_the_weekly_cycle(quota):
    opencode = quota.provider('opencode')
    when, anchor = quota.parse_time('2026-10-22T00:00:00Z'), quota.parse_time('2026-10-17T23:20:00Z')
    frames = {name: (start, end) for name, start, end, _ in
              opencode.cycles(when, anchor, quota.parse_time('2026-10-18T23:20:00Z'))}
    assert frames['thirty_day'] == (anchor, quota.parse_time('2026-11-17T23:20:00Z'))
    assert frames['seven_day'] == (quota.parse_time('2026-10-18T23:20:00Z'), quota.parse_time('2026-10-25T23:20:00Z'))


def test_opencode_anchor_sums_the_current_cycle_and_a_limit_error_expires_at_the_boundary(quota, tmp_path):
    anchor = (NOW - 2 * day).isoformat()   # the weekly and monthly cycles began two days ago
    v2 = ((3 * day, 9.0, 'opencode-go'), (day, 4.0, 'opencode-go'), (hour, 4.0, 'opencode-go'))
    windows, fetched = opencode_rows(quota, opencode_account(quota, tmp_path, v2=v2, limits={'cycle_anchor': anchor}))
    assert windows['seven_day']['used_percent'] == pytest.approx(50.0)
    assert windows['thirty_day']['used_percent'] == pytest.approx(25.0)
    assert windows['five_hour']['reset_at'] is None and windows['five_hour']['window_minutes'] == 300
    assert quota.parse_time(windows['seven_day']['reset_at']) == NOW + 5 * day
    assert 'fixed cycles' in fetched['source']
    # An error in this cycle holds until the next boundary; one before the anchor is expired.
    (tmp_path / 'f').mkdir()
    failed = opencode_account(quota, tmp_path / 'f', v2=((day, LIMIT_ERROR, 'opencode-go'),), limits={'cycle_anchor': anchor})
    windows, _ = opencode_rows(quota, failed)
    assert windows['seven_day']['used_percent'] == 100
    assert quota.parse_time(windows['seven_day']['reset_at']) == NOW + 5 * day
    (tmp_path / 'g').mkdir()
    old = opencode_account(quota, tmp_path / 'g', v2=((3 * day, LIMIT_ERROR, 'opencode-go'),), limits={'cycle_anchor': anchor})
    assert opencode_rows(quota, old)[0]['seven_day']['used_percent'] == 0


def test_opencode_future_success_does_not_clear_a_limit_error(quota, tmp_path):
    account = opencode_account(quota, tmp_path, v2=((day, LIMIT_ERROR, 'opencode-go'),
                                                    (-day, 1.0, 'opencode-go')))
    assert opencode_rows(quota, account)[0]['seven_day']['used_percent'] == 100


@pytest.mark.parametrize('body', ['{"error":{"type":"GoUsageLimitError"},"metadata":1}', '[1]', 'null', '{"error":3}'])
def test_opencode_malformed_error_bodies_never_abort_the_reading(quota, tmp_path, body):
    account = opencode_account(quota, tmp_path, v2=((hour, body, 'opencode-go'), (hour, 1.6, 'opencode-go')))
    windows, fetched = opencode_rows(quota, account)
    assert windows['five_hour']['used_percent'] == pytest.approx(1.6 / 6.4 * 100)
    assert fetched['unreported'] == []


def test_opencode_legacy_only_store_still_reports_spend(quota, tmp_path):
    home = tmp_path / 'oc'
    home.mkdir()
    with sqlite3.connect(home / 'opencode.db') as db:
        db.execute('CREATE TABLE message (id text, time_created integer, data text)')
        db.execute('INSERT INTO message VALUES (?,?,?)', ('x', int((NOW - hour).timestamp() * 1000),
                                                          json.dumps({'providerID': 'opencode-go', 'cost': 3.2})))
    windows, fetched = opencode_rows(quota, quota.Account('opencode', 'go', home, True))
    assert windows['thirty_day']['used_percent'] == pytest.approx(10.0) and fetched['unreported'] == []


def test_opencode_unreadable_store_is_unreported_never_zero(quota, tmp_path):
    windows, fetched = opencode_rows(quota, quota.Account('opencode', 'go', tmp_path / 'missing', True))
    assert windows == {} and fetched['unreported'] == ['five_hour', 'seven_day', 'thirty_day']


# Recorded from `agy -p /usage --output-format json` on 2026-10-22; descriptions shortened and
# the five-hour reset moved after NOW.
AGY_USAGE = {'conversation_id': '', 'status': 'SUCCESS', 'response': '...', 'command': {'name': 'usage', 'data': {
    'description': 'Within each group, models share a weekly limit and a 5-hour limit.',
    'groups': [{'name': 'Gemini Models', 'description': 'Models within this group: Gemini Flash, Gemini Pro', 'buckets': [
                   {'id': 'gemini-weekly', 'name': 'Weekly Limit Remaining', 'window': 'weekly',
                    'remaining_fraction': 0, 'reset_time': iso(NOW + timedelta(days=2))},
                   {'id': 'gemini-5h', 'name': 'Five Hour Limit Remaining', 'window': '5h', 'disabled': True,
                    'remaining_fraction': 1}]},
               {'name': 'Claude and GPT models', 'description': 'Models within this group: Claude Opus, Claude Sonnet, GPT-OSS',
                'buckets': [
                   {'id': '3p-weekly', 'name': 'Weekly Limit Remaining', 'window': 'weekly',
                    'remaining_fraction': 0.9330692291259766, 'reset_time': iso(NOW + timedelta(days=6))},
                   {'id': '3p-5h', 'name': 'Five Hour Limit Remaining', 'window': '5h',
                    'remaining_fraction': 0.8661383986473083, 'reset_time': iso(NOW + timedelta(hours=2))}]}]}}}
AGY_EMAIL = 'second.account@example.com'


def agy_home(quota, monkeypatch, tmp_path, *emails):
    """An agy state directory whose logs record these sign-ins, oldest first, on a stand-in Keychain."""
    keychain = tmp_path / 'login.keychain-db'
    keychain.write_text('')
    original = quota.identity.default_identity
    monkeypatch.setattr(quota.identity, 'default_identity', lambda name: keychain if name == 'agy' else original(name))
    logs = tmp_path / 'home/.gemini/antigravity-cli/log'
    logs.mkdir(parents=True)
    for index, email in enumerate(emails):
        line = f'I1022 08:39:53.180653 server_oauth.go:229] OAuth: authenticated successfully as {email}\n' if email else ''
        (logs / f'cli-20261022_08{index:02d}00.log').write_text('I1022 startup\n' + line)
    return keychain


def agy_cli(monkeypatch, quota, email=AGY_EMAIL, payload=AGY_USAGE, output=None):
    seen = []

    class Process:
        def __init__(self, argv, **kwargs):
            seen.append((argv, kwargs))
            Path(argv[argv.index('--log-file') + 1]).write_text(f'OAuth: authenticated successfully as {email}\n')
            self.stdout = io.BytesIO(output if output is not None else json.dumps(payload).encode())
            self.returncode = 0
            seen.append(self)
        def kill(self):
            self.returncode = -9
        def wait(self, timeout=None):
            return self.returncode
    monkeypatch.setattr(quota, 'executable', lambda name: '/bin/' + name)
    monkeypatch.setattr(quota, 'open_process', Process)
    return seen


def test_agy_usage_reports_each_pool_window_and_skips_disabled_buckets(quota):
    parsed = quota.provider('agy').parse(AGY_USAGE, NOW)
    rows = [quota.normalise_window(row) for row in parsed['windows']]
    assert [(row['name'], row['raw_kind'], row['used_percent'], row['window_minutes']) for row in rows] == [
        ('gemini.weekly', 'gemini-weekly', 100, WEEK), ('claude.weekly', '3p-weekly', 6.69, WEEK),
        ('claude.5h', '3p-5h', 13.39, 300)]
    assert rows[0]['reset_at'] == iso(NOW + timedelta(days=2))
    assert parsed['unreported'] == ['gemini.5h']


@pytest.mark.parametrize('fraction', [True, False, -0.1, 1.5, 10 ** 400, float('nan'), '0.5', None])
def test_agy_invalid_remaining_fractions_are_unreported_never_0_or_100_percent(quota, fraction):
    payload = {'command': {'data': {'groups': [{'name': 'Claude and GPT models', 'buckets': [
        {'id': '3p-weekly', 'window': 'weekly', 'remaining_fraction': fraction,
         'reset_time': iso(NOW + timedelta(days=6))}]}]}}}
    assert quota.provider('agy').parse(payload, NOW) == {'windows': [], 'unreported': ['claude.weekly']}


def test_agy_output_is_read_within_the_response_limit(quota, monkeypatch, tmp_path):
    agy_home(quota, monkeypatch, tmp_path, AGY_EMAIL)
    monkeypatch.setattr(quota.IO, 'MAX_RESPONSE_BYTES', 64)
    seen = agy_cli(monkeypatch, quota, output=b'{"status": "SUCCESS", "padding": "' + b'x' * 10_000 + b'"}')
    account, = [account for account in quota.accounts() if account.provider == 'agy']
    value = quota.refresh(account, NOW, force=True)
    assert value['status'] == 'unavailable' and value['error'] == 'agy quota request failed'
    (argv, kwargs), process = seen
    # The read stops one byte past the limit and the oversized CLI is killed.
    assert kwargs['stdout'] == quota.PIPE and process.stdout.closed and process.returncode == -9


def test_agy_names_the_signed_in_account_by_a_fingerprint_from_its_newest_log(quota, monkeypatch, tmp_path):
    assert not any(account.provider == 'agy' for account in quota.accounts())
    agy_home(quota, monkeypatch, tmp_path, 'first.account@example.com', AGY_EMAIL, None)
    account, = [account for account in quota.accounts() if account.provider == 'agy']
    agy = quota.provider('agy')
    assert account.active and account.label == agy.fingerprint(AGY_EMAIL) != agy.fingerprint('first.account@example.com')
    assert quota.LABEL.fullmatch(account.label) and 'example' not in account.label
    assert agy.fingerprint(' Second.Account@Example.com ') == account.label


def test_agy_quota_comes_from_print_mode_and_never_stores_the_address(quota, monkeypatch, tmp_path, capsys):
    agy_home(quota, monkeypatch, tmp_path, AGY_EMAIL)
    seen = agy_cli(monkeypatch, quota)
    account, = [account for account in quota.accounts() if account.provider == 'agy']
    value = quota.refresh(account, NOW, force=True)
    (argv, kwargs), _ = seen
    assert argv[:5] == ['/bin/agy', '-p', '/usage', '--output-format', 'json'] and kwargs['stdin'] == quota.DEVNULL
    assert not Path(argv[-1]).exists()
    assert value['status'] == 'ok' and value['source'] == 'agy print-mode /usage'
    assert [row['name'] for row in value['windows']] == ['gemini.weekly', 'claude.weekly', 'claude.5h']
    report = quota_display.report(vars(quota), [value], NOW)
    # Gemini is exhausted but the Claude pool is not, so the account stays usable; quota sets no cooldown.
    assert report['accounts'][0]['state'] == 'reserve' and quota.apply_to_cooldowns({}, NOW) == {}
    stored = quota.cache_path('agy', account.label, identity_dir=account.identity_dir).read_text()
    for text in (stored, json.dumps(report), capsys.readouterr().out):
        assert AGY_EMAIL not in text and 'second.account' not in text


@pytest.mark.parametrize('email, payload, error', [
    ('other.account@example.com', AGY_USAGE, 'different account'),
    (AGY_EMAIL, {'status': 'ERROR'}, 'sign in'),
    (AGY_EMAIL, {'status': 'SUCCESS', 'command': {'data': {'groups': []}}}, 'no quota')])
def test_agy_refuses_a_reading_for_another_account_or_without_quota(quota, monkeypatch, tmp_path, email, payload, error):
    agy_home(quota, monkeypatch, tmp_path, AGY_EMAIL)
    agy_cli(monkeypatch, quota, email, payload)
    account, = [account for account in quota.accounts() if account.provider == 'agy']
    value = quota.refresh(account, NOW, force=True)
    assert value['status'] == 'unavailable' and error in value['error'] and value['windows'] == []


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
    report = quota_display.report(vars(quota), quota.gather(NOW), NOW)
    row = report['accounts'][0]
    assert (row['provider'], row['plan'], row['state'], row['windows'][0]['name']) == ('fakeprov', 'team', 'exhausted', 'monthly_tokens')
    assert quota.apply_to_cooldowns({}, NOW)['fake/*']['cooling_until'] == iso(NOW + timedelta(days=2))
    assert len(calls) == 1 and calls[0].options == account.options
    assert 'organisation' not in json.dumps(report)
    assert quota.Account('fakeprov', 'default', identity).options == {}


# Cache, config and accounts ------------------------------------------------------------------

def test_cache_is_reused_within_ttl_and_disabled_quota_never_fetches(quota, monkeypatch, tmp_path):
    # With the schedule off, the cache period alone gates fetches, exactly as before the schedule.
    configure(tmp_path, {'quota': {'schedule': {'enabled': False}}})
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


@pytest.mark.parametrize(('provider_name', 'environment'), [
    ('claude', 'CLAUDE_CONFIG_DIR'), ('codex', 'CODEX_HOME'), ('opencode', 'XDG_DATA_HOME')])
def test_configured_accounts_without_identity_dir_keep_default_directory(
        quota, tmp_path, monkeypatch, provider_name, environment):
    default = quota.identity.default_identity(provider_name)
    monkeypatch.setenv(environment, str(tmp_path / 'alternate'))
    alternate = quota.identity.selected_identity(provider_name)
    configure(tmp_path, {'accounts': {provider_name: {'a': {}, 'b': {'identity_dir': str(alternate)}}}})
    rows = {row.label: row for row in quota.accounts()}
    assert rows['a'].identity_dir == default and not rows['a'].active
    assert rows['b'].identity_dir == alternate and rows['b'].active

    monkeypatch.delenv(environment)
    assert {row.label: row.active for row in quota.accounts()} == {'a': True, 'b': False}

    configure(tmp_path, {'accounts': {provider_name: {'a': {}, 'b': {'identity_dir': str(default)}}}})
    assert {row.label: row.active for row in quota.accounts()} == {'a': False, 'b': True}

    monkeypatch.setenv(environment, str(tmp_path / 'alternate'))
    configure(tmp_path, {'accounts': {provider_name: {'a': {}}}})
    account, = quota.accounts()
    assert account.identity_dir == default and not account.active
    assert quota.identity.scope(provider_name)['account'].startswith('path-')


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


# Credits and use-it-or-lose-it ----------------------------------------------------------------

def test_credits_and_resets_appear_only_for_accounts_that_have_them(quota, monkeypatch, capsys):
    expires = NOW + timedelta(days=16)
    replies = {'default': {'windows': [window(77, WEEK, timedelta(days=4), 'primary')],
                           'usage_credits': [{'kind': 'credits', 'balance': '27406.21', 'unit': 'credits'}],
                           'resets': {'available': 1, 'expires_at': [int(expires.timestamp())]}},
               'spare': {'windows': [window(10, WEEK, timedelta(days=4), 'primary')],
                         'usage_credits': [{'kind': 'credits', 'balance': '0'}], 'resets': {'available': 0}}}
    monkeypatch.setattr(quota, 'fetch', lambda account, now: replies[account.label])
    values = [quota.refresh(quota.Account('codex', label, Path('/nowhere') / label, label == 'default'), NOW, force=True)
              for label in replies]
    assert 'usage_credits' not in values[1] and 'resets' not in values[1]
    rows = {row['account']: row for row in quota_display.report(vars(quota), values, NOW)['accounts']}
    assert rows['default']['usage_credits'][0]['balance'] == 27406.21
    assert rows['default']['resets'] == {'available': 1, 'expires_at': [iso(expires)], 'source': 'provider'}
    assert 'usage_credits' not in rows['spare'] and 'resets' not in rows['spare']
    quota_display.render(vars(quota), quota_display.report(vars(quota), values, NOW))
    out = capsys.readouterr().out
    local = expires.astimezone()
    assert f'codex/default: 27,406 credits available; 1 banked reset (expires {local.day} {local:%b})' in out
    assert 'codex/spare:' not in out


def test_budget_windows_show_amounts_under_a_configurable_label(quota, tmp_path, monkeypatch, capsys):
    budget = {**window(54.5, None, timedelta(days=14), 'iguana_necktie'), 'used_amount': 136.29, 'limit_amount': 250,
              'currency': 'USD'}
    monkeypatch.setattr(quota, 'fetch', lambda account, now: {'windows': [budget, window(3, WEEK, timedelta(days=6))]})
    value = quota.refresh(quota.Account('claude', 'max-a', Path('/nowhere'), True), NOW, force=True)
    account = quota_display.report(vars(quota), [value], NOW)['accounts'][0]
    rows = {row['name']: row for row in account['windows']}
    assert (rows['iguana_necktie']['label'], rows['seven_day']['label']) == ('Cloud session credits', 'Weekly')
    # The budget's pace is unknown, but only the timed window sets the account's state.
    assert rows['iguana_necktie']['pace']['state'] == 'unknown'
    assert account['state'] == 'reserve' and account['summary'].startswith('seven_day:')
    quota_display.render(vars(quota), quota_display.report(vars(quota), [value], NOW))
    out = capsys.readouterr().out
    assert 'Cloud session credits' in out and '$136 of $250' in out and '3%' in out
    configure(tmp_path, {'quota': {'window_labels': {'iguana_necktie': 'Cloud credits', 'seven_day': 'Week'}}})
    rows = {row['name']: row for row in quota_display.report(vars(quota), [value], NOW)['accounts'][0]['windows']}
    assert (rows['iguana_necktie']['label'], rows['seven_day']['label']) == ('Cloud credits', 'Week')


def test_manual_reset_credits_and_redeem_advice(quota, capsys):
    assert quota.main(['credit', 'codex', 'add']) == 0
    assert quota.main(['credit', 'codex', '--account', 'default', 'add']) == 0
    assert quota.main(['credit', 'codex', 'use']) == 0
    assert quota.credits()[('codex', 'default', 'studio')]['reset_credits'] == 1
    assert quota.main(['credit', 'codex', '--account', 'spare', 'use']) == 1

    def advice(used, reset_in):
        return quota_display.report(vars(quota), [snapshot(quota, 'codex', [window(used, WEEK, reset_in)])], NOW)['accounts'][0]['credit_advice']
    assert advice(40, timedelta(days=3))['action'] == 'hold'
    assert advice(100, timedelta(hours=2))['action'] == 'wait'
    assert advice(100, timedelta(days=3))['action'] == 'redeem'
    # A hand-recorded reset is still a banked reset; it has no known expiry.
    row = quota_display.report(vars(quota), [snapshot(quota, 'codex', [window(40, WEEK, timedelta(days=3))])], NOW)['accounts'][0]
    assert row['resets'] == {'available': 1, 'expires_at': [], 'source': 'manual'}
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
    report = quota_display.report(vars(quota), [later, soon], NOW)
    rows = {row['account']: row for row in report['accounts'] if row['provider'] == 'claude'}
    assert rows['max-a']['use_it_or_lose_it']['expiring_headroom_percent'] > 0
    assert rows['max-a']['use_it_or_lose_it']['reset_at'] == iso(NOW + timedelta(days=1))
    assert report['placement_preference']['claude'] == ['max-a', 'max-b']


def test_peer_readings_of_one_account_keep_the_freshest(quota):
    old = snapshot(quota, 'claude', [window(10, WEEK, timedelta(days=2))], observed=NOW - timedelta(hours=2), host='workshop')
    new = snapshot(quota, 'claude', [window(30, WEEK, timedelta(days=2))], observed=NOW - timedelta(minutes=1))
    rows = quota_display.report(vars(quota), [new, old], NOW)['accounts']
    assert [(row['host'], row['windows'][0]['used_percent']) for row in rows] == [('studio', 30)]


# Routing input -------------------------------------------------------------------------------

def test_reserve_snapshot_lifts_a_stale_claude_usage_cooldown(quota):
    cooldowns = {'claude/*': {'account': 'default', 'host': 'studio', 'adapter': 'claude', 'model': '*', 'status': 'usage_limited',
                              'recorded_at': iso(NOW - timedelta(hours=1)),
                              'cooling_until': iso(NOW + timedelta(hours=10))},
                 'claude/opus': {'account': 'default', 'host': 'studio', 'adapter': 'claude', 'model': 'opus', 'status': 'rate_limited',
                                 'recorded_at': iso(NOW - timedelta(hours=1)),
                                 'cooling_until': iso(NOW + timedelta(hours=10))}}
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


def test_exhausted_opencode_go_window_cools_only_go_models_not_free_zen_models(quota, tmp_path):
    snapshot(quota, 'opencode', [window(100, WEEK, timedelta(days=3), 'seven_day')])
    record = quota.apply_to_cooldowns({}, NOW)['opencode/*']
    assert record['status'] == 'quota_exhausted' and record['covers'] == ['opencode-go/']


def test_both_claude_accounts_exhausted_block_routing_until_reset_then_lift(quota, tmp_path):
    reset = NOW + timedelta(days=6, hours=10)
    configure(tmp_path, {'accounts': {'claude': {'max-a': {'identity_dir': '~/.claude'},
                                                 'max-b': {'identity_dir': '~/.claude-b'}}}})
    for label, active in (('max-a', True), ('max-b', False)):
        snapshot(quota, 'claude', [window(100, WEEK, reset - NOW), window(40, 300, timedelta(hours=2), 'five_hour')],
                 account=label, active=active, plan='max')
    cooled = quota.apply_to_cooldowns({}, NOW)
    assert cooled == {'claude/*': {**cooled['claude/*'], 'account': 'max-a', 'status': 'quota_exhausted',
                                   'cooling_until': iso(reset)}}
    assert quota_display.report(vars(quota), quota.snapshots(), NOW)['placement_preference']['claude'] == ['max-a', 'max-b']
    assert quota.apply_to_cooldowns({}, reset + timedelta(minutes=1)) == {}


def test_exhausted_window_cools_the_provider_until_reset_and_ahead_of_pace_is_report_only(quota):
    snapshot(quota, 'codex', [window(100, 300, timedelta(hours=3), 'primary')])
    cooled = quota.apply_to_cooldowns({}, NOW)
    assert cooled['codex/*']['cooling_until'] == iso(NOW + timedelta(hours=3))
    assert cooled['codex/*']['status'] == 'quota_exhausted'
    snapshot(quota, 'codex', [window(64.2, WEEK, timedelta(hours=135.7), 'secondary')])
    assert quota.apply_to_cooldowns({}, NOW) == {}
    state = quota.route_states(NOW)['codex']
    assert state['state'] == 'ahead_of_pace' and 'allowance lasts about' in state['summary']


CREDITS = [{'kind': 'credits', 'balance': 27406.21, 'unlimited': False, 'currency': None, 'unit': 'credits',
            'expires_at': None, 'source': 'provider'}]


def spent_codex(quota, account='default', credits=CREDITS, allowance=None, reset_in=timedelta(hours=3)):
    return snapshot(quota, 'codex', [window(100, WEEK, reset_in, 'primary')], account=account, usage_credits=credits,
                    **({'allowance': allowance} if allowance else {}))


def cooldown(status, recorded, **extra):
    return {'codex/*': {'account': 'default', 'host': 'studio', 'adapter': 'codex', 'model': '*', 'status': status,
                        'cooling_until': iso(NOW + timedelta(hours=8)), 'recorded_at': iso(recorded), **extra}}


def test_credits_keep_an_exhausted_codex_account_usable_as_a_last_resort(quota):
    spent_codex(quota)
    # The cooldown an earlier exhausted reading set goes once credits keep the account usable.
    assert quota.apply_to_cooldowns(cooldown('quota_exhausted', NOW - timedelta(hours=1), source='provider quota'), NOW) == {}
    state = quota.route_states(NOW)['codex']
    assert state['state'] == 'credit_only' and state['last_resort'] is True
    assert state['summary'] == 'primary: allowance spent; drawing on credits, 27,406 credits available'
    row = quota_display.report(vars(quota), quota.snapshots(), NOW)['accounts'][0]
    assert row['drawing_credits'] is True and row['credit_burn_per_hour'] is None
    # The provider's own refusal of ordinary use spends the allowance even below 100%.
    snapshot(quota, 'codex', [window(60, WEEK, timedelta(days=3), 'primary')], usage_credits=CREDITS,
             allowance={'ordinary_usage_allowed': False})
    assert quota.route_states(NOW)['codex']['state'] == 'credit_only'


def test_a_refusal_while_drawing_on_credits_keeps_its_cooldown_until_allowance_returns(quota):
    # Dispatch is refused (credits switched off, say); the next reading still shows credits.
    refused = cooldown('usage_limited', NOW - timedelta(hours=1))
    spent_codex(quota)
    assert quota.apply_to_cooldowns(refused, NOW) == refused
    snapshot(quota, 'codex', [window(100, WEEK, timedelta(hours=3), 'primary')], observed=NOW + timedelta(minutes=10),
             usage_credits=CREDITS)
    assert quota.apply_to_cooldowns(refused, NOW + timedelta(minutes=10)) == refused
    # A reading with allowance again confirms recovery, so routing may retry.
    snapshot(quota, 'codex', [window(5, WEEK, timedelta(days=7), 'primary')], observed=NOW + timedelta(hours=4))
    assert quota.apply_to_cooldowns(refused, NOW + timedelta(hours=4)) == {}


@pytest.mark.parametrize('used', [100, 60])
@pytest.mark.parametrize('case', ['floor', 'spend_control', 'depleted', 'no_credits', 'never'])
def test_credits_that_cannot_be_spent_leave_the_account_exhausted(quota, tmp_path, case, used):
    configure(tmp_path, {'quota': {'credit_floor': 30000} if case == 'floor' else
                                  {'credits_policy': 'never'} if case == 'never' else {}})
    allowance = {'spend_control': {'spend_control_reached': True},
                 'depleted': {'limit_reached_type': 'workspace_member_credits_depleted'}}.get(case) or {}
    # Below 100%, only the provider's refusal of ordinary use spends the allowance.
    allowance |= {'ordinary_usage_allowed': False} if used < 100 else {}
    snapshot(quota, 'codex', [window(used, WEEK, timedelta(hours=3), 'primary')],
             usage_credits=None if case == 'no_credits' else CREDITS, **({'allowance': allowance} if allowance else {}))
    state = quota.route_states(NOW)['codex']
    assert state['state'] == 'exhausted' and state['cooling_until'] == iso(NOW + timedelta(hours=3))
    assert quota.apply_to_cooldowns({}, NOW)['codex/*']['status'] == 'quota_exhausted'
    if used < 100:
        assert state['summary'] == 'primary: allowance spent; codex refuses ordinary use'


@pytest.mark.parametrize('reset_in', [None, -timedelta(hours=1)])
@pytest.mark.parametrize('configured', [None, 600])
def test_a_refused_allowance_with_no_future_reset_cools_for_a_bounded_wait(quota, tmp_path, reset_in, configured):
    configure(tmp_path, {'quota': {'unknown_reset_cooldown_seconds': configured} if configured else {}})
    snapshot(quota, 'codex', [window(60, WEEK, reset_in, 'primary')], usage_credits=None,
             allowance={'ordinary_usage_allowed': False})
    assert quota.route_states(NOW)['codex']['state'] == 'exhausted'
    until = iso(NOW + timedelta(seconds=configured or 3600))
    assert quota.route_states(NOW)['codex']['cooling_until'] == until
    assert quota.apply_to_cooldowns({}, NOW)['codex/*']['cooling_until'] == until
    # Once the wait ends, the next reading decides.
    assert 'codex/*' not in quota.apply_to_cooldowns({}, NOW + timedelta(seconds=(configured or 3600) + 1))


@pytest.mark.parametrize('reached', [None, 'rate_limit_reached', 'workspace_member_credits_depleted',
                                     'primary', 'five_hour', 'secondary'])
def test_refusal_codes_and_unnamed_refusals_use_the_same_cooldown(quota, reached):
    snapshot(quota, 'codex', [window(30, 300, -timedelta(hours=1), 'primary', raw_kind='five_hour'),
                              window(10, WEEK, timedelta(days=3.5), 'secondary')],
             allowance={'ordinary_usage_allowed': False,
                        **({'limit_reached_type': reached} if reached else {})})
    until = iso(NOW + (timedelta(days=3.5) if reached == 'secondary' else timedelta(hours=1)))
    assert quota.apply_to_cooldowns({}, NOW)['codex/*']['cooling_until'] == until


@pytest.mark.parametrize('policy, state, order, cooled', [
    ('last_resort', 'credit_only', ['work', 'default'], False),
    ('normal', 'credit_only', ['default', 'work'], False),
    ('never', 'exhausted', ['work', 'default'], True)])
def test_credits_policy_sets_state_ranking_and_cooldown(quota, tmp_path, policy, state, order, cooled):
    configure(tmp_path, {'quota': {'credits_policy': policy},
                         'accounts': {'codex': {'default': {}, 'work': {'identity_dir': str(tmp_path / 'codex-work')}}}})
    spent_codex(quota, reset_in=timedelta(days=1))
    snapshot(quota, 'codex', [window(30, WEEK, timedelta(days=5), 'primary')], account='work')
    report = quota_display.report(vars(quota), quota.snapshots(), NOW)
    assert next(row for row in report['accounts'] if row['account'] == 'default')['state'] == state
    assert report['placement_preference']['codex'] == order
    routed = quota.route_states(NOW)['codex']
    assert routed.get('last_resort', False) is (policy == 'last_resort')
    assert ('codex/*' in quota.apply_to_cooldowns({}, NOW)) is cooled


def test_credit_burn_rate_spans_the_run_of_falling_balance(quota, monkeypatch):
    balances = iter([30000, 27000, 26000, 26000])
    monkeypatch.setattr(quota, 'fetch', lambda account, now: {
        'windows': [window(100, WEEK, timedelta(hours=3), 'primary')],
        'usage_credits': [{'kind': 'credits', 'balance': next(balances), 'unit': 'credits'}]})
    account = quota.Account('codex', 'default', quota.provider('codex').default_identity(), True)
    readings = [quota.refresh(account, NOW + timedelta(hours=hour), force=True) for hour in (0, 1, 2)]
    assert readings[-1]['credit_anchor'] == {'balance': 30000, 'observed_at': iso(NOW)}
    row = quota_display.report(vars(quota), [readings[-1]], NOW + timedelta(hours=2))['accounts'][0]
    assert row['credit_burn_per_hour'] == 2000 and row['summary'].endswith('26,000 credits available, about 2,000/h')
    # A steady balance starts a new run.
    assert quota.refresh(account, NOW + timedelta(hours=3), force=True)['credit_anchor']['balance'] == 26000


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
    result = subprocess.run([str(front_door_binary()), 'quota', '--json'], env=env, capture_output=True, text=True, check=False)
    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report['schema'] == 'provenant.quota.report.v1' and report['enabled'] is False
    rows = {(row['provider'], row['account']): row for row in report['accounts']}
    assert set(rows) == {('agy', 'work'), ('claude', 'max-b'), ('codex', 'default')}
    codex = rows[('codex', 'default')]
    assert (codex['plan'], codex['host'], codex['state']) == ('pro', 'studio', 'on_pace')
    assert rows[('claude', 'max-b')]['status'] == 'disabled' and rows[('claude', 'max-b')]['plan'] == 'Max'
    assert rows[('agy', 'work')]['status'] == 'disabled'
    table = subprocess.run([str(front_door_binary()), 'quota'], env=env, capture_output=True, text=True, check=False)
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


@pytest.mark.parametrize('reset_in', [None, -timedelta(hours=1), timedelta(hours=8)])
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
    value = quota_display.report(vars(quota), [snapshot(quota, windows=[window(64.2, WEEK, timedelta(hours=135.7))])], NOW)
    expiring = value['accounts'][0]['use_it_or_lose_it']
    assert expiring['unused_percent'] == pytest.approx(35.8)
    assert expiring['expiring_headroom_percent'] == 0


def test_concurrent_reads_share_one_cached_fetch_and_credit_updates_are_not_lost(quota):
    from concurrent.futures import ThreadPoolExecutor
    import threading
    import time
    calls = []
    barrier = threading.Barrier(4, timeout=10)  # a dead reader fails the test, not the worker
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
    monkeypatch.setattr(quota.identity.sys, 'platform', 'darwin')
    first = quota.identity.selected_identity('agy')
    assert first.name == 'login.keychain-db'
    monkeypatch.setenv('HOME', str(tmp_path / 'another-home'))
    monkeypatch.setenv('AGY_CONFIG_DIR', str(tmp_path / 'another-config'))
    assert quota.identity.selected_identity('agy') == first
    assert quota.identity.scope('agy')['account'] == 'default'


def test_agy_identity_off_macos_is_its_state_directory_not_a_keychain(quota, monkeypatch, tmp_path):
    monkeypatch.setattr(quota.identity.sys, 'platform', 'linux')
    identity = quota.identity.selected_identity('agy')
    assert identity == tmp_path / 'home/.gemini/antigravity-cli'
    assert 'Keychain' not in str(identity)
    assert quota.identity.scope('agy')['account'] == 'default'


def test_main_json_flag_emits_valid_report_contract(quota, capsys, monkeypatch):
    """The fields apps/quota-menubar decodes from `provenant quota --json`."""
    monkeypatch.setattr(quota, 'utcnow', lambda: NOW)
    snapshot(quota, 'codex', [window(64.2, WEEK, timedelta(hours=135.7))])
    assert quota.main(['--json']) == 0
    captured = capsys.readouterr()
    assert captured.err == ''
    payload = json.loads(captured.out)
    assert payload['schema'] == quota.REPORT_SCHEMA == 'provenant.quota.report.v1'
    assert payload['host'] == 'studio' and payload['enabled'] is True
    codex = next(row for row in payload['accounts'] if row['provider'] == 'codex')
    assert codex['account'] == 'default' and codex['state'] == 'ahead_of_pace'
    assert codex['status'] == 'ok' and codex['active'] is True and codex['error'] is None
    assert codex['plan'] is None and codex['unreported'] == [] and codex['observed_at'] == iso(NOW)
    [win] = codex['windows']
    assert win['name'] == 'seven_day' and win['used_percent'] == pytest.approx(64.2)
    assert win['window_minutes'] == WEEK and win['reset_at'] == iso(NOW + timedelta(hours=135.7))
    assert win['pace']['state'] == 'ahead_of_pace'
    assert {'state', 'summary', 'delta_percent', 'runs_out_at', 'headroom_percent'} <= set(win['pace'])


# Adaptive refresh: tick, report.json, backoff and trace -------------------------------------

def fake_accounts(quota, tmp_path, monkeypatch, labels, fetch, **settings):
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None, fetch=fetch))
    configure(tmp_path, {**({'quota': settings} if settings else {}), 'accounts': {'fake': {
        label: {'identity_dir': str(tmp_path / 'ids' / label)} for label in labels}}})
    monkeypatch.setattr(quota, 'utcnow', lambda: NOW)


def refuse_fetch(*args, **kwargs):
    raise AssertionError('nothing is due, so nothing may be fetched')


REPORT_KEYS = {'schema', 'generated_at', 'host', 'enabled', 'providers', 'accounts', 'placement_preference',
               'display', 'headline'}
ROW_KEYS = {'provider', 'account', 'plan', 'host', 'active', 'priority', 'observed_at', 'attempted_at', 'status',
            'error', 'source', 'state', 'summary', 'windows', 'unreported', 'coverage_note', 'credits',
            'credit_advice', 'use_it_or_lose_it',
            # The display vocabulary, added in place: older readers ignore it.
            'provider_name', 'display_name', 'plan_label', 'display_window', 'back_at'}
SCHEDULE_KEYS = {'next_due_at', 'reason', 'ttl_seconds', 'trigger', 'failures', 'backoff_until', 'error_kind',
                 'last_duration_ms', 'source_kind'}


def test_tick_fetches_only_due_accounts_and_publishes_progress(quota, tmp_path, monkeypatch, capsys):
    calls, seen = [], []

    def fetch(account, io, now):
        calls.append(account.label)
        seen.append(json.loads(quota.report_path().read_text())['refreshing'])
        return {'windows': [window(20, 300, timedelta(hours=2), 'five_hour')]}
    fake_accounts(quota, tmp_path, monkeypatch, ['fresh', 'stale', 'new'], fetch)
    snapshot(quota, 'fake', [window(20, 300, timedelta(hours=2))], account='fresh', observed=NOW - timedelta(minutes=1))
    snapshot(quota, 'fake', [window(20, 300, timedelta(hours=2))], account='stale', observed=NOW - timedelta(hours=3))
    assert quota.main(['tick', '--json']) == 0
    printed = json.loads(capsys.readouterr().out)
    assert sorted(calls) == ['new', 'stale']
    # Readers saw a complete report naming what was still being fetched, then an idle one.
    assert all(set(names) <= {'fake/new', 'fake/stale'} and names for names in seen)
    assert printed == json.loads(quota.report_path().read_text()) and printed['refreshing'] == []
    rows = {row['account']: row for row in printed['accounts']}
    assert rows['fresh']['schedule']['reason'] == 'idle' and rows['fresh']['attempted_at'] == iso(NOW - timedelta(minutes=1))
    assert rows['new']['schedule']['trigger'] == 'timer' and rows['new']['status'] == 'ok'
    assert printed['next_due_at'] == min((row['schedule']['next_due_at'] for row in rows.values()), key=quota.parse_time)
    assert not [path.name for path in quota.quota_dir().iterdir() if path.name.startswith('report.json.')]


def test_a_crashed_tick_still_clears_refreshing(quota, tmp_path, monkeypatch):
    fake_accounts(quota, tmp_path, monkeypatch, ['work'], lambda account, io, now: {'windows': []})
    monkeypatch.setattr(quota, 'refresh', lambda *args: (_ for _ in ()).throw(KeyError('provider bug')))
    with pytest.raises(KeyError):
        quota.tick()
    written = json.loads(quota.report_path().read_text())
    assert written['refreshing'] == [] and [row['account'] for row in written['accounts']] == ['work']


def test_tick_with_the_refresh_lock_held_prints_the_current_report_and_never_fetches(quota, tmp_path, monkeypatch, capsys):
    import fcntl
    fake_accounts(quota, tmp_path, monkeypatch, ['work'], refuse_fetch)
    lock = quota.quota_dir() / 'refresh.lock'
    lock.parent.mkdir(parents=True, exist_ok=True)
    quota.report_path().write_text(json.dumps({'schema': quota.REPORT_SCHEMA, 'accounts': [], 'marker': 1}))
    with lock.open('a') as held:
        fcntl.flock(held, fcntl.LOCK_EX)
        assert quota.main(['tick', '--json', '--trigger', 'manual']) == 0
    assert json.loads(capsys.readouterr().out)['marker'] == 1


def test_report_writes_are_atomic(quota, monkeypatch):
    path = quota.report_path()
    quota.atomic_json(path, {'schema': quota.REPORT_SCHEMA, 'generation': 1})

    def broken(*args, **kwargs):
        raise ValueError('write interrupted')
    monkeypatch.setattr(quota, 'json', SimpleNamespace(dumps=broken, loads=json.loads))
    with pytest.raises(ValueError):
        quota.atomic_json(path, {'schema': quota.REPORT_SCHEMA, 'generation': 2})
    assert json.loads(path.read_text())['generation'] == 1
    assert [entry.name for entry in path.parent.iterdir()] == ['report.json']


def test_report_schema_is_unchanged_apart_from_optional_schedule_fields(quota, monkeypatch, capsys):
    monkeypatch.setattr(quota, 'utcnow', lambda: NOW)
    monkeypatch.setattr(quota, 'fetch', refuse_fetch)
    snapshot(quota, 'codex', [window(64.2, WEEK, timedelta(hours=135.7))])
    for command in (['--json'], ['tick', '--json']):
        assert quota.main(command) == 0
        payload = json.loads(capsys.readouterr().out)
        assert set(payload) == REPORT_KEYS | {'next_due_at', 'refreshing'}
        assert payload['schema'] == 'provenant.quota.report.v1' and payload['refreshing'] == []
        codex, = payload['accounts']
        assert set(codex) == ROW_KEYS | {'schedule'}
        assert (codex['state'], codex['status'], codex['observed_at']) == ('ahead_of_pace', 'ok', iso(NOW))
        assert set(codex['schedule']) == SCHEDULE_KEYS
        assert codex['schedule']['error_kind'] is None and codex['schedule']['source_kind'] == 'fetch'
        assert payload['next_due_at'] == codex['schedule']['next_due_at']


def test_tick_fast_path_fetches_nothing_and_rewrites_only_a_moved_due_time(quota, tmp_path, monkeypatch, capsys):
    fake_accounts(quota, tmp_path, monkeypatch, ['work'], refuse_fetch)
    monkeypatch.setattr(quota, 'fetch', refuse_fetch)
    snapshot(quota, 'fake', [window(20, 300, timedelta(hours=2))], account='work', observed=NOW - timedelta(minutes=2))
    assert quota.main(['tick']) == 0
    first = quota.report_path().stat()
    assert quota.main(['tick', '--json']) == 0 and quota.main(['tick', '--trigger', 'power']) == 0
    assert quota.report_path().stat().st_ino == first.st_ino
    # Another process fetched meanwhile: the due time moved, so the report is refreshed.
    snapshot(quota, 'fake', [window(30, 300, timedelta(hours=2))], account='work', observed=NOW - timedelta(minutes=1))
    assert quota.main(['tick']) == 0
    assert quota.report_path().stat().st_ino != first.st_ino
    assert 'fake/work: ok; next' in capsys.readouterr().out


@pytest.mark.parametrize('kind, retry, low, high, manual_fetches', [
    ('rate_limited', 900, 900, 900, False),
    ('rate_limited', None, 108, 132, False),
    ('network', None, 108, 132, True),
    ('auth', None, 3240, 3960, True),
    ('missing', None, 77760, 95040, True),
    ('other', None, 108, 132, True),
])
def test_a_failed_fetch_backs_off_by_kind(quota, tmp_path, kind, retry, low, high, manual_fetches):
    calls = []

    def fetch(account, io, now):
        calls.append(now)
        raise io.Unavailable('provider quota request failed', kind, retry)
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None, fetch=fetch))
    account = quota.Account('fake', 'work', tmp_path / 'id', True)
    value = quota.refresh(account, NOW)
    wait = (quota.parse_time(value['fetch']['backoff_until']) - NOW).total_seconds()
    assert low <= wait <= high + 1 and value['fetch']['retry_after_s'] == retry  # from completion
    assert (value['schedule']['reason'], value['schedule']['error_kind'], value['schedule']['failures']) == (
        'backoff', kind, 1)
    assert quota.refresh(account, NOW + timedelta(minutes=1), trigger='panel')['status'] == 'unavailable'
    assert len(calls) == 1
    again = quota.refresh(account, NOW + timedelta(minutes=1), trigger='manual')
    assert len(calls) == (2 if manual_fetches else 1)
    if manual_fetches and kind not in ('auth', 'missing'):
        assert again['fetch']['failures'] == 2 and low * 2 <= (
            quota.parse_time(again['fetch']['backoff_until']) - NOW - timedelta(minutes=1)).total_seconds() <= high * 2 + 1
    quota.refresh(account, NOW + timedelta(minutes=2), trigger='manual', ignore_backoff=True)
    assert len(calls) == (3 if manual_fetches else 2)


@pytest.mark.parametrize('code, header, kind, wait', [
    (429, '120', 'rate_limited', 120), (429, 'date', 'rate_limited', 300), (429, None, 'rate_limited', None),
    (401, None, 'auth', None), (403, None, 'auth', None), (503, None, 'network', None), (404, None, 'other', None),
    (None, None, 'network', None)])
def test_http_json_surfaces_status_kind_and_retry_after_without_credentials(monkeypatch, code, header, kind, wait):
    from email.message import Message
    from email.utils import format_datetime
    from urllib.error import HTTPError, URLError
    module = load('quota_http_kinds', ROOT / 'scripts/provider_quota.py')
    headers = Message()
    if header:
        headers['Retry-After'] = header if header != 'date' else format_datetime(
            datetime.now(UTC) + timedelta(seconds=300), usegmt=True)

    def opener(request, timeout):
        assert request.get_header('Authorization') == 'Bearer ' + TOKEN
        if code is None:
            raise URLError(f'connection refused for {TOKEN}')
        raise HTTPError(request.full_url, code, f'denied {TOKEN}', headers, io.BytesIO(TOKEN.encode()))
    monkeypatch.setattr(module, 'urlopen', opener)
    with pytest.raises(module.Unavailable) as caught:
        module.http_json('https://api.anthropic.com/api/oauth/usage', {'Authorization': 'Bearer ' + TOKEN})
    assert (caught.value.kind, caught.value.status) == (kind, code)
    assert caught.value.retry_after == pytest.approx(wait, abs=2) if wait else caught.value.retry_after is None
    assert (caught.value.retry_at is not None) == (header == 'date')
    assert TOKEN not in str(caught.value) and TOKEN not in repr(caught.value.args)
    assert caught.value.__cause__ is None and caught.value.__suppress_context__


def test_trace_records_each_attempt_and_summarises_and_prunes(quota, tmp_path, monkeypatch, capsys):
    replies = iter([{'windows': [window(10, 300, timedelta(hours=1))]},
                    quota.Unavailable('provider quota request could not connect', 'network'),
                    quota.Unavailable('provider quota service is rate limited', 'rate_limited', 60),
                    {'windows': [window(12, 300, timedelta(hours=1))]}])

    def fetch(account, io, now):
        reply = next(replies)
        if isinstance(reply, Exception):
            raise reply
        return reply
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None, fetch=fetch))
    account = quota.Account('fake', 'work', tmp_path / 'id', True)
    for minutes in range(4):
        quota.refresh(account, NOW - timedelta(hours=1) + timedelta(minutes=minutes), force=True, trigger='manual')
    lines = [json.loads(line) for line in (quota.quota_dir() / 'fetches.jsonl').read_text().splitlines()]
    assert [line['outcome'] for line in lines] == ['ok', 'error', 'error', 'ok']
    assert set(lines[2]) == {'at', 'account', 'provider', 'trigger', 'outcome', 'error_kind', 'duration_ms',
                             'retry_after_s', 'error'}
    assert (lines[2]['error_kind'], lines[2]['retry_after_s'], lines[2]['trigger']) == ('rate_limited', 60, 'manual')
    monkeypatch.setattr(quota, 'utcnow', lambda: NOW)
    assert quota.main(['trace', '--since', '24h', '--json']) == 0
    summary = json.loads(capsys.readouterr().out)
    row, = summary['accounts']
    assert (summary['fetches'], summary['failures']) == (4, 2)
    assert (row['fetches'], row['ok'], row['failures']) == (4, 2, {'network': 1, 'rate_limited': 1})
    assert row['p50_ms'] is not None and row['max_ms'] >= row['p50_ms']
    assert quota.trace_summary(NOW - timedelta(minutes=30))['fetches'] == 0
    assert quota.main(['trace']) == 0 and 'rate_limited 1' in capsys.readouterr().out

    path = quota.quota_dir() / 'fetches.jsonl'
    old = json.dumps({**lines[0], 'at': iso(NOW - timedelta(days=40))}) + '\n'
    path.write_text(old + path.read_text())
    assert quota.prune_trace(NOW) is False      # the first append pruned an hour ago: once a day
    assert len(path.read_text().splitlines()) == 5
    assert quota.prune_trace(NOW + timedelta(days=1)) is True
    assert len(path.read_text().splitlines()) == 4
    assert stat.S_IMODE(path.stat().st_mode) == 0o600
    configure(tmp_path, {'quota': {'trace': {'retention_days': 1}}})
    assert quota.prune_trace(NOW + timedelta(days=2, hours=1)) is True
    assert path.read_text() == ''


def test_every_trace_writer_prunes_daily_not_only_tick(quota, tmp_path):
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None,
                                           fetch=lambda account, io, now: {'windows': [window(10, 300, timedelta(hours=1))]}))
    path = quota.quota_dir() / 'fetches.jsonl'
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({'at': iso(NOW - timedelta(days=40)), 'outcome': 'ok'}) + '\n')
    quota.refresh(quota.Account('fake', 'work', tmp_path / 'id', True), NOW, force=True)  # as `quota --refresh`
    assert [json.loads(line)['at'] for line in path.read_text().splitlines()] == [iso(NOW)]


@pytest.mark.parametrize('planted', ['fetches.jsonl', 'fetches.lock', 'fetches.pruned', 'refresh.lock'])
def test_quota_state_files_are_never_written_through_a_symlink(quota, tmp_path, planted):
    victim = tmp_path / 'victim'
    victim.write_text('keep\n')
    os.utime(victim, (1, 1))
    missing = tmp_path / 'never-created'
    quota.quota_dir().mkdir(parents=True)
    (quota.quota_dir() / planted).symlink_to(victim if planted != 'refresh.lock' else missing)
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None,
                                           fetch=lambda account, io, now: {'windows': [window(10, 300, timedelta(hours=1))]}))
    configure(tmp_path, {'accounts': {'fake': {'work': {'identity_dir': str(tmp_path / 'id')}}}})
    if planted == 'refresh.lock':
        with pytest.raises(OSError):
            quota.tick()
    else:
        quota.refresh(quota.Account('fake', 'work', tmp_path / 'id', True), NOW, force=True)
        try:
            quota.prune_trace(NOW + timedelta(days=2))
        except OSError:
            pass  # refusing the planted lock is the safe outcome
    assert victim.read_text() == 'keep\n' and victim.stat().st_mtime == 1 and not missing.exists()


def test_trace_pruning_ignores_a_planted_temporary(quota, tmp_path):
    victim = tmp_path / 'victim'
    victim.write_text('keep\n')
    folder = quota.quota_dir()
    folder.mkdir(parents=True)
    (folder / 'fetches.jsonl').write_text(json.dumps({'at': iso(NOW), 'outcome': 'ok'}) + '\n')
    for name in ('fetches.jsonl.prune', 'fetches.jsonl.tmp', 'fetches.pruned.tmp'):
        (folder / name).symlink_to(victim)
    assert quota.prune_trace(NOW) is True
    assert victim.read_text() == 'keep\n'
    assert (folder / 'fetches.jsonl').read_text().count('\n') == 1


def test_backoff_runs_from_completion_and_a_retry_after_date_stays_absolute(quota, tmp_path, monkeypatch):
    clock = iter([100.0, 130.0, 200.0, 230.0])
    monkeypatch.setattr(quota, 'monotonic', lambda: next(clock))
    replies = iter([quota.Unavailable('provider quota service is rate limited', 'rate_limited', 900),
                    quota.Unavailable('provider quota service is rate limited', 'rate_limited', 600,
                                      retry_at=NOW + timedelta(minutes=10))])

    def fetch(account, io, now):
        raise next(replies)
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None, fetch=fetch))
    account = quota.Account('fake', 'work', tmp_path / 'id', True)
    first = quota.refresh(account, NOW, force=True)
    assert first['fetch']['backoff_until'] == iso(NOW + timedelta(seconds=30 + 900))   # 30 s request, then 900 s
    second = quota.refresh(account, NOW, force=True)
    assert second['fetch']['backoff_until'] == iso(NOW + timedelta(minutes=10))        # the date, not 30 s later


def test_errors_and_trace_never_carry_credentials(quota, tmp_path):
    secret = 'sk-ant-oat01-' + 'a1B2' * 12
    replies = iter([RuntimeError(f'Bearer {TOKEN} {secret}'),
                    quota.Unavailable(f'refused for owner@example.com with Bearer {TOKEN} and {secret}', 'auth')])

    def fetch(account, io, now):
        raise next(replies)
    quota.register('fake', SimpleNamespace(ADAPTER='fake', default_identity=lambda: None, fetch=fetch))
    account = quota.Account('fake', 'work', tmp_path / 'id', True)
    first = quota.refresh(account, NOW, force=True)
    second = quota.refresh(account, NOW + timedelta(minutes=1), force=True)
    assert first['error'] == 'quota source failed (RuntimeError)'
    stored = quota.cache_path('fake', 'work', identity_dir=account.identity_dir).read_text()
    traced = (quota.quota_dir() / 'fetches.jsonl').read_text()
    for text in (json.dumps(second), stored, traced, json.dumps(quota_display.report(vars(quota), [second], NOW))):
        for value in (TOKEN, secret, 'owner@example.com'):
            assert value not in text
    assert all(len(json.loads(line)['error'] or '') <= 200 for line in traced.splitlines())


def test_gather_times_each_attempt_when_it_starts(quota, tmp_path, monkeypatch):
    clock = iter([NOW, NOW + timedelta(seconds=3), NOW + timedelta(seconds=9)])
    fake_accounts(quota, tmp_path, monkeypatch, ['a', 'b'], lambda account, io, now: {
        'windows': [window(10, 300, timedelta(hours=1))]})
    monkeypatch.setattr(quota, 'utcnow', lambda: next(clock))
    assert [row['attempted_at'] for row in quota.gather()] == [iso(NOW), iso(NOW + timedelta(seconds=3))]


def test_schedule_command_prints_each_account_block_without_fetching(quota, tmp_path, monkeypatch, capsys):
    fake_accounts(quota, tmp_path, monkeypatch, ['work'], refuse_fetch)
    snapshot(quota, 'fake', [window(20, 300, timedelta(hours=2))], account='work', observed=NOW - timedelta(minutes=2))
    assert quota.main(['schedule', '--json']) == 0
    row, = json.loads(capsys.readouterr().out)
    assert (row['provider'], row['account'], row['schedule']['reason']) == ('fake', 'work', 'idle')
    assert quota.main(['schedule']) == 0 and 'idle' in capsys.readouterr().out
    assert quota.main(['path']) == 0 and capsys.readouterr().out.strip() == str(quota.quota_dir())
    assert quota.main(['path', '--json']) == 0
    assert json.loads(capsys.readouterr().out)['report'] == str(quota.report_path())


def test_schedule_disabled_keeps_the_cache_period_and_enabled_drops_its_floor(quota, tmp_path, monkeypatch):
    calls = []
    fake_accounts(quota, tmp_path, monkeypatch, ['work'], lambda account, io, now: calls.append(now) or {
        'windows': [window(10, 300, timedelta(hours=1))]}, cache_seconds=60,
        providers={'fake': {'ttl_idle_seconds': 90}})
    account, = quota.accounts()
    quota.refresh(account, NOW)
    quota.refresh(account, NOW + timedelta(seconds=120))
    assert len(calls) == 2  # a 90 s schedule fetches again; the old 300 s floor would not
    configure(tmp_path, {'quota': {'cache_seconds': 60, 'schedule': {'enabled': False}},
                         'accounts': {'fake': {'work': {'identity_dir': str(tmp_path / 'ids/work')}}}})
    quota.refresh(account, NOW + timedelta(seconds=240))
    assert len(calls) == 2  # cache_seconds keeps its 300 s floor
    assert quota.refresh(account, NOW + timedelta(seconds=421))['schedule']['reason'] == 'cache_seconds'
    assert len(calls) == 3


def test_tick_runs_a_solo_provider_alone_and_others_in_parallel(quota, tmp_path, monkeypatch):
    import threading
    import time
    running, overlaps, guard = set(), [], threading.Lock()

    def fetch(account, io, now):
        with guard:
            running.add(account.provider)
            overlaps.append(set(running))
        time.sleep(0.05)
        with guard:
            running.discard(account.provider)
        return {'windows': [window(10, 300, timedelta(hours=1))]}
    for name in ('fake', 'other', 'agy'):
        quota.register(name, SimpleNamespace(ADAPTER=name, default_identity=lambda: None, fetch=fetch))
    configure(tmp_path, {'accounts': {name: {'work': {'identity_dir': str(tmp_path / name)}} for name in ('fake', 'other', 'agy')}})
    monkeypatch.setattr(quota, 'utcnow', lambda: NOW)
    quota.tick()
    assert {'fake', 'other'} in overlaps
    assert all(seen == {'agy'} for seen in overlaps if 'agy' in seen)


def test_claude_auth_status_is_cached_per_identity_until_it_ages_or_is_used(quota, monkeypatch, tmp_path):
    identity = tmp_path / 'claude-c'
    identity.mkdir()
    history = identity / 'history.jsonl'
    history.write_text('')
    runs = []
    monkeypatch.setattr(quota, 'executable', lambda name: '/fake/claude')

    def run(args, **kwargs):
        runs.append(args[1:3])
        return SimpleNamespace(returncode=0, stdout=json.dumps({
            'loggedIn': True, 'authMethod': 'claude.ai', 'subscriptionType': 'max', 'email': 'owner@example.com'}))
    monkeypatch.setattr(quota, 'run_process', run)
    claude, account = quota.provider('claude'), quota.Account('claude', 'c', identity, False)
    assert claude.status(account, quota.IO) == {'loggedIn': True, 'authMethod': 'claude.ai', 'subscriptionType': 'max'}
    claude.status(account, quota.IO)
    assert runs == [['auth', 'status']]
    assert 'owner@example.com' not in ''.join(path.read_text() for path in (quota.quota_dir() / 'memo').iterdir())
    later = history.stat().st_mtime + 60
    os.utime(history, (later, later))
    claude.status(account, quota.IO)
    assert len(runs) == 2
    monkeypatch.setattr(quota, 'utcnow', lambda: datetime.now(UTC) + timedelta(hours=25))
    claude.status(account, quota.IO)
    assert len(runs) == 3
    other = tmp_path / 'claude-d'
    other.mkdir()
    claude.status(quota.Account('claude', 'd', other, False), quota.IO)
    assert len(runs) == 4


def test_opencode_counts_a_recent_message_in_a_session_last_updated_before_the_window(quota, tmp_path):
    home = tmp_path / 'oc'
    home.mkdir()
    ms = lambda age: int((NOW - age).timestamp() * 1000)
    with sqlite3.connect(home / 'opencode.db') as db:
        db.executescript('''
            CREATE TABLE session (id text PRIMARY KEY, time_created integer NOT NULL, time_updated integer NOT NULL);
            CREATE TABLE message (id text PRIMARY KEY, session_id text NOT NULL, time_created integer NOT NULL,
                                  time_updated integer NOT NULL, data text NOT NULL);
            CREATE INDEX message_session_time_created_id_idx ON message (session_id, time_created, id);''')
        db.execute('INSERT INTO session VALUES (?,?,?)', ('stale', ms(90 * day), ms(60 * day)))
        db.execute('INSERT INTO message VALUES (?,?,?,?,?)', ('m1', 'stale', ms(hour), ms(hour),
                   json.dumps({'providerID': 'opencode-go', 'cost': 7.0})))
    windows, _ = opencode_rows(quota, quota.Account('opencode', 'go', home, True))
    assert windows['five_hour']['used_percent'] == pytest.approx(7.0 / 6.4 * 100)


def test_opencode_spend_queries_reach_messages_through_indexes(quota, tmp_path, monkeypatch):
    home = tmp_path / 'oc'
    home.mkdir()
    ms = lambda age: int((NOW - age).timestamp() * 1000)
    with sqlite3.connect(home / 'opencode.db') as db:
        # The indexes of the real store (opencode 2.0.30); v1 `message` has no time index of its own.
        db.executescript('''
            CREATE TABLE session (id text PRIMARY KEY, time_created integer NOT NULL, time_updated integer NOT NULL);
            CREATE TABLE message (id text PRIMARY KEY, session_id text NOT NULL, time_created integer NOT NULL,
                                  time_updated integer NOT NULL, data text NOT NULL);
            CREATE INDEX message_session_time_created_id_idx ON message (session_id, time_created, id);
            CREATE TABLE session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
                                          seq integer NOT NULL, time_created integer NOT NULL,
                                          time_updated integer NOT NULL, data text NOT NULL);
            CREATE INDEX session_message_session_time_created_id_idx ON session_message (session_id, time_created, id);
            CREATE INDEX session_message_time_created_idx ON session_message (time_created);''')
        db.executemany('INSERT INTO session VALUES (?,?,?)', [('recent', ms(day), ms(hour)), ('old', ms(90 * day), ms(60 * day))])
        db.executemany('INSERT INTO message VALUES (?,?,?,?,?)', [
            ('m1', 'recent', ms(hour), ms(hour), json.dumps({'providerID': 'opencode-go', 'cost': 3.2})),
            ('m2', 'old', ms(60 * day), ms(60 * day), json.dumps({'providerID': 'opencode-go', 'cost': 99}))])
        db.execute('INSERT INTO session_message VALUES (?,?,?,?,?,?,?)', ('s1', 'v2', 'assistant', 1, ms(hour), ms(hour),
                   json.dumps({'model': {'providerID': 'opencode-go'}, 'cost': 1.6})))
    opencode, queries = quota.provider('opencode'), []

    class Recording:
        def __init__(self, *args, **kwargs):
            self.db = sqlite3.connect(*args, **kwargs)
        def execute(self, sql, params=()):
            queries.append((sql, params))
            return self.db.execute(sql, params)
        def close(self):
            self.db.close()
    monkeypatch.setattr(quota.IO, 'readonly', lambda path: Recording(path.resolve().as_uri() + '?mode=ro', uri=True))
    windows, _ = opencode_rows(quota, quota.Account('opencode', 'go', home, True))
    assert windows['thirty_day']['used_percent'] == pytest.approx(4.8 / 32 * 100)
    reads = [(sql, params) for sql, params in queries if 'FROM message' in sql or 'FROM session_message' in sql]
    assert len(reads) == 4
    with sqlite3.connect(home / 'opencode.db') as db:
        for sql, params in reads:
            plan = [row[3] for row in db.execute('EXPLAIN QUERY PLAN ' + sql, params)]
            assert not [step for step in plan if step in ('SCAN message', 'SCAN session_message')], (sql, plan)
            assert any('USING' in step and 'INDEX' in step for step in plan), (sql, plan)
            if 'FROM message' in sql:
                assert any('message_session_time_created_id_idx' in step for step in plan)


def test_codex_reports_its_usage_credit_balance_for_the_series(quota):
    parsed = quota.provider('codex').parse({'rateLimits': {'planType': 'prolite', 'primary': {'usedPercent': 1},
                                                           'credits': {'hasCredits': True, 'balance': '27406.21'}}})
    assert (parsed['plan'], parsed['credit_balance']) == ('prolite', '27406.21')
    assert quota.provider('codex').parse({'rateLimits': {'primary': {'usedPercent': 1}}})['credit_balance'] is None


@pytest.mark.parametrize('scale', [1, 1000])
def test_claude_limit_with_a_numeric_reset_still_matches_the_window_it_repeats(quota, scale):
    reset = NOW + timedelta(days=5)
    payload = {'seven_day': {'utilization': 3.0, 'resets_at': reset.isoformat()},
               'limits': [claude_limit('weekly_all', 'unlisted', 3, reset.timestamp() * scale)]}
    rows = {row['name']: row for row in quota.provider('claude').parse(payload)['windows']}
    assert rows['weekly_all:1']['window_minutes'] == WEEK and rows['weekly_all:1']['advisory'] is False


def test_atomic_json_writes_private_files(quota):
    path = quota.report_path()
    quota.atomic_json(path, {'schema': quota.REPORT_SCHEMA})
    assert path.stat().st_mode & 0o777 == 0o600 and json.loads(path.read_text())['schema'] == quota.REPORT_SCHEMA


def test_claude_parse_loads_quota_schedule_itself_when_it_is_not_registered(monkeypatch):
    monkeypatch.delitem(sys.modules, 'quota_schedule', raising=False)
    try:
        claude = load('claude_alone', ROOT / 'scripts/quota_providers/claude.py')
        reset = NOW + timedelta(days=5)
        payload = {'seven_day': {'utilization': 3.0, 'resets_at': reset.isoformat()},
                   'limits': [claude_limit('weekly_all', 'unlisted', 3, reset.timestamp())]}
        rows = {row['name']: row for row in claude.parse(payload)['windows']}
        assert rows['weekly_all:1']['window_minutes'] == WEEK
    finally:
        sys.modules.pop('quota_schedule', None)


@pytest.mark.parametrize('short,weekly,expected', [
    ((10, 20), (10, 35), 'pro'),  # More allowance would expire unused on Pro.
    ((100, 20), (10, 35), 'max'),  # An exhausted short window excludes weekly reserve.
    ((10, 20), (70, 35), 'max'),  # Spend projected unused allowance first.
    ((10, 20), (100, 35), 'max'),
    ((10, 100), (70, 35), 'pro'),  # An exhausted short window is never chosen.
    ((10, 20), (70, 80), 'pro'),  # Among ahead-of-pace windows, prefer the slower burn.
    ((10, 10), (60, 90), 'pro'),  # Label order cannot outweigh projected burn.
    ((10, 10), (10, 10), 'max'),  # Stable label tie-break, independent of input order.
])
def test_choose_claude_account_from_cached_windows(quota, short, weekly, expected):
    values = {label: {'status': 'ok', 'windows': [
        window(five, 300, timedelta(hours=2), 'five_hour'),
        window(seven, WEEK, timedelta(days=3.5)),
    ]} for label, five, seven in zip(('pro', 'max'), short, weekly)}
    choose = quota.choose_claude_account
    assert choose(values, NOW) == expected


@pytest.mark.parametrize('values,expected', [({}, None), ({'pro': None}, 'pro'),
    ({'pro': {'status': 'error', 'windows': []}}, 'pro')])
def test_choose_claude_account_without_usable_cache_uses_stable_labels(quota, values, expected):
    assert quota.choose_claude_account(values, NOW) == expected


@pytest.mark.parametrize('reading', [None, {'status': 'error', 'windows': []}])
def test_uncached_multiple_claude_accounts_match_the_dispatch_help(quota, reading):
    values = {'pro': reading, 'max': reading, 'default': reading}
    assert quota.choose_claude_account(values, NOW) == 'default'
    values.pop('default')
    assert quota.choose_claude_account(values, NOW) == 'max'
    help_text = subprocess.check_output([str(front_door_binary()), 'help', 'dispatch'], text=True)
    assert 'When all eligible accounts lack usable cached quota,' in help_text
    assert 'labels sort alphabetically.' in help_text


@pytest.mark.parametrize('selected,exhausted', [('pro', True), ('max', False)])
def test_cached_cooldown_targets_selected_claude_account(quota, tmp_path, selected, exhausted):
    configure(tmp_path, {'accounts': {'claude': {
        'pro': {'identity_dir': str(tmp_path / 'pro')},
        'max': {'identity_dir': str(tmp_path / 'max')},
    }}})
    snapshot(quota, 'claude', [window(100, WEEK, timedelta(days=2))], account='pro')
    snapshot(quota, 'claude', [window(10, WEEK, timedelta(days=2))], account='max')
    records = quota.apply_to_cooldowns({}, NOW, selected_accounts={'claude': selected})
    assert ('claude/*' in records) is exhausted
    if exhausted:
        assert records['claude/*']['account'] == selected


@pytest.mark.parametrize('five,seven', [(100, 10), (10, 100), (120, 100)])
def test_choose_claude_account_refuses_all_exhausted(quota, five, seven):
    values = {label: {'status': 'ok', 'windows': [
        window(five, 300, timedelta(hours=2), 'five_hour'),
        window(seven, WEEK, timedelta(days=3.5)),
    ]} for label in ('pro', 'max')}
    with pytest.raises(ValueError, match='claude_accounts_exhausted'):
        quota.choose_claude_account(values, NOW)


@pytest.mark.parametrize('used,expected', [(10, 'known'), (70, 'unknown'), (100, 'unknown')])
@pytest.mark.parametrize('unknown', [None, {'status': 'error'}, {'status': 'ok', 'windows': []}])
def test_choose_claude_account_places_unknown_below_usable_allowance(quota, used, expected, unknown):
    values = {'unknown': unknown, 'known': {'status': 'ok', 'windows': [
        window(10, 300, timedelta(hours=2), 'five_hour'),
        window(used, WEEK, timedelta(days=3.5)),
    ]}}
    assert quota.choose_claude_account(values, NOW) == expected


@pytest.mark.parametrize('reverse', [False, True])
def test_choose_claude_account_prefers_pro_plan_on_equal_waste(quota, reverse):
    values = {label: {'status': 'ok', 'plan': plan, 'windows': [
        window(10, 300, timedelta(hours=2), 'five_hour'),
        window(10, WEEK, timedelta(days=3.5)),
    ]} for label, plan in [('a-max', 'max'), ('z-pro', 'pro')]}
    if reverse:
        values = dict(reversed(list(values.items())))
    assert quota.choose_claude_account(values, NOW) == 'z-pro'


def test_choose_claude_account_releases_exhaustion_after_reset(quota):
    values = {'reset': {'status': 'ok', 'windows': [
        window(100, 300, timedelta(seconds=-1), 'five_hour'),
        window(100, WEEK, timedelta(seconds=-1)),
    ]}}
    with pytest.raises(quota.ClaudeAccountsExhaustedError):
        quota.choose_claude_account(values, NOW)
    assert quota.choose_claude_account(values, NOW + timedelta(seconds=quota.CLOCK_SKEW_SECONDS)) == 'reset'


@pytest.mark.parametrize('reset_in', [timedelta(0), -timedelta(hours=1)])
def test_reset_weekly_allowance_ranks_above_an_alphabetically_earlier_unknown(quota, reset_in):
    values = {'aaa': None, 'zreset': {'status': 'ok', 'windows': [
        window(10, 300, timedelta(hours=2), 'five_hour'), window(80, WEEK, reset_in)]}}
    assert quota.pace(window(0, WEEK, reset_in), NOW)['state'] == 'not_started'
    assert quota.choose_claude_account(values, NOW) == ('aaa' if reset_in == timedelta(0) else 'zreset')
    assert quota.choose_claude_account(values, NOW + timedelta(seconds=quota.CLOCK_SKEW_SECONDS)) == 'zreset'


def test_choose_claude_account_partial_reading_still_excludes_known_exhaustion(quota):
    values = {'spent': {'status': 'ok', 'windows': [
        window(100, 300, timedelta(hours=2), 'five_hour'),
    ]}, 'unknown': None}
    assert quota.choose_claude_account(values, NOW) == 'unknown'


@pytest.mark.parametrize('missing', ['reset_at', 'window_minutes'])
@pytest.mark.parametrize('used,expected', [(50, 'known-max'), (70, 'unknown-pro')])
def test_choose_claude_account_incomplete_weekly_pace_ranks_as_unknown(quota, missing, used, expected):
    incomplete = window(10, WEEK, timedelta(days=3.5))
    incomplete.pop(missing)
    values = {'unknown-pro': {'status': 'ok', 'plan': 'pro', 'windows': [
        window(10, 300, timedelta(hours=2), 'five_hour'), incomplete,
    ]}, 'known-max': {'status': 'ok', 'plan': 'max', 'windows': [
        window(10, 300, timedelta(hours=2), 'five_hour'),
        window(used, WEEK, timedelta(days=3.5)),
    ]}}
    assert quota.choose_claude_account(values, NOW) == expected


@pytest.mark.parametrize('reset_in', [timedelta(0), timedelta(hours=-1)])
def test_fresh_exhausted_reading_at_reset_remains_exhausted(quota, reset_in):
    snapshot(quota, 'codex', [window(100, WEEK, reset_in, 'primary')])
    result = quota.route_states(NOW)['codex']
    assert result['state'] == 'exhausted'
    assert 'stale' not in result['summary']
    assert quota.apply_to_cooldowns({}, NOW)['codex/*']['cooling_until'] == iso(NOW + timedelta(hours=1))


@pytest.mark.parametrize('resets,expected', [(0, 'unknown'), (1, 'unknown'), (6, 'burst')])
def test_ahead_of_pace_with_allowance_ranks_above_unknown_cache(quota, resets, expected):
    values = {'unknown': None, 'burst': {'status': 'ok', 'credits': {'reset_credits': resets}, 'windows': [
        window(10, 300, timedelta(hours=2), 'five_hour'),
        window(33, WEEK, timedelta(hours=159)),
    ]}}
    assert quota.choose_claude_account(values, NOW) == expected


def test_ahead_of_pace_clears_an_older_refusal_cooldown(quota):
    snapshot(quota, 'codex', [window(33, WEEK, timedelta(hours=159), 'primary')], usage_credits=CREDITS)
    assert quota.apply_to_cooldowns(cooldown('usage_limited', NOW - timedelta(hours=1)), NOW) == {}


@pytest.mark.parametrize('used', [0, 30])
@pytest.mark.parametrize('observed_in', [-timedelta(hours=1), timedelta(0)])
def test_fresh_lapsed_window_leaves_cooldown_recovery_to_current_allowance(quota, used, observed_in):
    snapshot(quota, 'codex', [window(used, 300, -timedelta(hours=1), 'primary'),
                              window(10, WEEK, timedelta(days=3.5), 'secondary')], observed=NOW + observed_in)
    state = quota.route_states(NOW)['codex']
    assert state['state'] == 'reserve' and state['has_headroom'] is True
    assert state['summary'].startswith('secondary:')
    assert quota.apply_to_cooldowns(cooldown('usage_limited', NOW - timedelta(hours=2)), NOW) == {}


@pytest.mark.parametrize('used', [94, 95, 96, 97])
def test_lapsed_window_needs_headroom_before_clearing_a_usage_refusal(quota, used):
    snapshot(quota, 'codex', [window(used, 300, -timedelta(hours=1), 'primary'),
                              window(10, WEEK, timedelta(days=3.5), 'secondary')])
    refused = cooldown('usage_limited', NOW - timedelta(minutes=10))
    assert quota.apply_to_cooldowns(refused, NOW) == ({} if used < 95 else refused)


@pytest.mark.parametrize('seconds_after_reset', [0, 1, 299, 300, 301])
def test_reset_waits_for_clock_skew_before_clearing_a_cached_refusal(quota, seconds_after_reset):
    snapshot(quota, 'codex', [window(30, 300, -timedelta(seconds=seconds_after_reset), 'primary'),
                              window(10, WEEK, timedelta(days=3.5), 'secondary')],
             observed=NOW - timedelta(minutes=10),
             allowance={'ordinary_usage_allowed': False, 'limit_reached_type': 'primary'})
    refused = cooldown('usage_limited', NOW - timedelta(minutes=20))
    result = quota.apply_to_cooldowns(refused, NOW)
    if seconds_after_reset < quota.CLOCK_SKEW_SECONDS:
        assert result['codex/*']['cooling_until'] == iso(NOW - timedelta(minutes=10) + timedelta(hours=1))
    else:
        assert result == {}


def test_route_summary_includes_manually_banked_resets(quota):
    snapshot(quota, 'codex', [window(33, WEEK, timedelta(hours=159), 'primary')])
    quota.change_credits('codex', 'default', 1)
    assert '1 banked reset' in quota.route_states(NOW)['codex']['summary']


@pytest.mark.parametrize('credit,expected', [(None, 'unknown'), ({'balance': 26000}, 'unknown'),
                                            ({'unlimited': True}, 'shortage')])
def test_weekly_shortage_ranks_below_unknown_unless_credit_runway_is_known(quota, credit, expected):
    values = {'unknown': None, 'shortage': {'status': 'ok', 'usage_credits': [credit] if credit else None,
              'windows': [window(10, 300, timedelta(hours=2), 'five_hour'),
                          window(95, WEEK, timedelta(days=3))]}}
    assert quota.choose_claude_account(values, NOW) == expected


@pytest.mark.parametrize('refused', [False, True])
@pytest.mark.parametrize('balance', [60000, 100000])
def test_finite_credit_runway_requires_current_draw_and_enough_balance(quota, refused, balance):
    values = {'aaa': None, 'zcovered': {
        'status': 'ok', 'observed_at': iso(NOW),
        'allowance': {'ordinary_usage_allowed': not refused},
        'usage_credits': [{'balance': balance, 'unit': 'credits'}],
        'credit_anchor': {'balance': balance + 1000, 'observed_at': iso(NOW - timedelta(hours=1))},
        'windows': [window(10, 300, timedelta(hours=2), 'five_hour'), window(95, WEEK, timedelta(days=3))]}}
    assert quota.choose_claude_account(values, NOW) == ('zcovered' if refused and balance == 100000 else 'aaa')


@pytest.mark.parametrize('used,reset_in,clears', [(94, timedelta(days=3), True),
                                                (95, timedelta(days=3), False),
                                                (97, timedelta(days=3), False),
                                                (97, timedelta(hours=1), True)])
def test_usage_refusal_needs_headroom_or_allowance_through_reset(quota, used, reset_in, clears):
    snapshot(quota, 'codex', [window(used, WEEK, reset_in, 'primary')])
    refused = cooldown('usage_limited', NOW - timedelta(minutes=10))
    assert quota.apply_to_cooldowns(refused, NOW) == ({} if clears else refused)


def test_unstarted_weekly_allowance_still_ranks_above_unknown(quota):
    values = {'unknown': None, 'unstarted': {'status': 'ok', 'windows': [
        window(10, 300, timedelta(hours=2), 'five_hour'), window(0, WEEK)]}}
    assert quota.choose_claude_account(values, NOW) == 'unstarted'
