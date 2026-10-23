"""Real dispatch fixtures shared by owner and lane conformance tests."""

import json
import os
import subprocess
import sys
from pathlib import Path

from tests.support.fake_cli import write_executable
from tests.support.runs import INIT

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "skills/orchestrate/scripts/dispatch_run.py"


def owner_command():
    """Invoke the remaining Python dispatch owner directly."""
    return [sys.executable, str(SCRIPT)]


def disable_memory_admission_floor(workspace: Path) -> None:
    """Keep process contract tests independent of host memory probes."""
    policy = workspace / ".agents/fabric-policy.json"
    policy.parent.mkdir(parents=True, exist_ok=True)
    policy.write_text(json.dumps({"memory_floor_percent": {"read_only": 0, "worktree_write": 0}}))


def real_owner_fixture(tmp_path, monkeypatch, code):
    """Exercise cf_dispatch --plan-only and the production supervisor, no owner stub."""
    disable_memory_admission_floor(tmp_path)
    bindir = tmp_path / 'provider-bin'
    bindir.mkdir()
    write_executable(bindir / 'claude', '#!/usr/bin/env python3\n' + code)
    monkeypatch.setenv('PATH', str(bindir) + os.pathsep + os.environ['PATH'])
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / '.agents'))
    monkeypatch.setenv('FABRIC_COOLDOWNS_PATH', str(tmp_path / 'cooldowns.json'))
    monkeypatch.delenv('PROVENANT_RUN_TOKEN', raising=False)
    run = Path(subprocess.check_output([str(INIT), '--kind', 'dispatch'], cwd=tmp_path, text=True).strip())
    prompt = tmp_path / 'caller-prompt.md'
    prompt.write_text('hello')
    command = owner_command() + ['--run-dir', str(run), '--adapter', 'claude',
               '--model', 'opus', '--prompt-file', str(prompt), '--fallback', 'false']
    return run, prompt, command
