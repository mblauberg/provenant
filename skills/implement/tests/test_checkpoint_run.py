import importlib.util
import json
from pathlib import Path
import sys


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "checkpoint_run.py"
SPEC = importlib.util.spec_from_file_location("checkpoint_run", SCRIPT)
checkpoint_run = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = checkpoint_run
SPEC.loader.exec_module(checkpoint_run)


def make_run(tmp_path):
    path = tmp_path / ".agent-run" / "CHECKPOINT"
    path.mkdir(parents=True)
    path = path / "RUN.json"
    path.write_text(json.dumps({
        "schema_version": 1,
        "contract": "delivery-run",
        "authority": {"allowed_artifact_paths": ["."]},
        "checkpoint": {"generation": 0, "current_slice": "draft", "next_action": "continue", "in_flight": [], "artifact_paths": ["RUN.json"]},
    }))
    return path


def test_checkpoint_updates_atomically_and_verifies(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run = make_run(tmp_path)
    artifact = tmp_path / "review.md"
    artifact.write_text("review")
    result = checkpoint_run.update(run, "review", "verify", ["reviewer-1"], ["review.md"])
    assert result["verified"] is True
    data = json.loads(run.read_text())
    assert data["checkpoint"]["generation"] == 1
    assert data["checkpoint"]["artifact_paths"] == ["RUN.json", "review.md"]


def test_checkpoint_rejects_missing_or_escaping_artifacts(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run = make_run(tmp_path)
    for artifact in ("missing.md", "../outside.md"):
        try:
            checkpoint_run.update(run, "review", "verify", [], [artifact])
        except ValueError:
            pass
        else:
            raise AssertionError("unsafe artifact path accepted")


def test_checkpoint_rejects_in_workspace_artifacts_outside_authority_scope(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run = make_run(tmp_path)
    receipt = json.loads(run.read_text())
    receipt["authority"]["allowed_artifact_paths"] = ["allowed"]
    run.write_text(json.dumps(receipt))
    (tmp_path / "other.md").write_text("outside authority")
    try:
        checkpoint_run.update(run, "review", "verify", [], ["other.md"])
    except ValueError as exc:
        assert "authority.allowed_artifact_paths" in str(exc)
    else:
        raise AssertionError("artifact outside authority scope accepted")
