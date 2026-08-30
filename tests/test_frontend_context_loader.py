import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
LOADER = ROOT / "runtime" / "ui-live" / "load-context.mjs"


def test_context_loader_reads_legacy_file_without_renaming_it(tmp_path):
    legacy = tmp_path / ".impeccable.md"
    legacy.write_text("# Legacy product context\n")

    before = sorted(path.name for path in tmp_path.iterdir())
    result = subprocess.run(
        ["node", str(LOADER)],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    )
    after = sorted(path.name for path in tmp_path.iterdir())
    context = json.loads(result.stdout)

    assert before == after == [".impeccable.md"]
    assert context["hasProduct"] is True
    assert context["productPath"] == ".impeccable.md"
    assert context["productChars"] == len("# Legacy product context\n")
    assert "product" not in context
    assert context["migrated"] is False
    assert not (tmp_path / "PRODUCT.md").exists()


def test_context_loader_default_output_is_bounded_metadata(tmp_path):
    sentinel = "PRIVATE-BODY-" + "x" * 100_000
    (tmp_path / "PRODUCT.md").write_text(f"# Product\n\n{sentinel}\n## Users\n")
    result = subprocess.run(
        ["node", str(LOADER)],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    )
    context = json.loads(result.stdout)
    assert sentinel not in result.stdout
    assert len(result.stdout) < 2_000
    assert context["productChars"] > 100_000
    assert context["productHeadings"] == [
        {"level": 1, "title": "Product"},
        {"level": 2, "title": "Users"},
    ]
    assert context["metadataTruncation"]["productHeadings"] == {
        "total": 2,
        "returned": 2,
        "omitted": 0,
        "titlesTruncated": 0,
    }


def test_context_loader_caps_adversarial_heading_metadata_and_reports_truncation(tmp_path):
    huge_title = "H" * 10_000
    headings = "\n".join(f"## {huge_title}-{index}" for index in range(2_000))
    (tmp_path / "PRODUCT.md").write_text(headings)
    result = subprocess.run(
        ["node", str(LOADER)],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    )
    context = json.loads(result.stdout)
    limits = context["metadataLimits"]
    receipt = context["metadataTruncation"]["productHeadings"]

    assert len(result.stdout) <= limits["maxOutputChars"] + 1  # print newline
    assert len(context["productHeadings"]) <= limits["maxHeadingsPerDocument"]
    assert all(len(item["title"]) <= limits["maxHeadingTitleChars"] for item in context["productHeadings"])
    assert receipt["total"] == 2_000
    assert receipt["returned"] == len(context["productHeadings"])
    assert receipt["omitted"] == 2_000 - receipt["returned"]
    assert receipt["titlesTruncated"] == limits["maxHeadingsPerDocument"]


def test_context_loader_preview_has_an_explicit_character_cap(tmp_path):
    (tmp_path / "PRODUCT.md").write_text("# Product\n" + "y" * 1_000)
    result = subprocess.run(
        ["node", str(LOADER), "--max-chars", "64"],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    )
    context = json.loads(result.stdout)
    assert len(context["product"]) == 64
    assert context["productTruncated"] is True


def test_context_loader_matches_unusual_filename_case(tmp_path):
    mixed = tmp_path / "pRoDuCt.Md"
    mixed.write_text("# Mixed case\n")
    result = subprocess.run(
        ["node", str(LOADER)],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    )
    context = json.loads(result.stdout)
    assert context["hasProduct"] is True
    assert context["productPath"].lower() == "product.md"
