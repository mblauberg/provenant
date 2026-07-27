from pathlib import Path
import json
import os
import shutil
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "scripts" / "provenant"


def make_checkout(tmp_path: Path) -> tuple[Path, Path]:
    checkout = tmp_path / "checkout"
    scripts = checkout / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SOURCE, scripts / "provenant")

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
    for owner in ("model-route", "worktree", "check-harness", "agent-fabric"):
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
