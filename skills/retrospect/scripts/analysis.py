"""Session-balanced adjusted comparisons; no transcript fields enter reports."""
import collections
import itertools
import math
import random
import statistics

ADJUST = ('project', 'period', 'work_mix', 'context', 'compactions')
GROUPS = ('experiment', 'installed_release', 'instruction_version',
          'release_era_proxy', 'captured_instruction_hash', 'within_session_era')
METRICS = ('user_objection', 'examines_source', 'document_basis', 'code_basis',
           'change_cost_argument')


def _dot(a, b):
    return sum(x * y for x, y in zip(a, b))


def _residual(v, basis):
    v = list(v)
    # Reorthogonalise to keep redundant nuisance columns harmless.
    for _ in range(2):
        for q in basis:
            d = _dot(v, q)
            v = [x - d * y for x, y in zip(v, q)]
    return v


def _fit(rows, group, metric, adjust, levels):
    counts = collections.Counter(r['session'] for r in rows)
    weights = [1 / math.sqrt(counts[r['session']]) for r in rows]
    x = [float(r[group] == levels[1]) * w for r, w in zip(rows, weights)]
    y = [float(r[metric]) * w for r, w in zip(rows, weights)]
    columns = [[1.0] * len(rows)]
    for field in adjust:
        values = [r.get(field) for r in rows]
        if field == 'context' and all(isinstance(v, (int, float)) for v in values):
            scale = max(1, max(abs(v) for v in values))
            columns.append([v / scale for v in values])
        else:
            for level in sorted(set(values), key=str)[1:]:
                columns.append([float(v == level) for v in values])
    basis = []
    for column in columns:
        v = _residual([c * w for c, w in zip(column, weights)], basis)
        norm = math.sqrt(_dot(v, v))
        if norm > 1e-9:
            basis.append([c / norm for c in v])
    rx, ry = _residual(x, basis), _residual(y, basis)
    denom = _dot(rx, rx)
    if denom < 1e-9:
        return None
    beta = _dot(rx, ry) / denom
    null_scores = collections.defaultdict(float)
    scores = collections.defaultdict(float)
    for r, a, b in zip(rows, rx, ry):
        null_scores[r['session']] += a * b / denom
        scores[r['session']] += a * (b - beta * a) / denom
    return beta, list(null_scores.values()), list(scores.values())


def _quantiles(values):
    values = sorted(values)
    return [values[int((len(values) - 1) * q)] for q in (.025, .975)]


def compare(rows, group='experiment', metric='user_objection', adjust=ADJUST,
            iterations=499, seed=7):
    """Binary contrast, second level minus first; rank loss is not an effect."""
    selected = [r for r in rows if r.get(group) is not None
                and isinstance(r.get(metric), (int, float))
                and math.isfinite(r[metric])]
    levels = sorted({r[group] for r in selected}, key=str)
    result = dict(status='non_identifiable', raw_difference=None,
                  adjusted_difference=None, permutation_interval=None,
                  confidence_interval=None, p_value=None,
                  permutation_unit='session', interval_method='session_residual_sign_flip',
                  counts={'replies': len(selected),
                          'sessions': len({r['session'] for r in selected}),
                          'excluded': len(rows) - len(selected)},
                  loso={'min': None, 'max': None, 'sign_flip': False,
                        'non_identifiable': 0},
                  unknown_adjustment=sum(any(r.get(k) is None for k in adjust) for r in selected))
    if len(levels) != 2:
        result['reason'] = 'requires_two_observed_levels'
        return result
    means = []
    for level in levels:
        sessions = collections.defaultdict(list)
        for r in selected:
            if r[group] == level:
                sessions[r['session']].append(r[metric])
        means.append(statistics.mean(statistics.mean(v) for v in sessions.values()))
    result['raw_difference'] = means[1] - means[0]
    fit = _fit(selected, group, metric, adjust, levels)
    if fit is None:
        result['reason'] = 'group_inseparable_from_adjustment'
        return result
    beta, null_scores, residual_scores = fit
    if len(null_scores) < 3:
        result['reason'] = 'too_few_sessions'
        return result
    rng = random.Random(seed)
    # Exact sign permutations for small samples, Monte Carlo otherwise.
    signs = (list(itertools.product((-1, 1), repeat=len(null_scores)))
             if len(null_scores) <= 10 else
             [[rng.choice((-1, 1)) for _ in null_scores] for _ in range(iterations)])
    null = [_dot(s, null_scores) for s in signs]
    shifted = [beta + _dot(s, residual_scores) for s in signs]
    result.update(status='identifiable', adjusted_difference=beta,
                  permutation_interval=_quantiles(null),
                  confidence_interval=_quantiles(shifted),
                  p_value=(1 + sum(abs(v) >= abs(beta) - 1e-12 for v in null)) / (1 + len(null)))
    estimates = []
    for session in {r['session'] for r in selected}:
        subset = [r for r in selected if r['session'] != session]
        remaining = {r[group] for r in subset}
        estimate = _fit(subset, group, metric, adjust, levels) if len(remaining) == 2 else None
        if estimate is None:
            result['loso']['non_identifiable'] += 1
        else:
            estimates.append(estimate[0])
    if estimates:
        result['loso'].update(min=min(estimates), max=max(estimates),
                              sign_flip=any(v * beta < -1e-12 for v in estimates))
    return result


def report(rows, labels=None, iterations=499, seed=7):
    """Publish only counts and numeric comparisons; opaque dimension aliases."""
    # Labels are joined by private row identity before aggregating.
    if labels is not None:
        rows = [dict(r, **labels.get(r['id'], {})) for r in rows]
    output = {'replies': len(rows), 'sessions': len({r['session'] for r in rows}),
              'comparisons': [], 'usage': {}, 'coverage': {}}
    for host in ('claude', 'codex', 'lane'):
        output['coverage'][host] = sum(r.get('host') == host for r in rows)
    outcomes = {}
    for row in rows:
        outcome = row.get('outcome')
        if outcome is not None:
            evidence = outcome['evidence']
            outcomes[(evidence['sample'], evidence.get('graph'))] = outcome
    output['outcomes'] = {
        'joined': len(outcomes),
        'unattributed': sum(o['stratum'] == 'unattributed' for o in outcomes.values()),
        'unmeasured': sum(o['stratum'] == 'unmeasured' for o in outcomes.values()),
        'mature': sum(o['mature'] for o in outcomes.values()),
        'ci_measured': sum(o['signals'].get('ci_failures') is not None for o in outcomes.values()),
    }
    usages = [r.get('usage') for r in rows]
    for token in ('uncached_input', 'cache_read', 'cache_write', 'cache_write_1h', 'output', 'reasoning'):
        values = [u[token] for u in usages if u is not None and u.get(token) is not None]
        output['usage'][token] = {'observed_total': sum(values), 'measured_replies': len(values)}
    for group in GROUPS:
        levels = sorted({r[group] for r in rows if r.get(group) is not None}, key=str)
        output['coverage'][group] = {'known': sum(r.get(group) is not None for r in rows),
                                     'levels': len(levels)}
        for i, j in itertools.combinations(range(len(levels)), 2):
            subset = [r for r in rows if r.get(group) in (levels[i], levels[j])]
            for metric in METRICS:
                other_exposures = tuple(k for k in GROUPS if k != group
                                        and any(r.get(k) is not None for r in subset))
                comparison = compare(subset, group, metric, adjust=ADJUST + other_exposures,
                                     iterations=iterations, seed=seed)
                output['comparisons'].append({'dimension': group, 'levels': [i + 1, j + 1],
                                              'metric': metric, **comparison})
    return output
