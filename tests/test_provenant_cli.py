from pathlib import Path
import json
import os
import shutil
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "scripts" / "provenant"
STUB = ROOT / "scripts" / "provenant.template"
GIT_REDIRECT_VARIABLES = (
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
)


def clean_git_environment() -> dict[str, str]:
    environment = os.environ.copy()
    for name in GIT_REDIRECT_VARIABLES:
        environment.pop(name, None)
    return environment


def git_fixture(cwd: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *arguments], cwd=cwd, env=clean_git_environment(), text=True, check=True
    )


def make_checkout(tmp_path: Path) -> tuple[Path, Path]:
    checkout = tmp_path / "checkout"
    scripts = checkout / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(SOURCE, scripts / "provenant")
    shutil.copy2(ROOT / "scripts/worktree.py", scripts / "worktree.py")
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
    for owner in ("model-route", "worktree", "check-harness"):
        path = scripts / owner
        path.write_text(recorder)
        path.chmod(0o755)
    dispatch_owner = checkout / "skills/orchestrate/scripts/dispatch_run.py"
    dispatch_owner.parent.mkdir(parents=True)
    dispatch_owner.write_text(recorder)
    dispatch_owner.chmod(0o755)
    run_owner = checkout / "skills/orchestrate/scripts/run_controls.py"
    run_owner.write_text(recorder)
    run_owner.chmod(0o755)
    batch_owner = checkout / "skills/orchestrate/scripts/batch_run.py"
    batch_owner.write_text(recorder)
    batch_owner.chmod(0o755)
    fabric_bin = checkout / "runtime" / "fabric" / "bin"
    fabric_bin.mkdir(parents=True)
    for owner in ("fabric", "fabric-mcp"):
        path = fabric_bin / owner
        path.write_text(recorder)
        path.chmod(0o755)

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    command = bin_dir / "provenant"
    command.symlink_to(scripts / "provenant")
    return checkout, command


def make_registered_linked_checkout(tmp_path: Path, branch: str) -> tuple[Path, Path]:
    primary, _ = make_checkout(tmp_path)
    git_fixture(primary, "init", "-q")
    git_fixture(primary, "config", "user.email", "test@example.com")
    git_fixture(primary, "config", "user.name", "Test")
    git_fixture(primary, "add", ".")
    git_fixture(primary, "commit", "-qm", "fixture")
    linked = tmp_path / "linked"
    git_fixture(primary, "worktree", "add", "-q", "-b", branch, str(linked))
    return primary, linked


#: Every test here asserts what the dispatcher itself does with the fabric root
#: variables, so inheriting them from the ambient environment makes the file
#: answer a different question than it asks. The split-root CI job exports
#: `AGENT_FABRIC_INSTANCE_ROOT` for the whole job, which is exactly the case
#: that caught this: three tests asserting an unset instance root read the
#: job's own instance instead. Scrub them, then let each test name what it
#: wants.
AMBIENT_ROOT_VARIABLES = (
    "AGENT_FABRIC_INSTANCE_ROOT",
    "AGENT_FABRIC_PRODUCT_ROOT",
    "AGENTS_HOME",
)


def invoke(command: Path, *args: str, cwd: Path, stdin: str = "", **env_updates: str):
    env = os.environ.copy()
    for name in AMBIENT_ROOT_VARIABLES:
        env.pop(name, None)
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
    product_owner = checkout / "runtime/fabric/bin/fabric"
    product_owner.write_text("#!/bin/sh\nprintf '%s\\n' \"$AGENTS_HOME\"\n")
    product_owner.chmod(0o755)
    bin_dir = tmp_path / "stable-bin"
    bin_dir.mkdir()
    command = bin_dir / "provenant"
    shutil.copy2(STUB, command)
    shadow = bin_dir / "fabric"
    shadow.write_text("#!/bin/sh\nprintf 'wrong-bin-owner\\n'\n")
    shadow.chmod(0o755)

    result = invoke(
        command,
        "fabric",
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
    wrapper = checkout / "runtime/fabric/bin/fabric-mcp"
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
        AGENT_FABRIC_INSTANCE_ROOT=str(instance_root),
        AGENTS_HOME=str(checkout),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == f"{instance_root}|{checkout}|{checkout}\n"


def test_installed_stub_does_not_materialize_the_default_instance_root(tmp_path):
    checkout, _ = make_checkout(tmp_path)
    command = install_stub(tmp_path)
    wrapper = checkout / "runtime/fabric/bin/fabric-mcp"
    wrapper.write_text(
        "#!/bin/sh\n"
        "printf '%s|%s|%s\\n' "
        '"${AGENT_FABRIC_INSTANCE_ROOT-<unset>}" '
        '"${AGENT_FABRIC_PRODUCT_ROOT-<unset>}" '
        '"${AGENTS_HOME-<unset>}"\n'
    )
    wrapper.chmod(0o755)

    result = invoke(
        command,
        cwd=tmp_path,
        AGENT_FABRIC_SEAT="codex",
        AGENT_FABRIC_PRODUCT_ROOT=str(checkout),
        AGENTS_HOME=str(checkout),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == f"<unset>|{checkout}|{checkout}\n"


def test_checkout_dispatcher_does_not_inherit_agents_home_as_instance_root(tmp_path):
    dispatcher = tmp_path / "dispatcher"
    dispatcher_scripts = dispatcher / "scripts"
    dispatcher_scripts.mkdir(parents=True)
    shutil.copy2(SOURCE, dispatcher_scripts / "provenant")
    shutil.copytree(ROOT / "scripts/lib", dispatcher_scripts / "lib")

    product, _ = make_checkout(tmp_path / "product")
    owner = product / "runtime/fabric/bin/fabric"
    owner.write_text(
        "#!/bin/sh\n"
        "printf '%s|%s|%s\\n' "
        '"${AGENT_FABRIC_INSTANCE_ROOT-<unset>}" '
        '"${AGENT_FABRIC_PRODUCT_ROOT-<unset>}" '
        '"${AGENTS_HOME-<unset>}"\n'
    )
    owner.chmod(0o755)

    result = invoke(
        dispatcher_scripts / "provenant",
        "fabric",
        cwd=tmp_path,
        AGENT_FABRIC_PRODUCT_ROOT=str(product),
        AGENTS_HOME=str(product),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == f"<unset>|{product}|{product}\n"


def test_checkout_dispatcher_resolves_the_instance_pointer_without_agents_home_fallback(tmp_path):
    dispatcher = tmp_path / "dispatcher"
    dispatcher_scripts = dispatcher / "scripts"
    dispatcher_scripts.mkdir(parents=True)
    shutil.copy2(SOURCE, dispatcher_scripts / "provenant")
    shutil.copytree(ROOT / "scripts/lib", dispatcher_scripts / "lib")

    ambient_product, _ = make_checkout(tmp_path / "ambient-product")
    pointed_product, _ = make_checkout(tmp_path / "pointed-product")
    home = tmp_path / "home"
    pointer = home / ".agents/.agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    pointer.write_text(json.dumps({
        "schema_version": 1,
        "product_root": str(pointed_product),
    }))
    for product, marker in ((ambient_product, "ambient"), (pointed_product, "pointed")):
        owner = product / "runtime/fabric/bin/fabric"
        owner.write_text(f"#!/bin/sh\nprintf '%s\\n' {marker}\n")
        owner.chmod(0o755)

    result = invoke(
        dispatcher_scripts / "provenant",
        "fabric",
        cwd=tmp_path,
        HOME=str(home),
        AGENTS_HOME=str(ambient_product),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "pointed\n"


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
    explicit_wrapper = explicit_product / "runtime/fabric/bin/fabric-mcp"
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


def test_installed_check_uses_the_registered_caller_worktree(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722")
    primary_check = primary / "scripts/check-harness"
    primary_check.write_text("#!/bin/sh\nprintf 'WRONG primary checkout\\n'\n")
    primary_check.chmod(0o755)
    linked_check = linked / "scripts/check-harness"
    linked_check.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, subprocess\n"
        "print(json.dumps({\n"
        "    'cwd': os.getcwd(),\n"
        "    'product_root': os.environ.get('AGENT_FABRIC_PRODUCT_ROOT'),\n"
        "    'git_head': subprocess.check_output(\n"
        "        ['git', 'rev-parse', '--verify', 'HEAD'], text=True\n"
        "    ).strip(),\n"
        "}, sort_keys=True))\n"
        "raise SystemExit(19)\n"
    )
    linked_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
        GIT_DIR=str(tmp_path / "redirected.git"),
        GIT_WORK_TREE=str(tmp_path / "redirected-worktree"),
        GIT_CEILING_DIRECTORIES=str(linked.parent),
    )

    assert result.returncode == 19
    assert "WRONG primary checkout" not in result.stdout
    payload = json.loads(result.stdout)
    assert payload == {
        "cwd": str(linked),
        "git_head": subprocess.check_output(
            ["git", "rev-parse", "--verify", "HEAD"],
            cwd=linked,
            env=clean_git_environment(),
            text=True,
        ).strip(),
        "product_root": str(linked),
    }


def test_checkout_check_overrides_an_ambient_primary_product_root(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-direct")
    primary_check = primary / "scripts/check-harness"
    primary_check.write_text("#!/bin/sh\nprintf 'WRONG primary checkout\\n'\n")
    primary_check.chmod(0o755)
    linked_check = linked / "scripts/check-harness"
    linked_check.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"${AGENT_FABRIC_PRODUCT_ROOT-<unset>}\"\n"
    )
    linked_check.chmod(0o755)

    result = invoke(
        linked / "scripts/provenant",
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == f"{linked}\n"
    assert "WRONG primary checkout" not in result.stdout


def test_installed_check_refuses_a_copied_linked_worktree(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-copy")
    copied = tmp_path / "copied"
    shutil.copytree(linked, copied)
    primary_check = primary / "scripts/check-harness"
    primary_check.write_text("#!/bin/sh\nprintf 'WRONG primary checkout\\n'\n")
    primary_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=copied,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "invalid Git metadata" in result.stderr
    assert str(copied / ".git") in result.stderr
    assert "WRONG primary checkout" not in result.stderr


def test_installed_check_refuses_an_orphaned_copied_linked_worktree(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-orphan")
    copied = tmp_path / "orphaned-copy"
    shutil.copytree(linked, copied)
    git_fixture(primary, "worktree", "remove", "--force", str(linked))
    primary_check = primary / "scripts/check-harness"
    primary_check.write_text("#!/bin/sh\nprintf 'WRONG primary checkout\\n'\n")
    primary_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=copied,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "invalid Git metadata" in result.stderr
    assert str(copied / ".git") in result.stderr
    assert "WRONG primary checkout" not in result.stderr


def test_installed_check_refuses_a_copied_worktree_nested_in_a_registered_checkout(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-nested-copy")
    staged = tmp_path / "staged-copy"
    shutil.copytree(linked, staged)
    copied = primary / "nested-copy"
    shutil.move(staged, copied)
    primary_check = primary / "scripts/check-harness"
    primary_check.write_text("#!/bin/sh\nprintf 'WRONG registered checkout\\n'\n")
    primary_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=copied,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "invalid Git metadata" in result.stderr
    assert str(copied / ".git") in result.stderr
    assert "WRONG registered checkout" not in result.stderr


def test_installed_check_ignores_a_shadow_git_on_the_caller_path(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-shadow-git")
    marker = tmp_path / "shadow-git-ran"
    shadow_dir = tmp_path / "shadow-bin"
    shadow_dir.mkdir()
    shadow = shadow_dir / "git"
    shadow.write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 1\n")
    shadow.chmod(0o755)
    linked_check = linked / "scripts/check-harness"
    linked_check.write_text("#!/bin/sh\nprintf 'LINKED\\n'\n")
    linked_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
        PATH=f"{shadow_dir}:{os.environ['PATH']}",
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "LINKED\n"
    assert not marker.exists()


def test_installed_check_handles_a_registered_worktree_path_ending_in_newline(tmp_path):
    primary, _ = make_checkout(tmp_path)
    git_fixture(primary, "init", "-q")
    git_fixture(primary, "config", "user.email", "test@example.com")
    git_fixture(primary, "config", "user.name", "Test")
    git_fixture(primary, "add", ".")
    git_fixture(primary, "commit", "-qm", "fixture")
    linked = tmp_path / "linked\n"
    git_fixture(primary, "worktree", "add", "-q", "-b", "issue-722-newline", str(linked))
    primary_check = primary / "scripts/check-harness"
    primary_check.write_text("#!/bin/sh\nprintf 'WRONG primary checkout\\n'\n")
    primary_check.chmod(0o755)
    linked_check = linked / "scripts/check-harness"
    linked_check.write_text("#!/bin/sh\nprintf 'LINKED\\n'\n")
    linked_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "LINKED\n"


def test_checkout_check_handles_a_primary_path_ending_in_newline(tmp_path):
    primary, _ = make_checkout(tmp_path)
    renamed = tmp_path / "primary\n"
    primary.rename(renamed)
    git_fixture(renamed, "init", "-q")
    git_fixture(renamed, "config", "user.email", "test@example.com")
    git_fixture(renamed, "config", "user.name", "Test")
    git_fixture(renamed, "add", ".")
    git_fixture(renamed, "commit", "-qm", "fixture")
    checker = renamed / "scripts/check-harness"
    checker.write_text("#!/bin/sh\nprintf 'PRIMARY\\n'\n")
    checker.chmod(0o755)

    result = invoke(renamed / "scripts/provenant", "check", cwd=renamed)

    assert result.returncode == 0, result.stderr
    assert result.stdout == "PRIMARY\n"


def test_non_git_product_nested_in_an_unrelated_repository_stays_non_git(tmp_path):
    outer = tmp_path / "outer"
    outer.mkdir()
    git_fixture(outer, "init", "-q")
    product, command = make_checkout(outer)

    result = invoke(command, "check", cwd=product)

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["cwd"] == str(product)


def test_check_reports_a_deleted_working_directory_with_a_remedy(tmp_path):
    checkout, command = make_checkout(tmp_path)
    gone = tmp_path / "gone"
    gone.mkdir()
    environment = clean_git_environment()
    for name in AMBIENT_ROOT_VARIABLES:
        environment.pop(name, None)
    environment["AGENT_FABRIC_PRODUCT_ROOT"] = str(checkout)

    result = subprocess.run(
        [
            "bash", "-c",
            'cd "$1" && rmdir "$1" && exec "$2" check',
            "provenant-deleted-cwd", str(gone), str(command),
        ],
        cwd=tmp_path,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "cannot resolve the working directory" in result.stderr
    assert "cd into the checkout you intend to check" in result.stderr
    assert "Traceback" not in result.stderr


def test_installed_check_refuses_a_symlinked_checker(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-symlink")
    outside = tmp_path / "outside-check"
    outside.write_text("#!/bin/sh\nprintf 'OUTSIDE\\n'\n")
    outside.chmod(0o755)
    linked_check = linked / "scripts/check-harness"
    linked_check.unlink()
    linked_check.symlink_to(outside)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "no regular executable scripts/check-harness" in result.stderr
    assert "OUTSIDE" not in result.stderr


@pytest.mark.parametrize("metadata_kind", ["missing", "garbled", "symlink"])
def test_installed_check_refuses_invalid_metadata_in_a_registered_worktree(
    tmp_path, metadata_kind
):
    primary, linked = make_registered_linked_checkout(
        tmp_path, f"issue-722-invalid-{metadata_kind}"
    )
    dot_git = linked / ".git"
    dot_git.unlink()
    if metadata_kind == "garbled":
        dot_git.write_text("invalid\n")
    elif metadata_kind == "symlink":
        dot_git.symlink_to(primary / ".git")
    linked_check = linked / "scripts/check-harness"
    linked_check.write_text("#!/bin/sh\nprintf 'WRONG linked checkout\\n'\n")
    linked_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "WRONG linked checkout" not in result.stderr


def test_installed_check_refuses_a_registered_worktree_path_replaced_by_a_symlink(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-path-symlink")
    outside = tmp_path / "outside-worktree"
    linked.rename(outside)
    linked.symlink_to(outside, target_is_directory=True)
    outside_check = outside / "scripts/check-harness"
    outside_check.write_text("#!/bin/sh\nprintf 'OUTSIDE\\n'\n")
    outside_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "registered checkout path is a symlink" in result.stderr
    assert "OUTSIDE" not in result.stderr


def test_installed_check_refuses_a_registered_path_replaced_by_another_repository(tmp_path):
    primary, linked = make_registered_linked_checkout(tmp_path, "issue-722-foreign-repo")
    displaced = tmp_path / "displaced-worktree"
    linked.rename(displaced)
    foreign, _ = make_checkout(tmp_path / "foreign")
    git_fixture(foreign, "init", "-q")
    git_fixture(foreign, "config", "user.email", "test@example.com")
    git_fixture(foreign, "config", "user.name", "Test")
    git_fixture(foreign, "add", ".")
    git_fixture(foreign, "commit", "-qm", "foreign")
    foreign.rename(linked)
    foreign_check = linked / "scripts/check-harness"
    foreign_check.write_text("#!/bin/sh\nprintf 'FOREIGN\\n'\n")
    foreign_check.chmod(0o755)
    command = install_stub(tmp_path)

    result = invoke(
        command,
        "check",
        cwd=linked,
        AGENT_FABRIC_PRODUCT_ROOT=str(primary),
    )

    assert result.returncode == 3
    assert result.stdout == ""
    assert "belongs to another Git repository" in result.stderr
    assert "FOREIGN" not in result.stderr


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
        ("fabric", ["tasks"]),
        ("batch", ["--manifest", "tasks.json"]),
        ("run", ["inspect"]),
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
        git_fixture(caller_cwd, "init", "-q")
        assert (caller_cwd / ".git").is_dir()
    else:
        caller_cwd = tmp_path / "nonrepo"
        caller_cwd.mkdir()
        assert not (caller_cwd / ".git").exists()

    result = invoke(command, subcommand, *arguments, cwd=caller_cwd)

    assert result.returncode == 0
    assert json.loads(result.stdout)["cwd"] == str(caller_cwd)


def test_check_and_fabric_delegate_without_reinterpreting_arguments(tmp_path):
    _, command = make_checkout(tmp_path)

    check = invoke(command, "check", "--doctor", cwd=tmp_path)
    fabric = invoke(command, "fabric", "send", "reviewer", "x y", cwd=tmp_path)

    assert json.loads(check.stdout)["argv"][1:] == ["--doctor"]
    assert json.loads(fabric.stdout)["argv"][1:] == ["send", "reviewer", "x y"]


def test_dispatch_delegates_without_reinterpreting_arguments(tmp_path):
    _, command = make_checkout(tmp_path)

    result = invoke(command, "dispatch", "--tool", "codex", "--task-id", "x y", cwd=tmp_path)

    payload = json.loads(result.stdout)
    assert result.returncode == 0
    assert payload["argv"][1:] == ["--tool", "codex", "--task-id", "x y"]
    assert payload["cwd"] == str(tmp_path)


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
    assert "fabric ...     runtime/fabric/bin/fabric ..." in result.stdout
    assert "batch ...      skills/orchestrate/scripts/batch_run.py ..." in result.stdout
    assert "doctor" not in result.stdout
    assert "project ..." not in result.stdout
    assert "Fabric derives who you are from the working directory" in result.stdout
