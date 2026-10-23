"""Contract and normalisation tests for language-independent CLI cases."""

import os
from pathlib import Path
import socket
import sys

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run import binary_path, compare, normalize  # noqa: E402


def test_normalize_preserves_boundaries_and_shapes(monkeypatch):
    monkeypatch.setattr(socket, "gethostname", lambda: "cat")
    assert normalize("cat concatenate /cat/file cat.local x.local/path", []) == (
        "<HOST> concatenate /cat/file <HOST> x.local/path"
    )
    assert normalize('rapid 12 pid 12 "pid": 42 pid=99', []) == (
        'rapid 12 pid 12 "pid": <PID> pid=<PID>'
    )
    assert normalize("run-20261023-123456-abc123", []) == normalize("run-20251022-223455-def456", [])
    assert normalize("run-20261023-123456-abc123", []) != normalize("run-20261023-123456-abcd123", [])
    assert normalize("sk-abcdefghijkl", []) != normalize("sk-abcdefghijklm", [])
    assert normalize("sk-abc1def2ghi3", []) == normalize("sk-ab12de34gh56", [])
    assert normalize("20261023-0606-dispatch-test-a1B2c3", []) == normalize("20261024-0707-dispatch-test-abCD12", [])
    assert normalize("20261023-0606-dispatch-test-a1B2c3", []) != normalize("20261023-0606-dispatch-test-a1B2c34", [])
    assert normalize("/private/var/tmp/x /var/tmp/x", [
        ("/private/var/tmp", "<FIXTURE>"), ("/var/tmp", "<FIXTURE>"),
    ]) == "<FIXTURE>/x <FIXTURE>/x"


def test_golden_cli_cases_match():
    binary = binary_path()
    if binary is None or not binary.is_file():
        reason = "no CLI binary found (set PROVENANT_GOLDEN_BIN or build tools/front-door)"
        if os.environ.get("PROVENANT_GOLDEN_ALLOW_SKIP") == "1" and os.environ.get("PROVENANT_GOLDEN_REQUIRED") != "1":
            pytest.skip(reason)
        pytest.fail(reason)
    assert compare(binary) == 0


def test_help_comparison_ignores_prose_but_detects_commands_and_flags():
    from run import comparison_fields

    def result(text):
        return {"class": "help", "exit_code": 0, "stdout": text, "stderr": "", "files": []}

    a = result("usage: tool {create,list} [-h] [--json]\noptions:\n  --json  old wording\n")
    b = result("usage: tool {list,create} [-h] [--json]\noptional arguments:\n  --json  new wording\n")
    assert comparison_fields(a) == comparison_fields(b)
    assert comparison_fields(a) != comparison_fields(result("usage: tool {create} [-h] [--json]"))
    assert comparison_fields(a) != comparison_fields(result("usage: tool {create,list} [-h]"))
    assert comparison_fields(result("usage: provenant <root|help|route>"))["help"]["subcommands"] == ["help", "root", "route"]
    parsed = comparison_fields(result("Commands\n  clean|reap ... scripts/clean.py|reap.py\n"))["help"]
    assert parsed["subcommands"] == ["clean", "reap"]
    parsed = comparison_fields(result("fabric <command>\n  dispatch --route junior|senior|principal\n"))["help"]
    assert parsed["subcommands"] == ["dispatch"]
    parsed = comparison_fields(result("usage: tool [--mode {fast,slow}] {create,list}\n"))["help"]
    assert parsed["subcommands"] == ["create", "list"]
    assert comparison_fields(result("usage: provenant <command> [arguments...]"))["help"]["subcommands"] == []
