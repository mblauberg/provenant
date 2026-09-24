from pathlib import Path
import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"


def test_migration_evidence_has_unique_sources_and_real_destinations():
    ledger_path = SKILL / "evals" / "reference_disposition.yaml"
    ledger = yaml.safe_load(ledger_path.read_text())
    rows = ledger["references"]
    assert len({row["old"] for row in rows}) == len(rows)
    assert {row["status"] for row in rows} <= {"retained", "folded", "removed"}
    owners = {path.name for path in (SKILL / "references").glob("*.md")}
    for row in rows:
        if row["status"] != "removed":
            assert row["owners"]
            assert set(row["owners"]) <= owners
