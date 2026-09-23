#!/usr/bin/env python3
"""Check the pointer-owned installed Provenant stub against its template."""

from __future__ import annotations

import os
import argparse
import json
from pathlib import Path
import shutil
import sys
import tomllib

from lib.jsonc import parse_jsonc
from lib.product_root_resolver import POINTER_RELATIVE_PATH, load_pointer_file
from instance_installation import InstallError, routing_drift


ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "scripts/provenant.template"


def _provider_lines(home: Path) -> tuple[list[str], bool]:
    claude_root = Path(os.environ.get("CLAUDE_CONFIG_DIR") or home / ".claude")
    codex_root = Path(os.environ.get("CODEX_HOME") or home / ".codex")
    opencode_root = Path(os.environ.get("OPENCODE_CONFIG_DIR") or home / ".config/opencode")
    agy_root = Path(os.environ.get("AGY_CONFIG_DIR") or home / ".gemini")
    locations = {
        "claude": (claude_root, Path(os.environ.get("CLAUDE_MCP_CONFIG") or home / ".claude.json")),
        "codex": (codex_root, Path(os.environ.get("CODEX_MCP_CONFIG") or codex_root / "config.toml")),
        "opencode": (opencode_root, Path(os.environ.get("OPENCODE_MCP_CONFIG") or opencode_root / "opencode.jsonc")),
        "agy": (agy_root, Path(os.environ.get("AGY_MCP_CONFIG") or agy_root / "config/mcp_config.json")),
        "cursor": (home / ".cursor", Path(os.environ.get("CURSOR_MCP_CONFIG") or home / ".cursor/mcp.json")),
        "kiro": (home / ".kiro", Path(os.environ.get("KIRO_MCP_CONFIG") or home / ".kiro/settings/mcp.json")),
    }
    lines = []
    missing = False
    for provider, (root, config) in locations.items():
        if not root.is_dir() or (
            provider == "agy" and not os.environ.get("AGY_CONFIG_DIR") and shutil.which("agy") is None
        ):
            lines.append(f"provider {provider} present=no")
            continue
        skills = (root / "skills/orchestrate/SKILL.md").is_file()
        try:
            content = config.read_text()
            if provider == "codex":
                document = tomllib.loads(content)
            elif provider == "opencode":
                document, _ = parse_jsonc(content)
            else:
                document = json.loads(content)
            key = "mcp_servers" if provider == "codex" else "mcp" if provider == "opencode" else "mcpServers"
            servers = document.get(key, {}) if isinstance(document, dict) else {}
            mcp = isinstance(servers, dict) and "fabric" in servers
        except (OSError, ValueError):
            mcp = False
        agents = (root / "agents").is_dir() if provider == "claude" else None
        complete = skills and mcp and agents is not False
        missing |= not complete
        lines.append(
            f"provider {provider} present=yes skills={'ok' if skills else 'missing'} "
            f"agents={'ok' if agents else 'missing' if agents is False else 'unsupported'} "
            f"mcp={'ok' if mcp else 'missing'}"
            + (" repair=install-harness --platform all" if not complete else "")
        )
    return lines, missing


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--strict", action="store_true", help="fail on routing or provider warnings")
    args = parser.parse_args()
    instance_value = os.environ.get("AGENT_FABRIC_INSTANCE_ROOT") or None
    instance_root = (
        Path(instance_value).expanduser()
        if instance_value is not None
        else Path.home() / ".agents"
    )
    if not instance_root.is_absolute():
        print("FAIL: Agent Fabric instance root must be absolute", file=sys.stderr)
        return 1
    pointed_product = load_pointer_file(instance_root)
    pointer = instance_root / POINTER_RELATIVE_PATH
    if (pointer.exists() or pointer.is_symlink()) and pointed_product is None:
        print(f"FAIL: product-root pointer is invalid or stale at {pointer}", file=sys.stderr)
        return 1
    if pointed_product is None or pointed_product.resolve() != ROOT.resolve():
        print("provenant installed stub=not-owned-by-this-checkout")
        return 0

    bin_value = os.environ.get("PROVENANT_BIN_DIR") or None
    bin_directory = (
        Path(bin_value).expanduser()
        if bin_value is not None
        else Path.home() / ".local/bin"
    )
    if not bin_directory.is_absolute():
        print("FAIL: PROVENANT_BIN_DIR must be absolute", file=sys.stderr)
        return 1
    command = bin_directory / "provenant"
    if command.is_symlink() or not command.is_file():
        print(
            f"FAIL: {command} must be a regular managed copy; re-run install-harness",
            file=sys.stderr,
        )
        return 1
    if not os.access(command, os.X_OK):
        print(
            f"FAIL: {command} is not executable; re-run install-harness",
            file=sys.stderr,
        )
        return 1
    if command.read_bytes() != TEMPLATE.read_bytes():
        print(
            "FAIL: installed stub differs from scripts/provenant.template; "
            "re-run install-harness",
            file=sys.stderr,
        )
        return 1
    print(f"provenant installed stub=ok path={command}")
    provider_lines, provider_missing = _provider_lines(Path.home())
    for line in provider_lines:
        print(line)
    try:
        differences = routing_drift(ROOT, instance_root)
    except InstallError as exc:
        print(f"warning: routing catalogue {exc}; repair=install-harness --platform all --refresh-routing", file=sys.stderr)
        differences = ["unreadable"]
    for key in differences:
        print(f"routing drift={key} repair=install-harness --platform all --refresh-routing", file=sys.stderr)
    if args.strict and (differences or provider_missing):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
