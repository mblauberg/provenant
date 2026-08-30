import hashlib
from collections.abc import Mapping

import pytest
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
    assert validator.LIFECYCLE_CONTRACT is validator._lifecycle_contract_module().LIFECYCLE_CONTRACT
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


def test_lifecycle_contract_rejects_top_level_mutation():
    contract = load(LOADER_PATH, "lifecycle_contract_top_level_mutation").LIFECYCLE_CONTRACT
    with pytest.raises(TypeError):
        contract["contract_digest"] = "sha256:drift"


def test_lifecycle_contract_rejects_nested_transition_mutation():
    contract = load(LOADER_PATH, "lifecycle_contract_transition_mutation").LIFECYCLE_CONTRACT
    with pytest.raises(TypeError):
        contract["transitions"][0]["to_state"] = "closed"


def test_lifecycle_contract_rejects_nested_evidence_list_mutation():
    contract = load(LOADER_PATH, "lifecycle_contract_evidence_mutation").LIFECYCLE_CONTRACT
    with pytest.raises(TypeError):
        contract["transitions"][4]["required_evidence_kinds"][0] = "tampered"


def test_lifecycle_contract_preserves_the_pinned_state_graph_and_five_invariants():
    contract = load(LOADER_PATH, "lifecycle_contract_shape").LIFECYCLE_CONTRACT
    assert contract["source"]["state_graph"] == "docs/specs/harness/lifecycle.md#state-graph"
    assert list(contract["states"]) == [
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


def contract_module():
    return load(LOADER_PATH, "lifecycle_contract_validation")


def mutated(**changes):
    module = contract_module()
    def mutable(item):
        if isinstance(item, Mapping):
            return {key: mutable(value) for key, value in item.items()}
        if isinstance(item, tuple):
            return [mutable(value) for value in item]
        return item

    value = mutable(module.LIFECYCLE_CONTRACT)
    value.pop("contract_digest")
    value.update(changes)
    return module, value


def test_the_loader_rejects_a_contract_that_names_a_state_twice():
    module, value = mutated()
    value["states"] = [*value["states"], "closed"]
    with pytest.raises(module.LifecycleContractError, match="names .* twice|duplicate state"):
        module._validate_contract(value)


def test_the_loader_rejects_terminal_states_that_are_not_states():
    module, value = mutated(terminal_states=["closed", "abandoned"])
    with pytest.raises(module.LifecycleContractError, match="terminal_states"):
        module._validate_contract(value)


def test_the_loader_rejects_terminal_states_that_disagree_with_the_transition_graph():
    # `accepted` has an outgoing row, so calling it terminal is a contract that
    # contradicts itself. Nothing read this field before, so it could drift
    # freely inside the very artifact that exists to stop drift.
    module, value = mutated(terminal_states=["closed", "accepted"])
    with pytest.raises(module.LifecycleContractError, match="terminal_states"):
        module._validate_contract(value)
