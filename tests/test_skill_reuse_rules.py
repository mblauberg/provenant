import json
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text()


def compact(relative: str) -> str:
    return " ".join(read(relative).split())


def test_reuse_rules_live_in_the_existing_routing_portfolio_not_prose_pair_files():
    expected_ids = {
        "natural-writing": {"q145", "q150", "q151"},
        "engineering-writing": {"q084", "q088"},
        "engineering-docs": {"q075"},
        "legal-writing": {"q138"},
        "deliver": {"q055", "q058"},
        "skill-craft": {"q708"},
    }
    for skill, ids in expected_ids.items():
        assert not (ROOT / f"skills/{skill}/evals/discipline_cases.yaml").exists()
        cases = yaml.safe_load(read(f"skills/{skill}/evals/trigger_cases.yaml"))["cases"]
        selected = [case for case in cases if case["id"] in ids]
        assert {case["id"] for case in selected} == ids
        assert selected


def test_writing_and_documentation_rules_encode_the_reusable_boundaries():
    natural = compact("skills/natural-writing/references/anti-ai-taxonomy.md")
    legal = compact("skills/legal-writing/references/decision-overviews.md")
    docs = compact("skills/engineering-docs/SKILL.md")
    engineering = compact("skills/engineering-writing/references/architecture-and-presentations.md")

    assert "name information or an action" in natural
    assert "authority, provenance, liability or negotiation meaning" in natural
    assert "Position or options" in legal
    assert "Professional confirmation" in legal
    assert "same audience, owner and lifecycle" in docs
    assert "pointer-only file" in docs
    assert "progressive disclosure" in engineering.lower()
    assert "contributing services or components" in engineering


def test_skill_promotion_uses_structural_form_and_routing_fixture():
    proposal = yaml.safe_load(read(".github/ISSUE_TEMPLATE/skill-proposal.yml"))
    proposal_fields = {
        field["id"]: field for field in proposal["body"] if "id" in field
    }
    cases = yaml.safe_load(read("skills/skill-craft/evals/trigger_cases.yaml"))["cases"]
    promotion = next(case for case in cases if case["id"] == "q708")

    assert promotion["relation"] == "boundary"
    assert promotion["expected"] == {
        "primary_skill": "skill-craft",
        "companion_skills": ["evaluate"],
    }
    status = proposal_fields["promotion-status"]
    rationale = proposal_fields["promotion-rationale"]
    assert status["attributes"]["options"] == [
        "Project-local",
        "Opt-in provisional",
        "Global-ready",
    ]
    assert status["validations"]["required"] is True
    assert rationale["type"] == "textarea"
    assert rationale["validations"]["required"] is True
    assert not (ROOT / "skills/skill-craft/scripts/promotion_readiness.py").exists()


def test_legal_decision_overview_is_one_hop_from_the_skill_front_door():
    legal_skill = read("skills/legal-writing/SKILL.md")
    assert "(references/decision-overviews.md)" in legal_skill


def test_document_profile_classifies_interactive_html_and_its_conditional_evidence():
    registry = json.loads(read("config/delivery-profiles.json"))
    document = registry["profiles"]["document"]
    surfaces = registry["artifact_type_surfaces"]
    deliver = compact("skills/deliver/references/interactive-documents.md")

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
    assert "link-integrity evidence" in deliver
    assert "interaction-smoke evidence" in deliver
