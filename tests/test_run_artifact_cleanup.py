import hashlib
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "session" / "scripts" / "cleanup_run_artifacts.py"
NOW = datetime.now(timezone.utc).replace(microsecond=0)


def test_workspace_inference_handles_canonical_run_root(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    canonical = tmp_path / ".agent-run" / "runs" / f"{NOW:%Y%m%d-%H%M}-delivery-task-a1b2c3"
    assert module.infer_workspace_root(canonical) == tmp_path


def test_minimal_fabricated_receipt_cannot_authorise_cleanup(tmp_path):
    module = load_script(SCRIPT, "cleanup_run_artifacts")
    run = tmp_path / ".agent-run" / "runs" / f"{NOW:%Y%m%d-%H%M}-delivery-fake-a1b2c3"
    run.mkdir(parents=True)
    (run / "user.txt").write_text("keep")
    (run / "RUN.json").write_text(json.dumps({"schema_version": 1, "contract": "delivery-run", "run_id": "fake", "status": "closed", "artifacts": []}))
    with pytest.raises(module.CleanupError, match="valid terminal delivery"):
        module.cleanup(run / "RUN.json", workspace_root=run.parent, now=NOW)
