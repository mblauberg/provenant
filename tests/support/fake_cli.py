"""Fake provider executables placed first on PATH."""

from __future__ import annotations

import inspect
import json
import stat
from collections.abc import Sequence
from pathlib import Path


def write_executable(path: Path, body: str) -> Path:
    """Write a cleaned-up script body and make it user-executable."""
    text = inspect.cleandoc(body)
    if text.lstrip().startswith("#!"):
        assert text.startswith("#!"), "executable fixture shebang must start at byte zero"
    path.write_text(text, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def fake_bin(root: Path) -> Path:
    """Create and return ``root/bin``."""
    bin_dir = root / "bin"
    bin_dir.mkdir()
    return bin_dir


def fake_codex(
    bin_dir: Path,
    body: str = "cat >/dev/null\nprintf 'OK\\n'\n",
    *,
    models: Sequence[str] = ("gpt-6-luna",),
    efforts: Sequence[str] = ("high",),
    mcp: bool = True,
) -> Path:
    """Write ``bin_dir/codex``: the `mcp` and `debug models` probes, then ``body``."""
    catalogue = json.dumps(
        {
            "models": [
                {
                    "slug": model,
                    "supported_reasoning_levels": [{"effort": effort} for effort in efforts],
                }
                for model in models
            ]
        },
        separators=(",", ":"),
    )
    probes = ['#!/usr/bin/env bash']
    if mcp:
        probes.append('if [ "$1" = "mcp" ]; then printf \'[]\'; exit 0; fi')
    probes.append(
        'if [ "$1" = "debug" ] && [ "$2" = "models" ]; then\n'
        f"  printf '%s' '{catalogue}'\n"
        "  exit 0\n"
        "fi"
    )
    return write_executable(bin_dir / "codex", "\n".join(probes) + "\n" + inspect.cleandoc(body) + "\n")
