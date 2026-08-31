import importlib.util
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def _checker():
    path = ROOT / "scripts" / "check_harness.py"
    spec = importlib.util.spec_from_file_location("check_harness_docs", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_markdown_link_check_rejects_a_missing_fragment(tmp_path):
    target = tmp_path / "target.md"
    source = tmp_path / "source.md"
    target.write_text("# Present heading\n")
    source.write_text("[missing](target.md#missing-heading)\n")

    errors = _checker().markdown_link_errors([source])

    assert errors == [f"{source}: broken link target.md#missing-heading"]


def test_markdown_link_check_rejects_a_missing_same_page_fragment(tmp_path):
    source = tmp_path / "source.md"
    source.write_text("# Present heading\n\n[missing](#missing-heading)\n")

    errors = _checker().markdown_link_errors([source])

    assert errors == [f"{source}: broken link #missing-heading"]


def test_markdown_link_check_accepts_a_duplicate_heading_suffix(tmp_path):
    target = tmp_path / "target.md"
    source = tmp_path / "source.md"
    target.write_text("# Repeated\n\n# Repeated\n")
    source.write_text("[second](target.md#repeated-1)\n")

    assert _checker().markdown_link_errors([source]) == []


def test_issue_form_check_rejects_a_body_item_without_attributes(tmp_path):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        "body:\n"
        "  - type: textarea\n"
        "    id: outcome\n"
    )

    errors = _checker().issue_form_errors([form])

    assert errors == [f"{form}: body item 1 requires attributes"]


def test_issue_form_check_rejects_malformed_dropdown_options(tmp_path):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        "body:\n"
        "  - type: dropdown\n"
        "    id: priority\n"
        "    attributes:\n"
        "      label: Priority\n"
        "      options:\n"
        "        - {}\n"
    )

    errors = _checker().issue_form_errors([form])

    assert errors == [f"{form}: dropdown item 1 requires non-empty string options"]


def test_issue_form_check_rejects_checkbox_options_without_labels(tmp_path):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        "body:\n"
        "  - type: checkboxes\n"
        "    id: checks\n"
        "    attributes:\n"
        "      label: Checks\n"
        "      options:\n"
        "        - required: true\n"
    )

    errors = _checker().issue_form_errors([form])

    assert errors == [f"{form}: checkbox item 1 requires options with labels"]
