import ast
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"
VALIDATOR_MODULES = tuple(sorted(VALIDATOR.parent.glob("*.py")))


SUBMODULES = tuple(m for m in VALIDATOR_MODULES if m != VALIDATOR)


@pytest.mark.parametrize("module", SUBMODULES)
def test_submodules_take_shared_imports_through_the_common_module(module: Path) -> None:
    """`delivery_validation_common` owns the load of `_shared`. A submodule
    importing `_shared` directly only works when some sibling has already
    established it, so the import order becomes load-bearing and silent."""
    tree = ast.parse(module.read_text())
    if module.name == "delivery_validation_common.py":
        return
    offending = [
        node.module for node in ast.walk(tree)
        if isinstance(node, ast.ImportFrom)
        and node.module is not None
        and node.module == "_shared"
    ]
    assert not offending, (
        f"{module.name} imports _shared directly: {offending}. "
        "Re-export it through delivery_validation_common instead."
    )
