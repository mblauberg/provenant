import hashlib
import importlib.util
import os
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "skills" / "deliver" / "contract" / "lifecycle.v1.json"
LOADER_PATH = ROOT / "skills" / "deliver" / "contract" / "lifecycle.py"
VALIDATOR_PATH = Path(
    os.environ.get(
        "LIFECYCLE_VALIDATOR_PATH",
        str(ROOT / "skills" / "deliver" / "scripts" / "delivery_validation_common.py"),
    )
)


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_validator_and_producer_consume_one_digest_bound_lifecycle_contract():
    assert CONTRACT_PATH.is_file()
    assert LOADER_PATH.is_file()

    producer_contract = load(LOADER_PATH, "lifecycle_contract_producer")
    validator = load(VALIDATOR_PATH, "validate_delivery_contract_drift")
    expected_digest = "sha256:" + hashlib.sha256(CONTRACT_PATH.read_bytes()).hexdigest()

    assert producer_contract.LIFECYCLE_CONTRACT_DIGEST == expected_digest
    assert validator.LIFECYCLE_CONTRACT_DIGEST == producer_contract.LIFECYCLE_CONTRACT_DIGEST
    assert validator.LIFECYCLE_CONTRACT["contract_digest"] == expected_digest
    assert validator.LIFECYCLE_CONTRACT["transitions"] == producer_contract.LIFECYCLE_CONTRACT["transitions"]
    expected_transitions = {
        state: {
            row["to_state"]
            for row in producer_contract.LIFECYCLE_CONTRACT["transitions"]
            if row["state"] == state
        }
        for state in producer_contract.LIFECYCLE_CONTRACT["states"]
    }
    assert validator.TRANSITIONS == expected_transitions


def test_lifecycle_contract_preserves_the_pinned_state_graph_and_five_invariants():
    contract = load(LOADER_PATH, "lifecycle_contract_shape").LIFECYCLE_CONTRACT
    assert contract["states"] == [
        "draft", "scoped", "approved", "executing", "verifying", "reviewing",
        "repairing", "awaiting_acceptance", "accepted", "awaiting_release",
        "observing", "closed",
    ]
    assert {
        row["transition"] for row in contract["transitions"]
    } == {
        "draft -> scoped", "scoped -> approved", "approved -> executing",
        "executing -> verifying", "verifying -> reviewing", "verifying -> executing",
        "reviewing -> repairing", "reviewing -> awaiting_acceptance",
        "repairing -> verifying", "awaiting_acceptance -> accepted",
        "awaiting_acceptance -> repairing", "accepted -> awaiting_release",
        "awaiting_release -> observing", "observing -> closed",
    }
    assert set(contract["invariants"]) == {
        "evidence_freshness_after_repair", "approval_artifact",
        "amended_artifact_redigest", "artifact_content_inspection", "closed_gate",
    }
    assert all(
        {"state", "transition", "required_evidence_kinds", "permitted_writer", "freshness_rule"}
        <= set(row)
        for row in contract["transitions"]
    )
