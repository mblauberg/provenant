"""Shared wire examples: additive fields are allowed, removals are versioned."""

import json
from pathlib import Path

FIXTURES = Path(__file__).parent / "fixtures/fabric-v1"
STATUSES = {
    "ok",
    "partial",
    "failed",
    "usage_limited",
    "rate_limited",
    "auth_required",
    "model_unavailable",
    "permission_blocked",
    "stalled",
    "timed_out",
    "cancelled",
    "interrupted",
    "rejected",
    "tool_missing",
    "input_required",
}


def validate_attempt(row):
    required = {
        "schema",
        "run_id",
        "task_id",
        "attempt",
        "state",
        "status",
        "mode",
        "cwd",
        "worktree",
        "started_at",
        "ended_at",
        "last_progress_at",
        "pgid",
        "session_id",
        "retryable",
        "reset_at",
        "retry_after",
        "fix",
        "evidence",
        "question",
        "applied",
        "warnings",
        "provenance",
        "paths",
        "digest",
    }
    assert required <= row.keys()
    assert row["schema"] in {"fabric.attempt.v1", "fabric.status.v1"}
    assert type(row["attempt"]) is int and row["attempt"] > 0
    assert row["state"] in {"queued", "running", "terminal"}
    assert (
        row["status"] in STATUSES
        if row["state"] == "terminal"
        else row["status"] is None
    )
    assert row["mode"] in {"read_only", "worktree_write"}
    assert type(row["retryable"]) is bool
    assert {"exit", "signal", "signature", "excerpt"} <= row["evidence"].keys()
    assert len(row["evidence"]["excerpt"]) <= 200
    assert {"sandbox", "network", "add_dirs", "guarantee"} <= row["applied"].keys()
    assert row["applied"]["guarantee"] in {"enforced", "best_effort", "prompt_only"}
    assert {"result", "stderr", "events", "receipt"} <= row["paths"].keys()
    provenance = row["provenance"]
    assert {
        "requested",
        "resolved_model",
        "observed_model",
        "observed_source",
        "identity",
        "provider",
        "transport",
        "family",
        "effort_requested",
        "effort_applied",
        "cli_version",
        "fallback_from",
        "notes",
        "line",
    } <= provenance.keys()
    assert provenance["identity"] in {"observed", "resolved", "unknown"}
    assert {"adapter", "alias", "model", "effort"} <= provenance["requested"].keys()
    assert isinstance(row["digest"], str) and row["digest"]


def test_shared_contract_examples():
    read = lambda name: json.loads((FIXTURES / name).read_text())
    validate_attempt(read("attempt.json"))
    validate_attempt(read("status.json"))
    assert read("attempt.json")["provenance"] == read("provenance.json")
    assert read("status.json")["attempts"] == [read("attempt.json")]
    cooldowns = read("cooldowns.json")
    assert cooldowns["schema"] == "fabric.cooldowns.v1"
    for key, item in cooldowns["cooldowns"].items():
        assert key == item["adapter"] + "/" + (item["model"] or "*")
        assert {
            "account",
            "status",
            "cooling_until",
            "source_run",
            "recorded_at",
            "signature",
        } <= item.keys()
    assert {
        "run_id",
        "task_id",
        "attempt",
        "dir",
        "status",
        "line",
        "result_sha256",
        "ended_at",
    } <= read("index.json").keys()
    assert read("resume.json")["resume"] == read("attempt.json")["run_id"]
    assert len(read("layout-cases.json")) == 4
