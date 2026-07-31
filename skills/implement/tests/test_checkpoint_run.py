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
    path = tmp_path / "RUN.json"
    path.write_text(json.dumps({"schema_version": 1, "contract": "delivery-run", "checkpoint": {"generation": 0, "artifact_paths": ["RUN.json"]}}))
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
