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

    ``env`` is merged over the inherited environment; ``path`` entries are
    prepended to ``PATH``.
    """
    merged = {**os.environ, **(env or {})}
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
    bin_dir: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict | None]:
    """Run cf_dispatch.py with a Fabric-free environment.

    Returns the process and its parsed stdout record (None when stdout is not
    one JSON object). Asserts nothing.
    """
    merged = {**fabric_free_env(), **(env or {})}
    result = run_cli(
        [SCRIPT, *args], cwd=cwd, env=merged, path=[bin_dir] if bin_dir else ()
    )
    try:
        record = json.loads(result.stdout)
    except ValueError:
        return result, None
    return result, record if isinstance(record, dict) else None
