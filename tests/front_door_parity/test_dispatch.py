"""Verb table, argv shapes and exec semantics (contract C§1, C§2, C§7; #1, 2, 4 to 10, 34)."""

from __future__ import annotations

import os
import signal
import subprocess
import time

import pytest

from parity_support import (
    REPAIR,
    needs_lease,
    needs_lint,
    sha256,
)

# (argv prefix, owner path, owner argv prefix). The caller's remaining
# arguments follow the prefix in both columns.
VERBS = [
    ("route", "scripts/model-route", []),
    ("routes", "scripts/model-route", ["routes"]),
    ("capabilities", "skills/orchestrate/scripts/capabilities.py", []),
    ("worktree", "scripts/worktree", []),
    ("clean", "scripts/clean.py", []),
    ("check", "scripts/check-harness", []),
    ("gate", "scripts/gate", []),
    ("telemetry", "scripts/provenant-telemetry", []),
    ("stats", "scripts/provenant-telemetry", ["stats"]),
    ("retro", "scripts/provenant-telemetry", ["retro"]),
    ("quota", "scripts/provenant-quota", []),
    ("profile", "scripts/provenant-profile", []),
    ("fabric", "runtime/fabric/bin/fabric", []),
    ("lanes", "runtime/fabric/bin/fabric", ["lanes"]),
    ("events", "runtime/fabric/bin/fabric", ["events"]),
    ("hosts", "scripts/fabric-hosts", ["hosts"]),
    ("peer", "scripts/fabric-hosts", ["peer"]),
    ("dispatch", "skills/orchestrate/scripts/dispatch_run.py", []),
    ("batch", "skills/orchestrate/scripts/batch_run.py", []),
    ("run", "skills/orchestrate/scripts/run_controls.py", []),
    pytest.param("lint", "scripts/provenant_lint.py", [], marks=needs_lint),
]

AWKWARD = [
    "",
    "two words",
    "--literal=*",
    "-x",
    "'single'",
    '"double"',
    "$HOME",
    "a\nb",
    "\udcff\udcfe",  # non-UTF-8 bytes, round-tripped as surrogate escapes
]


@pytest.mark.parametrize("verb,owner,prefix", VERBS)
def test_verb_execs_its_owner_with_the_documented_argv(fd, verb, owner, prefix):
    """C§16 #1 and #2: owner path, argv[0] and argv shape for every verb."""
    result = fd.run(verb, "first", *AWKWARD)
    payload = result.payload()

    assert result.returncode == 0, result.err
    assert result.err == "dummy stderr\n"
    assert payload["argv0"] == str(fd.owner(owner))
    assert payload["argv"] == [*prefix, "first", *AWKWARD]


@pytest.mark.parametrize("verb,owner,prefix", VERBS)
def test_verb_with_no_arguments(fd, verb, owner, prefix):
    """C§16 #2: a verb alone passes only its own prefix."""
    payload = fd.run(verb).payload()

    assert payload["argv"] == prefix
    assert payload["argv0"] == str(fd.owner(owner))


def test_clean_as_a_later_argument_does_not_select_the_clean_owner(fd):
    """C§16 #4: only a leading `clean` is special."""
    payload = fd.run("route", "clean", "now").payload()

    assert payload["argv0"] == str(fd.owner("scripts/model-route"))
    assert payload["argv"] == ["clean", "now"]


def test_stub_clean_bypasses_the_checkout_dispatcher(stub):
    """C§16 #4 [S]: `clean` reaches `clean.py` even with no dispatcher present."""
    stub.owner("scripts/provenant").unlink()

    result = stub.run("clean", "--prune-merged")

    assert result.returncode == 0, result.err
    assert result.payload()["argv0"] == str(stub.owner("scripts/clean.py"))
    assert result.payload()["argv"] == ["--prune-merged"]


@needs_lease
@pytest.mark.parametrize("verb", ["acquire", "release", "verify", "status", "push"])
def test_lease_verbs_rewrite_to_fabric_landing(fd, verb):
    """C§16 #10 (#1095): `lease VERB ARGS` runs fabric `landing-VERB ARGS`."""
    result = fd.run("lease", verb, "land", "--json", PROVENANT_TEST_EXIT="3")
    payload = result.payload()

    assert result.returncode == 3  # the owner's status passes through
    assert payload["argv0"] == str(fd.owner("runtime/fabric/bin/fabric"))
    assert payload["argv"] == [f"landing-{verb}", "land", "--json"]
    assert payload["cwd"] == str(fd.cwd)


@needs_lease
@pytest.mark.parametrize("arguments", [(), ("wait",), ("--json",), ("",)])
def test_lease_without_a_known_verb_is_a_usage_error(fd, arguments):
    """C§16 #10: first line of the lease detail on stderr, exit 2, no stdout."""
    result = fd.run("lease", *arguments)
    detail = fd.run("help", "lease").out

    assert result.returncode == 2
    assert result.stdout == b""
    assert result.out == ""
    assert result.err == detail.splitlines()[0] + "\n"
    assert result.err.startswith("usage: provenant lease <acquire|release|verify|status|push>")


@needs_lease
@pytest.mark.parametrize("arguments", [("-h",), ("--help",), ("--help", "acquire"), ("-h", "junk")])
def test_lease_help_flags_print_the_detail(fd, arguments):
    """C§16 #10: `lease -h|--help` prints the detail and exits 0."""
    result = fd.run("lease", *arguments)

    assert result.returncode == 0
    assert result.stdout == fd.run("help", "lease").stdout
    assert result.stderr == b""


@needs_lint
def test_lint_does_not_scrub_git_variables_but_check_does(fd):
    """C§16 #9 (#1094): GIT_DIR and GIT_INDEX_FILE reach lint, not check."""
    redirects = {"GIT_DIR": "/nonexistent/git-dir", "GIT_INDEX_FILE": "/nonexistent/index"}
    lint = fd.run("lint", "commit-msg", "MSG", **redirects).payload()
    assert lint["env"]["GIT_DIR"] == "/nonexistent/git-dir"
    assert lint["env"]["GIT_INDEX_FILE"] == "/nonexistent/index"

    check = fd.run("check", **redirects).payload()
    assert "GIT_DIR" not in check["env"] and "GIT_INDEX_FILE" not in check["env"]


# ------------------------------------------------------------- exec semantics


@pytest.mark.parametrize("kind", ["checkout", "other-repo", "non-repo", "symlink"])
def test_cwd_is_unchanged_for_every_verb(fd, kind):
    """C§16 #5: the owner starts in the caller's directory."""
    if kind == "checkout":
        cwd = fd.checkout
    else:
        cwd = fd.tmp / "elsewhere" / "nested"
        cwd.mkdir(parents=True)
        if kind == "other-repo":
            subprocess.run(["git", "init", "-q", str(cwd)], check=True)
        if kind == "symlink":
            link = fd.tmp / "link"
            link.symlink_to(cwd)
            cwd = link
    for verb in ("route", "worktree", "fabric", "quota"):
        assert fd.run(verb, cwd=cwd).payload()["cwd"] == os.path.realpath(cwd)


def test_stdin_reaches_the_owner_untouched(fd):
    """C§16 #6: binary stdin larger than a pipe buffer arrives byte for byte."""
    data = bytes(range(256)) * 4096  # 1 MiB, every byte value, NULs included

    payload = fd.run("route", stdin=data).payload()

    assert payload["stdin_sha256"] == sha256(data)


def test_stdout_and_stderr_are_not_buffered_or_wrapped(fd):
    """C§16 #6: owner output order and bytes survive; nothing is added."""
    owner = fd.owner("scripts/model-route")
    owner.write_text("#!/bin/sh\nprintf 'out1\\n'\nprintf 'err1\\n' >&2\nprintf 'out2\\n'\n")

    result = fd.run("route")

    assert (result.stdout, result.stderr, result.returncode) == (b"out1\nout2\n", b"err1\n", 0)


@pytest.mark.parametrize("status", [0, 1, 2, 3, 7, 127])
def test_owner_exit_status_is_passed_through(fd, status):
    """C§16 #7: owner statuses are never remapped."""
    result = fd.run("route", PROVENANT_TEST_EXIT=str(status))

    assert result.returncode == status


@pytest.mark.parametrize("name", ["TERM", "KILL", "INT"])
def test_owner_death_by_signal_is_visible_to_the_caller(fd, name):
    """C§16 #7: the caller sees the owner's signal death, so nothing waits in between."""
    fd.owner("scripts/model-route").write_text(f"#!/bin/sh\nkill -{name} $$\nsleep 5\n")

    result = fd.run("route")

    assert result.returncode == -getattr(signal, f"SIG{name}")


@pytest.mark.parametrize("sig", [signal.SIGINT, signal.SIGTERM])
def test_front_door_is_a_true_exec_and_signals_reach_the_owner(fd, sig):
    """C§16 #7 and #8: the owner's pid is the spawned pid; signals land on it."""
    ready = fd.tmp / "ready"
    pid_file = fd.tmp / "pid"
    fd.owner("scripts/model-route").write_text(
        f"#!/bin/sh\necho $$ > {pid_file}.tmp\nmv {pid_file}.tmp {pid_file}\n"
        f": > {ready}\nexec sleep 30\n"
    )
    process = subprocess.Popen(
        [str(fd.command), "route"],
        cwd=fd.cwd,
        env=fd.env(AGENT_FABRIC_PRODUCT_ROOT=str(fd.checkout)),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        deadline = time.monotonic() + 30
        while not ready.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert ready.exists(), "owner never started"
        assert int(pid_file.read_text()) == process.pid
        process.send_signal(sig)
        assert process.wait(timeout=10) == -sig
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()


def test_other_environment_reaches_the_owner_byte_for_byte(fd):
    """C§16 #30: PATH, HARNESS_PYTHON, PYTHON*, FOO and non-UTF-8 values survive."""
    extra = {
        "FOO": "bar baz",
        "HARNESS_PYTHON": "/opt/does/not/matter",
        "PYTHONOPTIMIZE": "1",
        "WEIRD": "\udcff\udcfe",
        "EMPTY": "",
        "PROVENANT_TEST_MARKER": "kept",
    }
    env = fd.env(**extra)
    env["PATH"] = f"/usr/bin:/bin:{os.environ.get('PATH', '')}"

    payload = fd.run("route", env={**env, "AGENT_FABRIC_PRODUCT_ROOT": str(fd.checkout)}).payload()

    for name, value in extra.items():
        assert payload["env"][name] == value
    assert payload["env"]["PATH"] == env["PATH"]


# ------------------------------------------------------------- exec failure


def break_owner(fd, how: str) -> str:
    path = fd.owner("scripts/model-route")
    path.unlink()
    if how == "missing":
        pass
    elif how == "directory":
        path.mkdir()
    elif how == "not-executable":
        path.write_text("#!/bin/sh\nexit 0\n")
        path.chmod(0o644)
    elif how == "bad-shebang":
        path.write_text("#!/nonexistent/interpreter\n")
        path.chmod(0o755)
    return str(path)


@pytest.mark.parametrize("how", ["missing", "directory", "not-executable", "bad-shebang"])
def test_exec_failure_is_exit_3_with_a_repair_line_and_no_traceback(fd, how):
    """C§16 #34 and #25: `provenant: cannot execute <path>: <strerror>; repair: ...`."""
    path = break_owner(fd, how)

    result = fd.run("route")

    assert result.returncode == 3
    assert result.stdout == b""
    assert result.err.startswith(f"provenant: cannot execute {path}: ")
    assert result.err.endswith(f"; {REPAIR}\n")
    assert result.err.count("\n") == 1
    assert "Traceback" not in result.err and "panicked" not in result.err
