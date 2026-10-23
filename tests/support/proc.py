"""Run commands and the cf_dispatch front door."""

from __future__ import annotations

import json
import os
import subprocess
from collections.abc import Mapping, Sequence
from pathlib import Path

from tests.cf_dispatch_support import SCRIPT, fabric_free_env


def run_cli(
    argv: Sequence[str | Path],
    *,
    cwd: Path,
    env: Mapping[str, str] | None = None,
    input: str | None = None,
    path: Sequence[Path] = (),
    timeout: float = 120,
) -> subprocess.CompletedProcess[str]:
    """Run ``argv`` capturing text output, never raising on a non-zero exit.

    ``env`` replaces the environment when given (subprocess semantics) and is
    inherited when None; ``path`` entries are prepended to its ``PATH``.
    """
    merged = dict(os.environ if env is None else env)
    if path:
        merged["PATH"] = os.pathsep.join([*map(str, path), merged.get("PATH", "")])
    return subprocess.run(
        [str(part) for part in argv],
        cwd=cwd,
        env=merged,
        input=input,
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout,
    )


def run_cf_dispatch(
    cwd: Path,
    *args: str,
    path: Sequence[Path] = (),
    env: Mapping[str, str] | None = None,
    timeout: float = 120,
) -> tuple[subprocess.CompletedProcess[str], dict | None]:
    """Run cf_dispatch.py with a Fabric-free environment.

    ``env`` is overlaid on the Fabric-free environment. Returns the process
    and its parsed stdout record, which is None when stdout is not one JSON
    object (callers assert with ``result.stderr``). Asserts nothing.
    """
    merged = {**fabric_free_env(), **(env or {})}
    result = run_cli([SCRIPT, *args], cwd=cwd, env=merged, path=path, timeout=timeout)
    try:
        record = json.loads(result.stdout)
    except ValueError:
        return result, None
    return result, record if isinstance(record, dict) else None


def process_state(pid):
    result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                            capture_output=True, text=True, timeout=5)
    assert result.returncode in {0, 1} and not result.stderr, result.stderr
    return result.stdout.strip()


def process_stopped(pid):
    state = process_state(pid)
    return not state or "Z" in state


def process_group_pids(pgid):
    # The headless host's ps supports a full table but not -g; filter the
    # retained group explicitly, including any surviving descendants.
    result = subprocess.run(["ps", "-A", "-o", "pgid=,pid="],
                            capture_output=True, text=True, timeout=5)
    assert result.returncode == 0 and not result.stderr, result.stderr
    return [int(pid) for group, pid in (line.split() for line in result.stdout.splitlines())
            if int(group) == pgid]
