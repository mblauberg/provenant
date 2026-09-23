"""The suite's single import-path contract (issue #755).

Before this, seventeen files repaired `sys.path` themselves so that
`skills/*/scripts/` and `scripts/` could be imported. The roots are now
declared once, in `[tool.pytest.ini_options] pythonpath` in `pyproject.toml`.

This file does not add paths. It fails collection loudly if the declared roots
are missing, so a suite that quietly reverts to per-file path repair is caught
here rather than by a confusing `ModuleNotFoundError` in one test.

It also points Fabric's cooldown store at a per-test file, so no test reads or
writes the host's live cooldowns.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DECLARED_IMPORT_ROOTS = (ROOT, ROOT / "scripts", ROOT / "skills")


def pytest_configure(config) -> None:
    resolved = {Path(entry).resolve() for entry in sys.path if entry}
    missing = [str(root) for root in DECLARED_IMPORT_ROOTS if root not in resolved]
    if missing:
        raise RuntimeError(
            "the declared pytest import roots are not on sys.path: "
            f"{missing}. Restore `pythonpath` in pyproject.toml rather than "
            "repairing sys.path inside a test."
        )


@pytest.fixture(autouse=True)
def _isolated_cooldowns(tmp_path_factory, monkeypatch):
    monkeypatch.setenv("FABRIC_COOLDOWNS_PATH", str(tmp_path_factory.mktemp("cooldowns") / "cooldowns.json"))


@pytest.fixture(autouse=True)
def _unconfined_provider_stubs(monkeypatch, request):
    """Keep OS confinement off for tests that execute stub providers in temp dirs."""
    if (
        request.node.path.name in {"test_dispatch_run.py", "test_batch_run.py"}
        and not request.node.name.startswith("test_provider_does_not_inherit_chair_fabric_environment")
    ):
        monkeypatch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")
