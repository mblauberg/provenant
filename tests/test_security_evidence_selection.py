from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "deliver" / "scripts" / "select_security_evidence.py"


def test_changed_surfaces_select_deterministic_checks_without_model_substitution():
    module = load_script(SCRIPT, "select_security_evidence")
    result = module.select(["source", "dependency", "auth-boundary", "generated-artifact"], ROOT)
    checks = {item["check"] for item in result["checks"]}
    assert {"secrets-scan", "sast", "dependency-advisory", "licence", "auth-boundary-tests", "provenance"} <= checks
    assert all(item["kind"] == "deterministic" for item in result["checks"])


def test_unknown_surface_fails_closed():
    module = load_script(SCRIPT, "select_security_evidence")
    with pytest.raises(module.SelectionError):
        module.select(["mystery-runtime"], ROOT)


def test_agent_product_adds_agentic_risk_catalogue():
    module = load_script(SCRIPT, "select_security_evidence")
    result = module.select(["agent-tools"], ROOT, profile="agent-product")
    assert "tool-misuse" in result["agentic_risks"]
