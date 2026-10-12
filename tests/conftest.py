"""The suite's single import-path contract (issue #755).

Before this, seventeen files repaired `sys.path` themselves so that
`skills/*/scripts/` and `scripts/` could be imported. The roots are now
declared once, in `[tool.pytest.ini_options] pythonpath` in `pyproject.toml`.

This file does not add paths. It fails collection loudly if the declared roots
are missing, so a suite that quietly reverts to per-file path repair is caught
here rather than by a confusing `ModuleNotFoundError` in one test.

It also points Fabric's cooldown and route health stores at per-test files, so
no test reads or writes the host's live route state.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DECLARED_IMPORT_ROOTS = (ROOT, ROOT / "scripts", ROOT / "skills")
PROVIDER_EXECUTABLES = (
    "claude", "codex", "herdr", "pi", "agy", "cursor-agent", "copilot", "kiro-cli", "opencode",
)


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
def _isolated_route_state(tmp_path_factory, monkeypatch):
    monkeypatch.setenv("FABRIC_COOLDOWNS_PATH", str(tmp_path_factory.mktemp("cooldowns") / "cooldowns.json"))
    monkeypatch.setenv("AGENT_FABRIC_ROUTE_HEALTH_PATH", str(tmp_path_factory.mktemp("route-health") / "route-health.json"))


@pytest.fixture(autouse=True)
def _unconfined_provider_stubs(monkeypatch, request):
    """Keep OS confinement off for tests that execute stub providers in temp dirs."""
    if (
        request.node.path.name in {"test_dispatch_run.py", "test_batch_run.py"}
        and not request.node.name.startswith("test_provider_does_not_inherit_chair_fabric_environment")
    ):
        monkeypatch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")


@pytest.fixture(scope="session")
def _provider_cli_stub_dir(tmp_path_factory):
    """Create fail-loud provider executables once for the pytest session."""
    bin_dir = tmp_path_factory.mktemp("provider-cli-stubs")
    stub = '#!/bin/sh\necho "blocked real provider CLI in the default pytest suite: $0" >&2\nexit 97\n'
    for executable in PROVIDER_EXECUTABLES:
        path = bin_dir / executable
        path.write_text(stub, encoding="utf-8")
        path.chmod(0o755)
    return bin_dir


@pytest.fixture(autouse=True)
def _block_live_provider_clis(_provider_cli_stub_dir, monkeypatch, request):
    """Shadow installed provider CLIs unless this test explicitly opts into live use."""
    if (os.environ.get("PROVENANT_TEST_ALLOW_LIVE_PROVIDER_CLI") == "1"
            or request.node.get_closest_marker("live_provider")):
        return
    monkeypatch.setenv(
        "PATH", f"{_provider_cli_stub_dir}{os.pathsep}{os.environ.get('PATH', '')}"
    )
