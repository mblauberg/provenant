import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("hook_bench", Path(__file__).with_name("hook.py"))
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


def answer():
    page = json.dumps({"items": [{"answer": {"id": "a"}, "delivery": {"answer_id": "a", "target_session": "chair"}}], "truncated": False})
    return json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                        "additionalContext": "Answers\n<untrusted_answer_data>\n" + page + "\n</untrusted_answer_data>"}}).encode()


class HookBenchmarkTests(unittest.TestCase):
    def run_case(self, delivery_count, populated=True):
        args = SimpleNamespace(binary="/cli", cwd="/project", session="chair", samples=20,
                               state_dir="/state" if populated else None)
        seen = 0

        def run(_command, **kwargs):
            nonlocal seen
            payload = json.loads(kwargs["input"])
            if "session_id" in payload:
                seen += 1
                return SimpleNamespace(stdout=answer() if seen <= delivery_count else b"", stderr=b"")
            return SimpleNamespace(stdout=b"", stderr=b"")

        # No real time or process is involved in these checks.
        with patch.object(hook.subprocess, "run", side_effect=run), patch.object(hook.time, "monotonic_ns", return_value=0):
            return hook.measure(args, "/state")

    def test_populated_requires_every_sample_to_deliver(self):
        for count in (0, 1, 19):
            with self.subTest(count=count), self.assertRaisesRegex(RuntimeError, f"delivered {count}/20"):
                self.run_case(count)
        result = self.run_case(20)
        self.assertEqual(result["delivered_samples"], 20)
        self.assertIn("delivery_wall_p95_ms", result)

    def test_empty_lookup_has_no_delivery_figures(self):
        result = self.run_case(0, populated=False)
        self.assertEqual(result["delivered_samples"], 0)
        self.assertIn("unregistered_lookup_wall_p95_ms", result)
        self.assertNotIn("delivery_wall_p95_ms", result)

    def test_only_answer_pages_count(self):
        self.assertTrue(hook.delivered(answer(), "chair"))
        self.assertFalse(hook.delivered(answer(), "other"))
        self.assertFalse(hook.delivered(b'{"hookSpecificOutput":{"permissionDecision":"deny"}}', "chair"))
        self.assertFalse(hook.delivered(b'not json', "chair"))
        empty = json.loads(answer())
        empty["hookSpecificOutput"]["additionalContext"] = "<untrusted_answer_data>\n{\"items\": []}\n</untrusted_answer_data>"
        self.assertFalse(hook.delivered(json.dumps(empty).encode(), "chair"))


if __name__ == "__main__":
    unittest.main()
