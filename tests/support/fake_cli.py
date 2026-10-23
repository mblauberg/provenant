"""Fake provider executables placed first on PATH."""

from __future__ import annotations

import inspect
import json
import os
import shlex
import stat
from collections.abc import Mapping, Sequence
from pathlib import Path


def write_executable(path: Path, body: str) -> Path:
    """Write a script body and make it user-executable.

    ``inspect.cleandoc`` expands tabs and dedents the first line separately
    from the rest, so a body may start on the quote line.
    """
    text = inspect.cleandoc(body) + "\n"
    if text.lstrip().startswith("#!"):
        assert text.startswith("#!"), "executable fixture shebang must start at byte zero"
    with path.open("w", encoding="utf-8") as executable:
        executable.write(text)
        executable.flush()
        os.fsync(executable.fileno())
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
    models: Mapping[str, Sequence[str]] | None = None,
    catalogue: object | None = None,
    mcp: bool = True,
) -> Path:
    """Write ``bin_dir/codex``: the `mcp` and `debug models` probes, then ``body``.

    ``models`` maps each slug to its efforts (default ``gpt-6-luna``: high).
    ``catalogue`` replaces the whole payload, for malformed catalogues.
    """
    if catalogue is None:
        catalogue = {
            "models": [
                {
                    "slug": model,
                    "supported_reasoning_levels": [{"effort": effort} for effort in efforts],
                }
                for model, efforts in (models or {"gpt-6-luna": ("high",)}).items()
            ]
        }
    payload = json.dumps(catalogue, separators=(",", ":"))
    probes = ['#!/usr/bin/env bash']
    if mcp:
        probes.append('if [ "$1" = "mcp" ]; then printf \'[]\'; exit 0; fi')
    probes.append(
        'if [ "$1" = "debug" ] && [ "$2" = "models" ]; then\n'
        f"  printf '%s' {shlex.quote(payload)}\n"
        "  exit 0\n"
        "fi"
    )
    return write_executable(bin_dir / "codex", "\n".join(probes) + "\n" + inspect.cleandoc(body))
