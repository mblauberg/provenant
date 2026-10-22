"""Product-root and instance-root resolution (contract C§4; assertions 17 to 25, 48 to 50)."""

from __future__ import annotations

import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess

import pytest

from parity_support import FABRIC_MCP, ROOT


def make_root(base: Path, name: str) -> Path:
    """A minimal product root: an executable Fabric MCP launcher plus the Python
    dispatcher, which the Python stub execs once it has resolved the root."""
    root = base / name
    launcher = root / FABRIC_MCP
    launcher.parent.mkdir(parents=True)
    launcher.write_text("#!/bin/sh\nexit 0\n")
    launcher.chmod(0o755)
    scripts = root / "scripts"
    scripts.mkdir()
    shutil.copy2(ROOT / "scripts/provenant", scripts / "provenant")
    shutil.copytree(ROOT / "scripts/lib", scripts / "lib")
    return root.resolve()


def write_pointer(instance: Path, content: str | bytes) -> Path:
    pointer = instance / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True, exist_ok=True)
    pointer.write_bytes(content.encode() if isinstance(content, str) else content)
    return pointer


def pointer_to(root: Path | str, **extra) -> str:
    return json.dumps({"schema_version": 1, "product_root": str(root), **extra})


def root_of(fd, **env) -> object:
    return fd.run("root", product=False, **env)


def assert_single_line_failure(result) -> None:
    """C§16 #25: `provenant: <message>; repair: ...`, stdout empty, exit 3."""
    assert result.returncode == 3, result.err
    assert result.stdout == b""
    assert result.err.startswith("provenant: ")
    assert result.err.count("\n") == 1 and result.err.endswith("\n")
    assert "Traceback" not in result.err and "panicked" not in result.err
    assert "repair:" in result.err


def test_root_prints_the_resolved_checkout(fd):
    """C§16 #17: realpath plus newline, exit 0; extra arguments are usage."""
    result = fd.run("root")

    assert result.returncode == 0, result.err
    assert result.stdout == f"{fd.checkout}\n".encode()
    assert fd.run("root", "extra").returncode == 2


def test_root_ignores_the_local_checkout(dispatcher):
    """C§16 #17 [D]: `root` uses root selection, not the tree the dispatcher lives in."""
    other = make_root(dispatcher.tmp, "other")

    result = dispatcher.run("root", product=other)

    assert result.out == f"{other}\n"


@pytest.mark.parametrize("rung", ["explicit", "pointer", "agents-home", "default"])
def test_precedence_one_rung_at_a_time(fd, rung):
    """C§16 #18: explicit > pointer > AGENTS_HOME > ~/.agents, each beating all below."""
    explicit = make_root(fd.tmp, "explicit")
    pointed = make_root(fd.tmp, "pointed")
    agents_home = make_root(fd.tmp, "agents-home")
    shutil.copytree(make_root(fd.tmp, "default-src"), fd.home / ".agents")
    instance = fd.tmp / "instance"
    write_pointer(instance, pointer_to(pointed))
    # Every rung below the winner is configured to a different valid root.
    env = {"AGENT_FABRIC_INSTANCE_ROOT": str(instance), "AGENTS_HOME": str(agents_home)}
    if rung == "explicit":
        env["AGENT_FABRIC_PRODUCT_ROOT"] = str(explicit)
    if rung in ("agents-home", "default"):
        env["AGENT_FABRIC_INSTANCE_ROOT"] = str(fd.tmp / "no-pointer-here")
    if rung == "default":
        del env["AGENTS_HOME"]
    want = {
        "explicit": explicit,
        "pointer": pointed,
        "agents-home": agents_home,
        "default": (fd.home / ".agents").resolve(),
    }[rung]

    result = root_of(fd, **env)

    assert result.out == f"{want}\n", result.err


def test_empty_values_behave_as_unset(fd):
    """C§16 #19: empty root variables fall through to the default instance pointer."""
    pointed = make_root(fd.tmp, "pointed")
    write_pointer(fd.home / ".agents", pointer_to(pointed))

    result = root_of(
        fd, AGENT_FABRIC_INSTANCE_ROOT="", AGENT_FABRIC_PRODUCT_ROOT="", AGENTS_HOME=""
    )

    assert result.out == f"{pointed}\n", result.err


def test_agents_home_is_never_the_instance_root(fd):
    """C§16 #20: a pointer under ~/.agents wins over AGENTS_HOME naming another root."""
    pointed = make_root(fd.tmp, "pointed")
    decoy = make_root(fd.tmp, "decoy")
    write_pointer(fd.home / ".agents", pointer_to(pointed))
    write_pointer(decoy, pointer_to(decoy))  # a pointer inside AGENTS_HOME must not count

    result = root_of(fd, AGENTS_HOME=str(decoy))

    assert result.out == f"{pointed}\n", result.err


def test_tilde_expands_against_home(fd):
    """C§16 #21: `~` and `~/x` expand in the root variables."""
    make_root(fd.home, "prod")

    for value, want in (("~/prod", fd.home / "prod"),):
        for name in ("AGENT_FABRIC_PRODUCT_ROOT", "AGENTS_HOME"):
            assert root_of(fd, **{name: value}).out == f"{want.resolve()}\n"
    write_pointer(fd.home / "inst", pointer_to(want))
    assert root_of(fd, AGENT_FABRIC_INSTANCE_ROOT="~/inst").out == f"{want.resolve()}\n"


@pytest.mark.parametrize(
    "name,message",
    [
        ("AGENT_FABRIC_PRODUCT_ROOT", "AGENT_FABRIC_PRODUCT_ROOT must be an absolute path"),
        ("AGENTS_HOME", "AGENTS_HOME must be an absolute path"),
        ("AGENT_FABRIC_INSTANCE_ROOT", "instance root must be an absolute path"),
    ],
)
def test_relative_values_are_refused(fd, name, message):
    """C§16 #21 and #25: relative root values exit 3 with the documented text."""
    result = root_of(fd, **{name: "relative/path"})

    assert_single_line_failure(result)
    assert message in result.err


def test_symlinked_and_unnormalised_roots_print_resolved(fd):
    """C§16 #21: symlinks resolve; trailing slash and `..` normalise."""
    real = make_root(fd.tmp, "real")
    link = fd.tmp / "link"
    link.symlink_to(real)
    (fd.tmp / "sub").mkdir()

    for value in (str(link), f"{real}/", f"{fd.tmp}/sub/../real", f"{link}/../real"):
        result = root_of(fd, AGENT_FABRIC_PRODUCT_ROOT=value)
        assert result.out == f"{real}\n", (value, result.err)


@pytest.mark.parametrize("damage", ["missing", "directory", "not-executable", "dangling"])
def test_launcher_must_be_an_executable_regular_file(fd, damage):
    """C§16 #22: a bad `runtime/fabric/bin/fabric-mcp` exits 3."""
    root = make_root(fd.tmp, "damaged")
    launcher = root / FABRIC_MCP
    launcher.unlink()
    if damage == "directory":
        launcher.mkdir()
    elif damage == "not-executable":
        launcher.write_text("#!/bin/sh\n")
        launcher.chmod(0o644)
    elif damage == "dangling":
        launcher.symlink_to(fd.tmp / "nowhere")

    result = root_of(fd, AGENT_FABRIC_PRODUCT_ROOT=str(root))

    assert_single_line_failure(result)
    assert f"Fabric MCP launcher is missing or not executable at {launcher}" in result.err


def test_missing_default_root_fails_loudly_and_creates_nothing(fd):
    """C§16 #22 and #24: no root anywhere is exit 3 and `~/.agents` stays absent."""
    result = root_of(fd)

    assert_single_line_failure(result)
    assert not (fd.home / ".agents").exists()


POINTER_CASES = {
    "relative-root": pointer_to("relative/root"),
    "schema-2": json.dumps({"schema_version": 2, "product_root": "/x"}),
    "schema-string": json.dumps({"schema_version": "1", "product_root": "/x"}),
    "schema-float": '{"schema_version": 1.0, "product_root": "/x"}',
    "schema-true": '{"schema_version": true, "product_root": "/x"}',
    "schema-null": '{"schema_version": null, "product_root": "/x"}',
    "schema-missing": json.dumps({"product_root": "/x"}),
    "root-missing": json.dumps({"schema_version": 1}),
    "root-number": json.dumps({"schema_version": 1, "product_root": 7}),
    "root-empty": json.dumps({"schema_version": 1, "product_root": ""}),
    "top-level-list": "[]",
    "top-level-string": '"x"',
    "invalid-json": "{not json",
    "empty-file": "",
    "stale": pointer_to("/definitely/not/a/real/product/root"),
}


@pytest.mark.parametrize("case", sorted(POINTER_CASES))
def test_bad_pointers_fail_and_never_fall_back(fd, case):
    """C§16 #23: each bad pointer is exit 3 naming the pointer, despite valid fallbacks."""
    valid = make_root(fd.tmp, "valid")
    shutil.copytree(valid, fd.home / ".agents", dirs_exist_ok=True)
    write_pointer(fd.home / ".agents", POINTER_CASES[case])

    result = root_of(fd, AGENTS_HOME=str(valid))

    assert_single_line_failure(result)
    assert "product-root pointer" in result.err


def test_pointer_that_is_a_directory_or_dangling_link_fails(fd):
    """C§16 #23: unreadable pointer shapes."""
    valid = make_root(fd.tmp, "valid")
    pointer = fd.home / ".agents/.agent-fabric/product-root.json"
    pointer.mkdir(parents=True)
    assert_single_line_failure(root_of(fd, AGENTS_HOME=str(valid)))
    pointer.rmdir()
    pointer.symlink_to(fd.tmp / "gone.json")
    result = root_of(fd, AGENTS_HOME=str(valid))
    assert_single_line_failure(result)
    assert "product-root pointer" in result.err


def test_pointer_with_invalid_utf8_is_exit_3(fd):
    """C§16 #23: only the exit status and the `provenant: ` prefix are pinned."""
    write_pointer(fd.home / ".agents", b'{"schema_version": 1, "product_root": "\xff\xfe"}')

    result = root_of(fd)

    assert result.returncode == 3 and result.stdout == b""
    assert result.err.startswith("provenant: ")


def test_pointer_with_extra_keys_and_duplicate_keys_works(fd):
    """C§16 #23: unknown keys are ignored; with duplicate keys the last wins."""
    first = make_root(fd.tmp, "first")
    second = make_root(fd.tmp, "second")
    write_pointer(
        fd.home / ".agents",
        '{"schema_version": 1, "extra": [1, {"a": null}], '
        f'"product_root": {json.dumps(str(first))}, "product_root": {json.dumps(str(second))}}}',
    )

    assert root_of(fd).out == f"{second}\n"


def test_explicit_root_ignores_a_broken_pointer(fd):
    """C§4a: an explicit product root never parses the pointer (stub); the dispatcher cannot fail on it either."""
    write_pointer(fd.home / ".agents", "{not json")

    result = fd.run("root")

    assert result.out == f"{fd.checkout}\n", result.err


def test_resolution_never_writes_a_pointer_or_creates_the_default_instance(fd):
    """C§16 #24: compare listings and mtimes before and after a spread of commands."""
    pointed = make_root(fd.tmp, "pointed")
    instance = fd.tmp / "instance"
    pointer = write_pointer(instance, pointer_to(pointed))

    def snapshot():
        return sorted(
            (str(p.relative_to(fd.tmp)), p.stat().st_mtime_ns, p.stat().st_size)
            for p in list(instance.rglob("*")) + [fd.home / ".agents"] if p.exists()
        )

    before = snapshot()
    pointer_bytes = pointer.read_bytes()
    for arguments in (("root",), ("help",), ("bogus",)):
        root_of(fd, AGENT_FABRIC_INSTANCE_ROOT=str(instance))
        fd.run(*arguments, AGENT_FABRIC_INSTANCE_ROOT=str(instance))
    fd.run("route", AGENT_FABRIC_INSTANCE_ROOT=str(instance))

    assert snapshot() == before
    assert pointer.read_bytes() == pointer_bytes
    assert not (fd.home / ".agents").exists()


# --------------------------------------------------------- invocation modes


def test_works_through_symlink_relative_path_path_lookup_and_odd_argv0(fd):
    """C§16 #48: how the command is named does not change the answer."""
    expected = f"{fd.checkout}\n"
    env = fd.env(AGENT_FABRIC_PRODUCT_ROOT=str(fd.checkout))

    link = fd.tmp / "elsewhere" / "pv"
    link.parent.mkdir()
    link.symlink_to(fd.command)
    done = subprocess.run([str(link), "root"], env=env, capture_output=True, cwd=fd.cwd)
    assert done.stdout.decode() == expected, done.stderr

    done = subprocess.run(
        ["./" + fd.command.name, "root"], env=env, capture_output=True, cwd=fd.command.parent
    )
    assert done.stdout.decode() == expected, done.stderr

    path_env = {**env, "PATH": f"{fd.command.parent}{os.pathsep}{env['PATH']}"}
    done = subprocess.run(
        [fd.command.name, "root"], env=path_env, capture_output=True, cwd=fd.cwd
    )
    assert done.stdout.decode() == expected, done.stderr

    assert fd.run("root", argv0="not-provenant").out == expected

    cwd_link = fd.tmp / "cwd-link"
    cwd_link.symlink_to(fd.cwd)
    assert fd.run("root", cwd=cwd_link).out == expected


def test_runs_with_a_minimal_environment(fd):
    """C§16 #49: `env -i` plus HOME (and a PATH for script shebangs) still works."""
    path = f"{Path(os.sys.executable).parent}{os.pathsep}/usr/bin{os.pathsep}/bin"
    env = {"HOME": str(fd.home), "PATH": path, "AGENT_FABRIC_PRODUCT_ROOT": str(fd.checkout)}

    result = fd.run("root", env=env)

    assert result.out == f"{fd.checkout}\n", result.err
    payload = fd.run("route", env=env).payload()
    assert payload["env"]["AGENT_FABRIC_PRODUCT_ROOT"] == str(fd.checkout)


def test_home_unset_falls_back_to_the_passwd_home(fd):
    """C§16 #49: with HOME unset the default root is the passwd home's `.agents`."""
    env = fd.env()
    env.pop("HOME")
    passwd_home = os.path.realpath(pwd.getpwuid(os.getuid()).pw_dir)

    result = fd.run("root", env=env)

    if result.returncode == 0:
        assert result.out.endswith("\n") and os.path.isabs(result.out.strip())
    else:
        assert_single_line_failure(result)
        assert f"{passwd_home}/.agents" in result.err or "product-root pointer" in result.err


@pytest.mark.parametrize("name", ["with space", "with\nnewline", "non-\udcffutf8"])
def test_roots_with_awkward_paths(fd, name):
    """C§16 #50: spaces, newlines and non-UTF-8 bytes in the root path."""
    try:
        root = make_root(fd.tmp, name)
    except OSError:
        pytest.skip("filesystem refuses this file name")

    result = fd.run("root", product=root)

    assert result.stdout == os.fsencode(root) + b"\n", result.err
