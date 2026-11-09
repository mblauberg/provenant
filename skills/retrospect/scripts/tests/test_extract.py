import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from extract import extract, _usage, _sum_usage

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
        # The preceding tool call omitted cache counters, so reply totals stay unknown.
        self.assertIsNone(first['usage']['cache_write'])
        self.assertIsNone(first['usage']['cache_write_1h'])
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

    def test_duplicate_sources_merge_independently_of_order(self):
        native = dict(source('claude'), project='native-project')
        lane = dict(native, host='lane', project='lane-project',
                    experiment='on', sample='sample', graph='graph')
        outcome = {'evidence': {'sample': 'sample', 'graph': 'graph'}}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'outcomes.json'
            path.write_text(json.dumps({'rows': [outcome]}))
            forward = extract({'sources': [native, lane], 'outcomes': str(path)})
            reverse = extract({'sources': [lane, native], 'outcomes': str(path)})
        self.assertEqual(forward, reverse)
        for row in forward:
            self.assertEqual(row['host'], 'lane')
            self.assertEqual(row['experiment'], 'on')
            self.assertEqual(row['outcome'], outcome)
            self.assertIsNone(row['project'])

    def test_missing_usage_components_stay_unknown_and_known_totals_survive(self):
        claude = _usage({'input_tokens': 3, 'cache_creation_input_tokens': 9,
                         'output_tokens': 2}, True)
        self.assertIsNone(claude['cache_write'])
        self.assertIsNone(claude['cache_write_1h'])
        self.assertEqual(claude['cache_creation_total'], 9)
        codex = _usage({'input_tokens': 9, 'output_tokens': 2})
        self.assertIsNone(codex['uncached_input'])
        self.assertIsNone(codex['cache_read'])
        self.assertEqual(codex['input_total'], 9)
        complete = _usage({'input_tokens': 3, 'cache_read_input_tokens': 4,
                           'cache_creation_input_tokens': 5,
                           'cache_creation': {'ephemeral_1h_input_tokens': 2},
                           'output_tokens': 6}, True)
        self.assertEqual((complete['cache_write'], complete['cache_write_1h']), (3, 2))
        combined = _sum_usage(complete, claude)
        self.assertIsNone(combined['cache_read'])
        self.assertIsNone(combined['cache_write'])
        self.assertEqual(combined['output'], 8)
        self.assertIsNone(_sum_usage(complete, _usage(None))['output'])

    def test_codex_cumulative_counter_missing_previous_value_has_unknown_delta(self):
        events = [{'type': 'session_meta', 'payload': {'id': 'synthetic-missing-cache'}}]
        for i, totals in enumerate(({'input_tokens': 9, 'output_tokens': 2},
                                    {'input_tokens': 15, 'cached_input_tokens': 3, 'output_tokens': 4},
                                    {'input_tokens': 20, 'cached_input_tokens': 4, 'output_tokens': 6})):
            events += [
                {'type': 'event_msg', 'payload': {'type': 'user_message', 'message': f'Question {i}'}},
                {'type': 'response_item', 'payload': {'type': 'message', 'role': 'assistant',
                    'phase': 'final_answer', 'content': [{'type': 'output_text', 'text': f'Answer {i}'}]}},
                {'type': 'event_msg', 'payload': {'type': 'token_count',
                    'info': {'total_token_usage': totals}}}]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'rollout.jsonl'
            path.write_text('\n'.join(map(json.dumps, events)))
            rows = extract({'sources': [{'host': 'codex', 'path': str(path)}]})
        self.assertEqual([r['usage']['input_total'] for r in rows], [9, 6, 5])
        self.assertEqual([r['usage']['cache_read'] for r in rows], [None, None, 1])
        self.assertEqual([r['usage']['uncached_input'] for r in rows], [None, None, 4])

    def test_claude_lane_tool_text_is_not_a_second_reply_and_result_usage_wins(self):
        events = [
            {'type': 'assistant', 'session_id': 'synthetic-stream', 'message': {
                'id': 'synthetic-tool-text', 'role': 'assistant', 'content': [
                    {'type': 'text', 'text': 'I will inspect it.'},
                    {'type': 'tool_use', 'id': 'synthetic-tool', 'name': 'Read', 'input': {}}],
                'usage': {'input_tokens': 1, 'output_tokens': 2}}},
            {'type': 'user', 'session_id': 'synthetic-stream', 'message': {
                'role': 'user', 'content': [{'type': 'tool_result',
                'tool_use_id': 'synthetic-tool', 'content': 'example'}]}},
            {'type': 'assistant', 'session_id': 'synthetic-stream', 'message': {
                'id': 'synthetic-final', 'role': 'assistant',
                'content': [{'type': 'text', 'text': 'Final answer.'}],
                'usage': {'input_tokens': 3, 'output_tokens': 4}}},
            {'type': 'result', 'session_id': 'synthetic-stream', 'subtype': 'success',
             'is_error': False, 'result': 'Final answer.', 'usage': {
                'input_tokens': 10, 'output_tokens': 20, 'cache_read_input_tokens': 30,
                'cache_creation_input_tokens': 40, 'cache_creation': {
                    'ephemeral_5m_input_tokens': 15, 'ephemeral_1h_input_tokens': 25}}}]
        empty_assistant = copy.deepcopy(events[2])
        empty_assistant['message']['content'] = []
        shapes = (events, [empty_assistant, events[-1]])
        for shape in shapes:
            with self.subTest(assistant_events=len(shape) - 1), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'stream.jsonl'
                path.write_text('\n'.join(map(json.dumps, shape)))
                src = {'host': 'lane', 'path': str(path), 'experiment': 'synthetic-arm'}
                rows = extract({'sources': [src]})
                self.assertEqual(len(rows), 1)
                self.assertEqual(rows[0]['reply'], 'Final answer.')
                self.assertEqual(rows[0]['experiment'], 'synthetic-arm')
                self.assertEqual(rows, extract({'sources': [src, src]}))
                usage = rows[0]['usage']
                self.assertEqual((usage['uncached_input'], usage['cache_read'],
                                  usage['cache_write'], usage['cache_write_1h'], usage['output']),
                                 (10, 30, 15, 25, 20))
                self.assertEqual(usage['input_total'], 80)
                self.assertEqual(usage['cache_creation_total'], 40)

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
