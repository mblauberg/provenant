"""OpenCode Go spend per model from its local message store, against the documented caps.

OpenCode Go limits each model separately: 5 hour 20%, weekly 50% and monthly 100% of that
model's monthly dollar cap (opencode.ai/docs/go). It documents no reset semantics and offers
no usage endpoint, so every window here is a rolling local estimate: spend on this machine
only. Models with no known cap (unlimited promotions included) are reported as unreported.
Free zen models cost nothing and are ignored.
"""

from __future__ import annotations

from contextlib import closing
from datetime import timedelta
import sqlite3


ADAPTER = 'opencode'
PAID = ('opencode-go',)
DEFAULT_PLAN = 'go'      # the cheapest documented plan ($10/month), used when none is configured
# Window label, minutes and share of the monthly cap.
WINDOWS = (('5h', 5 * 60, 0.2), ('7d', 7 * 24 * 60, 0.5), ('30d', 30 * 24 * 60, 1.0))
# Documented monthly dollar caps per model id: (Go, Go Plus). Peak/off-peak and long-context
# tiers share one cap. `limits.monthly_by_model` in the account config overrides or adds models.
CAPS = {
    'glm-5.3-flash': (60, 180), 'glm-5.3': (15, 120), 'glm-5.2': (60, 180),
    'kimi-k3': (15, 60), 'kimi-k2.7-code': (60, 180), 'longcat-2.0': (60, 240),
    'step-5-preview': (15, 60), 'mimo-v2.6-flash': (60, 120), 'mimo-v2.6-pro': (15, 60),
    'mimo-v2.5': (60, 120), 'mimo-v2.5-pro': (15, 60), 'minimax-m3': (60, 180),
    'muse-spark-1.3-contributor': (60, 120), 'muse-spark-1.2-contributor': (60, 120),
    'qwen3.8-max': (15, 60), 'qwen3.8-flash': (30, 90), 'qwen3.7-plus': (60, 180),
    'deepseek-v4.1-flash': (80, 160), 'deepseek-v4-pro': (15, 60), 'hy4-preview': (30, 120),
    'hy3': (60, 240), 'grok-4.7': (15, 60), 'gpt-6-luna': (15, 60), 'gpt-5.6-luna': (15, 60),
    'claude-haiku-5-5': (15, 60), 'gemini-4-argon-contributor': (60, 240),
}
PLANS = {'go': 0, 'go plus': 1}


def default_identity():
    from provider_identity import selected_identity
    return selected_identity('opencode')


def model_spend(database, now, providers):
    """Dollars per model for each window since now, over paid providers; None if unreadable."""
    marks = ','.join('?' * len(providers))
    sums = ','.join("SUM(CASE WHEN time_created>=? THEN COALESCE(json_extract(data,'$.cost'),0) ELSE 0 END)"
                    for _ in WINDOWS)
    since = [int((now - timedelta(minutes=minutes)).timestamp() * 1000) for _, minutes, _ in WINDOWS]
    try:
        with closing(sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True)) as db:
            rows = db.execute(f"SELECT json_extract(data,'$.modelID'), {sums} FROM message "
                              f"WHERE time_created BETWEEN ? AND ? AND json_extract(data,'$.providerID') IN ({marks}) "
                              "GROUP BY 1", (*since, min(since), int(now.timestamp() * 1000), *providers)).fetchall()
        return {row[0]: [float(value or 0) for value in row[1:]] for row in rows if row[0]}
    except (sqlite3.Error, OSError, TypeError, ValueError):
        return None


def cap_for(model, plan, overrides):
    try:
        cap = float(overrides[model])
    except (KeyError, TypeError, ValueError):
        caps = CAPS.get(model)
        cap = caps[plan] if caps else 0
    return cap if cap > 0 else None


def fetch(account, io, now):
    plan = PLANS.get(str(account.plan or DEFAULT_PLAN).strip().lower())
    limits = account.options.get('limits')
    overrides = limits.get('monthly_by_model') if isinstance(limits, dict) else None
    overrides = overrides if isinstance(overrides, dict) else {}
    providers = tuple(account.options.get('providers') or PAID)
    spend = model_spend(account.identity_dir / 'opencode.db', now, providers)
    windows, unreported = [], []
    if spend is None:
        unreported.append('usage')
    for model, spent in sorted((spend or {}).items()):
        cap = cap_for(model, plan, overrides) if plan is not None or model in overrides else None
        if cap is None:
            unreported.append(model)
            continue
        for (label, minutes, share), dollars in zip(WINDOWS, spent):
            windows.append({'name': f'{model}.{label}', 'raw_kind': 'local_spend_usd',
                            'used_percent': dollars / (cap * share) * 100,
                            'window_minutes': minutes, 'reset_at': None})
    assumed = ', assuming Go plan' if account.plan is None else ''
    return {'windows': windows, 'unreported': unreported, 'plan': None,
            'source': f'local estimate: opencode.db spend per model against documented caps{assumed} '
                      '(this machine only)'}
