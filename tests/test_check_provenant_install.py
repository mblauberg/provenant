import os
import json
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/check-provenant-install.py"
TEMPLATE = ROOT / "scripts/provenant.template"


def run_check(tmp_path: Path) -> subprocess.CompletedProcess[str]:
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True, exist_ok=True)
    pointer.write_text(
        f'{{"schema_version": 1, "product_root": "{ROOT}"}}\n'
    )
    return subprocess.run(
        [str(SCRIPT)],
        env={
            **os.environ,
            "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
            "PROVENANT_BIN_DIR": str(tmp_path / "bin"),
            "HOME": str(tmp_path / "home"),
        },
        text=True,
        capture_output=True,
        check=False,
    )


def test_check_accepts_an_installed_stub_matching_the_template(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    shutil.copy2(TEMPLATE, command)
    command.chmod(0o755)

    result = run_check(tmp_path)

    assert result.returncode == 0, result.stderr
    assert "provenant installed stub=ok" in result.stdout


def test_check_names_routing_drift_and_refresh_repair(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    shutil.copy2(TEMPLATE, command)
    command.chmod(0o755)
    catalogue = tmp_path / "instance/config/model-routing.json"
    catalogue.parent.mkdir(parents=True)
    product = json.loads((ROOT / "config/model-routing.json").read_text())
    product["adapters"]["opencode"]["endpoint_provider"] = "codex"
    catalogue.write_text(json.dumps(product))

    result = run_check(tmp_path)

    assert result.returncode == 1
    assert "routing drift=adapters.opencode.endpoint_provider" in result.stderr
    assert "install-harness --refresh-routing" in result.stderr


def test_check_reports_each_provider_on_one_line(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    shutil.copy2(TEMPLATE, command)
    command.chmod(0o755)
    result = run_check(tmp_path)

    assert result.returncode == 0, result.stderr
    lines = [line for line in result.stdout.splitlines() if line.startswith("provider ")]
    assert len(lines) == 6
    assert {line.split()[1] for line in lines} == {
        "claude", "codex", "opencode", "agy", "cursor", "kiro",
    }


def test_check_names_repair_for_present_provider_without_install(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    shutil.copy2(TEMPLATE, command)
    command.chmod(0o755)
    (tmp_path / "home/.config/opencode").mkdir(parents=True)

    result = run_check(tmp_path)

    assert result.returncode == 1
    assert "provider opencode present=yes skills=missing" in result.stdout
    assert "repair=install-harness --platform all" in result.stdout


def test_check_reports_invalid_provider_config_without_a_traceback(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    shutil.copy2(TEMPLATE, command)
    command.chmod(0o755)
    config = tmp_path / "home/.config/opencode/opencode.jsonc"
    config.parent.mkdir(parents=True)
    config.write_text("[]")

    result = run_check(tmp_path)

    assert result.returncode == 1
    assert "provider opencode present=yes skills=missing agents=unsupported mcp=missing" in result.stdout
    assert "Traceback" not in result.stderr


def test_check_rejects_installed_stub_drift(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.write_text("# managed but stale\n")
    command.chmod(0o755)

    result = run_check(tmp_path)

    assert result.returncode == 1
    assert "installed stub differs from scripts/provenant.template" in result.stderr
    assert "re-run install-harness" in result.stderr


def test_check_rejects_a_symlink_instead_of_a_managed_copy(tmp_path: Path) -> None:
    command = tmp_path / "bin/provenant"
    command.parent.mkdir()
    command.symlink_to(TEMPLATE)

    result = run_check(tmp_path)

    assert result.returncode == 1
    assert "must be a regular managed copy" in result.stderr


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
    shutil.copy2(TEMPLATE, command)
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
