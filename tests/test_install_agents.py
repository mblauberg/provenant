from pathlib import Path
import hashlib
import json
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "install-agents"
MANAGER = ROOT / "scripts" / "manage_installation.py"
MECHANICS = ROOT / "scripts" / "agent_installation.py"
SKILL_MANIFEST = ROOT / "scripts" / "managed_installation_manifest.py"
MANIFEST_NAME = ".agent-harness-agents-installation.json"
AGENT_NAMES = {
    "agy-reviewer.md",
    "codex-analyst.md",
    "codex-implementer.md",
}
AGENT_DIGESTS = {
    "agy-reviewer.md": "90f96cfbdeff3d72ffbb0965fca491939f9039f08ea8d5c75d6f78e9a970e2e4",
    "codex-analyst.md": "9752f0d18c713a4370df051f3f9eb55b2d5b650e33a0abde975bbf5ab6b807b8",
    "codex-implementer.md": "5ea965d7d8c346180225bffe297b5d4f4c13a60cac3265527ece21adfbc925bf",
}


def run(target: Path):
    return subprocess.run(
        [str(SCRIPT), "--target", str(target)],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def test_agent_definitions_are_tracked_by_installer(tmp_path):
    source = ROOT / "agents"
    assert source.is_dir(), "repository-owned agent source directory is missing"
    assert {path.name for path in source.glob("*.md")} == AGENT_NAMES

    target = tmp_path / "agents"
    result = run(target)

    assert result.returncode == 0, result.stderr
    assert {path.name for path in target.iterdir()} == AGENT_NAMES
    assert all((target / name).is_symlink() for name in AGENT_NAMES)
    assert all(
        (target / name).resolve() == (source / name).resolve()
        for name in AGENT_NAMES
    )
    manifest = json.loads((target.parent / MANIFEST_NAME).read_text())
    assert set(manifest["managed"]) == AGENT_NAMES
    assert {
        name: hashlib.sha256((source / name).read_bytes()).hexdigest()
        for name in AGENT_NAMES
    } == AGENT_DIGESTS


def test_agent_installer_is_idempotent_and_manifest_stable(tmp_path):
    target = tmp_path / "agents"
    first = run(target)
    assert first.returncode == 0, first.stderr
    before_manifest = (target.parent / MANIFEST_NAME).read_bytes()
    before_links = {
        path.name: path.lstat().st_mtime_ns for path in target.iterdir()
    }

    second = run(target)

    assert second.returncode == 0, second.stderr
    assert "agents linked=0 existing=3" in second.stdout
    assert (target.parent / MANIFEST_NAME).read_bytes() == before_manifest
    assert {
        path.name: path.lstat().st_mtime_ns for path in target.iterdir()
    } == before_links


def test_agent_installer_preserves_an_unmanaged_definition(tmp_path):
    target = tmp_path / "agents"
    target.mkdir()
    unmanaged = target / "codex-analyst.md"
    original = b"# User-owned definition\n"
    unmanaged.write_bytes(original)

    result = run(target)

    assert result.returncode == 3
    assert unmanaged.read_bytes() == original
    assert not unmanaged.is_symlink()
    assert "codex-analyst.md=unmanaged" in result.stderr
    manifest = json.loads((target.parent / MANIFEST_NAME).read_text())
    assert "codex-analyst.md" not in manifest["managed"]
    assert all(
        (target / name).is_symlink()
        for name in AGENT_NAMES - {"codex-analyst.md"}
    )


def test_agent_installer_does_not_touch_the_skill_manifest(tmp_path):
    target = tmp_path / "agents"
    skills_manifest = target.parent / ".agent-harness-installation.json"
    original = b"skills receipt owned by the skill installer\n"
    skills_manifest.write_bytes(original)

    result = run(target)

    assert result.returncode == 0, result.stderr
    assert skills_manifest.read_bytes() == original


def test_agent_installer_rejects_a_foreign_target_symlink(tmp_path):
    foreign = tmp_path / "foreign"
    foreign.mkdir()
    target = tmp_path / "agents"
    target.symlink_to(foreign, target_is_directory=True)

    result = run(target)

    assert result.returncode == 3
    assert "non-canonical symlink" in result.stderr
    assert not any(foreign.iterdir())


def test_agent_installer_rejects_a_broken_target_symlink(tmp_path):
    target = tmp_path / "agents"
    foreign = tmp_path / "foreign" / "missing"
    target.symlink_to(foreign, target_is_directory=True)

    result = run(target)

    assert result.returncode == 3
    assert target.is_symlink()
    assert "non-canonical symlink" in result.stderr


def test_agent_installer_rejects_a_symlinked_source_directory(tmp_path):
    fixture_root = tmp_path / "product"
    scripts = fixture_root / "scripts"
    external = tmp_path / "external"
    scripts.mkdir(parents=True)
    external.mkdir()
    (external / "evil.md").write_text("# external definition\n")
    shutil.copy2(SCRIPT, scripts / "install-agents")
    shutil.copy2(MANAGER, scripts / "manage_installation.py")
    shutil.copy2(MECHANICS, scripts / "agent_installation.py")
    shutil.copy2(SKILL_MANIFEST, scripts / "managed_installation_manifest.py")
    (fixture_root / "agents").symlink_to(external, target_is_directory=True)
    target = tmp_path / "claude" / "agents"

    result = subprocess.run(
        [str(scripts / "install-agents"), "--target", str(target)],
        cwd=fixture_root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 3
    assert "source must not be a symlink" in result.stderr
    assert not target.exists()


def test_agent_installer_preserves_a_foreign_broken_symlink(tmp_path):
    target = tmp_path / "agents"
    assert run(target).returncode == 0
    destination = target / "agy-reviewer.md"
    foreign_target = tmp_path / "foreign" / "missing.md"
    destination.unlink()
    destination.symlink_to(foreign_target)

    result = run(target)

    assert result.returncode == 3
    assert destination.is_symlink()
    assert destination.resolve(strict=False) == foreign_target
    assert "agy-reviewer.md" in result.stderr


def test_agent_installer_preserves_a_directory_link_to_canonical_sources(tmp_path):
    fixture_root = tmp_path / "product"
    scripts = fixture_root / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SCRIPT, scripts / "install-agents")
    shutil.copy2(MANAGER, scripts / "manage_installation.py")
    shutil.copy2(MECHANICS, scripts / "agent_installation.py")
    shutil.copy2(SKILL_MANIFEST, scripts / "managed_installation_manifest.py")
    shutil.copytree(ROOT / "agents", fixture_root / "agents")
    target = tmp_path / "claude" / "agents"
    target.parent.mkdir()
    target.symlink_to(fixture_root / "agents", target_is_directory=True)

    result = subprocess.run(
        [str(scripts / "install-agents"), "--target", str(target)],
        cwd=fixture_root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert target.is_symlink()
    assert target.resolve() == fixture_root / "agents"
    assert "agents existing=directory-link" in result.stdout
    assert not (tmp_path / "claude" / MANIFEST_NAME).exists()
    assert not (fixture_root / MANIFEST_NAME).exists()
