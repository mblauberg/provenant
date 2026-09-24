#!/usr/bin/env python3
"""Check compact chair-session state files."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys
import time


MAX_BYTES = 6144
INACTIVE_AFTER_SECONDS = 24 * 60 * 60
REQUIRED_HEADINGS = (
    "Goal and authority",
    "Stage and blockers",
    "Active lanes",
    "Queue",
    "Last checkpoint",
    "Next actions",
    "Links",
)
HEADING = re.compile(r"^##[ \t]+(.+?)\s*#*\s*$")
LIST_ITEM = re.compile(r"^(?:[-*+]|\d+[.)])\s+")


def project_root(cwd: Path) -> Path:
    """Nearest directory at or above cwd that holds session state, else cwd.

    Walking up reaches the primary project from a linked worktree or a nested
    repository, where the chair's `.agent-run/` usually lives.
    """
    for candidate in (cwd, *cwd.parents):
        if (candidate / ".agent-run" / "sessions").is_dir():
            return candidate
    return cwd


STATE_ENV = "PROVENANT_SESSION_STATE"


def hook_input(cwd: Path) -> tuple[Path, str]:
    """Working directory and host session id from PreCompact hook JSON, if any."""
    if sys.stdin.isatty():
        return cwd, ""
    try:
        payload = json.loads(sys.stdin.read())
    except (OSError, ValueError):
        return cwd, ""
    if not isinstance(payload, dict):
        return cwd, ""
    if isinstance(payload.get("cwd"), str) and payload["cwd"]:
        candidate = Path(payload["cwd"])
        cwd = (candidate if candidate.is_absolute() else cwd / candidate).resolve()
    session_id = payload.get("session_id")
    return cwd, session_id if isinstance(session_id, str) else ""


def names_session(path: Path, session_id: str) -> bool:
    if path.parent.name == session_id:
        return True
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return False
    line = re.compile(rf"^Chair session:[ \t]*{re.escape(session_id)}[ \t]*$", re.M)
    return line.search(text) is not None


def hook_entry() -> dict:
    command = f'python3 "{Path(__file__).resolve()}" --hook'
    return {"hooks": [{"type": "command", "command": command, "timeout": 5}]}


def hook_config() -> str:
    return json.dumps({"hooks": {"PreCompact": [hook_entry()]}}, indent=2)


def install_hook(settings: Path) -> str:
    """Add the PreCompact entry to a Claude Code settings file once; keep everything else."""
    settings = settings.resolve()  # write through a dotfile symlink, not over it
    data = json.loads(settings.read_text(encoding="utf-8")) if settings.exists() else {}
    if not isinstance(data, dict):
        raise ValueError(f"{settings} is not a JSON object")
    hooks = data.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        raise ValueError(f"{settings} has an unexpected hooks shape")
    entries = hooks.setdefault("PreCompact", [])
    if not isinstance(entries, list):
        raise ValueError(f"{settings} has an unexpected hooks shape")
    for entry in entries:
        inner = entry.get("hooks") if isinstance(entry, dict) else None
        for hook in inner if isinstance(inner, list) else []:
            if isinstance(hook, dict) and "state_check.py" in str(hook.get("command", "")):
                return f"already installed in {settings}"
    entries.append(hook_entry())
    settings.parent.mkdir(parents=True, exist_ok=True)
    temporary = settings.with_name(settings.name + ".state-check.tmp")
    temporary.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    os.replace(temporary, settings)
    return f"installed PreCompact hook in {settings}"


def recently_modified(path: Path, now: float) -> bool:
    try:
        return now - path.stat().st_mtime <= INACTIVE_AFTER_SECONDS
    except OSError:
        return False


def display_path(path: Path, root: Path, cwd: Path) -> str:
    for base in (root, cwd):
        try:
            return path.resolve().relative_to(base.resolve()).as_posix()
        except ValueError:
            continue
    return path.name


def check_file(path: Path, relative: str, *, max_age_minutes: float, now: float) -> list[str]:
    try:
        info = path.stat()
        content = path.read_bytes()
    except OSError as exc:
        message = exc.strerror or "I/O error"
        return [
            f"state_check: {relative}: cannot read file ({message}); make the state file readable"
        ]

    findings: list[str] = []
    if len(content) > MAX_BYTES:
        findings.append(
            f"state_check: {relative}: {len(content)} bytes exceeds {MAX_BYTES}; trim the state file below 6 KiB"
        )

    text = content.decode("utf-8", errors="replace")
    headings: list[tuple[str, int]] = []
    for index, line in enumerate(text.splitlines()):
        match = HEADING.match(line)
        if match:
            headings.append((match.group(1).strip(), index))

    for required in REQUIRED_HEADINGS:
        if not any(title.casefold().startswith(required.casefold()) for title, _ in headings):
            findings.append(
                f"state_check: {relative}: missing level-2 heading '{required}'; add the required heading"
            )

    next_heading = next(
        (
            (title, index)
            for title, index in headings
            if title.casefold().startswith("next actions")
        ),
        None,
    )
    if next_heading:
        _, start = next_heading
        end = next((index for _title, index in headings if index > start), len(text.splitlines()))
        lines = text.splitlines()
        item_count = sum(1 for line in lines[start + 1:end] if LIST_ITEM.match(line))
        if not 1 <= item_count <= 3:
            findings.append(
                f"state_check: {relative}: Next actions has {item_count} list items (expected 1 to 3); keep 1 to 3 next actions"
            )

    age_minutes = (now - info.st_mtime) / 60
    if age_minutes > max_age_minutes:
        findings.append(
            f"state_check: {relative}: file is older than {max_age_minutes:g} minutes; refresh the checkpoint"
        )
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", type=Path, help="state files to check")
    parser.add_argument("--max-age-minutes", type=float, default=30)
    parser.add_argument("--hook", action="store_true", help="run as a non-blocking PreCompact hook")
    parser.add_argument("--hook-config", action="store_true",
                        help="print the Claude Code settings fragment that registers the hook")
    parser.add_argument("--install-hook", nargs="?", const=Path.home() / ".claude" / "settings.json",
                        type=Path, metavar="SETTINGS",
                        help="add the PreCompact hook to a Claude Code settings file (default ~/.claude/settings.json)")
    args = parser.parse_args(argv)
    if args.hook_config:
        print(hook_config())
        return 0
    if args.install_hook:
        try:
            print(install_hook(args.install_hook.expanduser()))
        except (OSError, ValueError) as exc:
            print(f"state_check: cannot install hook: {exc}", file=sys.stderr)
            return 1
        return 0

    cwd = Path.cwd().resolve()
    session_id = ""
    if args.hook:
        cwd, session_id = hook_input(cwd)
    root = project_root(cwd)
    now = time.time()
    # Several chairs can share a project, so check only a state file this session
    # identifies: explicit paths, then the environment, then the host session id.
    paths = [path if path.is_absolute() else cwd / path for path in args.paths]
    if not paths and os.environ.get(STATE_ENV):
        named = Path(os.environ[STATE_ENV]).expanduser()
        paths = [named if named.is_absolute() else cwd / named]
    if not paths and session_id:
        # A session untouched for a day is finished, not a live chair to warn about.
        paths = [
            path
            for path in sorted((root / ".agent-run" / "sessions").glob("*/STATE.md"))
            if recently_modified(path, now) and names_session(path, session_id)
        ]

    findings = []
    for path in paths:
        findings.extend(
            check_file(
                path,
                display_path(path, root, cwd),
                max_age_minutes=args.max_age_minutes,
                now=now,
            )
        )
    prefix = "Checkpoint check: " if args.hook else ""
    for finding in findings:
        print(prefix + finding)
    return 0 if args.hook or not findings else 1


if __name__ == "__main__":
    raise SystemExit(main())
