"""Isolated, fixture-only differential oracle. Never probes providers or live stores."""
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[6]
sys.path.insert(0, str(ROOT / 'scripts'))
import provider_quota as quota
import quota_display as display
import quota_schedule as schedule

request = json.load(sys.stdin)
context = request.get('context', {})
quota.setting = lambda key, default=None: (context.get('settings') or {}).get(key, default)
quota.setting_map = lambda key: quota.setting(key) if isinstance(quota.setting(key), dict) else {}
quota.configured = lambda: context.get('entries') or {}
quota.telemetry_host = lambda: context.get('host', 'fixture-host')
quota.enabled = lambda: context.get('enabled', True)
quota.registered = lambda: sorted(context.get('providers', ['agy', 'claude', 'codex', 'cursor', 'opencode']))
quota.credits = lambda: {(p,a,h): value for p,a,h,value in context.get('banked', [])}
quota.local_activity = lambda: context.get('activity', {})
now = schedule.parse_time(request.get('now'))
args = request.get('args', [])
operation = request['operation']
if operation == 'schema':
    try:
        import jsonschema
    except ImportError:
        result = {'available': False}
    else:
        schema = json.loads((ROOT / 'schemas/quota-snapshot.schema.json').read_text())
        judge = jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker())
        result = {'available': True, 'valid': judge.is_valid(request['value']),
                  'mutant_rejected': not judge.is_valid(request['mutant'])}
elif operation == 'headline_outcome':
    try:
        result = display.headline(vars(quota), args[0], context.get('activity', {}))
    except TypeError:
        result = {'error': 'TypeError'}
elif operation == 'window_pool':
    hook = getattr(quota.provider(args[0]), 'window_pool', None)
    result = hook(args[1]) if hook else None
elif operation == 'pace':
    result = quota.pace(args[0], now)
elif operation == 'binding':
    rows = args[0]
    bound = quota.binding(rows)
    result = next((i for i,r in enumerate(rows) if r is bound), None)
elif operation == 'reset_passed':
    result = quota.reset_passed(args[0], now)
elif operation == 'credit_advice':
    result = quota.credit_advice(*args, now)
elif operation == 'freshest':
    result = quota.freshest(args[0])
elif operation == 'report':
    result = display.report(vars(quota), json.loads(request['values_json']) if 'values_json' in request else request['values'], now,
                            refreshing=context.get('refreshing', []), activity=context.get('activity', {}))
elif operation == 'schedule':
    config, notes = schedule.resolve(request['provider'], request.get('account_quota'), context.get('settings'))
    block, due = schedule.plan(provider=request['provider'], account_id=request['account_id'],
        snapshot=request.get('snapshot'), now=now, config=config, trigger=request.get('trigger', 'timer'),
        power=request.get('power', 'ac'), activity_at=request.get('activity_at'),
        ignore_backoff=request.get('ignore_backoff', False), notes=notes)
    result = [config, notes, block, due]
elif operation == 'time':
    result = [schedule.iso(schedule.parse_time(v)) for v in args]
elif operation == 'backoff':
    config, _ = schedule.resolve(request['provider'], None, context.get('settings'))
    result = repr(schedule.backoff_seconds(*args, config, request.get('seed', '')))
else:
    result = getattr(quota, operation)(*args)
json.dump(result, sys.stdout)
