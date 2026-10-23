"""Codex account rate limits from `codex app-server`, else the latest rollout events.

Adapted from token-meter (MIT, Galileo Agent Labs); see THIRD_PARTY_NOTICES.md.
"""

from __future__ import annotations

import json
import os
import queue
import re
import threading


ADAPTER = 'codex'
CREDIT_KEYS = ('resetCredits', 'reset_credits', 'availableResets', 'available_resets')


def default_identity():
    from provider_identity import selected_identity
    return selected_identity('codex')


def app_server(account, io, timeout=None):
    """One JSON-RPC exchange; the CLI holds the credential, never this process."""
    timeout = timeout or io.PROCESS_TIMEOUT
    executable = io.executable('codex')
    if not executable:
        raise io.Unavailable('Codex CLI is not installed', 'missing')
    try:
        process = io.open_process([executable, '-s', 'read-only', '-a', 'never', 'app-server'],
                                  stdin=io.PIPE, stdout=io.PIPE, stderr=io.DEVNULL, text=True, bufsize=1,
                                  env={**os.environ, 'CODEX_HOME': str(account.identity_dir)})
    except OSError:
        raise io.Unavailable('Codex could not start its account service') from None
    lines = queue.Queue()
    deadline = io.monotonic() + timeout

    def pump():
        total = 0
        try:
            for line in iter(lambda: process.stdout.readline(io.MAX_RESPONSE_BYTES + 1), ''):
                total += len(line)
                if total > io.MAX_RESPONSE_BYTES:
                    break
                lines.put(line)
        except (OSError, ValueError):
            pass
        finally:
            lines.put(None)
    reader = threading.Thread(target=pump, daemon=True)
    reader.start()

    def send(message):
        process.stdin.write(json.dumps({'jsonrpc': '2.0', **message}) + '\n')
        process.stdin.flush()

    def answer(request_id):
        while True:
            try:
                remaining = deadline - io.monotonic()
                if remaining <= 0:
                    raise queue.Empty
                line = lines.get(timeout=remaining)
            except queue.Empty:
                raise io.Unavailable('Codex quota request timed out') from None
            if line is None:
                raise io.Unavailable('Codex account service closed')
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if isinstance(message, dict) and message.get('id') == request_id:
                if message.get('error'):
                    raise io.Unavailable('Codex could not read account rate limits')
                return message.get('result') or {}

    try:
        send({'id': 1, 'method': 'initialize', 'params': {'clientInfo': {'name': 'provenant', 'version': '1'}}})
        answer(1)
        send({'method': 'initialized'})
        send({'id': 2, 'method': 'account/rateLimits/read'})
        return answer(2)
    except (BrokenPipeError, OSError):
        raise io.Unavailable('Codex account service failed') from None
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=1)
        except io.TimeoutExpired:
            process.kill()
            process.wait(timeout=1)
        finally:
            process.stdin.close()
            process.stdout.close()
            reader.join(timeout=1)


def _window(prefix, name, raw):
    if not isinstance(raw, dict):
        return None
    minutes = raw.get('windowDurationMins', raw.get('window_duration_mins', raw.get('window_minutes')))
    if minutes is None and raw.get('limit_window_seconds') is not None:
        minutes = raw['limit_window_seconds'] / 60
    return {'name': prefix + name, 'raw_kind': name, 'window_minutes': minutes,
            'used_percent': raw.get('usedPercent', raw.get('used_percent')),
            'reset_at': raw.get('resetsAt', raw.get('resets_at', raw.get('reset_at')))}


def parse(payload, now=None):
    root = payload.get('result') if isinstance(payload.get('result'), dict) else payload
    by_id = root.get('rateLimitsByLimitId') if isinstance(root.get('rateLimitsByLimitId'), dict) else {}
    main = root.get('rateLimits') or root.get('rate_limit') or by_id.get('codex') or {}
    windows, unreported = [], []
    groups = [('', main)] + [(f'{re.sub(r"[^a-z0-9]+", "-", str(key).lower()).strip("-")}.', value)
                             for key, value in by_id.items() if key != 'codex' and isinstance(value, dict)]
    for index, value in enumerate(root.get('additional_rate_limits') or []):
        if isinstance(value, dict):
            key = value.get('metered_feature') or f'extra-{index}'
            groups.append((f'{re.sub(r"[^a-z0-9]+", "-", str(key).lower()).strip("-")}.', value))
    for prefix, group in groups:
        group = group.get('rate_limit') if isinstance(group.get('rate_limit'), dict) else group
        for name in ('primary', 'secondary'):
            raw = group.get(name) or group.get(name + '_window')
            row = _window(prefix, name, raw)
            if row:
                windows.append(row)
            elif not prefix and name in group:
                unreported.append(name)
    resets = _resets(root.get('rateLimitResetCredits'))
    count = resets['available'] if resets else next(
        (main.get(key, root.get(key)) for key in CREDIT_KEYS if key in main or key in root), None)
    return {'windows': windows, 'unreported': unreported,
            'plan': main.get('planType') or main.get('plan_type') or root.get('plan_type'),
            'credits': {'reset_credits': count, 'source': 'provider'} if count is not None else None,
            'resets': resets, 'usage_credits': _balance(main.get('credits')),
            # The raw usage-credit balance (a decimal string) for the plan-value series.
            'credit_balance': (main.get('credits') or {}).get('balance') if isinstance(main.get('credits'), dict) else None,
            'allowance': {'ordinary_usage_allowed': root.get('ordinaryUsageAllowed'),
                          'spend_control_reached': main.get('spendControlReached'),
                          'limit_reached_type': main.get('rateLimitReachedType')}}


def _resets(raw):
    """Banked full resets; detail rows may be capped or absent, so only their expiries are kept."""
    if not isinstance(raw, dict) or type(raw.get('availableCount')) is not int:
        return None
    rows = raw.get('credits') if isinstance(raw.get('credits'), list) else []
    return {'available': raw['availableCount'], 'source': 'provider',
            'expires_at': [row.get('expiresAt') for row in rows
                           if isinstance(row, dict) and row.get('status') == 'available' and row.get('expiresAt')]}


def _balance(raw):
    """Credits drawn on once the plan allowance runs out; Codex reports no expiry for them."""
    if not isinstance(raw, dict) or not raw.get('hasCredits', raw.get('has_credits')):
        return None
    return [{'kind': 'credits', 'balance': raw.get('balance'), 'unlimited': raw.get('unlimited') is True,
             'unit': 'credits'}]


def fetch(account, io, now):
    return {**parse(app_server(account, io), now), 'source': 'codex app-server'}
