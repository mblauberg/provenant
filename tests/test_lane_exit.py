"""Supervisor exit records preserve the adapter receipt contract."""

import json
from pathlib import Path
import sys

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "skills/orchestrate/scripts"
from skills.orchestrate.scripts import provider_exec as supervisor
lane_exit = supervisor.lane_exit


def exit_record(**extra):
    return {"schema": "provenant.lane.exit.v1", "pid": 123, "pgid": 123,
            "started_at": "start", "ended_at": "end", "last_progress_at": "progress",
            "exit": None, "signal": 15, "forced": "wall_timeout", "terminal_event_seen": False,
            "reaped": [{"pid": 124, "command": "child"}], "spared": [{"pid": 125}],
            "census_warnings": ["partial census"], "observer_lost": False, "io_errors": ["capture: full"],
            "capture": {"events": {"bytes": 5000, "truncated": True}, "stderr": {"bytes": 30, "truncated": False}}, **extra}


def test_exit_mapping_keeps_signal_cleanup_and_capture_evidence():
    result = lane_exit.receipt_fields(exit_record())
    assert result["exit"] == -15
    assert result["pgid"] == 123
    assert result["forced"] == "timed_out"
    assert result["reaped"] == [{"pid": 124, "command": "child"}]
    assert result["spared"] == 1
    assert result["capture"]["events"] == {"bytes": 5000, "truncated": True}
    assert result["warnings"] == ["partial census", "capture: full"]


@pytest.mark.parametrize("forced,mapped", [(None, None), ("cancelled", "cancelled"),
    ("startup_timeout", "startup_timeout"), ("supervisor_signal", "interrupted"), ("usage_limited", "usage_limited")])
def test_forced_reasons_map_to_existing_receipt_status(forced, mapped):
    assert lane_exit.receipt_fields(exit_record(forced=forced))["forced"] == mapped


def test_stalled_and_unknown_schema_are_refused():
    for record in (exit_record(forced="stalled"), exit_record(schema="provenant.lane.exit.v2")):
        with pytest.raises(ValueError):
            lane_exit.receipt_fields(record)


def test_lost_observer_replays_capture_and_ignores_stale_summary(tmp_path):
    (tmp_path / "observer.json").write_text(json.dumps({"session_id": "stale"}))
    events = tmp_path / "events.jsonl"
    events.write_text(json.dumps({"type": "system", "subtype": "init", "model": "fixture", "session_id": "fresh"}) + "\n"
                      + json.dumps({"type": "result", "result": "DONE", "is_error": False}))
    stderr = tmp_path / "stderr.log"
    stderr.write_text("")
    observer = lane_exit.settle_observer({"adapter": "claude", "model": "fixture", "run_dir": str(tmp_path)},
                                         exit_record(observer_lost=True), events, stderr)
    assert observer.result()["session_id"] == "fresh"
    assert observer.terminal_text == "DONE"
    assert observer.terminal_at is not None
