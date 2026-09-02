"""One resolver owns the product root, and every Python caller honours it (#754)."""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
RESOLVER = ROOT / "skills" / "_shared" / "roots.py"

# Scripts that read product-rooted configuration. Each must resolve the root
# through `scripts.lib.roots.product_root`, not by counting `..` itself.
CONSUMERS = (
    "skills/deliver/scripts/delivery_receipt.py",
    "skills/deliver/scripts/delivery_validation_common.py",
    "skills/deliver/scripts/software_delivery_validation.py",
    "skills/deliver/scripts/select_security_evidence.py",
    "skills/deliver/scripts/reference_runs.py",
    "skills/implement/scripts/bind_merged_delivery.py",
    "skills/session/scripts/cleanup_run_artifacts.py",
    "scripts/model_route.py",
)


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


@pytest.mark.parametrize("relative", CONSUMERS)
def test_consumers_do_not_re_derive_the_product_root(relative):
    source = (ROOT / relative).read_text()

    assert "roots import product_root" in source or "roots.product_root" in source, (
        f"{relative} must resolve the product root through the one resolver"
    )
    offenders = [
        line
        for line in source.splitlines()
        if re.search(r"AGENT_FABRIC_PRODUCT_ROOT|parents\[3\]", line)
        and "roots.py" not in line
    ]
    assert offenders == [], f"{relative} re-derives the product root inline: {offenders}"


def test_no_module_level_product_root_global_is_mutated():
    offenders = []
    for path in sorted(ROOT.glob("skills/*/scripts/*.py")) + sorted(ROOT.glob("scripts/*.py")):
        if re.search(r"^\s*global\s+PRODUCT_ROOT\b", path.read_text(), re.MULTILINE):
            offenders.append(str(path.relative_to(ROOT)))
    assert offenders == []
