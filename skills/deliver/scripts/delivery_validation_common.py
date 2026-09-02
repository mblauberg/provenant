"""Shared primitives for the delivery receipt validator."""

from __future__ import annotations

from datetime import datetime
from functools import lru_cache
import importlib.util
import json
from pathlib import Path
import re
from typing import Any

SKILLS_ROOT = Path(__file__).resolve().parents[2]
# `skills/_shared/roots.py` is the single resolver for the product root (#754).
# The fallback loads that one file when this script is run directly by path and
# the product root is not on `sys.path`: it locates the resolver, it does not
# decide the root, and it leaves import resolution untouched (#755).
try:
    from _shared.roots import product_root
except ModuleNotFoundError:  # pragma: no cover - direct invocation by path
    import importlib.util as _roots_util
    _roots_spec = _roots_util.spec_from_file_location(
        "provenant_roots", Path(__file__).resolve().parents[2] / "_shared" / "roots.py"
    )
    _roots_module = _roots_util.module_from_spec(_roots_spec)
    _roots_spec.loader.exec_module(_roots_module)
    product_root = _roots_module.product_root

ROOT = product_root()
# The review ladder is loaded the same way as the resolver above: by import
# when the skills root is already an import root, and by file otherwise. This
# module is a library, imported by the validator coordinator and by callers
# outside `deliver`, so putting the whole skills catalogue on the process-wide
# import path as an import side effect would hand every one of them a root
# they never asked for (#755). `review_ladder` carries constants and pure
# functions only and no package-relative import, so loading it by file cannot
# split a type identity across the two paths.
try:
    from _shared.review_ladder import PRIMARY_FAMILIES, check_review_ladder
except ModuleNotFoundError:  # pragma: no cover - direct invocation by path
    _ladder_spec = importlib.util.spec_from_file_location(
        "provenant_review_ladder", SKILLS_ROOT / "_shared" / "review_ladder.py"
    )
    _ladder = importlib.util.module_from_spec(_ladder_spec)
    _ladder_spec.loader.exec_module(_ladder)
    PRIMARY_FAMILIES = _ladder.PRIMARY_FAMILIES
    check_review_ladder = _ladder.check_review_ladder
POLICY_VALIDATION_PATH = Path(__file__).with_name("delivery_policy_validation.py")
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


def fail(condition: bool, message: str) -> None:
    if condition:
        raise Invalid(message)


def _mapping(value: Any, field: str) -> dict[str, Any]:
    fail(not isinstance(value, dict), f"{field} must be an object")
    return value

def _list(value: Any, field: str) -> list[Any]:
    fail(not isinstance(value, list), f"{field} must be a list")
    return value

def _utc(value: Any, field: str) -> datetime:
    fail(not isinstance(value, str) or not value.endswith("Z"), f"{field} must be a UTC timestamp")
    try:
        return datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise Invalid(f"{field} must be an ISO UTC timestamp") from exc

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
