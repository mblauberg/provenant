"""Byte limits must not lose the terminal semantics beyond the retained head."""

from .support import read_json


def test_capture_over_cap_retains_terminal_result(lane):
    owner = lane()
    owner.start()
    owner.ready()
    owner.send("over-cap")
    row = owner.finish(0)
    launch = read_json(owner.evidence / "launch.json")
    cap = launch["capture"]["max_bytes"] if owner.impl == "rust" else 20 * 1024 * 1024
    assert cap < 24 * 1024 * 1024, "fixture must exceed the configured cap"
    for name in ("events.jsonl", "stderr.log"):
        path = owner.evidence / name
        assert 0 < path.stat().st_size <= cap
        with path.open("rb") as stream:
            assert b"fixture_noise" in stream.read(256)
    assert row["status"] == "ok"
    assert row["evidence"]["exit"] == 0
    assert (owner.run / row["paths"]["result"]).read_text().strip() == "DONE"
    if owner.impl == "rust":
        record = read_json(owner.evidence / "supervisor.exit.json")
        for stream in ("events", "stderr"):
            assert record["capture"][stream]["bytes"] > cap
            assert record["capture"][stream]["truncated"] is True
        assert record["forced"] is None
