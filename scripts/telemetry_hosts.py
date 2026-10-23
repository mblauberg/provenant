"""Read-only peer telemetry summaries over the existing bounded SSH transport."""

from concurrent.futures import ThreadPoolExecutor
import importlib.util
import math
from pathlib import Path
import sys

quota_display = sys.modules.get('quota_display')
if quota_display is None:
    _display_spec = importlib.util.spec_from_file_location('quota_display', Path(__file__).with_name('quota_display.py'))
    quota_display = importlib.util.module_from_spec(_display_spec)
    sys.modules['quota_display'] = quota_display
    _display_spec.loader.exec_module(quota_display)

USAGE_KEYS = ('provider', 'adapter', 'model')
DAY_KEYS = ('day', 'provider', 'adapter', 'account')


def hosts_owner():
    name = 'provenant_telemetry_fabric_hosts'
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name('fabric_hosts.py'))
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]


def merge(telemetry, summaries, statuses, since):
    # A session is transported as aggregate groups, never as raw usage records.
    sessions = {}
    reports = {}
    for summary in summaries:
        host = summary['host']
        if host in reports:
            continue
        reports[host] = summary
        for group in summary['session_usage']:
            key = (host, group['source'], group['session'])
            sessions.setdefault(key, []).append(group)
    groups = [group for session in sessions.values() for group in session]
    result = {'schema_version': 2, 'since': since, 'host': ', '.join(reports),
              'hosts': statuses, 'partial': any(row['status'] != 'ok' for row in statuses),
              'usage': telemetry.finalise(telemetry.combine(groups, USAGE_KEYS), USAGE_KEYS),
              'usage_per_day': telemetry.finalise(telemetry.combine(groups, DAY_KEYS), DAY_KEYS)}
    # Keep outcome percentiles per host: averaging quantiles would misstate latency.
    for field in ('outcomes', 'lanes', 'attempts', 'limits_per_day', 'quota_observations', 'unattributed',
                  'last_limit_events', 'compactions_per_lane'):
        result[field] = [{**row, 'host': host} for host, report in reports.items() for row in report.get(field, [])]
    result['excluded_rows'] = {}
    for report in reports.values():
        for reason, count in report['excluded_rows'].items():
            result['excluded_rows'][reason] = result['excluded_rows'].get(reason, 0) + count
    result['receipt_usage_rows_excluded'] = sum(report['receipt_usage_rows_excluded'] for report in reports.values())
    quota = telemetry.quota_owner()
    result['quota'] = quota_display.report(vars(quota), [value for report in reports.values() for value in report.get('quota_snapshots', [])])
    result['pending_source_rebuilds'] = [f'{host}/{source}' for host, report in reports.items()
                                        for source in report['pending_source_rebuilds']]
    return result



def validate(telemetry, value, host):
    def numeric(item, nullable=False):
        return item is None and nullable or type(item) in (int, float) and math.isfinite(item) and item >= 0
    if (value.get('schema') != 'provenant.telemetry.summary.v1' or value.get('host') != host
            or not isinstance(value.get('session_usage'), list)):
        raise ValueError('invalid summary')
    for group in value['session_usage']:
        if (not isinstance(group, dict) or group.get('host') != host
                or any(not numeric(group.get(field)) or field != 'known_cost_usd' and group[field] != int(group[field])
                       for field in telemetry.SUMMED)
                or not isinstance(group.get('missing_token_fields'), dict)
                or any(not numeric(group['missing_token_fields'].get(field))
                       or group['missing_token_fields'][field] != int(group['missing_token_fields'][field])
                       for field in telemetry.TOKEN_FIELDS)
                or not numeric(group.get('reported_cost_usd'), nullable=True)
                or any(not isinstance(group.get(field), list) or not all(isinstance(item, str) for item in group[field])
                       for field in ('model_raw', 'billing'))
                or any(group.get(field) is not None and not isinstance(group[field], str)
                       for field in ('source', 'session', *USAGE_KEYS, *DAY_KEYS))):
            raise ValueError('invalid aggregate')
    required = {
        'outcomes': ('adapter', 'model', 'effort', 'task_class', 'route', 'error_classes', 'lanes', 'succeeded',
                     'attempts', 'retries', 'success_rate', 'latency_p50_s', 'latency_p90_s', 'output_tokens_per_s'),
        'lanes': ('status', 'error_class', 'lanes'),
        'compactions_per_lane': ('lane', 'compactions'),
        'attempts': ('status', 'error_class', 'attempts'),
        'limits_per_day': ('day', 'provider', 'account', 'status', 'events'),
        'quota_observations': ('provider', 'window', 'used_percent', 'reset_at', 'timestamp'),
        'unattributed': ('source', 'adapter', 'rows', 'input_tokens', 'statuses'),
        'last_limit_events': ('provider', 'account', 'timestamp', 'age_seconds'),
    }
    # Optional counts (absent from older peers) are checked only when present.
    counts = {'lanes', 'succeeded', 'attempts', 'retries', 'events', 'rows', 'input_tokens', 'age_seconds',
              'compactions',
              'latency_samples'}
    measurements = {'success_rate', 'latency_p50_s', 'latency_p90_s', 'output_tokens_per_s', 'used_percent', 'window_minutes'}
    mappings = {'statuses', 'error_classes'}
    for field, keys in required.items():
        rows = value.get(field, [])
        if not isinstance(rows, list):
            raise ValueError('invalid summary rows')
        for row in rows:
            if not isinstance(row, dict) or not set(keys) <= row.keys():
                raise ValueError('invalid summary row fields')
            for key, item in row.items():
                if key in counts:
                    valid = numeric(item) and item == int(item)
                elif key in measurements:
                    valid = numeric(item, nullable=key != 'used_percent')
                elif key in mappings:
                    valid = isinstance(item, dict) and all(isinstance(k, str) and numeric(v) and v == int(v)
                                                          for k, v in item.items())
                else:
                    valid = item is None or isinstance(item, str)
                if not valid:
                    raise ValueError('invalid summary row value')
    if (not isinstance(value.get('excluded_rows'), dict)
            or not all(numeric(count) for count in value['excluded_rows'].values())
            or not numeric(value.get('receipt_usage_rows_excluded'))
            or not isinstance(value.get('pending_source_rebuilds'), list)):
        raise ValueError('invalid summary counts')
    for row in value.get('outcomes', []):
        for field in ('lanes', 'succeeded', 'attempts', 'retries', 'success_rate', 'latency_p50_s',
                      'latency_p90_s', 'output_tokens_per_s'):
            if field not in row or not numeric(row[field], nullable=True):
                raise ValueError('invalid outcome')
        if not isinstance(row.get('error_classes'), dict):
            raise ValueError('invalid outcome errors')


def validate_snapshots(telemetry, values, host):
    if not isinstance(values, list):
        raise ValueError('invalid quota snapshots')
    quota = telemetry.quota_owner()
    def stamp(value):
        return value is None or isinstance(value, str) and quota.parse_time(value) is not None
    def finite(value):
        return type(value) in (int, float) and math.isfinite(value)
    for value in values:
        if (not isinstance(value, dict) or value.get('schema') != quota.SCHEMA or value.get('host') != host
                or any(not isinstance(value.get(key), str) or not quota.LABEL.fullmatch(value[key])
                       for key in ('provider', 'account'))
                or not stamp(value.get('observed_at')) or not stamp(value.get('attempted_at'))
                or value.get('priority') is not None and type(value['priority']) is not int
                or value.get('active') is not None and type(value['active']) is not bool
                or any(value.get(key) is not None and not isinstance(value[key], str)
                       for key in ('plan', 'status', 'error', 'source'))
                or not isinstance(value.get('unreported', []), list)
                or not all(isinstance(name, str) for name in value.get('unreported', []))
                or not isinstance(value.get('windows'), list)):
            raise ValueError('invalid quota snapshot')
        for window in value['windows']:
            if (not isinstance(window, dict) or not isinstance(window.get('name'), str) or not window['name']
                    or not finite(window.get('used_percent')) or not 0 <= window['used_percent'] <= 100
                    or window.get('window_minutes') is not None and
                       (not finite(window['window_minutes']) or window['window_minutes'] <= 0)
                    or not stamp(window.get('reset_at')) or not stamp(window.get('window_start'))
                    or window.get('raw_kind') is not None and not isinstance(window['raw_kind'], str)):
                raise ValueError('invalid quota window')
        credits = value.get('credits')
        if credits is not None and (not isinstance(credits, dict) or type(credits.get('reset_credits')) is not int
                                    or credits['reset_credits'] < 0
                                    or credits.get('source') not in (None, 'manual', 'provider')):
            raise ValueError('invalid quota credits')

def collect(telemetry, db, since, selectors):
    hosts = hosts_owner()
    local = telemetry.peer_summary(db, since)
    try:
        config = hosts.load_config()
        selected = list(dict.fromkeys([telemetry.host_id(), *config.peers] if selectors == ['all'] else selectors))
    except (hosts.HostError, ValueError):
        local['hosts'] = [{'host': telemetry.host_id(), 'status': 'partial', 'reason': 'invalid_config'}]
        local['partial'] = True
        local.pop('session_usage')
        local.pop('schema')
        return local

    def read(host):
        if host == telemetry.host_id():
            return local, {'host': host, 'status': 'ok'}
        if host not in config.peers:
            return None, {'host': host, 'status': 'partial', 'reason': 'unknown_host'}
        try:
            response = hosts.PeerClient(host, config.peers[host]).call(
                'telemetry.stats', {'schema_version': 1, 'since': since})
            if not response.get('ok') or response.get('reads_flagged'):
                return None, {'host': host, 'status': 'partial',
                              'reason': response.get('error', {}).get('code', 'bad_response')}
            value = response['result']
            validate(telemetry, value, host)
            validate_snapshots(telemetry, value.get('quota_snapshots', []), host)
            value = {**value, 'quota_snapshots': [telemetry.public_snapshot(snapshot)
                                                for snapshot in value.get('quota_snapshots', [])]}
            # Validate the summable numeric contract before accepting a peer.
            merge(telemetry, [value], [], since)
            return value, {'host': host, 'status': 'ok'}
        except Exception:  # A malformed or old peer must never prevent local stats.
            return None, {'host': host, 'status': 'partial', 'reason': 'bad_response'}

    with ThreadPoolExecutor(max_workers=max(1, min(8, len(selected)))) as pool:
        rows = list(pool.map(read, selected))
    return merge(telemetry, [value for value, _ in rows if value], [status for _, status in rows], since)
