"""Safety contract for the repository run-artifact cleaner."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
import importlib.util
import json
import os
from pathlib import Path
import subprocess


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "clean.py"


def cleaner():
    spec = importlib.util.spec_from_file_location("provenant_clean_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_pid_alive_accepts_legacy_locale_start_after_canonical_check(monkeypatch):
    module = cleaner()
    calls = []
    monkeypatch.setattr(module.os, "kill", lambda _pid, _signal: None)

    def fake_command(*argv, **kwargs):
        canonical = kwargs.get("env", {}).get("LC_ALL") == "C"
        calls.append("C" if canonical else "inherited")
        return subprocess.CompletedProcess(argv, 0,
                                           stdout="Wed Sep 23 17:17:42 2026" if canonical
                                           else "Wed 23 Sep 17:17:42 2026")

    monkeypatch.setattr(module, "_command", fake_command)
    assert module._pid_alive(12345, "Wed 23 Sep 17:17:42 2026")
    assert calls == ["C", "inherited"]


def repo(tmp_path: Path) -> Path:
    root = tmp_path / "repo"
    root.mkdir()
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.name", "Test"], check=True)
    (root / "README.md").write_text("test\n")
    subprocess.run(["git", "-C", str(root), "add", "README.md"], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-qm", "init"], check=True)
    return root


def old(path: Path, days: int = 20):
    timestamp = (datetime.now(timezone.utc) - timedelta(days=days)).timestamp()
    for child in path.rglob("*"):
        os.utime(child, (timestamp, timestamp))
    os.utime(path, (timestamp, timestamp))


def test_classifies_new_legacy_and_unknown_without_deleting(tmp_path):
    root = repo(tmp_path)
    runs = root / ".agent-run" / "runs"
    completed = runs / "20260801-1200-dispatch-task-a1b2c3"
    completed.mkdir(parents=True)
    (completed / "RUN_RECEIPT.json").write_text(json.dumps({"status": "succeeded", "closed_at": "2026-08-01T12:00:00Z"}))
    current = runs / "20260801-1200-dispatch-current-b2c3d4"
    current.mkdir(parents=True)
    (current / "RUN_RECEIPT.json").write_text(json.dumps({"status": "ok", "closed_at": "2026-08-01T12:00:00Z"}))
    legacy = root / ".agent-run" / "mcp-abc123"
    legacy.mkdir()
    (legacy / "RUN_RECEIPT.json").write_text(json.dumps({"status": "failed", "closed_at": "2026-08-01T12:00:00Z"}))
    sibling = root / ".agent-run" / "mcp-abc123-owner.stderr.log"
    sibling.write_text("error\n")
    unknown = root / ".agent-run" / "someone-notes"
    unknown.mkdir()
    old(completed)
    old(current)
    old(legacy)
    old(sibling)
    old(unknown)

    plan = cleaner().plan(root, pr_bodies=[])
    rows = {row["path"]: row for row in plan["rows"]}
    assert rows[".agent-run/runs/20260801-1200-dispatch-task-a1b2c3"]["verdict"] == "delete"
    assert rows[".agent-run/runs/20260801-1200-dispatch-current-b2c3d4"]["verdict"] == "delete"
    assert rows[".agent-run/mcp-abc123"]["verdict"] == "delete"
    assert rows[".agent-run/mcp-abc123-owner.stderr.log"]["verdict"] == "delete"
    assert rows[".agent-run/someone-notes"]["verdict"].startswith("triage:")
    assert completed.exists() and legacy.exists() and sibling.exists() and unknown.exists()


def test_legacy_succeeded_receipt_uses_success_retention(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-dispatch-legacy-a1b2c3"
    run.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text(json.dumps({"status": "succeeded", "closed_at": "2026-08-01T12:00:00Z"}))
    old(run, days=5)

    plan = cleaner().plan(root, pr_bodies=[])
    row = next(item for item in plan["rows"] if item["path"].endswith("dispatch-legacy-a1b2c3"))
    assert row["verdict"] == "keep:retention-7d"


def test_git_repo_without_github_remote_can_clean_runs(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-dispatch-task-a1b2c3"
    run.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"ok"}\n')
    old(run)
    rows = {row["path"]: row for row in cleaner().plan(root)["rows"]}
    assert rows[f".agent-run/runs/{run.name}"]["verdict"] == "delete"


def test_unrelated_kept_run_growth_does_not_invalidate_cleanup_plan(tmp_path):
    root = repo(tmp_path)
    runs = root / ".agent-run" / "runs"
    expired = runs / "20260801-1200-dispatch-expired-a1b2c3"
    active = runs / "20260923-1200-dispatch-active-b1c2d3"
    for run, status in ((expired, "ok"), (active, "active")):
        run.mkdir(parents=True)
        (run / "RUN_RECEIPT.json").write_text(json.dumps({"status": status}))
    old(expired)
    module = cleaner()
    first = module.plan(root, pr_bodies=[])
    (active / "progress.log").write_text("progress\n")
    second = module.plan(root, pr_bodies=[])
    assert first["plan_sha256"] == second["plan_sha256"]


def test_apply_requires_current_digest_and_respects_pins_and_acceptance(tmp_path):
    root = repo(tmp_path)
    runs = root / ".agent-run" / "runs"
    ordinary = runs / "20260801-1200-dispatch-task-a1b2c3"
    pinned = runs / "20260801-1200-review-pin-b1c2d3"
    delivery = runs / "20260801-1200-delivery-out-c1d2e3"
    for path in (ordinary, pinned, delivery):
        path.mkdir(parents=True)
        old(path)
    (ordinary / "RUN_RECEIPT.json").write_text(json.dumps({"status": "ok"}))
    (pinned / "RUN_RECEIPT.json").write_text(json.dumps({"status": "ok"}))
    (pinned / "KEEP").write_text("needed\n")
    (delivery / "RUN.json").write_text(json.dumps({"human_gates": {"acceptance": {"status": "pending"}}}))
    for path in (ordinary, pinned, delivery):
        old(path)
    module = cleaner()
    first = module.plan(root, pr_bodies=[])
    assert module.apply(root, first["plan_sha256"], pr_bodies=[]) == [".agent-run/runs/20260801-1200-dispatch-task-a1b2c3"]
    assert not ordinary.exists()
    assert pinned.exists() and delivery.exists()
    try:
        module.apply(root, first["plan_sha256"], pr_bodies=[])
    except module.CleanError as exc:
        assert "plan" in str(exc)
    else:
        raise AssertionError("stale plan was accepted")


def test_scratch_retention_and_index_are_protected(tmp_path):
    root = repo(tmp_path)
    scratch = root / ".agent-run" / "scratch"
    scratch.mkdir(parents=True)
    expired = scratch / "old.txt"
    expired.write_text("temporary")
    old(expired, 2)
    index = root / ".agent-run" / "runs" / "index.jsonl"
    index.parent.mkdir(parents=True)
    index.write_text('{"ended_at":"2026-01-01T00:00:00Z"}\n')
    old(index, 400)
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[".agent-run/scratch/old.txt"]["verdict"] == "delete"
    assert rows[".agent-run/runs/index.jsonl"]["verdict"].startswith("keep:")
    delayed = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[], older_than=7)["rows"]}
    assert delayed[".agent-run/scratch/old.txt"]["verdict"].startswith("keep:")


def test_worktrees_need_registration_clean_state_and_merge_proof(tmp_path):
    root = repo(tmp_path)
    shared = root / ".worktrees"
    shared.mkdir()
    unregistered = shared / "notes"
    unregistered.mkdir()
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    merged = shared / "lane-merged"
    subprocess.run(["git", "-C", str(root), "branch", "lane/merged", head], check=True)
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", str(merged), "lane/merged"], check=True)
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[".worktrees/notes"]["verdict"] == "triage:unregistered"
    assert rows[".worktrees/lane-merged"]["verdict"] == "keep:branch-at-base"
    (merged / "work.txt").write_text("done\n")
    subprocess.run(["git", "-C", str(merged), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(merged), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--no-ff", "-qm", "merge lane", "lane/merged"], check=True)
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[".worktrees/lane-merged"]["verdict"] == "delete"
    (merged / "untracked.txt").write_text("changes\n")
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[".worktrees/lane-merged"]["verdict"] == "triage:merged-dirty"


def test_live_process_cwd_protects_a_merged_worktree(tmp_path):
    root = repo(tmp_path)
    shared = root / ".worktrees"
    shared.mkdir()
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    target = shared / "lane-live"
    subprocess.run(["git", "-C", str(root), "branch", "lane/live", head], check=True)
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", str(target), "lane/live"], check=True)
    (target / "work.txt").write_text("done\n")
    subprocess.run(["git", "-C", str(target), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(target), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--no-ff", "-qm", "merge lane", "lane/live"], check=True)
    process = subprocess.Popen(["sleep", "30"], cwd=target)
    try:
        rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
        assert rows[".worktrees/lane-live"]["verdict"] == "keep:live"
    finally:
        process.terminate()
        process.wait(timeout=5)


def test_installed_template_routes_clean_to_product_owner(tmp_path):
    environment = os.environ.copy()
    environment["AGENT_FABRIC_PRODUCT_ROOT"] = str(SCRIPT.parent.parent)
    result = subprocess.run([
        "python3", str(SCRIPT.with_name("provenant.template")), "clean", "--repo", str(tmp_path), "--json",
    ], env=environment, text=True, capture_output=True, check=False)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["root"] == str(tmp_path.resolve())


def test_checkout_entry_point_routes_clean(tmp_path):
    result = subprocess.run([
        "python3", str(SCRIPT.with_name("provenant")), "clean", "--repo", str(tmp_path), "--json",
    ], text=True, capture_output=True, check=False)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["root"] == str(tmp_path.resolve())


def test_abandoned_active_run_closes_before_retention_deletion(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-dispatch-lost-a1b2c3"
    run.mkdir(parents=True)
    receipt = run / "RUN_RECEIPT.json"
    receipt.write_text('{"status":"active","closed_at":null}\n')
    (run / "dispatch-owner.json").write_text('{"owner_pid":999999}\n')
    old(run, 3)
    module = cleaner()
    proposal = module.plan(root, pr_bodies=[])
    assert next(row for row in proposal["rows"] if row["path"].endswith(run.name))["verdict"] == "abandon"
    assert module.apply(root, proposal["plan_sha256"], pr_bodies=[]) == []
    assert run.exists()
    result = json.loads(receipt.read_text())
    assert result["status"] == "interrupted"
    assert result["terminal_reason"] == "abandoned: owner gone"


def test_active_orchestration_is_never_abandoned(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-orch-long-a1b2c3"
    run.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"active"}\n')
    old(run, 3)
    row = next(row for row in cleaner().plan(root, pr_bodies=[])["rows"] if row["path"].endswith(run.name))
    assert row["verdict"] == "triage:active-no-owner"


def test_recent_progress_prevents_abandoning_dispatch(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-dispatch-long-a1b2c3"
    owner = run / "_owner"
    owner.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"active"}\n')
    (owner / "dispatch-owner.json").write_text('{"owner_pid":999999}\n')
    old(run, 3)
    (owner / "progress.log").write_text("recent\n")
    row = next(row for row in cleaner().plan(root, pr_bodies=[])["rows"] if row["path"].endswith(run.name))
    assert row["verdict"] == "keep:active"


def test_typed_failures_expire_and_input_required_stays_resumable(tmp_path):
    root = repo(tmp_path)
    statuses = ("usage_limited", "rate_limited", "auth_required", "model_unavailable", "permission_blocked")
    for index, status in enumerate(statuses):
        run = root / ".agent-run" / "runs" / f"20260801-1200-dispatch-{status.replace('_', '-')}-{index:06x}"
        run.mkdir(parents=True)
        (run / "RUN_RECEIPT.json").write_text(json.dumps({"status": status}))
        old(run)
    resumable = root / ".agent-run" / "runs" / "20260801-1200-dispatch-question-a1b2c3"
    resumable.mkdir(parents=True)
    (resumable / "RUN_RECEIPT.json").write_text('{"status":"input_required"}\n')
    old(resumable)
    legacy_resumable = root / ".agent-run" / "runs" / "20260801-1200-dispatch-legacy-a1b2c4"
    legacy_resumable.mkdir(parents=True)
    (legacy_resumable / "RUN_RECEIPT.json").write_text('{"status":"failed","resumable":true}\n')
    old(legacy_resumable)
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    for status in statuses:
        assert any(row["verdict"] == "delete" for row in rows.values() if status.replace("_", "-") in row["path"])
    assert rows[f".agent-run/runs/{resumable.name}"]["verdict"] == "keep:resumable"
    assert rows[f".agent-run/runs/{legacy_resumable.name}"]["verdict"] == "keep:resumable"


def test_mixed_case_dispatch_run_names_are_classified(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-dispatch-MyWorkspace-aB3dE4"
    run.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"failed"}\n')
    old(run)
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[f".agent-run/runs/{run.name}"]["verdict"] == "delete"


def test_sessions_are_triage_only_even_when_old(tmp_path):
    root = repo(tmp_path)
    session = root / ".agent-run" / "sessions" / "20260801-chair"
    session.mkdir(parents=True)
    (session / "STATE.md").write_text("durable\n")
    old(session)
    row = next(row for row in cleaner().plan(root, include=frozenset({"sessions"}), pr_bodies=[])["rows"]
               if row["path"].endswith(session.name))
    assert row["verdict"] == "triage:session-idle"


def test_owner_logs_expire_before_a_failed_run_but_manifest_remains(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260901-1200-dispatch-failure-a1b2c3"
    owner = run / "_owner"
    owner.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"failed"}\n')
    log = owner / "stderr.log"
    log.write_text("diagnostic\n")
    manifest = owner / "task-manifest.json"
    manifest.write_text("{}\n")
    old(run, 10)
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[f".agent-run/runs/{run.name}"]["verdict"].startswith("keep:retention")
    assert rows[f".agent-run/runs/{run.name}/_owner/stderr.log"]["verdict"] == "delete"
    assert rows[f".agent-run/runs/{run.name}/_owner/task-manifest.json"]["verdict"].startswith("keep:")


def test_short_run_id_in_session_state_protects_canonical_run(tmp_path):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-dispatch-task-a1b2c3"
    run.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"ok","run_id":"mcp-a1b2c3"}\n')
    old(run)
    session = root / ".agent-run" / "sessions" / "20260923-chair"
    session.mkdir(parents=True)
    (session / "STATE.md").write_text("Current run: mcp-a1b2c3\n")
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[f".agent-run/runs/{run.name}"]["verdict"] == "keep:referenced"


def test_dangling_symlink_is_triaged_without_aborting_plan(tmp_path):
    root = repo(tmp_path)
    agent = root / ".agent-run"
    agent.mkdir()
    (agent / "dangling").symlink_to(root / "missing")
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[".agent-run/dangling"]["verdict"] == "triage:symlink"


def test_ignored_worktree_run_is_never_pruned(tmp_path):
    root = repo(tmp_path)
    shared = root / ".worktrees"
    shared.mkdir()
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    target = shared / "lane-merged"
    subprocess.run(["git", "-C", str(root), "branch", "lane/merged", head], check=True)
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", str(target), "lane/merged"], check=True)
    ignored = target / ".agent-run" / "runs" / "20260801-1200-delivery-x-a1b2c3"
    ignored.mkdir(parents=True)
    (ignored / "RUN.json").write_text('{"human_gates":{"acceptance":{"status":"pending"}}}\n')
    (root / ".git" / "info" / "exclude").write_text("/.agent-run/\n")
    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[".worktrees/lane-merged"]["verdict"] == "keep:worktree-runs"


def test_legacy_delivery_and_research_keep_merged_worktree(tmp_path):
    root = repo(tmp_path)
    shared = root / ".worktrees"
    shared.mkdir()
    target = shared / "lane-merged"
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", "-b", "lane/merged", str(target)], check=True)
    (target / "work.txt").write_text("merged work\n")
    subprocess.run(["git", "-C", str(target), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(target), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--no-ff", "-qm", "merge lane", "lane/merged"], check=True)
    delivery = target / ".agent-run" / "DEL-1"
    delivery.mkdir(parents=True)
    (delivery / "RUN.json").write_text('{"human_gates":{"acceptance":{"status":"pending"}}}\n')
    research = target / ".agent-run" / "research"
    research.mkdir()
    (research / "notes.md").write_text("keep this research\n")
    (root / ".git" / "info" / "exclude").write_text("/.agent-run/\n")

    rows = {row["path"]: row for row in cleaner().plan(root, pr_bodies=[])["rows"]}
    assert rows[".worktrees/lane-merged"]["verdict"] == "keep:worktree-runs"


def test_worktree_scratch_does_not_count_as_a_run(tmp_path):
    root = repo(tmp_path)
    target = root / ".worktrees" / "lane-done"
    target.parent.mkdir()
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", "-b", "lane/done", str(target)], check=True)
    (target / "work.txt").write_text("done\n")
    subprocess.run(["git", "-C", str(target), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(target), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--no-ff", "-qm", "merge lane", "lane/done"], check=True)
    (root / ".git" / "info" / "exclude").write_text("/.agent-run/\n")
    scratch = target / ".agent-run" / "scratch"
    scratch.mkdir(parents=True)
    (scratch / "note.txt").write_text("scratch\n")
    module = cleaner()
    proposal = module.plan(root, pr_bodies=[])
    row = next(row for row in proposal["rows"] if row["path"] == ".worktrees/lane-done")
    assert row["verdict"] == "delete"
    assert module.apply(root, proposal["plan_sha256"], pr_bodies=[], human_authorised=True) == [".worktrees/lane-done"]


def test_missing_gh_warns_and_git_proven_worktree_can_expire(tmp_path, monkeypatch):
    root = repo(tmp_path)
    target = root / ".worktrees" / "lane-done"
    target.parent.mkdir()
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", "-b", "lane/done", str(target)], check=True)
    (target / "work.txt").write_text("done\n")
    subprocess.run(["git", "-C", str(target), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(target), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--no-ff", "-qm", "merge lane", "lane/done"], check=True)
    subprocess.run(["git", "-C", str(root), "remote", "add", "origin", "https://example.invalid/repo.git"], check=True)
    module = cleaner()
    original = module._command

    def missing_gh(*args, **kwargs):
        if args[0] == "gh":
            raise FileNotFoundError("gh missing")
        return original(*args, **kwargs)

    monkeypatch.setattr(module, "_command", missing_gh)
    report = module.plan(root)
    row = next(row for row in report["rows"] if row["path"] == ".worktrees/lane-done")
    assert row["verdict"] == "delete"
    assert any("GitHub" in warning for warning in report["warnings"])


def test_worktree_liveness_probe_does_not_walk_large_tree(tmp_path, monkeypatch):
    root = repo(tmp_path)
    target = root / ".worktrees" / "lane-done"
    target.parent.mkdir()
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", "-b", "lane/done", str(target)], check=True)
    (target / "work.txt").write_text("done\n")
    subprocess.run(["git", "-C", str(target), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(target), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--no-ff", "-qm", "merge lane", "lane/done"], check=True)
    module = cleaner()
    original = module._command
    probes = []

    def observe(*args, **kwargs):
        if args[0] == "lsof":
            probes.append(args)
            return subprocess.CompletedProcess(args, 0, "p123\nn/elsewhere\n", "")
        return original(*args, **kwargs)

    monkeypatch.setattr(module, "_command", observe)
    row = next(row for row in module.plan(root, pr_bodies=[])["rows"] if row["path"] == ".worktrees/lane-done")
    assert row["verdict"] == "delete"
    assert probes and all("+D" not in probe for probe in probes)


def test_terminal_attempt_pgid_and_server_host_pid_do_not_pin_run(tmp_path, monkeypatch):
    root = repo(tmp_path)
    run = root / ".agent-run" / "runs" / "20260801-1200-dispatch-old-a1b2c3"
    attempt = run / "tasks" / "one" / "attempt-001"
    attempt.mkdir(parents=True)
    (run / "RUN_RECEIPT.json").write_text('{"status":"ok"}\n')
    (run / "_owner").mkdir()
    (run / "_owner" / "dispatch-owner.json").write_text('{"host_pid":123}\n')
    (attempt / "attempt.json").write_text('{"state":"terminal","pgid":456}\n')
    old(run)
    module = cleaner()
    monkeypatch.setattr(module, "_pid_alive", lambda pid, started_at: pid is not None)
    row = next(row for row in module.plan(root, pr_bodies=[])["rows"] if row["path"].endswith(run.name))
    assert row["verdict"] == "delete"


def test_clean_output_hides_internal_identity_and_summarises_kept_rows(tmp_path):
    root = repo(tmp_path)
    active = root / ".agent-run" / "runs" / "20260923-1200-dispatch-active-a1b2c3"
    active.mkdir(parents=True)
    (active / "RUN_RECEIPT.json").write_text('{"status":"input_required"}\n')
    json_result = subprocess.run(["python3", str(SCRIPT), "--repo", str(root), "--json"],
                                 text=True, capture_output=True, check=False)
    assert json_result.returncode == 0, json_result.stderr
    assert all("_identity" not in row for row in json.loads(json_result.stdout)["rows"])
    text_result = subprocess.run(["python3", str(SCRIPT), "--repo", str(root)],
                                 text=True, capture_output=True, check=False)
    assert text_result.returncode == 0, text_result.stderr
    assert "kept: 1 paths" in text_result.stdout
    assert active.name not in text_result.stdout


def test_worktree_apply_requires_explicit_authority_attestation(tmp_path):
    root = repo(tmp_path)
    target = root / ".worktrees" / "lane-done"
    target.parent.mkdir()
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-q", "-b", "lane/done", str(target)], check=True)
    (target / "work.txt").write_text("done\n")
    subprocess.run(["git", "-C", str(target), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(target), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--no-ff", "-qm", "merge lane", "lane/done"], check=True)
    module = cleaner()
    proposal = module.plan(root, pr_bodies=[])
    assert next(row for row in proposal["rows"] if row["path"] == ".worktrees/lane-done")["verdict"] == "delete"
    try:
        module.apply(root, proposal["plan_sha256"], pr_bodies=[])
    except module.CleanError as exc:
        assert "human-authorised" in str(exc)
    else:
        raise AssertionError("worktree removal was authorised by the cleaner itself")
    assert target.exists()
    assert module.apply(root, proposal["plan_sha256"], pr_bodies=[], human_authorised=True) == [".worktrees/lane-done"]
    assert not target.exists()


def test_squash_merge_proof_uses_pr_changed_paths(tmp_path, monkeypatch):
    root = repo(tmp_path)
    subprocess.run(["git", "-C", str(root), "switch", "-q", "-c", "lane/squashed"], check=True)
    (root / "work.txt").write_text("work\n")
    subprocess.run(["git", "-C", str(root), "add", "work.txt"], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-qm", "work"], check=True)
    subprocess.run(["git", "-C", str(root), "switch", "-q", "-"], check=True)
    subprocess.run(["git", "-C", str(root), "merge", "--squash", "lane/squashed"], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(root), "commit", "-qm", "squash"], check=True)
    (root / "unrelated.txt").write_text("later\n")
    subprocess.run(["git", "-C", str(root), "add", "unrelated.txt"], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-qm", "later"], check=True)
    module = cleaner()
    command = module._command

    def fake_command(*argv, cwd=None):
        if argv[:3] == ("gh", "pr", "list"):
            return subprocess.CompletedProcess(argv, 0, '[{"number":42,"state":"MERGED"}]', "")
        if argv[:3] == ("gh", "pr", "view"):
            return subprocess.CompletedProcess(argv, 0, '{"files":[{"path":"work.txt"}]}', "")
        return command(*argv, cwd=cwd)

    monkeypatch.setattr(module, "_command", fake_command)
    assert module._merged(root, "lane/squashed")
