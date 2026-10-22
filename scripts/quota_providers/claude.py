"""Claude subscription quota from the account's OAuth usage endpoint.

Adapted from token-meter (MIT, Galileo Agent Labs); see THIRD_PARTY_NOTICES.md.
"""

from __future__ import annotations

from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path


ADAPTER = 'claude'
USAGE_URL = 'https://api.anthropic.com/api/oauth/usage'
KEYCHAIN_SERVICE = 'Claude Code-credentials'
MATCH_SECONDS = 5


def keychain_service(identity_dir):
    """Claude Code suffixes the item with a path hash for any non-default config directory.

    It hashes the CLAUDE_CONFIG_DIR value, so this assumes the configured path is absolute,
    expanded and has no trailing slash. A mismatch only makes the reading unavailable.
    """
    if identity_dir == Path.home() / '.claude':
        return KEYCHAIN_SERVICE
    return f"{KEYCHAIN_SERVICE}-{hashlib.sha256(str(identity_dir).encode()).hexdigest()[:8]}"


def default_identity():
    from provider_identity import selected_identity
    return selected_identity('claude')


def status(account, io):
    """`claude auth status --json` for this identity; only login state is kept."""
    executable = io.executable('claude')
    if not executable:
        return {}
    try:
        result = io.run_process([executable, 'auth', 'status', '--json'], capture_output=True, text=True,
                                timeout=io.PROCESS_TIMEOUT, check=False,
                                env={**os.environ, 'CLAUDE_CONFIG_DIR': str(account.identity_dir)})
        value = json.loads(result.stdout or '{}')
    except (OSError, ValueError, io.TimeoutExpired):
        return {}
    return {key: value.get(key) for key in ('loggedIn', 'authMethod', 'subscriptionType')} if isinstance(value, dict) else {}


def credentials(account, state, io, now):
    """Read the OAuth record in-process. The token never leaves this call chain."""
    raw = None
    try:
        with open(account.identity_dir / '.credentials.json', 'rb') as stream:
            raw = stream.read(io.MAX_RESPONSE_BYTES + 1)
    except OSError:
        pass
    if raw is None and state.get('loggedIn'):
        try:
            result = io.run_process(['/usr/bin/security', 'find-generic-password', '-s',
                                     keychain_service(account.identity_dir), '-w'],
                                    capture_output=True, timeout=io.PROCESS_TIMEOUT, check=False)
        except (OSError, io.TimeoutExpired):
            raise io.Unavailable('Claude credentials are not available') from None
        raw = result.stdout if result.returncode == 0 else None
    if raw is None or len(raw) > io.MAX_RESPONSE_BYTES:
        raise io.Unavailable('Claude credentials are not available')
    try:
        oauth = json.loads(raw).get('claudeAiOauth')
    except (ValueError, AttributeError):
        raise io.Unavailable('Claude credentials are invalid') from None
    if not isinstance(oauth, dict) or not oauth.get('accessToken'):
        raise io.Unavailable('Claude credentials are not available')
    expires = io.parse_time(oauth.get('expiresAt'))
    if expires is not None and (expires - now).total_seconds() <= 60:
        raise io.Unavailable('Claude credentials need a refresh; run claude once')
    if oauth.get('scopes') and 'user:profile' not in oauth['scopes']:
        raise io.Unavailable('Claude credentials lack quota access')
    return oauth


def parse(payload, now=None):
    """Every window the endpoint reports, named as the endpoint names it."""
    windows, unreported = [], []
    for name, value in payload.items():
        if name == 'limits':
            continue
        if value is None and io_window_name(name):
            unreported.append(name)
        if isinstance(value, dict) and 'utilization' in value:
            windows.append({'name': name, 'raw_kind': name, 'used_percent': value.get('utilization'),
                            'window_minutes': io_window_name(name), 'reset_at': value.get('resets_at'),
                            # Dollar budgets, such as cloud session credits, report their amounts.
                            'used_amount': value.get('used_dollars'), 'limit_amount': value.get('limit_dollars'),
                            'currency': 'USD' if value.get('limit_dollars') is not None else None})
    for index, limit in enumerate(payload.get('limits') or []):
        if not isinstance(limit, dict) or limit.get('is_active') is False:
            continue
        scope = limit.get('scope') if isinstance(limit.get('scope'), dict) else {}
        model = scope.get('model') if isinstance(scope.get('model'), dict) else {}
        label = model.get('id') or model.get('display_name') or str(index + 1)
        minutes = (limit.get('window_minutes') or io_window_name(str(limit.get('group') or ''))
                   or duplicated_length(limit, windows))
        windows.append({'name': f"{limit.get('kind') or 'limit'}:{label}", 'raw_kind': limit.get('kind'),
                        'used_percent': limit.get('percent'), 'reset_at': limit.get('resets_at'),
                        # Without a length its pace is always unknown, so it is shown but never sets state.
                        'window_minutes': minutes, 'advisory': minutes is None})
    return {'windows': windows, 'unreported': unreported, 'usage_credits': usage_credits(payload.get('spend'))}


def _instant(value):
    try:
        moment = datetime.fromisoformat(value) if isinstance(value, str) else None
    except ValueError:
        return None
    return moment and (moment if moment.tzinfo else moment.replace(tzinfo=UTC))


def _percent(value):
    return float(value) if type(value) in (int, float) else None


def duplicated_length(limit, named):
    """A `limits` entry repeats a named window: the one with the same reset and, within rounding, the same use."""
    reset, used = _instant(limit.get('resets_at')), _percent(limit.get('percent'))
    for row in named:
        other, seen = _instant(row['reset_at']), _percent(row['used_percent'])
        if (row['window_minutes'] and reset and other and used is not None and seen is not None
                and abs((reset - other).total_seconds()) <= MATCH_SECONDS and abs(used - seen) < 1):
            return row['window_minutes']
    return None


def usage_credits(spend):
    """The prepaid balance that covers use past the plan limits, when the endpoint reports one."""
    if not isinstance(spend, dict):
        return None
    used = spend.get('used') if isinstance(spend.get('used'), dict) else {}
    balance, currency = spend.get('balance'), used.get('currency')
    if isinstance(balance, dict):  # The same minor-unit form as `used`.
        currency = balance.get('currency') or currency
        minor, exponent = balance.get('amount_minor'), balance.get('exponent')
        balance = minor / 10 ** exponent if type(minor) is int and type(exponent) is int else None
    return [{'kind': 'usage_credits', 'balance': balance, 'currency': currency}] if balance is not None else None


WORDS = {'one': 1, 'two': 2, 'three': 3, 'four': 4, 'five': 5, 'six': 6, 'seven': 7,
         'eight': 8, 'nine': 9, 'ten': 10, 'twelve': 12, 'fourteen': 14, 'thirty': 30}
UNITS = {'minute': 1, 'hour': 60, 'day': 1440, 'week': 10080}


def io_window_name(name):
    """Window length in minutes from the endpoint's own name, such as `five_hour`."""
    parts = str(name).lower().split('_')
    if len(parts) < 2:
        return None
    count = WORDS.get(parts[0]) or (int(parts[0]) if parts[0].isdigit() else None)
    unit = UNITS.get(parts[1].rstrip('s'))
    return count * unit if count and unit else None


def fetch(account, io, now):
    state = status(account, io)
    if str(state.get('authMethod') or '').lower() == 'third_party':
        raise io.Unavailable('Claude account quota is not exposed for third-party authentication')
    oauth = credentials(account, state, io, now)
    payload = io.http_json(USAGE_URL, {'Authorization': 'Bearer ' + oauth['accessToken'],
                                       'Accept': 'application/json', 'anthropic-beta': 'oauth-2025-04-20'})
    parsed = parse(payload, now)
    return {**parsed, 'plan': oauth.get('subscriptionType') or oauth.get('rateLimitTier') or state.get('subscriptionType'),
            'source': 'Claude OAuth usage endpoint'}
