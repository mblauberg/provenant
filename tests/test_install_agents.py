from pathlib import Path
import hashlib
import json
import subprocess


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "install-agents"
AGENT_NAMES = {
    "agy-reviewer.md",
    "codex-analyst.md",
    "codex-implementer.md",
}
MANIFEST_NAME = ".agent-harness-agents-installation.json"


def run(target: Path):
    return subprocess.run(
        [str(SCRIPT), "--target", str(target)],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def test_installs_all_claude_subagents_and_records_source_digests(tmp_path):
    source = ROOT / "agents"
    target = tmp_path / "agents"

    result = run(target)

    assert result.returncode == 0, result.stderr
    assert {path.name for path in target.iterdir()} == AGENT_NAMES
    manifest = json.loads((target.parent / MANIFEST_NAME).read_text())
    assert set(manifest["managed"]) == AGENT_NAMES
    for name in AGENT_NAMES:
        installed = target / name
        assert installed.is_symlink()
        assert installed.resolve() == (source / name).resolve()
        assert installed.read_bytes() == (source / name).read_bytes()
        assert manifest["managed"][name]["source_sha256"] == hashlib.sha256(
            (source / name).read_bytes()
        ).hexdigest()


def test_reinstalling_claude_subagents_is_idempotent(tmp_path):
    target = tmp_path / "agents"
    first = run(target)
    assert first.returncode == 0, first.stderr
    manifest_before = (target.parent / MANIFEST_NAME).read_bytes()
    links_before = {path.name: path.lstat().st_mtime_ns for path in target.iterdir()}

    second = run(target)

    assert second.returncode == 0, second.stderr
    assert "agents linked=0 existing=3" in second.stdout
    assert (target.parent / MANIFEST_NAME).read_bytes() == manifest_before
    assert {
        path.name: path.lstat().st_mtime_ns for path in target.iterdir()
    } == links_before


def test_claude_subagent_installation_preserves_an_unmanaged_file(tmp_path):
    target = tmp_path / "agents"
    target.mkdir(parents=True)
    unmanaged = target / "codex-analyst.md"
    original = b"# User-owned definition\r\nno trailing newline"
    unmanaged.write_bytes(original)

    result = run(target)

    assert result.returncode == 3
    assert unmanaged.read_bytes() == original
    assert not unmanaged.is_symlink()
    assert "codex-analyst.md=unmanaged" in result.stderr


def test_uninstall_managed_claude_subagents_removes_only_recorded_links(tmp_path):
    target = tmp_path / "agents"
    installed = run(target)
    assert installed.returncode == 0, installed.stderr
    (target / "user-owned.md").write_bytes(b"keep me\n")

    result = subprocess.run(
        [
            str(ROOT / "scripts" / "manage_installation.py"),
            "uninstall-managed",
            "--surface",
            "agents",
            "--source",
            str(ROOT / "agents"),
            "--target",
            str(target),
        ],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert not any((target / name).exists() for name in AGENT_NAMES)
    assert (target / "user-owned.md").read_bytes() == b"keep me\n"
