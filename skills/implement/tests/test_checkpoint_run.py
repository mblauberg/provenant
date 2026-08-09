import hashlib
import importlib.util
import json
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "checkpoint_run.py"
SPEC = importlib.util.spec_from_file_location("checkpoint_run", SCRIPT)
checkpoint_run = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = checkpoint_run
SPEC.loader.exec_module(checkpoint_run)


def run_payload():
    return {
        "schema_version": 1,
        "contract": "delivery-run",
        "status": "draft",
        "risk_tier": "routine",
        "risk_assessment": {
            "blast_radius": "local",
            "reversibility": "easy",
            "data_sensitivity": "public",
            "migration": "none",
            "oracle_quality": "strong",
            "external_effects": "none",
            "critical_surface": "none",
        },
        "risk_override": {
            "status": "not-required",
            "approved_by": "",
            "evidence": "",
            "reason": "",
        },
        "state_history": [{"state": "draft", "risk_tier": "routine"}],
        "artifacts": [],
        "evidence": [],
        "human_corrections": [],
        "checkpoint": {"generation": 0, "artifact_paths": ["RUN.json"]},
    }


def make_run(tmp_path):
    path = tmp_path / "RUN.json"
    path.write_text(json.dumps(run_payload()))
    return path


def test_checkpoint_updates_atomically_and_verifies(tmp_path):
    run = make_run(tmp_path)
    artifact = tmp_path / "review.md"
    artifact.write_text("review")
    result = checkpoint_run.update(run, "review", "verify", ["reviewer-1"], ["review.md"])
    assert result["verified"] is True
    data = json.loads(run.read_text())
    assert data["checkpoint"]["generation"] == 1
    assert data["checkpoint"]["artifact_paths"] == ["RUN.json", "review.md"]
    assert (tmp_path / ".RUN.lock").is_file()


def test_checkpoint_rejects_missing_or_escaping_artifacts(tmp_path):
    run = make_run(tmp_path)
    for artifact in ("missing.md", "../outside.md"):
        try:
            checkpoint_run.update(run, "review", "verify", [], [artifact])
        except ValueError:
            pass
        else:
            raise AssertionError("unsafe artifact path accepted")


def test_checkpoint_accepts_producer_workspace_relative_paths(tmp_path):
    workspace_artifact = tmp_path / "intent.md"
    workspace_artifact.write_text("# Intent\n")
    run_dir = tmp_path / ".agent-run" / "DEL-TEST"
    run_dir.mkdir(parents=True)
    run = run_dir / "RUN.json"
    receipt = run_payload()
    receipt["checkpoint"] = {
        "generation": 1,
        "artifact_paths": ["RUN.json", "intent.md"],
    }
    run.write_text(json.dumps(receipt))

    result = checkpoint_run.update(run, "review", "verify", [], [])

    assert result["verified"] is True
    assert json.loads(run.read_text())["checkpoint"]["artifact_paths"] == [
        "RUN.json", "intent.md",
    ]


def test_checkpoint_rejects_live_absolute_and_parent_paths(tmp_path):
    run = make_run(tmp_path)
    local = tmp_path / "review.md"
    local.write_text("# Review\n")
    parent = tmp_path.parent / f"{tmp_path.name}-parent-artifact.md"
    parent.write_text("# Parent\n")

    for artifact in (str(local), f"../{parent.name}"):
        try:
            checkpoint_run.update(run, "review", "verify", [], [artifact])
        except ValueError as exc:
            assert "safe and workspace-relative" in str(exc)
        else:
            raise AssertionError("lexically unsafe artifact path accepted")


def test_checkpoint_refuses_closed_run(tmp_path):
    run = make_run(tmp_path)
    receipt = json.loads(run.read_text())
    receipt["status"] = "closed"
    run.write_text(json.dumps(receipt))

    try:
        checkpoint_run.update(run, "post-close", "should refuse", ["late-work"], [])
    except ValueError as exc:
        assert "closed run is immutable" in str(exc)
    else:
        raise AssertionError("closed run accepted a checkpoint mutation")

    assert json.loads(run.read_text())["checkpoint"]["generation"] == 0


def test_checkpoint_rechecks_live_risk_override_artifact(tmp_path):
    run = make_run(tmp_path)
    approval = tmp_path / "risk-approval.json"
    approval.write_text('{"approved":true}\n')
    receipt = json.loads(run.read_text())
    receipt["risk_assessment"]["critical_surface"] = "build-release-gate"
    receipt["risk_override"] = {
        "status": "approved",
        "approved_by": "human-owner",
        "evidence": "risk-override-approval",
        "reason": "owner accepted the bounded downgrade",
    }
    receipt["authority"] = {"allowed_artifact_paths": ["."]}
    receipt["artifacts"] = [{
        "id": "risk-override-artifact",
        "path": "risk-approval.json",
        "digest": "sha256:" + hashlib.sha256(approval.read_bytes()).hexdigest(),
    }]
    receipt["evidence"] = [{
        "id": "risk-override-approval",
        "kind": "human",
        "gate": "risk-override",
        "status": "pass",
        "artifact_id": "risk-override-artifact",
    }]
    run.write_text(json.dumps(receipt))
    approval.write_text('{"approved":false}\n')

    try:
        checkpoint_run.update(run, "review", "verify", [], [])
    except ValueError as exc:
        assert "risk override artifact digest does not match live bytes" in str(exc)
    else:
        raise AssertionError("changed risk override artifact was accepted")

    assert json.loads(run.read_text())["checkpoint"]["generation"] == 0
