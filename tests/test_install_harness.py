from pathlib import Path
import json
import os
import shutil
import stat
import subprocess
import sys
import tomllib
from datetime import date, datetime, timedelta, timezone

import pytest
from tests.support.proc import run_cli

from tests.support.front_door import front_door_binary

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "install-harness"
PROVENANT_BINARY = front_door_binary()
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
def instance_root_for(home: Path) -> Path:
    """Where `install-harness` seeds the instance under a scratch HOME."""
    return home / ".agents"


def utc_timestamp() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def otel_tools(tmp_path, system="Darwin", swiftc=True):
    """A PATH prefix with a stand-in `uname` and, optionally, a stand-in `swiftc`."""
    tools = tmp_path / "tools"
    tools.mkdir()
    uname = tools / "uname"
    uname.write_text(f"#!/bin/sh\necho {system}\n")
    uname.chmod(0o755)
    if swiftc:
        compiler = tools / "swiftc"
        compiler.write_text('#!/bin/bash\nwhile [[ "$1" != -o ]]; do shift; done\ntouch "$2"\n')
        compiler.chmod(0o755)
    return tools


def otel_install(tmp_path, tools, path=None, **extra):
    return run_cli(
        [shutil.which("bash"), "-c", 'umask 022; exec "$1"', "bash", str(ROOT / "scripts/install-otel-collector")],
        cwd=ROOT,
        env={**os.environ, "HOME": str(tmp_path), "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state"),
             "PROVENANT_BIN_DIR": str(tmp_path / "bin"), "PROVENANT_OTEL_NO_LAUNCHCTL": "1",
             "PATH": path or f"{tools}{os.pathsep}{os.environ['PATH']}", **extra},
        timeout=5,
    )


def otel_plist(tmp_path):
    return (tmp_path / "Library/LaunchAgents/com.provenant.otlp-collector.plist").read_text()


@pytest.mark.parametrize("existing", ["new", "directory", "symlink"])
def test_otel_installer_restricts_storage_permissions(tmp_path, existing):
    tools = otel_tools(tmp_path)
    state = tmp_path / "state"
    directory = state / "otel"
    if existing == "directory":
        directory.mkdir(parents=True, mode=0o755)
    elif existing == "symlink":
        state.mkdir()
        outside = tmp_path / "outside"
        outside.mkdir(mode=0o755)
        directory.symlink_to(outside, target_is_directory=True)
    result = otel_install(tmp_path, tools)
    if existing == "symlink":
        assert result.returncode != 0
        assert stat.S_IMODE(outside.stat().st_mode) == 0o755
        assert list(outside.iterdir()) == []
        return
    assert result.returncode == 0, result.stderr
    assert stat.S_IMODE(directory.stat().st_mode) == 0o700
    assert "<string>4318</string>" in otel_plist(tmp_path) and "@PORT@" not in otel_plist(tmp_path)


def test_otel_installer_refuses_a_host_without_macos_launchd(tmp_path):
    result = otel_install(tmp_path, otel_tools(tmp_path, system="Linux"))
    assert result.returncode != 0
    assert "needs macOS and launchd; this host is Linux" in result.stderr
    assert not (tmp_path / "bin").exists() and not (tmp_path / "Library").exists()


def test_otel_installer_fails_loudly_without_swiftc(tmp_path):
    tools = otel_tools(tmp_path, swiftc=False)
    for name in ("bash", "dirname"):  # the shebang and ROOT need these; swiftc stays off PATH
        (tools / name).symlink_to(shutil.which(name))
    result = otel_install(tmp_path, tools, path=str(tools))
    assert result.returncode != 0
    assert "swiftc not found" in result.stderr
    assert not (tmp_path / "bin").exists()


def test_otel_installer_substitutes_a_configured_port(tmp_path):
    result = otel_install(tmp_path, otel_tools(tmp_path), PROVENANT_OTEL_PORT="4400")
    assert result.returncode == 0, result.stderr
    assert "<key>SockServiceName</key><string>4400</string>" in otel_plist(tmp_path)
    assert "port=4400" in result.stdout


@pytest.mark.parametrize("port", ["0", "65536", "99999", "080", "43a", "-1", " 4318", ""])
def test_otel_installer_rejects_an_invalid_port(tmp_path, port):
    result = otel_install(tmp_path, otel_tools(tmp_path), PROVENANT_OTEL_PORT=port)
    if port == "":
        # An empty value means unset: the default port.
        assert result.returncode == 0, result.stderr
        assert "<string>4318</string>" in otel_plist(tmp_path)
        return
    assert result.returncode != 0
    assert "PROVENANT_OTEL_PORT must be an integer from 1 to 65535" in result.stderr
    assert not (tmp_path / "Library").exists()


def run(platform: str, home: Path, *arguments: str, bash=None, **extra_env):
    env = os.environ.copy()
    env.pop("AGENT_FABRIC_STATE_DIRECTORY", None)
    # Caller-inherited install targets would leak into the scratch HOME.
    for name in ("CLAUDE_CONFIG_DIR", "PROVENANT_BIN_DIR", "CODEX_HOME"):
        env.pop(name, None)
    env.update({"HOME": str(home)})
    # Keep the instance root deterministic in the scratch HOME. AGENTS_HOME now
    # names only the product root, but an explicit instance value also keeps the
    # test independent of any caller-provided instance selection.
    env["AGENT_FABRIC_INSTANCE_ROOT"] = str(instance_root_for(home))
    env["PROVENANT_ALLOW_LINKED_WORKTREE_INSTALL"] = "1"
    # General install cases use the real binary; build failures have focused cases.
    env["PROVENANT_FRONT_DOOR_BINARY"] = str(PROVENANT_BINARY)
    env.update(extra_env)
    return run_cli(
        [*([bash] if bash else []), SCRIPT, "--platform", platform, *arguments],
        cwd=ROOT,
        env=env,
        timeout=600,  # main ran these without a timeout
    )


@pytest.fixture(scope="module")
def product_template(tmp_path_factory) -> Path:
    """One filtered product checkout per module; each test clones it."""
    template = tmp_path_factory.mktemp("product-template") / "product"
    shutil.copytree(
        ROOT,
        template,
        symlinks=True,
        # Cargo's in-tree `target` is build output: copying it into every test
        # product exhausted the CI runner's /tmp quota.
        ignore=shutil.ignore_patterns(
            ".git", ".agent-run", "node_modules", ".venv", "installation.json", "target"
        ),
    )
    return template


def copy_product(template: Path, destination: Path) -> Path:
    # cp -R keeps symlinks as links; -c makes an APFS clone on macOS.
    command = ["cp", "-c", "-R"] if sys.platform == "darwin" else ["cp", "-R"]
    subprocess.run([*command, str(template), str(destination)], check=True)
    return destination


def run_product(product: Path, platform: str, home: Path, *arguments: str, **extra_env):
    build_front_door = extra_env.pop("_build_front_door", False)
    environment = os.environ.copy()
    environment.pop("AGENT_FABRIC_STATE_DIRECTORY", None)
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
            "PROVENANT_FRONT_DOOR_BINARY": str(PROVENANT_BINARY),
        }
    )
    if build_front_door:
        environment.pop("PROVENANT_FRONT_DOOR_BINARY", None)
    environment.update(extra_env)
    return run_cli(
        [product / "scripts/install-harness", "--platform", platform, *arguments],
        cwd=product,
        env=environment,
        timeout=600,  # main ran these without a timeout
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
    result = run_cli(
        [str(command), "--platform", "claude"],
        cwd=tmp_path,
        env=env,
    )

    assert result.returncode == 3
    assert "linked worktree" in result.stderr


def run_workflow_installer(target: Path):
    return run_cli(
        [str(WORKFLOW_SCRIPT), "--target", str(target)],
        cwd=ROOT,
    )


def expected_skills():
    return {
        path.parent.name
        for base in ("skills", "library")
        for path in (ROOT / base).glob("*/SKILL.md")
    }


def expected_installed_entries():
    """A per-entry layout carries the skills plus the shared library they import."""
    return expected_skills() | {"_shared"}


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
    assert command.read_bytes() == PROVENANT_BINARY.read_bytes()
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
    instructions = config / "CLAUDE.md"
    content = instructions.read_text()
    # Doctrine is read from the seeded instance copy; the harness constitution
    # stays product-shipped (ADR 0019).
    assert str(instance_root_for(tmp_path) / "AGENTS.md") in content
    assert str(ROOT / "HARNESS.md") in content
    registration = json.loads((config / ".claude.json").read_text())["mcpServers"]["fabric"]
    assert not (tmp_path / ".claude.json").exists()
    assert not (config / "settings.json").exists()
    assert not (config / "settings.json").is_symlink()
    assert not (tmp_path / ".claude/settings.json").exists()
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
    assert instructions.exists()
    assert not (config / "settings.json").exists()
    assert not (config / "settings.json").is_symlink()


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

    result = run_cli(
        [str(product / "scripts/install-harness"), "--platform", "codex"],
        cwd=product,
        env=environment,
    )

    assert result.returncode == 7
    assert pointer.read_text() == original
    assert (instance / "AGENTS.md").is_file(), "the forced failure must occur after seeding"


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
    (config / "workflows").symlink_to(ROOT / "workflows", target_is_directory=True)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    # The personal library sits beside skills/, so the skills link converts to
    # per-entry links; the workflows link is left as a directory link.
    assert (config / "skills").is_dir() and not (config / "skills").is_symlink()
    assert (config / "workflows").resolve() == (ROOT / "workflows").resolve()


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


def test_claude_directory_link_layout_converts_to_project_custom_skills(tmp_path):
    """A whole-directory link cannot carry a custom skill, so the install converts it."""
    config = tmp_path / "claude-config"
    config.mkdir()
    (config / "skills").symlink_to(ROOT / "skills", target_is_directory=True)
    custom_skill = instance_root_for(tmp_path) / "custom-skills" / "local-skill"
    custom_skill.mkdir(parents=True)
    (custom_skill / "SKILL.md").write_text(
        "---\nname: local-skill\ndescription: Instance-owned test skill.\n---\n"
    )

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert "warning: directory-link layout skipped" not in result.stderr
    target = config / "skills"
    assert not target.is_symlink()
    installed = target / "local-skill"
    assert installed.is_symlink()
    assert installed.resolve() == custom_skill.resolve()
    product = {path.parent.name for path in (ROOT / "skills").glob("*/SKILL.md")}
    assert product <= {path.name for path in target.iterdir()}
    for name in product:
        assert (target / name).resolve() == (ROOT / "skills" / name).resolve()
    manifest = json.loads((config / ".agent-harness-installation.json").read_text())
    assert manifest["custom"]["local-skill"] == {
        "source_target": str(custom_skill.resolve())
    }
    assert "local-skill" not in manifest["managed"]


def test_claude_directory_link_layout_converts_to_carry_the_library(
    tmp_path,
):
    config = tmp_path / "claude-config"
    config.mkdir()
    (config / "skills").symlink_to(ROOT / "skills", target_is_directory=True)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert (config / "skills").is_dir() and not (config / "skills").is_symlink()
    assert (config / "skills" / "tanstack-query").is_symlink()
    assert (config / ".agent-harness-installation.json").exists()


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
    interrupt = run_cli(
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
    fixture_root = tmp_path / "workflow-fixture"
    scripts = fixture_root / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(WORKFLOW_SCRIPT, scripts / "install-workflows")
    shutil.copytree(ROOT / "workflows", fixture_root / "workflows")
    platform_home = tmp_path / "claude"
    platform_home.mkdir()
    target = platform_home / "workflows"
    target.symlink_to(fixture_root / "workflows", target_is_directory=True)

    result = run_cli(
        [str(scripts / "install-workflows"), "--target", str(target)],
        cwd=fixture_root,
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


@pytest.mark.parametrize(
    "platform, config_name, variable",
    (
        ("claude", ".claude", "CLAUDE_CONFIG_DIR"),
        ("codex", ".codex", "CODEX_HOME"),
    ),
)
def test_installed_skills_resolve_on_both_platform_layouts(
    tmp_path, platform, config_name, variable
):
    """Each supported platform receives links to the current product skills."""
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
    assert installed_names == expected_skills() | {"_shared"}
    assert all((installed_root / name).is_symlink() for name in installed_names)


def test_all_mcp_clients_are_an_explicit_subscription_native_opt_in(tmp_path):
    config = tmp_path / "codex-home"
    config.mkdir()

    result = run("codex", tmp_path, "--mcp-clients", "all", CODEX_HOME=str(config))

    assert result.returncode == 0, result.stderr
    # Each client has its own Fabric inbox; seat alone is not model-family proof.
    for client, path, seat in (
        ("cursor", tmp_path / ".cursor/mcp.json", "cursor"),
        ("agy", tmp_path / ".gemini/config/mcp_config.json", "agy"),
        ("kiro", tmp_path / ".kiro/settings/mcp.json", "kiro"),
    ):
        registration = json.loads(path.read_text())["mcpServers"]["fabric"]
        assert registration["env"]["AGENT_FABRIC_SEAT"] == seat
        assert registration["env"]["AGENT_FABRIC_CLIENT_LABEL"] == client
        assert "AGENT_FABRIC_PROJECT_PATH" not in registration["env"]
    opencode = json.loads((tmp_path / ".config/opencode/opencode.jsonc").read_text())
    registration = opencode["mcp"]["fabric"]
    assert registration["command"] == [str(tmp_path / ".local/bin/provenant")]
    assert registration["environment"]["AGENT_FABRIC_SEAT"] == "opencode"
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


def test_opencode_primary_installs_bootstrap_skills_harness_and_mcp(tmp_path):
    result = run("opencode", tmp_path)

    assert result.returncode == 0, result.stderr
    root = tmp_path / ".config/opencode"
    assert str(ROOT / "HARNESS.md") in (root / "AGENTS.md").read_text()
    assert (root / "HARNESS.md").resolve() == ROOT / "HARNESS.md"
    assert {path.name for path in (root / "skills").iterdir()} == expected_installed_entries()
    config = json.loads((root / "opencode.jsonc").read_text())
    assert config["instructions"] == [
        str(instance_root_for(tmp_path) / "AGENTS.md"), str(ROOT / "HARNESS.md"),
    ]
    assert config["mcp"]["fabric"]["environment"]["AGENT_FABRIC_SEAT"] == "opencode"


def test_all_installs_present_optional_provider_surfaces(tmp_path):
    for relative in (".config/opencode", ".gemini", ".cursor", ".kiro"):
        (tmp_path / relative).mkdir(parents=True)
    (tmp_path / "bin").mkdir()
    (tmp_path / "bin/agy").write_text("#!/bin/sh\n")
    (tmp_path / "bin/agy").chmod(0o755)

    result = run("all", tmp_path, PATH=f"{tmp_path / 'bin'}:{os.environ['PATH']}")

    assert result.returncode == 0, result.stderr
    for relative in (".config/opencode", ".gemini", ".cursor", ".kiro"):
        assert (tmp_path / relative / "skills" / "orchestrate/SKILL.md").exists()
    assert (tmp_path / ".gemini/GEMINI.md").is_file()
    assert (tmp_path / ".gemini/HARNESS.md").resolve() == ROOT / "HARNESS.md"
    for relative in (
        ".claude.json", ".codex/config.toml", ".config/opencode/opencode.jsonc",
        ".gemini/config/mcp_config.json", ".cursor/mcp.json", ".kiro/settings/mcp.json",
    ):
        assert (tmp_path / relative).exists(), relative
    checked = run_cli(
        [str(ROOT / "scripts/check-provenant-install.py")],
        cwd=ROOT,
        env={
            **os.environ,
            "HOME": str(tmp_path),
            "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root_for(tmp_path)),
            "PROVENANT_BIN_DIR": str(tmp_path / ".local/bin"),
            "CODEX_HOME": str(tmp_path / ".codex"),
        },
    )
    assert checked.returncode == 0, checked.stderr
    assert len([line for line in checked.stdout.splitlines() if line.startswith("provider ")]) == 6
    assert "missing" not in checked.stdout


def test_all_without_skipped_platforms_runs_under_bash_3(tmp_path):
    bash = Path("/bin/bash")
    if not bash.exists() or "version 3." not in run_cli(
        [str(bash), "--version"], cwd=ROOT
    ).stdout:
        pytest.skip("requires /bin/bash 3.x")

    result = run(
        "all", tmp_path, bash=str(bash), HARNESS_PYTHON=sys.executable,
        PATH="/usr/bin:/bin", CLAUDE_CONFIG_DIR="", CODEX_HOME="",
        OPENCODE_CONFIG_DIR="", AGY_CONFIG_DIR="",
        PROVENANT_BIN_DIR=str(tmp_path / "bin"),
        CLAUDE_MCP_CONFIG=str(tmp_path / "claude.json"),
        CODEX_MCP_CONFIG=str(tmp_path / "codex/config.toml"),
        AGENT_FABRIC_PRODUCT_ROOT="",
    )

    assert result.returncode == 0, result.stderr
    assert "skipped=" not in result.stdout + result.stderr
    assert (tmp_path / ".claude/skills/orchestrate/SKILL.md").is_file()
    assert (tmp_path / ".codex/skills/orchestrate/SKILL.md").is_file()


def test_all_skips_detected_clients_with_user_instructions_and_keeps_primary_installs(tmp_path):
    opencode = tmp_path / ".config/opencode/AGENTS.md"
    gemini = tmp_path / ".gemini/GEMINI.md"
    opencode.parent.mkdir(parents=True)
    gemini.parent.mkdir()
    opencode.write_bytes(UNMANAGED_BYTES)
    gemini.write_bytes(UNMANAGED_BYTES)
    (tmp_path / "bin").mkdir()
    agy = tmp_path / "bin/agy"
    agy.write_text("#!/bin/sh\n")
    agy.chmod(0o755)

    result = run("all", tmp_path, PATH=f"{agy.parent}:{os.environ['PATH']}")

    assert result.returncode == 0, result.stderr
    assert "provider opencode skipped=instructions-preserved" in result.stdout
    assert "provider agy skipped=instructions-preserved" in result.stdout
    assert opencode.read_bytes() == gemini.read_bytes() == UNMANAGED_BYTES
    assert (tmp_path / ".claude/skills/orchestrate/SKILL.md").exists()
    assert (tmp_path / ".codex/skills/orchestrate/SKILL.md").exists()
    assert not (opencode.parent / "skills").exists()
    assert not (gemini.parent / "skills").exists()


def test_gemini_directory_alone_does_not_detect_agy(tmp_path):
    gemini = tmp_path / ".gemini/GEMINI.md"
    gemini.parent.mkdir()
    gemini.write_bytes(UNMANAGED_BYTES)

    result = run("all", tmp_path, PATH="/usr/bin:/bin")

    assert result.returncode == 0, result.stderr
    assert gemini.read_bytes() == UNMANAGED_BYTES
    assert not (gemini.parent / "skills").exists()


def test_all_mcp_opt_in_still_skips_detected_clients_with_user_instructions(tmp_path):
    opencode = tmp_path / ".config/opencode/AGENTS.md"
    opencode.parent.mkdir(parents=True)
    opencode.write_bytes(UNMANAGED_BYTES)

    result = run("all", tmp_path, "--mcp-clients", "all", PATH="/usr/bin:/bin")

    assert result.returncode == 0, result.stderr
    assert "provider opencode skipped=instructions-preserved" in result.stdout
    assert not (opencode.parent / "opencode.jsonc").exists()
    assert (tmp_path / ".claude.json").exists()


def test_all_skips_detected_opencode_jsonc_without_blocking_primary_clients(tmp_path):
    config = tmp_path / ".config/opencode/opencode.jsonc"
    config.parent.mkdir(parents=True)
    original = '{\n  // keep this note\n  "mcp": {},\n}\n'
    config.write_text(original)

    result = run("all", tmp_path, PATH="/usr/bin:/bin")

    assert result.returncode == 0, result.stderr
    assert "provider opencode skipped=mcp-conflict" in result.stdout
    assert "warning: detected provider opencode" in result.stderr
    assert "mcp.fabric" in result.stderr
    assert config.read_text() == original
    assert (tmp_path / ".claude.json").exists()
    assert (tmp_path / ".codex/config.toml").exists()


def test_all_skips_broken_detected_cursor_mcp_but_named_cursor_fails(tmp_path):
    config = tmp_path / ".cursor/mcp.json"
    config.parent.mkdir()
    config.write_text("{broken")

    detected = run("all", tmp_path, "--mcp-clients", "all", PATH="/usr/bin:/bin")
    assert detected.returncode == 0, detected.stderr
    assert "provider cursor skipped=mcp-conflict" in detected.stdout
    assert "warning: detected provider cursor" in detected.stderr
    assert config.read_text() == "{broken"
    assert (tmp_path / ".claude.json").exists()
    assert (tmp_path / ".codex/config.toml").exists()

    named = run("cursor", tmp_path, PATH="/usr/bin:/bin")
    assert named.returncode == 3
    assert config.read_text() == "{broken"


def test_mcp_all_skips_unnamed_primary_client_conflict(tmp_path):
    claude_config = tmp_path / ".claude.json"
    claude_config.write_text("{broken")

    result = run("codex", tmp_path, "--mcp-clients", "all")

    assert result.returncode == 0, result.stderr
    assert "provider claude skipped=mcp-conflict" in result.stdout
    assert claude_config.read_text() == "{broken"
    assert (tmp_path / ".codex/config.toml").exists()


def test_all_reports_detected_client_link_failure_and_continues(tmp_path):
    (tmp_path / ".config/opencode").mkdir(parents=True)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    blocked_ln = bin_dir / "ln"
    blocked_ln.write_text("#!/bin/sh\nexit 1\n")
    blocked_ln.chmod(0o755)

    result = run("all", tmp_path, PATH=f"{bin_dir}:{os.environ['PATH']}")

    assert result.returncode == 0, result.stderr
    assert "provider opencode skipped=install-conflict" in result.stdout
    assert "harness created=" not in result.stdout
    assert not (tmp_path / ".config/opencode/opencode.jsonc").exists()
    assert (tmp_path / ".claude.json").exists()
    assert (tmp_path / ".codex/config.toml").exists()


def test_install_refreshes_newer_routing_catalogue_and_preserves_overrides(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
    first = run_product(product, "codex", tmp_path / "install")
    assert first.returncode == 0, first.stderr
    instance_root = tmp_path / "install" / "instance"
    target = instance_root / "config/model-routing.json"
    document = json.loads(target.read_text())
    document["adapters"]["opencode"]["endpoint_provider"] = "codex"
    target.write_text(json.dumps(document))

    product_catalogue = product / "config/model-routing.json"
    shipped = json.loads(product_catalogue.read_text())
    catalogue_date = max(date.today(), date.fromisoformat(shipped["catalog_date"]))
    shipped["catalog_date"] = (catalogue_date + timedelta(days=1)).isoformat()
    shipped["task_class_routes"]["review"]["models"]["codex"] = ["new-product-route"]
    product_catalogue.write_text(json.dumps(shipped))

    second = run_product(product, "codex", tmp_path / "install")
    assert second.returncode == 0, second.stderr
    assert json.loads(target.read_text())["adapters"]["opencode"]["endpoint_provider"] == "codex"
    refreshed = json.loads(target.read_text())
    assert refreshed["task_class_routes"]["review"]["models"]["codex"] == ["new-product-route"]
    assert len(list(target.parent.glob("model-routing.json.bak-*"))) == 1

    newer = {**shipped, "catalog_date": (catalogue_date + timedelta(days=2)).isoformat()}
    product_catalogue.write_text(json.dumps(newer))
    refreshed = run_product(product, "codex", tmp_path / "install")
    assert refreshed.returncode == 0, refreshed.stderr
    assert json.loads(target.read_text())["catalog_date"] == newer["catalog_date"]
    assert len(list(target.parent.glob("model-routing.json.bak-*"))) == 2

    unchanged = run_product(product, "codex", tmp_path / "install", "--refresh-routing")
    assert unchanged.returncode == 0, unchanged.stderr
    assert len(list(target.parent.glob("model-routing.json.bak-*"))) == 2

    incomplete = json.loads(target.read_text())
    del incomplete["context"]
    target.write_text(json.dumps(incomplete))
    product_catalogue.write_text(json.dumps({**newer, "additional_product_field": "new"}))
    repaired = run_product(product, "codex", tmp_path / "install")
    assert repaired.returncode == 0, repaired.stderr
    repaired_catalogue = json.loads(target.read_text())
    assert "context" not in repaired_catalogue
    assert repaired_catalogue["additional_product_field"] == "new"
    assert len(list(target.parent.glob("model-routing.json.bak-*"))) == 3

    (instance_root / "config/.model-routing.base.json").unlink()
    product_catalogue.write_text(json.dumps({
        **json.loads(product_catalogue.read_text()),
        "catalog_date": (catalogue_date + timedelta(days=3)).isoformat(),
        "second_product_field": "newer",
    }))
    before_unbased_refresh = target.read_bytes()
    legacy_refresh = run_product(product, "codex", tmp_path / "install")
    assert legacy_refresh.returncode == 0, legacy_refresh.stderr
    assert target.read_bytes() == before_unbased_refresh
    assert "--refresh-routing" in legacy_refresh.stdout
    assert len(list(target.parent.glob("model-routing.json.bak-*"))) == 3

    forced = run_product(product, "codex", tmp_path / "install", "--refresh-routing")
    assert forced.returncode == 0, forced.stderr
    legacy_catalogue = json.loads(target.read_text())
    assert legacy_catalogue["adapters"]["opencode"]["endpoint_provider"] == "opencode"
    assert legacy_catalogue["second_product_field"] == "newer"
    assert len(list(target.parent.glob("model-routing.json.bak-*"))) == 4


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


@pytest.mark.parametrize(
    "platform, config_name, variable, instructions_name, expected_returncode",
    [
        pytest.param("claude", "claude-config", "CLAUDE_CONFIG_DIR", "CLAUDE.md", 3, id="claude"),
        pytest.param("codex", "codex-home", "CODEX_HOME", "AGENTS.md", 3, id="codex"),
    ],
)
def test_preserves_existing_unmanaged_instructions(
    tmp_path, platform, config_name, variable, instructions_name, expected_returncode
):
    config = tmp_path / config_name
    config.mkdir()
    instructions = config / instructions_name
    instructions.write_bytes(UNMANAGED_BYTES)

    result = run(platform, tmp_path, **{variable: str(config)})
    assert result.returncode == expected_returncode
    assert instructions.read_bytes() == UNMANAGED_BYTES
    assert not (config / "skills").exists()


@pytest.mark.parametrize(
    "platform, config_name, variable, instructions_name, expected_returncode, diagnostic",
    [
        pytest.param("claude", "claude-config", "CLAUDE_CONFIG_DIR", "CLAUDE.md", 0, "add this line", id="claude"),
        pytest.param("codex", "codex-home", "CODEX_HOME", "AGENTS.md", 0, "add this line", id="codex"),
    ],
)
def test_accepts_instruction_symlink_to_canonical_agents_file(
    tmp_path, platform, config_name, variable, instructions_name, expected_returncode, diagnostic
):
    config = tmp_path / config_name
    config.mkdir()
    instance_agents = tmp_path / ".agents/AGENTS.md"
    instance_agents.parent.mkdir()
    instance_agents.write_text("# Instance instructions\n")
    instructions = config / instructions_name
    instructions.symlink_to(instance_agents)

    result = run(platform, tmp_path, **{variable: str(config)})

    assert result.returncode == expected_returncode, result.stderr
    assert instructions.is_symlink()
    assert instructions.resolve() == instance_agents
    assert diagnostic not in result.stderr


def test_claude_install_retires_duplicate_agents_symlink(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    instance_agents = tmp_path / ".agents/AGENTS.md"
    instance_agents.parent.mkdir(parents=True)
    instance_agents.write_text("# Instance instructions\n")
    duplicate_agents = config / "AGENTS.md"
    duplicate_agents.symlink_to(instance_agents)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert (config / "CLAUDE.md").exists()
    assert not duplicate_agents.exists()
    assert not duplicate_agents.is_symlink()
    assert "duplicate instructions retired" in result.stdout


@pytest.mark.parametrize(
    "user_content",
    ["# Instance instructions\n", "# User instructions\nKeep this custom rule.\n"],
)
def test_claude_install_preserves_and_reports_regular_agents_file(tmp_path, user_content):
    config = tmp_path / "claude-config"
    config.mkdir()
    instance_agents = tmp_path / ".agents/AGENTS.md"
    instance_agents.parent.mkdir(parents=True)
    instance_agents.write_text("# Instance instructions\n")
    duplicate_agents = config / "AGENTS.md"
    duplicate_agents.write_text(user_content)

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert (config / "CLAUDE.md").exists()
    assert duplicate_agents.read_text() == user_content
    assert "duplicate instructions preserved" in result.stderr


def test_claude_install_preserves_symlink_to_product_doctrine(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    instance_agents = tmp_path / ".agents/AGENTS.md"
    instance_agents.parent.mkdir(parents=True)
    instance_agents.write_text("# Instance instructions\n")
    duplicate_agents = config / "AGENTS.md"
    duplicate_agents.symlink_to(ROOT / "AGENTS.md")

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert duplicate_agents.is_symlink()
    assert duplicate_agents.resolve() == (ROOT / "AGENTS.md").resolve()
    assert "duplicate instructions retired" not in result.stdout


def test_claude_install_keeps_doctrine_when_config_is_instance_root(tmp_path):
    shared = tmp_path / "shared"
    shared.mkdir()
    doctrine = shared / "AGENTS.md"
    doctrine.write_text("# Edited instance instructions\n")

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(shared),
                 AGENT_FABRIC_INSTANCE_ROOT=str(shared))

    assert doctrine.read_text() == "# Edited instance instructions\n"
    assert "duplicate instructions retired" not in result.stdout


@pytest.mark.parametrize("shape", ["direct", "hop", "dot_dot", "absolute_alias"])
def test_claude_install_keeps_agents_link_on_indirect_chain(tmp_path, shape):
    real_config = tmp_path / "claude-config"
    real_config.mkdir()
    config = real_config
    instance_agents = tmp_path / ".agents/AGENTS.md"
    instance_agents.parent.mkdir(parents=True)
    instance_agents.write_text("# Instance instructions\n")
    (real_config / "AGENTS.md").symlink_to(instance_agents)
    if shape == "direct":
        (real_config / "CLAUDE.md").symlink_to("AGENTS.md")
    elif shape == "hop":
        (real_config / "hop.md").symlink_to("AGENTS.md")
        (real_config / "CLAUDE.md").symlink_to("hop.md")
    elif shape == "dot_dot":
        (real_config / "CLAUDE.md").symlink_to("././AGENTS.md")
    else:
        config = tmp_path / "claude-alias"
        config.symlink_to(real_config)
        (real_config / "CLAUDE.md").symlink_to(real_config / "AGENTS.md")

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert (real_config / "CLAUDE.md").resolve() == instance_agents
    assert "duplicate instructions retired" not in result.stdout


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


def test_reinstall_moves_a_recorded_custom_skill_into_the_managed_library(
    tmp_path, product_template
):
    product = copy_product(product_template, tmp_path / "product")
    custom_source = tmp_path / "instance/custom-skills"
    custom_skill = custom_source / "moved-skill"
    custom_skill.mkdir(parents=True)
    (custom_skill / "SKILL.md").write_text(
        "---\nname: moved-skill\ndescription: Previously custom skill.\n---\n"
    )
    config = tmp_path / "codex-home"

    first = run_product(
        product, "codex", tmp_path, CODEX_HOME=str(config)
    )
    assert first.returncode == 0, first.stderr
    assert (config / "skills/moved-skill").resolve() == custom_skill.resolve()

    managed_skill = product / "library/moved-skill"
    managed_skill.mkdir(parents=True)
    (managed_skill / "SKILL.md").write_text(
        "---\nname: moved-skill\ndescription: Now managed skill.\n---\n"
    )

    second = run_product(
        product, "codex", tmp_path, CODEX_HOME=str(config)
    )

    assert second.returncode == 0, second.stderr
    assert (config / "skills/moved-skill").resolve() == managed_skill.resolve()
    migration_notices = [
        line
        for line in second.stdout.splitlines()
        if line.startswith("skills migrated=custom-to-managed")
    ]
    assert migration_notices == [
        f"skills migrated=custom-to-managed name=moved-skill remove={custom_skill.resolve()}"
    ]
    manifest = json.loads((config / ".agent-harness-installation.json").read_text())
    assert manifest["managed"]["moved-skill"]["source_target"] == str(
        managed_skill.resolve()
    )
    assert "moved-skill" not in manifest["custom"]

    shutil.rmtree(custom_skill)
    third = run_product(
        product, "codex", tmp_path, CODEX_HOME=str(config)
    )

    assert third.returncode == 0, third.stderr
    assert (config / "skills/moved-skill").resolve() == managed_skill.resolve()


@pytest.mark.parametrize("receipt_root", ["current", "relocated"])
def test_check_lists_stale_legacy_agy_skill_links_without_removing_them(tmp_path, receipt_root):
    legacy = tmp_path / ".gemini/config/skills"
    legacy.mkdir(parents=True)
    dangling = legacy / "dangling-skill"
    dangling.symlink_to(tmp_path / "removed-product/skills/dangling-skill")
    foreign_source = tmp_path / "user-skills/foreign-skill"
    foreign_source.mkdir(parents=True)
    (foreign_source / "SKILL.md").write_text("user-owned\n")
    foreign = legacy / "foreign-skill"
    foreign.symlink_to(foreign_source)
    current = legacy / "implement"
    current.symlink_to(ROOT / "skills" / "implement")
    (legacy.parent / ".agent-harness-installation.json").write_text(json.dumps({
        "schema_version": 1,
        "owner": "agent-harness",
        # An older install may record a previous home directory.
        "target_root": str(legacy.resolve()) if receipt_root == "current" else "/Users/previous/.gemini/config/skills",
        "updated_at": utc_timestamp(),
        "managed": {
            "retired-skill": {
                "owner": "agent-harness",
                "source_target": str(tmp_path / "old-product/skills/retired-skill"),
                "source_sha256": "0" * 64,
                "installed_at": utc_timestamp(),
                "history": [],
            }
        },
        "custom": {},
    }))

    env = os.environ.copy()
    env.update({
        "HOME": str(tmp_path),
        "HARNESS_PYTHON": sys.executable,
    })
    result = run_cli(
        [str(SCRIPT), "--check-legacy-skills"],
        cwd=ROOT,
        env=env,
    )

    assert result.returncode == 1
    assert "legacy agy skill stale=dangling-skill (dangling)" in result.stdout
    assert "legacy agy skill stale=foreign-skill (foreign)" in result.stdout
    assert "legacy agy skill retired=retired-skill (no longer in product)" in result.stdout
    assert "legacy agy skill stale=implement" not in result.stdout
    assert dangling.is_symlink()
    assert foreign.is_symlink()
    assert foreign.resolve() == foreign_source.resolve()


def test_claude_retired_agent_conflict_preflights_before_harness_mutation(tmp_path):
    config = tmp_path / "claude-config"
    agents = config / "agents"
    agents.mkdir(parents=True)
    modified_agent = agents / "retired-worker.md"
    replacement = tmp_path / "user-agent.md"
    replacement.write_text("user-owned definition\n")
    modified_agent.symlink_to(replacement)
    old_source = tmp_path / "old-product/agents/retired-worker.md"
    receipt = config / ".agent-harness-agents-installation.json"
    receipt.write_text(json.dumps({
        "schema_version": 1,
        "owner": "agent-harness",
        "surface": "claude-agents",
        "target_root": str(agents.resolve()),
        "updated_at": utc_timestamp(),
        "managed": {
            old_source.name: {
                "owner": "agent-harness",
                "source_target": str(old_source),
                "source_sha256": "0" * 64,
                "installed_at": utc_timestamp(),
            }
        },
    }))

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 3
    assert "conflicting retired managed agent target" in result.stderr
    assert modified_agent.is_symlink()
    assert modified_agent.resolve() == replacement.resolve()
    assert receipt.is_file()
    assert not (config / "skills").exists()
    assert not instance_root_for(tmp_path).exists()
    assert not (tmp_path / "bin" / "provenant").exists()


def test_rejects_a_relative_provenant_bin_directory_before_mutation(tmp_path):
    relative_bin = "relative-provenant-bin"

    result = run("codex", tmp_path, PROVENANT_BIN_DIR=relative_bin)

    assert result.returncode == 3
    assert "PROVENANT_BIN_DIR must be absolute" in result.stderr
    assert not (ROOT / relative_bin).exists()
    assert not (tmp_path / ".codex").exists()


def test_rejects_a_provenant_bin_directory_inside_the_product(tmp_path, product_template):
    product = copy_product(product_template, tmp_path / "product")
    source = product / "scripts/install-harness"
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


def test_rejects_a_missing_bin_directory_under_a_symlinked_product_parent(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
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


def test_rejects_a_byte_identical_foreign_symlink_without_clobbering_it(tmp_path):
    bin_dir = tmp_path / ".local/bin"
    bin_dir.mkdir(parents=True)
    foreign = tmp_path / "foreign/provenant"
    foreign.parent.mkdir()
    shutil.copy2(PROVENANT_BINARY, foreign)
    command = bin_dir / "provenant"
    command.symlink_to(foreign)
    original_target = command.readlink()

    result = run("codex", tmp_path)

    assert result.returncode == 3
    assert "collision" in result.stderr
    assert command.is_symlink()
    assert command.readlink() == original_target
    assert foreign.read_bytes() == PROVENANT_BINARY.read_bytes()


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
    assert command.read_bytes() == PROVENANT_BINARY.read_bytes()
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
    assert instructions.exists()


def test_split_install_rebinds_its_bootstrap_when_instance_root_changes(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
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
    tmp_path,
    platform,
    product_template,
):
    old_product = copy_product(product_template, tmp_path / "old product")
    new_product = copy_product(product_template, tmp_path / "new product")

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


def test_three_line_custom_instructions_are_not_mistaken_for_generated_bootstrap(
    tmp_path,
    product_template,
):
    old_product = copy_product(product_template, tmp_path / "old product")
    new_product = copy_product(product_template, tmp_path / "new product")
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


LEGACY_BOOTSTRAP = (
    "Read and follow `{product}/AGENTS.md` and `{product}/HARNESS.md` before making "
    "orchestration, delegation, model-routing or memory decisions. Platform/system "
    "policy and explicit user authority lead; the nearest project instruction may "
    "specialise or strengthen the global harness but may not silently broaden "
    "authority, weaken safety gates or redefine global cross-project memory policy."
)


def test_split_relocation_recognises_the_exact_legacy_fused_bootstrap(tmp_path, product_template):
    new_product = copy_product(product_template, tmp_path / "new product")
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


def test_generated_bootstrap_is_repaired_even_when_pointer_already_names_product(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
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


def test_all_primary_cutover_preflights_second_primary_before_mutating_first(
    tmp_path,
    product_template,
):
    old_product = copy_product(product_template, tmp_path / "old product")
    new_product = copy_product(product_template, tmp_path / "new product")
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


def test_all_primary_install_configures_both_surfaces_and_publishes_once(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product with spaces")

    result = run_product(product, "all", tmp_path)

    assert result.returncode == 0, result.stderr
    assert result.stdout.count("product root pointer updated=") == 1
    assert (tmp_path / "claude/skills/scope/SKILL.md").is_file()
    assert (tmp_path / "claude/workflows/implement-run.js").is_file()
    assert (tmp_path / "codex/skills/scope/SKILL.md").is_file()
    assert (tmp_path / "claude/CLAUDE.md").read_text().find(str(product / "HARNESS.md")) >= 0
    assert (tmp_path / "codex/AGENTS.md").read_text().find(str(product / "HARNESS.md")) >= 0

    claude_mcp = json.loads((tmp_path / "claude/.claude.json").read_text())
    assert not (tmp_path / "home/.claude.json").exists()
    assert not (tmp_path / "claude/settings.json").exists()
    assert not (tmp_path / "claude/settings.json").is_symlink()
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
    registered = run_cli(
        [claude_mcp["mcpServers"]["fabric"]["command"], "root"],
        cwd=clean_home,
        env=registered_environment,
    )
    assert registered.returncode == 0, registered.stderr
    assert registered.stdout == f"{product.resolve()}\n"
    assert not (tmp_path / "home/.cursor/mcp.json").exists()
    assert not (tmp_path / "home/.gemini/config/mcp_config.json").exists()
    assert not (tmp_path / "home/.kiro/settings/mcp.json").exists()
    assert not (tmp_path / "home/.config/opencode/opencode.jsonc").exists()


def test_all_primary_install_uses_selected_python_when_path_has_no_python3(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
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
    assert (tmp_path / "codex/skills/scope/SKILL.md").is_file()


@pytest.mark.parametrize("platform", ["claude", "codex"])
def test_foreign_harness_link_is_rejected_before_pointer_publish(
    tmp_path,
    platform,
    product_template,
):
    old_product = copy_product(product_template, tmp_path / "old product")
    new_product = copy_product(product_template, tmp_path / "new product")
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


def test_generated_instruction_symlink_is_rejected_without_following_external_target(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
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


def test_harness_rebind_uses_collision_safe_staging(tmp_path, product_template):
    old_product = copy_product(product_template, tmp_path / "old product")
    new_product = copy_product(product_template, tmp_path / "new product")
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


def test_relocation_rebinds_a_dangling_relative_old_harness_link(tmp_path, product_template):
    old_product = copy_product(product_template, tmp_path / "old'\\product")
    new_product = copy_product(product_template, tmp_path / "new product")
    first = run_product(old_product, "claude", tmp_path)
    assert first.returncode == 0, first.stderr

    harness = tmp_path / "claude/HARNESS.md"
    harness.symlink_to(Path("../old'\\product/HARNESS.md"))
    old_product.rename(tmp_path / "moved old product")

    result = run_product(new_product, "claude", tmp_path)

    assert result.returncode == 0, result.stderr
    assert harness.resolve() == (new_product / "HARNESS.md").resolve()


def test_relocation_matches_dangling_link_through_a_symlinked_parent(tmp_path, product_template):
    physical = tmp_path / "physical"
    physical.mkdir()
    alias = tmp_path / "alias"
    alias.symlink_to(physical, target_is_directory=True)
    old_product = copy_product(product_template, physical / "old product")
    new_product = copy_product(product_template, physical / "new product")
    first = run_product(old_product, "claude", tmp_path)
    assert first.returncode == 0, first.stderr

    harness = tmp_path / "claude/HARNESS.md"
    harness.symlink_to(Path("../alias/old product/HARNESS.md"))
    old_product.rename(physical / "moved old product")

    result = run_product(new_product, "claude", tmp_path)

    assert result.returncode == 0, result.stderr
    assert harness.resolve() == (new_product / "HARNESS.md").resolve()


def test_instance_leaf_symlink_conflicts_are_rejected_before_publication(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
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


def test_instance_config_directory_symlink_is_rejected_before_publication(
    tmp_path,
    product_template,
):
    product = copy_product(product_template, tmp_path / "product")
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


def test_claude_custom_config_dir_targets_mcp_config_and_links_settings(tmp_path):
    config = tmp_path / "claude-config"
    default_settings = tmp_path / ".claude/settings.json"
    default_settings.parent.mkdir(parents=True, exist_ok=True)
    default_settings.write_text('{"autoCompactWindow": 200000}\n')

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert (config / ".claude.json").is_file()
    assert not (tmp_path / ".claude.json").exists()
    assert (config / "settings.json").is_symlink()
    assert os.readlink(config / "settings.json") == str(default_settings)
    assert (config / "settings.json").read_text() == '{"autoCompactWindow": 200000}\n'


def test_claude_custom_config_dir_skips_settings_link_when_default_does_not_exist(tmp_path):
    config = tmp_path / "claude-config"

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert not (config / "settings.json").exists()
    assert not (config / "settings.json").is_symlink()
    assert "claude settings link skipped: target is not a regular file:" in result.stdout


def test_claude_relative_config_dir_is_rejected(tmp_path):
    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR="relative/claude")

    assert result.returncode == 3
    assert "CLAUDE_CONFIG_DIR must be absolute" in result.stderr


def test_claude_config_dir_with_trailing_slash_matching_default_does_not_link(tmp_path):
    default_dir = tmp_path / ".claude"
    default_settings = default_dir / "settings.json"
    default_settings.parent.mkdir(parents=True, exist_ok=True)
    default_settings.write_text('{"env": "default"}\n')

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=f"{default_dir}/")

    assert result.returncode == 0, result.stderr
    assert not (default_dir / "settings.json").is_symlink()


def test_claude_custom_config_dir_with_trailing_slash_links_settings(tmp_path):
    config = tmp_path / "claude-config"
    default_settings = tmp_path / ".claude/settings.json"
    default_settings.parent.mkdir(parents=True, exist_ok=True)
    default_settings.write_text('{"autoCompactWindow": 200000}\n')

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=f"{config}/")

    assert result.returncode == 0, result.stderr
    assert (config / "settings.json").is_symlink()
    assert os.readlink(config / "settings.json") == str(default_settings)


def test_claude_custom_config_dir_preserves_existing_settings_and_never_overwrites(tmp_path):
    config = tmp_path / "claude-config"
    config.mkdir()
    settings = config / "settings.json"
    settings.write_text('{"user": "preserved"}\n')

    result = run("claude", tmp_path, CLAUDE_CONFIG_DIR=str(config))

    assert result.returncode == 0, result.stderr
    assert not settings.is_symlink()
    assert settings.read_text() == '{"user": "preserved"}\n'


def test_claude_custom_config_dir_respects_explicit_claude_mcp_config(tmp_path):
    config = tmp_path / "claude-config"
    explicit_mcp = tmp_path / "custom-claude-mcp.json"

    result = run(
        "claude",
        tmp_path,
        CLAUDE_CONFIG_DIR=str(config),
        CLAUDE_MCP_CONFIG=str(explicit_mcp),
    )

    assert result.returncode == 0, result.stderr
    assert explicit_mcp.is_file()
    assert not (config / ".claude.json").exists()
    assert not (tmp_path / ".claude.json").exists()
    registration = json.loads(explicit_mcp.read_text())["mcpServers"]["fabric"]
    assert registration["env"]["AGENT_FABRIC_CLIENT_LABEL"] == "claude"


def test_claude_default_config_dir_does_not_link_settings_to_itself(tmp_path):
    result = run("claude", tmp_path)

    assert result.returncode == 0, result.stderr
    assert not (tmp_path / ".claude/settings.json").is_symlink()
    assert not (tmp_path / ".claude/settings.json").exists()
    assert (tmp_path / ".claude.json").is_file()


# --- Rust front door selection (ADR 0027) -----------------------------------

MANAGED_MARKER_LINE = (
    "# Managed by scripts/install-harness; "
    "local edits to an installed copy are not preserved."
)


def add_fake_crate(product: Path) -> Path:
    """A minimal crate whose sources hash to something; cargo is faked separately."""
    crate = product / "tools/front-door"
    # The product tree may already carry the real crate; the fake replaces it.
    shutil.rmtree(crate, ignore_errors=True)
    (crate / "src").mkdir(parents=True)
    (crate / "Cargo.toml").write_text(
        '[workspace]\nmembers = [".", "crates/*"]\n'
        '[package]\nname = "provenant"\nversion = "0.0.0"\n'
    )
    (crate / "Cargo.lock").write_text("# lock\n")
    (crate / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.94.1"\n')
    (crate / "src/main.rs").write_text("fn main() {}\n")
    (crate / "crates/core/src").mkdir(parents=True)
    (crate / "crates/core/Cargo.toml").write_text('[package]\nname = "provenant-core"\n')
    (crate / "crates/core/src/lib.rs").write_text("// library\n")
    (crate / "crates/core/build.rs").write_text("fn main() {}\n")
    return crate


def fake_binary_text(digest: str) -> str:
    # The identity lines follow `exit`, so the shell never parses them as commands.
    return (
        "#!/bin/sh\n"
        'case "$1" in help) echo "usage: provenant <command> [arguments...]"; exit 0;; esac\n'
        "exit 2\n"
        f"\n{MANAGED_MARKER_LINE}\n"
        f"provenant-front-door source-sha256={digest}\n"
    )


def install_fake_cargo(directory: Path, *, fail: bool = False) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    cargo = directory / "cargo"
    log = directory / "cargo.log"
    body = (
        "#!/bin/sh\n"
        f'echo "$@" >> "{log}"\n'
        f'echo "MACOSX_DEPLOYMENT_TARGET=${{MACOSX_DEPLOYMENT_TARGET-unset}}" >> "{log}"\n'
        f'echo "CARGO_TARGET_DIR=$CARGO_TARGET_DIR" >> "{log}"\n'
    )
    if fail:
        body += "exit 101\n"
    else:
        body += (
            'mkdir -p "$CARGO_TARGET_DIR/release"\n'
            'printf \'%s\\n\' \'#!/bin/sh\' '
            "'case \"$1\" in help) echo \"usage: provenant <command> [arguments...]\"; exit 0;; esac' "
            "'exit 2' '' "
            f"'{MANAGED_MARKER_LINE}' "
            '"provenant-front-door source-sha256=$PROVENANT_FD_SOURCE_SHA256" '
            '> "$CARGO_TARGET_DIR/release/provenant"\n'
            'chmod 755 "$CARGO_TARGET_DIR/release/provenant"\n'
        )
    cargo.write_text(body)
    cargo.chmod(0o755)
    return log


@pytest.fixture
def rust_product(tmp_path, product_template):
    product = copy_product(product_template, tmp_path / "product")
    add_fake_crate(product)
    return product


def rust_env(tmp_path: Path, **extra: str) -> dict[str, str]:
    fake_bin = tmp_path / "fake-bin"
    return {"_build_front_door": True, "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}", **extra}


def test_rust_mode_builds_stamps_and_installs_the_binary(tmp_path, rust_product):
    log = install_fake_cargo(tmp_path / "fake-bin")
    from lib.front_door_build import source_digest

    result = run_product(
        rust_product, "codex", tmp_path, **rust_env(tmp_path, PROVENANT_FRONT_DOOR="rust")
    )

    assert result.returncode == 0, result.stderr
    installed = (tmp_path / "bin/provenant").read_text()
    assert f"provenant-front-door source-sha256={source_digest(rust_product / 'tools/front-door')}\n" in installed
    assert MANAGED_MARKER_LINE in installed.splitlines()
    assert os.access(tmp_path / "bin/provenant", os.X_OK)
    assert f"command installed={tmp_path / 'bin/provenant'}" in result.stdout
    assert "front-door=rust" in result.stdout.splitlines()
    cargo_log = log.read_text()
    assert "build --release --locked --manifest-path" in cargo_log
    if sys.platform == "darwin":
        assert "MACOSX_DEPLOYMENT_TARGET=15.0" in cargo_log


def test_default_builds_rust_into_configured_persistent_target_directory(tmp_path, rust_product):
    log = install_fake_cargo(tmp_path / "fake-bin")
    target = tmp_path / "cargo-target"

    result = run_product(rust_product, "codex", tmp_path,
                         **rust_env(tmp_path, CARGO_TARGET_DIR=str(target)))

    assert result.returncode == 0, result.stderr
    assert "front-door=rust" in result.stdout.splitlines()
    assert f"CARGO_TARGET_DIR={target}" in log.read_text()


def test_reinstalling_the_same_binary_is_idempotent(tmp_path, rust_product):
    install_fake_cargo(tmp_path / "fake-bin")
    environment = rust_env(tmp_path, PROVENANT_FRONT_DOOR="rust")
    first = run_product(rust_product, "codex", tmp_path, **environment)
    second = run_product(rust_product, "codex", tmp_path, **environment)

    assert first.returncode == 0 and second.returncode == 0, second.stderr
    assert f"command existing={tmp_path / 'bin/provenant'}" in second.stdout


def test_rust_mode_fails_without_installing_when_the_build_fails(tmp_path, rust_product):
    install_fake_cargo(tmp_path / "fake-bin", fail=True)

    result = run_product(
        rust_product, "codex", tmp_path, **rust_env(tmp_path, PROVENANT_FRONT_DOOR="rust")
    )

    assert result.returncode == 3
    assert "cargo build failed" in result.stderr
    assert not (tmp_path / "bin/provenant").exists()


def test_prebuilt_binary_with_matching_sources_is_installed_without_cargo(tmp_path, rust_product):
    from lib.front_door_build import source_digest

    prebuilt = tmp_path / "prebuilt/provenant"
    prebuilt.parent.mkdir()
    prebuilt.write_text(fake_binary_text(source_digest(rust_product / "tools/front-door")))
    prebuilt.chmod(0o755)
    install_fake_cargo(tmp_path / "fake-bin", fail=True)  # would fail if it were used

    result = run_product(
        rust_product,
        "codex",
        tmp_path,
        **rust_env(tmp_path, PROVENANT_FRONT_DOOR="rust", PROVENANT_FRONT_DOOR_BINARY=str(prebuilt)),
    )

    assert result.returncode == 0, result.stderr
    assert (tmp_path / "bin/provenant").read_bytes() == prebuilt.read_bytes()
    assert not (tmp_path / "fake-bin/cargo.log").exists()


@pytest.mark.parametrize("change_library", [False, True])
def test_prebuilt_binary_from_other_sources_is_refused(
    tmp_path, rust_product, change_library
):
    from lib.front_door_build import source_digest

    prebuilt = tmp_path / "prebuilt/provenant"
    prebuilt.parent.mkdir()
    digest = source_digest(rust_product / "tools/front-door") if change_library else "0" * 64
    prebuilt.write_text(fake_binary_text(digest))
    prebuilt.chmod(0o755)
    if change_library:
        (rust_product / "tools/front-door/crates/core/src/lib.rs").write_text("// changed library\n")
    environment = {"PROVENANT_FRONT_DOOR_BINARY": str(prebuilt)}

    strict = run_product(
        rust_product, "codex", tmp_path, **rust_env(tmp_path, PROVENANT_FRONT_DOOR="rust", **environment)
    )
    assert strict.returncode == 3
    assert "built from different sources" in strict.stderr
    assert not (tmp_path / "bin/provenant").exists()
    assert not (tmp_path / "instance").exists()
    assert not (tmp_path / "codex").exists()


def test_unknown_front_door_mode_is_refused_before_any_mutation(tmp_path, rust_product):
    result = run_product(rust_product, "codex", tmp_path, PROVENANT_FRONT_DOOR="fast")

    assert result.returncode == 3
    assert "PROVENANT_FRONT_DOOR must be rust" in result.stderr
    assert not (tmp_path / "bin/provenant").exists()


def test_check_reports_an_unreadable_legacy_agy_receipt_and_still_lists_links(tmp_path):
    legacy = tmp_path / ".gemini/config/skills"
    legacy.mkdir(parents=True)
    (legacy / "dangling-skill").symlink_to(tmp_path / "removed-product/skills/dangling-skill")
    (legacy.parent / ".agent-harness-installation.json").write_text("not json\n")
    env = {**os.environ, "HOME": str(tmp_path), "HARNESS_PYTHON": sys.executable}
    result = run_cli([str(SCRIPT), "--check-legacy-skills"], cwd=ROOT, env=env)
    assert result.returncode == 1
    assert "legacy agy skill stale=dangling-skill (dangling)" in result.stdout
    assert "legacy agy skill stale=.agent-harness-installation.json (unreadable receipt)" in result.stdout


def test_installs_optional_utility_output_hook_and_removes_only_its_entry(tmp_path):
    config = tmp_path / 'claude-config'
    config.mkdir()
    settings = config / 'settings.json'
    foreign = {'matcher': 'Read', 'hooks': [{'type': 'command', 'command': 'keep'}]}
    settings.write_text(json.dumps({'hooks': {'PostToolUse': [foreign]}, 'user': 'preserved'}))
    result = run('claude', tmp_path, '--utility-output-hooks', CLAUDE_CONFIG_DIR=str(config))
    assert result.returncode == 0, result.stderr
    data = json.loads(settings.read_text())
    assert data['user'] == 'preserved'
    assert data['hooks']['PostToolUse'][0] == foreign
    assert data['hooks']['PostToolUse'][1]['matcher'] == 'Bash'
    assert 'utility_output_hook.py' in data['hooks']['PostToolUse'][1]['hooks'][0]['command']
    result = run('claude', tmp_path, '--no-utility-output-hooks', CLAUDE_CONFIG_DIR=str(config))
    assert result.returncode == 0, result.stderr
    assert json.loads(settings.read_text()) == {'hooks': {'PostToolUse': [foreign]}, 'user': 'preserved'}


@pytest.mark.parametrize('flag', ['--utility-output-hooks', '--no-utility-output-hooks'])
def test_check_mode_rejects_utility_hook_changes(tmp_path, flag):
    result = run('', tmp_path, '--check-legacy-skills', flag)
    assert result.returncode == 2
    assert 'usage: install-harness' in result.stderr


@pytest.mark.parametrize("prebuilt", [False, True])
def test_rust_install_uses_private_snapshot_after_classification(tmp_path, rust_product, prebuilt):
    from lib.front_door_build import source_digest

    log = install_fake_cargo(tmp_path / "fake-bin", fail=prebuilt)
    target = tmp_path / "cargo-target"
    original_source = target / "release/provenant"
    environment = rust_env(tmp_path, PROVENANT_FRONT_DOOR="rust", CARGO_TARGET_DIR=str(target))
    if prebuilt:
        original_source = tmp_path / "prebuilt/provenant"
        original_source.parent.mkdir()
        original_source.write_text(fake_binary_text(source_digest(rust_product / "tools/front-door")))
        original_source.chmod(0o755)
        environment["PROVENANT_FRONT_DOOR_BINARY"] = str(original_source)
    publisher = rust_product / "scripts/install-provenant-command.py"
    publisher.write_text(publisher.read_text().replace(
        'result = classify(args.source, args.destination)',
        'result = classify(args.source, args.destination)\n'
        f'            Path({str(tmp_path / "selected-source")!r}).write_text(str(args.source))\n'
        f'            Path({str(original_source)!r}).write_text("swapped by another install")',
    ))

    result = run_product(rust_product, "codex", tmp_path, **environment)

    assert result.returncode == 0, result.stderr
    installed = (tmp_path / "bin/provenant").read_text()
    assert source_digest(rust_product / "tools/front-door") in installed
    assert original_source.read_text() == "swapped by another install"
    if prebuilt:
        assert installed == fake_binary_text(source_digest(rust_product / "tools/front-door"))
        assert not log.exists()
    source = Path((tmp_path / "selected-source").read_text())
    assert source != original_source
    assert not source.parent.exists()  # the install's EXIT trap removes its private snapshot


def test_missing_cargo_exits_three_with_rustup_fix_before_installing(tmp_path, rust_product):
    result = run_product(
        rust_product, "codex", tmp_path,
        PATH="/usr/bin:/bin", PROVENANT_FRONT_DOOR_BINARY="", _build_front_door=True,
    )

    assert result.returncode == 3
    assert "cargo is not on PATH" in result.stderr
    assert "fix: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" in result.stderr.splitlines()
    assert not (tmp_path / "bin/provenant").exists()
    assert not (tmp_path / "instance/.agent-fabric/product-root.json").exists()
    for destination in ("bin", "instance", "codex", "claude", "home"):
        assert not (tmp_path / destination).exists(), destination
