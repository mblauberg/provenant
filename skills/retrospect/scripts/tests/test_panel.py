"""Blinding and aggregate-only rater agreement contracts."""
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import panel


def label(identity, **changes):
    return dict(id=identity, basis="documented_rule", examines_source=False,
                change_cost_argument=True, user_objection="too_narrow", **changes)


class PanelAcceptance(unittest.TestCase):
    def test_blind_items_exclude_exposure_and_preserve_conversation(self):
        rows = [dict(id=f"private-row-{i}", session=f"session-{i}", experiment="EXPOSURE",
                     installed_release="RELEASE", instruction_version="INSTRUCTIONS",
                     period="PERIOD", project="PROJECT", within_session_era="ERA",
                     user=f"user {i}", reply=f"reply {i}", next_user=f"next {i}")
                for i in range(8)]
        items, key = panel.blind_items(rows, seed=5)
        self.assertEqual(len(items), len(rows))
        self.assertEqual(set(key.values()), {r["id"] for r in rows})
        for item in items:
            self.assertEqual(set(item), {"id", "user", "reply", "next_user"})
            original = next(r for r in rows if r["id"] == key[item["id"]])
            for field in ("user", "reply", "next_user"):
                self.assertEqual(item[field], original[field])
            self.assertNotIn("private-row", item["id"])
        serialized = json.dumps(items)
        for secret in ("EXPOSURE", "RELEASE", "INSTRUCTIONS", "PERIOD", "PROJECT", "ERA", "session-"):
            self.assertNotIn(secret, serialized)
        self.assertEqual(panel.blind_items(rows, seed=5), (items, key))

    def test_label_validation_requires_one_valid_label_per_blind_item(self):
        items = [dict(id="blind-a", user="u", reply="r", next_user="n")]
        valid = label("blind-a")
        panel.validate_labels(items, [valid])
        for invalid in ([], [valid, valid], [dict(valid, id="unknown")],
                        [dict(valid, basis="unrecognised")],
                        [dict(valid, examines_source="false")],
                        [dict(valid, change_cost_argument=1)]):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                panel.validate_labels(items, invalid)

    def test_stronger_audit_summary_exposes_disagreement_without_free_text(self):
        cheap = [label("blind-a"), label("blind-b")]
        audit = [label("blind-a"), dict(label("blind-b"), examines_source=True)]
        summary = panel.summarise(cheap, audit)
        self.assertIsInstance(summary, dict)
        encoded = json.dumps(summary)
        self.assertNotIn("blind-a", encoded)
        self.assertNotIn("blind-b", encoded)
        self.assertNotEqual(summary, panel.summarise(cheap, cheap))


if __name__ == "__main__":
    unittest.main()
