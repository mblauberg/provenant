#!/usr/bin/env python3
"""SessionStart(compact) hook: re-inject this session's own STATE.md after compaction.

The state file is the last STATE.md the session wrote, found from the hook's
`transcript_path`; a file it only read never counts. Prints nothing when the
session never wrote one. Always exits 0 so it cannot block a session.
"""

from __future__ import annotations

import json
import os
import re
import sys
from typing import Iterator


NAME = "STATE.md"
MAX_CHARS = 8000
CHUNK = 4 << 20
WRITE_TOOLS = ("Write", "Edit", "MultiEdit")

PATH = re.compile(
    r"(?:(?<=[\s\"'=(:])|^)((?:(?:~|\.{1,2}|[\w.-]+)?/(?:[^\s\"'`;|&<>()]*/)?)?"
    + re.escape(NAME)
    + r")(?![\w.-])"
)
CD = re.compile(r"\bcd\s+(\"[^\"]+\"|'[^']+'|[^\s;&|]+)")
# A path is written when it follows a redirect or tee, or ends a cp/mv/install command.
REDIRECT = re.compile(r"(?:>>?|\btee(?:\s+-\w+)*)\s*[\"']?$")
COPY = re.compile(r"^\s*(?:cp|mv|install)\s")
SEPARATOR = re.compile(r"[;\n]|&&|\|\|?")
# Commands that rewrite a named file in place.
IN_PLACE = re.compile(r"sed\s+-i|\.write\(|\.write_text\(|open\([^)]*['\"][wa]")


def reverse_lines(path: str) -> Iterator[bytes]:
    """Yield the file's lines last to first, reading it in chunks."""
    with open(path, "rb") as handle:
        handle.seek(0, os.SEEK_END)
        position, tail = handle.tell(), b""
        while position > 0:
            step = min(CHUNK, position)
            position -= step
            handle.seek(position)
            lines = (handle.read(step) + tail).split(b"\n")
            tail = lines.pop(0)
            yield from reversed(lines)
        if tail:
            yield tail


def resolve(candidate: str, bases: list[str]) -> str | None:
    """Existing file for candidate; relative paths try each base and its parents."""
    expanded = os.path.expanduser(candidate)
    if os.path.isabs(expanded):
        return expanded if os.path.isfile(expanded) else None
    for base in bases:
        directory = base
        while directory:
            found = os.path.abspath(os.path.join(directory, expanded))
            if os.path.isfile(found):
                return found
            parent = os.path.dirname(directory)
            if parent == directory:
                break
            directory = parent
    return None


def is_written(command: str, match: re.Match) -> bool:
    if REDIRECT.search(command[: match.start()]):
        return True
    starts = [m.end() for m in SEPARATOR.finditer(command, 0, match.start())]
    segment_start = starts[-1] if starts else 0
    after = SEPARATOR.search(command, match.end())
    rest = command[match.end() : after.start() if after else len(command)]
    return bool(COPY.match(command[segment_start : match.start()])) and not rest.strip(" \"'")


def references(block: dict, cwd: str) -> Iterator[tuple[str, bool]]:
    """Yield (absolute path, is_write) for each STATE.md one tool_use block names, last first."""
    tool, params = block.get("name"), block.get("input") or {}
    if tool in (*WRITE_TOOLS, "Read"):
        target = params.get("file_path") or ""
        if os.path.basename(target) == NAME:
            found = resolve(target, [cwd])
            if found:
                yield found, tool in WRITE_TOOLS
        return
    command = params.get("command") or ""
    if tool != "Bash" or NAME not in command:
        return
    bases, directory = [cwd], cwd
    for match in CD.findall(command):
        directory = os.path.join(directory, os.path.expanduser(match.strip("\"'")))
        bases.insert(0, directory)
    in_place = bool(IN_PLACE.search(command))
    for match in reversed(list(PATH.finditer(command))):
        found = resolve(match.group(1), bases)
        if found:
            yield found, in_place or is_written(command, match)


def failed_ids(entry: dict) -> Iterator[str]:
    message = entry.get("message")
    content = message.get("content") if isinstance(message, dict) else None
    for block in content if isinstance(content, list) else []:
        if isinstance(block, dict) and block.get("type") == "tool_result" and block.get("is_error"):
            yield str(block.get("tool_use_id"))


def last_written_state(transcript: str) -> str | None:
    failed: set[str] = set()
    for raw in reverse_lines(transcript):
        has_error = b'"is_error":true' in raw or b'"is_error": true' in raw
        if not has_error and (NAME.encode() not in raw or b'"tool_use"' not in raw):
            continue
        try:
            entry = json.loads(raw)
        except ValueError:
            continue
        if not isinstance(entry, dict):
            continue
        if has_error:
            failed.update(failed_ids(entry))
        message = entry.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            continue
        cwd = entry.get("cwd") if isinstance(entry.get("cwd"), str) else ""
        for block in reversed(content):
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            if str(block.get("id")) in failed:
                continue
            for path, is_write in references(block, cwd):
                if is_write:
                    return path
    return None


def render(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as handle:
        text = handle.read()
    if len(text) > MAX_CHARS:
        text = text[:MAX_CHARS] + f"\n[... truncated; read the rest from {path}]"
    return (
        f"Context was just compacted. This session's state file is {path} "
        f"(re-injected below). Its standing directions still bind; keep "
        f"maintaining this file.\n\n{text}"
    )


def main() -> int:
    try:
        payload = json.load(sys.stdin)
        transcript = payload.get("transcript_path") if isinstance(payload, dict) else None
        if isinstance(transcript, str) and os.path.isfile(transcript):
            path = last_written_state(transcript)
            if path:
                print(render(path))
    except Exception:  # a hook must never fail the session
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
