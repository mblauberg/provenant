from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL_CRAFT = ROOT / "skills" / "skill-craft"
AUDIT = SKILL_CRAFT / "references" / "audit.md"
METHOD = SKILL_CRAFT / "references" / "method.md"
FIXTURES = SKILL_CRAFT / "evals" / "boundary_trace_cases.yaml"


def test_skill_craft_references_keep_owned_structure():
    skill = (SKILL_CRAFT / "SKILL.md").read_text()
    audit = AUDIT.read_text()
    method = METHOD.read_text()

    assert "[references/author.md](references/author.md)" in skill
    assert "[references/audit.md](references/audit.md)" in skill
    assert "[method.md](method.md)" in audit
    assert {"# Audit branch", "## Evidence modes", "## Workflow", "## Output"} <= {
        line.strip() for line in audit.splitlines() if line.lstrip().startswith("#")
    }
    assert {
        "# Skill-audit method",
        "## Scoring",
        "## Static checks",
        "## Local and shared evidence",
    } <= {
        line.strip() for line in method.splitlines() if line.lstrip().startswith("#")
    }
    retired = [
        SKILL_CRAFT / "scripts" / "collect_telemetry.py",
        SKILL_CRAFT / "scripts" / "validate_telemetry.py",
        SKILL_CRAFT / "SKILL-TELEMETRY.template.json",
    ]
    assert not [path for path in retired if path.exists()]


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
