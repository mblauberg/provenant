from pathlib import Path
import re

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"
OWNERS = {
    "review.md",
    "design.md",
    "reference-grounding.md",
    "surfaces.md",
    "visual-system.md",
    "design-systems.md",
    "interaction-states.md",
    "responsive-accessibility.md",
    "motion.md",
    "content-conversion.md",
    "visual-qa.md",
    "live.md",
}
OLD = {
    "adapt.md", "animate.md", "bolder.md", "brand.md", "clarify.md", "codex.md",
    "cognitive-load.md", "color-and-contrast.md", "colorize.md", "command-routing.md",
    "core-laws.md", "craft.md", "delight.md", "distill.md", "document.md", "extract.md",
    "harden.md", "interaction-design.md", "layout.md", "live.md", "motion-design.md",
    "onboard.md", "overdrive.md", "polish.md", "product.md", "quieter.md",
    "responsive-design.md", "review.md", "setup.md", "shape.md", "spatial-design.md",
    "teach.md", "typeset.md", "typography.md", "ux-writing.md",
}


def test_reference_inventory_and_one_hop_links_are_exact():
    references = SKILL / "references"
    assert {path.name for path in references.glob("*.md")} == OWNERS
    assert not (SKILL / "reference").exists()

    entry = (SKILL / "SKILL.md").read_text()
    for owner in OWNERS:
        assert f"(references/{owner})" in entry
    targets = re.findall(r"\[[^\]]+\]\((references/[^)\s]+)(?:\s+[^)]*)?\)", entry)
    assert {Path(target).name for target in targets} == OWNERS
    for target in targets:
        assert "/" not in target.removeprefix("references/")
        assert (SKILL / target).is_file()


def test_migration_evidence_gives_every_old_reference_one_disposition():
    ledger_path = SKILL / "evals" / "reference_disposition.yaml"
    assert "Migration evidence" in ledger_path.read_text().splitlines()[0]
    ledger = yaml.safe_load(ledger_path.read_text())
    rows = ledger["references"]
    assert len(rows) == len(OLD)
    assert {row["old"] for row in rows} == OLD
    assert len({row["old"] for row in rows}) == len(rows)
    assert {row["status"] for row in rows} <= {"retained", "folded", "removed"}
    for row in rows:
        if row["status"] != "removed":
            assert set(row["owners"]) <= OWNERS
