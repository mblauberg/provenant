"""Load the checked delivery lifecycle contract for every consumer."""

from __future__ import annotations

from copy import deepcopy
import hashlib
import json
from pathlib import Path
from typing import Any


CONTRACT_PATH = Path(__file__).with_name("lifecycle.v1.json")


class LifecycleContractError(ValueError):
    """The checked lifecycle contract is unreadable or malformed."""


def _load_json(raw: bytes) -> dict[str, Any]:
    def no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise LifecycleContractError(f"duplicate JSON key: {key}")
            result[key] = value
        return result

    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=no_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise LifecycleContractError(f"lifecycle contract is not readable JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise LifecycleContractError("lifecycle contract root must be an object")
    return value


def _validate_contract(value: dict[str, Any]) -> None:
    if value.get("schema_version") != 1 or value.get("contract") != "delivery-lifecycle":
        raise LifecycleContractError("lifecycle contract must use schema_version 1")
    states = value.get("states")
    if not isinstance(states, list) or not states or any(not isinstance(item, str) for item in states):
        raise LifecycleContractError("lifecycle contract states must be non-empty strings")
    side_states = value.get("side_states")
    if not isinstance(side_states, list) or any(not isinstance(item, str) for item in side_states):
        raise LifecycleContractError("lifecycle contract side_states must be strings")
    rows = value.get("transitions")
    if not isinstance(rows, list) or not rows:
        raise LifecycleContractError("lifecycle contract transitions must be non-empty")
    seen: set[str] = set()
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise LifecycleContractError(f"transition row {index} must be an object")
        required = {
            "state", "transition", "to_state", "required_evidence_kinds",
            "permitted_writer", "freshness_rule",
        }
        if not required <= set(row):
            missing = sorted(required - set(row))
            raise LifecycleContractError(f"transition row {index} is missing {missing}")
        state = row["state"]
        to_state = row["to_state"]
        if state not in states or to_state not in states:
            raise LifecycleContractError(f"transition row {index} names an unknown state")
        if row["transition"] != f"{state} -> {to_state}":
            raise LifecycleContractError(f"transition row {index} has an inconsistent transition label")
        if row["transition"] in seen:
            raise LifecycleContractError(f"duplicate transition row: {row['transition']}")
        seen.add(row["transition"])
        if not isinstance(row["required_evidence_kinds"], list):
            raise LifecycleContractError(f"transition row {index} evidence kinds must be a list")
        if not isinstance(row["permitted_writer"], str) or not row["permitted_writer"]:
            raise LifecycleContractError(f"transition row {index} permitted_writer is required")
        if not isinstance(row["freshness_rule"], dict) or not row["freshness_rule"].get("mode"):
            raise LifecycleContractError(f"transition row {index} freshness_rule is required")
    invariants = value.get("invariants")
    expected_invariants = {
        "evidence_freshness_after_repair", "approval_artifact",
        "amended_artifact_redigest", "artifact_content_inspection", "closed_gate",
    }
    if not isinstance(invariants, dict) or not expected_invariants <= set(invariants):
        raise LifecycleContractError("lifecycle contract must name all five lifecycle invariants")


def load_lifecycle_contract() -> dict[str, Any]:
    """Return a validated contract copy with its exact file digest attached."""
    try:
        raw = CONTRACT_PATH.read_bytes()
    except OSError as exc:
        raise LifecycleContractError(f"cannot read {CONTRACT_PATH}: {exc}") from exc
    value = _load_json(raw)
    _validate_contract(value)
    result = deepcopy(value)
    result["contract_digest"] = "sha256:" + hashlib.sha256(raw).hexdigest()
    return result


LIFECYCLE_CONTRACT = load_lifecycle_contract()
LIFECYCLE_CONTRACT_DIGEST = LIFECYCLE_CONTRACT["contract_digest"]

