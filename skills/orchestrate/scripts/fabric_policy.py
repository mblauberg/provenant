"""Lenient reads of a project's `.agents/fabric-policy.json` lane settings.

A malformed setting falls back to its default with a warning rather than refusing the dispatch.
Protected paths and memory floors keep their own strict readers.
"""

from __future__ import annotations

import json
from pathlib import Path
import re

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


TOOLS_DEFAULTS = {
    "default_branch": "main",
    "lint": {
        "areas": [],
        "forbid_attribution": True,
        "forbid_closing_keywords": True,
    },
}


def tools_policy(workspace_root) -> tuple[dict, list[str]]:
    """The `tools` object with defaults filled in, for `provenant lint` and later verbs.

    Unknown keys are kept and ignored. A key of the wrong type falls back to its default with a warning.
    """
    policy, warnings = _load(workspace_root)
    configured = policy.get("tools", {})
    if not isinstance(configured, dict):
        warnings.append(f"{POLICY} tools must be an object; using defaults")
        configured = {}
    tools = {**configured}
    default_branch = configured.get("default_branch", TOOLS_DEFAULTS["default_branch"])
    if not isinstance(default_branch, str) or not default_branch:
        warnings.append(f"{POLICY} tools.default_branch must be a non-empty string; using main")
        default_branch = TOOLS_DEFAULTS["default_branch"]
    tools["default_branch"] = default_branch
    configured_lint = configured.get("lint", {})
    if not isinstance(configured_lint, dict):
        warnings.append(f"{POLICY} tools.lint must be an object; using defaults")
        configured_lint = {}
    lint = {**configured_lint}
    areas = configured_lint.get("areas", [])
    if not isinstance(areas, list) or not all(isinstance(a, str) and re.fullmatch(r"[a-z][a-z0-9]*", a) for a in areas):
        warnings.append(f"{POLICY} tools.lint.areas must be a list of lowercase words; allowing any area")
        areas = []
    lint["areas"] = areas
    for key in ("forbid_attribution", "forbid_closing_keywords"):
        value = configured_lint.get(key, True)
        if not isinstance(value, bool):
            warnings.append(f"{POLICY} tools.lint.{key} must be true or false; using true")
            value = True
        lint[key] = value
    tools["lint"] = lint
    return tools, warnings
