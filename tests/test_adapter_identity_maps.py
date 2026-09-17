"""Adapter identity maps must not drift from the product dispatch registry.

cf_dispatch.sh derives `endpoint_provider` when a route does not name one, and
review_terminal.py re-derives the same facts when it decides whether a review
leg proves cross-family lineage. Two independent maps for one identity fact
silently disagree when an adapter is added in one place only (#827, review
finding 4): a missing agy/opencode entry in the Python copy would make every
agy or opencode review leg fail with "endpoint/provider identity mismatch",
and silently adding one would widen assurance eligibility without a decision.

The registry in config/adapter-compatibility.yaml is the product-owned source;
these tests fail when any mirror disagrees with it.
"""

from pathlib import Path

import pytest
import yaml

from skills._shared import review_terminal

ROOT = Path(__file__).resolve().parents[1]


def _registry() -> dict:
    data = yaml.safe_load((ROOT / "config" / "adapter-compatibility.yaml").read_text())
    return data["dispatch_registry"]


def test_endpoint_provider_map_covers_every_implemented_adapter():
    implemented = {
        name for name, entry in _registry().items()
        if entry["dispatch"] == "implemented"
    }
    missing = sorted(implemented - set(review_terminal._ENDPOINT_PROVIDERS))
    assert not missing, (
        f"adapters missing from review_terminal._ENDPOINT_PROVIDERS: {missing}; "
        "a missing entry makes every review leg for that adapter fail closed"
    )


def test_fixed_family_maps_mirror_the_catalogue_fixed_model_family():
    import json
    routing = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalogue_fixed = {
        name: entry["fixed_model_family"]
        for name, entry in routing["adapters"].items()
        if entry.get("fixed_model_family")
    }
    assert review_terminal._FIXED_MODEL_FAMILIES == catalogue_fixed
    assert review_terminal._FIXED_PROVIDER_FAMILIES == catalogue_fixed


@pytest.mark.parametrize("adapter,expected", sorted(
    (name, entry["endpoint_provider"])
    for name, entry in yaml.safe_load(
        (ROOT / "config" / "model-routing.json").read_text(),
    )["adapters"].items()
    if entry.get("endpoint_provider")
))
def test_shell_endpoint_provider_map_matches_the_catalogue(adapter, expected):
    source = (ROOT / "skills" / "orchestrate" / "scripts" / "cf_dispatch.sh").read_text()
    import re
    match = re.search(
        r"^endpoint_provider\(\) \{\n(.*?)\n\}", source, re.MULTILINE | re.DOTALL,
    )
    assert match, "cf_dispatch.sh has no endpoint_provider() map"
    arm = re.search(rf"^\s+{re.escape(adapter)}\) echo \"([^\"]+)\";;$", match.group(1), re.MULTILINE)
    assert arm, f"cf_dispatch.sh endpoint_provider() has no {adapter} arm"
    assert arm.group(1) == expected
