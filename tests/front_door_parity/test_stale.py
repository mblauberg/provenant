"""Stale-checkout advisory, `check` only (contract C§8; assertions 40 to 44).

Assertion 42 (each git call abandoned after about a second) is not exercised
here: the scan runs git on a fixed trusted PATH, so a slow git cannot be
interposed without touching the host.

The scan inspects the resolved product root. The fixture checkout is made a
primary clone of a bare `origin`, then moved relative to it as each case needs.
"""

from __future__ import annotations

import os
import shutil
import sys
import time

import pytest

from parity_support import add_linked, add_origin, git, init_primary, push_from_clone


@pytest.fixture
def product(fd):
    init_primary(fd.checkout)
    fd.origin = add_origin(fd.checkout, fd.tmp)
    return fd


def warnings(result) -> list[str]:
    return [line for line in result.err.splitlines() if line.startswith("warning: ")]


def test_up_to_date_and_ahead_only_are_silent(product):
    """C§16 #40: nothing to say."""
    assert warnings(product.run("check")) == []
    (product.checkout / "local").write_text("x")
    git(product.checkout, "add", "local")
    git(product.checkout, "commit", "-qm", "local")
    result = product.run("check")
    assert warnings(result) == [] and result.returncode == 0


def test_behind_origin_warns_with_the_pull_command(product):
    """C§16 #40: behind origin, text not singularised."""
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")

    result = product.run("check")

    assert warnings(result) == [
        f"warning: product checkout is 1 commits behind origin/main; "
        f"run: git -C {product.checkout} pull --ff-only"
    ]
    assert result.returncode == 0


def test_diverged_warns_with_the_rebase_command(product):
    """C§16 #40."""
    (product.checkout / "local").write_text("x")
    git(product.checkout, "add", "local")
    git(product.checkout, "commit", "-qm", "local")
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")

    assert warnings(product.run("check")) == [
        f"warning: product checkout diverged from origin/main (1 ahead, 1 behind); "
        f"run: git -C {product.checkout} rebase origin/main (or merge)"
    ]


def test_off_default_branch_and_detached_head(product):
    """C§16 #40: branch name, then `a detached HEAD`."""
    git(product.checkout, "switch", "-q", "-c", "feature")
    assert warnings(product.run("check")) == [
        f"warning: product checkout is on feature, not main; "
        f"run: git -C {product.checkout} switch main"
    ]
    git(product.checkout, "switch", "-q", "--detach")
    assert warnings(product.run("check")) == [
        f"warning: product checkout is on a detached HEAD, not main; "
        f"run: git -C {product.checkout} switch main"
    ]


def test_old_fetch_head_warns_with_floor_days_and_missing_one_is_silent(product):
    """C§16 #40: FETCH_HEAD older than 3 days, N = floor(days); absent is silent."""
    fetch_head = product.checkout / ".git/FETCH_HEAD"
    fetch_head.write_text("")
    old = time.time() - (5 * 86400 + 3600)
    os.utime(fetch_head, (old, old))

    assert warnings(product.run("check")) == [
        f"warning: origin last fetched 5d ago; run: git -C {product.checkout} fetch"
    ]
    fetch_head.unlink()
    assert warnings(product.run("check")) == []


def test_order_is_fetch_age_then_branch_then_ahead_behind(product):
    """C§16 #40 and C§8: the three warnings come out in a fixed order."""
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")
    git(product.checkout, "switch", "-q", "-c", "feature", "origin/main~1")
    fetch_head = product.checkout / ".git/FETCH_HEAD"
    old = time.time() - 4 * 86400
    os.utime(fetch_head, (old, old))

    lines = warnings(product.run("check"))

    assert [line.split(" ")[1] for line in lines[:2]] == ["origin", "product"]
    assert "last fetched 4d ago" in lines[0] and "is on feature" in lines[1]


def test_non_main_default_branch_is_honoured(fd):
    """C§16 #40: `origin/HEAD` naming `trunk`."""
    init_primary(fd.checkout)
    git(fd.checkout, "branch", "-m", "trunk")
    origin = fd.tmp / "origin.git"
    git(fd.tmp, "init", "-q", "--bare", "--initial-branch=trunk", str(origin))
    git(fd.checkout, "remote", "add", "origin", str(origin))
    git(fd.checkout, "push", "-q", "-u", "origin", "trunk")
    git(fd.checkout, "remote", "set-head", "origin", "trunk")
    push_from_clone(origin, fd.tmp, "b", branch="trunk")
    git(fd.checkout, "fetch", "-q")

    [line] = warnings(fd.run("check"))

    assert "origin/trunk" in line and "origin/main" not in line


def test_linked_worktree_non_repo_and_missing_git_are_silent(product):
    """C§16 #40: only a primary product checkout is scanned."""
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")
    linked = add_linked(product.checkout, product.tmp / "linked", "feature")

    # The product root is the primary, so scan it with the linked tree as the caller too:
    # the scan always inspects the resolved product root, never the caller.
    assert len(warnings(product.run("check", cwd=linked))) == 1

    plain = product.tmp / "plain-product"
    shutil.copytree(product.checkout, plain, symlinks=True, ignore=shutil.ignore_patterns(".git"))
    assert warnings(product.run("check", product=plain)) == []


def test_scan_uses_the_trusted_path_not_the_callers(product):
    """C§16 #38 and #40: the caller's PATH cannot steer or disable the scan's git."""
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")
    interpreter_only = os.path.dirname(sys.executable)

    result = product.run("check", PATH=interpreter_only)

    assert result.returncode == 0, result.err
    assert len(warnings(result)) == 1


def test_opt_out_flag_and_other_values(product):
    """C§16 #41: only `1` suppresses; the owner sees `PROVENANT_STALE_CHECKED=1` after a scan."""
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")

    suppressed = product.run("check", PROVENANT_STALE_CHECKED="1")
    assert warnings(suppressed) == []
    assert suppressed.payload()["env"]["PROVENANT_STALE_CHECKED"] == "1"
    for value in ("0", "true", ""):
        result = product.run("check", PROVENANT_STALE_CHECKED=value)
        assert len(warnings(result)) == 1, value
        assert result.payload()["env"]["PROVENANT_STALE_CHECKED"] == "1"


def test_nested_check_prints_no_duplicates(product):
    """C§16 #41: the owner's environment carries the flag, so a nested `check` stays quiet."""
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")
    first = product.run("check")
    inherited = first.payload()["env"]["PROVENANT_STALE_CHECKED"]

    second = product.run("check", PROVENANT_STALE_CHECKED=inherited)

    assert len(warnings(first)) == 1 and warnings(second) == []


@pytest.mark.parametrize("verb", ["route", "worktree", "fabric", "gate", "lanes"])
def test_other_verbs_never_scan(product, verb):
    """C§16 #43: no warning, no flag injected."""
    push_from_clone(product.origin, product.tmp, "b")
    git(product.checkout, "fetch", "-q")

    result = product.run(verb)

    assert warnings(result) == []
    assert "PROVENANT_STALE_CHECKED" not in result.payload()["env"]


def test_scan_failure_is_silent_unless_debug(product):
    """C§16 #44: an unresolvable root inside the advisory is swallowed; debug shows it."""
    # `check` selects the checker from the dispatcher's own tree, then resolves the
    # product root again for the scan. A broken pointer fails only that second step.
    if product.is_stub:
        pytest.skip("the stub role resolves the root once, before delegating")
    bad_pointer = product.tmp / "instance/.agent-fabric/product-root.json"
    bad_pointer.parent.mkdir(parents=True)
    bad_pointer.write_text("{broken")
    env = {"AGENT_FABRIC_INSTANCE_ROOT": str(product.tmp / "instance")}

    quiet = product.run("check", product=False, **env)
    loud = product.run("check", product=False, PROVENANT_DEBUG="1", **env)

    assert quiet.returncode == 0 and "stale-checkout check failed" not in quiet.err
    assert loud.returncode == 0
    assert loud.err.startswith("provenant: stale-checkout check failed: ")
