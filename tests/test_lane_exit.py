"""Supervisor exit records preserve the adapter receipt contract."""

import json

import pytest

from skills.orchestrate.scripts import lane_exit, provider_exec as supervisor


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


def test_observer_summary_preserves_early_semantics_and_context(tmp_path):
    plan = {"adapter": "claude", "model": "fixture", "run_dir": str(tmp_path)}
    observer = supervisor.provider_observer.LiveObserver(plan)
    observer.feed_stdout((json.dumps({"type": "system", "subtype": "init", "session_id": "retained", "model": "fixture"}) + "\n").encode())
    observer.feed_stdout((json.dumps({"type": "assistant", "message": {"model": "answered", "content": [{"type": "text", "text": "DONE"}],
                                                                      "usage": {"input_tokens": 4, "output_tokens": 2}}}) + "\n").encode())
    observer.feed_stdout((json.dumps({"type": "result", "result": "DONE", "is_error": False}) + "\n").encode())
    (tmp_path / "observer.json").write_text(json.dumps(observer.result()))
    (tmp_path / "events.jsonl").write_text("capture no longer has early semantics\n")
    (tmp_path / "stderr.log").write_text("")
    restored = lane_exit.settle_observer(plan, exit_record(), tmp_path / "events.jsonl", tmp_path / "stderr.log")
    assert restored.result() == observer.result()


def test_incomplete_summary_cannot_leak_state_into_capture_fallback(tmp_path):
    plan = {"adapter": "claude", "model": "fixture", "run_dir": str(tmp_path)}
    (tmp_path / "observer.json").write_text(json.dumps({"session_id": "stale"}))
    (tmp_path / "events.jsonl").write_text(json.dumps({"type": "result", "result": "DONE", "is_error": False}))
    (tmp_path / "stderr.log").write_text("")
    restored = lane_exit.settle_observer(plan, exit_record(), tmp_path / "events.jsonl", tmp_path / "stderr.log")
    assert "session_id" not in restored.result()
    assert restored.terminal_text == "DONE"


def test_exit_records_are_bounded_and_symlinks_are_refused(tmp_path):
    record = tmp_path / "exit.json"
    record.write_text(json.dumps(exit_record()))
    assert lane_exit.read_json(record)["pid"] == 123
    with pytest.raises(ValueError):
        lane_exit.read_json(record, limit=10)
    link = tmp_path / "linked.json"
    link.symlink_to(record)
    with pytest.raises(OSError):
        lane_exit.read_json(link)


def test_settlement_diagnostics_keep_the_capture_byte_limit(tmp_path, monkeypatch):
    monkeypatch.setattr(supervisor.provider_observer, "MAX_EVENTS_BYTES", 128)
    path = tmp_path / "stderr.log"
    path.write_bytes(b"a" * 128)
    capture = lane_exit.Capture(path)
    capture.total = 1000
    capture.write(b"deadline exceeded\n")
    assert path.stat().st_size == 128
    assert capture.total == 1018
    assert capture.text().startswith("a")
    assert capture.text().endswith("deadline exceeded\n")
