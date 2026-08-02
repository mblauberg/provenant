import os
from pathlib import Path
import shutil
import subprocess
import sys


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
        [sys.executable, str(SCRIPT)],
        env={
            **os.environ,
            "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
            "PROVENANT_BIN_DIR": str(tmp_path / "bin"),
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
