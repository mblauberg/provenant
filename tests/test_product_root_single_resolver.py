"""One resolver owns the product root, and every Python caller honours it (#754)."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
RESOLVER = ROOT / "skills" / "_shared" / "roots.py"

def _load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    return module


@pytest.fixture()
def resolver():
    return _load(RESOLVER, "product_roots_under_test")


def test_resolver_prefers_the_configured_root(monkeypatch, resolver, tmp_path):
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(tmp_path))

    assert resolver.product_root() == tmp_path
    assert resolver.skills_root() == tmp_path / "skills"


def test_resolver_treats_an_empty_configured_root_as_unset(monkeypatch, resolver):
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", "")

    assert resolver.product_root() == ROOT


def test_resolver_falls_back_to_its_own_location(monkeypatch, resolver):
    monkeypatch.delenv("AGENT_FABRIC_PRODUCT_ROOT", raising=False)

    assert resolver.product_root() == ROOT


def test_delivery_receipt_honours_the_configured_product_root(monkeypatch, tmp_path):
    """The divergence in #754: this script ignored the variable cf_dispatch sets."""
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(tmp_path))
    monkeypatch.syspath_prepend(str(ROOT / "skills" / "deliver" / "scripts"))

    module = _load(
        ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py",
        "delivery_receipt_root_under_test",
    )

    assert module.RISK_POLICY_PATH == tmp_path / "config" / "risk-policy.json"
    assert module.PROFILE_PATH == tmp_path / "config" / "delivery-profiles.json"
