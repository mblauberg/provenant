import json
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text()


def test_skill_promotion_uses_structural_form_and_routing_fixture():
    proposal = yaml.safe_load(read(".github/ISSUE_TEMPLATE/skill-proposal.yml"))
    proposal_fields = {
        field["id"]: field for field in proposal["body"] if "id" in field
    }
    cases = yaml.safe_load(read("skills/skill-craft/evals/trigger_cases.yaml"))["cases"]
    promotion = next(
        case for case in cases
        if case["relation"] == "boundary" and case["expected"]["primary_skill"] == "skill-craft"
        and "evaluate" in case["expected"]["companion_skills"]
    )

    assert promotion["relation"] == "boundary"
    assert promotion["expected"] == {
        "primary_skill": "skill-craft",
        "companion_skills": ["evaluate"],
    }
    status = proposal_fields["promotion-status"]
    rationale = proposal_fields["promotion-rationale"]
    assert len(status["attributes"]["options"]) >= 2
    assert status["validations"]["required"] is True
    assert rationale["type"] == "textarea"
    assert rationale["validations"]["required"] is True
    assert not (ROOT / "skills/skill-craft/scripts/promotion_readiness.py").exists()


def test_document_profile_classifies_interactive_html_and_its_conditional_evidence():
    registry = json.loads(read("config/delivery-profiles.json"))
    document = registry["profiles"]["document"]
    surfaces = registry["artifact_type_surfaces"]
    assert {"html", "interactive-document"} <= set(document["artifact_types"])
    assert surfaces["html"] == ["generated-artifact"]
    assert surfaces["interactive-document"] == ["generated-artifact", "source"]
    assert document["conditional_evidence"] == {
        "html": {"deterministic": ["link-integrity"], "judgement": []},
        "interactive-document": {
            "deterministic": ["link-integrity", "interaction-smoke"],
            "judgement": [],
        },
    }
