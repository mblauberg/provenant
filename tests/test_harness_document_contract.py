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


def test_markdown_link_check_preserves_each_space_in_github_anchors(tmp_path):
    target = tmp_path / "target.md"
    source = tmp_path / "source.md"
    target.write_text("# Two  spaces\n")
    source.write_text("[heading](target.md#two--spaces)\n")

    assert _checker().markdown_link_errors([source]) == []


def test_markdown_link_check_ignores_headings_inside_fences(tmp_path):
    target = tmp_path / "target.md"
    source = tmp_path / "source.md"
    target.write_text("```text\n# Not a heading\n```\n")
    source.write_text("[false heading](target.md#not-a-heading)\n")

    assert _checker().markdown_link_errors([source]) == [
        f"{source}: broken link target.md#not-a-heading"
    ]


def test_markdown_link_check_allocates_collision_safe_duplicate_slugs(tmp_path):
    target = tmp_path / "target.md"
    source = tmp_path / "source.md"
    target.write_text("# Foo\n\n# Foo\n\n# Foo-1\n")
    source.write_text("[third](target.md#foo-1-1)\n")

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


def test_issue_form_check_accepts_upload_fields(tmp_path):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        "body:\n"
        "  - type: upload\n"
        "    id: evidence_files\n"
        "    attributes:\n"
        "      label: Evidence files\n"
    )

    assert _checker().issue_form_errors([form]) == []


def test_issue_form_check_rejects_invalid_or_duplicate_ids(tmp_path):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        "body:\n"
        "  - type: input\n"
        "    id: invalid id\n"
        "    attributes:\n"
        "      label: First\n"
        "  - type: textarea\n"
        "    id: invalid id\n"
        "    attributes:\n"
        "      label: Second\n"
    )

    assert _checker().issue_form_errors([form]) == [
        f"{form}: body item 1 has an invalid id",
        f"{form}: body item 2 has an invalid id",
        f"{form}: body item 2 duplicates id invalid id",
    ]


def test_issue_form_check_requires_user_input(tmp_path):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        "body:\n"
        "  - type: markdown\n"
        "    attributes:\n"
        "      value: Context only.\n"
    )

    assert _checker().issue_form_errors([form]) == [
        f"{form}: body must contain at least one input item"
    ]


def test_issue_form_check_rejects_duplicate_options(tmp_path):
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
        "        - high\n"
        "        - high\n"
    )

    assert _checker().issue_form_errors([form]) == [
        f"{form}: dropdown item 1 requires unique options"
    ]


def test_issue_form_check_rejects_unknown_top_level_and_body_keys(tmp_path):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        "unknown: true\n"
        "body:\n"
        "  - type: input\n"
        "    id: outcome\n"
        "    unknown: true\n"
        "    attributes:\n"
        "      label: Outcome\n"
    )

    assert _checker().issue_form_errors([form]) == [
        f"{form}: unsupported top-level keys: unknown",
        f"{form}: body item 1 has unsupported keys: unknown",
    ]
