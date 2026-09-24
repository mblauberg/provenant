#!/usr/bin/env python3
"""Claude Code PreToolUse template: suggest the declared tracker command."""

import json
from pathlib import Path
import re
import shlex
import sys


def raw_tracker_write(command: str) -> bool:
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|")
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return False
    segment: list[str] = []
    for token in [*tokens, ";"]:
        if token in {";", "&&", "||", "|", "&"}:
            parts = segment[:]
            segment.clear()
            if parts[:1] == ["env"]:
                parts.pop(0)
            while parts and re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*=.*", parts[0]):
                parts.pop(0)
            if parts[:1] != ["gh"]:
                continue
            parts.pop(0)
            while parts and parts[0] in {"-R", "--repo", "--hostname"} and len(parts) > 1:
                del parts[:2]
            if parts[:1] == ["issue"] and parts[1:2] in (["create"], ["edit"], ["close"]):
                return True
            if parts[:1] == ["project"] and parts[1:2] in (["item-add"], ["item-archive"], ["item-create"], ["item-edit"], ["item-delete"]):
                return True
        else:
            segment.append(token)
    return False


def tracker_command(repo: Path) -> str | None:
    try:
        content = (repo / "MAINTAINING.md").read_text(encoding="utf-8")
    except OSError:
        return None
    section = content.split("### Tracker\n", 1)
    if len(section) != 2:
        return None
    section = section[1].split("\n### ", 1)[0]
    match = re.search(r"(?m)^- Command: `([^`]+)`\s*$", section)
    return match[1] if match and match[1] != "none" else None


def main() -> None:
    event = json.load(sys.stdin)
    if event.get("tool_name") != "Bash" or not raw_tracker_write(event.get("tool_input", {}).get("command", "")):
        return
    command = tracker_command(Path(__file__).resolve().parents[2])
    if command:
        print(json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": f"Use the repository tracker command: {command}",
        }}))


if __name__ == "__main__":
    main()
