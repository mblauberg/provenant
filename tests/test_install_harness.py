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
WORKFLOW_SCRIPT = ROOT / "scripts" / "install-workflows"
WORKFLOW_NAMES = {
    "codebase-polish.js",
    "cross-verify.js",
    "implement-run.js",
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
        "flagship",
        "routine",
        "scout",
        "substantial",
        "terminal",
        "workhorse",
    }
)


def run(platform: str, home: Path, *arguments: str, **extra_env):
    env = os.environ.copy()
    env.update({"HOME": str(home), **extra_env})
    return subprocess.run(
        [str(SCRIPT), "--platform", platform, *arguments],
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


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


def ambient_skill_names_and_resolver_root():
    available = expected_skills()
    texts = []
    resolver_roots = set()
    for ambient in (ROOT / "AGENTS.md", ROOT / "HARNESS.md"):
        text = ambient.read_text()
        texts.append(text)
        roots = re.findall(r"`(\$HOME/\.agents/skills/<name>/)`", text)
        assert roots == ["$HOME/.agents/skills/<name>/"], (
            f"{ambient.name} must state exactly one D12 resolver root"
        )
        resolver_roots.update(roots)
    assert len(resolver_roots) == 1
    return _ambient_skill_names(texts, available), resolver_roots.pop()


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
    assert command.is_symlink()
    assert command.resolve() == ROOT / "scripts" / "provenant"
    assert {path.name for path in (config / "skills").iterdir()} == expected_skills()
    workflows = config / "workflows"
    assert {path.name for path in workflows.iterdir()} == WORKFLOW_NAMES
    for name in WORKFLOW_NAMES:
        assert (workflows / name).is_symlink()
        assert (workflows / name).resolve() == ROOT / "workflows" / name
    workflow_manifest = json.loads(
        (config / ".agent-harness-workflows-installation.json").read_text()
    )
    assert set(workflow_manifest["managed"]) == WORKFLOW_NAMES
    instructions = config / "CLAUDE.md"
    content = instructions.read_text()
    assert str(ROOT / "AGENTS.md") in content
    assert str(ROOT / "HARNESS.md") in content
    registration = json.loads((tmp_path / ".claude.json").read_text())["mcpServers"]["agent-fabric"]
    assert registration["command"] == str(ROOT / "scripts" / "agent-fabric-mcp")
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


def test_installs_codex_skills_and_global_instructions(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()
    codex_config = config / "config.toml"
    codex_config.write_text("[custom]\nvalue = 'preserved'\n")
    result = run("codex", tmp_path, CODEX_HOME=str(config))
    assert result.returncode == 0, result.stderr
    assert {path.name for path in (config / "skills").iterdir()} == expected_skills()
    assert not (tmp_path / ".claude" / "workflows").exists()
    assert not (config / "workflows").exists()
    assert not (config / ".agent-harness-workflows-installation.json").exists()
    assert str(ROOT / "HARNESS.md") in (config / "AGENTS.md").read_text()
    configured = codex_config.read_text()
    assert "[custom]\nvalue = 'preserved'" in configured
    assert configured.count('name = "skill-creator"') == 1
    assert "enabled = false" in configured
    registration = tomllib.loads(configured)["mcp_servers"]["agent-fabric"]
    assert registration == {
        "command": str(ROOT / "scripts" / "agent-fabric-mcp"),
        "env": {
            "AGENT_FABRIC_CLIENT_LABEL": "codex",
            "AGENT_FABRIC_SEAT": "codex",
            "AGENT_FABRIC_STATE_DIRECTORY": str(tmp_path / ".local/state/agent-harness/fabric"),
        },
    }

    second = run("codex", tmp_path, CODEX_HOME=str(config))
    assert second.returncode == 0, second.stderr
    assert codex_config.read_text() == configured


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
    assert "codebase-polish.js=unmanaged" in result.stderr
    managed_names = WORKFLOW_NAMES - {name}
    for managed_name in managed_names:
        installed = workflows / managed_name
        assert installed.is_symlink()
        assert installed.resolve() == ROOT / "workflows" / managed_name
    manifest = json.loads(
        (config / ".agent-harness-workflows-installation.json").read_text()
    )
    assert set(manifest["managed"]) == managed_names


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
    assert "codebase-polish.js=unmanaged" in result.stderr
    manifest = json.loads(
        (config / ".agent-harness-workflows-installation.json").read_text()
    )
    assert name not in manifest["managed"]


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
    names, resolver_template = ambient_skill_names_and_resolver_root()
    assert names == EXPECTED_AMBIENT_SKILL_NAMES

    home = tmp_path / platform
    home.mkdir()
    # Model the canonical checkout location named by the D12 resolver line
    # while keeping the isolated install's actual source tree immutable.
    (home / ".agents").symlink_to(ROOT, target_is_directory=True)
    config = home / config_name
    result = run(platform, home, **{variable: str(config)})
    assert result.returncode == 0, result.stderr

    installed_root = config / "skills"
    installed_names = {path.name for path in installed_root.iterdir()}
    assert installed_names == expected_skills()
    resolver_root = Path(
        resolver_template.replace("$HOME", str(home)).replace("<name>/", "")
    )
    assert resolver_root.resolve() == (ROOT / "skills").resolve()
    installed_source_roots = {
        (installed_root / name / "SKILL.md").resolve().parents[1] for name in names
    }
    assert installed_source_roots == {resolver_root.resolve()}
    for name in names:
        installed = installed_root / name / "SKILL.md"
        resolved = resolver_root / name / "SKILL.md"
        assert installed.is_file(), f"{platform} did not install skills/{name}/SKILL.md"
        assert resolved.is_file(), f"resolver root cannot find skills/{name}/SKILL.md"
        assert installed.resolve() == resolved.resolve(), (
            f"{platform} installed root disagrees with $HOME/.agents/skills/{name}/"
        )


def test_all_mcp_clients_are_an_explicit_subscription_native_opt_in(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()

    result = run("codex", tmp_path, "--mcp-clients", "all", CODEX_HOME=str(config))

    assert result.returncode == 0, result.stderr
    for client, path in {
        "cursor": tmp_path / ".cursor/mcp.json",
        "agy": tmp_path / ".gemini/config/mcp_config.json",
        "kiro": tmp_path / ".kiro/settings/mcp.json",
    }.items():
        registration = json.loads(path.read_text())["mcpServers"]["agent-fabric"]
        assert registration["env"]["AGENT_FABRIC_SEAT"] == "codex"
        assert registration["env"]["AGENT_FABRIC_CLIENT_LABEL"] == client
        assert "AGENT_FABRIC_PROJECT_PATH" not in registration["env"]
    opencode = json.loads((tmp_path / ".config/opencode/opencode.jsonc").read_text())
    registration = opencode["mcp"]["agent-fabric"]
    assert registration["command"] == [str(ROOT / "scripts" / "agent-fabric-mcp")]
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
    assert {path.name for path in (config / "skills").iterdir()} == expected_skills()


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
    assert str(ROOT / "AGENTS.md") in result.stderr
    assert str(ROOT / "HARNESS.md") in result.stderr
    assert {path.name for path in (config / "skills").iterdir()} == expected_skills()


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
    assert str(ROOT / "AGENTS.md") in result.stderr
    assert str(ROOT / "HARNESS.md") in result.stderr
    assert {path.name for path in (config / "skills").iterdir()} == expected_skills()


def test_accepts_claude_instruction_symlink_to_canonical_agents_file(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    instructions = config / "CLAUDE.md"
    instructions.symlink_to(ROOT / "AGENTS.md")

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert instructions.is_symlink()
    assert instructions.resolve() == ROOT / "AGENTS.md"
    assert f"instructions existing={instructions}" in result.stdout
    assert "add this line" not in result.stderr


def test_accepts_codex_instruction_symlink_to_canonical_agents_file(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()
    instructions = config / "AGENTS.md"
    instructions.symlink_to(ROOT / "AGENTS.md")

    result = run("codex", tmp_path, CODEX_HOME=str(config))

    assert result.returncode == 0, result.stderr
    assert instructions.is_symlink()
    assert instructions.resolve() == ROOT / "AGENTS.md"
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
    assert "add this line" in result.stderr


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
    assert (bin_dir / "provenant").resolve() == ROOT / "scripts" / "provenant"
    assert not (tmp_path / ".zshrc").exists()
    assert not (tmp_path / ".bashrc").exists()
