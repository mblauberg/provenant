from pathlib import Path
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "install-harness"
PROVENANT_TEMPLATE = ROOT / "scripts" / "provenant.template"
WORKFLOW_SCRIPT = ROOT / "scripts" / "install-workflows"
WORKFLOW_NAMES = {
    "codebase-polish.js",
    "cross-verify.js",
    "implement-run.js",
}
AGENT_NAMES = {
    "agy-reviewer.md",
    "agy-stylist.md",
    "codex-analyst.md",
    "codex-implementer.md",
}
UNMANAGED_WORKFLOW_BYTES = (
    b"export const meta = { name: 'mine' };\r\n"
    b"// User-owned workflow with no trailing newline"
)
EXPECTED_AMBIENT_SKILL_NAMES = frozenset(
    {
        "caveman",
        "code-review",
        "deliver",
        "diagnose",
        "evaluate",
        "implement",
        # The constitution routes reader-facing prose to one named entry point;
        # `natural-writing` resolves its own specialists, so only the entry
        # point is ambient.
        "natural-writing",
        "orchestrate",
        "release",
        "retrospect",
        "scope",
        "session",
        "tdd",
    }
)
AMBIENT_NON_SKILL_CODE_NAMES = frozenset(
    {
        "clean",
        "crucial",
        "fabric",
        "flagship",
        "routine",
        "scout",
        "substantial",
        "terminal",
        "workhorse",
    }
)


def instance_root_for(home: Path) -> Path:
    """Where `install-harness` seeds the instance under a scratch HOME."""
    return home / ".agents"


def run(platform: str, home: Path, *arguments: str, **extra_env):
    env = os.environ.copy()
    env.update({"HOME": str(home)})
    # Keep the instance root deterministic in the scratch HOME. AGENTS_HOME now
    # names only the product root, but an explicit instance value also keeps the
    # test independent of any caller-provided instance selection.
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(instance_root_for(home))
    env["PROVENANT_ALLOW_LINKED_WORKTREE_INSTALL"] = "1"
    env.update(extra_env)
    return subprocess.run(
        [str(SCRIPT), "--platform", platform, *arguments],
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def copy_product(root: Path, destination: Path) -> Path:
    shutil.copytree(
        root,
        destination,
        symlinks=True,
        ignore=shutil.ignore_patterns(".git", ".agent-run", "node_modules", ".venv"),
    )
    return destination


def run_product(product: Path, platform: str, home: Path, **extra_env):
    environment = os.environ.copy()
    # Resolve the copied product through the pointer written by this install,
    # not through an explicit product root inherited from the outer CI job.
    environment.pop("AGENT_FABRIC_PRODUCT_ROOT", None)
    environment.update(
        {
            "HOME": str(home / "home"),
            "AGENT_FABRIC_INSTANCE_ROOT": str(home / "instance"),
            "HARNESS_PYTHON": sys.executable,
            "PROVENANT_BIN_DIR": str(home / "bin"),
            "CLAUDE_CONFIG_DIR": str(home / "claude"),
            "CODEX_HOME": str(home / "codex"),
            "PATH": f"{home / 'bin'}{os.pathsep}{os.environ['PATH']}",
        }
    )
    environment.update(extra_env)
    return subprocess.run(
        [str(product / "scripts/install-harness"), "--platform", platform],
        cwd=product,
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )


def test_install_harness_requires_acknowledgement_for_a_linked_worktree(tmp_path):
    product = tmp_path / "product"
    scripts = product / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SCRIPT, scripts / "install-harness")
    (product / ".git").write_text("gitdir: /canonical/.git/worktrees/issue-549\n")
    command = tmp_path / "bin/install-harness"
    command.parent.mkdir()
    command.symlink_to(scripts / "install-harness")

    env = os.environ.copy()
    env.update({
        "HOME": str(tmp_path / "home"),
        "PROVENANT_BIN_DIR": str(tmp_path / "bin-out"),
    })
    env.pop("PROVENANT_ALLOW_LINKED_WORKTREE_INSTALL", None)
    result = subprocess.run(
        [str(command), "--platform", "claude"],
        cwd=tmp_path,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 3
    assert "linked worktree" in result.stderr


def run_workflow_installer(target: Path):
    return subprocess.run(
        [str(WORKFLOW_SCRIPT), "--target", str(target)],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def expected_skills():
    return {path.parent.name for path in (ROOT / "skills").glob("*/SKILL.md")}


def expected_installed_entries():
    """A per-entry layout carries the skills plus the shared library they import."""
    return expected_skills() | {"_shared"}


def _ambient_skill_names(texts, available):
    code_words = set()
    singleton_code_names = set()
    for text in texts:
        for code_span in re.findall(r"`([^`]+)`", text):
            code_words.update(re.findall(r"[a-z][a-z0-9-]*", code_span))
            if re.fullmatch(r"[a-z][a-z0-9-]*", code_span):
                singleton_code_names.add(code_span)

    unresolved = singleton_code_names - available - AMBIENT_NON_SKILL_CODE_NAMES
    assert not unresolved, (
        f"ambient files reference unknown skill name(s): {sorted(unresolved)}"
    )
    names = code_words & available
    assert names == EXPECTED_AMBIENT_SKILL_NAMES, (
        f"ambient skill-name contract drifted: expected "
        f"{sorted(EXPECTED_AMBIENT_SKILL_NAMES)}, found {sorted(names)}"
    )
    return names


def ambient_skill_names_and_resolver_roots():
    available = expected_skills()
    texts = []
    resolver_roots = set()
    for ambient in (ROOT / "AGENTS.md", ROOT / "HARNESS.md"):
        text = ambient.read_text()
        texts.append(text)
        roots = re.findall(r"`(~/\.(?:claude|codex)/skills/)`", text)
        # D12 amendment: HARNESS.md is the sole home of the resolver line. AGENTS.md
        # must not restate it - both harnesses already discover skills through their
        # own installed skills directory. Skills ship in the product checkout and are
        # linked into those platform homes; the thin instance root holds none, so
        # `.agents/skills/` must not appear in either ambient file.
        expected = ["~/.claude/skills/", "~/.codex/skills/"] if ambient.name == "HARNESS.md" else []
        assert roots == expected, (
            f"{ambient.name} must state {len(expected)} D12 resolver root(s)"
        )
        assert ".agents/skills/" not in text, (
            f"{ambient.name} names an instance skills root that is never installed"
        )
        resolver_roots.update(roots)
    assert resolver_roots == {"~/.claude/skills/", "~/.codex/skills/"}
    return _ambient_skill_names(texts, available), sorted(resolver_roots)


def test_installs_claude_skills_and_global_instructions_idempotently(tmp_path):
    config = tmp_path / "claude-config"
    bin_dir = tmp_path / "custom-bin"
    first = run(
        "claude",
        tmp_path,
        CLAUDE_CONFIG_DIR=str(config),
        PROVENANT_BIN_DIR=str(bin_dir),
        PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
    )
    assert first.returncode == 0, first.stderr
    command = bin_dir / "provenant"
    assert command.is_file()
    assert not command.is_symlink()
    assert command.read_bytes() == PROVENANT_TEMPLATE.read_bytes()
    assert json.loads((tmp_path / ".agents/.agent-fabric/product-root.json").read_text()) == {
        "product_root": str(ROOT),
        "schema_version": 1,
    }
    assert {path.name for path in (config / "skills").iterdir()} == expected_installed_entries()
    workflows = config / "workflows"
    assert {path.name for path in workflows.iterdir()} == WORKFLOW_NAMES
    for name in WORKFLOW_NAMES:
        assert (workflows / name).is_symlink()
        assert (workflows / name).resolve() == ROOT / "workflows" / name
    workflow_manifest = json.loads(
        (config / ".agent-harness-workflows-installation.json").read_text()
    )
    assert set(workflow_manifest["managed"]) == WORKFLOW_NAMES
    agents = config / "agents"
    assert {path.name for path in agents.iterdir()} == AGENT_NAMES
    agent_manifest_path = config / ".agent-harness-agents-installation.json"
    agent_manifest_before = agent_manifest_path.read_bytes()
    agent_manifest = json.loads(agent_manifest_before)
    assert set(agent_manifest["managed"]) == AGENT_NAMES
    for name in AGENT_NAMES:
        installed = agents / name
        source = ROOT / "agents" / name
        assert installed.is_symlink()
        assert installed.resolve() == source
        assert installed.read_bytes() == source.read_bytes()
    instructions = config / "CLAUDE.md"
    content = instructions.read_text()
    # Doctrine is read from the seeded instance copy; the harness constitution
    # stays product-shipped (ADR 0019).
    assert str(instance_root_for(tmp_path) / "AGENTS.md") in content
    assert str(ROOT / "HARNESS.md") in content
    registration = json.loads((tmp_path / ".claude.json").read_text())["mcpServers"]["fabric"]
    assert registration["command"] == str(command)
    assert registration["env"] == {
        "AGENT_FABRIC_CLIENT_LABEL": "claude",
        "AGENT_FABRIC_SEAT": "claude",
        "AGENT_FABRIC_STATE_DIRECTORY": str(tmp_path / ".local/state/agent-harness/fabric"),
    }

    second = run(
        "claude",
        tmp_path,
        CLAUDE_CONFIG_DIR=str(config),
        PROVENANT_BIN_DIR=str(bin_dir),
        PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
    )
    assert second.returncode == 0, second.stderr
    assert f"instructions existing={instructions}" in second.stdout
    assert agent_manifest_path.read_bytes() == agent_manifest_before
    assert all(
        (agents / name).read_bytes() == (ROOT / "agents" / name).read_bytes()
        for name in AGENT_NAMES
    )


def test_installs_codex_skills_and_global_instructions(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()
    codex_config = config / "config.toml"
    codex_config.write_text("[custom]\nvalue = 'preserved'\n")
    result = run("codex", tmp_path, CODEX_HOME=str(config))
    assert result.returncode == 0, result.stderr
    assert {path.name for path in (config / "skills").iterdir()} == expected_installed_entries()
    assert not (tmp_path / ".claude" / "workflows").exists()
    assert not (config / "workflows").exists()
    assert not (config / ".agent-harness-workflows-installation.json").exists()
    assert str(ROOT / "HARNESS.md") in (config / "AGENTS.md").read_text()
    configured = codex_config.read_text()
    assert "[custom]\nvalue = 'preserved'" in configured
    assert configured.count('name = "skill-creator"') == 1
    assert "enabled = false" in configured
    registration = tomllib.loads(configured)["mcp_servers"]["fabric"]
    assert registration == {
        "command": str(tmp_path / ".local/bin/provenant"),
        "env": {
            "AGENT_FABRIC_CLIENT_LABEL": "codex",
            "AGENT_FABRIC_SEAT": "codex",
            "AGENT_FABRIC_STATE_DIRECTORY": str(tmp_path / ".local/state/agent-harness/fabric"),
        },
    }

    second = run("codex", tmp_path, CODEX_HOME=str(config))
    assert second.returncode == 0, second.stderr
    assert codex_config.read_text() == configured


def test_late_instruction_conflict_leaves_existing_product_pointer_unchanged(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    instructions = config / "CLAUDE.md"
    instructions.write_bytes(UNMANAGED_BYTES)
    instance = instance_root_for(tmp_path)
    pointer = instance / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    original = '{"product_root": "/old/product", "schema_version": 1}\n'
    pointer.write_text(original)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert pointer.read_text() == original
    assert not (config / "skills").exists()


def test_post_seed_failure_leaves_existing_product_pointer_unchanged(tmp_path):
    product = tmp_path / "product with spaces"
    shutil.copytree(
        ROOT,
        product,
        symlinks=True,
        ignore=shutil.ignore_patterns(".git", ".agent-run", "node_modules", ".venv"),
    )
    failing_skills = product / "scripts/install-skills"
    failing_skills.write_text("#!/usr/bin/env bash\nexit 7\n")
    failing_skills.chmod(0o755)
    instance = instance_root_for(tmp_path)
    pointer = instance / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    original = '{"product_root": "/old/product", "schema_version": 1}\n'
    pointer.write_text(original)
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(tmp_path / "home"),
            "AGENT_FABRIC_INSTANCE_ROOT": str(instance),
            "HARNESS_PYTHON": sys.executable,
            "PROVENANT_ALLOW_LINKED_WORKTREE_INSTALL": "1",
            "PROVENANT_BIN_DIR": str(tmp_path / "bin"),
            "CODEX_HOME": str(tmp_path / "codex"),
        }
    )

    result = subprocess.run(
        [str(product / "scripts/install-harness"), "--platform", "codex"],
        cwd=product,
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 7
    assert pointer.read_text() == original
    assert (instance / "AGENTS.md").is_file(), "the forced failure must occur after seeding"
    assert result.stdout.count("product root pointer updated=") == 0
    source = (product / "scripts/install-harness").read_text()
    assert source.count("write-product-root-pointer.py") == 1
    assert 'instance_installation.py" seed' in source
    assert "--no-pointer" in source


def test_refuses_receiptless_noncanonical_skill_directory_link_before_pointer(tmp_path):
    config = tmp_path / "claude-config"
    old_product_skills = tmp_path / "old-product/skills"
    old_product_skills.mkdir(parents=True)
    config.mkdir()
    (config / "skills").symlink_to(old_product_skills, target_is_directory=True)
    instance = instance_root_for(tmp_path)
    pointer = instance / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    original = '{"product_root": "/old/product", "schema_version": 1}\n'
    pointer.write_text(original)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert "non-canonical symlink" in result.stderr
    assert pointer.read_text() == original
    assert (config / "skills").is_symlink()


def test_refuses_receiptless_skill_entry_before_pointer(tmp_path):
    config = tmp_path / "codex-home"
    foreign_skill = tmp_path / "old-product/skills/scope"
    foreign_skill.mkdir(parents=True)
    (foreign_skill / "SKILL.md").write_text("---\nname: scope\ndescription: old\n---\n")
    (config / "skills").mkdir(parents=True)
    (config / "skills/scope").symlink_to(foreign_skill, target_is_directory=True)
    instance = instance_root_for(tmp_path)
    pointer = instance / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    original = '{"product_root": "/old/product", "schema_version": 1}\n'
    pointer.write_text(original)

    result = run("codex", tmp_path, CODEX_HOME=str(config))

    assert result.returncode == 3
    assert "conflicting managed targets changed outside harness" in result.stderr
    assert pointer.read_text() == original
    assert (config / "skills/scope").is_symlink()
    assert not (config / "AGENTS.md").exists()


def test_refuses_receiptless_workflow_entry_before_pointer(tmp_path):
    config = tmp_path / "claude-config"
    foreign_workflow = tmp_path / "old-product/workflows/codebase-polish.js"
    foreign_workflow.parent.mkdir(parents=True)
    foreign_workflow.write_text("// old workflow\n")
    (config / "workflows").mkdir(parents=True)
    (config / "workflows/codebase-polish.js").symlink_to(foreign_workflow)
    instance = instance_root_for(tmp_path)
    pointer = instance / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    original = '{"product_root": "/old/product", "schema_version": 1}\n'
    pointer.write_text(original)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert "conflicting managed targets" in result.stderr
    assert pointer.read_text() == original
    assert (config / "workflows/codebase-polish.js").is_symlink()
    assert not (config / "AGENTS.md").exists()


def test_refuses_self_targeting_pointer_directory_before_seed(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()
    instance = instance_root_for(tmp_path)
    instance.mkdir()
    pointer = instance / "product-root.json"
    original = '{"product_root": "/old/product", "schema_version": 1}\n'
    pointer.write_text(original)
    (instance / ".agent-fabric").symlink_to(instance, target_is_directory=True)

    result = run("codex", tmp_path, CODEX_HOME=str(config))

    assert result.returncode == 3
    assert "must not be a symlink" in result.stderr
    assert pointer.read_text() == original
    assert not (instance / "AGENTS.md").exists()
    assert not (instance / ".gitignore").exists()


def test_accepts_canonical_directory_links_for_claude_surfaces(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    (config / "skills").symlink_to(ROOT / "skills", target_is_directory=True)
    (config / "agents").symlink_to(ROOT / "agents", target_is_directory=True)
    (config / "workflows").symlink_to(ROOT / "workflows", target_is_directory=True)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert (config / "skills").resolve() == (ROOT / "skills").resolve()
    assert (config / "agents").resolve() == (ROOT / "agents").resolve()
    assert (config / "workflows").resolve() == (ROOT / "workflows").resolve()


def test_claude_subagent_conflict_fails_before_harness_mutation(tmp_path):
    config = tmp_path / "claude-config"
    agents = config / "agents"
    agents.mkdir(parents=True)
    unmanaged = agents / "codex-analyst.md"
    original = b"# User-owned definition\n"
    unmanaged.write_bytes(original)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert "conflicting agent targets" in result.stderr
    assert unmanaged.read_bytes() == original
    assert not unmanaged.is_symlink()
    assert not (config / "skills").exists()
    assert not (config / "workflows").exists()
    assert not (config / ".agent-harness-workflows-installation.json").exists()


def test_codex_install_projects_instance_custom_skill_without_managed_ownership(
    tmp_path,
):
    config = tmp_path / "codex-home"
    custom_skill = instance_root_for(tmp_path) / "custom-skills" / "local-skill"
    custom_skill.mkdir(parents=True)
    (custom_skill / "SKILL.md").write_text(
        "---\nname: local-skill\ndescription: Instance-owned test skill.\n---\n"
    )

    result = run("codex", tmp_path, CODEX_HOME=str(config))

    assert result.returncode == 0, result.stderr
    installed = config / "skills" / "local-skill"
    assert installed.is_symlink()
    assert installed.resolve() == custom_skill.resolve()
    manifest = json.loads(
        (config / ".agent-harness-installation.json").read_text()
    )
    assert "local-skill" not in manifest["managed"]
    assert manifest["custom"]["local-skill"] == {
        "source_target": str(custom_skill.resolve())
    }


def test_claude_workflow_upgrade_relinks_a_previously_managed_file(tmp_path):
    config = tmp_path / "claude-config"
    first = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))
    assert first.returncode == 0, first.stderr

    name = "cross-verify.js"
    previous_source = tmp_path / "previous-checkout" / "workflows" / name
    previous_source.parent.mkdir(parents=True)
    previous_source.write_bytes((ROOT / "workflows" / name).read_bytes())
    destination = config / "workflows" / name
    destination.unlink()
    destination.symlink_to(previous_source)
    manifest_path = config / ".agent-harness-workflows-installation.json"
    manifest = json.loads(manifest_path.read_text())
    manifest["managed"][name]["source_target"] = str(previous_source)
    manifest_path.write_text(json.dumps(manifest))

    upgraded = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert upgraded.returncode == 0, upgraded.stderr
    assert destination.is_symlink()
    assert destination.resolve() == ROOT / "workflows" / name
    installed = json.loads(manifest_path.read_text())
    assert installed["managed"][name]["source_target"] == str(
        ROOT / "workflows" / name
    )


def test_claude_workflow_install_preserves_an_unmanaged_file_byte_identically(
    tmp_path,
):
    config = tmp_path / "claude-config"
    workflows = config / "workflows"
    workflows.mkdir(parents=True)
    name = "codebase-polish.js"
    unmanaged = workflows / name
    unmanaged.write_bytes(UNMANAGED_WORKFLOW_BYTES)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert unmanaged.read_bytes() == UNMANAGED_WORKFLOW_BYTES
    assert not unmanaged.is_symlink()
    assert "codebase-polish.js" in result.stderr
    assert not (config / ".agent-harness-workflows-installation.json").exists()
    for managed_name in WORKFLOW_NAMES - {name}:
        assert not (workflows / managed_name).exists()


@pytest.mark.parametrize("kind", ["copy", "symlink"])
def test_claude_workflow_install_rejects_an_equivalent_unmanaged_file(
    tmp_path, kind
):
    config = tmp_path / "claude-config"
    workflows = config / "workflows"
    workflows.mkdir(parents=True)
    name = "codebase-polish.js"
    unmanaged = workflows / name
    source = ROOT / "workflows" / name
    if kind == "copy":
        unmanaged.write_bytes(source.read_bytes())
    else:
        unmanaged.symlink_to(source)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    if kind == "copy":
        assert unmanaged.read_bytes() == source.read_bytes()
        assert not unmanaged.is_symlink()
    else:
        assert unmanaged.is_symlink()
        assert unmanaged.resolve() == source
    assert "codebase-polish.js" in result.stderr
    assert not (config / ".agent-harness-workflows-installation.json").exists()


def test_claude_workflow_install_rejects_a_foreign_broken_symlink_at_a_managed_path(
    tmp_path,
):
    # A managed link replaced by a foreign symlink that resolves to neither the
    # current nor the recorded source is foreign tampering, not a repairable
    # managed link, even though a broken symlink reports exists()==False. It must
    # conflict (exit 3) and leave every workflow target unmutated.
    config = tmp_path / "claude-config"
    first = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))
    assert first.returncode == 0, first.stderr

    name = "cross-verify.js"
    destination = config / "workflows" / name
    foreign_target = tmp_path / "foreign" / "missing.js"  # never created: broken
    destination.unlink()
    destination.symlink_to(foreign_target)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert name in result.stderr
    # Zero mutation: the foreign broken symlink is preserved, not relinked.
    assert destination.is_symlink()
    assert os.readlink(destination) == str(foreign_target)
    assert not destination.exists()


def test_workflow_install_does_not_publish_links_when_ownership_write_fails(tmp_path):
    config = tmp_path / "claude-config"
    target = config / "workflows"
    target.mkdir(parents=True)
    config.chmod(0o500)
    try:
        failed = run_workflow_installer(target)
    finally:
        config.chmod(0o700)

    assert failed.returncode == 3
    assert not any((target / name).exists() for name in WORKFLOW_NAMES)

    retried = run_workflow_installer(target)
    assert retried.returncode == 0, retried.stderr
    assert all((target / name).is_symlink() for name in WORKFLOW_NAMES)


def test_workflow_install_recovers_after_interruption_during_link_publication(
    tmp_path,
):
    config = tmp_path / "claude-config"
    target = config / "workflows"
    target.mkdir(parents=True)
    interrupt = subprocess.run(
        [
            sys.executable,
            "-c",
            "\n".join(
                (
                    "import os, runpy",
                    "from pathlib import Path",
                    f"module = runpy.run_path({str(WORKFLOW_SCRIPT)!r}, run_name='interrupt_test')",
                    "publish = module['_replace_link']",
                    "def interrupt(destination, source):",
                    "    publish(destination, source)",
                    "    os._exit(99)",
                    "module['install'].__globals__['_replace_link'] = interrupt",
                    f"module['install'](Path({str(target)!r}))",
                )
            ),
        ],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert interrupt.returncode == 99
    manifest_path = config / ".agent-harness-workflows-installation.json"
    assert manifest_path.is_file()
    interrupted_manifest = json.loads(manifest_path.read_text())
    assert set(interrupted_manifest["managed"]) == WORKFLOW_NAMES

    retried = run_workflow_installer(target)
    assert retried.returncode == 0, retried.stderr
    assert all((target / name).is_symlink() for name in WORKFLOW_NAMES)
    assert set(json.loads(manifest_path.read_text())["managed"]) == WORKFLOW_NAMES


def test_workflow_installer_preserves_a_directory_link_to_canonical_sources(
    tmp_path,
):
    fixture_root = tmp_path / "agents"
    scripts = fixture_root / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(WORKFLOW_SCRIPT, scripts / "install-workflows")
    shutil.copytree(ROOT / "workflows", fixture_root / "workflows")
    platform_home = tmp_path / "claude"
    platform_home.mkdir()
    target = platform_home / "workflows"
    target.symlink_to(fixture_root / "workflows", target_is_directory=True)

    result = subprocess.run(
        [str(scripts / "install-workflows"), "--target", str(target)],
        cwd=fixture_root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert target.is_symlink()
    assert target.resolve() == fixture_root / "workflows"
    assert "workflows existing=directory-link" in result.stdout
    assert not (
        fixture_root / ".agent-harness-workflows-installation.json"
    ).exists()
    assert not (
        platform_home / ".agent-harness-workflows-installation.json"
    ).exists()


def test_ambient_skill_name_extraction_rejects_unknown_explicit_skill():
    ambient = "\n".join(
        (ROOT / name).read_text() for name in ("AGENTS.md", "HARNESS.md")
    )
    for unknown_reference in (
        "Use the `phantom` skill.",
        "Use `phantom` for context.",
    ):
        with pytest.raises(AssertionError, match=r"unknown skill name.*phantom"):
            _ambient_skill_names([f"{ambient}\n{unknown_reference}\n"], expected_skills())


@pytest.mark.parametrize(
    "platform, config_name, variable",
    (
        ("claude", ".claude", "CLAUDE_CONFIG_DIR"),
        ("codex", ".codex", "CODEX_HOME"),
    ),
)
def test_ambient_skill_names_resolve_on_both_installed_platform_layouts(
    tmp_path, platform, config_name, variable
):
    """AC-P3: ambient skill names resolve through each static install layout."""
    names, resolver_templates = ambient_skill_names_and_resolver_roots()
    assert names == EXPECTED_AMBIENT_SKILL_NAMES

    home = tmp_path / platform
    home.mkdir()
    # Model a fused layout, so the isolated install's actual source tree stays
    # immutable while the instance root still resolves.
    (home / ".agents").symlink_to(ROOT, target_is_directory=True)
    config = home / config_name
    result = run(platform, home, **{variable: str(config)})
    assert result.returncode == 0, result.stderr

    installed_root = config / "skills"
    installed_names = {path.name for path in installed_root.iterdir()}
    assert installed_names == expected_installed_entries()
    # The resolver root for this platform is the one HARNESS.md names for it, and
    # it must be exactly where install-harness placed the managed links.
    resolver_template = next(
        template for template in resolver_templates if f"/{config_name}/" in template
    )
    resolver_root = Path(resolver_template.replace("~", str(home), 1))
    assert resolver_root.resolve() == installed_root.resolve()
    installed_source_roots = {
        (installed_root / name / "SKILL.md").resolve().parents[1] for name in names
    }
    # Every installed entry is a managed link back to the one product checkout.
    assert installed_source_roots == {(ROOT / "skills").resolve()}
    for name in names:
        resolved = resolver_root / name / "SKILL.md"
        assert resolved.is_file(), f"resolver root cannot find skills/{name}/SKILL.md"
        assert resolved.resolve() == (ROOT / "skills" / name / "SKILL.md").resolve(), (
            f"{platform} resolver root disagrees with the product checkout for {name}"
        )


def test_all_mcp_clients_are_an_explicit_subscription_native_opt_in(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()

    result = run("codex", tmp_path, "--mcp-clients", "all", CODEX_HOME=str(config))

    assert result.returncode == 0, result.stderr
    # Brokers sit in the codex seat; Agy holds the `agy` seat for stable
    # addressing, not model-family proof. See CLIENT_SEATS in
    # scripts/configure-fabric-mcp.py.
    for client, path, seat in (
        ("cursor", tmp_path / ".cursor/mcp.json", "codex"),
        ("agy", tmp_path / ".gemini/config/mcp_config.json", "agy"),
        ("kiro", tmp_path / ".kiro/settings/mcp.json", "codex"),
    ):
        registration = json.loads(path.read_text())["mcpServers"]["fabric"]
        assert registration["env"]["AGENT_FABRIC_SEAT"] == seat
        assert registration["env"]["AGENT_FABRIC_CLIENT_LABEL"] == client
        assert "AGENT_FABRIC_PROJECT_PATH" not in registration["env"]
    opencode = json.loads((tmp_path / ".config/opencode/opencode.jsonc").read_text())
    registration = opencode["mcp"]["fabric"]
    assert registration["command"] == [str(tmp_path / ".local/bin/provenant")]
    assert registration["environment"]["AGENT_FABRIC_SEAT"] == "codex"
    assert registration["environment"]["AGENT_FABRIC_CLIENT_LABEL"] == "opencode"
    assert all("API_KEY" not in key for key in registration["environment"])


def test_primary_mcp_clients_remain_the_default(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()

    result = run("codex", tmp_path, CODEX_HOME=str(config))

    assert result.returncode == 0, result.stderr
    assert not (tmp_path / ".cursor/mcp.json").exists()
    assert not (tmp_path / ".gemini/config/mcp_config.json").exists()
    assert not (tmp_path / ".kiro/settings/mcp.json").exists()
    assert not (tmp_path / ".config/opencode/opencode.jsonc").exists()


def test_rejects_unknown_mcp_client_selection(tmp_path):
    result = run("codex", tmp_path, "--mcp-clients", "optional")
    assert result.returncode == 2
    assert "--mcp-clients <primary|all>" in result.stderr


def test_codex_skill_override_conflict_fails_without_rewriting_config(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()
    codex_config = config / "config.toml"
    original = '[[skills.config]]\nname = "skill-creator"\nenabled = true\n'
    codex_config.write_text(original)

    result = run("codex", tmp_path, CODEX_HOME=str(config))
    assert result.returncode == 3
    assert "conflicting" in result.stderr
    assert codex_config.read_text() == original
    assert not (config / "skills").exists()


def test_codex_inline_skill_config_fails_closed_without_invalid_rewrite(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()
    codex_config = config / "config.toml"
    original = '[skills]\nconfig = [{name = "other", enabled = true}]\n'
    codex_config.write_text(original)

    result = run("codex", tmp_path, CODEX_HOME=str(config))
    assert result.returncode == 3
    assert "invalid TOML" in result.stderr
    assert codex_config.read_text() == original
    assert not (config / "skills").exists()


def test_codex_skill_override_preserves_symlinked_config(tmp_path):
    config = tmp_path / "codex-home"
    target_dir = tmp_path / "dotfiles"
    config.mkdir()
    target_dir.mkdir()
    target = target_dir / "codex.toml"
    target.write_text("[custom]\nvalue = 'preserved'\n")
    codex_config = config / "config.toml"
    codex_config.symlink_to(target)

    result = run("codex", tmp_path, CODEX_HOME=str(config))
    assert result.returncode == 0, result.stderr
    assert codex_config.is_symlink()
    assert target.read_text().count('name = "skill-creator"') == 1
    assert {path.name for path in (config / "skills").iterdir()} == expected_installed_entries()


# A discriminating payload: CRLF line endings and no trailing newline. Any
# text-mode rewrite (LF<->CRLF normalisation, appended newline) changes the
# bytes, so a byte comparison — not read_text() — is what proves preservation.
UNMANAGED_BYTES = b"# My existing instructions\r\nsecond line, no trailing newline"


def test_preserves_existing_instructions_and_prints_merge_line(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    instructions = config / "CLAUDE.md"
    instructions.write_bytes(UNMANAGED_BYTES)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))
    assert result.returncode == 3
    # Unmanaged instructions are preserved byte-for-byte; the merge line names
    # both ambient files (AC-P2 existing-unmanaged arm).
    assert instructions.read_bytes() == UNMANAGED_BYTES
    assert "instructions preserved=" in result.stderr
    assert str(instance_root_for(tmp_path) / "AGENTS.md") in result.stderr
    assert str(ROOT / "HARNESS.md") in result.stderr
    assert not (config / "skills").exists()


def test_preserves_existing_codex_instructions_and_prints_merge_line(tmp_path):
    # AC-P2: the codex platform layout ($CODEX_HOME/AGENTS.md) must fail closed
    # over an existing unmanaged instructions file exactly like claude does —
    # exit 3, byte-identical preservation, merge line naming both ambient files.
    config = tmp_path / "codex-home"
    config.mkdir()
    instructions = config / "AGENTS.md"
    instructions.write_bytes(UNMANAGED_BYTES)

    result = run("codex", tmp_path, CODEX_HOME=str(config))
    assert result.returncode == 3
    assert instructions.read_bytes() == UNMANAGED_BYTES
    assert "instructions preserved=" in result.stderr
    assert str(instance_root_for(tmp_path) / "AGENTS.md") in result.stderr
    assert str(ROOT / "HARNESS.md") in result.stderr
    assert not (config / "skills").exists()


def test_accepts_claude_instruction_symlink_to_canonical_agents_file(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    instance_agents = tmp_path / ".agents/AGENTS.md"
    instance_agents.parent.mkdir()
    instance_agents.write_text("# Instance instructions\n")
    instructions = config / "CLAUDE.md"
    instructions.symlink_to(instance_agents)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert instructions.is_symlink()
    assert instructions.resolve() == instance_agents
    assert f"instructions existing={instructions}" in result.stdout
    assert "add this line" not in result.stderr


def test_accepts_codex_instruction_symlink_to_canonical_agents_file(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()
    instance_agents = tmp_path / ".agents/AGENTS.md"
    instance_agents.parent.mkdir()
    instance_agents.write_text("# Instance instructions\n")
    instructions = config / "AGENTS.md"
    instructions.symlink_to(instance_agents)

    result = run("codex", tmp_path, CODEX_HOME=str(config))

    assert result.returncode == 0, result.stderr
    assert instructions.is_symlink()
    assert instructions.resolve() == instance_agents
    assert f"instructions existing={instructions}" in result.stdout
    assert "add this line" not in result.stderr


def test_rejects_instruction_symlink_to_foreign_file(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    foreign = tmp_path / "foreign-instructions.md"
    foreign.write_text("# Foreign instructions\n")
    instructions = config / "CLAUDE.md"
    instructions.symlink_to(foreign)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert instructions.is_symlink()
    assert instructions.resolve() == foreign
    assert foreign.read_text() == "# Foreign instructions\n"
    assert "restore the file or link it to" in result.stderr


def test_requires_supported_platform(tmp_path):
    result = run("other", tmp_path)
    assert result.returncode == 2
    assert "usage:" in result.stderr


def test_refuses_provenant_command_collision_before_any_mutation(tmp_path):
    config = tmp_path / "claude-config"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    collision = bin_dir / "provenant"
    collision.write_text("user-owned\n")

    result = run(
        "claude",
        tmp_path,
        CLAUDE_CONFIG_DIR=str(config),
        PROVENANT_BIN_DIR=str(bin_dir),
    )

    assert result.returncode == 3
    assert "collision" in result.stderr
    assert collision.read_text() == "user-owned\n"
    assert not config.exists()
    assert not (tmp_path / ".claude.json").exists()


def test_skill_source_collision_preflights_before_harness_mutation(tmp_path):
    config = tmp_path / "codex-home"
    bin_dir = tmp_path / "bin"
    custom_scope = instance_root_for(tmp_path) / "custom-skills" / "scope"
    custom_scope.mkdir(parents=True)
    (custom_scope / "SKILL.md").write_text(
        "---\nname: scope\ndescription: Colliding instance skill.\n---\n"
    )

    result = run(
        "codex",
        tmp_path,
        CODEX_HOME=str(config),
        PROVENANT_BIN_DIR=str(bin_dir),
    )

    assert result.returncode == 3
    assert str((ROOT / "skills" / "scope").resolve()) in result.stderr
    assert str(custom_scope.resolve()) in result.stderr
    assert not (bin_dir / "provenant").exists()
    assert not (
        instance_root_for(tmp_path) / ".agent-fabric" / "product-root.json"
    ).exists()
    assert not (instance_root_for(tmp_path) / "AGENTS.md").exists()
    assert not (config / "skills").exists()
    assert not (config / ".agent-harness-installation.json").exists()


def test_rejects_a_relative_provenant_bin_directory_before_mutation(tmp_path):
    relative_bin = "relative-provenant-bin"

    result = run("codex", tmp_path, PROVENANT_BIN_DIR=relative_bin)

    assert result.returncode == 3
    assert "PROVENANT_BIN_DIR must be absolute" in result.stderr
    assert not (ROOT / relative_bin).exists()
    assert not (tmp_path / ".codex").exists()


def test_rejects_a_provenant_bin_directory_inside_the_product(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    source = product / "scripts/provenant"
    original = source.read_bytes()

    result = run_product(
        product,
        "codex",
        tmp_path,
        PROVENANT_BIN_DIR=str(product / "scripts"),
    )

    assert result.returncode == 3
    assert "must be outside the product checkout" in result.stderr
    assert source.read_bytes() == original
    assert not (tmp_path / "instance/.agent-fabric/product-root.json").exists()


def test_rejects_a_missing_bin_directory_under_a_symlinked_product_parent(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    alias = tmp_path / "product-alias"
    alias.symlink_to(product, target_is_directory=True)

    result = run_product(
        product,
        "codex",
        tmp_path,
        PROVENANT_BIN_DIR=str(alias / "new-bin"),
    )

    assert result.returncode == 3
    assert "resolves inside the product checkout" in result.stderr
    assert not (product / "new-bin").exists()
    assert not (tmp_path / "instance/.agent-fabric/product-root.json").exists()


def test_updates_a_managed_provenant_file_to_the_current_template(tmp_path):
    bin_dir = tmp_path / "custom-bin"
    bin_dir.mkdir()
    command = bin_dir / "provenant"
    command.write_bytes(PROVENANT_TEMPLATE.read_bytes() + b"\n# stale managed copy\n")

    result = run(
        "codex",
        tmp_path,
        PROVENANT_BIN_DIR=str(bin_dir),
    )

    assert result.returncode == 0, result.stderr
    assert command.is_file()
    assert not command.is_symlink()
    assert command.read_bytes() == PROVENANT_TEMPLATE.read_bytes()
    assert f"command updated={command}" in result.stdout


def test_rejects_a_byte_identical_foreign_symlink_without_clobbering_it(tmp_path):
    bin_dir = tmp_path / ".local/bin"
    bin_dir.mkdir(parents=True)
    foreign = tmp_path / "foreign/provenant"
    foreign.parent.mkdir()
    shutil.copy2(PROVENANT_TEMPLATE, foreign)
    command = bin_dir / "provenant"
    command.symlink_to(foreign)
    original_target = command.readlink()

    result = run("codex", tmp_path)

    assert result.returncode == 3
    assert "collision" in result.stderr
    assert command.is_symlink()
    assert command.readlink() == original_target
    assert foreign.read_bytes() == PROVENANT_TEMPLATE.read_bytes()


def test_warns_when_provenant_bin_directory_is_outside_path(tmp_path):
    config = tmp_path / "claude-config"
    bin_dir = tmp_path / "not-on-path"

    result = run(
        "claude",
        tmp_path,
        CLAUDE_CONFIG_DIR=str(config),
        PROVENANT_BIN_DIR=str(bin_dir),
        PATH=os.environ["PATH"],
    )

    assert result.returncode == 0, result.stderr
    assert f"warning: {bin_dir} is not on PATH" in result.stderr
    command = bin_dir / "provenant"
    assert command.is_file()
    assert not command.is_symlink()
    assert command.read_bytes() == PROVENANT_TEMPLATE.read_bytes()
    assert not (tmp_path / ".zshrc").exists()
    assert not (tmp_path / ".bashrc").exists()


def test_split_install_points_client_instructions_at_the_instance_agents_md(tmp_path):
    """AGENTS.md is instance-owned once seeded, so the client must read that copy.

    Binding the bootstrap text to the product copy would mean an instance edit
    never reaches Claude or Codex, which is the failure the ownership split
    exists to prevent (ADR 0019). HARNESS.md stays product-shipped.
    """
    config = tmp_path / "claude-config"
    bin_dir = tmp_path / "custom-bin"
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    result = run(
        "claude",
        tmp_path,
        CLAUDE_CONFIG_DIR=str(config),
        PROVENANT_BIN_DIR=str(bin_dir),
        PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
        AGENT_FABRIC_INSTANCE_ROOT=str(instance_root),
    )

    assert result.returncode == 0, result.stderr
    seeded = instance_root / "AGENTS.md"
    assert seeded.is_file(), "the split instance should have been seeded"
    assert seeded.read_text() == (ROOT / "AGENTS.md").read_text()

    content = (config / "CLAUDE.md").read_text()
    assert str(seeded) in content
    assert str(ROOT / "HARNESS.md") in content
    # The product copy is not what the client is told to read.
    assert str(ROOT / "AGENTS.md") not in content

    # An instance edit reaches the client, because the client reads that file.
    seeded.write_text("# My own doctrine\n")
    assert (config / "CLAUDE.md").read_text() == content
    assert seeded.read_text() == "# My own doctrine\n"


def test_split_install_is_idempotent_over_its_own_instructions(tmp_path):
    config = tmp_path / "claude-config"
    bin_dir = tmp_path / "custom-bin"
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    environment = {
        "CLAUDE_CONFIG_DIR": str(config),
        "PROVENANT_BIN_DIR": str(bin_dir),
        "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
        "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
    }

    first = run("claude", tmp_path, **environment)
    assert first.returncode == 0, first.stderr
    second = run("claude", tmp_path, **environment)

    assert second.returncode == 0, second.stderr
    instructions = config / "CLAUDE.md"
    assert f"instructions existing={instructions}" in second.stdout


def test_split_install_rebinds_its_bootstrap_when_instance_root_changes(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    first_instance = tmp_path / "instance-one"
    second_instance = tmp_path / "instance-two"

    first = run_product(
        product, "codex", tmp_path,
        AGENT_FABRIC_INSTANCE_ROOT=str(first_instance),
    )
    assert first.returncode == 0, first.stderr

    second = run_product(
        product, "codex", tmp_path,
        AGENT_FABRIC_INSTANCE_ROOT=str(second_instance),
    )

    assert second.returncode == 0, second.stderr
    instructions = (tmp_path / "codex/AGENTS.md").read_text()
    assert str(second_instance / "AGENTS.md") in instructions
    assert str(first_instance / "AGENTS.md") not in instructions
    assert json.loads(
        (second_instance / ".agent-fabric/product-root.json").read_text()
    )["product_root"] == str(product.resolve())


@pytest.mark.parametrize("platform", ["claude", "codex"])
def test_relocation_rewrites_only_an_exact_generated_bootstrap_and_preserves_conflicts(
    tmp_path, platform
):
    old_product = copy_product(ROOT, tmp_path / "old product")
    new_product = copy_product(ROOT, tmp_path / "new product")

    first = run_product(old_product, platform, tmp_path)
    assert first.returncode == 0, first.stderr
    instructions = (
        tmp_path / "claude" / "CLAUDE.md"
        if platform == "claude"
        else tmp_path / "codex" / "AGENTS.md"
    )
    original = instructions.read_bytes()
    pointer = tmp_path / "instance/.agent-fabric/product-root.json"
    old_pointer = pointer.read_text()
    harness = (
        tmp_path / "claude" / "HARNESS.md"
        if platform == "claude"
        else tmp_path / "codex" / "HARNESS.md"
    )
    harness.symlink_to(Path("../old product/HARNESS.md"))
    old_harness_target = os.readlink(harness)
    instructions.write_bytes(original + b"\n# User-owned addition\n")

    refused = run_product(new_product, platform, tmp_path)

    assert refused.returncode == 3
    assert instructions.read_bytes() == original + b"\n# User-owned addition\n"
    assert os.readlink(harness) == old_harness_target
    assert pointer.read_text() == old_pointer
    instructions.write_bytes(original)

    relocated = run_product(new_product, platform, tmp_path)

    assert relocated.returncode == 0, relocated.stderr
    assert "instructions updated=" in relocated.stdout
    assert str(new_product / "HARNESS.md") in instructions.read_text()
    assert str(old_product / "HARNESS.md") not in instructions.read_text()
    assert harness.resolve() == (new_product / "HARNESS.md").resolve()
    assert (tmp_path / platform / "skills/scope").resolve().is_relative_to(
        (new_product / "skills").resolve()
    )
    assert json.loads(pointer.read_text())["product_root"] == str(new_product.resolve())


def test_three_line_custom_instructions_are_not_mistaken_for_generated_bootstrap(tmp_path):
    old_product = copy_product(ROOT, tmp_path / "old product")
    new_product = copy_product(ROOT, tmp_path / "new product")
    first = run_product(old_product, "codex", tmp_path)
    assert first.returncode == 0, first.stderr

    instructions = tmp_path / "codex/AGENTS.md"
    generated_line = instructions.read_text().splitlines()[2]
    custom = f"# My instructions\n\n{generated_line}\n"
    instructions.write_text(custom)
    pointer = tmp_path / "instance/.agent-fabric/product-root.json"
    original_pointer = pointer.read_text()

    result = run_product(new_product, "codex", tmp_path)

    assert result.returncode == 3
    assert instructions.read_text() == custom
    assert pointer.read_text() == original_pointer


def test_split_relocation_recognises_the_exact_legacy_fused_bootstrap(tmp_path):
    new_product = copy_product(ROOT, tmp_path / "new product")
    old_product = tmp_path / "old product"
    instance = tmp_path / "instance"
    pointer = instance / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    pointer.write_text(
        json.dumps({"schema_version": 1, "product_root": str(old_product)}) + "\n"
    )
    instructions = tmp_path / "codex/AGENTS.md"
    instructions.parent.mkdir(parents=True)
    instructions.write_text(
        "# Provenant\n\n"
        + LEGACY_BOOTSTRAP.format(product=old_product)
        + "\n"
    )
    (tmp_path / "codex/HARNESS.md").symlink_to(old_product / "HARNESS.md")

    result = run_product(new_product, "codex", tmp_path)

    assert result.returncode == 0, result.stderr
    assert "instructions updated=" in result.stdout
    assert str(instance / "AGENTS.md") in instructions.read_text()
    assert str(new_product / "HARNESS.md") in instructions.read_text()


def test_generated_bootstrap_is_repaired_even_when_pointer_already_names_product(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    first = run_product(product, "codex", tmp_path)
    assert first.returncode == 0, first.stderr
    instructions = tmp_path / "codex/AGENTS.md"
    stale = instructions.read_text().replace(
        str(product / "HARNESS.md"), str(tmp_path / "older product/HARNESS.md")
    )
    instructions.write_text(stale)

    result = run_product(product, "codex", tmp_path)

    assert result.returncode == 0, result.stderr
    assert "instructions updated=" in result.stdout
    assert str(product / "HARNESS.md") in instructions.read_text()


def test_all_primary_cutover_preflights_second_primary_before_mutating_first(tmp_path):
    old_product = copy_product(ROOT, tmp_path / "old product")
    new_product = copy_product(ROOT, tmp_path / "new product")
    first = run_product(old_product, "claude", tmp_path)
    assert first.returncode == 0, first.stderr
    pointer = tmp_path / "instance/.agent-fabric/product-root.json"
    old_pointer = pointer.read_text()
    claude_skill = tmp_path / "claude/skills/scope"
    old_claude_target = claude_skill.resolve()

    foreign_skill = tmp_path / "foreign/skills/scope"
    foreign_skill.mkdir(parents=True)
    (foreign_skill / "SKILL.md").write_text("---\nname: scope\ndescription: old\n---\n")
    codex_skills = tmp_path / "codex/skills"
    codex_skills.mkdir(parents=True)
    (codex_skills / "scope").symlink_to(foreign_skill, target_is_directory=True)

    result = run_product(new_product, "all", tmp_path)

    assert result.returncode == 3
    assert pointer.read_text() == old_pointer
    assert claude_skill.resolve() == old_claude_target
    assert (tmp_path / "claude/CLAUDE.md").read_bytes() != b""
    assert (codex_skills / "scope").is_symlink()
    assert (tmp_path / "new product/HARNESS.md").is_file()


def test_all_primary_install_configures_both_surfaces_and_publishes_once(tmp_path):
    product = copy_product(ROOT, tmp_path / "product with spaces")

    result = run_product(product, "all", tmp_path)

    assert result.returncode == 0, result.stderr
    assert result.stdout.count("product root pointer updated=") == 1
    assert (tmp_path / "claude/skills/scope/SKILL.md").is_file()
    assert (tmp_path / "claude/agents/codex-analyst.md").is_file()
    assert (tmp_path / "claude/workflows/implement-run.js").is_file()
    assert (tmp_path / "codex/skills/scope/SKILL.md").is_file()
    assert (tmp_path / "claude/CLAUDE.md").read_text().find(str(product / "HARNESS.md")) >= 0
    assert (tmp_path / "codex/AGENTS.md").read_text().find(str(product / "HARNESS.md")) >= 0

    claude_mcp = json.loads((tmp_path / "home/.claude.json").read_text())
    codex_config = tomllib.loads((tmp_path / "codex/config.toml").read_text())
    assert claude_mcp["mcpServers"]["fabric"]["command"] == str(tmp_path / "bin/provenant")
    assert codex_config["mcp_servers"]["fabric"]["command"] == str(tmp_path / "bin/provenant")
    assert claude_mcp["mcpServers"]["fabric"]["env"]["AGENT_FABRIC_SEAT"] == "claude"
    assert codex_config["mcp_servers"]["fabric"]["env"]["AGENT_FABRIC_SEAT"] == "codex"
    assert claude_mcp["mcpServers"]["fabric"]["env"]["AGENT_FABRIC_INSTANCE_ROOT"] == str(tmp_path / "instance")
    assert codex_config["mcp_servers"]["fabric"]["env"]["AGENT_FABRIC_INSTANCE_ROOT"] == str(tmp_path / "instance")
    clean_home = tmp_path / "clean-home"
    clean_home.mkdir()
    registered_environment = os.environ.copy()
    registered_environment.pop("AGENT_FABRIC_PRODUCT_ROOT", None)
    registered_environment.update({
        "HOME": str(clean_home),
        **claude_mcp["mcpServers"]["fabric"]["env"],
    })
    registered = subprocess.run(
        [claude_mcp["mcpServers"]["fabric"]["command"], "root"],
        cwd=clean_home,
        env=registered_environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert registered.returncode == 0, registered.stderr
    assert registered.stdout == f"{product.resolve()}\n"
    assert not (tmp_path / "home/.cursor/mcp.json").exists()
    assert not (tmp_path / "home/.gemini/config/mcp_config.json").exists()
    assert not (tmp_path / "home/.kiro/settings/mcp.json").exists()
    assert not (tmp_path / "home/.config/opencode/opencode.jsonc").exists()


def test_all_primary_install_uses_selected_python_when_path_has_no_python3(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    tool_bin = tmp_path / "tools"
    tool_bin.mkdir()
    for name in (
        "bash", "basename", "chmod", "cp", "dirname", "grep", "ln", "mkdir",
        "mktemp", "mv", "readlink", "realpath", "rm", "rmdir", "sed", "sort", "wc",
    ):
        executable = shutil.which(name)
        assert executable is not None
        (tool_bin / name).symlink_to(executable)

    result = run_product(product, "all", tmp_path, PATH=str(tool_bin))

    assert result.returncode == 0, result.stderr
    assert (tmp_path / "claude/workflows/implement-run.js").is_file()
    assert (tmp_path / "claude/agents/codex-analyst.md").is_file()
    assert (tmp_path / "codex/skills/scope/SKILL.md").is_file()


@pytest.mark.parametrize("platform", ["claude", "codex"])
def test_foreign_harness_link_is_rejected_before_pointer_publish(tmp_path, platform):
    old_product = copy_product(ROOT, tmp_path / "old product")
    new_product = copy_product(ROOT, tmp_path / "new product")
    first = run_product(old_product, platform, tmp_path)
    assert first.returncode == 0, first.stderr

    home = tmp_path / ("claude" if platform == "claude" else "codex")
    harness = home / "HARNESS.md"
    foreign = tmp_path / "foreign/HARNESS.md"
    foreign.parent.mkdir()
    foreign.write_text("user-owned harness\n")
    harness.symlink_to(Path("../foreign/HARNESS.md"))
    pointer = tmp_path / "instance/.agent-fabric/product-root.json"
    old_pointer = pointer.read_text()
    old_target = os.readlink(harness)

    result = run_product(new_product, platform, tmp_path)

    assert result.returncode == 3
    assert pointer.read_text() == old_pointer
    assert os.readlink(harness) == old_target
    assert "HARNESS.md is not a canonical or receipt-owned product link" in result.stderr


def test_generated_instruction_symlink_is_rejected_without_following_external_target(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    first = run_product(product, "codex", tmp_path)
    assert first.returncode == 0, first.stderr

    instructions = tmp_path / "codex/AGENTS.md"
    generated = instructions.read_bytes()
    external = tmp_path / "external/AGENTS.md"
    external.parent.mkdir()
    external.write_bytes(generated)
    instructions.unlink()
    instructions.symlink_to(Path("../external/AGENTS.md"))
    pointer = tmp_path / "instance/.agent-fabric/product-root.json"
    original_pointer = pointer.read_text()

    result = run_product(product, "codex", tmp_path)

    assert result.returncode == 3
    assert instructions.is_symlink()
    assert external.read_bytes() == generated
    assert pointer.read_text() == original_pointer
    assert "instructions symlink is not the instance doctrine" in result.stderr


def test_harness_rebind_uses_collision_safe_staging(tmp_path):
    old_product = copy_product(ROOT, tmp_path / "old product")
    new_product = copy_product(ROOT, tmp_path / "new product")
    first = run_product(old_product, "claude", tmp_path)
    assert first.returncode == 0, first.stderr

    harness = tmp_path / "claude/HARNESS.md"
    harness.symlink_to(old_product / "HARNESS.md")
    reserved = tmp_path / "claude/HARNESS.md.provenant-new"
    reserved.write_text("user-owned staging name\n")

    result = run_product(new_product, "claude", tmp_path)

    assert result.returncode == 0, result.stderr
    assert reserved.read_text() == "user-owned staging name\n"
    assert harness.resolve() == (new_product / "HARNESS.md").resolve()


def test_relocation_rebinds_a_dangling_relative_old_harness_link(tmp_path):
    old_product = copy_product(ROOT, tmp_path / "old'\\product")
    new_product = copy_product(ROOT, tmp_path / "new product")
    first = run_product(old_product, "claude", tmp_path)
    assert first.returncode == 0, first.stderr

    harness = tmp_path / "claude/HARNESS.md"
    harness.symlink_to(Path("../old'\\product/HARNESS.md"))
    old_product.rename(tmp_path / "moved old product")

    result = run_product(new_product, "claude", tmp_path)

    assert result.returncode == 0, result.stderr
    assert harness.resolve() == (new_product / "HARNESS.md").resolve()


def test_relocation_matches_dangling_link_through_a_symlinked_parent(tmp_path):
    physical = tmp_path / "physical"
    physical.mkdir()
    alias = tmp_path / "alias"
    alias.symlink_to(physical, target_is_directory=True)
    old_product = copy_product(ROOT, physical / "old product")
    new_product = copy_product(ROOT, physical / "new product")
    first = run_product(old_product, "claude", tmp_path)
    assert first.returncode == 0, first.stderr

    harness = tmp_path / "claude/HARNESS.md"
    harness.symlink_to(Path("../alias/old product/HARNESS.md"))
    old_product.rename(physical / "moved old product")

    result = run_product(new_product, "claude", tmp_path)

    assert result.returncode == 0, result.stderr
    assert harness.resolve() == (new_product / "HARNESS.md").resolve()


def test_instance_leaf_symlink_conflicts_are_rejected_before_publication(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    instance = tmp_path / "instance"
    instance.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (instance / ".agent-fabric").mkdir()
    (instance / ".agent-fabric/.gitignore").symlink_to(outside / "gitignore")
    (instance / "config").symlink_to(outside / "config", target_is_directory=True)
    pointer = instance / ".agent-fabric/product-root.json"

    result = run_product(
        product,
        "codex",
        tmp_path,
        AGENT_FABRIC_INSTANCE_ROOT=str(instance),
    )

    assert result.returncode == 3
    assert not pointer.exists()
    assert list(outside.iterdir()) == []


def test_instance_config_directory_symlink_is_rejected_before_publication(tmp_path):
    product = copy_product(ROOT, tmp_path / "product")
    instance = tmp_path / "instance"
    instance.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (instance / "config").symlink_to(outside / "config", target_is_directory=True)
    pointer = instance / ".agent-fabric/product-root.json"

    result = run_product(product, "codex", tmp_path, AGENT_FABRIC_INSTANCE_ROOT=str(instance))

    assert result.returncode == 3
    assert not pointer.exists()
    assert list(outside.iterdir()) == []


LEGACY_BOOTSTRAP = (
    "Read and follow `{product}/AGENTS.md` and `{product}/HARNESS.md` before making "
    "orchestration, delegation, model-routing or memory decisions. Platform/system "
    "policy and explicit user authority lead; the nearest project instruction may "
    "specialise or strengthen the global harness but may not silently broaden "
    "authority, weaken safety gates or redefine global cross-project memory policy."
)


def test_a_fused_upgrade_leaves_legacy_instructions_byte_stable(tmp_path):
    """When both roots are one directory the two forms coincide: no rewrite."""
    config = tmp_path / "claude-config"
    config.mkdir()
    instructions = config / "CLAUDE.md"
    original = "# Provenant\n\n" + LEGACY_BOOTSTRAP.format(product=ROOT) + "\n"
    instructions.write_bytes(original.encode())

    result = run(
        "claude",
        tmp_path,
        CLAUDE_CONFIG_DIR=str(config),
        AGENT_FABRIC_INSTANCE_ROOT=str(ROOT),
    )

    assert result.returncode == 0, result.stderr
    assert f"instructions existing={instructions}" in result.stdout
    assert instructions.read_bytes() == original.encode()


def test_a_genuinely_foreign_instructions_file_is_still_refused(tmp_path):
    """Migration must not become a licence to rewrite user-authored files."""
    config = tmp_path / "claude-config"
    config.mkdir()
    instructions = config / "CLAUDE.md"
    instructions.write_bytes(UNMANAGED_BYTES)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert instructions.read_bytes() == UNMANAGED_BYTES
    assert "instructions preserved=" in result.stderr
