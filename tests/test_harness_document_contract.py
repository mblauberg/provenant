import importlib.util
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]


def _checker():
    path = ROOT / "scripts" / "check_harness.py"
    spec = importlib.util.spec_from_file_location("check_harness_docs", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize(
    ("target_content", "source_content", "has_errors"),
    (
        ("# Present heading\n", "[missing](target.md#missing-heading)\n", True),
        (None, "# Present heading\n\n[missing](#missing-heading)\n", True),
        ("# Repeated\n\n# Repeated\n", "[second](target.md#repeated-1)\n", False),
        ("# Two  spaces\n", "[heading](target.md#two--spaces)\n", False),
        ("```text\n# Not a heading\n```\n", "[false heading](target.md#not-a-heading)\n", True),
        ("# Foo\n\n# Foo\n\n# Foo-1\n", "[third](target.md#foo-1-1)\n", False),
        ("# A _helpful_ section\n", "[heading](target.md#a-helpful-section)\n", False),
        ("# The `__init__` hook\n", "[heading](target.md#the-__init__-hook)\n", False),
        ("# An _x_ value\n", "[heading](target.md#an-x-value)\n", False),
        ("# A snake_case value\n", "[heading](target.md#a-snake_case-value)\n", False),
    ),
    ids=(
        "missing-fragment", "missing-same-page-fragment", "duplicate-heading-suffix",
        "spaces-in-anchors", "ignore-fenced-headings", "collision-safe-slugs",
        "underscore-emphasis", "code-span-underscores", "single-character-emphasis",
        "intraword-underscores",
    ),
)
def test_markdown_link_checker_rules(tmp_path, target_content, source_content, has_errors):
    target = tmp_path / "target.md"
    source = tmp_path / "source.md"
    if target_content is not None:
        target.write_text(target_content)
    source.write_text(source_content)

    errors = _checker().markdown_link_errors([source])

    assert bool(errors) is has_errors


@pytest.mark.parametrize(
    ("top_level", "body", "has_errors"),
    (
        ("", "  - type: textarea\n    id: outcome\n", True),
        (
            "",
            "  - type: dropdown\n    id: priority\n    attributes:\n"
            "      label: Priority\n      options:\n        - {}\n",
            True,
        ),
        (
            "",
            "  - type: checkboxes\n    id: checks\n    attributes:\n"
            "      label: Checks\n      options:\n        - required: true\n",
            True,
        ),
        (
            "",
            "  - type: upload\n    id: evidence_files\n    attributes:\n"
            "      label: Evidence files\n",
            False,
        ),
        (
            "",
            "  - type: input\n    id: invalid id\n    attributes:\n"
            "      label: First\n  - type: textarea\n    id: invalid id\n"
            "    attributes:\n      label: Second\n",
            True,
        ),
        ("", "  - type: markdown\n    attributes:\n      value: Context only.\n", True),
        (
            "",
            "  - type: dropdown\n    id: priority\n    attributes:\n"
            "      label: Priority\n      options:\n        - high\n        - high\n",
            True,
        ),
        (
            "unknown: true\n",
            "  - type: input\n    id: outcome\n    unknown: true\n"
            "    attributes:\n      label: Outcome\n",
            True,
        ),
        (
            "",
            "  - type: markdown\n    id: context\n    attributes:\n"
            "      value: Context.\n  - type: input\n    id: outcome\n"
            "    attributes:\n      label: Outcome\n",
            True,
        ),
        (
            "",
            "  - type: input\n    id: outcome\n    attributes:\n"
            "      label: Outcome\n    validations:\n      required: nope\n",
            True,
        ),
    ),
    ids=(
        "body-item-needs-attributes", "dropdown-option-shape", "checkbox-option-label",
        "upload-fields-accepted", "invalid-or-duplicate-ids", "requires-user-input",
        "duplicate-options", "unknown-keys", "markdown-cannot-have-id",
        "required-is-boolean",
    ),
)
def test_issue_form_checker_rules(tmp_path, top_level, body, has_errors):
    form = tmp_path / "work-item.yml"
    form.write_text(
        "name: Work item\n"
        "description: A bounded change.\n"
        + top_level
        + "body:\n"
        + body
    )

    errors = _checker().issue_form_errors([form])

    assert bool(errors) is has_errors
