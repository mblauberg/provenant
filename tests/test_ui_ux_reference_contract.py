from pathlib import Path
import re

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"


def test_every_reference_is_linked_once_from_the_entrypoint():
    references = SKILL / "references"
    assert not (SKILL / "reference").exists()

    entry = (SKILL / "SKILL.md").read_text()
    targets = re.findall(r"\[[^\]]+\]\((references/[^)\s]+)(?:\s+[^)]*)?\)", entry)
    target_names = [Path(target).name for target in targets]
    assert len(target_names) == len(set(target_names))
    assert set(target_names) == {path.name for path in references.glob("*.md")}
    for target in targets:
        assert "/" not in target.removeprefix("references/")
        assert (SKILL / target).is_file()


def test_migration_evidence_has_unique_sources_and_real_destinations():
    ledger_path = SKILL / "evals" / "reference_disposition.yaml"
    assert "Migration evidence" in ledger_path.read_text().splitlines()[0]
    ledger = yaml.safe_load(ledger_path.read_text())
    rows = ledger["references"]
    assert len({row["old"] for row in rows}) == len(rows)
    assert {row["status"] for row in rows} <= {"retained", "folded", "removed"}
    owners = {path.name for path in (SKILL / "references").glob("*.md")}
    for row in rows:
        if row["status"] != "removed":
            assert row["owners"]
            assert set(row["owners"]) <= owners
