import importlib
import json
from pathlib import Path
from datetime import UTC, datetime, timedelta
import pytest

FIX = Path(__file__).parent / "fixtures/fabric-v1"


def records():
    return importlib.import_module("skills.orchestrate.scripts.fabric_records")


def test_digest_matches_shared_example_and_question():
    row = json.loads((FIX / "attempt.json").read_text())
    assert records().render_digest(row) == row["digest"]
    row.update(status="input_required", question="Target main or release/3?")
    assert (
        records().render_digest(row)
        == 'input_required mcp-a81f3c "Target main or release/3?" · reply: fabric_dispatch{resume:"mcp-a81f3c",prompt:"…"}'
    )


def test_cooldown_locked_merge_expiry_and_defaults(tmp_path):
    module = records()
    path = tmp_path / "cooldowns.json"
    row = json.loads((FIX / "attempt.json").read_text())
    row.update(status="usage_limited", reset_at=None, retry_after=None)
    row["provenance"]["requested"]["adapter"] = "claude"
    row["provenance"]["resolved_model"] = "opus"
    at = datetime(2026, 9, 23, tzinfo=UTC)
    module.write_cooldown(row, path=path, at=at)
    data = json.loads(path.read_text())
    assert data["cooldowns"]["claude/opus"]["cooling_until"] == "2026-09-23T01:00:00Z"
    row["provenance"]["resolved_model"] = "sonnet"
    row.update(status="rate_limited", retry_after=30)
    module.write_cooldown(row, path=path, at=at + timedelta(hours=2))
    assert set(json.loads(path.read_text())["cooldowns"]) == {"claude/sonnet"}
    assert (tmp_path / "cooldowns.lock").is_file()


def test_index_keeps_distinct_attempts_and_deduplicates_recovery(tmp_path):
    module = records()
    row = json.loads((FIX / "attempt.json").read_text())
    for attempt in (1, 1, 2):
        row["attempt"] = attempt
        module.append_index(row, tmp_path / "runs/example", root=tmp_path)
    entries = [
        json.loads(line)
        for line in (tmp_path / "runs/index.jsonl").read_text().splitlines()
    ]
    assert [item["attempt"] for item in entries] == [1, 2]
    assert all(item["line"] == row["provenance"]["line"] for item in entries)


def test_cooldown_writer_refuses_symlinked_store(tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir()
    linked = tmp_path / "linked"
    linked.symlink_to(outside, target_is_directory=True)
    row = json.loads((FIX / "attempt.json").read_text())
    row["status"] = "rate_limited"
    with pytest.raises((ValueError, OSError)):
        records().write_cooldown(row, path=linked / "cooldowns.json")
    assert list(outside.iterdir()) == []
