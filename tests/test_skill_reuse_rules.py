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
    cases = json.loads(read("skills/skill-craft/evals/trigger_cases.json"))["cases"]
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
