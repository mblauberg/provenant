#!/usr/bin/env python3
"""Source oracle for the session, setup-repo hook and tracker Rust units."""

from __future__ import annotations

from dataclasses import asdict
from datetime import datetime
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import hashlib
import importlib
import json
from pathlib import Path
import sys

ROOT = Path(sys.argv[1])
sys.path.insert(0, str(ROOT))

from skills.session.scripts import compact_reinject, context_audit
claude_tracker_hook = importlib.import_module("skills.setup-repo.templates.claude-tracker-hook")
from skills.tracker.scripts import steward


def compact(case: dict):
    op = case["op"]
    if op == "reverse":
        path = case["path"]
        rows = []
        for line in compact_reinject.reverse_lines(path):
            rows.append({"length": len(line), "digest": "sha256:" + hashlib.sha256(line).hexdigest()})
        return rows
    if op == "hook":
        payload = json.loads(case["input"])
        transcript = payload.get("transcript_path") if isinstance(payload, dict) else None
        if not isinstance(transcript, str) or not Path(transcript).is_file():
            return None
        try:
            path = compact_reinject.last_written_state(transcript)
            return compact_reinject.render(path) if path else None
        except Exception:
            return None
    raise ValueError(f"unknown compact operation: {op}")


def audit(case: dict):
    if case["op"] == "cli":
        stdout, stderr = StringIO(), StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = context_audit.main(case["argv"])
        return {"exit_code": code, "stdout": stdout.getvalue(), "stderr": stderr.getvalue()}
    now = datetime.fromisoformat(case["now"].replace("Z", "+00:00")) if "now" in case else None
    options = {key: value for key, value in case.items() if key.startswith("max_") or key.startswith("stale_")}
    findings = context_audit.audit(Path(case["root"]), now=now, **options)
    return [asdict(item) for item in findings]


def hook(case: dict):
    if case["op"] == "commands":
        return {
            "positive": [claude_tracker_hook.raw_tracker_write(command) for command in case["positive"]],
            "negative": [claude_tracker_hook.raw_tracker_write(command) for command in case["negative"]],
        }
    if case["op"] == "evaluate":
        event = case["event"]
        if event.get("tool_name") != "Bash" or not claude_tracker_hook.raw_tracker_write(
            event.get("tool_input", {}).get("command", "")
        ):
            return None
        command = claude_tracker_hook.tracker_command(Path(case["repo"]))
        if not command:
            return None
        return {"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": f"Use the repository tracker command: {command}",
        }}
    if case["op"] == "evaluate_wire":
        event = case["event"]
        if event.get("tool_name") != "Bash" or not claude_tracker_hook.raw_tracker_write(
            event.get("tool_input", {}).get("command", "")
        ):
            return None
        command = claude_tracker_hook.tracker_command(Path(case["repo"]))
        if not command:
            return None
        return json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": f"Use the repository tracker command: {command}",
        }}) + "\n"
    raise ValueError(f"unknown hook operation: {case['op']}")


def steward_case(case: dict):
    op = case["op"]
    path = Path(case["path"])
    if op == "load":
        events = steward.load_events(path)
        return {"events": events, "pending": steward.pending(events)}
    if op == "record":
        changed = steward.record_outcome(path, case["id"], case["outcome"])
        return {"changed": changed, "bytes": path.read_text(encoding="utf-8")}
    if op == "replay":
        processed = steward.replay(
            path,
            lambda event: {"status": "done", "summary": f"handled {event['id']}"},
        )
        return {"processed": processed, "bytes": path.read_text(encoding="utf-8")}
    if op == "digest":
        processed = [event for event in steward.load_events(path) if "outcome" in event]
        return steward.digest(processed, max_lines=case.get("max_lines", steward.DEFAULT_MAX_DIGEST_LINES))
    if op == "cli":
        stdout, stderr = StringIO(), StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = steward.main(["--queue", str(path), *case["argv"]])
        return {"exit_code": code, "stdout": stdout.getvalue(), "stderr": stderr.getvalue()}
    raise ValueError(f"unknown steward operation: {op}")


def main():
    unit = sys.argv[2]
    cases = json.load(sys.stdin)
    dispatch = {
        "compact_reinject": compact,
        "context_audit": audit,
        "tracker_hook": hook,
        "steward": steward_case,
    }[unit]
    output = []
    for case in cases:
        try:
            output.append(dispatch(case))
        except Exception as error:
            output.append({"error_type": type(error).__name__, "error": str(error)})
    print(json.dumps(output))


if __name__ == "__main__":
    main()
