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
import importlib
import os
import shutil
import subprocess
import sys
import tempfile
import zlib
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
    """Keep test scratch in TMPDIR without inheriting the checkout's Git identity.

    A lane's writable TMPDIR lies under `.agent-run`. Without a discovery
    boundary, fixture source files inherit that directory's ignore rule and
    generated-file guards reject them. Moving to /tmp escapes the sandbox.
    """
    probe = subprocess.run(["git", "rev-parse", "--git-dir"], cwd=tempfile.gettempdir(),
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if probe.returncode == 0:
        relocated = Path(tempfile.mkdtemp(prefix="provenant-pytest-")).resolve()
        # An existing invalid gitdir stops discovery at this scratch boundary.
        # Nested fixture repositories can still initialise their own .git.
        (relocated / ".git").write_text("gitdir: .\n", encoding="utf-8")
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
    """Skip live CLIs by default and optionally select a stable file shard."""
    if os.environ.get(LIVE_PROVIDER_ENV) != "1":
        skip = pytest.mark.skip(reason=f"real provider CLI test; set {LIVE_PROVIDER_ENV}=1 to run")
        for item in items:
            if item.get_closest_marker("live_provider"):
                item.add_marker(skip)

    shard = os.environ.get("PROVENANT_TEST_SHARD")
    if not shard:
        return
    try:
        index, count = (int(part) for part in shard.split("/", maxsplit=1))
    except (ValueError, TypeError):
        raise pytest.UsageError("PROVENANT_TEST_SHARD must be formatted as i/n") from None
    if count < 2 or not 1 <= index <= count:
        raise pytest.UsageError("PROVENANT_TEST_SHARD must select 1..n with n at least 2")

    selected = []
    deselected = []
    for item in items:
        path = Path(item.path).resolve().relative_to(ROOT).as_posix()
        if path == "tests/test_fabric_hosts.py":
            keep = index == 1
        else:
            bucket = zlib.crc32(path.encode("utf-8")) % (count - 1) + 2
            keep = index == bucket
        (selected if keep else deselected).append(item)
    items[:] = selected
    config.hook.pytest_deselected(items=deselected)


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
    # A developer's legacy profile must not steer routing tests.
    monkeypatch.setenv("PROVENANT_PROFILE_ROOT", str(tmp_path_factory.mktemp("profile-root")))
    for key in [k for k in os.environ if k.startswith(("PROVENANT_KNOB_", "PROVENANT_PROFILE"))
                and k != "PROVENANT_PROFILE_ROOT"]:
        monkeypatch.delenv(key, raising=False)


@pytest.fixture(autouse=True)
def _isolated_fabric_state(tmp_path_factory, monkeypatch):
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path_factory.mktemp("fabric-state")))


@pytest.fixture(autouse=True)
def _isolated_instance_root(tmp_path_factory, monkeypatch, request):
    """Pin the Fabric instance root so `~/.agents` (hosts.json peers, quota config and
    snapshots) never leaks into a test, and pin the Claude config dir and bin dir so a
    developer's `~/.claude` or `~/.local/bin` never leaks into a subprocess. Tests that
    need a root set their own over this. The installer tests derive those dirs from the
    HOME they build, so they keep their own."""
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(tmp_path_factory.mktemp("instance-root")))
    # A test run inside a remote Fabric lane must still exercise local placement.
    monkeypatch.delenv("PROVENANT_REMOTE_LANE", raising=False)
    # Nor may an enclosing run's lineage or custody leak into a nested-CLI test.
    for key in ("PROVENANT_PARENT_ATTEMPT", "PROVENANT_RUN_TOKEN", "PROVENANT_RUN_OWNER",
                "PROVENANT_NESTED_CUSTODY_FD", "PROVENANT_PARENT_RUN_TOKEN",
                "PROVENANT_RUN_DIR", "PROVENANT_RUN_ID"):
        monkeypatch.delenv(key, raising=False)
    if request.node.path.name == "test_install_harness.py":
        return
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path_factory.mktemp("claude-config")))
    monkeypatch.setenv("PROVENANT_BIN_DIR", str(tmp_path_factory.mktemp("bin-dir")))


@pytest.fixture(autouse=True)
def _isolated_codex_home_and_state(tmp_path_factory, monkeypatch, request):
    """Codex argv reads the operator's MCP servers and names Provenant's state dir; pin both.

    The installer tests build their own HOME and expect CODEX_HOME to follow it."""
    if request.node.path.name == "test_install_harness.py":
        return
    monkeypatch.setenv("CODEX_HOME", str(tmp_path_factory.mktemp("codex-home")))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path_factory.mktemp("state-home")))


@pytest.fixture(autouse=True)
def _codex_mcp_discovery_from_user_config(monkeypatch, request):
    """Tests never run the real `codex mcp list`; read the pinned CODEX_HOME config instead.
    Tests of discovery itself (project layers, failure) patch `_discover_mcp_servers` again."""
    if request.node.path.name == "test_install_harness.py":
        return
    import tomllib

    def discover(cwd):
        path = Path(os.environ["CODEX_HOME"]) / "config.toml"
        try:
            servers = tomllib.loads(path.read_text(encoding="utf-8")).get("mcp_servers", {})
        except (OSError, tomllib.TOMLDecodeError):
            return {}
        return {name: {"exec": "url" not in server or "http_headers_helper" in server, "plugin": False}
                for name, server in servers.items() if isinstance(server, dict)}

    codex = importlib.import_module("skills.orchestrate.scripts.provider_exec").profile("codex")
    if not hasattr(codex, "_real_discover_mcp_servers"):
        codex._real_discover_mcp_servers = codex._discover_mcp_servers
    monkeypatch.setattr(codex, "_discover_mcp_servers", discover)


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
