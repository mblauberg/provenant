from pathlib import Path
import json
import shutil
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "install-skills"
MANAGER = ROOT / "scripts" / "manage_installation.py"
SHARED = "_shared"


def run(target: Path, custom_source: Path | None = None):
    command = [str(SCRIPT), "--target", str(target)]
    if custom_source is not None:
        command.extend(["--custom-source", str(custom_source)])
    return subprocess.run(
        command,
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def catalogue_names():
    """Skills plus the shared library every per-entry layout must also carry."""
    return {path.parent.name for path in (ROOT / "skills").glob("*/SKILL.md")} | {SHARED}


def custom_skill_source(tmp_path: Path, name: str = "instance-skill"):
    source = tmp_path / "custom-skills"
    skill = source / name
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: Instance-owned test skill.\n---\n"
    )
    return source


def test_installer_links_every_skill_and_is_idempotent(tmp_path):
    target = tmp_path / "skills"
    first = run(target)
    assert first.returncode == 0, first.stderr
    expected = catalogue_names()
    assert {path.name for path in target.iterdir()} == expected
    assert all((target / name).is_symlink() for name in expected)
    assert f"linked={len(expected)} existing=0" in first.stdout

    second = run(target)
    assert second.returncode == 0, second.stderr
    assert f"linked=0 existing={len(expected)}" in second.stdout


def test_instance_skill_appears_in_per_entry_layout_without_managed_ownership(tmp_path):
    target = tmp_path / "skills"
    custom_source = custom_skill_source(tmp_path)

    result = run(target, custom_source)

    assert result.returncode == 0, result.stderr
    link = target / "instance-skill"
    assert link.is_symlink()
    assert link.resolve() == (custom_source / "instance-skill").resolve()
    manifest = json.loads(manifest_for(target).read_text())
    assert "instance-skill" not in manifest["managed"]


def test_source_name_collision_aborts_before_link_or_receipt_mutation(tmp_path):
    target = tmp_path / "skills"
    target.mkdir()
    marker = target / "keep"
    marker.write_text("untouched\n")
    custom_source = custom_skill_source(tmp_path, "scope")

    result = run(target, custom_source)

    assert result.returncode == 3
    assert str((ROOT / "skills" / "scope").resolve()) in result.stderr
    assert str((custom_source / "scope").resolve()) in result.stderr
    assert {path.name for path in target.iterdir()} == {"keep"}
    assert marker.read_text() == "untouched\n"
    assert not manifest_for(target).exists()


def test_second_two_source_run_is_a_filesystem_no_op(tmp_path):
    target = tmp_path / "skills"
    custom_source = custom_skill_source(tmp_path)
    first = run(target, custom_source)
    assert first.returncode == 0, first.stderr
    manifest = manifest_for(target)
    before_manifest = manifest.read_bytes()
    before_manifest_mtime = manifest.stat().st_mtime_ns
    before_links = {
        path.name: path.lstat().st_mtime_ns
        for path in target.iterdir()
        if path.is_symlink()
    }

    second = run(target, custom_source)

    assert second.returncode == 0, second.stderr
    assert "linked=0" in second.stdout
    assert manifest.read_bytes() == before_manifest
    assert manifest.stat().st_mtime_ns == before_manifest_mtime
    assert {
        path.name: path.lstat().st_mtime_ns
        for path in target.iterdir()
        if path.is_symlink()
    } == before_links


def test_flagless_run_preserves_custom_link_and_manifest_receipt(tmp_path):
    source = tiny_source(tmp_path)
    custom_source = custom_skill_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(
        target, "install", source, custom_source=custom_source
    ).returncode == 0
    custom_link = target / "instance-skill"
    manifest = manifest_for(target)
    before_link_mtime = custom_link.lstat().st_mtime_ns
    before_manifest = manifest.read_bytes()
    before_manifest_mtime = manifest.stat().st_mtime_ns

    result = manager(target, "install", source)

    assert result.returncode == 0, result.stderr
    assert custom_link.is_symlink()
    assert custom_link.resolve() == (custom_source / "instance-skill").resolve()
    assert custom_link.lstat().st_mtime_ns == before_link_mtime
    assert manifest.read_bytes() == before_manifest
    assert manifest.stat().st_mtime_ns == before_manifest_mtime
    assert "instance-skill" not in {
        item["name"] for item in json.loads(result.stdout)["items"]
    }


def test_flagless_run_does_not_handoff_recorded_custom_name_to_managed(tmp_path):
    source = tiny_source(tmp_path)
    custom_source = custom_skill_source(tmp_path, "gamma")
    target = tmp_path / "installed"
    assert manager(
        target, "install", source, custom_source=custom_source
    ).returncode == 0
    managed_gamma = source / "gamma"
    managed_gamma.mkdir()
    (managed_gamma / "SKILL.md").write_text(
        "---\nname: gamma\ndescription: Newly managed collision.\n---\n"
    )
    custom_link = target / "gamma"
    manifest_before = manifest_for(target).read_bytes()

    result = manager(target, "install", source)

    assert result.returncode == 0, result.stderr
    assert custom_link.resolve() == (custom_source / "gamma").resolve()
    assert manifest_for(target).read_bytes() == manifest_before
    assert "gamma" not in {
        item["name"] for item in json.loads(result.stdout)["items"]
    }


def test_removing_instance_source_prunes_its_projected_link(tmp_path):
    target = tmp_path / "skills"
    custom_source = custom_skill_source(tmp_path)
    assert run(target, custom_source).returncode == 0
    shutil.rmtree(custom_source)

    result = run(target, custom_source)

    assert result.returncode == 0, result.stderr
    assert not (target / "instance-skill").exists()
    manifest = json.loads(manifest_for(target).read_text())
    assert "instance-skill" not in manifest["managed"]


def test_modified_instance_link_is_refused_instead_of_replaced(tmp_path):
    target = tmp_path / "skills"
    custom_source = custom_skill_source(tmp_path)
    assert run(target, custom_source).returncode == 0
    foreign_target = tmp_path / "modified-out-of-band"
    foreign_target.mkdir()
    link = target / "instance-skill"
    link.unlink()
    link.symlink_to(foreign_target)
    manifest_before = manifest_for(target).read_bytes()

    result = run(target, custom_source)

    assert result.returncode == 3
    assert "changed outside harness" in result.stderr
    assert link.resolve() == foreign_target.resolve()
    assert manifest_for(target).read_bytes() == manifest_before


def test_missing_custom_link_error_names_manual_restore_or_source_remedy(tmp_path):
    target = tmp_path / "skills"
    custom_source = custom_skill_source(tmp_path)
    assert run(target, custom_source).returncode == 0
    link = target / "instance-skill"
    link.unlink()
    manifest_before = manifest_for(target).read_bytes()

    result = run(target)

    assert result.returncode == 3
    assert (
        f"manually restore instance-skill from "
        f"{(custom_source / 'instance-skill').resolve()}"
    ) in result.stderr
    assert (
        "or provide --custom-source pointing to the intended custom skills directory"
        in result.stderr
    )
    assert not link.exists()
    assert manifest_for(target).read_bytes() == manifest_before


def test_flagless_check_rejects_missing_recorded_custom_source(tmp_path):
    target = tmp_path / "skills"
    custom_source = custom_skill_source(tmp_path)
    assert run(target, custom_source).returncode == 0
    shutil.rmtree(custom_source / "instance-skill")
    manifest_before = manifest_for(target).read_bytes()

    result = run(target)

    assert result.returncode == 3
    assert (
        f"manually restore instance-skill from "
        f"{(custom_source / 'instance-skill').resolve()}"
    ) in result.stderr
    assert (
        "or provide --custom-source pointing to the intended custom skills directory"
        in result.stderr
    )
    assert (target / "instance-skill").is_symlink()
    assert manifest_for(target).read_bytes() == manifest_before


def test_installer_preserves_existing_entries(tmp_path):
    target = tmp_path / "skills"
    target.mkdir()
    existing = target / "scope"
    existing.mkdir()
    marker = existing / "keep.txt"
    marker.write_text("owned elsewhere\n")

    result = run(target)
    assert result.returncode == 3
    assert "scope=noncanonical" in result.stderr
    assert marker.read_text() == "owned elsewhere\n"
    assert not existing.is_symlink()


def test_installer_reports_foreign_global_skill_without_removing_it(tmp_path):
    target = tmp_path / "skills"
    target.mkdir()
    private = tmp_path / "private-project" / "private-skill"
    private.mkdir(parents=True)
    foreign = target / "private-skill"
    foreign.symlink_to(private)

    result = run(target)

    assert result.returncode == 0
    assert "warning:" in result.stderr
    assert "private-skill=foreign" in result.stderr
    assert foreign.is_symlink()
    assert foreign.resolve() == private.resolve()


def test_check_rejects_empty_directory_at_required_catalogue_name(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    target.mkdir()
    (target / "alpha").mkdir()

    result = manager(target, "check", source)

    assert result.returncode == 3
    assert "alpha=noncanonical" in result.stderr
    assert (target / "alpha").is_dir()


def test_check_rejects_mismatched_skill_at_required_catalogue_name(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    target.mkdir()
    collision = target / "alpha"
    collision.mkdir()
    (collision / "SKILL.md").write_text(
        "---\nname: alpha\ndescription: Different content.\n---\n"
    )

    result = manager(target, "check", source)

    assert result.returncode == 3
    assert "alpha=noncanonical" in result.stderr
    assert "Different content" in (collision / "SKILL.md").read_text()


def test_installer_reconciles_previously_managed_link_drift(tmp_path):
    target = tmp_path / "skills"
    assert run(target).returncode == 0
    scope = target / "scope"
    scope.unlink()

    result = run(target)

    assert result.returncode == 0, result.stderr
    assert scope.resolve() == (ROOT / "skills" / "scope").resolve()


def test_directory_symlink_to_canonical_skills_is_preserved_without_manifest(tmp_path):
    fixture_root = tmp_path / "agents"
    scripts = fixture_root / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SCRIPT, scripts / "install-skills")
    shutil.copy2(MANAGER, scripts / "manage_installation.py")
    shutil.copytree(ROOT / "skills", fixture_root / "skills")
    platform_home = tmp_path / "claude"
    platform_home.mkdir()
    target = platform_home / "skills"
    target.symlink_to(fixture_root / "skills", target_is_directory=True)

    result = subprocess.run(
        [str(scripts / "install-skills"), "--target", str(target)],
        cwd=fixture_root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert target.is_symlink()
    assert target.resolve() == fixture_root / "skills"
    assert "skills existing=" in result.stdout
    # The projection exposes the shared library with the skills, so this layout
    # owns no per-entry manifest row for it.
    assert (target / SHARED / "review_ladder.py").is_file()
    assert not (fixture_root / ".agent-harness-installation.json").exists()
    assert not (platform_home / ".agent-harness-installation.json").exists()


def test_directory_symlink_layout_warns_about_skipped_custom_skills(tmp_path):
    fixture_root = tmp_path / "agents"
    scripts = fixture_root / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SCRIPT, scripts / "install-skills")
    shutil.copy2(MANAGER, scripts / "manage_installation.py")
    shutil.copy2(
        ROOT / "scripts" / "managed_installation_manifest.py",
        scripts / "managed_installation_manifest.py",
    )
    shutil.copytree(ROOT / "skills", fixture_root / "skills")
    custom_source = custom_skill_source(fixture_root)
    platform_home = tmp_path / "claude"
    platform_home.mkdir()
    target = platform_home / "skills"
    target.symlink_to(fixture_root / "skills", target_is_directory=True)

    result = subprocess.run(
        [
            str(scripts / "install-skills"),
            "--target",
            str(target),
            "--custom-source",
            str(custom_source),
        ],
        cwd=fixture_root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert (
        "warning: directory-link layout skipped custom skills: instance-skill"
        in result.stderr
    )
    assert target.is_symlink()
    assert target.resolve() == fixture_root / "skills"
    assert not (platform_home / ".agent-harness-installation.json").exists()


def test_directory_symlink_layout_rejects_two_source_name_collision(tmp_path):
    fixture_root = tmp_path / "agents"
    scripts = fixture_root / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SCRIPT, scripts / "install-skills")
    shutil.copy2(MANAGER, scripts / "manage_installation.py")
    shutil.copy2(
        ROOT / "scripts" / "managed_installation_manifest.py",
        scripts / "managed_installation_manifest.py",
    )
    shutil.copytree(ROOT / "skills", fixture_root / "skills")
    custom_source = fixture_root / "custom-skills"
    custom_scope = custom_source / "scope"
    custom_scope.mkdir(parents=True)
    (custom_scope / "SKILL.md").write_text(
        "---\nname: scope\ndescription: Colliding instance skill.\n---\n"
    )
    platform_home = tmp_path / "claude"
    platform_home.mkdir()
    target = platform_home / "skills"
    target.symlink_to(fixture_root / "skills", target_is_directory=True)

    result = subprocess.run(
        [
            str(scripts / "install-skills"),
            "--target",
            str(target),
            "--custom-source",
            str(custom_source),
        ],
        cwd=fixture_root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 3
    assert str((fixture_root / "skills" / "scope").resolve()) in result.stderr
    assert str(custom_scope.resolve()) in result.stderr
    assert target.is_symlink()
    assert target.resolve() == fixture_root / "skills"
    assert not (platform_home / ".agent-harness-installation.json").exists()


def test_per_entry_layout_installs_and_tracks_the_shared_library(tmp_path):
    target = tmp_path / "skills"
    assert run(target).returncode == 0

    link = target / SHARED
    assert link.is_symlink()
    assert link.resolve() == (ROOT / "skills" / SHARED).resolve()
    entry = json.loads(manifest_for(target).read_text())["managed"][SHARED]
    assert entry["owner"] == "agent-harness"
    assert entry["source_target"] == str((ROOT / "skills" / SHARED).resolve())
    assert len(entry["source_sha256"]) == 64


def test_check_fails_on_a_missing_shared_library_and_install_repairs_it(tmp_path):
    target = tmp_path / "skills"
    assert run(target).returncode == 0
    (target / SHARED).unlink()

    missing = manager(target, "check")

    assert missing.returncode == 3
    report = json.loads(missing.stdout)
    assert {item["name"]: item["state"] for item in report["items"]}[SHARED] == "missing"

    assert run(target).returncode == 0
    assert (target / SHARED).resolve() == (ROOT / "skills" / SHARED).resolve()


def test_stale_shared_library_content_is_relinked_and_redigested(tmp_path):
    source = tiny_source(tmp_path)
    shared = source / SHARED
    shared.mkdir()
    (shared / "review_ladder.py").write_text("VALUE = 1\n")
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    first = json.loads(manifest_for(target).read_text())["managed"][SHARED]

    (shared / "review_ladder.py").write_text("VALUE = 2\n")
    plan = json.loads(manager(target, "plan", source).stdout)
    assert {item["name"]: item["state"] for item in plan["items"]}[SHARED] == "stale"

    assert manager(target, "install", source).returncode == 0
    second = json.loads(manifest_for(target).read_text())["managed"][SHARED]
    assert second["source_sha256"] != first["source_sha256"]


def test_uninstall_managed_reclaims_the_shared_library(tmp_path):
    target = tmp_path / "skills"
    assert run(target).returncode == 0

    result = manager(target, "uninstall-managed")

    assert result.returncode == 0, result.stderr
    assert not (target / SHARED).exists()
    assert json.loads(manifest_for(target).read_text())["managed"] == {}


def test_installed_per_entry_layout_is_a_sufficient_import_root_for_shared(tmp_path):
    target = tmp_path / "skills"
    assert run(target).returncode == 0

    result = subprocess.run(
        [
            sys.executable,
            "-I",
            "-c",
            "import sys; sys.path.insert(0, sys.argv[1]);"
            " import _shared.review_ladder, _shared.review_panel;"
            " print(_shared.review_ladder.__file__)",
            str(target),
        ],
        cwd=tmp_path,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().startswith(str(target))


CONSUMERS = (
    "orchestrate/scripts/run_dir_finalize.py",
    "deliver/scripts/validate_delivery.py",
)


def materialise(target: Path, product: Path):
    """Copy the receipt-tracked entries into a layout of real directories.

    A symlinked entry is not enough to prove ownership: both known consumers
    canonicalise their own file before importing `_shared`, so a symlink
    resolves back into the source tree and the source copy satisfies the import
    whether or not the installer owns it. Materialising exactly what the
    manifest declares makes the installed catalogue the resolved root, so a
    catalogue that omitted `_shared` would produce a broken layout here.
    """
    managed = json.loads(manifest_for(target).read_text())["managed"]
    installed = product / "skills"
    installed.mkdir(parents=True)
    for name in managed:
        shutil.copytree(target / name, installed / name, symlinks=False)
    return installed


def consumer_help(script: Path, cwd: Path):
    # -I isolates the interpreter, so an inherited PYTHONPATH or a user-site
    # .pth cannot satisfy the import and make the negative leg vacuous.
    return subprocess.run(
        [sys.executable, "-I", str(script), "--help"],
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def test_materialised_per_entry_layout_makes_the_shared_library_load_bearing(tmp_path):
    target = tmp_path / "skills"
    assert run(target).returncode == 0
    installed = materialise(target, tmp_path / "product")

    for consumer in CONSUMERS:
        result = consumer_help(installed / consumer, tmp_path)
        assert result.returncode == 0, f"{consumer}: {result.stderr}"
        assert "usage:" in result.stdout

    # Removing only the installed shared library must break both consumers,
    # which is what proves the installed copy - not the source tree - satisfied
    # the import above.
    shutil.rmtree(installed / SHARED)

    for consumer in CONSUMERS:
        result = consumer_help(installed / consumer, tmp_path)
        assert result.returncode != 0, f"{consumer} ran without the shared library"
        assert "No module named '_shared'" in result.stderr


def test_installer_requires_a_target():
    result = subprocess.run(
        [str(SCRIPT)],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert result.returncode == 2
    assert "usage:" in result.stderr


def manager(
    target: Path,
    action: str,
    source: Path | None = None,
    renames: Path | None = None,
    custom_source: Path | None = None,
):
    command = [str(MANAGER), action, "--target", str(target)]
    if source:
        command.extend(["--source", str(source)])
    if renames:
        command.extend(["--renames", str(renames)])
    if custom_source:
        command.extend(["--custom-source", str(custom_source)])
    return subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)


def manifest_for(target: Path):
    return target.parent / ".agent-harness-installation.json"


def tiny_source(tmp_path: Path):
    source = tmp_path / "source"
    for name in ("alpha", "beta"):
        skill = source / name
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text(f"---\nname: {name}\ndescription: Use when testing.\n---\n")
    return source


def test_plan_is_read_only_and_distinguishes_missing_and_unmanaged(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    target.mkdir()
    (target / "alpha").mkdir()
    result = manager(target, "plan", source)
    assert result.returncode == 0, result.stderr
    plan = json.loads(result.stdout)
    assert {item["name"]: item["state"] for item in plan["items"]} == {"alpha": "unmanaged", "beta": "missing"}
    assert not manifest_for(target).exists()
    assert not (target / "beta").exists()


def test_install_records_versioned_ownership_without_overwriting_unmanaged(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    target.mkdir()
    unmanaged = target / "alpha"
    unmanaged.mkdir()
    (unmanaged / "keep").write_text("mine")
    result = manager(target, "install", source)
    assert result.returncode == 0, result.stderr
    manifest = json.loads(manifest_for(target).read_text())
    assert manifest["schema_version"] == 1
    assert set(manifest["managed"]) == {"beta"}
    assert manifest["managed"]["beta"]["owner"] == "agent-harness"
    assert (target / "beta").is_symlink()
    assert (unmanaged / "keep").read_text() == "mine"


def test_reconcile_repairs_broken_managed_link_and_rejects_conflict(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (target / "alpha").unlink()
    (target / "alpha").symlink_to(tmp_path / "missing")
    repaired = manager(target, "reconcile", source)
    assert repaired.returncode == 0, repaired.stderr
    assert (target / "alpha").resolve() == (source / "alpha").resolve()

    (target / "beta").unlink()
    (target / "beta").mkdir()
    conflict = manager(target, "reconcile", source)
    assert conflict.returncode == 3
    assert "conflicting" in conflict.stderr
    assert (target / "beta").is_dir()


def test_normal_install_repairs_managed_drift_and_retires_safe_old_links(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (source / "beta" / "SKILL.md").write_text(
        "---\nname: beta\ndescription: Use when changed.\n---\n"
    )
    retired_source = source / "alpha"
    retired_target = target / "alpha"
    retired_source.rename(source / "gamma")

    result = manager(target, "install", source)

    assert result.returncode == 0, result.stderr
    manifest = json.loads(manifest_for(target).read_text())
    assert set(manifest["managed"]) == {"beta", "gamma"}
    assert not retired_target.exists()
    assert (target / "beta").resolve() == (source / "beta").resolve()
    assert manifest["managed"]["beta"]["source_sha256"] != "0" * 64
    assert (target / "gamma").resolve() == (source / "gamma").resolve()


def test_uninstall_managed_removes_only_owned_exact_links(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    unmanaged = target / "mine"
    unmanaged.mkdir()
    result = manager(target, "uninstall-managed", source)
    assert result.returncode == 0, result.stderr
    assert unmanaged.is_dir()
    assert not (target / "alpha").exists()
    assert not (target / "beta").exists()
    assert json.loads(manifest_for(target).read_text())["managed"] == {}


def test_reconcile_applies_safe_managed_rename_with_history(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (source / "alpha").rename(source / "gamma")
    renames = tmp_path / "renames.json"
    renames.write_text(json.dumps({"schema_version": 1, "renames": [{"from": "alpha", "to": "gamma"}]}))
    result = manager(target, "reconcile", source, renames)
    assert result.returncode == 0, result.stderr
    manifest = json.loads(manifest_for(target).read_text())
    assert "alpha" not in manifest["managed"]
    assert manifest["managed"]["gamma"]["history"][-1]["from"] == "alpha"
    assert not (target / "alpha").exists()
    assert (target / "gamma").resolve() == (source / "gamma").resolve()


def test_reconcile_preserves_compatible_unmanaged_rename_target(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (source / "alpha").rename(source / "gamma")
    unmanaged = target / "gamma"
    unmanaged.symlink_to(source / "gamma")
    renames = tmp_path / "renames.json"
    renames.write_text(json.dumps({"schema_version": 1, "renames": [{"from": "alpha", "to": "gamma"}]}))

    reconciled = manager(target, "reconcile", source, renames)

    assert reconciled.returncode == 0, reconciled.stderr
    manifest = json.loads(manifest_for(target).read_text())
    assert set(manifest["managed"]) == {"beta"}
    assert unmanaged.is_symlink()
    assert unmanaged.resolve() == (source / "gamma").resolve()

    uninstalled = manager(target, "uninstall-managed", source)

    assert uninstalled.returncode == 0, uninstalled.stderr
    assert unmanaged.is_symlink()
    assert unmanaged.resolve() == (source / "gamma").resolve()


def test_reconcile_merges_two_sources_into_one_target(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    # A many-to-one skill merge: alpha + beta collapse into a single gamma.
    (source / "alpha").rename(source / "gamma")
    (source / "beta" / "SKILL.md").unlink()
    (source / "beta").rmdir()
    renames = tmp_path / "renames.json"
    renames.write_text(json.dumps({"schema_version": 1, "renames": [
        {"from": "alpha", "to": "gamma"},
        {"from": "beta", "to": "gamma"},
    ]}))
    result = manager(target, "reconcile", source, renames)
    assert result.returncode == 0, result.stderr
    manifest = json.loads(manifest_for(target).read_text())
    assert set(manifest["managed"]) == {"gamma"}
    assert not (target / "alpha").exists()
    assert not (target / "beta").exists()
    assert (target / "gamma").resolve() == (source / "gamma").resolve()
    froms = {entry["from"] for entry in manifest["managed"]["gamma"]["history"]}
    assert {"alpha", "beta"} <= froms


def test_plain_install_then_reconcile_merges_an_already_installed_rename(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (source / "alpha").rename(source / "gamma")
    installed = manager(target, "install", source)
    assert installed.returncode == 0
    assert not (target / "alpha").exists()
    assert (target / "gamma").resolve() == (source / "gamma").resolve()
    renames = tmp_path / "renames.json"
    renames.write_text(json.dumps({"schema_version": 1, "renames": [{"from": "alpha", "to": "gamma"}]}))
    result = manager(target, "reconcile", source, renames)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["changed"] == []
    manifest = json.loads(manifest_for(target).read_text())
    assert set(manifest["managed"]) == {"beta", "gamma"}
    assert not (target / "alpha").exists()


def test_manifest_is_bound_to_one_target_root(tmp_path):
    source = tiny_source(tmp_path)
    first = tmp_path / "first"
    second = tmp_path / "second"
    assert manager(first, "install", source).returncode == 0
    result = manager(second, "install", source)
    assert result.returncode == 3
    assert "different target root" in result.stderr


def test_retired_missing_entry_does_not_block_uninstall(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (source / "alpha").rename(source / "gamma")
    (target / "alpha").unlink()
    result = manager(target, "uninstall-managed", source)
    assert result.returncode == 0, result.stderr
    assert json.loads(manifest_for(target).read_text())["managed"] == {}


def test_uninstall_persists_when_every_managed_link_is_already_missing(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    for name in ("alpha", "beta"):
        (target / name).unlink()

    result = manager(target, "uninstall-managed", source)

    assert result.returncode == 0, result.stderr
    assert json.loads(manifest_for(target).read_text())["managed"] == {}


def test_removed_custom_skill_can_handoff_to_new_managed_skill_in_one_run(
    tmp_path,
):
    source = tiny_source(tmp_path)
    custom_source = custom_skill_source(tmp_path, "gamma")
    target = tmp_path / "installed"
    assert manager(
        target, "install", source, custom_source=custom_source
    ).returncode == 0
    shutil.rmtree(custom_source / "gamma")
    managed_gamma = source / "gamma"
    managed_gamma.mkdir()
    (managed_gamma / "SKILL.md").write_text(
        "---\nname: gamma\ndescription: Product-managed replacement.\n---\n"
    )

    result = manager(target, "install", source, custom_source=custom_source)

    assert result.returncode == 0, result.stderr
    assert (target / "gamma").resolve() == managed_gamma.resolve()
    manifest = json.loads(manifest_for(target).read_text())
    assert "gamma" in manifest["managed"]
    assert "gamma" not in manifest["custom"]


def test_custom_source_rebind_updates_only_an_unchanged_recorded_link(tmp_path):
    source = tiny_source(tmp_path)
    first_custom = custom_skill_source(tmp_path / "first", "local-skill")
    second_custom = custom_skill_source(tmp_path / "second", "local-skill")
    target = tmp_path / "installed"
    assert manager(
        target, "install", source, custom_source=first_custom
    ).returncode == 0

    result = manager(target, "install", source, custom_source=second_custom)

    assert result.returncode == 0, result.stderr
    assert (target / "local-skill").resolve() == (
        second_custom / "local-skill"
    ).resolve()
    manifest = json.loads(manifest_for(target).read_text())
    assert manifest["custom"]["local-skill"]["source_target"] == str(
        (second_custom / "local-skill").resolve()
    )


def test_custom_rebind_refuses_out_of_band_repoint_to_new_desired_source(
    tmp_path,
):
    source = tiny_source(tmp_path)
    first_custom = custom_skill_source(tmp_path / "first", "local-skill")
    second_custom = custom_skill_source(tmp_path / "second", "local-skill")
    target = tmp_path / "installed"
    assert manager(
        target, "install", source, custom_source=first_custom
    ).returncode == 0
    link = target / "local-skill"
    link.unlink()
    link.symlink_to(second_custom / "local-skill")
    manifest_before = manifest_for(target).read_bytes()

    result = manager(target, "install", source, custom_source=second_custom)

    assert result.returncode == 3
    assert "changed outside harness" in result.stderr
    assert link.resolve() == (second_custom / "local-skill").resolve()
    assert manifest_for(target).read_bytes() == manifest_before


def test_removed_managed_skill_can_handoff_to_new_custom_skill_in_one_run(
    tmp_path,
):
    source = tiny_source(tmp_path)
    custom_source = tmp_path / "custom-skills"
    custom_source.mkdir()
    target = tmp_path / "installed"
    assert manager(
        target, "install", source, custom_source=custom_source
    ).returncode == 0
    shutil.rmtree(source / "alpha")
    custom_alpha = custom_source / "alpha"
    custom_alpha.mkdir()
    (custom_alpha / "SKILL.md").write_text(
        "---\nname: alpha\ndescription: Instance-owned replacement.\n---\n"
    )

    result = manager(target, "install", source, custom_source=custom_source)

    assert result.returncode == 0, result.stderr
    assert (target / "alpha").resolve() == custom_alpha.resolve()
    manifest = json.loads(manifest_for(target).read_text())
    assert "alpha" not in manifest["managed"]
    assert manifest["custom"]["alpha"] == {
        "source_target": str(custom_alpha.resolve())
    }


def test_full_skill_tree_digest_marks_reference_change_stale(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    (source / "alpha" / "reference.md").write_text("one\n")
    assert manager(target, "install", source).returncode == 0
    (source / "alpha" / "reference.md").write_text("two\n")
    plan = json.loads(manager(target, "plan", source).stdout)
    assert {item["name"]: item["state"] for item in plan["items"]}["alpha"] == "stale"


def test_full_skill_tree_digest_marks_executable_mode_change_stale(tmp_path):
    source = tiny_source(tmp_path)
    helper = source / "alpha" / "run"
    helper.write_text("#!/bin/sh\nexit 0\n")
    helper.chmod(0o755)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    helper.chmod(0o644)
    plan = json.loads(manager(target, "plan", source).stdout)
    assert {item["name"]: item["state"] for item in plan["items"]}["alpha"] == "stale"


def test_check_reports_foreign_resolving_link_without_deleting_it(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    private_skill = tmp_path / "private-project" / "secret-skill"
    private_skill.mkdir(parents=True)
    foreign = target / "secret-skill"
    foreign.symlink_to(private_skill)

    result = manager(target, "check", source)

    assert result.returncode == 0
    report = json.loads(result.stdout)
    assert {item["name"]: item["state"] for item in report["items"]}["secret-skill"] == "foreign"
    assert foreign.is_symlink()
    assert foreign.resolve() == private_skill.resolve()


def test_check_verifies_canonical_catalogue_and_ignores_ds_store(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (target / ".DS_Store").write_bytes(b"metadata")
    (target / "alpha").unlink()

    missing = manager(target, "check", source)

    assert missing.returncode == 3
    report = json.loads(missing.stdout)
    assert {item["name"]: item["state"] for item in report["items"]}["alpha"] == "missing"
    assert ".DS_Store" not in {item["name"] for item in report["items"]}

    assert manager(target, "install", source).returncode == 0
    assert manager(target, "check", source).returncode == 0


def test_manifest_key_traversal_fails_before_uninstall_mutation(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    target.mkdir()
    victim = tmp_path / "victim"
    victim.symlink_to(source / "alpha")
    # Every other top-level field must be valid, including the bound target
    # root, so the rejection can only come from the managed-name guard itself.
    manifest_for(target).write_text(json.dumps({
        "schema_version": 1,
        "owner": "agent-harness",
        "target_root": str(target.resolve()),
        "updated_at": "2026-07-10T00:00:00Z",
        "managed": {"../victim": {"owner": "agent-harness", "source_target": str(source / "alpha"), "source_sha256": "a" * 64, "installed_at": "2026-07-10T00:00:00Z", "history": []}},
    }))
    result = manager(target, "uninstall-managed", source)
    assert result.returncode == 3
    assert "installation manifest contains an invalid skill name" in result.stderr
    assert victim.is_symlink()


def test_rename_reconcile_preflights_all_conflicts_before_mutation(tmp_path):
    source = tiny_source(tmp_path)
    target = tmp_path / "installed"
    assert manager(target, "install", source).returncode == 0
    (source / "alpha").rename(source / "gamma")
    (target / "beta").unlink()
    (target / "beta").mkdir()
    renames = tmp_path / "renames.json"
    renames.write_text(json.dumps({"schema_version": 1, "renames": [{"from": "alpha", "to": "gamma"}]}))
    result = manager(target, "reconcile", source, renames)
    assert result.returncode == 3
    assert (target / "alpha").is_symlink()
    assert not (target / "gamma").exists()
    assert "alpha" in json.loads(manifest_for(target).read_text())["managed"]


def test_installer_refuses_symlinked_source_content(tmp_path):
    source = tiny_source(tmp_path)
    outside = tmp_path / "outside.txt"
    outside.write_text("not part of the source tree\n")
    (source / "alpha" / "reference.md").symlink_to(outside)
    target = tmp_path / "installed"

    result = manager(target, "install", source)

    assert result.returncode == 3
    assert "skill source contains a symlink" in result.stderr
    assert not (target / "alpha").exists()
    assert not manifest_for(target).exists()
