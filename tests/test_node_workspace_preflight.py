from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
PREFLIGHT = ROOT / "scripts/node-workspace-preflight.mjs"


def run(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=cwd,
        check=False,
        text=True,
        capture_output=True,
    )


def git(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    return run("git", *args, cwd=cwd)


def direct_dependencies(checkout: Path) -> set[str]:
    manifests = [checkout / "package.json", checkout / "runtime/fabric/package.json"]
    dependencies: set[str] = set()
    for manifest in manifests:
        package = json.loads(manifest.read_text())
        dependencies.update(package.get("dependencies", {}))
        dependencies.update(package.get("devDependencies", {}))
    return dependencies


def provision_direct_dependencies(checkout: Path) -> None:
    for dependency in direct_dependencies(checkout):
        package = checkout / "node_modules" / dependency / "package.json"
        package.parent.mkdir(parents=True, exist_ok=True)
        package.write_text(json.dumps({"name": dependency}) + "\n")


def registered_worktree(tmp_path: Path) -> tuple[Path, Path]:
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    (primary / "runtime/fabric").mkdir(parents=True)
    (primary / "scripts").mkdir()

    root_package = json.loads((ROOT / "package.json").read_text())
    root_package["scripts"] = {
        "precheck": "node scripts/node-workspace-preflight.mjs",
        "check": "node -e \"require('node:fs').writeFileSync('gate-ran','yes')\"",
    }
    (primary / "package.json").write_text(json.dumps(root_package, indent=2) + "\n")
    shutil.copy2(ROOT / "runtime/fabric/package.json", primary / "runtime/fabric/package.json")
    shutil.copy2(PREFLIGHT, primary / "scripts/node-workspace-preflight.mjs")

    assert git("init", "-q", cwd=primary).returncode == 0
    assert git("config", "user.email", "test@example.invalid", cwd=primary).returncode == 0
    assert git("config", "user.name", "Test", cwd=primary).returncode == 0
    assert git("add", ".", cwd=primary).returncode == 0
    assert git("commit", "-qm", "fixture", cwd=primary).returncode == 0
    created = git("worktree", "add", "-q", "-b", "lane", str(linked), cwd=primary)
    assert created.returncode == 0, created.stderr
    return primary, linked


def test_root_check_runs_checkout_dependency_preflight() -> None:
    package = json.loads((ROOT / "package.json").read_text())

    assert package["scripts"]["precheck"] == "node scripts/node-workspace-preflight.mjs"
    assert package["scripts"]["check"] == "npm run typecheck && npm run test"


def test_fresh_registered_worktree_fails_before_dependent_gate(tmp_path: Path) -> None:
    _primary, linked = registered_worktree(tmp_path)

    result = run("npm", "run", "check", cwd=linked)

    assert result.returncode == 3
    assert "missing checkout dependencies" in result.stderr
    assert str(linked.resolve()) in result.stderr
    assert "npm ci" in result.stderr
    assert not (linked / "gate-ran").exists()
    assert git("status", "--porcelain", cwd=linked).stdout == ""


def test_borrowed_primary_dependencies_do_not_mask_unready_worktree(tmp_path: Path) -> None:
    primary, linked = registered_worktree(tmp_path)
    provision_direct_dependencies(primary)
    (linked / "node_modules").symlink_to(primary / "node_modules", target_is_directory=True)

    result = run("npm", "run", "check", cwd=linked)

    assert result.returncode == 3
    assert "missing checkout dependencies" in result.stderr
    assert not (linked / "gate-ran").exists()


def test_symlinked_preflight_cannot_select_the_primary_checkout(tmp_path: Path) -> None:
    primary, linked = registered_worktree(tmp_path)
    provision_direct_dependencies(primary)
    (linked / "scripts/node-workspace-preflight.mjs").unlink()
    (linked / "scripts/node-workspace-preflight.mjs").symlink_to(
        primary / "scripts/node-workspace-preflight.mjs"
    )

    result = run("npm", "run", "check", cwd=linked)

    assert result.returncode == 3
    assert str(linked.resolve()) in result.stderr
    assert not (linked / "gate-ran").exists()


def test_external_package_directory_is_not_a_local_dependency(tmp_path: Path) -> None:
    _primary, linked = registered_worktree(tmp_path)
    provision_direct_dependencies(linked)
    external = tmp_path / "external-tsx"
    external.mkdir()
    marker = linked / "node_modules/.tsx-package.json"
    marker.write_text('{"name":"tsx"}\n')
    (external / "package.json").symlink_to(marker)
    shutil.rmtree(linked / "node_modules/tsx")
    (linked / "node_modules/tsx").symlink_to(external, target_is_directory=True)

    result = run("npm", "run", "check", cwd=linked)

    assert result.returncode == 3
    assert "tsx" in result.stderr
    assert not (linked / "gate-ran").exists()


def test_package_marker_must_name_the_declared_dependency(tmp_path: Path) -> None:
    _primary, linked = registered_worktree(tmp_path)
    provision_direct_dependencies(linked)
    (linked / "node_modules/tsx/package.json").write_text("{}\n")

    result = run("npm", "run", "check", cwd=linked)

    assert result.returncode == 3
    assert "tsx" in result.stderr
    assert not (linked / "gate-ran").exists()


def test_locally_provisioned_worktree_continues_into_gate(tmp_path: Path) -> None:
    _primary, linked = registered_worktree(tmp_path)
    provision_direct_dependencies(linked)

    result = run("npm", "run", "check", cwd=linked)

    assert result.returncode == 0, result.stderr
    assert (linked / "gate-ran").read_text() == "yes"
