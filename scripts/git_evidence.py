"""Run read-only Git evidence commands without inherited routing state."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
from typing import Mapping


def sanitized_git_environment(
    source: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Build the closed child environment used only for Git evidence reads."""
    inherited = os.environ if source is None else source
    environment = {
        name: inherited[name]
        for name in ("PATH", "HOME", "TMPDIR")
        if name in inherited
    }
    environment.update({
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_NO_REPLACE_OBJECTS": "1",
        "GIT_GRAFT_FILE": os.devnull,
        "GIT_NO_LAZY_FETCH": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_TERMINAL_PROMPT": "0",
    })
    return environment


def git_output(
    repository: Path, *args: str, text: bool = True,
    source_environment: Mapping[str, str] | None = None,
) -> str | bytes:
    """Return one fail-closed Git evidence read from the named repository."""
    return subprocess.run(
        ["git", "-C", str(repository), *args],
        check=True,
        capture_output=True,
        text=text,
        env=sanitized_git_environment(source_environment),
    ).stdout
