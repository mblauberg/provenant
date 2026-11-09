"""Medium: exercise the local workflow with synthetic native histories."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import audit

FIXTURES = Path(__file__).parent / 'fixtures'


class AuditWorkflow(unittest.TestCase):
    def test_prepare_label_report_and_aggregate_lenses_leave_sources_unchanged(self):
        sources = [dict(host=host, path=str(FIXTURES / f'{host}.jsonl'),
                        project='PRIVATE PROJECT', experiment=host,
                        sample='synthetic-sample', graph='synthetic-graph')
                   for host in ('claude', 'codex', 'lane')]
        before = {s['path']: Path(s['path']).read_bytes() for s in sources}
        outcome = {'evidence': {'sample': 'synthetic-sample', 'graph': 'synthetic-graph'},
                   'stratum': 'unattributed', 'mature': False,
                   'signals': {'ci_failures': None}}
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            outcomes = directory / 'outcomes.json'
            outcomes.write_text(json.dumps({'rows': [outcome]}))
            audit.prepare({'sources': sources, 'outcomes': str(outcomes)}, directory,
                          audit_count=3)
            for items_file, labels_file in (('blind.json', 'cheap.json'),
                                             ('audit-blind.json', 'strong.json')):
                items = audit.read(directory / items_file)
                labels = [dict(id=i['id'], basis='documented_rule', examines_source=False,
                               change_cost_argument=False,
                               user_objection=None if i['next_user'] is None else 'none')
                          for i in items]
                audit.write(directory / labels_file, labels)
            report = audit.publish(directory, directory / 'cheap.json', directory / 'strong.json', iterations=19)
            self.assertEqual(report['replies'], 5)
            self.assertEqual(report['raters']['audited'], 3)
            self.assertEqual(report['outcomes']['joined'], 1)
            self.assertEqual(report['outcomes']['unattributed'], 1)
            self.assertEqual(report['outcomes']['ci_measured'], 0)
            self.assertEqual(report['coverage']['installed_release']['known'], 0)
            encoded = json.dumps(report)
            for secret in ('PRIVATE PROJECT', 'synthetic-sample', 'synthetic-graph',
                           'First answer.', 'synthetic-codex'):
                self.assertNotIn(secret, encoded)
            report_file = directory / 'report.json'
            audit.write(report_file, report)
            result = subprocess.run([sys.executable, str(Path(audit.__file__)), 'lenses',
                                     str(report_file)], capture_output=True, text=True, check=True)
            lenses = json.loads(result.stdout)
            self.assertEqual({r['lens'] for r in lenses}, {'causes', 'reasoning', 'changes'})
            self.assertTrue(all(r['aggregates']['replies'] == 5 for r in lenses))
        for path, content in before.items():
            self.assertEqual(Path(path).read_bytes(), content)


if __name__ == '__main__':
    unittest.main()
