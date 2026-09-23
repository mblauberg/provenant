import importlib.util
from datetime import datetime, timezone
from pathlib import Path
import hashlib
import sys


SCRIPT = Path(__file__).resolve().parents[1] / "skills" / "session" / "scripts" / "context_audit.py"
SPEC = importlib.util.spec_from_file_location("context_audit", SCRIPT)
context_audit = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = context_audit
SPEC.loader.exec_module(context_audit)


def codes(findings):
    return {item.code for item in findings}


def test_audit_reports_context_growth_without_deleting(tmp_path):
    docs = tmp_path / "docs"
    handoffs = docs / "handoffs"
    handoffs.mkdir(parents=True)
    (docs / "STATE.md").write_text("# State\n" + "x\n" * 121)
    (docs / "large.md").write_text("x" * 16000)
    scratch = tmp_path / ".temp-review.txt"
    scratch.write_text("keep me")
    backup = tmp_path / "SKILL.md.bak-2026-07-10"
    backup.write_text("backup")
    handoff = handoffs / "HANDOFF-old.md"
    handoff.write_text("old")
    handoff.touch()
    findings = context_audit.audit(
        tmp_path,
        now=datetime(2030, 1, 1, tzinfo=timezone.utc),
    )
    assert {"large-agent-doc", "root-scratch", "state-over-cap", "state-freshness-missing", "stale-live-handoff"} <= codes(findings)
    assert scratch.read_text() == "keep me"
    assert any(item.code == "root-scratch" and item.path == backup.name for item in findings)


def test_audit_reports_incomplete_run_index(tmp_path):
    run = tmp_path / ".agent-run" / "one"
    run.mkdir(parents=True)
    (run / "MANIFEST.md").write_text("manifest")
    findings = context_audit.audit(tmp_path)
    assert "incomplete-run-index" in codes(findings)


def test_direct_delivery_run_with_run_receipt_does_not_require_orchestration_scaffold(tmp_path):
    run = tmp_path / ".agent-run" / "direct"
    run.mkdir(parents=True)
    (run / "RUN.json").write_text('{"schema_version": 1, "contract": "delivery-run"}')
    assert "incomplete-run-index" not in codes(context_audit.audit(tmp_path))


def test_new_layout_reads_runs_and_skips_sessions_and_scratch(tmp_path):
    run = tmp_path / ".agent-run" / "runs" / "20260923-1000-dispatch-task-a1b2c3"
    run.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"ok"}')
    (tmp_path / ".agent-run" / "runs" / "index.jsonl").write_text("{}\n")
    (tmp_path / ".agent-run" / "sessions" / "20260923-chair").mkdir(parents=True)
    (tmp_path / ".agent-run" / "scratch" / "temp").mkdir(parents=True)
    assert "incomplete-run-index" not in codes(context_audit.audit(tmp_path))


def test_audit_reports_incomplete_claude_workflow_run(tmp_path):
    run = tmp_path / ".work" / "wf" / "implement" / "one"
    run.mkdir(parents=True)
    (run / "MANIFEST.md").write_text("manifest")
    findings = context_audit.audit(tmp_path)
    assert "incomplete-run-index" in codes(findings)


def test_cli_is_advisory_unless_strict(tmp_path):
    (tmp_path / "scratch.tmp").write_text("x")
    assert context_audit.main([str(tmp_path)]) == 0
    assert context_audit.main([str(tmp_path), "--strict"]) == 0
    assert context_audit.main([str(tmp_path), "--warnings-as-errors"]) == 1


def test_structural_errors_fail_without_an_extra_flag(tmp_path):
    (tmp_path / ".agent-run" / "broken").mkdir(parents=True)
    assert context_audit.main([str(tmp_path)]) == 1


def test_state_freshness_rejects_template_placeholder_and_accepts_utc(tmp_path):
    state = tmp_path / "STATE.md"
    template = Path(__file__).resolve().parents[1] / "skills" / "autopilot" / "templates" / "STATE.template.md"
    state.write_text(template.read_text())
    findings = context_audit.audit(tmp_path)
    assert "state-freshness-missing" in codes(findings)
    state.write_text(template.read_text().replace("<YYYY-MM-DDTHH:MM:SSZ>", "2026-07-10T12:00:00Z"))
    assert "state-freshness-missing" not in codes(context_audit.audit(tmp_path))
    state.write_text(template.read_text().replace("<YYYY-MM-DDTHH:MM:SSZ>", "2026-07-10"))
    assert "state-freshness-missing" in codes(context_audit.audit(tmp_path))


def test_duplicate_active_handoffs_are_structural_errors(tmp_path):
    handoffs = tmp_path / "docs" / "handoffs"
    handoffs.mkdir(parents=True)
    body = "Status: active\nEffort: E1\nLeg: L2\nSupersedes: none\nConsumed-at: pending\n"
    (handoffs / "HANDOFF-one.md").write_text(body)
    (handoffs / "HANDOFF-two.md").write_text(body)
    findings = context_audit.audit(tmp_path)
    assert "duplicate-active-handoff" in codes(findings)


def test_partial_handoff_metadata_is_an_error(tmp_path):
    handoffs = tmp_path / "docs" / "handoffs"
    handoffs.mkdir(parents=True)
    (handoffs / "HANDOFF-partial.md").write_text("Status: active\n")
    findings = context_audit.audit(tmp_path)
    assert "handoff-metadata-missing" in codes(findings)


def test_audit_is_byte_read_only(tmp_path):
    (tmp_path / "scratch.tmp").write_text("payload")
    before = {
        path.relative_to(tmp_path).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in tmp_path.rglob("*")
        if path.is_file()
    }
    context_audit.audit(tmp_path)
    after = {
        path.relative_to(tmp_path).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in tmp_path.rglob("*")
        if path.is_file()
    }
    assert after == before


def test_audit_does_not_traverse_shared_worktree_checkouts(tmp_path):
    worktree = tmp_path / ".worktrees" / "peer"
    handoffs = worktree / "docs" / "handoffs"
    handoffs.mkdir(parents=True)
    (worktree / "AGENTS.md").write_text("x" * 20000)
    (worktree / "STATE.md").write_text("# stale\n" + "x\n" * 200)
    (handoffs / "HANDOFF-old.md").write_text("old")

    assert context_audit.audit(
        tmp_path, now=datetime(2030, 1, 1, tzinfo=timezone.utc),
    ) == []


def test_audit_uses_pruned_walk_instead_of_unpruned_rglob(tmp_path, monkeypatch):
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "ok.md").write_text("ok")

    def explode(*_args, **_kwargs):
        raise AssertionError("unpruned rglob used")

    monkeypatch.setattr(Path, "rglob", explode)
    context_audit.audit(tmp_path)


def test_audit_never_reads_symlinked_run_or_markdown_outside_root(tmp_path):
    outside = tmp_path.parent / f"{tmp_path.name}-outside"
    outside.mkdir()
    (outside / "private-run").mkdir()
    (tmp_path / ".agent-run").symlink_to(outside, target_is_directory=True)
    (outside / "secret.md").write_text("Canonical key: private\n")
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "linked.md").symlink_to(outside / "secret.md")
    findings = context_audit.audit(tmp_path)
    assert not any("private-run" in finding.path or "linked.md" in finding.path for finding in findings)


def test_audit_does_not_follow_nested_run_directory_symlinks(tmp_path):
    outside = tmp_path.parent / f"{tmp_path.name}-nested-outside"
    outside_run = outside / "private-run"
    outside_run.mkdir(parents=True)
    (outside_run / "MANIFEST.md").write_text("private")

    agent_runs = tmp_path / ".agent-run"
    agent_runs.mkdir()
    (agent_runs / "linked-run").symlink_to(outside_run, target_is_directory=True)

    work = tmp_path / ".work"
    work.mkdir()
    (work / "wf").symlink_to(outside, target_is_directory=True)

    findings = context_audit.audit(tmp_path)
    assert not any("linked-run" in finding.path or "private-run" in finding.path for finding in findings)


def test_audit_does_not_read_symlinked_run_receipt_outside_root(tmp_path, monkeypatch):
    outside = tmp_path.parent / f"{tmp_path.name}-receipt-outside"
    outside.mkdir()
    private_receipt = outside / "RUN.json"
    private_receipt.write_text('{"schema_version": 1, "contract": "delivery-run"}')
    run = tmp_path / ".agent-run" / "linked-receipt"
    run.mkdir(parents=True)
    (run / "RUN.json").symlink_to(private_receipt)

    original = Path.read_text

    def guarded(path, *args, **kwargs):
        if path.resolve(strict=False) == private_receipt.resolve():
            raise AssertionError("audit read a receipt through an out-of-root symlink")
        return original(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", guarded)
    findings = context_audit.audit(tmp_path)
    assert "incomplete-run-index" in codes(findings)


def test_delivery_run_manifest_reports_orphan_and_expired_scratch(tmp_path):
    run = tmp_path / ".agent-run" / "DEL-1"
    run.mkdir(parents=True)
    for name in ("MANIFEST.md", "RUN_RECEIPT.json", "SYNTHESIS.md", "FINAL_GATE.md"):
        (run / name).write_text(name)
    (run / "RUN.json").write_text(__import__("json").dumps({
        "schema_version": 1,
        "contract": "delivery-run",
        "artifacts": [{
            "id": "scratch-one",
            "path": "scratch-one.tmp",
            "class": "scratch",
            "owner": "run",
            "retention": "until-expiry",
            "expires_at": "2026-01-01T00:00:00Z",
        }],
    }))
    (run / "scratch-one.tmp").write_text("owned")
    (run / "scratch-orphan.tmp").write_text("unknown")
    findings = context_audit.audit(tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))
    assert "expired-run-scratch" in codes(findings)
    assert "orphan-run-scratch" in codes(findings)
    assert (run / "scratch-one.tmp").exists()


def test_duplicate_explicit_canonical_keys_are_reported(tmp_path):
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "one.md").write_text("Canonical key: current-architecture\n")
    (docs / "two.md").write_text("Canonical key: current-architecture\n")
    findings = context_audit.audit(tmp_path)
    assert "duplicate-canonical-key" in codes(findings)


def test_old_raw_log_is_a_retention_signal(tmp_path):
    log = tmp_path / "worker.jsonl"
    log.write_text("{}\n")
    import os
    old = datetime(2020, 1, 1, tzinfo=timezone.utc).timestamp()
    os.utime(log, (old, old))
    findings = context_audit.audit(tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc), stale_log_days=30)
    assert "stale-raw-log" in codes(findings)


def test_effort_markdown_does_not_own_handoff_state(tmp_path):
    docs = tmp_path / "docs"
    efforts = docs / "efforts"
    handoffs = docs / "handoffs"
    efforts.mkdir(parents=True)
    handoffs.mkdir()
    (efforts / "EFFORT-one.md").write_text("# EFFORT: one\n\nStatus: done\n")
    (handoffs / "HANDOFF-one.md").write_text(
        "Status: active\nEffort: one\nLeg: final\nSupersedes: none\nConsumed-at: pending\n"
    )
    findings = context_audit.audit(tmp_path)
    assert "done-effort-active-handoff" not in codes(findings)
