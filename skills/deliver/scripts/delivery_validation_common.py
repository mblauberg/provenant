"""Shared primitives for the delivery receipt validator."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from functools import lru_cache
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
from typing import Any

SKILLS_ROOT = Path(__file__).resolve().parents[2]
ROOT = Path(os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[3])).expanduser()
sys.path.insert(0, str(SKILLS_ROOT))
from _shared.review_ladder import PRIMARY_FAMILIES, check_review_ladder
POLICY_VALIDATION_PATH = Path(__file__).with_name("delivery_policy_validation.py")
LIFECYCLE_CONTRACT_LOADER_PATH = SKILLS_ROOT / "deliver" / "contract" / "lifecycle.py"
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
SAFE_CLASSES = {"canonical", "evidence", "handoff", "scratch", "external"}
REVIEW_ROLES = {"targeted", "other-primary", "distinct-family"}
RISKS = ("routine", "substantial", "crucial", "terminal")
REPAIR_BUDGETS = {"routine": 2, "substantial": 4, "crucial": 5, "terminal": 5}
AGENTIC_RISKS = {
    "goal-hijack", "tool-misuse", "excessive-privilege", "supply-chain",
    "code-execution", "memory-context-poisoning", "insecure-inter-agent-communication",
    "cascading-failures", "human-trust-exploitation",
}
EVALUATION_BINDING_FIELDS = {
    "status", "anchored_at", "evidence_id", "evaluation_artifact_id",
    "evaluation_id", "evaluation_digest", "plan_digest",
}
class Invalid(ValueError):
    pass


@lru_cache(maxsize=1)
def _lifecycle_contract_module():
    spec = importlib.util.spec_from_file_location(
        "delivery_lifecycle_contract", LIFECYCLE_CONTRACT_LOADER_PATH,
    )
    fail(spec is None or spec.loader is None, "delivery lifecycle contract loader is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fail(condition: bool, message: str) -> None:
    if condition:
        raise Invalid(message)


LIFECYCLE_CONTRACT = _lifecycle_contract_module().LIFECYCLE_CONTRACT
LIFECYCLE_CONTRACT_DIGEST = LIFECYCLE_CONTRACT["contract_digest"]
NORMAL_STATES = tuple(LIFECYCLE_CONTRACT["states"])
SIDE_STATES = frozenset(LIFECYCLE_CONTRACT["side_states"])
TRANSITIONS = {
    state: {
        row["to_state"] for row in LIFECYCLE_CONTRACT["transitions"]
        if row["state"] == state
    }
    for state in NORMAL_STATES
}

def _mapping(value: Any, field: str) -> dict[str, Any]:
    fail(not isinstance(value, dict), f"{field} must be an object")
    return value

def _list(value: Any, field: str) -> list[Any]:
    fail(not isinstance(value, list), f"{field} must be a list")
    return value

def _utc(value: Any, field: str) -> datetime:
    fail(not isinstance(value, str) or not value.endswith("Z"), f"{field} must be a UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise Invalid(f"{field} must be an ISO UTC timestamp") from exc
    if parsed > datetime.now(timezone.utc) + timedelta(minutes=5):
        raise Invalid(f"{field} exceeds the bounded future timestamp tolerance")
    return parsed

def _digest(value: Any, field: str) -> None:
    fail(not isinstance(value, str) or not DIGEST.fullmatch(value), f"{field} must be a sha256 digest")

def _identifier(value: Any, field: str) -> str:
    fail(
        not isinstance(value, str) or not IDENTIFIER.fullmatch(value),
        f"{field} must be a bounded stable identifier",
    )
    return value

def _safe_path(value: Any, field: str) -> str:
    fail(not isinstance(value, str) or not value, f"{field} must be a non-empty path")
    path = Path(value)
    fail(path.is_absolute() or ".." in path.parts, f"{field} must be safe and relative")
    return path.as_posix().rstrip("/")

def _inside(path: str, scope: str) -> bool:
    return scope in {"", "."} or path == scope or path.startswith(scope + "/")

@lru_cache(maxsize=1)
def _policy_validation_module():
    spec = importlib.util.spec_from_file_location(
        "delivery_policy_validation", POLICY_VALIDATION_PATH,
    )
    fail(spec is None or spec.loader is None, "delivery policy validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

def _retrospect_validator():
    path = SKILLS_ROOT / "retrospect" / "scripts" / "validate_retrospect.py"
    spec = importlib.util.spec_from_file_location("delivery_retrospect_validator", path)
    fail(not spec or not spec.loader, "retrospective validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

def _evaluate_validator():
    path = SKILLS_ROOT / "evaluate" / "scripts" / "validate_evaluation.py"
    spec = importlib.util.spec_from_file_location("delivery_evaluate_validator", path)
    fail(not spec or not spec.loader, "evaluation validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    fail(not callable(getattr(module, "validate", None)), "evaluation validator API is unavailable")
    return module

@lru_cache(maxsize=1)
def _software_delivery_validator():
    spec = importlib.util.spec_from_file_location("software_delivery_validation", Path(__file__).with_name("software_delivery_validation.py"))
    fail(not spec or not spec.loader, "software delivery validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_bound_json(raw: bytes, field: str) -> dict[str, Any]:
    def no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            fail(key in result, f"{field} contains duplicate JSON key: {key}")
            result[key] = value
        return result

    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=no_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise Invalid(f"{field} is not readable JSON: {exc}") from exc
    fail(not isinstance(value, dict), f"{field} root must be an object")
    return value
