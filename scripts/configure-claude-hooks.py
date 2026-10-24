#!/usr/bin/env python3
"""Add or remove managed lane-safety and utility-output hooks in Claude settings."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile

GUARD_NAME = "lane_safety_guard.py"


class ConfigError(ValueError):
    pass


def hook_command(guard: Path, interpreter: str = "python3", args: tuple[str, ...] = ()) -> str:
    return " ".join((shlex.quote(interpreter), shlex.quote(str(guard)), *(shlex.quote(arg) for arg in args)))


def _managed(entry: object, name: str = GUARD_NAME) -> bool:
    return isinstance(entry, dict) and any(
        isinstance(hook, dict) and name in str(hook.get("command", ""))
        for hook in entry.get("hooks", [])
        if isinstance(entry.get("hooks"), list)
    )


def merged(settings: dict, guard: Path | None, *, event="PreToolUse", name=GUARD_NAME, matcher="Bash",
           command_args: tuple[str, ...] = ()) -> dict:
    """Settings with the managed entry replaced by one for guard, or removed."""
    hooks = settings.get("hooks", {})
    if not isinstance(hooks, dict):
        raise ConfigError("settings 'hooks' must be an object")
    entries = hooks.get(event, [])
    if not isinstance(entries, list):
        raise ConfigError(f"settings hooks.{event} must be a list")
    entries = [entry for entry in entries if not _managed(entry, name)]
    if guard is not None:
        entries.append({
            **({"matcher": matcher} if matcher else {}),
            "hooks": [{"type": "command", "command": hook_command(guard, args=command_args)}],
        })
    result = {**settings, "hooks": {**hooks, event: entries}}
    if not entries:
        del result["hooks"][event]
        if not result["hooks"]:
            del result["hooks"]
    return result


def apply(settings_path: Path, guard: Path | None, *, event="PreToolUse", name=GUARD_NAME, matcher="Bash",
          command_args: tuple[str, ...] = ()) -> str:
    target = Path(os.path.realpath(settings_path))
    try:
        text = target.read_text() if target.exists() else ""
        settings = json.loads(text) if text.strip() else {}
    except (OSError, ValueError) as exc:
        raise ConfigError(f"cannot read settings {target}: {exc}") from exc
    if not isinstance(settings, dict):
        raise ConfigError(f"settings {target} must be a JSON object")
    updated = merged(settings, guard, event=event, name=name, matcher=matcher, command_args=command_args)
    if updated == settings:
        return "unchanged"
    target.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary = tempfile.mkstemp(dir=target.parent, prefix=".settings-")
    try:
        with os.fdopen(handle, "w") as stream:
            json.dump(updated, stream, indent=2)
            stream.write("\n")
        if target.exists():
            os.chmod(temporary, target.stat().st_mode & 0o777)
        os.replace(temporary, target)
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise
    return "installed" if guard is not None else "removed"


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--settings", required=True, type=Path)
    parser.add_argument("--guard", type=Path, help="path to lane_safety_guard.py")
    parser.add_argument("--remove", action="store_true")
    options = parser.parse_args(arguments)
    if options.remove == (options.guard is not None):
        parser.error("give exactly one of --guard and --remove")
    guard = None if options.remove else options.guard.resolve()
    try:
        state = apply(options.settings, guard)
    except ConfigError as exc:
        print(f"configure-claude-hooks: {exc}", file=sys.stderr)
        return 3
    print(f"lane-safety hook {state}={options.settings}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
