import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/check-provenant-install.py"


def installed_identity() -> bytes:
    return f"{MARKER}\nprovenant-front-door source-sha256={current_digest(ROOT)}\n".encode()


def run_check(tmp_path: Path, *arguments: str, **extra_env: str) -> subprocess.CompletedProcess[str]:
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True, exist_ok=True)
    pointer.write_text(
        f'{{"schema_version": 1, "product_root": "{ROOT}"}}\n'
    )
    return subprocess.run(
        [str(SCRIPT), *arguments],
        env={
            **os.environ,
            "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
            "PROVENANT_BIN_DIR": str(tmp_path / "bin"),
            "HOME": str(tmp_path / "home"),
            **extra_env,
        },
        text=True,
        capture_output=True,
        check=False,
    )


def provider_state(result: subprocess.CompletedProcess[str], provider: str) -> dict[str, str]:
    line = next(line for line in result.stdout.splitlines() if line.startswith(f"provider {provider} "))
    return {
        key: value
        for key, value in (field.split("=", 1) for field in line.split()[2:] if "=" in field)
        if key in {"present", "skills", "mcp"}
    }


def test_check_names_routing_drift_and_refresh_repair(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.write_bytes(installed_identity())
    command.chmod(0o755)
    catalogue = tmp_path / "instance/config/model-routing.json"
    catalogue.parent.mkdir(parents=True)
    product = json.loads((ROOT / "config/model-routing.json").read_text())
    product["adapters"]["opencode"]["endpoint_provider"] = "codex"
    catalogue.write_text(json.dumps(product))

    result = run_check(tmp_path)

    assert result.returncode == 0
    assert "routing drift=adapters.opencode.endpoint_provider" in result.stderr
    assert run_check(tmp_path, "--strict").returncode == 1


def test_check_names_repair_for_present_provider_without_install(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.write_bytes(installed_identity())
    command.chmod(0o755)
    (tmp_path / "home/.config/opencode").mkdir(parents=True)

    result = run_check(tmp_path)

    assert result.returncode == 0
    assert provider_state(result, "opencode") == {
        "present": "yes", "skills": "missing", "mcp": "missing",
    }
    assert run_check(tmp_path, "--strict").returncode == 1


def test_check_reports_invalid_provider_config_without_a_traceback(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.write_bytes(installed_identity())
    command.chmod(0o755)
    config = tmp_path / "home/.config/opencode/opencode.jsonc"
    config.parent.mkdir(parents=True)
    config.write_text("[]")

    result = run_check(tmp_path)

    assert result.returncode == 0
    assert provider_state(result, "opencode") == {
        "present": "yes", "skills": "missing", "mcp": "missing",
    }
    assert "Traceback" not in result.stderr


def test_check_accepts_strict_json_opencode_registration(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.write_bytes(installed_identity())
    command.chmod(0o755)
    root = tmp_path / "home/.config/opencode"
    skill = root / "skills/orchestrate/SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("# Test\n")
    (root / "opencode.jsonc").write_text('{"mcp": {"fabric": {}}}')

    result = run_check(tmp_path)

    assert result.returncode == 0, result.stderr
    assert provider_state(result, "opencode") == {
        "present": "yes", "skills": "ok", "mcp": "ok",
    }


def test_check_does_not_detect_agy_from_gemini_directory_alone(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.write_bytes(installed_identity())
    command.chmod(0o755)
    (tmp_path / "home/.gemini").mkdir(parents=True)

    result = run_check(tmp_path, PATH="/opt/homebrew/bin:/usr/bin:/bin")

    assert result.returncode == 0, result.stderr
    assert provider_state(result, "agy") == {"present": "no"}


def test_check_uses_the_installer_mcp_config_override(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.write_bytes(installed_identity())
    command.chmod(0o755)
    skill = tmp_path / "home/.cursor/skills/orchestrate/SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("# Test\n")
    config = tmp_path / "custom-cursor.json"
    config.write_text('{"mcpServers": {"fabric": {}}}')

    result = run_check(tmp_path, CURSOR_MCP_CONFIG=str(config))

    assert result.returncode == 0, result.stderr
    assert provider_state(result, "cursor") == {
        "present": "yes", "skills": "ok", "mcp": "ok",
    }


def test_check_rejects_a_symlink_instead_of_a_managed_copy(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.symlink_to(SCRIPT)

    result = run_check(tmp_path)

    assert result.returncode == 1
    assert "regular managed copy" in result.stderr


def test_check_rejects_a_stale_pointer_instead_of_abstaining(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    pointer.write_text(
        f'{{"schema_version": 1, "product_root": "{tmp_path / "moved"}"}}\n'
    )

    result = subprocess.run(
        [str(SCRIPT)],
        env={**os.environ, "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root)},
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 1
    assert "product-root pointer is invalid or stale" in result.stderr


def test_check_treats_empty_root_variables_as_unset(tmp_path: Path) -> None:
    home = tmp_path / "home"
    instance_root = home / ".agents"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    pointer.write_text(f'{{"schema_version": 1, "product_root": "{ROOT}"}}\n')
    command = home / ".local/bin/provenant"
    command.parent.mkdir(parents=True)
    command.write_bytes(installed_identity())
    command.chmod(0o755)

    result = subprocess.run(
        [str(SCRIPT)],
        env={
            **os.environ,
            "HOME": str(home),
            "AGENT_FABRIC_INSTANCE_ROOT": "",
            "PROVENANT_BIN_DIR": "",
        },
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert "provenant installed stub=ok" in result.stdout


def test_registration_diagnostics_do_not_require_an_adapter_on_path(tmp_path, monkeypatch):
    monkeypatch.syspath_prepend(str(ROOT / 'scripts'))
    module = load_script(SCRIPT, 'install_check_hosts_test')
    home = tmp_path / 'home'
    registry = home / '.gemini/config/mcp_config.json'
    registry.parent.mkdir(parents=True)
    registry.write_text(json.dumps({'mcpServers': {'fabric': {'command': 'provenant'}}}))
    monkeypatch.setenv('PATH', '/usr/bin:/bin')
    monkeypatch.delenv('AGY_CONFIG_DIR', raising=False)
    monkeypatch.delenv('AGY_MCP_CONFIG', raising=False)
    assert module.provider_registration_status(home)['agy']['registered'] is True
    # The pre-existing install check still reports provider availability as before.
    lines, _ = module._provider_lines(home)
    assert 'provider agy present=no' in lines


# --- Rust front door acceptance (ADR 0027) ----------------------------------

MARKER = "# Managed by scripts/install-harness; local edits to an installed copy are not preserved."


@pytest.fixture(scope="module")
def crate_product(tmp_path_factory) -> Path:
    """A copy of the product with a crate in it, so source digests can be computed."""
    product = tmp_path_factory.mktemp("product") / "product"
    shutil.copytree(
        ROOT,
        product,
        symlinks=True,
        ignore=shutil.ignore_patterns(".git", ".agent-run", "node_modules", ".venv", "target"),
    )
    crate = product / "tools/front-door"
    shutil.rmtree(crate, ignore_errors=True)
    (crate / "src").mkdir(parents=True)
    (crate / "Cargo.toml").write_text(
        '[workspace]\nmembers = [".", "crates/*"]\n[package]\nname = "provenant"\n'
    )
    (crate / "src/main.rs").write_text("fn main() {}\n")
    (crate / "crates/core/src").mkdir(parents=True)
    (crate / "crates/core/Cargo.toml").write_text(
        '[package]\nname = "provenant-core"\n'
        'build = "scripts/build.rs"\n'
        '[lib]\npath = "library.rs"\n'
    )
    (crate / "crates/core/scripts").mkdir()
    (crate / "crates/core/scripts/build.rs").write_text("fn main() {}\n")
    (crate / "crates/core/library.rs").write_text("// library\n")
    return product


def run_check_in(product: Path, tmp_path: Path) -> subprocess.CompletedProcess[str]:
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True, exist_ok=True)
    pointer.write_text(f'{{"schema_version": 1, "product_root": "{product}"}}\n')
    return subprocess.run(
        [str(product / "scripts/check-provenant-install.py")],
        env={
            **os.environ,
            "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
            "PROVENANT_BIN_DIR": str(tmp_path / "bin"),
            "HOME": str(tmp_path / "home"),
            "PROVENANT_STALE_CHECKED": "1",
        },
        text=True,
        capture_output=True,
        check=False,
    )


def install_binary(tmp_path: Path, identity: str | None) -> Path:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    lines = ["#!/bin/sh", "exit 0", "", MARKER]
    if identity is not None:
        lines.append(f"provenant-front-door source-sha256={identity}")
    command.write_text("\n".join(lines) + "\n")
    command.chmod(0o755)
    return command


def current_digest(product: Path) -> str:
    from lib.front_door_build import source_digest

    return source_digest(product / "tools/front-door")


def test_check_accepts_a_binary_whose_embedded_digest_matches_the_sources(
    tmp_path: Path, crate_product: Path
) -> None:
    install_binary(tmp_path, current_digest(crate_product))

    result = run_check_in(crate_product, tmp_path)

    assert result.returncode == 0, result.stderr
    assert "provenant installed stub=ok" in result.stdout
    assert "front-door=rust" in result.stdout.splitlines()


def test_check_reports_a_rebuild_when_the_binary_predates_the_sources(
    tmp_path: Path, crate_product: Path
) -> None:
    install_binary(tmp_path, "0" * 64)

    result = run_check_in(crate_product, tmp_path)

    assert result.returncode == 1
    assert "built from different sources" in result.stderr
    assert "rebuild: re-run install-harness" in result.stderr


def test_check_detects_a_library_change_without_a_new_stamp(tmp_path, crate_product):
    binary = install_binary(tmp_path, current_digest(crate_product))
    stamped = binary.read_bytes()
    source = crate_product / "tools/front-door/crates/core/library.rs"
    original = source.read_bytes()
    assert run_check_in(crate_product, tmp_path).returncode == 0
    try:
        source.write_bytes(original + b"// changed library\n")
        result = run_check_in(crate_product, tmp_path)
        assert result.returncode == 1
        assert "built from different sources" in result.stderr
        assert "rebuild: re-run install-harness" in result.stderr
        assert binary.read_bytes() == stamped
    finally:
        source.write_bytes(original)
    assert run_check_in(crate_product, tmp_path).returncode == 0


def test_check_treats_an_unstamped_binary_as_stale(tmp_path: Path, crate_product: Path) -> None:
    install_binary(tmp_path, "unknown")

    result = run_check_in(crate_product, tmp_path)

    assert result.returncode == 1
    assert "rebuild: re-run install-harness" in result.stderr


def test_check_rejects_a_managed_file_with_no_identity_at_all(
    tmp_path: Path, crate_product: Path
) -> None:
    install_binary(tmp_path, None)

    result = run_check_in(crate_product, tmp_path)

    assert result.returncode == 1
    assert "has no Rust source identity" in result.stderr
