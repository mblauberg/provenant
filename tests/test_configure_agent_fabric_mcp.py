import json
import importlib.util
import io
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tomllib

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "configure-agent-fabric-mcp.py"
PROVENANT_TEMPLATE = ROOT / "scripts" / "provenant.template"


def stable_shim(tmp_path: Path) -> Path:
    shim = tmp_path / "bin/provenant"
    shim.parent.mkdir(exist_ok=True)
    if not shim.exists():
        shutil.copy2(PROVENANT_TEMPLATE, shim)
        shim.chmod(0o755)
    return shim


def load_configurer():
    spec = importlib.util.spec_from_file_location("configure_agent_fabric_mcp", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def run_configure(tmp_path: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    shim = stable_shim(tmp_path)
    return subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--agents-home", str(ROOT),
            "--state-directory", str(tmp_path / "state"),
            "--shim-path", str(shim),
            "--claude-config", str(tmp_path / "claude.json"),
            "--codex-config", str(tmp_path / "codex.toml"),
            "--cursor-config", str(tmp_path / "cursor.json"),
            "--agy-config", str(tmp_path / "agy.json"),
            "--kiro-config", str(tmp_path / "kiro.json"),
            "--opencode-config", str(tmp_path / "opencode.jsonc"),
            *arguments,
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )


def run_configure_with_closed_stdout(
    tmp_path: Path, *arguments: str,
) -> tuple[int, str]:
    paths = all_client_paths(tmp_path)
    read_descriptor, write_descriptor = os.pipe()
    os.close(read_descriptor)
    process = subprocess.Popen(
        [sys.executable, str(SCRIPT), *all_client_arguments(tmp_path, paths), *arguments],
        cwd=ROOT,
        stdout=write_descriptor,
        stderr=subprocess.PIPE,
        text=True,
    )
    os.close(write_descriptor)
    _, error_output = process.communicate(timeout=10)
    return process.returncode, error_output


def all_client_paths(tmp_path: Path) -> dict[str, Path]:
    return {
        "claude": tmp_path / "claude.json",
        "codex": tmp_path / "codex.toml",
        "cursor": tmp_path / "cursor.json",
        "agy": tmp_path / "agy.json",
        "kiro": tmp_path / "kiro.json",
        "opencode": tmp_path / "opencode.jsonc",
    }


def all_client_arguments(tmp_path: Path, paths: dict[str, Path]) -> list[str]:
    return [
        "--agents-home", str(ROOT),
        "--state-directory", str(tmp_path / "state"),
        "--shim-path", str(stable_shim(tmp_path)),
        "--claude-config", str(paths["claude"]),
        "--codex-config", str(paths["codex"]),
        "--cursor-config", str(paths["cursor"]),
        "--agy-config", str(paths["agy"]),
        "--kiro-config", str(paths["kiro"]),
        "--opencode-config", str(paths["opencode"]),
    ]


def test_configures_all_global_clients_without_a_fixed_project_path(tmp_path: Path) -> None:
    shim = stable_shim(tmp_path)
    claude_config = tmp_path / "claude.json"
    codex_config = tmp_path / "codex.toml"
    json_configs = {
        client: tmp_path / f"{client}.json"
        for client in ("cursor", "agy", "kiro")
    }
    opencode_config = tmp_path / "opencode.jsonc"
    claude_config.write_text(json.dumps({
        "unrelatedSecret": "never-print-claude",
        "mcpServers": {
            "other": {"command": "other"},
            "agent-fabric": {
                "command": "/old/proxy",
                "env": {"AGENT_FABRIC_PROJECT_PATH": "/wrong/project"},
            },
        },
    }))
    codex_config.write_text("""
[custom]
secret = "never-print-codex"

[mcp_servers.agent-fabric]
command = "/old/proxy"

[mcp_servers.agent-fabric.env]
AGENT_FABRIC_PROJECT_PATH = "/wrong/project"
AGENT_FABRIC_CAPABILITY = "never-print-capability"
""")
    for path in json_configs.values():
        path.write_text(json.dumps({
            "unrelatedSecret": "never-print-optional",
            "mcpServers": {
                "other": {"command": "other"},
                "agent-fabric": {
                    "command": "/old/proxy",
                    "env": {"AGENT_FABRIC_PROJECT_PATH": "/wrong/project"},
                },
            },
        }))
    opencode_config.write_text(json.dumps({
        "unrelatedSecret": "never-print-opencode",
        "mcp": {"other": {"type": "remote", "url": "https://example.invalid"}},
    }))

    result = run_configure(tmp_path)

    assert result.returncode == 0, result.stderr
    claude = json.loads(claude_config.read_text())
    codex = tomllib.loads(codex_config.read_text())
    expected_common = {
        "AGENT_FABRIC_STATE_DIRECTORY": str(tmp_path / "state"),
    }
    assert claude["mcpServers"]["agent-fabric"] == {
        "type": "stdio",
        "command": str(shim),
        "args": [],
        "env": {**expected_common, "AGENT_FABRIC_SEAT": "claude", "AGENT_FABRIC_CLIENT_LABEL": "claude"},
    }
    assert codex["mcp_servers"]["agent-fabric"] == {
        "command": str(shim),
        "env": {**expected_common, "AGENT_FABRIC_SEAT": "codex", "AGENT_FABRIC_CLIENT_LABEL": "codex"},
    }
    for client, path in json_configs.items():
        value = json.loads(path.read_text())
        assert value["mcpServers"]["agent-fabric"] == {
            "command": str(shim),
            "env": {**expected_common, "AGENT_FABRIC_SEAT": "codex", "AGENT_FABRIC_CLIENT_LABEL": client},
        }
        assert value["mcpServers"]["other"] == {"command": "other"}
        assert value["unrelatedSecret"] == "never-print-optional"
    opencode = json.loads(opencode_config.read_text())
    assert opencode["mcp"]["agent-fabric"] == {
        "type": "local",
        "command": [str(shim)],
        "enabled": True,
        "environment": {
            **expected_common,
            "AGENT_FABRIC_SEAT": "codex",
            "AGENT_FABRIC_CLIENT_LABEL": "opencode",
        },
    }
    assert opencode["mcp"]["other"] == {"type": "remote", "url": "https://example.invalid"}
    assert opencode["unrelatedSecret"] == "never-print-opencode"
    assert claude["mcpServers"]["other"] == {"command": "other"}
    assert claude["unrelatedSecret"] == "never-print-claude"
    assert codex["custom"] == {"secret": "never-print-codex"}
    rendered = result.stdout + result.stderr
    assert "AGENT_FABRIC_PROJECT_PATH" not in rendered
    assert "AGENT_FABRIC_CAPABILITY" not in rendered
    assert "never-print" not in rendered
    receipts = [line for line in result.stdout.splitlines() if "configured platform=" in line]
    assert [line.split("platform=", 1)[1].split(" ", 1)[0] for line in receipts] == [
        "claude", "codex", "cursor", "agy", "kiro", "opencode",
    ]
    assert str(ROOT) not in json.dumps([
        claude["mcpServers"]["agent-fabric"],
        codex["mcp_servers"]["agent-fabric"],
        *(json.loads(path.read_text())["mcpServers"]["agent-fabric"] for path in json_configs.values()),
        opencode["mcp"]["agent-fabric"],
    ])

    original_claude = claude_config.read_bytes()
    original_codex = codex_config.read_bytes()
    original_json = {client: path.read_bytes() for client, path in json_configs.items()}
    original_opencode = opencode_config.read_bytes()
    second = run_configure(tmp_path)
    assert second.returncode == 0, second.stderr
    assert claude_config.read_bytes() == original_claude
    assert codex_config.read_bytes() == original_codex
    assert {client: path.read_bytes() for client, path in json_configs.items()} == original_json
    assert opencode_config.read_bytes() == original_opencode


def test_stable_registration_launches_after_product_relocation(tmp_path: Path) -> None:
    configurer = load_configurer()
    shim = stable_shim(tmp_path)
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    old_product = tmp_path / "old-product"
    wrapper = old_product / "scripts/agent-fabric-mcp"
    wrapper.parent.mkdir(parents=True)
    wrapper.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$AGENT_FABRIC_SEAT|$AGENT_FABRIC_STATE_DIRECTORY|$AGENTS_HOME\"\n"
    )
    wrapper.chmod(0o755)
    registration = configurer.registration(
        old_product,
        tmp_path / "state",
        "codex",
        shim_path=shim,
    )
    registered_command = registration["command"]
    original_registration = json.dumps(registration, sort_keys=True)
    pointer.write_text(json.dumps({
        "schema_version": 1,
        "product_root": str(old_product),
    }))

    relocated_product = tmp_path / "relocated-product"
    old_product.rename(relocated_product)
    pointer.write_text(json.dumps({
        "schema_version": 1,
        "product_root": str(relocated_product),
    }))
    environment = {
        **{
            key: value for key, value in os.environ.items()
            if key not in {
                "AGENT_FABRIC_PRODUCT_ROOT",
                "AGENT_FABRIC_INSTANCE_ROOT",
                "AGENTS_HOME",
            }
        },
        **registration["env"],
        "HOME": str(tmp_path / "home"),
        "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
    }
    launched = subprocess.run(
        [registered_command],
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )

    assert registration["command"] == str(shim)
    assert str(old_product) not in json.dumps(registration)
    assert json.dumps(registration, sort_keys=True) == original_registration
    assert "AGENT_FABRIC_PRODUCT_ROOT" not in environment
    assert "AGENTS_HOME" not in environment
    assert launched.returncode == 0, launched.stderr
    assert launched.stdout == f"codex|{tmp_path / 'state'}|{relocated_product}\n"


def test_stable_registration_reports_repair_for_missing_product(tmp_path: Path) -> None:
    configurer = load_configurer()
    shim = stable_shim(tmp_path)
    registration = configurer.registration(
        tmp_path / "unused-product",
        tmp_path / "state",
        "codex",
        shim_path=shim,
    )
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in {
            "AGENT_FABRIC_PRODUCT_ROOT",
            "AGENT_FABRIC_INSTANCE_ROOT",
            "AGENTS_HOME",
        }
    }
    environment.update(registration["env"])
    environment["HOME"] = str(tmp_path / "home")

    launched = subprocess.run(
        [registration["command"]],
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )

    assert launched.returncode != 0
    assert "Agent Fabric MCP target is missing or not executable" in launched.stderr
    assert "repair: re-run install-harness from the product checkout" in launched.stderr


def test_present_empty_seat_still_enters_mcp_mode(tmp_path: Path) -> None:
    shim = stable_shim(tmp_path)
    product = tmp_path / "product"
    wrapper = product / "scripts/agent-fabric-mcp"
    wrapper.parent.mkdir(parents=True)
    wrapper.write_text("#!/bin/sh\nprintf 'mcp-mode:%s\\n' \"$AGENT_FABRIC_SEAT\"\n")
    wrapper.chmod(0o755)
    environment = {
        **os.environ,
        "AGENT_FABRIC_PRODUCT_ROOT": str(product),
        "AGENT_FABRIC_SEAT": "",
    }

    launched = subprocess.run(
        [str(shim)],
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )

    assert launched.returncode == 0, launched.stderr
    assert launched.stdout == "mcp-mode:\n"


def test_rejects_a_relative_stable_shim_path(tmp_path: Path) -> None:
    result = run_configure(tmp_path, "--shim-path", "relative/provenant")

    assert result.returncode == 3
    assert "stable Provenant shim path must be absolute" in result.stderr
    assert not any(path.exists() for path in all_client_paths(tmp_path).values())


def test_check_rejects_a_missing_stable_shim(tmp_path: Path) -> None:
    configured = run_configure(tmp_path)
    assert configured.returncode == 0, configured.stderr
    missing = tmp_path / "missing/provenant"

    checked = run_configure(tmp_path, "--shim-path", str(missing), "--check")

    assert checked.returncode == 3
    assert f"stable Provenant shim is missing or not executable at {missing}" in checked.stderr


def test_configuration_does_not_require_wrapper_under_agents_home(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance-without-product"
    instance_root.mkdir()

    configured = run_configure(
        tmp_path,
        "--platform", "codex",
        "--agents-home", str(instance_root),
    )

    assert configured.returncode == 0, configured.stderr
    assert "Agent Fabric MCP wrapper is missing from AGENTS_HOME" not in configured.stderr


def test_check_reports_only_agent_fabric_entry_status(tmp_path: Path) -> None:
    configured = run_configure(tmp_path)
    assert configured.returncode == 0, configured.stderr
    checked = run_configure(tmp_path, "--check")
    assert checked.returncode == 0, checked.stderr
    assert "agent-fabric MCP verified platform=claude" in checked.stdout
    assert "agent-fabric MCP verified platform=codex" in checked.stdout
    for client in ("cursor", "agy", "kiro", "opencode"):
        assert f"agent-fabric MCP verified platform={client}" in checked.stdout
    assert "AGENT_FABRIC_" not in checked.stdout + checked.stderr


def test_opencode_commented_jsonc_fails_closed_without_rewriting(tmp_path: Path) -> None:
    config = tmp_path / "opencode.jsonc"
    original = '{\n  // user comment\n  "mcp": {}\n}\n'
    config.write_text(original)

    result = run_configure(tmp_path, "--platform", "opencode")

    assert result.returncode == 3
    assert "OpenCode config is invalid JSON" in result.stderr
    assert config.read_text() == original


def test_preflight_rejects_malformed_codex_without_mutating_claude(tmp_path: Path) -> None:
    claude_config = tmp_path / "claude.json"
    codex_config = tmp_path / "codex.toml"
    original = '{"unrelatedSecret":"preserved"}\n'
    claude_config.write_text(original)
    codex_config.write_text("[mcp_servers.agent-fabric\n")

    result = run_configure(tmp_path, "--preflight")

    assert result.returncode == 3
    assert "invalid TOML" in result.stderr
    assert "preserved" not in result.stdout + result.stderr
    assert claude_config.read_text() == original


def test_rejects_inline_codex_entry_instead_of_rewriting_ambiguous_toml(tmp_path: Path) -> None:
    codex_config = tmp_path / "codex.toml"
    original = '[mcp_servers]\nagent-fabric = { command = "/old" }\n'
    codex_config.write_text(original)

    result = run_configure(tmp_path, "--platform", "codex")

    assert result.returncode == 3
    assert "unsupported inline or quoted table form" in result.stderr
    assert codex_config.read_text() == original


@pytest.mark.parametrize("client", ["claude", "codex"])
@pytest.mark.parametrize("drift", ["absent-created", "content", "symlink"])
def test_write_rejects_config_source_drift(tmp_path: Path, client: str, drift: str) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    original = '{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n'
    external = '{"external":true}\n' if client == "claude" else '[external]\nvalue = true\n'

    if drift == "absent-created":
        proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
        requested.write_text(external)
        protected = requested
    elif drift == "content":
        requested.write_text(original)
        proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
        requested.write_text(external)
        protected = requested
    else:
        first = tmp_path / f"first.{suffix}"
        second = tmp_path / f"second.{suffix}"
        first.write_text(original)
        second.write_text(external)
        requested.symlink_to(first)
        proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
        requested.unlink()
        requested.symlink_to(second)
        protected = second

    with pytest.raises(configurer.RegistrationConflictError, match=f"{client.title()} config changed"):
        configurer.write_proposal(proposal)
    assert protected.read_text() == external


def test_platform_all_revalidates_codex_after_writing_claude(tmp_path: Path, monkeypatch, capsys) -> None:
    configurer = load_configurer()
    claude_config = tmp_path / "claude.json"
    codex_config = tmp_path / "codex.toml"
    claude_config.write_text('{}\n')
    codex_config.write_text('[unrelated]\nvalue = "before"\n')
    external = '[external]\nvalue = "during-claude-write"\n'
    write_proposal = configurer.write_proposal

    def interleaved_write(proposal):
        write_proposal(proposal)
        if proposal.client == "claude":
            codex_config.write_text(external)

    monkeypatch.setattr(configurer, "write_proposal", interleaved_write)
    result = configurer.main([
        "--agents-home", str(ROOT),
        "--state-directory", str(tmp_path / "state"),
        "--shim-path", str(stable_shim(tmp_path)),
        "--claude-config", str(claude_config),
        "--codex-config", str(codex_config),
    ])

    captured = capsys.readouterr()
    assert result == 4
    assert "partial-state: agent-fabric MCP registration" in captured.err
    assert "committed=claude" in captured.err
    assert "remaining=codex,cursor,agy,kiro,opencode" in captured.err
    assert "Codex config changed" in captured.err
    assert codex_config.read_text() == external
    assert json.loads(claude_config.read_text())["mcpServers"]["agent-fabric"]["env"]["AGENT_FABRIC_SEAT"] == "claude"


@pytest.mark.parametrize("failure", ["post-exchange-mismatch", "post-link-fsync"])
def test_first_client_post_install_failure_reports_partial_state(
    tmp_path: Path, monkeypatch, capsys, failure: str,
) -> None:
    configurer = load_configurer()
    config = tmp_path / "claude.json"
    if failure == "post-exchange-mismatch":
        config.write_text('{}\n')
        atomic_exchange = configurer.atomic_exchange

        def interleaved_exchange(first: Path, second: Path) -> None:
            replacement = tmp_path / "external.json"
            replacement.write_text('{"external":true}\n')
            os.replace(replacement, second)
            atomic_exchange(first, second)

        monkeypatch.setattr(configurer, "atomic_exchange", interleaved_exchange)
    else:
        fsync_directory = configurer._fsync_directory
        calls = 0

        def fail_first_fsync(path: Path) -> None:
            nonlocal calls
            calls += 1
            if calls == 1:
                raise OSError("injected post-link durability failure")
            fsync_directory(path)

        monkeypatch.setattr(configurer, "_fsync_directory", fail_first_fsync)

    result = configurer.main([
        "--platform", "claude",
        "--agents-home", str(ROOT),
        "--state-directory", str(tmp_path / "state"),
        "--shim-path", str(stable_shim(tmp_path)),
        "--claude-config", str(config),
    ])

    captured = capsys.readouterr()
    assert result == 4
    assert "partial-state: agent-fabric MCP registration" in captured.err
    assert "committed=none" in captured.err
    assert "remaining=claude" in captured.err
    assert "agent-fabric" in config.read_text()
    assert len(list(tmp_path.glob(".claude.json.recovery.*/*"))) == 1


def test_platform_all_revalidates_an_existing_client_after_an_earlier_commit(
    tmp_path: Path, monkeypatch, capsys,
) -> None:
    configurer = load_configurer()
    paths = all_client_paths(tmp_path)
    arguments = all_client_arguments(tmp_path, paths)
    assert configurer.main(arguments) == 0
    capsys.readouterr()
    paths["claude"].write_text('{}\n')
    external = '[external]\nvalue = "during-claude-write"\n'
    write_proposal = configurer.write_proposal

    def interleaved_write(proposal):
        write_proposal(proposal)
        if proposal.client == "claude":
            paths["codex"].write_text(external)

    monkeypatch.setattr(configurer, "write_proposal", interleaved_write)

    result = configurer.main(arguments)

    captured = capsys.readouterr()
    assert result == 4
    assert captured.out.splitlines() == [
        f"agent-fabric MCP configured platform=claude config={paths['claude']}"
    ]
    assert "partial-state: agent-fabric MCP registration" in captured.err
    assert "cause=config-conflict" in captured.err
    assert "committed=claude" in captured.err
    assert "remaining=codex,cursor,agy,kiro,opencode" in captured.err
    assert "Codex config changed" in captured.err
    assert paths["codex"].read_text() == external


def test_existing_drift_before_any_commit_uses_non_partial_conflict(
    tmp_path: Path, monkeypatch, capsys,
) -> None:
    configurer = load_configurer()
    config = tmp_path / "claude.json"
    arguments = [
        "--platform", "claude",
        "--agents-home", str(ROOT),
        "--state-directory", str(tmp_path / "state"),
        "--shim-path", str(stable_shim(tmp_path)),
        "--claude-config", str(config),
    ]
    assert configurer.main(arguments) == 0
    capsys.readouterr()
    external = '{"external":"after-compose"}\n'
    claude_update = configurer.claude_update

    def interleaved_compose(path, desired):
        proposal = claude_update(path, desired)
        config.write_text(external)
        return proposal

    monkeypatch.setattr(configurer, "claude_update", interleaved_compose)

    result = configurer.main(arguments)

    captured = capsys.readouterr()
    assert result == 3
    assert "conflicting: Claude config changed after registration was composed" in captured.err
    assert "partial-state" not in captured.err
    assert captured.out == ""
    assert config.read_text() == external


@pytest.mark.parametrize("mode", ["--check", "--preflight"])
def test_read_only_modes_revalidate_existing_snapshots_before_success(
    tmp_path: Path, monkeypatch, capsys, mode: str,
) -> None:
    configurer = load_configurer()
    paths = all_client_paths(tmp_path)
    arguments = all_client_arguments(tmp_path, paths)
    assert configurer.main(arguments) == 0
    capsys.readouterr()
    external = '{"external":"after-claude-snapshot"}\n'
    opencode_update = configurer.opencode_update

    def interleaved_compose(path, desired):
        proposal = opencode_update(path, desired)
        paths["claude"].write_text(external)
        return proposal

    monkeypatch.setattr(configurer, "opencode_update", interleaved_compose)

    result = configurer.main([*arguments, mode])

    captured = capsys.readouterr()
    assert result == 3
    assert "Claude config changed after registration was composed" in captured.err
    assert "partial-state" not in captured.err
    assert captured.out == ""
    assert paths["claude"].read_text() == external


def test_platform_all_reports_commits_and_typed_recovery_on_late_conflict(
    tmp_path: Path, monkeypatch, capsys,
) -> None:
    configurer = load_configurer()
    paths = all_client_paths(tmp_path)
    for client, path in paths.items():
        path.write_text('[unrelated]\nvalue = "before"\n' if client == "codex" else '{"unrelated":"before"}\n')
    write_proposal = configurer.write_proposal
    external = '{"unrelated":"concurrent-opencode-write"}\n'

    def interleaved_write(proposal):
        write_proposal(proposal)
        if proposal.client == "kiro":
            paths["opencode"].write_text(external)

    monkeypatch.setattr(configurer, "write_proposal", interleaved_write)
    arguments = all_client_arguments(tmp_path, paths)
    result = configurer.main(arguments)

    captured = capsys.readouterr()
    assert result == 4
    receipts = captured.out.splitlines()
    assert [line.split("platform=", 1)[1].split(" ", 1)[0] for line in receipts] == [
        "claude", "codex", "cursor", "agy", "kiro",
    ]
    assert all("agent-fabric MCP configured" in line for line in receipts)
    assert "partial-state: agent-fabric MCP registration" in captured.err
    assert "committed=claude,codex,cursor,agy,kiro" in captured.err
    assert "remaining=opencode" in captured.err
    assert "OpenCode config changed" in captured.err
    assert "reconcile the reported configuration and any recovery file, then rerun --platform all" in captured.err
    assert paths["opencode"].read_text() == external
    for client in ("claude", "codex", "cursor", "agy", "kiro"):
        assert "agent-fabric" in paths[client].read_text()

    recovered = configurer.main(arguments)
    recovery_output = capsys.readouterr()
    assert recovered == 0, recovery_output.err
    assert '"unrelated": "concurrent-opencode-write"' in paths["opencode"].read_text()
    assert "agent-fabric" in paths["opencode"].read_text()
    assert "configured platform=opencode" in recovery_output.out


@pytest.mark.parametrize("failure", ["write", "flush", "closed"])
def test_committed_receipt_output_failure_reports_partial_state_and_stops(
    tmp_path: Path, monkeypatch, failure: str,
) -> None:
    configurer = load_configurer()
    paths = all_client_paths(tmp_path)
    paths["claude"].write_text('{}\n')
    paths["codex"].write_text('[unrelated]\nvalue = "before"\n')
    for client in ("cursor", "agy", "kiro", "opencode"):
        paths[client].write_text('{}\n')
    writes: list[str] = []
    write_proposal = configurer.write_proposal

    def tracked_write(proposal):
        writes.append(proposal.client)
        write_proposal(proposal)

    class FailingOutput:
        def write(self, value: str) -> int:
            if failure == "write":
                raise BrokenPipeError("injected stdout write failure")
            return len(value)

        def flush(self) -> None:
            if failure == "flush":
                raise BrokenPipeError("injected stdout flush failure")

    output = io.StringIO() if failure == "closed" else FailingOutput()
    if failure == "closed":
        output.close()
    error_output = io.StringIO()
    monkeypatch.setattr(configurer, "write_proposal", tracked_write)
    monkeypatch.setattr(configurer.sys, "stdout", output)
    monkeypatch.setattr(configurer.sys, "stderr", error_output)

    result = configurer.main(all_client_arguments(tmp_path, paths))

    diagnostic = error_output.getvalue()
    assert result == 4
    assert writes == ["claude"]
    assert "agent-fabric" in paths["claude"].read_text()
    assert paths["codex"].read_text() == '[unrelated]\nvalue = "before"\n'
    assert "partial-state: agent-fabric MCP registration" in diagnostic
    assert "cause=receipt-output" in diagnostic
    assert "committed=claude" in diagnostic
    assert "remaining=codex,cursor,agy,kiro,opencode" in diagnostic
    assert f"config={paths['claude']}" in diagnostic
    assert "recovery=restore stdout, inspect the committed configuration, then rerun --platform all" in diagnostic


def test_closed_stdout_reader_exits_with_typed_partial_state_without_shutdown_override(
    tmp_path: Path,
) -> None:
    paths = all_client_paths(tmp_path)
    paths["claude"].write_text('{}\n')
    paths["codex"].write_text('[unrelated]\nvalue = "before"\n')
    for client in ("cursor", "agy", "kiro", "opencode"):
        paths[client].write_text('{}\n')
    return_code, error_output = run_configure_with_closed_stdout(tmp_path)

    assert return_code == 4
    assert "agent-fabric" in paths["claude"].read_text()
    assert paths["codex"].read_text() == '[unrelated]\nvalue = "before"\n'
    assert "partial-state: agent-fabric MCP registration" in error_output
    assert "cause=receipt-output" in error_output
    assert "committed=claude" in error_output
    assert "remaining=codex,cursor,agy,kiro,opencode" in error_output
    assert f"config={paths['claude']}" in error_output
    assert "recovery=restore stdout, inspect the committed configuration, then rerun --platform all" in error_output
    assert "Exception ignored" not in error_output


@pytest.mark.parametrize("mode", [(), ("--check",), ("--preflight",)])
def test_closed_stdout_before_any_commit_preserves_conflict_exit(
    tmp_path: Path, mode: tuple[str, ...],
) -> None:
    configured = run_configure(tmp_path, "--platform", "claude")
    assert configured.returncode == 0, configured.stderr

    return_code, error_output = run_configure_with_closed_stdout(
        tmp_path, "--platform", "claude", *mode,
    )

    assert return_code == 3
    assert "conflicting: agent-fabric MCP receipt output failed" in error_output
    assert "Exception ignored" not in error_output


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_existing_direct_config_under_symlinked_parent_binds_installed_inode(tmp_path: Path, client: str) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    real_parent = tmp_path / "real-parent"
    real_parent.mkdir()
    linked_parent = tmp_path / "linked-parent"
    linked_parent.symlink_to(real_parent, target_is_directory=True)
    requested = linked_parent / f"{client}.{suffix}"
    requested.write_text('{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n')
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)

    configurer.write_proposal(proposal)

    assert "agent-fabric" in requested.read_text()


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_absent_config_under_symlinked_parent_binds_installed_inode(tmp_path: Path, client: str) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    real_parent = tmp_path / "real-parent"
    real_parent.mkdir()
    linked_parent = tmp_path / "linked-parent"
    linked_parent.symlink_to(real_parent, target_is_directory=True)
    requested = linked_parent / f"{client}.{suffix}"
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)

    configurer.write_proposal(proposal)

    assert "agent-fabric" in requested.read_text()


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_existing_write_preserves_post_validation_interleave_as_recovery(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    original = '{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n'
    external = '{"external":"after-validation"}\n' if client == "claude" else '[external]\nvalue = "after-validation"\n'
    requested.write_text(original)
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    atomic_exchange = configurer.atomic_exchange
    interleaved = False

    def interleaved_exchange(first: Path, second: Path) -> None:
        nonlocal interleaved
        if not interleaved:
            interleaved = True
            replacement = tmp_path / f"external.{suffix}"
            replacement.write_text(external)
            replacement.chmod(0o644)
            os.replace(replacement, requested)
        atomic_exchange(first, second)

    monkeypatch.setattr(configurer, "atomic_exchange", interleaved_exchange)
    with pytest.raises(configurer.RegistrationConflictError, match=f"{client.title()} config changed") as caught:
        configurer.write_proposal(proposal)

    assert "agent-fabric" in requested.read_text()
    recovery = Path(str(caught.value).rsplit(" ", 1)[-1])
    assert recovery.read_text() == external
    assert recovery.parent != tmp_path
    assert stat.S_IMODE(recovery.parent.stat().st_mode) == 0o700
    recovery.unlink()
    recovery.parent.rmdir()


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_private_recovery_preserves_displaced_hardlink_without_chmod(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    requested.write_text('{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n')
    external = '{"external":"hardlinked"}\n' if client == "claude" else '[external]\nvalue = "hardlinked"\n'
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    atomic_exchange = configurer.atomic_exchange
    hardlink = tmp_path / f"external-hardlink.{suffix}"

    def interleaved_exchange(first: Path, second: Path) -> None:
        replacement = tmp_path / f"external.{suffix}"
        replacement.write_text(external)
        replacement.chmod(0o644)
        os.link(replacement, hardlink)
        os.replace(replacement, requested)
        atomic_exchange(first, second)

    monkeypatch.setattr(configurer, "atomic_exchange", interleaved_exchange)
    with pytest.raises(configurer.RegistrationConflictError) as caught:
        configurer.write_proposal(proposal)

    recovery = Path(str(caught.value).rsplit(" ", 1)[-1])
    assert recovery.parent != tmp_path
    assert stat.S_IMODE(recovery.parent.stat().st_mode) == 0o700
    assert recovery.stat().st_ino == hardlink.stat().st_ino
    assert stat.S_IMODE(hardlink.stat().st_mode) == 0o644
    assert hardlink.read_text() == external
    recovery.unlink()
    recovery.parent.rmdir()


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_post_exchange_fsync_error_retains_private_displaced_recovery(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    original = '{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n'
    requested.write_text(original)
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    fsync_directory = configurer._fsync_directory
    calls = 0

    def fail_first_fsync(path: Path) -> None:
        nonlocal calls
        calls += 1
        if calls == 1:
            raise OSError("post-exchange durability failure")
        fsync_directory(path)

    monkeypatch.setattr(configurer, "_fsync_directory", fail_first_fsync)
    with pytest.raises(configurer.RegistrationConflictError, match="preserve private recovery file") as caught:
        configurer.write_proposal(proposal)

    assert "agent-fabric" in requested.read_text()
    recovery = Path(str(caught.value).rsplit(" ", 1)[-1])
    assert recovery.read_text() == original
    assert stat.S_IMODE(recovery.parent.stat().st_mode) == 0o700
    recovery.unlink()
    recovery.parent.rmdir()


@pytest.mark.parametrize("client", ["claude", "codex"])
@pytest.mark.parametrize("failure_call", [3, 4])
def test_post_commit_cleanup_fsync_error_does_not_report_conflict(
    tmp_path: Path, client: str, failure_call: int, monkeypatch, capsys,
) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    requested.write_text('{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n')
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    fsync_directory = configurer._fsync_directory
    calls = 0
    failed_path: Path | None = None

    def fail_cleanup_fsync(path: Path) -> None:
        nonlocal calls, failed_path
        calls += 1
        if calls == failure_call:
            failed_path = path
            raise OSError("cleanup durability failure")
        fsync_directory(path)

    monkeypatch.setattr(configurer, "_fsync_directory", fail_cleanup_fsync)

    configurer.write_proposal(proposal)

    assert "agent-fabric" in requested.read_text()
    assert not list(tmp_path.glob(f".{requested.name}.recovery.*"))
    operation = "recovery-directory-fsync" if failure_call == 3 else "target-parent-fsync"
    assert capsys.readouterr().err.strip() == (
        f"warning: post-commit recovery cleanup failed operation={operation} path={failed_path}"
    )


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_post_commit_recovery_rmdir_error_warns_without_reporting_conflict(
    tmp_path: Path, client: str, monkeypatch, capsys,
) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    requested.write_text('{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n')
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    rmdir = configurer.Path.rmdir
    failed_path: Path | None = None

    def fail_recovery_rmdir(path: Path) -> None:
        nonlocal failed_path
        if ".recovery." in path.name:
            failed_path = path
            raise OSError("cleanup rmdir failure")
        rmdir(path)

    monkeypatch.setattr(configurer.Path, "rmdir", fail_recovery_rmdir)

    configurer.write_proposal(proposal)

    assert "agent-fabric" in requested.read_text()
    assert failed_path is not None and failed_path.is_dir()
    assert capsys.readouterr().err.strip() == (
        f"warning: post-commit recovery cleanup failed operation=recovery-directory-rmdir path={failed_path}"
    )


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_pre_exchange_registration_error_cleans_private_staging(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    original = '{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n'
    requested.write_text(original)
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)

    def unsupported_exchange(first: Path, second: Path) -> None:
        raise configurer.RegistrationError("atomic config exchange is unavailable")

    monkeypatch.setattr(configurer, "atomic_exchange", unsupported_exchange)
    with pytest.raises(configurer.RegistrationError, match="unavailable"):
        configurer.write_proposal(proposal)

    assert requested.read_text() == original
    assert not list(tmp_path.glob(f".{requested.name}.recovery.*"))


@pytest.mark.parametrize("client", ["claude", "codex"])
@pytest.mark.parametrize("existing", [False, True])
def test_post_commit_recovery_unlink_error_does_not_report_conflict(
    tmp_path: Path, client: str, existing: bool, monkeypatch, capsys,
) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    if existing:
        requested.write_text('{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n')
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    unlink = configurer.Path.unlink

    def fail_recovery_unlink(path: Path, *args, **kwargs) -> None:
        if ".recovery." in path.parent.name:
            raise OSError("recovery cleanup failure")
        unlink(path, *args, **kwargs)

    monkeypatch.setattr(configurer.Path, "unlink", fail_recovery_unlink)

    configurer.write_proposal(proposal)

    assert "agent-fabric" in requested.read_text()
    recovery_directories = list(tmp_path.glob(f".{requested.name}.recovery.*"))
    assert len(recovery_directories) == 1
    assert stat.S_IMODE(recovery_directories[0].stat().st_mode) == 0o700
    recovery_files = list(recovery_directories[0].iterdir())
    assert len(recovery_files) == 1
    assert str(recovery_files[0]) in capsys.readouterr().err


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_private_recovery_preserves_displaced_symlink_without_following(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    requested.write_text('{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n')
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    atomic_exchange = configurer.atomic_exchange
    referent = tmp_path / f"external-target.{suffix}"
    referent.write_text('{"external":true}\n' if client == "claude" else '[external]\nvalue = true\n')

    def interleaved_exchange(first: Path, second: Path) -> None:
        replacement = tmp_path / f"external-link.{suffix}"
        replacement.symlink_to(referent)
        os.replace(replacement, requested)
        atomic_exchange(first, second)

    monkeypatch.setattr(configurer, "atomic_exchange", interleaved_exchange)
    with pytest.raises(configurer.RegistrationConflictError) as caught:
        configurer.write_proposal(proposal)

    recovery = Path(str(caught.value).rsplit(" ", 1)[-1])
    assert recovery.is_symlink()
    assert os.readlink(recovery) == str(referent)
    assert stat.S_IMODE(recovery.parent.stat().st_mode) == 0o700
    assert referent.exists()
    recovery.unlink()
    recovery.parent.rmdir()


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_conflict_never_rolls_back_over_a_newer_target(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    original = '{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n'
    first_race = '{"external":"first-race"}\n' if client == "claude" else '[external]\nvalue = "first-race"\n'
    newest = '{"external":"newest"}\n' if client == "claude" else '[external]\nvalue = "newest"\n'
    requested.write_text(original)
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    atomic_exchange = configurer.atomic_exchange
    displaced_matches = configurer._displaced_matches
    exchanged = False

    def first_interleave(first: Path, second: Path) -> None:
        nonlocal exchanged
        if not exchanged:
            exchanged = True
            replacement = tmp_path / f"first-race.{suffix}"
            replacement.write_text(first_race)
            os.replace(replacement, requested)
        atomic_exchange(first, second)

    def second_interleave(candidate, displaced: Path) -> bool:
        matched = displaced_matches(candidate, displaced)
        replacement = tmp_path / f"newest.{suffix}"
        replacement.write_text(newest)
        os.replace(replacement, requested)
        return matched

    monkeypatch.setattr(configurer, "atomic_exchange", first_interleave)
    monkeypatch.setattr(configurer, "_displaced_matches", second_interleave)
    with pytest.raises(configurer.RegistrationConflictError) as caught:
        configurer.write_proposal(proposal)

    assert requested.read_text() == newest
    recovery = Path(str(caught.value).rsplit(" ", 1)[-1])
    assert recovery.read_text() == first_race
    recovery.unlink()


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_symlink_retarget_after_exchange_fails_closed_with_recovery(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    original = '{}\n' if client == "claude" else '[unrelated]\nvalue = "before"\n'
    external = '{"external":"retargeted"}\n' if client == "claude" else '[external]\nvalue = "retargeted"\n'
    first = tmp_path / f"first.{suffix}"
    second = tmp_path / f"second.{suffix}"
    first.write_text(original)
    second.write_text(external)
    requested.symlink_to(first)
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    atomic_exchange = configurer.atomic_exchange

    def retarget_after_exchange(first_path: Path, second_path: Path) -> None:
        atomic_exchange(first_path, second_path)
        requested.unlink()
        requested.symlink_to(second)

    monkeypatch.setattr(configurer, "atomic_exchange", retarget_after_exchange)
    with pytest.raises(configurer.RegistrationConflictError) as caught:
        configurer.write_proposal(proposal)

    assert requested.resolve() == second
    assert second.read_text() == external
    recovery = Path(str(caught.value).rsplit(" ", 1)[-1])
    assert recovery.read_text() == original
    recovery.unlink()


@pytest.mark.parametrize("client", ["claude", "codex"])
def test_absent_target_replaced_after_link_fails_closed_with_recovery(tmp_path: Path, client: str, monkeypatch) -> None:
    configurer = load_configurer()
    desired = configurer.registration(ROOT, tmp_path / "state", client)
    suffix = "json" if client == "claude" else "toml"
    requested = tmp_path / f"{client}.{suffix}"
    external = '{"external":"newest"}\n' if client == "claude" else '[external]\nvalue = "newest"\n'
    proposal = configurer.claude_update(requested, desired) if client == "claude" else configurer.codex_update(requested, desired)
    link = configurer.os.link

    def replace_after_link(source: Path, target: Path, **options) -> None:
        link(source, target, **options)
        replacement = tmp_path / f"external.{suffix}"
        replacement.write_text(external)
        os.replace(replacement, requested)

    monkeypatch.setattr(configurer.os, "link", replace_after_link)
    with pytest.raises(configurer.RegistrationConflictError) as caught:
        configurer.write_proposal(proposal)

    assert requested.read_text() == external
    recovery = Path(str(caught.value).rsplit(" ", 1)[-1])
    assert "agent-fabric" in recovery.read_text()
    recovery.unlink()


def test_operations_docs_define_dynamic_primary_registration_and_bounded_fixed_paths() -> None:
    runbook = (ROOT / "docs/runbooks/agent-fabric-operations.md").read_text()
    runtime_readme = (ROOT / "runtime/agent-fabric/README.md").read_text()
    for document in (runbook, runtime_readme):
        assert "configure-agent-fabric-mcp.py" in document
        assert "AGENT_FABRIC_PROJECT_PATH" in document
        assert "Claude Code and Codex" in document
        assert "cannot preserve" in document
        assert "exactly three environment variables" in document
        assert "AGENT_FABRIC_STATE_DIRECTORY" in document
        assert "AGENT_FABRIC_SEAT" in document
        assert "AGENT_FABRIC_CLIENT_LABEL" in document
        assert "six clients" in document
    assert "opencode mcp list" in runbook
    assert "exit code `4`" in runbook
    assert "partial-state" in runbook
    assert "first-client atomic install" in runbook
    assert "affected current client and later" in runbook
    assert "shutdown-time status override" in runbook
    assert "Registry entries bind `AGENT_FABRIC_PROJECT_PATH`" not in runbook
    verification = runbook.split("## Verify registrations", 1)[1].split("\n## ", 1)[0]
    assert "opencode mcp list" in verification
    assert "exactly three global" in verification
    assert "four non-secret" not in verification
    assert "AGENT_FABRIC_PROJECT_PATH" in verification
    assert "explicit project-scoped compatibility" in verification
