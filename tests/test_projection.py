"""Architecture risk-table and escalation policy assertions (CI only)."""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_architecture_risk_projection_matches_the_runtime_policy():
    architecture = (ROOT / "docs" / "ARCHITECTURE.md").read_text()
    policy = json.loads((ROOT / "config" / "risk-policy.json").read_text())
    start = "<!-- risk-factor-table:start -->"
    end = "<!-- risk-factor-table:end -->"
    assert architecture.count(start) == architecture.count(end) == 1
    assert architecture.index(start) < architecture.index(end)
    region = architecture.split(start)[1].split(end)[0]
    rows = {}
    table_lines = [line for line in region.splitlines() if line.startswith("|")]
    header = [cell.strip().replace("`", "") for cell in table_lines[0].strip("|").split("|")]
    tiers = policy["tier_order"]
    assert header == ["Factor", *tiers]
    for line in table_lines[2:]:
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        assert len(cells) == len(header)
        factor = cells[0].replace(" ", "_")
        assert factor not in rows
        rows[factor] = cells[1:]

    assert set(rows) == set(policy["factors"])
    for factor, mapping in policy["factors"].items():
        expected = [
            {value for value, tier in mapping.items() if tier == level}
            for level in tiers
        ]
        actual = [
            {value.strip() for value in cell.replace("`", "").split(",") if value.strip()}
            for cell in rows[factor]
        ]
        assert actual == expected


def _minimum_tier(policy, **overrides):
    # Lowest-risk rating for every factor, then apply the given ratings.
    ratings = {factor: next(iter(mapping)) for factor, mapping in policy["factors"].items()}
    ratings.update(overrides)
    tiers = policy["tier_order"]
    return max(
        (policy["factors"][factor][value] for factor, value in ratings.items()),
        key=tiers.index,
    )


def test_internal_data_alone_stays_routine_but_consequences_escalate():
    policy = json.loads((ROOT / "config" / "risk-policy.json").read_text())
    assert _minimum_tier(policy, data_sensitivity="internal") == "routine"
    assert _minimum_tier(
        policy, data_sensitivity="internal", reversibility="irreversible"
    ) == "terminal"
    assert _minimum_tier(policy, data_sensitivity="confidential") == "crucial"
