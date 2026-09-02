import json
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text()


def route(relative: str, case_id: str) -> dict:
    cases = yaml.safe_load(read(relative))["cases"]
    return next(case["expected"] for case in cases if case["id"] == case_id)


def test_agent_product_evaluation_and_redaction_are_conditional_not_universal():
    profile = json.loads(read("config/delivery-profiles.json"))["profiles"]["agent-product"]

    assert profile["stochastic_policy"]["required"] is False
    assert profile["required_evidence"]["judgement"] == ["agent-product-review"]
    assert profile["evidence_policy"]["redaction"] == "project-policy"
    assert {"tests", "permission-check"} <= set(profile["required_evidence"]["deterministic"])


def test_scope_loads_grill_me_only_when_interactive_stress_testing_is_wanted():
    assert route("skills/scope/evals/trigger_cases.yaml", "q223") == {
        "primary_skill": "scope",
        "companion_skills": ["grill-me"],
    }


def test_release_and_frontend_source_changes_keep_the_current_lifecycle_owner_primary():
    legal_send_route = {
        "primary_skill": "legal-writing",
        "companion_skills": ["release"],
    }
    assert route("skills/release/evals/trigger_cases.yaml", "q207") == legal_send_route
    assert route("skills/legal-writing/evals/trigger_cases.yaml", "q142") == legal_send_route


def test_caveman_is_explicit_instead_of_reloading_the_global_terse_default():
    explicit_fixture = route("skills/caveman/evals/trigger_cases.yaml", "q028")
    generic_fixture = route("skills/caveman/evals/trigger_cases.yaml", "q031")

    assert explicit_fixture == {"primary_skill": "caveman", "companion_skills": []}
    assert generic_fixture == {"primary_skill": None, "companion_skills": []}


def test_skill_craft_declares_action_owner_primary_for_composed_requests():
    # These richer branch-tagged composition cases live in
    # boundary_trace_cases.yaml's routing_reference_cases (not in the
    # strictly schema-validated evals/trigger_cases.yaml; see
    # tests/test_skill_eval_fixtures.py for that contract).
    routing_cases = {
        case["id"]: case
        for case in yaml.safe_load(read("skills/skill-craft/evals/boundary_trace_cases.yaml"))[
            "routing_reference_cases"
        ]
    }
    for case_id, primary in (("sc-007", "implement"), ("sc-008", "evaluate"), ("sc-009", "release")):
        expected = routing_cases[case_id]["expected"]
        assert expected["primary_skill"] == primary
        assert "skill-craft" in expected["companion_skills"]
    sc_008 = routing_cases["sc-008"]
    assert "audit" in sc_008["prompt"].lower()
