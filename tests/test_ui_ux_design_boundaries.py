from pathlib import Path
import re

import yaml


ROOT = Path(__file__).resolve().parents[1]
UI_UX_DESIGN = ROOT / "skills" / "ui-ux-design"
FORBIDDEN_MUTATION_TOOLS = {"Write", "Edit", "NotebookEdit", "ApplyPatch", "Delete"}


def _fixture(name: str) -> dict:
    return yaml.safe_load((UI_UX_DESIGN / "evals" / name).read_text())


def _assert_route_fixture_schema(fixture: dict, *, allowed_relations: set[str]) -> None:
    assert fixture["schema_version"] == 1
    assert fixture["target_skill"] == "ui-ux-design"
    ids = []
    for case in fixture["cases"]:
        assert set(case) >= {"id", "relation", "prompt", "tags", "expected"}
        assert case["relation"] in allowed_relations
        assert isinstance(case["prompt"], str) and case["prompt"].strip()
        assert isinstance(case["tags"], list)
        assert set(case["expected"]) >= {"primary_skill", "companion_skills"}
        assert isinstance(case["expected"]["companion_skills"], list)
        ids.append(case["id"])
    assert len(ids) == len(set(ids))


def test_ui_ux_entrypoint_routes_by_natural_intent_without_internal_incantations():
    skill = (UI_UX_DESIGN / "SKILL.md").read_text().lower()
    sidecar = (UI_UX_DESIGN / "agents" / "openai.yaml").read_text().lower()
    assert all(word in skill for word in ("build", "redesign", "fix", "polish", "implement"))
    assert "read-only" in skill and "explicit read-only" in skill
    assert "consequential" in skill and "scope" in skill
    assert "write envelope" not in skill
    assert "write envelope" not in sidecar


def test_always_loaded_description_names_composition_and_adjacent_owners():
    entry = (UI_UX_DESIGN / "SKILL.md").read_text()
    frontmatter = yaml.safe_load(re.split(r"^---\s*$", entry, maxsplit=2, flags=re.MULTILINE)[1])
    description = frontmatter["description"]
    assert len(description) <= 250
    for owner in ("implement", "code-review", "react-performance", "playwright"):
        assert owner in description

    prompt = yaml.safe_load((UI_UX_DESIGN / "agents" / "openai.yaml").read_text())[
        "interface"
    ]["default_prompt"].lower()
    for review_intent in ("review", "audit", "comparison", "advice without change intent"):
        assert review_intent in prompt
    assert "review-and-fix" in prompt
    assert "implement" in prompt


def test_ui_ux_design_has_no_competing_review_or_performance_commands():
    references = UI_UX_DESIGN / "references"
    for retired in ("critique.md", "audit.md", "optimize.md", "heuristics-scoring.md", "personas.md"):
        assert not (references / retired).exists()


def test_review_cases_encode_the_complete_zero_mutation_contract():
    boundary = _fixture("boundary_cases.yaml")
    review_cases = [case for case in boundary["cases"] if case["branch"] == "review"]
    assert review_cases
    for case in review_cases:
        expected = case["expected"]
        assert set(expected["tool_calls_forbidden"]) == FORBIDDEN_MUTATION_TOOLS
        assert expected["shell_mutation_forbidden"] is True
        assert expected["browser_external_write_forbidden"] is True
        assert expected["tree_unchanged"] is True
        assert expected["report_outside_protected_root"] is True
        assert set(expected["browser_read_effects_permitted"]) == {"navigate", "get", "screenshot"}


def test_natural_change_requests_route_to_implementation_without_user_facing_ceremony():
    boundary = _fixture("boundary_cases.yaml")
    cases = {case["id"]: case for case in boundary["cases"]}

    for case_id in ("b004", "b005", "b007", "b009"):
        case = cases[case_id]
        assert case["branch"] == "composition"
        assert case["expected"]["primary_skill"] == "implement"
        assert case["expected"]["companion_skills"] == ["ui-ux-design"]
        assert "envelope" not in case["prompt"].lower()

    assert cases["b002"]["branch"] == "review"
    assert "read-only" in cases["b002"]["prompt"].lower()
    assert cases["b008"]["branch"] == "review"
    assert "live" in cases["b008"]["prompt"].lower()
    assert "read-only" in cases["b008"]["prompt"].lower()
    assert "live setup" in cases["b008"]["expected"]["behaviour"].lower()
    assert cases["b006"]["branch"] == "scope"
    assert cases["b006"]["expected"]["primary_skill"] == "scope"


def test_trigger_and_regression_routes_are_both_active_structural_contracts():
    trigger = _fixture("trigger_cases.yaml")
    regression = _fixture("regression_cases.yaml")
    _assert_route_fixture_schema(trigger, allowed_relations={"positive", "negative", "boundary"})
    _assert_route_fixture_schema(regression, allowed_relations={"positive", "negative", "boundary"})

    combined_ids = [case["id"] for fixture in (trigger, regression) for case in fixture["cases"]]
    assert len(combined_ids) == len(set(combined_ids))
    assert any(
        case["expected"]["primary_skill"] == "implement"
        and "ui-ux-design" in case["expected"]["companion_skills"]
        for case in trigger["cases"]
    )

    regressions = {case["id"]: case for case in regression["cases"]}
    assert regressions["r004"]["expected"] == {
        "primary_skill": "implement",
        "companion_skills": ["ui-ux-design"],
    }
    assert regressions["r005"]["expected"]["primary_skill"] == "ui-ux-design"
    assert regressions["r006"]["expected"] == {
        "primary_skill": "implement",
        "companion_skills": ["ui-ux-design"],
    }
    assert regressions["r007"]["expected"] == {
        "primary_skill": "ui-ux-design",
        "companion_skills": [],
    }
    assert "read-only" in regressions["r007"]["prompt"].lower()
    assert "live" in regressions["r007"]["prompt"].lower()


def test_review_branch_has_no_legacy_write_or_cleanup_surface():
    for retired in (
        "pin.mjs",
        "command-metadata.json",
        "critique-storage.mjs",
        "cleanup-deprecated.mjs",
    ):
        assert not (UI_UX_DESIGN / "scripts" / retired).exists()

    paths = (UI_UX_DESIGN / "scripts" / "impeccable-paths.mjs").read_text()
    assert "CRITIQUE_DIR" not in paths
    assert "getCritiqueDir" not in paths
