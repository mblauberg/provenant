import re
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL_CRAFT = ROOT / "skills" / "skill-craft"
AUDIT = SKILL_CRAFT / "references" / "audit.md"
METHOD = SKILL_CRAFT / "references" / "method.md"
FIXTURES = SKILL_CRAFT / "evals" / "boundary_trace_cases.yaml"
SPEC = ROOT / "docs" / "specs" / "harness" / "lifecycle.md"
DISCLOSURE = ROOT / "docs" / "specs" / "harness" / "disclosure-refactor.md"
ADR = ROOT / "docs" / "adr" / "0001-personal-first-product-compatible.md"


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
    assert "## Local skill evidence and shared exports" in SPEC.read_text()

    retired = [
        SKILL_CRAFT / "scripts" / "collect_telemetry.py",
        SKILL_CRAFT / "scripts" / "validate_telemetry.py",
        SKILL_CRAFT / "SKILL-TELEMETRY.template.json",
    ]
    assert not [path for path in retired if path.exists()]


def test_local_history_policy_boundaries_are_present_in_owned_sections():
    evidence = " ".join(
        re.search(r"(?ms)^## Evidence modes\n(.*?)(?=^## |\Z)", AUDIT.read_text())
        .group(1)
        .split()
    ).casefold()
    method = " ".join(
        re.search(
            r"(?ms)^## Local and shared evidence\n(.*?)(?=^## |\Z)",
            METHOD.read_text(),
        )
        .group(1)
        .split()
    ).casefold()

    assert re.search(
        r"(?:direct|explicit)(?: user)? request\s+"
        r"(?:authori[sz]e?s?|permit(?:s)?|allow(?:s)?)\s+read-only"
        r".{0,80}"
        r"named local histor(?:y|ies).{0,100}"
        r"(?:do not require|no|without|needs? no|requires? no) (?:a )?"
        r"(?:second|additional) (?:privacy )?(?:receipt|confirmation)",
        evidence,
    )
    assert re.search(
        r"(?:^|[.;:] )(?:read|inspect|analy[sz]e) "
        r"(?:source )?histories? in place"
        r".{0,100}(?:keep|remain) .*local.{0,100}"
        r"(?:never|do not) commit raw transcripts.{0,60}"
        r"(?:promote|become) .*project truth",
        evidence,
    )
    assert re.search(
        r"(?:aggregate|paraphras).{0,80}same authori[sz]ed session"
        r".{0,80}local delivery.{0,60}(?:not sharing[/ ]?export|not export)"
        r".{0,80}need(?:s)? no second disclosure confirmation",
        evidence,
    )
    assert re.search(
        r"separate authority (?:is )?(?:required|needed)(?: before)?"
        r"(?: creating)?(?: a)? persistent "
        r"(?:repository/shared artifact|repository|shared artifact)"
        r".{0,100}raw.{0,60}(?:provider|cross-provider).{0,80}new audience"
        r".{0,40}external destination",
        evidence,
    )
    assert re.search(
        r"unsupported or unattributable.{0,40}`?n/a`?.{0,30}"
        r"(?:never|not|no) zero",
        evidence,
    )
    assert re.search(
        r"(?:historical data|history).{0,25}(?:older|predat\w+).{0,25}skill"
        r".{0,120}cannot score (?:a )?(?:new )?skill.{0,80}`?n/a`?",
        method,
    )


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
    disclosure = DISCLOSURE.read_text()
    compact_spec = " ".join(spec.split())
    compact_adr = " ".join(ADR.read_text().split())

    assert "current issue" not in compact_spec.lower()
    assert "Status: Base implementation machine verified" not in spec
    assert "## Local skill evidence and shared exports" in spec
    assert re.search(
        r"direct user request\s+authori[sz]e?s?\s+read-only",
        compact_spec,
    )
    assert re.search(
        r"persistent repository/shared artifact.{0,180}requires separate authority",
        compact_spec,
    )
    assert re.search(
        r"direct request for read-only local history analysis is sufficient authority",
        compact_adr,
    )
    assert re.search(
        r"seam is separate authority for a persistent shared artifact",
        compact_adr,
    )
    assert "Canonical decision:" in disclosure
    assert "superseded Fabric text is retained" not in disclosure
    assert "not a permanent full-tree checker" in disclosure
    assert "Fabric remains the task/authority owner" not in (
        ROOT / "docs" / "research" / "native-orchestration-and-discovery-surfaces.md"
    ).read_text()
    retired_names = (
        "collect_telemetry.py",
        "validate_telemetry.py",
        "SKILL-TELEMETRY.template.json",
    )
    assert not [name for name in retired_names if name in spec]
