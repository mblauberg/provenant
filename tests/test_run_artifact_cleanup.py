import hashlib
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "session" / "scripts" / "cleanup_run_artifacts.py"
NOW = datetime.now(timezone.utc).replace(microsecond=0)


def iso(moment):
    return moment.isoformat().replace("+00:00", "Z")


def test_workspace_inference_handles_canonical_run_root(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    canonical = tmp_path / ".agent-run" / "runs" / f"{NOW:%Y%m%d-%H%M}-delivery-task-a1b2c3"
    assert module.infer_workspace_root(canonical) == tmp_path


def test_cleanup_rejects_a_root_level_legacy_delivery_receipt(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    legacy = tmp_path / ".agent-run" / "DEL-OLD"
    legacy.mkdir(parents=True)
    receipt = legacy / "RUN.json"
    receipt.write_bytes((make_run(tmp_path) / "RUN.json").read_bytes())
    with pytest.raises(module.CleanupError, match=r"\.agent-run/runs"):
        module.cleanup(receipt, workspace_root=tmp_path)


def make_run(tmp_path, acceptance_basis=None):
    run = tmp_path / ".agent-run" / "runs" / f"{NOW:%Y%m%d-%H%M}-delivery-clean-a1b2c3"
    run.mkdir(parents=True)
    ref_path = ROOT / "skills" / "deliver" / "scripts" / "reference_runs.py"
    reference = load_script(ref_path, "cleanup_reference")
    receipt = reference.make_reference_run("research", ROOT, acceptance_basis=acceptance_basis)
    assert not any(key.startswith("cost:") for key in receipt["authority"]["budget"])
    receipt["run_id"] = "CLEAN-1"
    receipt["fabric_relationships"]["delivery_run_id"] = "CLEAN-1"
    prefix = run.name
    receipt["authority"]["allowed_artifact_paths"] = [prefix]
    receipt["authority"]["allowed_source_paths"] = ["input", prefix]
    receipt["intent"]["artifact"] = f"{prefix}/intent.md"
    receipt["artifacts"][0]["path"] = f"{prefix}/intent.md"
    receipt["artifacts"][1]["path"] = f"{prefix}/evidence.json"
    for artifact in receipt["artifacts"]:
        if artifact["id"].startswith("review-route-"):
            route_path = f"{prefix}/{artifact['path']}"
            artifact["path"] = route_path
            for evidence in receipt["evidence"]:
                route_receipt = evidence.get("route_receipt")
                if route_receipt and route_receipt.get("path") == route_path.removeprefix(f"{prefix}/"):
                    route_receipt["path"] = route_path
                if route_path.removeprefix(f"{prefix}/") in evidence.get("source_paths", []):
                    evidence["source_paths"].remove(route_path.removeprefix(f"{prefix}/"))
                    evidence["source_paths"].append(route_path)
    receipt["checkpoint"].update({"current_slice": "closed", "next_action": "authorised cleanup", "in_flight": []})
    receipt["human_gates"]["acceptance"] = {"status": "approved", "approver": "human", "evidence": "acceptance-approval"}
    if acceptance_basis in {"checks-and-review", "project-landing"}:
        receipt["human_gates"]["acceptance"] = {"status": "pending", "approver": "", "evidence": ""}
    receipt["human_gates"]["release"] = {"status": "approved", "approver": "human", "evidence": "release-approval"}
    receipt["evidence"].append({"id": "observation-result", "kind": "observation", "gate": "citation-audit", "status": "pass", "method": "observed comparable use", "artifact_id": "evidence-bundle", "source_paths": ["input"], "observed_at": iso(NOW), "measured_value": 1})
    receipt["observation"].update({"status": "pass", "started_at": iso(NOW), "ended_at": iso(NOW + timedelta(days=1)), "observed_events": 1, "evidence_ids": ["observation-result"]})
    (run / "scratch.tmp").write_text("remove")
    (run / "intent.md").write_text("intent")
    (run / "evidence.json").write_text("keep")
    (run / "unknown.tmp").write_text("keep unknown")
    receipt["artifacts"].append({"id": "scratch", "path": f"{prefix}/scratch.tmp", "media_type": "text/plain", "artifact_type": "scratch", "digest": "sha256:" + hashlib.sha256(b"remove").hexdigest(), "class": "scratch", "owner": "CLEAN-1", "retention": "until-expiry", "expires_at": iso(NOW - timedelta(days=1))})
    (run / "RUN.json").write_text(json.dumps(receipt))
    return run


@pytest.mark.parametrize("acceptance_basis", [None, "checks-and-review", "project-landing", "user"])
def test_plan_is_read_only_and_names_only_expired_manifest_owned_scratch(tmp_path, acceptance_basis):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = make_run(tmp_path, acceptance_basis)
    receipt = module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)
    assert receipt["mode"] == "plan"
    assert receipt["eligible"] == [f"{run.name}/scratch.tmp"]
    assert receipt["removed"] == []
    assert (run / "scratch.tmp").exists()
    assert (run / "unknown.tmp").exists()


def test_execute_requires_explicit_authority_and_never_removes_unknown_or_evidence(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = make_run(tmp_path)
    with pytest.raises(module.CleanupError, match="explicit cleanup authority"):
        module.cleanup(run / "RUN.json", workspace_root=run.parent, execute=True, now=NOW)
    plan = module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)
    receipt = module.cleanup(
        run / "RUN.json",
        workspace_root=run.parent,
        execute=True,
        authorised_by="human",
        authority_evidence="direct-instruction",
        approved_plan_sha256=plan["plan_sha256"],
        now=NOW,
    )
    assert receipt["removed"] == [f"{run.name}/scratch.tmp"]
    assert not (run / "scratch.tmp").exists()
    assert (run / "evidence.json").exists()
    assert (run / "unknown.tmp").exists()


def test_failed_worker_partial_is_preserved_when_not_manifest_owned(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = make_run(tmp_path)
    partial = run / "failed-worker.partial"
    partial.write_text("worker returned null after a possible partial write")
    plan = module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)
    receipt = module.cleanup(
        run / "RUN.json", workspace_root=run.parent, execute=True,
        authorised_by="human", authority_evidence="direct-instruction",
        approved_plan_sha256=plan["plan_sha256"],
        now=NOW,
    )
    assert receipt["unknown_files_removed"] is False
    assert partial.exists()


def test_escape_paths_and_duplicate_artifact_ownership_fail_closed(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = make_run(tmp_path)
    data = json.loads((run / "RUN.json").read_text())
    data["artifacts"][-1]["path"] = "../outside.tmp"
    (run / "RUN.json").write_text(json.dumps(data))
    with pytest.raises(module.CleanupError, match="safe and relative"):
        module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)

    data["artifacts"][-1]["path"] = f"{run.name}/evidence.json"
    (run / "RUN.json").write_text(json.dumps(data))
    with pytest.raises(module.CleanupError, match="duplicate artifact path"):
        module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)


def test_execute_rejects_file_changed_after_approved_plan(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = make_run(tmp_path)
    plan = module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)
    (run / "scratch.tmp").write_text("changed")
    with pytest.raises(module.CleanupError, match="digest"):
        module.cleanup(
            run / "RUN.json", workspace_root=run.parent, execute=True,
            authorised_by="human", authority_evidence="direct-instruction",
            approved_plan_sha256=plan["plan_sha256"],
            now=NOW,
        )


def test_minimal_fabricated_receipt_cannot_authorise_cleanup(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = tmp_path / ".agent-run" / "runs" / f"{NOW:%Y%m%d-%H%M}-delivery-fake-a1b2c3"
    run.mkdir(parents=True)
    (run / "user.txt").write_text("keep")
    (run / "RUN.json").write_text(json.dumps({"schema_version": 1, "contract": "delivery-run", "run_id": "fake", "status": "closed", "artifacts": []}))
    with pytest.raises(module.CleanupError, match="valid terminal delivery"):
        module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)


def test_execute_reports_partial_failure_instead_of_raising_traceback(tmp_path, monkeypatch):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = make_run(tmp_path)
    plan = module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)
    original = Path.unlink

    def fail_unlink(path, *args, **kwargs):
        if path.name == "scratch.tmp":
            raise PermissionError("simulated race")
        return original(path, *args, **kwargs)

    monkeypatch.setattr(Path, "unlink", fail_unlink)
    receipt = module.cleanup(
        run / "RUN.json", workspace_root=run.parent, execute=True,
        authorised_by="human", authority_evidence="direct-instruction",
        approved_plan_sha256=plan["plan_sha256"],
        now=NOW,
    )
    assert receipt["status"] == "fail"
    assert receipt["removed"] == []
    assert "simulated race" in receipt["error"]
    assert (run / "scratch.tmp").exists()


def test_evidence_acceptance_keeps_release_and_cleanup_authority_guards(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = make_run(tmp_path, "checks-and-review")
    with pytest.raises(module.CleanupError, match="explicit cleanup authority"):
        module.cleanup(run / "RUN.json", workspace_root=run.parent, execute=True)
    receipt = json.loads((run / "RUN.json").read_text())
    receipt["human_gates"]["release"] = {"status": "pending", "approver": "", "evidence": ""}
    receipt["observation"]["status"] = "planned"
    (run / "RUN.json").write_text(json.dumps(receipt))
    with pytest.raises(module.CleanupError, match="approved release gate"):
        module.cleanup(run / "RUN.json", workspace_root=run.parent)
