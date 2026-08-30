from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL_CRAFT = ROOT / "skills" / "skill-craft"
SKILL = SKILL_CRAFT / "SKILL.md"
AUDIT = SKILL_CRAFT / "references" / "audit.md"
METHOD = SKILL_CRAFT / "references" / "method.md"
FIXTURES = SKILL_CRAFT / "evals" / "boundary_trace_cases.yaml"
SPEC = ROOT / "docs" / "specs" / "harness" / "lifecycle.md"
ADR = ROOT / "docs" / "adr" / "0001-personal-first-product-compatible.md"


def test_local_history_audit_is_local_first_and_export_gated():
    skill = SKILL.read_text()
    audit = AUDIT.read_text()
    compact = " ".join(audit.split())
    lowered = compact.lower()

    # Behaviour and routing fixtures constrain this branch; prose length is a
    # review concern, not a semantic threshold.
    assert (
        "direct user request authorises read-only analysis of the named "
        "local histories" in lowered
    )
    assert "Do not require a second privacy receipt" in compact
    assert "same authorised session is local delivery, not sharing/export" in compact
    assert "needs no second disclosure confirmation" in compact
    assert "persistent repository/shared artifact" in compact
    assert "sending raw excerpts to another provider" in compact
    assert "new audience or external destination" in compact
    assert "confirm with the user" in compact
    assert "Unsupported or unattributable evidence is `N/A`, never zero" in compact

    method = " ".join(METHOD.read_text().split())
    assert "inspect source history in place" in method
    assert "cannot score a new skill" in method
    assert "same authorised session is local delivery, not export" in method
    assert "raw cross-provider handoff" in method
    assert "Minimise authorised exports to aggregates or paraphrases" in method

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
    assert "audit" in audit_then_evaluate["prompt"].lower()
    assert audit_then_evaluate["expected"]["primary_skill"] == "evaluate"
    assert audit_then_evaluate["expected"]["companion_skills"] == ["skill-craft"]


def test_normative_docs_match_the_local_first_contract():
    spec = SPEC.read_text()
    adr = ADR.read_text()
    compact_adr = " ".join(adr.split())
    compact_spec = " ".join(spec.split())

    assert "Issue #23" in spec
    assert "Status: Base implementation machine verified" not in spec
    assert "current contract permits direct read-only analysis" in compact_spec
    assert "## Local skill evidence and shared exports" in spec
    assert "same authorised session is local delivery, not sharing/export" in compact_spec
    assert "persistent repository/shared artifact" in compact_spec
    assert "no provider-native adapter or producer" in compact_spec
    assert "History predating a skill" in spec
    assert "cannot score that skill" in compact_spec
    assert "prospective contract coverage" in spec
    assert "Unsupported or unattributable evidence is `N/A`, never zero" in spec
    assert "direct request for read-only local history analysis" in compact_adr
    assert "raw cross-provider handoff" in compact_adr
    retired_names = (
        "collect_telemetry.py",
        "validate_telemetry.py",
        "SKILL-TELEMETRY.template.json",
    )
    assert not [name for name in retired_names if name in spec]
