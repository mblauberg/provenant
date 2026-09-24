"""Structural contract for the tracker skill's duplicate/standalone/epic placement eval (#872)."""

from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "skills" / "tracker" / "evals" / "placement_cases.yaml"
VALID_DECISIONS = {"duplicate", "new-standalone", "new-under-epic"}


def load():
    return yaml.safe_load(FIXTURE.read_text())


def test_fixture_covers_a_duplicate_a_standalone_bug_and_an_epic_placement():
    data = load()
    assert data["schema_version"] == 1
    assert data["target_skill"] == "tracker"
    cases = {case["id"]: case for case in data["cases"]}
    assert set(cases) == {"placement-duplicate", "placement-standalone-bug", "placement-epic"}

    decisions = {case_id: case["decision"] for case_id, case in cases.items()}
    assert decisions == {
        "placement-duplicate": "duplicate",
        "placement-standalone-bug": "new-standalone",
        "placement-epic": "new-under-epic",
    }
    assert set(decisions.values()) <= VALID_DECISIONS


def test_every_case_records_the_searches_it_ran_and_a_scenario_and_action():
    for case in load()["cases"]:
        assert isinstance(case["scenario"], str) and case["scenario"].strip()
        assert isinstance(case["action"], str) and case["action"].strip()
        searches = case["searches_run"]
        assert isinstance(searches, list) and len(searches) >= 2
        assert all(isinstance(query, str) and query.strip() for query in searches)
