"""Help, usage and per-command text (contract C§15, assertions 11 to 16, 3, 14).

Help text is compared with the live Python dispatcher (`reference_run`), never a
hand-copied golden, so the suite follows the Python text when it changes. The
structural assertions hold for either implementation on their own.
"""

from __future__ import annotations

import pytest

from parity_support import (
    FABRIC_MCP,
    HELP_BUDGET,
    IMPL,
    SUPPORTS_LEASE,
    SUPPORTS_LINT,
    assert_matches_reference,
    reference_run,
)

MAIN_TOPICS = (
    "root route routes capabilities worktree clean check gate fabric hosts peer "
    "lanes events telemetry stats retro quota profile dispatch batch run help"
).split()
TOPICS = [
    *MAIN_TOPICS,
    *(["lint"] if SUPPORTS_LINT else []),
    *(["lease"] if SUPPORTS_LEASE else []),
]
USAGE_PREFIX = "usage: provenant <help|root|route|"


def drop_root(fd) -> None:
    """Make the product root unresolvable: no launcher in the fixture, empty HOME."""
    fd.owner(FABRIC_MCP).unlink()


@pytest.mark.parametrize("flag", ["help", "-h", "--help"])
def test_general_help_is_golden_and_quiet_on_stderr(fd, flag):
    """C§16 #11: help, -h, --help print the general help to stdout, exit 0."""
    result = fd.run(flag)

    assert result.returncode == 0, result.err
    assert result.stderr == b""
    assert_matches_reference(result, reference_run("help"))


def test_general_help_stays_within_the_byte_budget_and_names_owners(fd):
    """C§16 #12: byte budget, key lines, one physical line per command."""
    result = fd.run("help")
    text = result.out

    assert len(result.stdout) <= HELP_BUDGET
    assert result.stderr == b""
    assert "route" in text and "scripts/model-route" in text
    assert "  fabric ...     runtime/fabric/bin/fabric ..." in text
    assert "  batch ...      skills/orchestrate/scripts/batch_run.py ..." in text
    assert "project ..." not in text
    assert all("\r" not in line for line in text.splitlines())
    if SUPPORTS_LINT:
        assert "scripts/provenant_lint.py" in text
    if SUPPORTS_LEASE:
        assert "landing-*" in text


@pytest.mark.parametrize("topic", TOPICS)
@pytest.mark.parametrize("flag", ["help", "-h", "--help"])
def test_command_detail_is_golden(fd, flag, topic):
    """C§16 #13: `help K`, `-h K`, `--help K` print the detail for every key."""
    if topic == "routes" and flag == "help":
        pytest.skip("`help routes` executes the route owner (assertion 3)")
    result = fd.run(flag, topic)

    assert result.returncode == 0, result.err
    assert result.stderr == b""
    assert result.out.startswith("usage: provenant ") or topic == "help"
    assert result.stdout.endswith(b"\n") and not result.stdout.endswith(b"\n\n")
    assert_matches_reference(result, reference_run(flag, topic))


@pytest.mark.parametrize(
    "arguments",
    [
        (),
        ("bogus",),
        ("",),
        ("--version",),
        ("-v",),
        ("ROUTE",),
        ("Route",),
        ("help", "bogus"),
        ("help", "root", "extra"),
        ("-h", "bogus"),
        ("--help", "a", "b"),
        ("root", "x"),
    ],
    ids=lambda a: " ".join(a) or "no-arguments",
)
def test_usage_errors_are_one_stderr_line_with_exit_2(fd, arguments):
    """C§16 #14 (and #33): the generic usage line, stderr only, exit 2."""
    result = fd.run(*arguments)

    assert result.returncode == 2
    assert result.stdout == b""
    assert result.out == ""
    assert result.err.startswith(USAGE_PREFIX) and result.err.endswith("[arguments...]\n")
    assert result.err.count("\n") == 1
    assert_matches_reference(result, reference_run(*arguments))


def test_help_works_without_a_resolvable_product_root(fd):
    """C§16 #15 and divergence D1.

    The dispatcher never needed a root for help. The Python stub resolves
    first and exits 3; the Rust binary answers help and usage in both roles.
    """
    drop_root(fd)
    stub_resolves_first = IMPL == "python" and fd.is_stub

    helped = fd.run("help", product=False)
    usage = fd.run("bogus", product=False)

    if stub_resolves_first:
        assert helped.returncode == 3 and usage.returncode == 3
        assert "provenant: " in helped.err and "repair:" in helped.err
        assert helped.stdout == b""
    else:
        assert helped.returncode == 0
        if IMPL == "rust":
            # No root means no owners to prove #1094/#1095: base help, whatever
            # the reference tree carries.
            assert "  lint " not in helped.out and "  lease " not in helped.out
            assert "  lanes ...      runtime/fabric/bin/fabric lanes\n" in helped.out
            assert "|lint|" not in usage.err and "|lease|" not in usage.err
        else:
            assert_matches_reference(helped, reference_run("help"))
        assert usage.returncode == 2 and usage.err.startswith(USAGE_PREFIX)


def test_help_routes_executes_but_dash_h_routes_prints_detail(fd):
    """C§16 #3 and #16: `help routes` runs the owner; `-h routes` is text."""
    result = fd.run("help", "routes")
    payload = result.payload()

    assert result.returncode == 0
    assert payload["argv"] == ["routes", "--health"]
    assert payload["argv0"] == str(fd.owner("scripts/model-route"))
    for flag in ("-h", "--help"):
        detail = fd.run(flag, "routes")
        assert detail.returncode == 0
        assert detail.out.startswith("usage: provenant routes")
        assert_matches_reference(detail, reference_run(flag, "routes"))
    assert fd.run("help", "routes", "extra").returncode == 2


def test_help_routes_needs_a_root_but_dash_h_routes_does_not(fd):
    """C§16 #16: with resolution failing, `help routes` exits 3, `-h routes` 0."""
    drop_root(fd)

    executes = fd.run("help", "routes", product=False)
    text = fd.run("-h", "routes", product=False)

    assert executes.returncode == 3
    assert executes.stdout == b""
    assert executes.err.startswith("provenant: ")
    if IMPL == "python" and fd.is_stub:
        assert text.returncode == 3  # D1: the old stub resolved before anything
    else:
        assert text.returncode == 0


def test_lint_is_listed_in_usage_and_help(fd):
    """C§2a (#1094): lint appears between clean and check."""
    if not SUPPORTS_LINT:
        pytest.skip("lint (#1094) is not on this branch")
    usage = fd.run("bogus").err
    assert "|clean|lint|check|" in usage
    text = fd.run("help").out
    assert text.index("  clean ") < text.index("  lint ") < text.index("  check ")


def test_lease_is_listed_in_usage_and_help(fd):
    """C§2b (#1095): lease follows profile in usage; help merges lanes|events."""
    if not SUPPORTS_LEASE:
        pytest.skip("lease (#1095) is not on this branch")
    assert "|lease|" in fd.run("bogus").err
    text = fd.run("help").out
    assert "  lanes|events   runtime/fabric/bin/fabric lanes|events" in text
    assert "`provenant help <command>` for detail." in text


@pytest.mark.skipif(IMPL != "rust", reason="the Python dispatcher is the reference, not a subject")
def test_rust_follows_the_tree_when_lint_and_lease_owners_are_absent(fd):
    """A tree without #1094/#1095 answers as its Python dispatcher would: no lint, no lease."""
    for relative in ("scripts/provenant_lint.py", "runtime/fabric/src/landing-cli.ts"):
        path = fd.owner(relative)
        if path.exists():
            path.unlink()
    help_text = fd.run("help").out
    usage = fd.run("bogus").err

    assert "  lint " not in help_text and "  lease " not in help_text
    assert "|lint|" not in usage and "|lease|" not in usage
    for arguments in (("lint",), ("lease", "status"), ("help", "lint"), ("help", "lease")):
        result = fd.run(*arguments)
        assert result.returncode == 2 and result.stdout == b"", arguments
        assert result.err == usage, arguments
