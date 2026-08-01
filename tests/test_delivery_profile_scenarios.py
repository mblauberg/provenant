import importlib.util
from pathlib import Path

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "validate_delivery_scenarios.py"
DATASET = ROOT / "evals" / "delivery-profile-scenarios.yaml"


def load_module():
    spec = importlib.util.spec_from_file_location("validate_delivery_scenarios", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def test_held_out_dataset_covers_positive_negative_and_boundary_cases():
    data = yaml.safe_load(DATASET.read_text())
    cases = data["cases"]
    profiles = {"software", "research", "analysis", "document", "agent-product"}

    assert data["thresholds"] == {
        "minimum_expectation_match_rate": 1.0,
        "minimum_cases_per_profile": 2,
        "minimum_high_stakes_cases": 2,
    }
    assert {case["profile"] for case in cases} == profiles
    assert {case["case_type"] for case in cases} == {"positive", "negative", "boundary"}
    for profile in profiles:
        profile_cases = [case for case in cases if case["profile"] == profile]
        assert any(case["expected"] == "pass" for case in profile_cases)
        assert any(case["expected"] == "fail" for case in profile_cases)
    assert sum(bool(case["high_stakes"]) for case in cases) >= 2
    assert any(case["repetitions"] > 1 for case in cases)
    agent_cases = [case for case in cases if case["profile"] == "agent-product"]
    assert any("evaluation_id" in case.get("expected_error", "") for case in agent_cases)
    assert any("digest mismatch" in case.get("expected_error", "") for case in agent_cases)
    assert any(case.get("stochastic") is False and case["expected"] == "pass" for case in agent_cases)
    assert any(case.get("stochastic", True) is True and case["expected"] == "pass" for case in agent_cases)
    assert all("/repetitions" not in patch.get("path", "") for case in cases for patch in case.get("patches", []))


def test_held_out_scenarios_meet_the_explicit_pass_threshold():
    report = load_module().validate(DATASET)
    assert report["matched"] == report["attempted"]
    assert report["match_rate"] == 1.0


def test_scenario_expectation_tampering_fails_closed(tmp_path):
    data = yaml.safe_load(DATASET.read_text())
    negative = next(case for case in data["cases"] if case["expected"] == "fail")
    negative["expected_error"] = "an error the kernel does not emit"
    path = tmp_path / "scenarios.yaml"
    path.write_text(yaml.safe_dump(data, sort_keys=False))

    with pytest.raises(ValueError, match="expectation mismatch"):
        load_module().validate(path)


def test_dataset_rejects_a_threshold_below_full_expectation_match(tmp_path):
    data = yaml.safe_load(DATASET.read_text())
    data["thresholds"]["minimum_expectation_match_rate"] = 0.8
    path = tmp_path / "scenarios.yaml"
    path.write_text(yaml.safe_dump(data, sort_keys=False))

    with pytest.raises(ValueError, match="must be 1.0"):
        load_module().validate(path)


@pytest.mark.parametrize("invalid", ["false", [], 0])
def test_dataset_rejects_non_boolean_stochastic_override(tmp_path, invalid):
    data = yaml.safe_load(DATASET.read_text())
    case = next(item for item in data["cases"] if item["profile"] == "agent-product")
    case["stochastic"] = invalid
    path = tmp_path / "scenarios.yaml"
    path.write_text(yaml.safe_dump(data, sort_keys=False))

    with pytest.raises(ValueError, match="stochastic must be boolean"):
        load_module().validate(path)


def test_evaluator_does_not_import_the_production_reference_generator():
    source = SCRIPT.read_text()
    assert "reference_runs" not in source
    assert "make_reference_run" not in source
    assert "delivery_receipt" in source
    assert "_compile_receipt" not in source
    assert "verify_hashes=True" in source
    assert "materialise_reference_run" in source
    assert "legacy-reference-v1" not in (ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt_reference.py").read_text()


def test_fabric_delivery_fixture_delegates_run_writes_to_the_producer():
    source = (ROOT / "runtime" / "agent-fabric" / "tests" / "support" / "delivery-run-fixture.ts").read_text()
    assert "delivery_receipt.py" in source
    assert "writeFile(runReceiptPath" not in source
