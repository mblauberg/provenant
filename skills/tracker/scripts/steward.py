#!/usr/bin/env python3
"""Idempotent replay bookkeeping for the tracker skill's steward mode.

The actual triage judgement (duplicate check, placement, closing reason)
belongs to the agent running this skill, not this script. This module owns
only the mechanical part: parse the append-only queue, find events no one has
recorded an outcome for yet, and persist each outcome exactly once so a
rerun or a crash-resume never re-files, re-closes or re-links the same event.
"""

from __future__ import annotations

import argparse
from collections.abc import Callable
import json
from pathlib import Path
import sys
from typing import Any


EVENT_TYPES = frozenset({
    "lane_followup", "pr_opened", "pr_landed", "lane_abandoned",
    "owner_decision", "run_sweep",
})
REQUIRED_EVENT_FIELDS = frozenset({"id", "type", "payload"})
REQUIRED_OUTCOME_FIELDS = frozenset({"status", "summary"})
DEFAULT_MAX_DIGEST_LINES = 15


class QueueError(ValueError):
    """The queue file, an event or an outcome is not well-formed."""


def load_events(path: Path) -> list[dict[str, Any]]:
    """Parse the append-only JSONL queue; an absent file is an empty queue."""
    events: list[dict[str, Any]] = []
    if not path.is_file():
        return events
    for line_number, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except ValueError as exc:
            raise QueueError(f"{path}:{line_number}: invalid JSON: {exc}") from exc
        if not isinstance(event, dict) or not REQUIRED_EVENT_FIELDS <= set(event):
            raise QueueError(f"{path}:{line_number}: event needs {sorted(REQUIRED_EVENT_FIELDS)}")
        if event.get("type") not in EVENT_TYPES:
            raise QueueError(f"{path}:{line_number}: unknown event type {event.get('type')!r}")
        if not isinstance(event.get("id"), str) or not event["id"]:
            raise QueueError(f"{path}:{line_number}: id must be a non-empty string")
        outcome = event.get("outcome")
        if outcome is not None and (not isinstance(outcome, dict) or not REQUIRED_OUTCOME_FIELDS <= set(outcome)):
            raise QueueError(f"{path}:{line_number}: outcome needs {sorted(REQUIRED_OUTCOME_FIELDS)}")
        events.append(event)
    ids = [event["id"] for event in events]
    duplicates = sorted({event_id for event_id in ids if ids.count(event_id) > 1})
    if duplicates:
        raise QueueError(f"{path}: duplicate event id(s): {duplicates}")
    return events


def pending(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Events no prior run has recorded an outcome for."""
    return [event for event in events if "outcome" not in event]


def _write_events(path: Path, events: list[dict[str, Any]]) -> None:
    path.write_text("".join(json.dumps(event, sort_keys=True) + "\n" for event in events))


def record_outcome(path: Path, event_id: str, outcome: dict[str, Any]) -> bool:
    """Persist ``outcome`` for one event. Returns False, unchanged, if already recorded."""
    if not REQUIRED_OUTCOME_FIELDS <= set(outcome):
        raise QueueError(f"outcome needs {sorted(REQUIRED_OUTCOME_FIELDS)}")
    events = load_events(path)
    for event in events:
        if event["id"] == event_id:
            if "outcome" in event:
                return False
            event["outcome"] = outcome
            _write_events(path, events)
            return True
    raise QueueError(f"{path}: unknown event id {event_id!r}")


Handler = Callable[[dict[str, Any]], dict[str, Any]]


def replay(path: Path, handler: Handler) -> list[dict[str, Any]]:
    """Run ``handler`` over every pending event, in order, and persist outcomes.

    Idempotent: an event that already carries an outcome from an earlier
    (possibly interrupted) run is never re-handed to ``handler``, so replaying
    the same queue file twice performs each side effect at most once.
    """
    events = load_events(path)
    processed: list[dict[str, Any]] = []
    for event in events:
        if "outcome" in event:
            continue
        outcome = handler(event)
        if not isinstance(outcome, dict) or not REQUIRED_OUTCOME_FIELDS <= set(outcome):
            raise QueueError(f"handler for {event['id']} must return {sorted(REQUIRED_OUTCOME_FIELDS)}")
        event["outcome"] = outcome
        processed.append(event)
    if processed:
        _write_events(path, events)
    return processed


def digest(processed: list[dict[str, Any]], max_lines: int = DEFAULT_MAX_DIGEST_LINES) -> str:
    """Render at most ``max_lines`` lines, flagging outcomes needing a coordinator call."""
    lines = [
        f"{'! ' if event['outcome']['status'] == 'flagged' else ''}{event['type']} {event['id']}: {event['outcome']['summary']}"
        for event in processed
    ]
    if len(lines) <= max_lines:
        return "\n".join(lines)
    kept = lines[: max(0, max_lines - 1)]
    kept.append(f"... +{len(lines) - len(kept)} more")
    return "\n".join(kept)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--queue", type=Path, required=True)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("pending")
    record = sub.add_parser("record", help="persist one event's outcome (idempotent)")
    record.add_argument("--id", required=True)
    record.add_argument("--status", required=True)
    record.add_argument("--summary", required=True)
    digest_cmd = sub.add_parser("digest", help="render all recorded outcomes, bounded")
    digest_cmd.add_argument("--max-lines", type=int, default=DEFAULT_MAX_DIGEST_LINES)
    args = parser.parse_args(argv)

    try:
        if args.command == "pending":
            print(json.dumps(pending(load_events(args.queue)), sort_keys=True))
        elif args.command == "record":
            changed = record_outcome(args.queue, args.id, {"status": args.status, "summary": args.summary})
            if not changed:
                print(f"tracker steward: {args.id} already recorded; idempotent no-op", file=sys.stderr)
        elif args.command == "digest":
            processed = [event for event in load_events(args.queue) if "outcome" in event]
            print(digest(processed, max_lines=args.max_lines))
    except QueueError as exc:
        print(f"tracker steward: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
