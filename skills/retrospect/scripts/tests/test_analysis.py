"""Synthetic acceptance cases for retrospective comparisons (issue #3064)."""
import copy
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import analysis


def row(session, group, value, project="synthetic-project", period="2030-01-01"):
    return dict(id=f"private-{session}", session=session, experiment=group,
                project=project, period=period, work_mix="implementation",
                context=100, compactions=0, score=value,
                reply="PRIVATE REPLY", user="PRIVATE USER", next_user="PRIVATE FOLLOWUP")


class ComparisonAcceptance(unittest.TestCase):
    def compare(self, rows, **kwargs):
        return analysis.compare(rows, metric="score", iterations=199, seed=17, **kwargs)

    def test_recovers_an_effect_with_overlapping_work_and_projects(self):
        rows = [row(f"s{g}{i}", g, .2 + .35 * (g == "B") + .02 * ((i % 3) - 1),
                    project=f"project-{i % 2}", period=f"2030-01-0{1 + i % 2}")
                for g in ("A", "B") for i in range(12)]
        result = self.compare(rows)
        self.assertEqual(result["status"], "identifiable")
        self.assertAlmostEqual(result["adjusted_difference"], .35, places=6)
        self.assertLess(result["p_value"], .05)
        self.assertEqual(result["permutation_unit"], "session")
        self.assertIsNotNone(result["permutation_interval"])
        self.assertEqual(result["interval_method"], "session_residual_sign_flip")
        self.assertLess(result["confidence_interval"][0], .35)
        self.assertGreater(result["confidence_interval"][1], .35)

    def test_project_adjustment_removes_a_raw_difference(self):
        rows = []
        for group in ("A", "B"):
            for i in range(16):
                high = i < (4 if group == "A" else 12)
                rows.append(row(f"{group}{i}", group, .8 if high else .2,
                                project="project-high" if high else "project-low"))
        result = self.compare(rows)
        self.assertGreater(result["raw_difference"], .2)
        self.assertEqual(result["status"], "identifiable")
        self.assertAlmostEqual(result["adjusted_difference"], 0, places=8)

    def test_complete_project_and_time_confound_is_non_identifiable(self):
        rows = [row(f"{group}{i}", group, .1 + .7 * (group == "B"),
                    project=f"project-{group}", period=f"2030-01-0{1 + (group == 'B')}")
                for group in ("A", "B") for i in range(8)]
        result = self.compare(rows)
        self.assertEqual(result["status"], "non_identifiable")
        self.assertIsNone(result["adjusted_difference"])
        self.assertIsNone(result["permutation_interval"])
        self.assertIsNone(result["p_value"])

    def test_loso_exposes_a_result_driven_by_one_session(self):
        rows = [row(f"A{i}", "A", .05) for i in range(4)]
        rows += [row(f"B{i}", "B", 1 if i == 0 else 0) for i in range(4)]
        result = self.compare(rows)
        self.assertGreater(result["adjusted_difference"], 0)
        self.assertLess(result["loso"]["min"], 0)
        self.assertTrue(result["loso"]["sign_flip"])

    def test_turn_duplication_does_not_create_independent_sessions(self):
        rows = [row(f"{g}{i}", g, .15 + .2 * (g == "B") + .1 * (i % 3))
                for g in ("A", "B") for i in range(8)]
        duplicated = [dict(r, id=f"{r['id']}-{i}") for r in rows for i in range(5)]
        once, repeated = self.compare(rows), self.compare(duplicated)
        self.assertEqual(once["permutation_unit"], "session")
        self.assertAlmostEqual(once["adjusted_difference"], repeated["adjusted_difference"], places=8)
        for a, b in zip(once["permutation_interval"], repeated["permutation_interval"]):
            self.assertAlmostEqual(a, b, places=8)
        self.assertEqual(once["p_value"], repeated["p_value"])

    def test_aggregate_report_has_no_private_identity_or_text(self):
        rows = [row(f"secret-session-{g}{i}", g, .1 + .2 * (g == "B"),
                    project="PRIVATE PROJECT NAME")
                for g in ("A", "B") for i in range(5)]
        published = json.dumps(analysis.report(rows))
        for value in ("PRIVATE PROJECT NAME", "PRIVATE REPLY", "PRIVATE USER",
                      "PRIVATE FOLLOWUP", "secret-session", "private-secret"):
            self.assertNotIn(value, published)
        other = copy.deepcopy(rows)
        for r in other:
            r["project"] = "ANOTHER PROJECT"
        self.assertEqual(analysis.report(rows), analysis.report(other))


if __name__ == "__main__":
    unittest.main()
