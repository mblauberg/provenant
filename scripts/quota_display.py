"""Quota display vocabulary, reports, menu bar headlines and text rendering.

Quota-dependent helpers take the caller's live namespace, including alias-loaded modules.
"""

import re


# Providers name some windows by codename; `quota.window_labels` in telemetry.json adds or overrides labels.
WINDOW_LABELS = {'iguana_necktie': 'Cloud session credits'}
# One display vocabulary for every reader of the report: the menu bar, `provenant quota` and the console.
# An unknown provider or plan falls back to title case.
PROVIDER_NAMES = {'agy': 'Antigravity', 'claude': 'Claude', 'codex': 'Codex', 'copilot': 'Copilot',
                  'cursor': 'Cursor', 'kiro': 'Kiro', 'opencode': 'OpenCode'}
PLAN_NAMES = {'prolite': 'Pro Lite', 'pro_lite': 'Pro Lite', 'pro_student': 'Pro (Student)'}
LENGTH_LABELS = {60: 'Hourly', 300: '5-hour', 1440: 'Daily', 10080: 'Weekly'}
MONTH_MINUTES = (28 * 1440, 31 * 1440)  # a calendar month or billing cycle
# Name parts that only restate a window's length or slot; what remains qualifies its label, as in `Weekly (Opus)`.
LENGTH_WORDS = frozenset({'five', 'seven', 'thirty', 'hour', 'hours', 'day', 'days', 'week', 'month', 'hourly',
                          'daily', 'weekly', 'monthly', 'primary', 'secondary', 'billing', 'cycle', 'session',
                          'all', 'scoped', 'window', 'limit'})
SHORT_WINDOW_ALERT = 90  # a shorter window this used is shown instead of the longest (`quota.display`)
HEADLINES = ('recent', 'worst', 'none')  # `quota.menubar.headline`, or a pinned `<provider>/<account>`


def window_length(quota, minutes):
    return quota['duration'](minutes * 60) if minutes else '-'


def display(quota, rows, now, alert=SHORT_WINDOW_ALERT):
    """The window a reader should see: an exhausted one binds; else a shorter window at `alert`% or more, the
    most used, even when the longest is higher, since the short one runs out first; else the longest, since
    weekly and monthly headroom decides more than a 5-hour window does. A window whose reset has passed is never
    shown as current."""
    current = [row for row in rows if quota['sets_state'](row) and not quota['reset_passed'](row, now)]
    spent = [row for row in current if row['pace']['state'] == 'exhausted']
    if spent:
        return max(spent, key=lambda row: row.get('reset_at') or '9999')
    longest = max(current, key=lambda row: (row['window_minutes'] or 0, row['used_percent']), default=None)
    alerts = [row for row in current if row['used_percent'] >= alert and row is not longest and
              (row['window_minutes'] or 0) < (longest['window_minutes'] or 0)]
    if alerts:
        return max(alerts, key=lambda row: (row['used_percent'], -(row['window_minutes'] or 0)))
    return longest


def alert_percent(quota):
    value = quota['number'](quota['setting_map']('display').get('short_window_alert_percent'))
    return value if value is not None and 0 < value <= 100 else SHORT_WINDOW_ALERT


def words(text):
    return [word for word in re.split(r'[\s_.:-]+', str(text)) if word]


def title_case(text):
    """`pro_student` reads `Pro Student`; text that already has capitals is kept as written."""
    text = str(text).strip()
    return text if any(char.isupper() for char in text) else ' '.join(word[:1].upper() + word[1:] for word in words(text))


def sentence_case(text):
    text = str(text).strip() if any(char.isupper() for char in str(text)) else ' '.join(words(text))
    return text[:1].upper() + text[1:]


def provider_name(name):
    return PROVIDER_NAMES.get(name) or title_case(name)


def plan_label(plan):
    return (PLAN_NAMES.get(str(plan).strip().lower()) or title_case(plan)) if plan else None


def length_label(minutes):
    """`Weekly` for 10080 minutes; an unnamed length reads `3-day` or `90-minute`."""
    if not minutes:
        return None
    minutes = round(minutes)
    if minutes in LENGTH_LABELS:
        return LENGTH_LABELS[minutes]
    if MONTH_MINUTES[0] <= minutes <= MONTH_MINUTES[1]:
        return 'Monthly'
    for unit, size in (('day', 1440), ('hour', 60)):
        if minutes % size == 0:
            return f'{minutes // size}-{unit}'
    return f'{minutes}-minute'


def window_label(row, labels):
    """A label a provider or `quota.window_labels` gives wins. Otherwise the length names the window, so Codex's
    `primary` of 10080 minutes reads `Weekly`, and what the name adds qualifies it: `seven_day_opus` reads
    `Weekly (Opus)`. An untimed window keeps its own name in sentence case."""
    if labels.get(row['name']):
        return labels[row['name']]
    length = length_label(row.get('window_minutes'))
    if not length:
        return sentence_case(row['name'])
    base, _, tag = row['name'].partition(':')
    extra = [word for word in words(base) if word.lower() not in LENGTH_WORDS and word != row.get('pool')
             and not re.fullmatch(r'\d+[mhdw]', word.lower())]
    extra += words(tag) if tag and not tag.isdigit() else []
    return length + (f" ({sentence_case(' '.join(extra))})" if extra else '')


def pool_of(quota, provider_name, row):
    hook = getattr(quota['provider'](provider_name), 'window_pool', None) if provider_name in quota['registered']() else None
    value = row.get('pool') or (hook(row['name']) if hook else None)
    return value if isinstance(value, str) and quota['LABEL'].fullmatch(value) else None


def display_name(quota, provider_name, label, entries):
    """`accounts.<provider>.<label>.display_name`, else the label in title case. The CLI's `default` and a
    discovered fingerprint name no one, so they read as the provider alone (None)."""
    entry = (entries.get(provider_name) or {}).get(label)
    named = entry.get('display_name') if isinstance(entry, dict) else None
    if isinstance(named, str) and named.strip():
        return named.strip()[:64]
    shared = getattr(quota['provider'](provider_name), 'fingerprinted', None) if provider_name in quota['registered']() else None
    return None if label == 'default' or (shared and shared(label)) else title_case(label)


def configured_entries(quota):
    try:
        return quota['configured']()
    except ValueError:
        return {}


def repeats(quota, window, windows):
    """A `limits` entry (`weekly_all:2`) that restates a named window (`seven_day`): same pool, length, use and
    reset. Only such an entry is dropped, never a named window, so two distinct limits that happen to match
    both stay."""
    def stamp(row):
        return quota['parse_time'](row['reset_at'])
    return ':' in window['name'] and any(
        ':' not in other['name'] and other.get('pool') == window.get('pool') and
        other['window_minutes'] == window['window_minutes'] and
        other['used_percent'] == window['used_percent'] and stamp(other) and stamp(window) and
        abs((stamp(other) - stamp(window)).total_seconds()) <= 60 for other in windows)


def window_labels(quota):
    configured = quota['setting_map']('window_labels')
    return {**WINDOW_LABELS, **{str(key): value for key, value in configured.items() if isinstance(value, str)}}


def summary(quota, value, worst, spent, drawing, rate):
    if not worst:
        return value.get('error') or 'unreported'
    if spent and not drawing and worst['pace']['state'] != 'exhausted':
        return f"{worst['name']}: allowance spent; {value.get('provider')} refuses ordinary use"
    if not drawing:
        return f"{worst['name']}: {worst['pace']['summary']}"
    return (f"{worst['name']}: allowance spent; drawing on credits, {credit_text(quota, drawing)}"
            + (f', about {rate:,.0f}/h' if rate else ''))


def runway(quota, value, extras, drawing):
    """Separate reserve resources from the allowance estimate; never infer a credit conversion rate."""
    parts = []
    credit = quota['credit_draw'](value, True)
    if credit:
        rate = quota['burn_rate'](value, credit) if drawing else None
        if credit['unlimited']:
            parts.append('then unlimited credits')
        elif rate:
            floor = quota['number'](quota['setting']('credit_floor')) or 0
            seconds = max(0, credit['balance'] - floor) / rate * 3600
            parts.append(f"then about {quota['duration'](seconds)} on credits at the measured credit burn rate")
        else:
            parts.append(f"then {credit_text(quota, credit)}; credit runway unknown")
    if extras.get('resets'):
        parts.append(reset_text(quota, extras['resets']))
    return '; '.join(parts)


def refusal_reset(quota, value, rows):
    """A reset invalidates only the refusal attached to that window."""
    allowance = value.get('allowance') or {}
    if allowance.get('spend_control_reached'):
        return False
    reached = allowance.get('limit_reached_type')
    windows = [row for row in rows if quota['sets_state'](row)]
    if reached:
        windows = [row for row in windows if reached in (row['name'], row.get('raw_kind'))]
    elif value.get('unreported'):
        return False
    return bool(windows) and all(row.get('reset_passed') for row in windows)


def describe(quota, value, banked, now, entries=None, alert=None):
    labels, name = window_labels(quota), value.get('provider')
    rows, stale = [], False
    observed = quota['parse_time'](value.get('observed_at'))
    allowance = value.get('allowance') or {}
    reached = allowance.get('limit_reached_type')
    extras = optional_credits(quota, value, banked)
    named_refusal = reached and any(reached in (row['name'], row.get('raw_kind'))
                                    for row in value.get('windows') or [])
    for row in value.get('windows') or []:
        pool = pool_of(quota, name, row)
        passed = quota['reset_passed'](row, now)
        outdated = bool(observed and passed and observed < quota['parse_time'](row.get('reset_at')))
        stale = stale or outdated and quota['sets_state'](row)
        refused = reached in (row['name'], row.get('raw_kind')) if named_refusal else allowance.get('ordinary_usage_allowed') is False
        # A fresh lapsed window still binds without headroom or while refused.
        inactive = outdated or bool(observed and passed and row['used_percent'] < quota['NEAR_EXHAUSTED'] and not refused)
        row = {**row, 'pace': quota['pace'](row, now), **({'pool': pool, 'pool_label': f'{title_case(pool)} models'} if pool else {}),
               **({'reset_passed': True} if inactive else {})}
        rows.append({**row, 'label': window_label(row, labels)})
    # A restated window would show twice and count twice; the report keeps the named one.
    rows = [row for row in rows if not repeats(quota, row, rows)]
    # A refusal belongs to its reading's windows; after a reset it needs fresh evidence.
    allowance = {**value, 'allowance': {}} if refusal_reset(quota, value, rows) else value
    worst = quota['binding']([row for row in rows if not row.get('reset_passed')])
    if worst is None and (allowance.get('allowance') or {}).get('ordinary_usage_allowed') is False:
        worst = quota['binding'](rows)
    spent = quota['allowance_spent'](allowance, worst)
    drawing = quota['credit_draw'](value, spent)
    rate = quota['burn_rate'](value, drawing) if drawing else None
    reserves = runway(quota, value, extras, drawing)
    for row in rows:
        if row['pace']['state'] == 'ahead_of_pace' and reserves:
            row['pace']['summary'] += '; ' + reserves
    unreported = list(value.get('unreported') or [])
    dated = [row for row in rows if row.get('reset_at') and row.get('window_minutes')]
    longest = max(dated, key=lambda row: row['window_minutes'], default=None)
    signed_out = value.get('status') == 'signed_out'
    # Reported windows that all reset since are unknown, not unreported.
    lapsed = any(row.get('reset_passed') for row in rows)
    state = 'signed_out' if signed_out else 'credit_only' if drawing else 'exhausted' if spent else \
        worst['pace']['state'] if worst else 'unknown' if stale or lapsed else 'unreported'
    shown = display(quota, [row for row in rows if row.get('pool') in (None, (worst or {}).get('pool'))], now,
                    alert_percent(quota) if alert is None else alert)
    # A signed-out account's return is history too: "back since" once its reset has passed.
    history = [{**row, 'pace': {**row['pace'], 'state': 'exhausted'}}
               if row['used_percent'] >= 100 else row for row in rows] if signed_out else []
    back = quota['binding'](history) if signed_out else worst if spent else None
    back = back if back and back['used_percent'] >= 100 else None
    detail = summary(quota, value, worst, spent, drawing, rate)
    if stale:
        detail = 'stale; window reset since reading; ' + (detail if worst else 'awaiting a fresh reading')
    elif lapsed and not worst and not spent and not drawing:
        detail = 'reset passed; awaiting a fresh reading'
    return {key: value.get(key) for key in ('provider', 'account', 'plan', 'host', 'active', 'priority',
                                            'observed_at', 'attempted_at', 'status', 'error', 'source')} | {
        'provider_name': provider_name(name),
        'display_name': display_name(quota, name, value.get('account'), configured_entries(quota) if entries is None else entries),
        'plan_label': plan_label(value.get('plan')), 'display_window': shown['name'] if shown else None,
        'back_at': back.get('reset_at') if back else None,
        'state': state,
        'status': 'stale' if stale and not signed_out else value.get('status'),
        'summary': f"signed out; last read {value.get('observed_at') or 'never'}" if signed_out else
                   detail,
        'windows': rows, 'unreported': unreported,
        'coverage_note': f"{', '.join(unreported)} not reported by {value.get('provider')}; missing does not mean 0%"
                         if unreported else '',
        'credits': value.get('credits') or banked, 'credit_advice': quota['credit_advice'](rows, value.get('credits') or banked, now),
        'use_it_or_lose_it': {'window': longest['name'], 'reset_at': longest['reset_at'],
                              'unused_percent': longest['pace']['headroom_percent'],
                              'expiring_headroom_percent': longest['pace']['expiring_headroom_percent']}
                             if longest else None} | extras | (
        {'drawing_credits': True, 'credit_burn_per_hour': rate} if drawing else {}) | (
        {'schedule': value['schedule']} if isinstance(value.get('schedule'), dict) else {})


def optional_credits(quota, value, banked):
    """Usage credits and banked resets, present only when an account has some."""
    counted = value.get('credits') or banked or {}
    resets = (quota['normalise_resets'](value.get('resets')) or
              quota['normalise_resets']({'available': counted.get('reset_credits'), 'source': counted.get('source', 'manual')}))
    extras = {'usage_credits': quota['normalise_usage_credits'](value.get('usage_credits')), 'resets': resets}
    return {key: extra for key, extra in extras.items() if extra}


def usable(row):
    return row['status'] in ('ok', 'stale') and row['display_window'] and row['state'] not in (
        'exhausted', 'credit_only', 'signed_out', 'unreported')


def headline(quota, rows, activity):
    """The account the menu bar shows (`quota.menubar.headline`): `recent`, the usable account used most recently
    here, falling back to the highest priority; `worst`, the usable account with the highest binding use; a
    pinned `<provider>/<account>`; or `none`, the icon alone. None when nothing usable remains."""
    mode = quota['setting_map']('menubar').get('headline')
    mode = mode if mode in HEADLINES or isinstance(mode, str) and '/' in mode else 'recent'
    rows = [row for row in rows if mode != 'none']
    pinned = next((row for row in rows if f"{row['provider']}/{row['account']}" == mode), None)
    candidates = [row for row in rows if usable(row)]
    if pinned:
        return {'mode': mode, 'account': mode, 'window': pinned['display_window']}
    if mode == 'worst':
        peaks = [(row, quota['binding']([item for item in row['windows'] if not item.get('reset_passed')]))
                 for row in candidates]
        row, peak = max(peaks, key=lambda item: item[1]['used_percent'], default=(None, None))
        return {'mode': mode, 'account': row and f"{row['provider']}/{row['account']}", 'window': peak and peak['name']}
    used = [row for row in candidates if activity.get(f"{row['provider']}/{row['account']}")]
    row = (max(used, key=lambda row: activity[f"{row['provider']}/{row['account']}"]) if used else
           min(candidates, key=lambda row: row['priority'] if row['priority'] is not None else 100, default=None))
    return {'mode': mode, 'account': row and f"{row['provider']}/{row['account']}", 'window': row and row['display_window']}


def report(quota, values, now=None, refreshing=(), activity=None):
    now = now or quota['utcnow']()
    banked = quota['credits']()
    entries, alert = configured_entries(quota), alert_percent(quota)
    rows = [describe(quota, value, banked.get((value.get('provider'), value.get('account'), quota['telemetry_host']())), now,
                     entries, alert)
            for value in quota['freshest'](values)]
    preference = {}
    for row in rows:
        if row['state'] != 'signed_out':  # routing never places work on an account that is not signed in
            preference.setdefault(row['provider'], []).append(row)

    def order(row):
        # Spend the account whose longest window resets soonest first, so its headroom is not lost.
        longest = max(row['windows'], key=lambda window: window['window_minutes'] or 0, default=None)
        spare = row['state'] == 'credit_only' and quota['credits_policy']() == 'last_resort'
        return (row['state'] == 'exhausted', spare, row['priority'] if row['priority'] is not None else 100,
                (longest or {}).get('reset_at') or '9999', row['account'])
    due = [row['schedule']['next_due_at'] for row in rows if row.get('schedule', {}).get('next_due_at')]
    return {'schema': quota['REPORT_SCHEMA'], 'generated_at': quota['iso'](now), 'host': quota['telemetry_host'](), 'enabled': quota['enabled'](),
            'next_due_at': min(due, key=quota['parse_time']) if due else None, 'refreshing': sorted(refreshing),
            'providers': quota['registered'](), 'accounts': rows,
            'display': {'short_window_alert_percent': alert},
            'headline': headline(quota, rows, quota['local_activity']() if activity is None else activity),
            'placement_preference': {name: [row['account'] for row in sorted(group, key=order)]
                                     for name, group in sorted(preference.items())}}


def day(quota, stamp):
    moment = quota['parse_time'](stamp).astimezone()
    return f'{moment.day} {moment:%b}'


def amount(value, currency):
    return f'${value:,.0f}' if currency == 'USD' else f'{value:,.0f}' + (f' {currency}' if currency else '')


def used_text(window):
    """`$136 of $250` for a budget window, otherwise the reported percentage."""
    if window.get('limit_amount'):
        currency = window.get('currency')
        return f"{amount(window['used_amount'], currency)} of {amount(window['limit_amount'], currency)}"
    return f"{window['used_percent']:g}%"


def credit_text(quota, entry):
    text = 'unlimited credits' if entry['unlimited'] else \
        f"{amount(entry['balance'], entry['currency'])} credit" if entry['currency'] else \
        f"{entry['balance']:,.0f} {entry['unit'] or 'credits'}"
    return text + ' available' + (f" (expires {day(quota, entry['expires_at'])})" if entry['expires_at'] else '')


def reset_text(quota, resets):
    count, expiries = resets['available'], resets['expires_at']
    lead = 'next expires' if len(expiries) > 1 else 'expires'
    return f"{count} banked reset{'s' if count != 1 else ''}" + (f' ({lead} {day(quota, expiries[0])})' if expiries else '')


def table(headers, rows):
    cells = [[str(cell) for cell in row] for row in rows]
    widths = [max([len(str(header)), *(len(row[i]) for row in cells)]) for i, header in enumerate(headers)]
    return [' '.join(str(cell).ljust(width) for cell, width in zip(row, widths)).rstrip()
            for row in [headers, *cells]]


def render(quota, value, verbose=False):
    lines = [f"Provider quota on {value['host']}" + ('' if value['enabled'] else ' (fetching disabled)')]
    rows = []
    for row in value['accounts']:
        head = (row['provider'], row['account'] + ('*' if row['active'] else ''), row.get('plan_label') or row['plan'] or '-')
        for window in row['windows']:
            pace_state, detail, used = window['pace']['state'], window['pace']['summary'], used_text(window)
            if window.get('limit_amount') and not window['window_minutes']:  # a dollar budget has no pace
                pace_state = '-'
                detail = f"{amount(window['limit_amount'] - window['used_amount'], window.get('currency'))} left; " + \
                    detail.removesuffix('; window length unreported')
            if window.get('reset_passed'):  # a passed reset makes the reading history, never current use
                pace_state, used, detail = '-', '-', f"reset since {window['reset_at'][:16].replace('T', ' ')}"
            if row['state'] == 'signed_out':
                pace_state = 'signed out'
            label = window.get('label') or window['name']
            rows.append((*head, f"{window['pool_label']} · {label}" if window.get('pool_label') else label, used,
                         window_length(quota, window['window_minutes']),
                         (window['reset_at'] or '-')[:16].replace('T', ' '), pace_state, detail))
        if not row['windows']:
            rows.append((*head, '-', '-', '-', '-', row['status'].replace('_', ' '), row['error'] or ''))
    lines += table(('provider', 'account', 'plan', 'window', 'used', 'length', 'resets', 'pace', 'detail'), rows)
    # A plan lacks many optional windows, so absence alone is not news: list them only on request.
    for row in value['accounts'] if verbose else []:
        if row['unreported']:
            lines.append(f"{row['provider']}/{row['account']}: {len(row['unreported'])} window(s) not reported "
                         "(missing does not mean 0%)")
    # Credits and banked resets appear only for an account that has some.
    for row in value['accounts']:
        parts = [credit_text(quota, entry) for entry in row.get('usage_credits') or []]
        parts += [reset_text(quota, row['resets'])] if row.get('resets') else []
        advice = row['credit_advice']
        parts += [f"{advice['action']}: {advice['reason']}"] if advice['action'] != 'none' else []
        if parts:
            lines.append(f"{row['provider']}/{row['account']}: " + '; '.join(parts))
    lines.append('* active account (the CLI default identity); routing reads only cached readings')
    print('\n'.join(lines))
