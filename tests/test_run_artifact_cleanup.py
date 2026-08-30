import importlib.util
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "session" / "scripts" / "cleanup_run_artifacts.py"


def load_module():
    spec = importlib.util.spec_from_file_location("cleanup_run_artifacts", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def make_run(tmp_path):
    run = tmp_path / "run"
    run.mkdir()
    ref_path = ROOT / "skills" / "deliver" / "scripts" / "reference_runs.py"
    spec = importlib.util.spec_from_file_location("cleanup_reference", ref_path)
    reference = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(reference)
    receipt = reference.make_reference_run("research", ROOT)
    assert not any(key.startswith("cost:") for key in receipt["authority"]["budget"])
    receipt["run_id"] = "CLEAN-1"
    receipt["fabric_relationships"]["delivery_run_id"] = "CLEAN-1"
    receipt["authority"]["allowed_artifact_paths"] = ["run"]
    receipt["authority"]["allowed_source_paths"] = ["input", "run"]
    receipt["intent"]["artifact"] = "run/intent.md"
    receipt["artifacts"][0]["path"] = "run/intent.md"
    receipt["artifacts"][1]["path"] = "run/evidence.json"
    for artifact in receipt["artifacts"]:
        if artifact["id"].startswith("review-route-"):
            route_path = f"run/{artifact['path']}"
            artifact["path"] = route_path
            for evidence in receipt["evidence"]:
                route_receipt = evidence.get("route_receipt")
                if route_receipt and route_receipt.get("path") == route_path.removeprefix("run/"):
                    route_receipt["path"] = route_path
                if route_path.removeprefix("run/") in evidence.get("source_paths", []):
                    evidence["source_paths"].remove(route_path.removeprefix("run/"))
                    evidence["source_paths"].append(route_path)
    receipt["status"] = "closed"
    receipt["checkpoint"].update({"current_slice": "closed", "next_action": "authorised cleanup", "in_flight": []})
    receipt["human_gates"]["acceptance"] = {"status": "approved", "approver": "human", "evidence": "acceptance-approval"}
    receipt["human_gates"]["release"] = {"status": "approved", "approver": "human", "evidence": "release-approval"}
    receipt["evidence"].append({"id": "observation-result", "kind": "observation", "gate": "citation-audit", "status": "pass", "method": "observed comparable use", "artifact_id": "evidence-bundle", "source_paths": ["input"], "observed_at": "2026-07-10T12:00:00Z", "measured_value": 1})
    receipt["state_history"].extend([
        {"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "awaiting_release", "at": "2026-07-10T00:10:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "observing", "at": "2026-07-10T00:11:00Z", "evidence_ids": ["release-approval"]},
        {"state": "closed", "at": "2026-07-11T00:11:00Z", "evidence_ids": ["observation-result"]},
    ])
    receipt["observation"].update({"status": "pass", "started_at": "2026-07-10T00:11:00Z", "ended_at": "2026-07-11T00:11:00Z", "observed_events": 1, "evidence_ids": ["observation-result"]})
    (run / "scratch.tmp").write_text("remove")
    (run / "intent.md").write_text("intent")
    (run / "evidence.json").write_text("keep")
    (run / "unknown.tmp").write_text("keep unknown")
    receipt["artifacts"].append({"id": "scratch", "path": "run/scratch.tmp", "media_type": "text/plain", "artifact_type": "scratch", "digest": "sha256:" + hashlib.sha256(b"remove").hexdigest(), "class": "scratch", "owner": "CLEAN-1", "retention": "until-expiry", "expires_at": "2026-01-01T00:00:00Z"})
    (run / "RUN.json").write_text(json.dumps(receipt))
    return run


def test_plan_is_read_only_and_names_only_expired_manifest_owned_scratch(tmp_path):
    module = load_module()
    run = make_run(tmp_path)
    receipt = module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))
    assert receipt["mode"] == "plan"
    assert receipt["eligible"] == ["run/scratch.tmp"]
    assert receipt["removed"] == []
    assert (run / "scratch.tmp").exists()
    assert (run / "unknown.tmp").exists()


def test_execute_requires_explicit_authority_and_never_removes_unknown_or_evidence(tmp_path):
    module = load_module()
    run = make_run(tmp_path)
    with pytest.raises(module.CleanupError, match="explicit cleanup authority"):
        module.cleanup(run / "RUN.json", workspace_root=tmp_path, execute=True, now=datetime(2026, 7, 10, tzinfo=timezone.utc))
    plan = module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))
    receipt = module.cleanup(
        run / "RUN.json",
        workspace_root=tmp_path,
        execute=True,
        authorised_by="human",
        authority_evidence="direct-instruction",
        approved_plan_sha256=plan["plan_sha256"],
        now=datetime(2026, 7, 10, tzinfo=timezone.utc),
    )
    assert receipt["removed"] == ["run/scratch.tmp"]
    assert not (run / "scratch.tmp").exists()
    assert (run / "evidence.json").exists()
    assert (run / "unknown.tmp").exists()


def test_failed_worker_partial_is_preserved_when_not_manifest_owned(tmp_path):
    module = load_module()
    run = make_run(tmp_path)
    partial = run / "failed-worker.partial"
    partial.write_text("worker returned null after a possible partial write")
    plan = module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))
    receipt = module.cleanup(
        run / "RUN.json", workspace_root=tmp_path, execute=True,
        authorised_by="human", authority_evidence="direct-instruction",
        approved_plan_sha256=plan["plan_sha256"],
        now=datetime(2026, 7, 10, tzinfo=timezone.utc),
    )
    assert receipt["unknown_files_removed"] is False
    assert partial.exists()


def test_escape_paths_and_duplicate_artifact_ownership_fail_closed(tmp_path):
    module = load_module()
    run = make_run(tmp_path)
    data = json.loads((run / "RUN.json").read_text())
    data["artifacts"][-1]["path"] = "../outside.tmp"
    (run / "RUN.json").write_text(json.dumps(data))
    with pytest.raises(module.CleanupError, match="safe and relative"):
        module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))

    data["artifacts"][-1]["path"] = "run/evidence.json"
    (run / "RUN.json").write_text(json.dumps(data))
    with pytest.raises(module.CleanupError, match="duplicate artifact path"):
        module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))


def test_execute_rejects_file_changed_after_approved_plan(tmp_path):
    module = load_module()
    run = make_run(tmp_path)
    plan = module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))
    (run / "scratch.tmp").write_text("changed")
    with pytest.raises(module.CleanupError, match="digest"):
        module.cleanup(
            run / "RUN.json", workspace_root=tmp_path, execute=True,
            authorised_by="human", authority_evidence="direct-instruction",
            approved_plan_sha256=plan["plan_sha256"],
            now=datetime(2026, 7, 10, tzinfo=timezone.utc),
        )


def test_minimal_fabricated_receipt_cannot_authorise_cleanup(tmp_path):
    module = load_module()
    run = tmp_path / "fake"
    run.mkdir()
    (run / "user.txt").write_text("keep")
    (run / "RUN.json").write_text(json.dumps({"schema_version": 1, "contract": "delivery-run", "run_id": "fake", "status": "closed", "artifacts": []}))
    with pytest.raises(module.CleanupError, match="valid terminal delivery"):
        module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))


def test_execute_reports_partial_failure_instead_of_raising_traceback(tmp_path, monkeypatch):
    module = load_module()
    run = make_run(tmp_path)
    plan = module.cleanup(run / "RUN.json", workspace_root=tmp_path, now=datetime(2026, 7, 10, tzinfo=timezone.utc))
    original = Path.unlink

    def fail_unlink(path, *args, **kwargs):
        if path.name == "scratch.tmp":
            raise PermissionError("simulated race")
        return original(path, *args, **kwargs)

    monkeypatch.setattr(Path, "unlink", fail_unlink)
    receipt = module.cleanup(
        run / "RUN.json", workspace_root=tmp_path, execute=True,
        authorised_by="human", authority_evidence="direct-instruction",
        approved_plan_sha256=plan["plan_sha256"],
        now=datetime(2026, 7, 10, tzinfo=timezone.utc),
    )
    assert receipt["status"] == "fail"
    assert receipt["removed"] == []
    assert "simulated race" in receipt["error"]
    assert (run / "scratch.tmp").exists()
