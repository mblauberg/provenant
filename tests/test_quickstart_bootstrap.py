from pathlib import Path
import json
import os
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def test_documented_fresh_checkout_sequence_produces_runnable_fabric(tmp_path):
    readme = (ROOT / "README.md").read_text()
    commands = [
        'npm ci',
        'scripts/install-harness --platform claude',
        'provenant help',
        'provenant fabric whoami',
    ]
    positions = [readme.index(command) for command in commands]
    assert positions == sorted(positions)

    checkout = tmp_path / "fresh-checkout"
    scripts = checkout / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(ROOT / "scripts" / "provenant", scripts / "provenant")
    shutil.copytree(ROOT / "scripts" / "lib", scripts / "lib")
    shutil.copytree(ROOT / "runtime" / "fabric", checkout / "runtime" / "fabric")

    common_dir = Path(
        subprocess.run(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    )
    installed_modules = common_dir.parent / "node_modules"
    assert (installed_modules / "tsx" / "dist" / "loader.mjs").is_file()
    (checkout / "node_modules").symlink_to(installed_modules, target_is_directory=True)

    env = {
        **os.environ,
        "HOME": str(tmp_path / "home"),
        "AGENTS_HOME": str(checkout),
        "AGENT_FABRIC_PRODUCT_ROOT": str(checkout),
        "AGENT_FABRIC_SEAT": "codex",
        "AGENT_FABRIC_STATE_DIRECTORY": str(tmp_path / "state"),
    }

    installed_bin = tmp_path / "installed-bin"
    installed_bin.mkdir()
    (installed_bin / "provenant").symlink_to(scripts / "provenant")
    caller_cwd = tmp_path / "project"
    caller_cwd.mkdir()
    fabric = subprocess.run(
        [str(installed_bin / "provenant"), "fabric", "whoami"],
        cwd=caller_cwd,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert fabric.returncode == 0, fabric.stderr
    payload = json.loads(fabric.stdout)
    assert payload["project"] == str(caller_cwd)
    assert payload["agentId"] == "codex"
    assert payload["provider"] == "codex"
    assert payload["database"] == str(tmp_path / "state" / "fabric.sqlite3")
    assert (tmp_path / "state" / "fabric.sqlite3").is_file()
