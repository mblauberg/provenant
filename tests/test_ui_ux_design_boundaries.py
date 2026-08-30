from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
UI_UX_DESIGN = ROOT / "skills" / "ui-ux-design"
FORBIDDEN_MUTATION_TOOLS = {"Write", "Edit", "NotebookEdit", "ApplyPatch", "Delete"}


def _fixture(name: str) -> dict:
    return yaml.safe_load((UI_UX_DESIGN / "evals" / name).read_text())


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
