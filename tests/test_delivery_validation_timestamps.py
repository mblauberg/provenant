from datetime import datetime, timedelta, timezone
import importlib.util
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR_PATH = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"
REFERENCE_RUNS_PATH = ROOT / "skills" / "deliver" / "scripts" / "reference_runs.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def receipt():
    return load(REFERENCE_RUNS_PATH, "future_timestamp_reference_runs").make_reference_run(
        "software", ROOT,
    )


def test_validator_rejects_future_dated_state_history():
    validator = load(VALIDATOR_PATH, "validate_delivery_future_history")
    candidate = receipt()
    candidate["state_history"][-1]["at"] = (
        datetime.now(timezone.utc) + timedelta(minutes=10)
    ).isoformat().replace("+00:00", "Z")

    with pytest.raises(validator.Invalid, match="future"):
        validator.validate(candidate, ROOT)


def test_validator_rejects_future_dated_evidence():
    validator = load(VALIDATOR_PATH, "validate_delivery_future_evidence")
    candidate = receipt()
    candidate["evidence"][0]["started_at"] = (
        datetime.now(timezone.utc) + timedelta(minutes=10)
    ).isoformat().replace("+00:00", "Z")

    with pytest.raises(validator.Invalid, match="future"):
        validator.validate(candidate, ROOT)


def test_validator_accepts_timestamp_within_future_tolerance():
    validator = load(VALIDATOR_PATH, "validate_delivery_future_boundary")
    candidate = receipt()
    candidate["evidence"][0]["recorded_at"] = (
        datetime.now(timezone.utc) + timedelta(minutes=5, seconds=-1)
    ).isoformat().replace("+00:00", "Z")

    validator.validate(candidate, ROOT)
