"""Prices, model attribution and the summable cost groups behind `telemetry stats`.

Functions that need the ledger core take the loaded `telemetry` module, as
telemetry_hosts does, so this module never imports it.
"""

from __future__ import annotations

import json
import re


TOKEN_FIELDS = ('input_tokens', 'cache_read_tokens', 'cache_write_tokens', 'output_tokens', 'reasoning_tokens')
INPUT_FIELDS = TOKEN_FIELDS[:3]
# Test doubles and probes that reach the ledger through real dispatch paths.
FIXTURE_MODEL = re.compile(r'(?:^|/)(?:fixture|[^/]*nonexistent[^/]*)$')
EFFORT_SUFFIX = re.compile(r'-(?:minimal|low|medium|high|xhigh|max)$')
CONTEXT_SUFFIX = re.compile(r'-\d+k$')
DATE_SUFFIX = re.compile(r'-\d{8}$')


def catalogue(telemetry):
    try:
        return json.loads((telemetry.PRODUCT_ROOT / 'config/model-routing.json').read_text()).get('adapters', {})
    except (OSError, ValueError, AttributeError):
        return {}


def normalise_model(adapter, provider, model, adapters):
    """Map an observed spelling onto the routing catalogue id; unknown spellings stay as observed."""
    if not isinstance(model, str) or not model.strip():
        return None
    model = model.strip()
    entries = (adapters.get(adapter) or {}).get('models') or []
    ids = {entry.get('id') for entry in entries}
    if adapter == 'opencode' and provider:
        # Native OpenCode rows name the provider separately; receipts qualify the model.
        namespaces = {str(name).split('/')[0] for name in ids if name} | {provider}
        if model.split('/')[0] not in namespaces:
            model = f'{provider}/{model}'
    if model in ids:
        return model
    # Bare aliases such as `opus` resolve; a versioned spelling is a distinct model.
    aliases = {name.lower(): entry['id'] for entry in entries for name in entry.get('names') or []
               if isinstance(name, str) and name.isalpha()}
    if model.lower() in aliases:
        return aliases[model.lower()]
    candidate = re.sub(r'-+', '-', re.sub(r'\s+', '-', model.lower()))
    for pattern in (None, DATE_SUFFIX, EFFORT_SUFFIX, CONTEXT_SUFFIX):
        if pattern is not None:
            candidate = pattern.sub('', candidate)
        if candidate in ids:
            return candidate
    return model


def classify(row):
    if FIXTURE_MODEL.search(row.get('model_raw') or ''):
        return 'test_fixture'
    if not row.get('adapter') or not row.get('model') or row['model'] == '<synthetic>':
        return 'unattributed'
    return None


def load_prices(telemetry):
    table = json.loads(telemetry.PRICE_TABLE.read_text())
    return table.get('models', {}), table.get('free_patterns', {})


def price_entry(row, prices, free_patterns=None):
    entry = (prices.get(f"{row['adapter']}/{row['model']}") or
             prices.get(f"{row['adapter']}/{row.get('provider')}/{row['model']}"))
    if entry:
        return entry
    if any(re.search(pattern, str(row['model'])) for pattern in (free_patterns or {}).get(row['adapter'], ())):
        return {'billing': 'free', 'price_status': 'free', 'usd_per_million': dict.fromkeys(
            ('input', 'cached_input', 'cache_write', 'output'), 0)}
    return {}


def estimated_cost(telemetry, row, prices, entry=None):
    entry = entry if entry is not None else price_entry(row, prices)
    if entry.get('billing') == 'free':
        return 0.0
    rates = entry.get('usd_per_million') or {}
    total = 0.0
    for token, rate in (('input_tokens', 'input'), ('cache_read_tokens', 'cached_input'),
                        ('cache_write_tokens', 'cache_write'), ('output_tokens', 'output'),
                        ('reasoning_tokens', 'output')):
        count = row[token]
        if count is None and token == 'reasoning_tokens':
            continue  # unreported reasoning is billed inside output
        price = telemetry.number(rates.get(rate))
        if count is None or (count > 0 and price is None):
            return None
        total += count * (price or 0) / 1_000_000
    return total


def unit(telemetry, row, keys, prices, free_patterns):
    """One row as a summable group; `combine` adds groups from any host."""
    entry = price_entry(row, prices, free_patterns)
    cost = estimated_cost(telemetry, row, prices, entry)
    known = all(row[field] is not None for field in INPUT_FIELDS)
    return {**{key: row.get(key) for key in keys}, 'rows': 1,
            **{field: row[field] or 0 for field in TOKEN_FIELDS},
            'missing_token_fields': {field: int(row[field] is None) for field in TOKEN_FIELDS},
            'missing_token_rows': int(any(row[field] is None for field in TOKEN_FIELDS)),
            'cache_share_rows': int(known),
            'cache_share_input': sum(row[field] for field in INPUT_FIELDS) if known else 0,
            'cache_share_read': row['cache_read_tokens'] if known else 0,
            'known_cost_usd': cost or 0.0, 'missing_cost_rows': int(cost is None),
            'reported_cost_usd': row.get('reported_cost'),
            'model_raw': [row['model_raw']] if row.get('model_raw') else [],
            'billing': [entry['billing']] if entry.get('billing') else []}


SUMMED = ('rows', *TOKEN_FIELDS, 'missing_token_rows', 'cache_share_rows', 'cache_share_input',
          'cache_share_read', 'known_cost_usd', 'missing_cost_rows')


def combine(groups, keys):
    merged = {}
    for group in groups:
        key = tuple(group.get(k) for k in keys)
        target = merged.get(key)
        if target is None:
            merged[key] = {**group, 'missing_token_fields': dict(group['missing_token_fields']),
                           'model_raw': set(group['model_raw']), 'billing': set(group['billing']),
                           **{k: group.get(k) for k in keys}}
            continue
        for field in SUMMED:
            target[field] += group[field]
        for field, count in group['missing_token_fields'].items():
            target['missing_token_fields'][field] += count
        if group['reported_cost_usd'] is not None:
            target['reported_cost_usd'] = (target['reported_cost_usd'] or 0) + group['reported_cost_usd']
        target['model_raw'].update(group['model_raw'])
        target['billing'].update(group['billing'])
    for group in merged.values():
        group['model_raw'] = sorted(group['model_raw'])
        group['billing'] = sorted(group['billing'])
    return sorted(merged.values(), key=lambda row: tuple(str(row[k] or '') for k in keys))


def micro(value):
    """Micro-dollars: one figure however the rows were summed. Applied once, at the output edge."""
    return None if value is None else round(value, 6)


def finalise(groups, keys):
    rows = []
    for group in groups:
        row = {**{key: group[key] for key in keys},
               **{field: group[field] for field in SUMMED if field.startswith(('rows', *TOKEN_FIELDS))},
               'missing_token_rows': group['missing_token_rows'],
               'missing_token_fields': group['missing_token_fields'],
               'cache_read_share': (group['cache_share_read'] / group['cache_share_input']
                                    if group['cache_share_input'] else None),
               'cache_share_rows': group['cache_share_rows'],
               'estimated_cost_usd': None if group['missing_cost_rows'] else micro(group['known_cost_usd']),
               'missing_cost_rows': group['missing_cost_rows'],
               'reported_cost_usd': micro(group['reported_cost_usd']),
               'cost_basis': 'api_equivalent_estimate',
               'billing': group['billing'][0] if len(group['billing']) == 1 else 'mixed' if group['billing'] else None}
        if 'model' in keys:
            row['model_raw'] = group['model_raw']
        for field in TOKEN_FIELDS:
            if group['missing_token_fields'][field] == group['rows']:
                row[field] = None
        rows.append(row)
    return rows


def cell_cost(telemetry, cell, entry):
    """`estimated_cost` for rows that share null and positive flags, from their summed counts."""
    if entry.get('billing') == 'free':
        return 0.0
    rates = entry.get('usd_per_million') or {}
    total = 0.0
    for token, rate in (('input_tokens', 'input'), ('cache_read_tokens', 'cached_input'),
                        ('cache_write_tokens', 'cache_write'), ('output_tokens', 'output'),
                        ('reasoning_tokens', 'output')):
        if cell['null_' + token]:
            if token == 'reasoning_tokens':
                continue
            return None
        price = telemetry.number(rates.get(rate))
        if cell['positive_' + token] and price is None:
            return None
        total += cell[token] * (price or 0) / 1_000_000
    return total


def cell_unit(telemetry, cell, keys, prices, free_patterns):
    """`unit` for one SQL group of `cell['rows']` rows; `combine` adds it like the rows it stands for."""
    entry = price_entry(cell, prices, free_patterns)
    cost = cell_cost(telemetry, cell, entry)
    count = cell['rows']
    known = not any(cell['null_' + field] for field in INPUT_FIELDS)
    return {**{key: cell.get(key) for key in keys}, 'rows': count,
            **{field: cell[field] for field in TOKEN_FIELDS},
            'missing_token_fields': {field: count * cell['null_' + field] for field in TOKEN_FIELDS},
            'missing_token_rows': count * any(cell['null_' + field] for field in TOKEN_FIELDS),
            'cache_share_rows': count * known,
            'cache_share_input': sum(cell[field] for field in INPUT_FIELDS) if known else 0,
            'cache_share_read': cell['cache_read_tokens'] if known else 0,
            'known_cost_usd': cost or 0.0, 'missing_cost_rows': count * (cost is None),
            'reported_cost_usd': cell['reported_cost'],
            'model_raw': [cell['model_raw']] if cell.get('model_raw') else [],
            'billing': [entry['billing']] if entry.get('billing') else []}


def usage_cells(db, where, parameters, summary, boundary, end, need_native, order, current, aliases):
    """Non-receipt usage rows grouped in SQL, so a large ledger is never held as Python rows.

    A day key stands for every row of a UTC day lying wholly inside the bounds; any other
    timestamp stays whole for `included`. Rows of one group agree on every field a report reads."""
    simple = ("timestamp GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*' "
              "AND (timestamp LIKE '%Z' OR timestamp LIKE '%+00:00')")
    parameters = dict(parameters)
    parameters['host_identity_current'] = current
    alias_parameters = []
    for index, alias in enumerate(sorted(aliases)):
        name = f'host_identity_alias_{index}'
        parameters[name] = alias
        alias_parameters.append(':' + name)
    host_condition = f"host IN ({', '.join(alias_parameters)})"
    host_expression = f"CASE WHEN {host_condition} THEN :host_identity_current ELSE host END"
    for name, moment, operator in (('low', boundary, '>'), ('high', end, '<')):
        if moment:
            simple += f' AND substr(timestamp,1,10) {operator} :{name}'
            parameters[name] = moment.date().isoformat()
    stamp = f'CASE WHEN {simple} THEN substr(timestamp,1,10) ELSE timestamp END'
    where = " WHERE source!='fabric'" + where
    columns = ['source', 'adapter', 'provider', 'model', 'account', 'status']
    group_columns = list(columns)
    if summary:
        columns += ['local_host AS host', 'session']
        group_columns += ['local_host', 'session']
    flags = [f'{field} IS NULL AS null_{field}, COALESCE({field}>0,0) AS positive_{field}' for field in TOKEN_FIELDS]
    sums = [f'SUM(COALESCE({field},0)) AS {field}' for field in TOKEN_FIELDS]
    ranked = ("WITH ranked_usage AS (SELECT usage.*, rowid AS ledger_rowid, "
              f"{host_expression} AS local_host, ROW_NUMBER() OVER ("
              f"PARTITION BY source, event_id, {host_expression} "
              "ORDER BY CASE WHEN host=:host_identity_current THEN 0 ELSE 1 END, rowid) AS alias_rank "
              f"FROM usage{where}) ")
    cells = [dict(row) for row in db.execute(
        f"{ranked}SELECT {', '.join(columns)}, {stamp} AS timestamp, {', '.join(flags)}, {', '.join(sums)}, "
        f"SUM(reported_cost) AS reported_cost, COUNT(*) AS rows, MIN({order}) AS first_row "
        f"FROM ranked_usage WHERE alias_rank=1 "
        f"GROUP BY {', '.join(group_columns)}, {stamp}, {', '.join(f'null_{f}, positive_{f}' for f in TOKEN_FIELDS)}",
        parameters)]
    native = []
    if need_native:
        native = [dict(row) for row in db.execute(
            f"{ranked}SELECT DISTINCT adapter, provider, model, session, {stamp} AS timestamp "
            "FROM ranked_usage WHERE alias_rank=1 AND session IS NOT NULL AND session!=''", parameters)]
    return cells, native


def aggregate(telemetry, rows, keys, prices, free_patterns=None):
    return finalise(combine((unit(telemetry, row, keys, prices, free_patterns) for row in rows), keys), keys)
