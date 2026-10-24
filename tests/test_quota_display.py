"""Quota display vocabulary, pools, the display rule, the menu bar headline and signed-out accounts."""

from datetime import timedelta
import io
import json
from pathlib import Path

import pytest

import quota_display
from tests.support.quota import NOW, configure, quota, snapshot, window


WEEK = 7 * 24 * 60
ANESE, MICHAEL = 'anese@example.com', 'michael@example.com'


def iso(moment):
    return moment.isoformat().replace('+00:00', 'Z')


def value(quota, provider, account, windows, observed=NOW, **extra):
    return snapshot(quota, provider, windows, observed, account, **extra)


def row(quota, *values, **settings):
    """The first account of a report built from these snapshots, with no local activity."""
    return quota_display.report(vars(quota), list(values), NOW, activity={})['accounts'][0]


def shown(account):
    return next(item for item in account['windows'] if item['name'] == account['display_window'])


# Vocabulary ----------------------------------------------------------------------------------

def test_codex_primary_of_a_week_reads_weekly_with_display_names(quota):
    codex = row(quota, value(quota, 'codex', 'default', [window(83, WEEK, timedelta(days=5), 'primary')], plan='prolite'))
    assert (codex['provider_name'], codex['display_name'], codex['plan_label']) == ('Codex', None, 'Pro Lite')
    assert codex['windows'][0]['label'] == 'Weekly' and codex['display_window'] == 'primary'


def test_names_degrade_to_sane_title_and_sentence_case(quota, tmp_path):
    assert quota_display.provider_name('agy') == 'Antigravity' and quota_display.provider_name('opencode') == 'OpenCode'
    assert quota_display.provider_name('fakeprov') == 'Fakeprov' and quota_display.provider_name('big_model') == 'Big Model'
    assert [quota_display.plan_label(plan) for plan in ('pro_student', 'prolite', 'Max', 'go', None)] == [
        'Pro (Student)', 'Pro Lite', 'Max', 'Go', None]
    labels = quota_display.window_labels(vars(quota), )
    cases = [(300, 'five_hour', '5-hour'), (1440, 'daily', 'Daily'), (4320, 'odd', '3-day (Odd)'),
             (90, 'burst', '90-minute (Burst)'), (44640, 'billing_cycle', 'Monthly'), (43200, 'thirty_day', 'Monthly'),
             (WEEK, 'seven_day_opus', 'Weekly (Opus)'), (WEEK, 'weekly_scoped:fable', 'Weekly (Fable)'),
             (WEEK, 'weekly_all:2', 'Weekly'), (None, 'odd_window_name', 'Odd window name'),
             (None, 'iguana_necktie', 'Cloud session credits')]
    for minutes, name, label in cases:
        assert quota_display.window_label({'name': name, 'window_minutes': minutes}, labels) == label
    assert quota_display.window_label({'name': 'claude.5h', 'window_minutes': 300, 'pool': 'claude'}, labels) == '5-hour'
    configure(tmp_path, {'accounts': {'claude': {'anese': {'identity_dir': '~/.claude'},
                                                 'max-a': {'identity_dir': '~/.claude-a', 'display_name': 'Work'}}}})
    entries = quota_display.configured_entries(vars(quota), )
    assert quota_display.display_name(vars(quota), 'claude', 'anese', entries) == 'Anese'
    assert quota_display.display_name(vars(quota), 'claude', 'max-a', entries) == 'Work'
    assert quota_display.display_name(vars(quota), 'claude', 'default', entries) is None
    assert quota_display.display_name(vars(quota), 'agy', 'google-4e7d2a91c05b', entries) is None
    # Labels themselves stay lowercase: they key snapshots and routing.
    assert {account.label for account in quota.accounts() if account.provider == 'claude'} == {'anese', 'max-a'}
    assert all('display_name' not in account.options for account in quota.accounts())


def test_a_restated_limit_is_dropped_and_a_different_one_kept(quota):
    weekly = window(9, WEEK, timedelta(days=6), 'seven_day')
    same = window(9, WEEK, timedelta(days=6), 'weekly_all:2', advisory=False)
    other = window(12, WEEK, timedelta(days=6), 'weekly_all:3')
    claude = row(quota, value(quota, 'claude', 'anese', [weekly, same, other]))
    assert [item['name'] for item in claude['windows']] == ['seven_day', 'weekly_all:3']


# Display rule --------------------------------------------------------------------------------

@pytest.mark.parametrize('short, shown_name', [(89, 'seven_day'), (90, 'five_hour'), (100, 'five_hour')])
def test_the_longest_window_shows_unless_a_shorter_one_is_nearly_spent(quota, short, shown_name):
    claude = row(quota, value(quota, 'claude', 'anese', [window(short, 300, timedelta(hours=1), 'five_hour'),
                                                          window(30, WEEK, timedelta(days=2))]))
    assert claude['display_window'] == shown_name
    assert quota_display.report(vars(quota), [], NOW, activity={})['display'] == {'short_window_alert_percent': 90}


def test_the_alert_threshold_is_a_quota_setting(quota, tmp_path):
    configure(tmp_path, {'quota': {'display': {'short_window_alert_percent': 80}}})
    claude = row(quota, value(quota, 'claude', 'anese', [window(85, 300, timedelta(hours=1), 'five_hour'),
                                                          window(30, WEEK, timedelta(days=2))]))
    assert claude['display_window'] == 'five_hour'
    assert quota_display.report(vars(quota), [], NOW, activity={})['display'] == {'short_window_alert_percent': 80}


def test_a_window_whose_reset_passed_is_never_shown_as_current(quota):
    claude = row(quota, value(quota, 'claude', 'anese', [window(95, 300, timedelta(minutes=-5), 'five_hour'),
                                                          window(30, WEEK, timedelta(days=2))], observed=NOW - timedelta(minutes=10)))
    passed = next(item for item in claude['windows'] if item['name'] == 'five_hour')
    assert passed['reset_passed'] is True and claude['display_window'] == 'seven_day'
    only = row(quota, value(quota, 'claude', 'anese', [window(40, WEEK, timedelta(minutes=-5))]))
    assert only['display_window'] is None


# Pools ---------------------------------------------------------------------------------------

def agy_windows(gemini=100, claude=6, short=12):
    return [window(gemini, WEEK, timedelta(days=1, hours=16), 'gemini.weekly'),
            window(claude, WEEK, timedelta(days=6, hours=4), 'claude.weekly'),
            window(short, 300, timedelta(hours=1, minutes=40), 'claude.5h')]


def test_agy_with_one_pool_exhausted_stays_usable_and_names_the_usable_pool(quota, tmp_path, monkeypatch, capsys):
    agy = row(quota, value(quota, 'agy', 'google-4e7d2a91c05b', agy_windows()))
    assert agy['state'] not in ('exhausted', 'signed_out') and agy['back_at'] is None
    assert (agy['provider_name'], agy['display_name']) == ('Antigravity', None)
    assert {item['name']: (item['pool'], item['pool_label']) for item in agy['windows']} == {
        'gemini.weekly': ('gemini', 'Gemini models'), 'claude.weekly': ('claude', 'Claude models'),
        'claude.5h': ('claude', 'Claude models')}
    assert agy['display_window'] == 'claude.weekly' and shown(agy)['label'] == 'Weekly'
    report = quota_display.report(vars(quota), [value(quota, 'agy', 'google-4e7d2a91c05b', agy_windows())], NOW, activity={})
    assert report['headline']['account'] == 'agy/google-4e7d2a91c05b'
    assert report['placement_preference']['agy'] == ['google-4e7d2a91c05b']
    quota_display.render(vars(quota), report)
    out = capsys.readouterr().out
    assert 'Claude models · Weekly' in out and 'Gemini models · Weekly' in out
    # Routing is unchanged: agy quota is report-only and sets no cooldown.
    assert quota.provider('agy').ADAPTER is None


def test_agy_with_every_pool_exhausted_is_back_when_the_first_pool_returns(quota):
    windows = [window(100, WEEK, timedelta(days=2), 'gemini.weekly'), window(100, WEEK, timedelta(days=6), 'claude.weekly'),
               window(100, 300, timedelta(hours=1), 'claude.5h')]
    agy = row(quota, value(quota, 'agy', 'google-4e7d2a91c05b', windows))
    assert agy['state'] == 'exhausted' and agy['back_at'] == iso(NOW + timedelta(days=2))
    assert agy['display_window'] == 'gemini.weekly'
    assert quota_display.report(vars(quota), [value(quota, 'agy', 'google-4e7d2a91c05b', windows)], NOW, activity={})['headline']['account'] is None


def test_a_pool_needing_both_windows_back_returns_at_the_later_reset(quota):
    windows = [window(100, WEEK, timedelta(days=3), 'gemini.weekly'), window(100, WEEK, timedelta(days=6), 'claude.weekly'),
               window(100, 300, timedelta(hours=1), 'claude.5h')]
    agy = row(quota, value(quota, 'agy', 'google-4e7d2a91c05b', windows))
    assert agy['back_at'] == iso(NOW + timedelta(days=3))


def test_pools_never_merge_matching_windows(quota):
    windows = [window(0, 300, None, 'gemini.5h'), window(0, 300, None, 'claude.5h')]
    agy = row(quota, value(quota, 'agy', 'google-4e7d2a91c05b', windows))
    assert len(agy['windows']) == 2


# Headline ------------------------------------------------------------------------------------

def readings(quota):
    return [value(quota, 'claude', 'anese', [window(2, 300, timedelta(hours=4), 'five_hour'), window(9, WEEK, timedelta(days=6))]),
            value(quota, 'claude', 'michael', [window(100, WEEK, timedelta(days=3))]),
            value(quota, 'codex', 'default', [window(83, WEEK, timedelta(days=5), 'primary')], priority=1),
            value(quota, 'agy', 'google-4e7d2a91c05b', agy_windows())]


def test_headline_recent_is_the_usable_account_used_most_recently(quota):
    activity = {'claude/michael': 300, 'claude/anese': 200, 'codex/default': 100}
    headline = quota_display.report(vars(quota), readings(quota), NOW, activity=activity)['headline']
    # Michael is exhausted, so the most recent usable account wins, shown by its display window.
    assert headline == {'mode': 'recent', 'account': 'claude/anese', 'window': 'seven_day'}


def test_headline_without_activity_falls_back_to_priority_and_to_nothing(quota):
    assert quota_display.report(vars(quota), readings(quota), NOW, activity={})['headline']['account'] == 'codex/default'
    spent = [value(quota, 'claude', 'michael', [window(100, WEEK, timedelta(days=3))])]
    assert quota_display.report(vars(quota), spent, NOW, activity={})['headline'] == {'mode': 'recent', 'account': None, 'window': None}


@pytest.mark.parametrize('mode, account', [('worst', 'codex/default'), ('none', None),
                                           ('agy/google-4e7d2a91c05b', 'agy/google-4e7d2a91c05b'),
                                           ('claude/gone', 'codex/default'), ('bogus', 'codex/default')])
def test_headline_modes(quota, tmp_path, mode, account):
    configure(tmp_path, {'quota': {'menubar': {'headline': mode}}})
    headline = quota_display.report(vars(quota), readings(quota), NOW, activity={})['headline']
    assert headline['account'] == account


# Signed-out accounts -------------------------------------------------------------------------

def agy_usage(fraction=0.93):
    return {'status': 'SUCCESS', 'command': {'data': {'groups': [
        {'name': 'Gemini Models', 'buckets': [{'id': 'gemini-weekly', 'window': 'weekly', 'remaining_fraction': 0,
                                               'reset_time': iso(NOW + timedelta(days=2))}]},
        {'name': 'Claude and GPT models', 'buckets': [{'id': '3p-weekly', 'window': 'weekly',
                                                       'remaining_fraction': fraction,
                                                       'reset_time': iso(NOW + timedelta(days=3))}]}]}}}


class Agy:
    """agy on a stand-in Keychain: its logs say who is signed in, and its print-mode CLI answers for them."""

    def __init__(self, quota, monkeypatch, tmp_path):
        self.quota, self.runs, self.logs, self.count = quota, [], tmp_path / 'home/.gemini/antigravity-cli/log', 0
        keychain = tmp_path / 'login.keychain-db'
        keychain.write_text('')
        original = quota.identity.default_identity
        monkeypatch.setattr(quota.identity, 'default_identity', lambda name: keychain if name == 'agy' else original(name))
        self.logs.mkdir(parents=True)
        agy = self

        class Process:
            def __init__(self, argv, **kwargs):
                agy.runs.append(agy.email)
                # No email means a response with no recognised sign-in in its log.
                Path(argv[argv.index('--log-file') + 1]).write_text(
                    f'OAuth: authenticated successfully as {agy.email}\n' if agy.email else 'started\n')
                self.stdout, self.returncode = io.BytesIO(json.dumps(agy.payload).encode()), 0

            def kill(self):
                self.returncode = -9

            def wait(self, timeout=None):
                return self.returncode
        monkeypatch.setattr(quota, 'executable', lambda name: '/bin/' + name)
        monkeypatch.setattr(quota, 'open_process', Process)

    def sign_in(self, email, payload=None):
        self.email, self.payload, self.count = email, payload or agy_usage(), self.count + 1
        (self.logs / f'cli-20261022_09{self.count:02d}00.log').write_text(
            f'OAuth: authenticated successfully as {email}\n')
        return self.quota.provider('agy').fingerprint(email)

    def accounts(self):
        return {account.label: account for account in self.quota.accounts() if account.provider == 'agy'}

    def tick(self, now):
        return {account.label: self.quota.refresh(account, now, force=True) for account in self.accounts().values()}


def test_switching_agy_accounts_keeps_the_first_as_signed_out_and_never_fetches_it(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    michael = agy.sign_in(MICHAEL, agy_usage(0))
    agy.tick(NOW - timedelta(hours=1))
    anese = agy.sign_in(ANESE)
    agy.runs.clear()
    values = agy.tick(NOW)
    assert agy.runs == [ANESE]  # only the signed-in account is fetched
    accounts = agy.accounts()
    assert accounts[michael].signed_out and not accounts[michael].active
    assert accounts[anese].active and not accounts[anese].signed_out
    report = quota_display.report(vars(quota), list(values.values()), NOW, activity={})
    rows = {row['account']: row for row in report['accounts']}
    assert rows[michael]['status'] == rows[michael]['state'] == 'signed_out'
    assert rows[michael]['observed_at'] == iso(NOW - timedelta(hours=1))
    assert rows[michael]['back_at'] == iso(NOW + timedelta(days=2))  # both pools spent: back when Gemini resets
    assert {item['reset_at'] for item in rows[michael]['windows']} == {iso(NOW + timedelta(days=2)), iso(NOW + timedelta(days=3))}
    # Each reading stays under its own fingerprint.
    assert rows[anese]['windows'][1]['used_percent'] == pytest.approx(7)
    assert rows[michael]['windows'][1]['used_percent'] == 100
    assert report['placement_preference']['agy'] == [anese] and report['headline']['account'] == f'agy/{anese}'
    # A forced refresh of the signed-out account still never runs agy.
    agy.runs.clear()
    assert quota.refresh(accounts[michael], NOW, force=True)['status'] == 'signed_out' and agy.runs == []


def test_a_signed_out_account_shows_back_since_once_its_reset_passed(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    michael = agy.sign_in(MICHAEL, agy_usage(0))
    agy.tick(NOW)
    agy.sign_in(ANESE)
    later = NOW + timedelta(days=4)
    report = quota_display.report(vars(quota), list(agy.tick(later).values()), later, activity={})
    row = next(row for row in report['accounts'] if row['account'] == michael)
    assert all(item.get('reset_passed') for item in row['windows'])
    assert row['back_at'] == iso(NOW + timedelta(days=2)) and row['display_window'] is None


def test_signing_back_in_resumes_the_same_history(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    michael = agy.sign_in(MICHAEL)
    agy.tick(NOW - timedelta(hours=2))
    anese = agy.sign_in(ANESE)
    agy.tick(NOW - timedelta(hours=1))
    agy.sign_in(MICHAEL)
    accounts = agy.accounts()
    assert accounts[michael].active and accounts[anese].signed_out
    before = quota.load_snapshot('agy', michael, accounts[michael].identity_dir)
    assert before['observed_at'] == iso(NOW - timedelta(hours=2))
    assert agy.tick(NOW)[michael]['observed_at'] == iso(NOW)
    # The label and snapshot are the account's own, so the same file carries on.
    assert quota.cache_path('agy', michael, identity_dir=accounts[michael].identity_dir).exists()


def test_forget_refuses_the_signed_in_account_and_removes_a_signed_out_one(quota, monkeypatch, tmp_path, capsys):
    agy = Agy(quota, monkeypatch, tmp_path)
    michael = agy.sign_in(MICHAEL)
    agy.tick(NOW)
    anese = agy.sign_in(ANESE)
    agy.tick(NOW)
    assert quota.main(['forget', f'agy/{anese}']) == 1
    assert 'signed in' in capsys.readouterr().err
    assert quota.main(['forget', 'agy/google-000000000000']) == 1
    capsys.readouterr()
    assert quota.main(['forget', f'agy/{michael}', '--json']) == 0
    printed = json.loads(capsys.readouterr().out)
    assert [row['account'] for row in printed['accounts'] if row['provider'] == 'agy'] == [anese]
    assert set(agy.accounts()) == {anese}


def test_configured_fingerprints_name_accounts_and_others_still_appear(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    michael = agy.sign_in(MICHAEL)
    agy.tick(NOW)
    anese = agy.sign_in(ANESE)
    configure(tmp_path, {'accounts': {'agy': {michael: {'display_name': 'Michael'}}}})
    accounts = agy.accounts()
    assert accounts[michael].signed_out and accounts[anese].active
    report = quota_display.report(vars(quota), list(agy.tick(NOW).values()), NOW, activity={})
    names = {row['account']: row['display_name'] for row in report['accounts']}
    assert names == {michael: 'Michael', anese: None}


def test_remembered_accounts_unseen_too_long_are_no_longer_listed(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    michael = agy.sign_in(MICHAEL)
    agy.tick(NOW - timedelta(days=10))
    anese = agy.sign_in(ANESE)
    assert set(agy.accounts()) == {michael, anese}
    configure(tmp_path, {'quota': {'remembered': {'forget_after_days': 7}}})
    monkeypatch.setattr(quota, 'utcnow', lambda: NOW)
    assert set(agy.accounts()) == {anese}


def test_local_activity_never_credits_a_signed_out_account(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    michael = agy.sign_in(MICHAEL)
    agy.tick(NOW)
    anese = agy.sign_in(ANESE)
    activity = quota.local_activity()
    assert f'agy/{anese}' in activity and f'agy/{michael}' not in activity


# Review repairs ------------------------------------------------------------------------------

def test_an_expired_exhausted_window_never_makes_the_account_exhausted(quota):
    reading = value(quota, 'claude', 'anese', [window(100, WEEK, timedelta(hours=-1)),
                                                window(20, 300, timedelta(hours=2), 'five_hour')], observed=NOW - timedelta(hours=2))
    report = quota_display.report(vars(quota), [reading], NOW, activity={})
    claude = report['accounts'][0]
    assert claude['state'] != 'exhausted' and claude['back_at'] is None
    assert claude['display_window'] == 'five_hour'
    assert report['headline']['account'] == 'claude/anese'
    # The cached history stays, marked as reset.
    weekly = next(item for item in claude['windows'] if item['name'] == 'seven_day')
    assert weekly['used_percent'] == 100 and weekly['reset_passed'] is True


@pytest.mark.parametrize('weekly, short, shown_name', [(95, 90, 'five_hour'), (95, 50, 'seven_day'), (99, 92, 'five_hour')])
def test_a_short_window_at_the_alert_takes_the_display_even_below_the_long_one(quota, weekly, short, shown_name):
    claude = row(quota, value(quota, 'claude', 'anese', [window(short, 300, timedelta(hours=1), 'five_hour'),
                                                          window(weekly, WEEK, timedelta(days=2))]))
    assert claude['display_window'] == shown_name


def test_an_alias_on_a_shared_store_keeps_its_own_account_when_another_signs_in(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    configure(tmp_path, {'accounts': {'agy': {'work': {'display_name': 'Work'}}}})
    michael = agy.sign_in(MICHAEL, agy_usage(0))
    assert set(agy.accounts()) == {'work'} and agy.accounts()['work'].active
    agy.tick(NOW - timedelta(hours=1))
    anese = agy.sign_in(ANESE)
    accounts = agy.accounts()
    # The alias is bound to the account it first read, so the newcomer gets its own fingerprint.
    assert set(accounts) == {'work', anese}
    assert accounts['work'].signed_out and accounts[anese].active
    agy.runs.clear()
    values = agy.tick(NOW)
    assert agy.runs == [ANESE]
    assert values['work']['status'] == 'signed_out' and values['work']['windows'][1]['used_percent'] == 100
    assert values[anese]['windows'][1]['used_percent'] == pytest.approx(7)
    # Signing back in resumes the alias, and the other account is remembered.
    agy.sign_in(MICHAEL)
    accounts = agy.accounts()
    assert accounts['work'].active and accounts[anese].signed_out and michael not in accounts


@pytest.mark.parametrize('email, expected_error, expect_fingerprint', [
    (ANESE, None, False), (None, 'auth', True)],
    ids=['another-account', 'no-account-identity'])
def test_a_shared_store_reading_without_its_bound_account_is_refused(quota, monkeypatch, tmp_path,
                                                                    email, expected_error, expect_fingerprint):
    agy = Agy(quota, monkeypatch, tmp_path)
    configure(tmp_path, {'accounts': {'agy': {'work': {}}}})
    michael = agy.sign_in(MICHAEL, agy_usage(0))
    agy.tick(NOW - timedelta(hours=1))
    work = agy.accounts()['work']
    agy.email, agy.payload = email, agy_usage()  # the sign-in changed after accounts were listed
    value = quota.refresh(work, NOW, force=True)
    assert value['status'] == 'stale'
    if expected_error:
        assert value['fetch']['error_kind'] == expected_error
    assert value['windows'][1]['used_percent'] == 100
    if expect_fingerprint:
        assert value['fingerprint'] == michael


def test_concurrent_refreshes_of_an_unbound_alias_bind_it_to_the_first_reading(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    configure(tmp_path, {'accounts': {'agy': {'work': {}}}})
    michael = agy.sign_in(MICHAEL, agy_usage(0))
    work = agy.accounts()['work']  # listed while unbound, as two concurrent ticks would see it
    first = quota.refresh(work, NOW - timedelta(minutes=1), force=True)
    assert first['fingerprint'] == michael
    agy.email, agy.payload = ANESE, agy_usage()  # the second tick runs after a switch
    second = quota.refresh(work, NOW, force=True)
    assert second['status'] == 'stale' and second['fetch']['error_kind'] == 'auth'
    assert second['windows'][1]['used_percent'] == 100 and second['fingerprint'] == michael


def test_a_fingerprint_in_the_alias_entry_rebinds_it_deliberately(quota, monkeypatch, tmp_path):
    agy = Agy(quota, monkeypatch, tmp_path)
    configure(tmp_path, {'accounts': {'agy': {'work': {}}}})
    agy.sign_in(MICHAEL, agy_usage(0))
    agy.tick(NOW - timedelta(hours=1))
    anese = agy.sign_in(ANESE)
    configure(tmp_path, {'accounts': {'agy': {'work': {'fingerprint': anese}}}})
    value = quota.refresh(agy.accounts()['work'], NOW, force=True)
    assert value['status'] == 'ok' and value['fingerprint'] == anese
    assert value['windows'][1]['used_percent'] == pytest.approx(7)


@pytest.mark.parametrize('measured', [True, False])
def test_post_reset_burst_summary_includes_credit_and_banked_reset_runway(quota, measured):
    reading = value(quota, 'codex', 'default', [window(33, WEEK, timedelta(hours=159), 'primary')],
                    usage_credits=[{'balance': 26000, 'unit': 'credits'}],
                    credits={'reset_credits': 1, 'source': 'provider'},
                    credit_anchor={'balance': 27000, 'observed_at': iso(NOW - timedelta(hours=1))} if measured else {})
    account = row(quota, reading)
    assert account['state'] == 'ahead_of_pace'
    summary = account['summary']
    assert 'ahead of pace; allowance lasts about 18h' in summary
    assert 'credit runway unknown' in summary
    assert '26h on credits' not in summary
    assert '1 banked reset' in summary
    assert 'deficit' not in summary and 'runs out' not in summary
    assert account['windows'][0]['pace']['summary'] in summary


@pytest.mark.parametrize('reset_in', [timedelta(0), timedelta(hours=-1)])
@pytest.mark.parametrize('current', [False, True])
def test_pre_reset_refusal_is_stale_and_never_claims_allowance_spent(quota, reset_in, current):
    reset_in -= timedelta(seconds=quota.CLOCK_SKEW_SECONDS)
    windows = [window(100, WEEK, reset_in, 'primary')]
    if current:
        windows.append(window(20, 300, timedelta(hours=2), 'secondary'))
    reading = value(quota, 'codex', 'default', windows, observed=NOW - timedelta(hours=2),
                    usage_credits=[{'balance': 26000, 'unit': 'credits'}],
                    allowance={'ordinary_usage_allowed': False, 'limit_reached_type': 'primary'})
    account = row(quota, reading)
    assert account['status'] == 'stale'
    assert account['state'] not in ('credit_only', 'exhausted')
    assert 'stale' in account['summary'] and 'allowance spent' not in account['summary']
    assert not account.get('drawing_credits')
    assert account['display_window'] == ('secondary' if current else None)


@pytest.mark.parametrize('used', [60, 100])
@pytest.mark.parametrize('reset_in', [timedelta(0), timedelta(hours=-1)])
def test_fresh_post_reset_refusal_remains_exhausted(quota, used, reset_in):
    reading = value(quota, 'codex', 'default', [window(used, WEEK, reset_in, 'primary')],
                    allowance={'ordinary_usage_allowed': False})
    account = row(quota, reading)
    assert account['state'] == 'exhausted'
    assert account['status'] == 'ok'
    assert 'stale' not in account['summary']


@pytest.mark.parametrize('used', [0, 30])
@pytest.mark.parametrize('observed_in', [-timedelta(hours=1), timedelta(0)])
def test_fresh_non_exhausted_lapsed_window_does_not_bind_the_account(quota, used, observed_in):
    reading = value(quota, 'codex', 'default', [window(used, 300, -timedelta(hours=1), 'primary'),
                                              window(10, WEEK, timedelta(days=3.5), 'secondary')],
                    observed=NOW + observed_in)
    account = row(quota, reading)
    assert account['state'] == 'reserve' and account['status'] == 'ok'
    assert account['summary'].startswith('secondary:')
    assert account['display_window'] == 'secondary'


def test_fresh_reading_whose_windows_all_reset_is_unknown_not_unreported(quota):
    reading = value(quota, 'codex', 'default', [window(30, 300, -timedelta(hours=1), 'primary'),
                                              window(40, WEEK, -timedelta(hours=1), 'secondary')])
    account = row(quota, reading)
    assert account['state'] == 'unknown'
    assert account['summary'] == 'reset passed; awaiting a fresh reading'


@pytest.mark.parametrize('used,reached', [(100, None), (30, 'primary'), (30, 'five_hour'), (30, None)])
def test_fresh_exhaustion_or_refusal_still_binds_a_lapsed_window(quota, used, reached):
    reading = value(quota, 'codex', 'default', [window(used, 300, -timedelta(hours=1), 'primary', raw_kind='five_hour'),
                                              window(10, WEEK, timedelta(days=3.5), 'secondary')],
                    **({'allowance': {'ordinary_usage_allowed': False,
                                     **({'limit_reached_type': reached} if reached else {})}}
                       if used < 100 else {}))
    account = row(quota, reading)
    assert account['state'] == 'exhausted' and account['status'] == 'ok'
    assert account['summary'].startswith('primary:')


@pytest.mark.parametrize('reached,all_reset,spend_cap,unreported,exhausted', [
    ('primary', False, False, [], False),
    ('primary', False, False, ['unrelated_window'], False),
    ('secondary', False, False, [], True),
    ('unreported_window', True, False, ['unreported_window'], True),
    (None, False, False, [], True),
    (None, True, False, [], False),
    (None, True, False, ['missing'], True),
    ('primary', True, True, [], True),
    ('workspace_member_credits_depleted', True, False, [], True),
])
def test_reset_only_drops_the_refusal_for_its_own_window(quota, reached, all_reset, spend_cap, unreported, exhausted):
    reading = value(quota, 'codex', 'default', [
        window(60, WEEK, timedelta(hours=-1), 'primary'),
        window(20, 300, timedelta(hours=-1) if all_reset else timedelta(hours=2), 'secondary')],
        observed=NOW - timedelta(hours=2), unreported=unreported,
        allowance={'ordinary_usage_allowed': False, 'spend_control_reached': spend_cap,
                   **({'limit_reached_type': reached} if reached else {})})
    account = row(quota, reading)
    assert (account['state'] == 'exhausted') is exhausted


def test_credit_runway_uses_the_anchor_while_credits_are_being_drawn(quota):
    reading = value(quota, 'codex', 'default', [window(64.2, WEEK, timedelta(hours=135.7), 'primary')],
                    allowance={'ordinary_usage_allowed': False}, usage_credits=[{'balance': 26000, 'unit': 'credits'}],
                    credit_anchor={'balance': 27000, 'observed_at': iso(NOW - timedelta(hours=1))})
    account = row(quota, reading)
    assert account['state'] == 'credit_only'
    assert 'then about 26h on credits' in account['windows'][0]['pace']['summary']


def test_allowance_lasting_to_reset_does_not_append_credit_runway(quota):
    reading = value(quota, 'codex', 'default', [window(90, WEEK, timedelta(hours=1), 'primary')],
                    usage_credits=[{'balance': 26000, 'unit': 'credits'}])
    account = row(quota, reading)
    assert account['windows'][0]['pace']['will_last_to_reset'] is True
    assert 'allowance lasts until reset' in account['summary']
    assert 'then' not in account['summary'] and 'credit runway' not in account['summary']
