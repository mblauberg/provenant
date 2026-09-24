import importlib.util
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "deliver" / "scripts" / "select_security_evidence.py"


def load_module():
    spec = importlib.util.spec_from_file_location("select_security_evidence", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def test_changed_surfaces_select_deterministic_checks_without_model_substitution():
    module = load_module()
    result = module.select(["source", "dependency", "auth-boundary", "generated-artifact"], ROOT)
    checks = {item["check"] for item in result["checks"]}
    assert {"secrets-scan", "sast", "dependency-advisory", "licence", "auth-boundary-tests", "provenance"} <= checks
    assert all(item["kind"] == "deterministic" for item in result["checks"])


def test_unknown_surface_fails_closed():
    module = load_module()
    with pytest.raises(module.SelectionError):
        module.select(["mystery-runtime"], ROOT)


def test_agent_product_adds_agentic_risk_catalogue():
    module = load_module()
    result = module.select(["agent-tools"], ROOT, profile="agent-product")
    assert "tool-misuse" in result["agentic_risks"]
