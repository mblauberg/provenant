import json
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text()


def compact(relative: str) -> str:
    return " ".join(read(relative).split())


def route(relative: str, case_id: str) -> dict:
    cases = yaml.safe_load(read(relative))["cases"]
    return next(case["expected"] for case in cases if case["id"] == case_id)


def test_agent_product_evaluation_and_redaction_are_conditional_not_universal():
    profile = json.loads(read("config/delivery-profiles.json"))["profiles"]["agent-product"]

    assert profile["stochastic_policy"]["required"] is False
    assert profile["required_evidence"]["judgement"] == ["agent-product-review"]
    assert profile["evidence_policy"]["redaction"] == "project-policy"
    assert {"tests", "permission-check"} <= set(profile["required_evidence"]["deterministic"])


def test_implement_receipt_rule_scales_once_with_risk():
    skill = compact("skills/implement/SKILL.md")

    assert "For substantial+ work" in skill
    assert "complete installed-producer command" in skill
    assert "canonical `delivery-run`" in skill
    assert "Routine minor work may proceed without `RUN.json`" in skill
    assert skill.count("Routine minor work may") == 1


def test_orchestrate_names_the_current_ordinary_dispatch_owner():
    skill = compact("skills/orchestrate/SKILL.md")

    assert "ordinary dispatch is owned by `scripts/dispatch_run.py`" in skill
    assert "future ordinary-dispatch owner" not in skill


def test_scope_loads_grill_me_only_when_interactive_stress_testing_is_wanted():
    skill = compact("skills/scope/SKILL.md")

    assert "Load `grill-me` only" in skill
    assert "explicitly asks to be grilled" in skill
    assert "compact decision packet" in skill
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
    assert route("skills/playwright/evals/trigger_cases.yaml", "q167") == {
        "primary_skill": "implement",
        "companion_skills": ["ui-ux-design"],
    }


def test_caveman_is_explicit_instead_of_reloading_the_global_terse_default():
    agents = read("AGENTS.md")
    skill = compact("skills/caveman/SKILL.md")
    explicit_fixture = route("skills/caveman/evals/trigger_cases.yaml", "q028")
    generic_fixture = route("skills/caveman/evals/trigger_cases.yaml", "q031")

    assert "$caveman by default" not in agents
    assert "terse for inter-agent, mechanical, and status traffic" in agents
    assert "Use when the user invokes Caveman" in skill
    assert "governing harness/project instruction enables" not in skill
    assert explicit_fixture == {"primary_skill": "caveman", "companion_skills": []}
    assert generic_fixture == {"primary_skill": None, "companion_skills": []}


def test_session_records_a_friction_pointer_but_retrospect_owns_process_change():
    skill = compact("skills/session/SKILL.md")

    assert "FRICTION_LOG" not in skill
    assert "docs/FRICTION.md" not in skill
    assert "compact friction pointer" in skill
    assert "`retrospect` owns analysis and process changes" in skill
    assert "read-only write authority" not in skill


def test_work_map_links_live_work_state_instead_of_repeating_it():
    skill = compact("skills/work-map/SKILL.md")

    assert "declared workflow-state owner" in skill
    assert "never restates current status, owner, dependencies or user gates" in skill
    assert '${AGENTS_HOME:-$HOME/.agents}/skills/work-map/scripts/validate_work_map.py' in skill
    assert "Stable grouping/order" not in skill
    assert "Stable ordering is allowed" in skill


def test_autopilot_yields_when_bounded_reenumeration_finds_no_work():
    skill = compact("skills/autopilot/SKILL.md")
    reference = compact("skills/autopilot/references/cross-family-review.md")

    assert "one bounded re-enumeration pass" in skill
    assert "idle checkpoint" in skill
    assert "only user STOP closes the mission" in skill
    assert "`orchestrate` owns provider routing" in reference
    assert "reviewer roster" not in reference.lower()


def test_autopilot_claude_stop_hook_uses_the_same_pause_validator():
    recovery = read("skills/autopilot/references/recovery-and-cadence.md")
    template = read("skills/autopilot/templates/README.template.md")

    assert "templates/README.template.md" in recovery
    assert "references/codex-operator.md" in recovery
    for source in (recovery, template):
        assert "validate_idle_pause.py" in source
        assert "--queue" in source
        assert "non-zero" in source


def test_skill_craft_declares_action_owner_primary_for_composed_requests():
    skill = compact("skills/skill-craft/SKILL.md")
    audit = compact("skills/skill-craft/references/audit.md")

    assert "companion to a primary lifecycle owner" in skill
    assert "stays the action-owner" in skill
    assert "plugin packaging uses its platform owner" in skill.lower()
    assert "action-owning lifecycle remains primary" in audit
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
