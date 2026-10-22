"""Environment hygiene, local-checkout preference and the MCP entry (C§3, C§5, C§7, C§12; #26 to #33)."""

from __future__ import annotations

import shutil

import pytest

from parity_support import (
    FABRIC_MCP,
    GIT_REDIRECT_VARIABLES,
    ROOT,
    install_front_door,
    sha256,
)

SENTINEL = "<unset>"


def env_of(result, name):
    return result.payload()["env"].get(name, SENTINEL)


def test_dispatcher_in_a_complete_tree_runs_it_and_leaves_env_as_inherited(dispatcher):
    """C§16 #26 [D]: local owner wins; root variables are neither injected nor changed."""
    unset = dispatcher.run("route", product=False)
    assert unset.payload()["argv0"] == str(dispatcher.owner("scripts/model-route"))
    assert env_of(unset, "AGENT_FABRIC_PRODUCT_ROOT") == SENTINEL
    assert env_of(unset, "AGENTS_HOME") == SENTINEL

    elsewhere = dispatcher.tmp / "elsewhere"
    elsewhere.mkdir()
    set_ = dispatcher.run(
        "route", product=False, AGENT_FABRIC_PRODUCT_ROOT=str(elsewhere), AGENTS_HOME="/x/y"
    )
    assert set_.payload()["argv0"] == str(dispatcher.owner("scripts/model-route"))
    assert env_of(set_, "AGENT_FABRIC_PRODUCT_ROOT") == str(elsewhere)
    assert env_of(set_, "AGENTS_HOME") == "/x/y"


def test_dispatcher_without_a_launcher_resolves_and_injects_the_root(dispatcher):
    """C§16 #26 [D]: a tree lacking fabric-mcp falls through to root selection."""
    bare = dispatcher.tmp / "bare"
    (bare / "scripts").mkdir(parents=True)
    shutil.copytree(ROOT / "scripts/lib", bare / "scripts/lib")
    shutil.copy2(ROOT / "scripts/worktree.py", bare / "scripts/worktree.py")
    install_front_door(bare / "scripts/provenant", "dispatcher")
    dispatcher.command = bare / "scripts/provenant"

    result = dispatcher.run("route")  # AGENT_FABRIC_PRODUCT_ROOT names the full fixture

    assert result.payload()["argv0"] == str(dispatcher.owner("scripts/model-route"))
    assert env_of(result, "AGENT_FABRIC_PRODUCT_ROOT") == str(dispatcher.checkout)
    assert env_of(result, "AGENTS_HOME") == str(dispatcher.checkout)


def test_symlinked_dispatcher_resolves_to_its_real_tree(dispatcher):
    """C§16 #27 [D]: the checkout is found through symlinks."""
    link = dispatcher.tmp / "links" / "provenant"
    link.parent.mkdir()
    link.symlink_to(dispatcher.command)
    dispatcher.command = link

    result = dispatcher.run("route", product=False)

    assert result.payload()["argv0"] == str(dispatcher.owner("scripts/model-route"))
    assert env_of(result, "AGENT_FABRIC_PRODUCT_ROOT") == SENTINEL


def test_non_executable_local_owner_is_an_exec_failure_not_a_resolution_error(dispatcher):
    """C§16 #27 [D]: presence is by file existence, so this is exit 3 `Permission denied`."""
    owner = dispatcher.owner("scripts/model-route")
    owner.chmod(0o644)

    result = dispatcher.run("route", product=False)

    assert result.returncode == 3
    assert result.err.startswith(f"provenant: cannot execute {owner}: Permission denied; repair:")


def test_stub_ignores_same_named_commands_in_its_own_directory(stub):
    """C§16 #28 [S]: a `fabric` beside the stub is not the owner."""
    shadow = stub.command.parent / "fabric"
    shadow.write_text("#!/bin/sh\necho wrong-bin-owner\n")
    shadow.chmod(0o755)

    result = stub.run("fabric")

    assert result.payload()["argv0"] == str(stub.owner("runtime/fabric/bin/fabric"))


def test_stub_always_sets_product_and_agents_home_and_never_the_instance_root(stub):
    """C§16 #29 [S]: both root variables are the resolved root, conflicts overridden."""
    result = stub.run("route", AGENTS_HOME="/stale/product", AGENT_FABRIC_INSTANCE_ROOT=None)

    assert env_of(result, "AGENT_FABRIC_PRODUCT_ROOT") == str(stub.checkout)
    assert env_of(result, "AGENTS_HOME") == str(stub.checkout)
    assert env_of(result, "AGENT_FABRIC_INSTANCE_ROOT") == SENTINEL
    assert not (stub.home / ".agents").exists()


def test_inherited_instance_root_is_forwarded_unchanged(fd):
    """C§16 #29 and #32: the front door forwards a chosen instance root, never invents one."""
    instance = fd.tmp / "instance"
    instance.mkdir()

    chosen = fd.run("route", AGENT_FABRIC_INSTANCE_ROOT=str(instance))
    not_chosen = fd.run("route")

    assert env_of(chosen, "AGENT_FABRIC_INSTANCE_ROOT") == str(instance)
    assert env_of(not_chosen, "AGENT_FABRIC_INSTANCE_ROOT") == SENTINEL


def test_check_removes_exactly_the_eight_git_redirects(fd):
    """C§16 #31: `check` scrubs the named eight and keeps every other GIT_ variable."""
    redirects = {name: f"/nonexistent/{name.lower()}" for name in GIT_REDIRECT_VARIABLES}
    kept = {"GIT_AUTHOR_NAME": "someone", "GIT_CONFIG_COUNT": "0", "GIT_EXEC_PATH_X": "1"}

    check = fd.run("check", **redirects, **kept)

    env = check.payload()["env"]
    assert not set(GIT_REDIRECT_VARIABLES) & set(env)
    for name, value in kept.items():
        assert env[name] == value


@pytest.mark.parametrize(
    "verb", ["route", "worktree", "fabric", "lanes", "gate", "quota", "dispatch", "hosts"]
)
def test_other_verbs_remove_no_git_variable(fd, verb):
    """C§16 #31: only `check` scrubs."""
    redirects = {name: f"/nonexistent/{name.lower()}" for name in GIT_REDIRECT_VARIABLES}

    env = fd.run(verb, **redirects).payload()["env"]

    for name, value in redirects.items():
        assert env[name] == value


def test_check_selected_root_variables_name_the_checker_checkout(fd):
    """C§16 #35 and #7 (env row): `check` sets both root variables to the selected checkout."""
    result = fd.run("check", "--fast", AGENTS_HOME="/stale/product")

    payload = result.payload()
    assert payload["argv0"] == str(fd.owner("scripts/check-harness"))
    assert payload["argv"] == ["--fast"]
    assert payload["env"]["AGENT_FABRIC_PRODUCT_ROOT"] == str(fd.checkout)
    assert payload["env"]["AGENTS_HOME"] == str(fd.checkout)


# ------------------------------------------------------------- MCP entry


@pytest.mark.parametrize("seat", ["codex-1", ""])
def test_seat_with_no_arguments_launches_fabric_mcp(fd, seat):
    """C§16 #32: presence of the seat (even empty) selects the MCP launcher, no arguments."""
    data = b"\x00MCP frame\n" * 1000

    result = fd.run(stdin=data, AGENT_FABRIC_SEAT=seat)

    payload = result.payload()
    assert result.returncode == 0, result.err
    assert payload["argv0"] == str(fd.owner(FABRIC_MCP))
    assert payload["argv"] == []
    assert payload["cwd"] == str(fd.cwd)
    assert payload["env"]["AGENT_FABRIC_SEAT"] == seat
    assert payload["env"]["AGENT_FABRIC_PRODUCT_ROOT"] == str(fd.checkout)
    assert payload["env"]["AGENTS_HOME"] == str(fd.checkout)
    assert payload["stdin_sha256"] == sha256(data)
    assert "AGENT_FABRIC_INSTANCE_ROOT" not in payload["env"]


def test_mcp_entry_forwards_a_chosen_instance_root(fd):
    """C§12: the instance root reaches the MCP child only if the caller had one."""
    instance = fd.tmp / "instance"
    instance.mkdir()

    result = fd.run(AGENT_FABRIC_SEAT="s", AGENT_FABRIC_INSTANCE_ROOT=str(instance))

    assert result.payload()["env"]["AGENT_FABRIC_INSTANCE_ROOT"] == str(instance)


def test_seat_with_arguments_does_not_launch_mcp(fd):
    """C§16 #32: any argument disables the MCP shortcut."""
    routed = fd.run("route", AGENT_FABRIC_SEAT="s")
    assert routed.payload()["argv0"] == str(fd.owner("scripts/model-route"))

    bogus = fd.run("bogus", AGENT_FABRIC_SEAT="s")
    assert bogus.returncode == 2 and bogus.stdout == b""


def test_no_seat_and_no_arguments_is_usage_after_resolution(fd):
    """C§16 #33: usage, exit 2 (the Python stub resolved first; here resolution succeeds)."""
    result = fd.run()

    assert result.returncode == 2 and result.stdout == b""
    assert result.err.startswith("usage: provenant ")


def test_shebangless_owner_is_an_exec_failure_not_a_shell_script(dispatcher):
    """execve semantics: ENOEXEC is `Exec format error`, never a `/bin/sh` fallback."""
    owner = dispatcher.owner("scripts/model-route")
    owner.write_text("echo ran-through-a-shell\n")
    owner.chmod(0o755)

    result = dispatcher.run("route", product=False)

    assert result.returncode == 3
    assert result.stdout == b""
    assert result.err.startswith(f"provenant: cannot execute {owner}: Exec format error; repair:")
