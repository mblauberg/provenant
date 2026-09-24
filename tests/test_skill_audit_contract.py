from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL_CRAFT = ROOT / "skills" / "skill-craft"
FIXTURES = SKILL_CRAFT / "evals" / "boundary_trace_cases.yaml"


def test_local_history_routing_separates_audit_from_export():
    # The strictly schema-validated evals/trigger_cases.yaml (see
    # tests/test_skill_eval_fixtures.py) only carries the canonical 3+3+3
    # positive/negative/boundary set; these richer local-history/export
    # routing cases carry extra keys (branch, tags beyond that contract) and
    # live in boundary_trace_cases.yaml's routing_reference_cases instead.
    cases = {
        case["id"]: case
        for case in yaml.safe_load(FIXTURES.read_text())["routing_reference_cases"]
    }

    local = cases["sc-003"]
    assert local["relation"] == "positive"
    assert local["expected"]["primary_skill"] == "skill-craft"
    assert local["expected"]["companion_skills"] == []
    assert local["expected"].get("branch") == "audit"
    assert {"local-history", "direct"} <= set(local["tags"])

    export_only = cases["sc-004"]
    assert export_only["relation"] == "negative"
    assert export_only["expected"] == {
        "primary_skill": "release",
        "companion_skills": [],
    }
    assert {"local-history", "export"} <= set(export_only["tags"])

    audit_then_export = cases["sc-009"]
    assert audit_then_export["relation"] == "boundary"
    assert audit_then_export["expected"]["primary_skill"] == "release"
    assert audit_then_export["expected"]["companion_skills"] == ["skill-craft"]
    assert audit_then_export["expected"].get("branch") == "audit"
    assert {"composition", "local-history", "export"} <= set(
        audit_then_export["tags"]
    )

    audit_then_evaluate = cases["sc-008"]
    assert audit_then_evaluate["expected"]["primary_skill"] == "evaluate"
    assert audit_then_evaluate["expected"]["companion_skills"] == ["skill-craft"]
