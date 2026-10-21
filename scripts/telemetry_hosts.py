"""Read-only peer telemetry summaries over the existing bounded SSH transport."""

from concurrent.futures import ThreadPoolExecutor
import importlib.util
import math
from pathlib import Path
import sys


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
    for field in ('outcomes', 'lanes', 'attempts', 'limits_per_day', 'quota_observations', 'unattributed'):
        result[field] = [{**row, 'host': host} for host, report in reports.items() for row in report[field]]
    result['excluded_rows'] = {}
    for report in reports.values():
        for reason, count in report['excluded_rows'].items():
            result['excluded_rows'][reason] = result['excluded_rows'].get(reason, 0) + count
    result['receipt_usage_rows_excluded'] = sum(report['receipt_usage_rows_excluded'] for report in reports.values())
    quota = telemetry.quota_owner()
    result['quota'] = quota.report([value for report in reports.values() for value in report.get('quota_snapshots', [])])
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
                or any(not numeric(group.get(field)) for field in telemetry.SUMMED)
                or not isinstance(group.get('missing_token_fields'), dict)
                or any(not numeric(group['missing_token_fields'].get(field)) for field in telemetry.TOKEN_FIELDS)
                or not numeric(group.get('reported_cost_usd'), nullable=True)
                or any(not isinstance(group.get(field), list) or not all(isinstance(item, str) for item in group[field])
                       for field in ('model_raw', 'billing'))
                or any(group.get(field) is not None and not isinstance(group[field], str)
                       for field in ('source', 'session', *USAGE_KEYS, *DAY_KEYS))):
            raise ValueError('invalid aggregate')
    for field in ('outcomes', 'lanes', 'attempts', 'limits_per_day', 'quota_observations', 'unattributed'):
        if not isinstance(value.get(field), list) or not all(isinstance(row, dict) for row in value[field]):
            raise ValueError('invalid summary rows')
    if (not isinstance(value.get('excluded_rows'), dict)
            or not all(numeric(count) for count in value['excluded_rows'].values())
            or not numeric(value.get('receipt_usage_rows_excluded'))
            or not isinstance(value.get('pending_source_rebuilds'), list)):
        raise ValueError('invalid summary counts')
    for row in value['outcomes']:
        for field in ('lanes', 'succeeded', 'attempts', 'retries', 'success_rate', 'latency_p50_s',
                      'latency_p90_s', 'output_tokens_per_s'):
            if field not in row or not numeric(row[field], nullable=True):
                raise ValueError('invalid outcome')
        if not isinstance(row.get('error_classes'), dict):
            raise ValueError('invalid outcome errors')

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
            # Validate the summable numeric contract before accepting a peer.
            merge(telemetry, [value], [], since)
            return value, {'host': host, 'status': 'ok'}
        except Exception:  # A malformed or old peer must never prevent local stats.
            return None, {'host': host, 'status': 'partial', 'reason': 'bad_response'}

    with ThreadPoolExecutor(max_workers=max(1, min(8, len(selected)))) as pool:
        rows = list(pool.map(read, selected))
    return merge(telemetry, [value for value, _ in rows if value], [status for _, status in rows], since)
