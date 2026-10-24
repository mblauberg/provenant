#!/usr/bin/env python3
"""Read-only provider-registration diagnostics used by the Fabric host query."""
from __future__ import annotations
import os
import json
from pathlib import Path
import shutil
import tomllib
from lib.jsonc import parse_jsonc

def provider_registration_status(home: Path) -> dict[str, dict[str, bool]]:
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
    result = {}
    for provider, (root, config) in locations.items():
        present = root.is_dir() and not (
            provider == "agy" and not os.environ.get("AGY_CONFIG_DIR") and shutil.which("agy") is None
        )
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
        result[provider] = {"present": present, "skills": skills, "registered": mcp}
    return result
