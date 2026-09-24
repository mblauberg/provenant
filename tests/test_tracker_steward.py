"""Idempotent replay and bounded digest contract for the tracker skill's steward mode."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "tracker" / "scripts" / "steward.py"


def steward():
    spec = importlib.util.spec_from_file_location("tracker_steward_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(module)
    return module


def write_queue(path: Path, events: list[dict]) -> None:
    path.write_text("".join(json.dumps(event) + "\n" for event in events))


def test_replay_processes_each_pending_event_exactly_once(tmp_path):
    module = steward()
    queue = tmp_path / "tracker-queue.jsonl"
    write_queue(queue, [
        {"id": "evt-1", "type": "pr_opened", "payload": {"pr": 1}},
        {"id": "evt-2", "type": "run_sweep", "payload": {}},
    ])
    calls = []

    def handler(event):
        calls.append(event["id"])
        return {"status": "done", "summary": f"handled {event['id']}"}

    processed_first = module.replay(queue, handler)
    assert [event["id"] for event in processed_first] == ["evt-1", "evt-2"]
    assert calls == ["evt-1", "evt-2"]

    # A second replay of the same file must not re-invoke the handler: every
    # event already carries an outcome, so a rerun or crash-resume is a no-op.
    processed_second = module.replay(queue, handler)
    assert processed_second == []
    assert calls == ["evt-1", "evt-2"]

    persisted = module.load_events(queue)
    assert {event["id"]: event["outcome"]["summary"] for event in persisted} == {
        "evt-1": "handled evt-1",
        "evt-2": "handled evt-2",
    }


def test_replay_only_touches_events_still_pending_after_a_partial_run(tmp_path):
    module = steward()
    queue = tmp_path / "tracker-queue.jsonl"
    write_queue(queue, [
        {"id": "evt-1", "type": "pr_landed", "payload": {}, "outcome": {"status": "done", "summary": "closed #1"}},
        {"id": "evt-2", "type": "lane_followup", "payload": {}},
    ])
    calls = []

    def handler(event):
        calls.append(event["id"])
        return {"status": "flagged", "summary": "needs owner call"}

    processed = module.replay(queue, handler)
    assert calls == ["evt-2"]
    assert [event["id"] for event in processed] == ["evt-2"]


def test_record_outcome_is_idempotent(tmp_path):
    module = steward()
    queue = tmp_path / "tracker-queue.jsonl"
    write_queue(queue, [{"id": "evt-1", "type": "owner_decision", "payload": {}}])

    assert module.record_outcome(queue, "evt-1", {"status": "done", "summary": "recorded"}) is True
    assert module.record_outcome(queue, "evt-1", {"status": "done", "summary": "different text"}) is False

    event = module.load_events(queue)[0]
    assert event["outcome"]["summary"] == "recorded"


def test_load_events_rejects_duplicate_ids_and_unknown_types(tmp_path):
    module = steward()
    duplicate = tmp_path / "duplicate.jsonl"
    write_queue(duplicate, [
        {"id": "evt-1", "type": "run_sweep", "payload": {}},
        {"id": "evt-1", "type": "run_sweep", "payload": {}},
    ])
    with pytest.raises(module.QueueError, match="duplicate"):
        module.load_events(duplicate)

    unknown_type = tmp_path / "unknown.jsonl"
    write_queue(unknown_type, [{"id": "evt-1", "type": "batch_wave", "payload": {}}])
    with pytest.raises(module.QueueError, match="unknown event type"):
        module.load_events(unknown_type)


def test_digest_is_bounded_at_fifteen_lines_with_a_truncation_marker(tmp_path):
    module = steward()
    processed = [
        {"id": f"evt-{index}", "type": "run_sweep", "outcome": {"status": "done", "summary": f"swept {index}"}}
        for index in range(20)
    ]
    rendered = module.digest(processed)
    lines = rendered.splitlines()
    assert len(lines) == 15
    assert lines[-1] == "... +6 more"


def test_digest_flags_outcomes_needing_a_coordinator_call(tmp_path):
    module = steward()
    processed = [
        {"id": "evt-1", "type": "lane_followup", "outcome": {"status": "flagged", "summary": "possible duplicate of #42"}},
        {"id": "evt-2", "type": "pr_landed", "outcome": {"status": "done", "summary": "closed #7"}},
    ]
    rendered = module.digest(processed)
    assert rendered.splitlines() == [
        "! lane_followup evt-1: possible duplicate of #42",
        "pr_landed evt-2: closed #7",
    ]
