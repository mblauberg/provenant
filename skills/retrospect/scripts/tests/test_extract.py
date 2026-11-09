import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from extract import extract

FIXTURES = Path(__file__).parent / 'fixtures'


def source(host):
    return {'host': host, 'path': str(FIXTURES / (host + '.jsonl'))}


class ExtractionTests(unittest.TestCase):
    def test_claude_deduplicates_streams_and_keeps_unknowns_and_changes(self):
        src = source('claude')
        src['exposures'] = [{'timestamp': '2030-01-01', 'experiment': 'off'},
                            {'timestamp': '2030-01-02', 'experiment': 'on'}]
        manifest = {'sources': [src, src], 'releases': [
            {'timestamp': '2029-12-01', 'release': 'synthetic-release'}]}
        first, second = extract(manifest)
        self.assertEqual(first['usage']['output'], 7)
        self.assertEqual(first['usage']['uncached_input'], 5)
        self.assertEqual(first['usage']['cache_write'], 3)
        self.assertEqual(first['usage']['cache_write_1h'], 2)
        self.assertEqual(first['context'], 12)
        self.assertIsNone(second['context'])
        self.assertTrue(first['next_user'].startswith('Explain the reason.'))
        self.assertEqual((first['experiment'], second['experiment']), ('off', 'on'))
        self.assertEqual((first['within_session_era'], second['within_session_era']), (0, 1))
        self.assertEqual(second['compactions'], 1)
        self.assertIsNone(first['compactions'])
        self.assertIsNone(second['usage'])
        self.assertIsNone(first['installed_release'])
        self.assertIsNone(first['instruction_version'])
        self.assertEqual(first['release_era_proxy'], 'synthetic-release')
        self.assertNotEqual(first['captured_instruction_hash'], second['captured_instruction_hash'])

    def test_codex_native_final_replies_and_cumulative_usage(self):
        first, second = extract({'sources': [source('codex')]})
        self.assertEqual([first['reply'], second['reply']], ['First answer.', 'Second answer.'])
        self.assertEqual(first['next_user'], 'Why that answer?')
        self.assertEqual([r['usage']['uncached_input'] for r in (first, second)], [7, 4])
        self.assertEqual([r['usage']['output'] for r in (first, second)], [4, 3])
        self.assertEqual([r['usage']['cache_read'] for r in (first, second)], [3, 2])
        self.assertEqual(second['compactions'], 1)
        self.assertNotEqual(first['captured_instruction_hash'], second['captured_instruction_hash'])

    def test_lane_exec_final_and_existing_outcome_join(self):
        src = source('lane')
        src.update(sample='synthetic-sample', graph='synthetic-graph')
        outcome = {'evidence': {'sample': src['sample'], 'graph': src['graph']},
                   'stratum': 'unattributed', 'merge': None, 'mature': False,
                   'signals': {'ci_failures': None}}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'outcomes.json'
            path.write_text(json.dumps({'rows': [outcome]}))
            rows = extract({'sources': [src], 'outcomes': str(path)})
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['reply'], 'The fixture checks out.')
        self.assertEqual(rows[0]['usage']['uncached_input'], 6)
        self.assertEqual(rows[0]['usage']['output'], 3)
        self.assertEqual(rows[0]['outcome'], outcome)
        self.assertIsNone(rows[0]['timestamp'])
        self.assertIsNone(rows[0]['experiment'])

    def test_lane_claude_matches_native_identity(self):
        native = source('claude')
        lane = dict(native, host='lane')
        rows = extract({'sources': [native, lane]})
        self.assertEqual(len(rows), 2)
        self.assertEqual(sum((r['usage'] or {}).get('output') or 0 for r in rows), 7)

    def test_every_captured_attachment_affects_hash(self):
        original = [json.loads(s) for s in (FIXTURES / 'claude.jsonl').read_text().splitlines()]
        changed = copy.deepcopy(original)
        changed[0]['message']['content'] = changed[0]['message']['content'].replace('Prefer evidence.', 'A different second attachment.')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'session.jsonl'
            hashes = []
            for events in (original, changed):
                path.write_text('\n'.join(map(json.dumps, events)))
                hashes.append(extract({'sources': [{'host': 'claude', 'path': str(path)}]})[0]['captured_instruction_hash'])
        self.assertNotEqual(*hashes)


if __name__ == '__main__':
    unittest.main()
