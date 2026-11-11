#!/usr/bin/env python3
"""Local retrospective: prepare private blind packets, then publish aggregates."""
import argparse
import json
from pathlib import Path
import random

import analysis
from extract import extract
import panel


def read(path):
    return json.loads(Path(path).read_text())


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n')


def prepare(manifest, directory, seed=7, audit_count=30):
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    rows = extract(manifest)
    items, key = panel.blind_items(rows, seed)
    audit = random.Random(seed).sample(items, min(audit_count, len(items)))
    for filename, value in (('rows.json', rows), ('blind.json', items),
                            ('key.json', key), ('audit-blind.json', audit)):
        write(directory / filename, value)
    (directory / 'rubric.txt').write_text(panel.RUBRIC)


def publish(directory, cheap_path, audit_path, iterations=499, seed=7):
    directory = Path(directory)
    rows, key = read(directory / 'rows.json'), read(directory / 'key.json')
    cheap, strong = read(cheap_path), read(audit_path)
    panel.validate_labels(read(directory / 'blind.json'), cheap)
    panel.validate_labels(read(directory / 'audit-blind.json'), strong)
    metrics = {key[r['id']]: panel.metrics(r) for r in cheap}
    result = analysis.report(rows, metrics, iterations=iterations, seed=seed)
    result['raters'] = panel.summarise(cheap, strong)
    result['audit_sensitivity'] = analysis.report(
        [r for r in rows if r['id'] in {key[s['id']] for s in strong}],
        {key[r['id']]: panel.metrics(r) for r in strong}, iterations=iterations, seed=seed)
    result['interpretation'] = {
        'causal_attribution': 'not_established_by_observational_comparisons',
        'proxies': ['release_era_proxy_is_tag_chronology_only',
                    'captured_instruction_hash_covers_captured_material_only'],
        'uncertainty': 'approximate_session_residual_sign_flip_intervals',
        'weak_samples': 'inspect_session_counts_loso_missingness_and_rater_agreement',
        'findings': 'file_actionable_problems_even_when_no_change_is_identifiable'}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    prep = sub.add_parser('prepare', help='write private rows and blind model packets')
    prep.add_argument('manifest')
    prep.add_argument('directory')
    prep.add_argument('--seed', type=int, default=7)
    prep.add_argument('--audit-count', type=int, default=30)
    report = sub.add_parser('report', help='validate both raters; print aggregates only')
    report.add_argument('directory')
    report.add_argument('--cheap', required=True)
    report.add_argument('--audit', required=True)
    report.add_argument('--iterations', type=int, default=499)
    report.add_argument('--seed', type=int, default=7)
    lenses = sub.add_parser('lenses', help='print aggregate-only prompts for multiple models')
    lenses.add_argument('report')
    args = parser.parse_args()
    if args.command == 'prepare':
        prepare(read(args.manifest), args.directory, args.seed, args.audit_count)
    elif args.command == 'report':
        print(json.dumps(publish(args.directory, args.cheap, args.audit,
                                 args.iterations, args.seed), indent=2))
    else:
        # Consume the aggregate report produced by the report command.
        report = read(args.report)
        aggregates = {k: report[k] for k in ('replies', 'sessions', 'comparisons',
                                            'coverage', 'usage', 'outcomes', 'raters', 'interpretation')}
        print(json.dumps([{'lens': name, 'instruction': instruction,
                           'aggregates': aggregates} for name, instruction in panel.LENSES.items()], indent=2))


if __name__ == '__main__':
    main()
