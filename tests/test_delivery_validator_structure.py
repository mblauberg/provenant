from pathlib import Path
import re

import pytest


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"
VALIDATOR_MODULES = tuple(sorted(VALIDATOR.parent.glob("*.py")))


def test_delivery_validator_coordinator_stays_within_review_cap() -> None:
    assert len(VALIDATOR.read_text().splitlines()) <= 1_000


@pytest.mark.parametrize("validator", VALIDATOR_MODULES)
def test_each_delivery_validator_module_stays_within_review_cap(validator: Path) -> None:
    assert len(validator.read_text().splitlines()) <= 1_000


SUBMODULES = tuple(m for m in VALIDATOR_MODULES if m != VALIDATOR)


def test_every_validator_module_on_disk_is_capped() -> None:
    """The cap list must not drift from what is actually on disk. A hardcoded
    tuple lets a ninth module ship uncapped while the suite reports green."""
    on_disk = set(VALIDATOR.parent.glob("delivery_validation_*.py"))
    assert on_disk <= set(VALIDATOR_MODULES), (
        "modules on disk but absent from the cap list: "
        f"{sorted(p.name for p in on_disk - set(VALIDATOR_MODULES))}"
    )


@pytest.mark.parametrize("module", SUBMODULES)
def test_submodules_take_shared_imports_through_the_common_module(module: Path) -> None:
    """`delivery_validation_common` owns the sys.path repair that makes `_shared`
    importable. A submodule importing `_shared` directly only works when some
    sibling has already run, so the import order becomes load-bearing and silent."""
    source = module.read_text()
    if module.name == "delivery_validation_common.py":
        return
    offending = re.findall(r"^from _shared[.\w]* import .*$", source, flags=re.MULTILINE)
    assert not offending, (
        f"{module.name} imports _shared directly: {offending}. "
        "Re-export it through delivery_validation_common instead."
    )
