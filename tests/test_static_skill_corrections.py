import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_agent_product_evaluation_and_redaction_are_conditional_not_universal():
    profile = json.loads((ROOT / "config/delivery-profiles.json").read_text())["profiles"]["agent-product"]

    assert profile["stochastic_policy"]["required"] is False
    assert profile["required_evidence"]["judgement"] == ["agent-product-review"]
    assert profile["evidence_policy"]["redaction"] == "project-policy"
    assert {"tests", "permission-check"} <= set(profile["required_evidence"]["deterministic"])
