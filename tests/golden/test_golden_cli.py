"""Contract and normalisation tests for language-independent CLI cases."""

import os
from pathlib import Path
import socket
import json
import subprocess
import sys

import pytest

from tests.support.modules import load_script

# Register as `run` so in-test `import run` and diff.py share one module.
run = sys.modules.get("run") or load_script(Path(__file__).with_name("run.py"), "run", register=True)
from run import compare, normalize  # noqa: E402


def test_normalize_preserves_boundaries_and_shapes(monkeypatch):
    monkeypatch.setattr(socket, "gethostname", lambda: "cat")
    assert normalize("cat concatenate /cat/file cat.local x.local/path foo.bar.local foo.bar.local/path", []) == (
        "<HOST> concatenate /cat/file <HOST> x.local/path <HOST> foo.bar.local/path"
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
    binary = run.suite_binary()
    if binary is None or not binary.is_file():
        reason = "end-to-end golden tests need PROVENANT_GOLDEN_BIN (CI's serial Rust front door job sets it)"
        if os.environ.get("PROVENANT_GOLDEN_REQUIRED") == "1":
            pytest.fail(reason)
        pytest.skip(reason)
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
    assert comparison_fields(result("usage: provenant <root|help|route>"))["help"]["subcommands"] == []
    parsed = comparison_fields(result("Commands\n  clean|reap ... scripts/clean.py|reap.py\n"))["help"]
    assert parsed["subcommands"] == ["clean", "reap"]
    parsed = comparison_fields(result("fabric <command>\n  dispatch --route junior|senior|principal\n"))["help"]
    assert parsed["subcommands"] == ["dispatch"]
    parsed = comparison_fields(result("usage: tool [--mode {fast,slow}] {create,list}\n"))["help"]
    assert parsed["subcommands"] == ["create", "list"]
    assert comparison_fields(result("usage: provenant <command> [arguments...]"))["help"]["subcommands"] == []


@pytest.mark.parametrize(("text", "commands"), [
    ("usage: tool [-h] file question", []),
    ("usage: utility.py digest [-h] file question", []),
    ("  use --force to overwrite\n  see <file> for more", []),
    ("Commands:\n  run     Run a task\n  status  Show status\n\nArguments:\n  file    Input file\n  question  Question", ["run", "status"]),
    ("Commands\n  graph run --file F  start\n  graph status [ID]\n  graph cancel ID\n", ["cancel", "run", "status"]),
    ("usage: tool [--mode {fast,slow}] {create,list}\n\noptions:\n  --mode {fast,slow}", ["create", "list"]),
    ("usage: tool {hosts} {list,doctor} [hosts ...]", ["doctor", "list"]),
])
def test_help_structure_command_boundaries(text, commands):
    from run import help_structure
    assert help_structure(text, verb="graph" if "graph run" in text else "hosts")["subcommands"] == commands




def test_manifest_pins_only_the_python_minor(monkeypatch, tmp_path):
    import run
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"python": "3.14"}))
    monkeypatch.setattr(run, "MANIFEST", manifest)
    monkeypatch.setattr(run.sys, "version_info", type("Version", (), {"major": 3, "minor": 14}))
    monkeypatch.setattr(run.sqlite3, "sqlite_version", "4.0.0")
    run.check_manifest()
    monkeypatch.setattr(run.sys, "version_info", type("Version", (), {"major": 3, "minor": 13}))
    with pytest.raises(ValueError, match="runtime mismatch"):
        run.check_manifest()


def test_help_only_compare_does_not_check_runtime(monkeypatch, tmp_path):
    import run
    case = run.Case("help-case-01", "help", ["help"], "help")
    data = {"argv": ["help"], "stdin": "", "class": "help", "exit_code": 0,
            "stdout": "Commands\n  help  show help\n", "stderr": "", "files": []}
    (tmp_path / "help").mkdir()
    (tmp_path / "help/help-case-01.json").write_text(json.dumps(data))
    monkeypatch.setattr(run, "CASES", tmp_path)
    monkeypatch.setattr(run, "all_cases", lambda: [case])
    monkeypatch.setattr(run, "run_case", lambda *args: data)
    def mismatch():
        raise ValueError("runtime mismatch")
    monkeypatch.setattr(run, "check_manifest", mismatch)
    assert run.compare("unused") == 0
    monkeypatch.setattr(run, "all_cases", lambda: [run.Case("root-case-01", "root", ["root"], "machine")])
    assert run.compare("unused") == 1


def test_unknown_record_verb():
    import run
    result = subprocess.run([sys.executable, str(run.HERE / "run.py"), "--record", "--verb", "typo"],
                            capture_output=True, text=True)
    assert result.returncode == 2
    assert "invalid choice" in result.stderr and "recorded" not in result.stdout
    with pytest.raises(ValueError, match="unknown verb"):
        run.record("unused", "typo")


@pytest.mark.parametrize("name", ["PYTHONPATH", "BASH_ENV", "LD_PRELOAD", "GIT_DIR", "GH_CONFIG_DIR", "AGENT_FABRIC_PRODUCT_ROOT"])
def test_selector_env_allowlist(name):
    from run import Case, run_case
    with pytest.raises(ValueError, match="selector"):
        run_case("/does-not-run", Case("test", "root", ["root"], "machine"), {name: "outside"})




def test_checkout_guard_includes_ignored_and_agent_run(monkeypatch, tmp_path):
    import run
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    (tmp_path / ".gitignore").write_text("ignored/\n.agent-run/\nreports/\n")
    monkeypatch.setattr(run, "REPO", tmp_path)
    before = run.checkout_status()
    for folder in ("ignored", ".agent-run"):
        (tmp_path / folder).mkdir()
        (tmp_path / folder / "child-write").write_text("unexpected")
        after = run.checkout_status()
        assert after != before
        before = after
    output = tmp_path / "reports"
    output.mkdir()
    before = run.checkout_status()
    (tmp_path / "ignored/child-write").write_text("changed existing ignored file")
    assert run.checkout_status() != before
    baseline = run.checkout_status((output,))
    (output / "summary.json").write_text("{}")
    assert run.checkout_status((output,)) == baseline


@pytest.mark.parametrize("name", ["bash", "dirname"])
def test_missing_fixture_tools_fail_loudly(monkeypatch, name):
    import run
    which = run.shutil.which
    monkeypatch.setattr(run.shutil, "which", lambda tool: None if tool == name else which(tool))
    with pytest.raises(RuntimeError, match=f"require {name}"):
        run.run_case("/does-not-run", run.Case("test", "root", ["root"], "machine"))


def test_partial_help_record_preserves_machine_runtime(monkeypatch, tmp_path):
    import run
    manifest = tmp_path / "manifest.json"
    original = '{"python": "0.0", "sqlite": "0.0"}\n'
    manifest.write_text(original)
    monkeypatch.setattr(run, "MANIFEST", manifest)
    monkeypatch.setattr(run, "CASES", tmp_path / "cases")
    case = run.Case("route-resolve", "route", ["route", "resolve", "--help"], "help")
    monkeypatch.setattr(run, "all_cases", lambda: [case])
    monkeypatch.setattr(run, "run_case", lambda *args: {"exit_code": 0})
    run.record("unused", "route")
    assert manifest.read_text() == original
