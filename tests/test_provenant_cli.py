from pathlib import Path
import json
import os
import shutil
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "scripts" / "provenant"
STUB = ROOT / "scripts" / "provenant.template"


def make_checkout(tmp_path: Path) -> tuple[Path, Path]:
    checkout = tmp_path / "checkout"
    scripts = checkout / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SOURCE, scripts / "provenant")
    shutil.copytree(ROOT / "scripts/lib", scripts / "lib")

    recorder = """#!/usr/bin/env python3
import json
import os
import sys

payload = {
    "argv": sys.argv,
    "cwd": os.getcwd(),
    "marker": os.environ.get("PROVENANT_TEST_MARKER"),
    "stdin": sys.stdin.read(),
}
print(json.dumps(payload, sort_keys=True))
print("dummy stderr", file=sys.stderr)
raise SystemExit(int(os.environ.get("PROVENANT_TEST_EXIT", "0")))
"""
    for owner in ("model-route", "worktree", "check-harness", "agent-fabric", "agent-fabric-mcp"):
        path = scripts / owner
        path.write_text(recorder)
        path.chmod(0o755)

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    command = bin_dir / "provenant"
    command.symlink_to(scripts / "provenant")
    return checkout, command


def invoke(command: Path, *args: str, cwd: Path, stdin: str = "", **env_updates: str):
    env = os.environ.copy()
    env.update(env_updates)
    return subprocess.run(
        [str(command), *args],
        cwd=cwd,
        env=env,
        input=stdin,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def install_stub(tmp_path: Path) -> Path:
    command = tmp_path / "stable-bin/provenant"
    command.parent.mkdir(exist_ok=True)
    shutil.copy2(STUB, command)
    command.chmod(0o755)
    return command


def test_installed_stub_contains_only_resolution_and_delegation() -> None:
    text = STUB.read_text()

    assert "owners =" not in text
    assert "def usage" not in text
    assert "def help_text" not in text
    assert "model-route" not in text
    assert "worktree" not in text
    assert "check-harness" not in text


def test_route_preserves_argv_environment_stdio_and_exit_status_through_symlink(tmp_path):
    _, command = make_checkout(tmp_path)
    caller_cwd = tmp_path / "unrelated" / "nested"
    caller_cwd.mkdir(parents=True)

    result = invoke(
        command,
        "route",
        "two words",
        "",
        "--literal=*",
        cwd=caller_cwd,
        stdin="input bytes\n",
        PROVENANT_TEST_MARKER="kept",
        PROVENANT_TEST_EXIT="17",
    )

    assert result.returncode == 17
    assert result.stderr == "dummy stderr\n"
    payload = json.loads(result.stdout)
    assert payload == {
        "argv": [str(command.parent.parent / "checkout" / "scripts" / "model-route"), "two words", "", "--literal=*"],
        "cwd": str(caller_cwd),
        "marker": "kept",
        "stdin": "input bytes\n",
    }


def test_worktree_runs_from_arbitrary_cwd_without_changing_it(tmp_path):
    _, command = make_checkout(tmp_path)
    caller_cwd = tmp_path / "another-repository"
    caller_cwd.mkdir()

    result = invoke(command, "worktree", "check", cwd=caller_cwd)

    assert result.returncode == 0
    payload = json.loads(result.stdout)
    assert payload["argv"][1:] == ["check"]
    assert payload["cwd"] == str(caller_cwd)


def test_installed_copy_ignores_same_named_commands_in_its_bin_directory(tmp_path):
    checkout, _ = make_checkout(tmp_path)
    product_owner = checkout / "scripts/agent-fabric"
    product_owner.write_text("#!/bin/sh\nprintf '%s\\n' \"$AGENTS_HOME\"\n")
    product_owner.chmod(0o755)
    bin_dir = tmp_path / "stable-bin"
    bin_dir.mkdir()
    command = bin_dir / "provenant"
    shutil.copy2(STUB, command)
    shadow = bin_dir / "agent-fabric"
    shadow.write_text("#!/bin/sh\nprintf 'wrong-bin-owner\\n'\n")
    shadow.chmod(0o755)

    result = invoke(
        command,
        "doctor",
        cwd=tmp_path,
        AGENT_FABRIC_PRODUCT_ROOT=str(checkout),
        AGENTS_HOME=str(tmp_path / "stale-product"),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == f"{checkout}\n"


def test_split_root_preserves_instance_root_for_mcp_child(tmp_path):
    checkout, _ = make_checkout(tmp_path)
    command = install_stub(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    wrapper = checkout / "scripts/agent-fabric-mcp"
    wrapper.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$AGENT_FABRIC_INSTANCE_ROOT|$AGENT_FABRIC_PRODUCT_ROOT|$AGENTS_HOME\"\n"
    )
    wrapper.chmod(0o755)

    result = invoke(
        command,
        cwd=tmp_path,
        AGENT_FABRIC_SEAT="codex",
        AGENT_FABRIC_PRODUCT_ROOT=str(checkout),
        AGENTS_HOME=str(instance_root),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == f"{instance_root}|{checkout}|{checkout}\n"


def test_explicit_product_root_overrides_instance_pointer_and_agents_home(tmp_path):
    explicit_product, _ = make_checkout(tmp_path / "explicit")
    pointed_product, _ = make_checkout(tmp_path / "pointed")
    command = install_stub(tmp_path)
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    pointer.write_text(json.dumps({
        "schema_version": 1,
        "product_root": str(pointed_product),
    }))
    explicit_wrapper = explicit_product / "scripts/agent-fabric-mcp"
    explicit_wrapper.write_text("#!/bin/sh\nprintf 'explicit\\n'\n")
    explicit_wrapper.chmod(0o755)

    result = invoke(
        command,
        cwd=tmp_path,
        AGENT_FABRIC_SEAT="codex",
        AGENT_FABRIC_PRODUCT_ROOT=str(explicit_product),
        AGENT_FABRIC_INSTANCE_ROOT=str(instance_root),
        AGENTS_HOME=str(pointed_product),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "explicit\n"


def test_installed_stub_delegates_non_mcp_commands_to_checkout_cli(tmp_path):
    checkout, _ = make_checkout(tmp_path)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "help",
        cwd=tmp_path,
        AGENT_FABRIC_PRODUCT_ROOT=str(checkout),
    )

    assert result.returncode == 0, result.stderr
    assert "Thin front door" in result.stdout


def test_owner_exec_failure_reports_command_context_without_traceback(tmp_path):
    checkout, command = make_checkout(tmp_path)
    owner = checkout / "scripts/model-route"
    owner.chmod(0o644)

    result = invoke(command, "route", cwd=tmp_path)

    assert result.returncode == 3
    assert str(owner) in result.stderr
    assert "repair: re-run install-harness" in result.stderr
    assert "Traceback" not in result.stderr


@pytest.mark.parametrize(
    ("subcommand", "arguments"),
    [
        ("route", ["resolve"]),
        ("worktree", ["check"]),
        ("check", ["--doctor"]),
        ("fabric", ["workspace", "inspect"]),
        ("doctor", []),
    ],
)
@pytest.mark.parametrize("cwd_kind", ["provenant-root", "unrelated-git", "nonrepo"])
def test_every_delegated_command_preserves_each_supported_caller_cwd(
    tmp_path, subcommand, arguments, cwd_kind
):
    checkout, command = make_checkout(tmp_path)
    if cwd_kind == "provenant-root":
        caller_cwd = checkout
    elif cwd_kind == "unrelated-git":
        caller_cwd = tmp_path / "unrelated-git"
        caller_cwd.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=caller_cwd, check=True)
        assert (caller_cwd / ".git").is_dir()
    else:
        caller_cwd = tmp_path / "nonrepo"
        caller_cwd.mkdir()
        assert not (caller_cwd / ".git").exists()

    result = invoke(command, subcommand, *arguments, cwd=caller_cwd)

    assert result.returncode == 0
    assert json.loads(result.stdout)["cwd"] == str(caller_cwd)


def test_doctor_is_owned_by_agent_fabric_and_forwards_explicit_arguments(tmp_path):
    _, command = make_checkout(tmp_path)

    result = invoke(command, "doctor", "--help", "--consume-provider-quota", cwd=tmp_path)

    assert result.returncode == 0
    payload = json.loads(result.stdout)
    assert payload["argv"][0].endswith("/scripts/agent-fabric")
    assert payload["argv"][1:] == ["doctor", "--help", "--consume-provider-quota"]


def test_check_and_fabric_delegate_without_reinterpreting_arguments(tmp_path):
    _, command = make_checkout(tmp_path)

    check = invoke(command, "check", "--doctor", cwd=tmp_path)
    fabric = invoke(command, "fabric", "workspace", "inspect", "--path", "x y", cwd=tmp_path)

    assert json.loads(check.stdout)["argv"][1:] == ["--doctor"]
    assert json.loads(fabric.stdout)["argv"][1:] == ["workspace", "inspect", "--path", "x y"]


def test_missing_or_unknown_command_prints_usage_to_stderr_and_exits_2(tmp_path):
    _, command = make_checkout(tmp_path)

    for args in ((), ("unknown",)):
        result = invoke(command, *args, cwd=tmp_path)
        assert result.returncode == 2
        assert result.stdout == ""
        assert result.stderr.startswith("usage: provenant ")


def test_help_is_concise_and_names_existing_command_owners(tmp_path):
    _, command = make_checkout(tmp_path)

    result = invoke(command, "help", cwd=tmp_path)

    assert result.returncode == 0
    assert result.stderr == ""
    assert "route" in result.stdout and "scripts/model-route" in result.stdout
    assert "doctor ...     scripts/agent-fabric doctor ..." in result.stdout
    assert "Kiro: optional subscription-native provider (ACP v1)." in result.stdout
    assert "OpenCode: optional provider for explicit opencode/* account models (ACP v1)." in result.stdout
