"""Contract for the shared helpers in tests/support."""

import os
import sys

from tests.support.fake_cli import fake_bin, write_executable
from tests.support.proc import run_cli


def test_run_cli_env_replaces_inherited_environment(tmp_path, monkeypatch):
    monkeypatch.setenv("AGENTS_HOME", "/leak")
    monkeypatch.setenv("AGENT_FABRIC_LABEL", "leak")
    code = "import os; print(sorted(k for k in os.environ if k in ('AGENTS_HOME', 'AGENT_FABRIC_LABEL')))"
    inherited = run_cli([sys.executable, "-c", code], cwd=tmp_path)
    assert "AGENTS_HOME" in inherited.stdout
    replaced = run_cli([sys.executable, "-c", code], cwd=tmp_path, env={"X": "1"})
    assert replaced.stdout.strip() == "[]"


def test_run_cli_path_entry_comes_first(tmp_path):
    bin_dir = fake_bin(tmp_path)
    write_executable(bin_dir / "probe", "#!/bin/sh\nprintf '%s' \"$PATH\"")
    result = run_cli(["probe"], cwd=tmp_path, path=[bin_dir])
    assert result.stdout.split(os.pathsep)[0] == str(bin_dir)


def test_run_cf_dispatch_gives_none_for_non_json_stdout(tmp_path, monkeypatch):
    import tests.support.proc as proc

    script = write_executable(tmp_path / "emit", "#!/bin/sh\necho not-json")
    monkeypatch.setattr(proc, "SCRIPT", script)
    result, record = proc.run_cf_dispatch(tmp_path)
    assert result.returncode == 0
    assert record is None


def test_run_cf_dispatch_parses_json_and_hides_fabric_variables(tmp_path, monkeypatch):
    import tests.support.proc as proc

    script = write_executable(
        tmp_path / "emit",
        f"""#!{sys.executable}
        import json, os
        print(json.dumps({{"home": os.environ.get("AGENTS_HOME"),
                          "label": os.environ.get("AGENT_FABRIC_LABEL")}}))
        """,
    )
    monkeypatch.setenv("AGENTS_HOME", "/leak")
    monkeypatch.setenv("AGENT_FABRIC_LABEL", "leak")
    monkeypatch.setattr(proc, "SCRIPT", script)
    result, record = proc.run_cf_dispatch(tmp_path)
    assert result.returncode == 0, result.stderr
    assert record == {"home": None, "label": None}


def test_process_group_pids_includes_current_process():
    from tests.support.proc import process_group_pids

    assert os.getpid() in process_group_pids(os.getpgrp())
