"""Builds the menu bar fixture reports from provider_quota.report() with synthetic readings.

Usage: python3 apps/quota-menubar/Fixtures/make_fixtures.py . apps/quota-menubar/Fixtures (from the repo root)."""
import importlib.util, json, os, sys, tempfile
from datetime import UTC, datetime, timedelta
from pathlib import Path

repo, out = Path(sys.argv[1]), Path(sys.argv[2])
temp = Path(tempfile.mkdtemp())
os.environ.update(HOME=str(temp / 'home'), AGENT_FABRIC_STATE_ROOT=str(temp / 'state'),
                  AGENT_FABRIC_INSTANCE_ROOT=str(temp / 'home/.agents'))
config = temp / 'home/.agents/config/telemetry.json'
config.parent.mkdir(parents=True)
config.write_text(json.dumps({'accounts': {'agy': {'google-4e7d2a91c05b': {'display_name': 'Michael'}}}}))
spec = importlib.util.spec_from_file_location('pq', repo / 'scripts/provider_quota.py')
q = importlib.util.module_from_spec(spec)
spec.loader.exec_module(q)
q.telemetry_host = lambda: 'laptop'
# Synthetic: no reading, date or amount here is a real account's.
NOW = datetime(2030, 3, 14, 1, 0, tzinfo=UTC)
H, D = timedelta(hours=1), timedelta(days=1)
def iso(moment):
    return moment.isoformat().replace('+00:00', 'Z') if moment else None


def at(text):
    return datetime.fromisoformat(text.replace('Z', '+00:00'))


def w(name, used, minutes, reset, **extra):
    return q.normalise_window({'name': name, 'raw_kind': name, 'used_percent': used, 'window_minutes': minutes,
                               'reset_at': reset if isinstance(reset, str) or reset is None else iso(reset), **extra})


def v(provider, account, plan, windows, observed=NOW - timedelta(minutes=3), status='ok', unreported=(), priority=None,
      due=None, **extra):
    return {'schema': q.SCHEMA, 'provider': provider, 'account': account, 'plan': plan, 'host': 'laptop', 'active': True,
            'priority': priority, 'observed_at': iso(observed), 'attempted_at': iso(observed), 'source': 'fixture',
            'status': status, 'error': None, 'unreported': list(unreported), 'credits': None, 'windows': windows,
            **({'schedule': {'next_due_at': iso(NOW + due), 'reason': 'active'}} if due else {}), **extra}


current = [
    v('agy', 'google-4e7d2a91c05b', None, [w('gemini.weekly', 100, 10080, NOW + 2 * D + 3 * H),
                                           w('claude.weekly', 6, 10080, NOW + 6 * D + 14 * H),
                                           w('claude.5h', 12, 300, NOW + 3 * H)],
      unreported=['gemini.5h'], due=timedelta(minutes=6)),
    v('claude', 'anese', 'Max', [w('five_hour', 3, 300, NOW + 4 * H + timedelta(minutes=40)),
                                 w('seven_day', 11, 10080, NOW + 6 * D + 20 * H),
                                 w('iguana_necktie', 30, None, None, used_amount=30, limit_amount=100, currency='USD'),
                                 w('weekly_all:2', 11, 10080, NOW + 6 * D + 20 * H)], priority=1, due=timedelta(minutes=4)),
    v('claude', 'michael', 'Max', [w('seven_day', 100, 10080, NOW + 2 * D + 23 * H), w('five_hour', 0, 300, None)], priority=2),
    v('claude', 'pro', 'Pro', [w('seven_day', 41, 10080, NOW + 3 * D + 18 * H)], priority=3),
    v('codex', 'default', 'prolite', [w('primary', 78, 10080, NOW + 5 * D + 4 * H)], unreported=['secondary']),
    v('cursor', 'default', 'pro_student', [w('billing_cycle', 100, 44640, NOW + 22 * D)]),
    v('opencode', 'go', 'Go', [w('seven_day', 100, 10080, NOW + 3 * D + 21 * H), w('thirty_day', 19, 43200, NOW + 16 * D),
                               w('five_hour', 0, 300, None)]),
    # Signed out after draining it: Gemini came back yesterday, so the account is back since then.
    v('agy', 'google-5d1e0c7a9b24', None, [w('gemini.weekly', 100, 10080, NOW - 20 * H),
                                           w('claude.weekly', 100, 10080, NOW + 4 * D + 7 * H)],
      observed=NOW - 3 * D, status='signed_out'),
]
tall = current + [
    v('claude', f'team-{n}', 'Max', [w('five_hour', 20 + n * 9, 300, NOW + timedelta(hours=n)),
                                     w('seven_day', 30 + n * 6, 10080, NOW + timedelta(days=n + 1))], priority=5 + n)
    for n in range(1, 7)] + [
    v('codex', 'work', 'pro', [w('primary', 35, 300, NOW + timedelta(hours=2)), w('secondary', 61, 10080, NOW + timedelta(days=4))]),
    v('kiro', 'default', 'pro', [w('monthly', 18, 43200, NOW + timedelta(days=20))]),
    v('copilot', 'default', 'pro', [w('premium_interactions', 92, 43200, NOW + timedelta(days=9))]),
    v('fakeprov', 'lab', 'odd_tier', [w('odd', 40, 4320, NOW + timedelta(days=2)), w('mystery', 12, None, None)]),
    v('agy', 'google-a07c33e1f918', None, [w('gemini.weekly', 100, 10080, NOW + timedelta(days=1, hours=13)),
                                           w('claude.weekly', 100, 10080, NOW + timedelta(days=3))],
      observed=NOW - timedelta(days=2), status='signed_out'),
    v('claude', 'old', 'Max', [], status='unavailable', error='token expired; run claude login'),
]
activity = {'claude/anese': iso(NOW - timedelta(minutes=10)), 'agy/google-4e7d2a91c05b': iso(NOW - timedelta(hours=2))}


def build(values):
    return q.report(values, NOW, activity=activity)


def old(report):
    """The same report as an older CLI wrote it: no display vocabulary, pools, headline or display settings."""
    report = json.loads(json.dumps(report))
    for key in ('display', 'headline'):
        report.pop(key)
    report['accounts'] = [row for row in report['accounts'] if row['state'] != 'signed_out']
    for row in report['accounts']:
        for key in ('provider_name', 'display_name', 'plan_label', 'display_window', 'back_at'):
            row.pop(key)
        for window in row['windows']:
            for key in ('pool', 'pool_label', 'reset_passed'):
                window.pop(key, None)
    return report


out.mkdir(parents=True, exist_ok=True)
reports = {'current': build(current), 'tall': build(tall)}
reports['old'] = old(reports['current'])
for name, report in reports.items():
    (out / f'{name}.json').write_text(json.dumps(report, indent=1) + '\n')
    print(name, report['headline'] if 'headline' in report else '-', len(report['accounts']))
