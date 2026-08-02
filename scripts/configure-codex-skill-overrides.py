"""Install the harness-owned Codex skill conflict override safely."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import sys
import tempfile
import tomllib


SKILL_NAME = "skill-creator"
BLOCK = """# agent-harness: disable overlapping bundled skill
[[skills.config]]
name = "skill-creator"
enabled = false
"""


class ConfigError(ValueError):
    pass


def _entries(text: str) -> list[dict[str, object]]:
    try:
        value = tomllib.loads(text) if text.strip() else {}
    except tomllib.TOMLDecodeError as exc:
        raise ConfigError(f"Codex config is invalid TOML: {exc}") from exc
    skills = value.get("skills", {})
    if not isinstance(skills, dict):
        raise ConfigError("Codex config skills value must be a table")
    config = skills.get("config", [])
    if not isinstance(config, list) or any(not isinstance(item, dict) for item in config):
        raise ConfigError("Codex config skills.config value must be an array of tables")
    return config


def state(text: str) -> str:
    matches = [item for item in _entries(text) if item.get("name") == SKILL_NAME]
    if not matches:
        return "missing"
    enabled = {item.get("enabled") for item in matches}
    if enabled == {False}:
        return "disabled"
    raise ConfigError("skill-creator has a conflicting enabled or malformed override")


def canonical_path(path: Path) -> Path:
    """Return the file to replace without destroying a caller-owned symlink."""
    try:
        if path.is_symlink():
            target = path.resolve(strict=True)
            if not target.is_file():
                raise ConfigError("Codex config symlink must target a regular file")
            return target
    except (OSError, RuntimeError) as exc:
        raise ConfigError(f"Codex config symlink cannot be resolved: {exc}") from exc
    return path


def proposed_update(path: Path) -> tuple[Path, str, str]:
    target = canonical_path(path)
    text = target.read_text() if target.exists() else ""
    current = state(text)
    if current == "disabled":
        return target, text, "existing"

    prefix = text
    if prefix and not prefix.endswith("\n"):
        prefix += "\n"
    if prefix:
        prefix += "\n"
    updated = prefix + BLOCK
    _entries(updated)
    if state(updated) != "disabled":
        raise ConfigError("composed Codex config does not disable skill-creator")
    return target, updated, "ready"


def preflight(path: Path) -> str:
    _, _, result = proposed_update(path)
    return result


def configure(path: Path) -> str:
    target, updated, result = proposed_update(path)
    if result == "existing":
        return result

    target.parent.mkdir(parents=True, exist_ok=True)

    temp: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w", dir=target.parent, prefix=".codex-config.", suffix=".tmp", delete=False,
        ) as handle:
            temp = Path(handle.name)
            handle.write(updated)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp, target)
    finally:
        if temp and temp.exists():
            temp.unlink()
    return "configured"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    default = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")) / "config.toml"
    parser.add_argument("--config", type=Path, default=default)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--preflight", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.check:
            target = canonical_path(args.config)
            result = state(target.read_text() if target.exists() else "")
            if result != "disabled":
                print(f"missing: {SKILL_NAME} disable override in {args.config}")
                return 1
            print(f"skill override verified={args.config}")
            return 0
        if args.preflight:
            result = preflight(args.config)
            print(f"skill override preflight {result}={args.config}")
            return 0
        result = configure(args.config)
    except (OSError, ConfigError) as exc:
        print(f"conflicting: {exc}", file=sys.stderr)
        return 3
    print(f"skill override {result}={args.config}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
