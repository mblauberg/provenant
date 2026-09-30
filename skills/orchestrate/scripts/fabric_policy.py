"""Lenient reads of a project's `.agents/fabric-policy.json` lane settings.

A malformed setting falls back to its default with a warning rather than refusing the dispatch.
Protected paths and memory floors keep their own strict readers.
"""

from __future__ import annotations

import json
from pathlib import Path

POLICY = ".agents/fabric-policy.json"
INSTRUCTION_MODES = ("quarantine", "allow", "deny")


def _load(workspace_root) -> tuple[dict, list[str]]:
    path = Path(workspace_root) / POLICY
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}, []
    except (OSError, UnicodeError, ValueError):
        return {}, [f"{POLICY} is unreadable; using defaults"]
    return (policy, []) if isinstance(policy, dict) else ({}, [f"{POLICY} is not an object; using defaults"])


def instruction_changes(workspace_root) -> tuple[str, list[str]]:
    """What a Codex writer's authored `.agents/` changes get: quarantine (default), allow or deny."""
    policy, warnings = _load(workspace_root)
    value = policy.get("instruction_changes", "quarantine")
    if value not in INSTRUCTION_MODES:
        warnings.append(f"{POLICY} instruction_changes must be quarantine, allow or deny; using quarantine")
        value = "quarantine"
    return value, warnings


def secret_scan_exclude(workspace_root) -> tuple[list[Path], list[str]]:
    """Directories, relative to the workspace root, the dispatch secret scan skips."""
    policy, warnings = _load(workspace_root)
    entries = policy.get("secret_scan_exclude", [])
    if not isinstance(entries, list):
        warnings.append(f"{POLICY} secret_scan_exclude must be a list; ignoring it")
        return [], warnings
    root = Path(workspace_root).resolve()
    excluded = []
    for entry in entries:
        # Canonical containment, so neither `..` nor a link can exclude a directory outside the project.
        resolved = (root / entry).resolve() if isinstance(entry, str) and entry and not Path(entry).is_absolute() else None
        if resolved is not None and resolved.is_relative_to(root):
            excluded.append(resolved)
        else:
            warnings.append(f"{POLICY} secret_scan_exclude takes relative paths inside the project; ignoring {entry!r}")
    return excluded, warnings
