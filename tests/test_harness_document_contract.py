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
