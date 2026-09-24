from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
UI_UX_DESIGN = ROOT / "skills" / "ui-ux-design"
FORBIDDEN_MUTATION_TOOLS = {"Write", "Edit", "NotebookEdit", "ApplyPatch", "Delete"}


def _fixture(name: str) -> dict:
    return yaml.safe_load((UI_UX_DESIGN / "evals" / name).read_text())


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


def test_implementation_requests_keep_implement_as_owner_with_ui_companion():
    cases = {case["id"]: case for case in _fixture("trigger_cases.yaml")["cases"]}
    assert cases["q716"]["expected"] == {
        "primary_skill": "implement",
        "companion_skills": ["ui-ux-design"],
    }
    compositions = [
        case for case in _fixture("boundary_cases.yaml")["cases"]
        if case["branch"] == "composition"
    ]
    assert compositions
    for case in compositions:
        assert case["expected"]["primary_skill"] == "implement"
        assert case["expected"]["companion_skills"] == ["ui-ux-design"]
