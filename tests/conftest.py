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

import atexit
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DECLARED_IMPORT_ROOTS = (ROOT, ROOT / "scripts", ROOT / "skills")
PROVIDER_EXECUTABLES = (
    "claude", "codex", "herdr", "pi", "agy", "cursor-agent", "copilot", "kiro-cli", "opencode",
)


_SESSION_SCRATCH: list[Path] = []
LIVE_PROVIDER_ENV = "PROVENANT_TEST_ALLOW_LIVE_PROVIDER_CLI"


def _remove_session_scratch() -> None:
    for path in _SESSION_SCRATCH:
        shutil.rmtree(path, ignore_errors=True)


atexit.register(_remove_session_scratch)


def _keep_tmpdir_outside_git() -> None:
    """Relocate TMPDIR when it sits inside a git work tree.

    A dispatched agent's TMPDIR is under the live product checkout's
    `.agent-run`, so every temp workspace would resolve its run root there.
    """
    probe = subprocess.run(["git", "rev-parse", "--git-dir"], cwd=tempfile.gettempdir(),
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if probe.returncode == 0:
        relocated = Path(tempfile.mkdtemp(prefix="provenant-pytest-", dir="/tmp")).resolve()
        os.environ["TMPDIR"] = str(relocated)
        tempfile.tempdir = None
        _SESSION_SCRATCH.append(relocated)


def pytest_configure(config) -> None:
    _keep_tmpdir_outside_git()
    # Before collection: modules cache the product root at import. Per-test
    # monkeypatch overrides still apply on top of this.
    os.environ["AGENT_FABRIC_PRODUCT_ROOT"] = str(ROOT)
    for inherited in ("PROVENANT_SCRIPTS_ROOT", "PROVENANT_SKILLS_ROOT", "PROVENANT_TESTS_ROOT"):
        os.environ.pop(inherited, None)
    resolved = {Path(entry).resolve() for entry in sys.path if entry}
    missing = [str(root) for root in DECLARED_IMPORT_ROOTS if root not in resolved]
    if missing:
        raise RuntimeError(
            "the declared pytest import roots are not on sys.path: "
            f"{missing}. Restore `pythonpath` in pyproject.toml rather than "
            "repairing sys.path inside a test."
        )


def pytest_collection_modifyitems(config, items) -> None:
    """Real-CLI tests run only when opted in; the default PATH holds fail-loud stubs."""
    if os.environ.get(LIVE_PROVIDER_ENV) == "1":
        return
    skip = pytest.mark.skip(reason=f"real provider CLI test; set {LIVE_PROVIDER_ENV}=1 to run")
    for item in items:
        if item.get_closest_marker("live_provider"):
            item.add_marker(skip)


def pytest_unconfigure(config) -> None:
    _remove_session_scratch()


@pytest.fixture(autouse=True)
def _checkout_under_test_is_product_root(monkeypatch):
    """Resolve the product root to this checkout, never an inherited live one."""
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))


@pytest.fixture(autouse=True)
def _isolated_route_state(tmp_path_factory, monkeypatch):
    monkeypatch.setenv("FABRIC_COOLDOWNS_PATH", str(tmp_path_factory.mktemp("cooldowns") / "cooldowns.json"))
    monkeypatch.setenv("AGENT_FABRIC_ROUTE_HEALTH_PATH", str(tmp_path_factory.mktemp("route-health") / "route-health.json"))


@pytest.fixture(autouse=True)
def _isolated_codex_home_and_state(tmp_path_factory, monkeypatch):
    """Codex argv reads the operator's MCP servers and names Provenant's state dir; pin both."""
    monkeypatch.setenv("CODEX_HOME", str(tmp_path_factory.mktemp("codex-home")))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path_factory.mktemp("state-home")))


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
    if (os.environ.get(LIVE_PROVIDER_ENV) == "1"
            or request.node.get_closest_marker("live_provider")):
        return
    monkeypatch.setenv(
        "PATH", f"{_provider_cli_stub_dir}{os.pathsep}{os.environ.get('PATH', '')}"
    )
