"""The run-state invariants a delivery-run receipt must satisfy on every write.

`ensure_immutable_risk` and `ensure_run_open` are enforced by two skills, not
one. The `deliver` receipt producer applies them around every mutation, and the
`implement` checkpoint writer applies them before it advances the recovery
checkpoint. They used to live in `skills/deliver/scripts/delivery_receipt.py`,
which meant `implement` reached across a skill boundary to import them; the
alternative, restating them in both skills, is how two enforcement points drift
apart. They live here so there is exactly one definition of each (#755).

`ReceiptError` moves with them, and that is the point of the arrangement rather
than a detail of it. Two classes of the same name would mean an `except
ReceiptError` in one caller silently ceasing to catch the other caller's
refusal, so this module must never be loaded by file under a private name
alongside a normal import. Both consumers put the skills root on `sys.path` and
import it as `_shared.delivery_run_invariants`, which keeps it one class object.

What stays with the producer is everything that is about writing a receipt
rather than about the invariants: bundling, locking and atomic writes, evidence
execution, gate binding, and the CLI.
"""

from __future__ import annotations

import hashlib
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import _shared.workspace_paths as paths
from _shared.roots import product_root
from _shared.run_gates import run_closed

RISK_POLICY_PATH = product_root() / "config" / "risk-policy.json"
RISKS = ("routine", "substantial", "crucial", "terminal")


class ReceiptError(ValueError):
    """A refused producer operation."""


def digest_bytes(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def _utc(value: Any, field: str) -> datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise ReceiptError(f"{field} must be a UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value[:-1])
    except ValueError as exc:
        raise ReceiptError(f"{field} must be an ISO UTC timestamp") from exc
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _reject_future_timestamp(value: Any, field: str) -> None:
    if _utc(value, field) > datetime.now(timezone.utc) + timedelta(minutes=5):
        raise ReceiptError(f"{field} exceeds the future timestamp tolerance")


def safe_workspace_path(workspace: Path, value: str, field: str) -> tuple[Path, str]:
    return paths.safe_workspace_path(workspace, value, field, ReceiptError)


def ensure_allowed_artifact_target(
    run: dict[str, Any], workspace: Path, target: Path,
) -> None:
    paths.ensure_within_scope(run, workspace, target, "artifact", ReceiptError)


def load_risk_policy() -> dict[str, Any]:
    try:
        policy = json.loads(RISK_POLICY_PATH.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise ReceiptError(f"risk policy is unreadable: {exc}") from exc
    if (
        not isinstance(policy, dict)
        or policy.get("tier_order") != list(RISKS)
        or not isinstance(policy.get("factors"), dict)
    ):
        raise ReceiptError("risk policy is invalid")
    return policy


def derive_risk(assessment: dict[str, Any]) -> str:
    policy = load_risk_policy()
    factors = policy["factors"]
    if set(assessment) != set(factors):
        raise ReceiptError("risk-assessment must cover every policy factor")
    index = 0
    for factor, mappings in factors.items():
        selected = assessment.get(factor)
        if selected not in mappings:
            raise ReceiptError(f"risk-assessment.{factor} is invalid")
        index = max(index, RISKS.index(mappings[selected]))
    return RISKS[index]


def validate_override(value: dict[str, Any], derived: str) -> None:
    if (
        value.get("status") != "approved"
        or not value.get("approved_by")
        or not value.get("evidence")
        or not value.get("reason")
    ):
        raise ReceiptError(
            f"risk tier below derived {derived} requires an approved human override"
        )


def ensure_immutable_risk(run: dict[str, Any], workspace: Path) -> None:
    initial = run.get("initial_risk_tier")
    if initial not in RISKS or run.get("risk_tier") != initial:
        raise ReceiptError("risk tier is immutable after init")
    derived = derive_risk(run.get("risk_assessment", {}))
    override = run.get("risk_override")
    if RISKS.index(initial) < RISKS.index(derived):
        if not isinstance(override, dict):
            raise ReceiptError("approved human risk override is missing")
        validate_override(override, derived)
    if isinstance(override, dict) and override.get("status") == "approved":
        validate_override(override, derived)
        linked = [
            item for item in run.get("evidence", [])
            if isinstance(item, dict)
            and item.get("id") == override.get("evidence")
            and item.get("kind") == "human"
            and item.get("status") == "pass"
            and item.get("gate") == "risk-override"
        ]
        if len(linked) != 1:
            raise ReceiptError("approved human risk override evidence is missing")
        artifact_id = linked[0].get("artifact_id")
        artifacts = [
            item for item in run.get("artifacts", [])
            if isinstance(item, dict) and item.get("id") == artifact_id
        ]
        if len(artifacts) != 1 or not artifacts[0].get("path"):
            raise ReceiptError("approved human risk override artifact is missing")
        target, _relative = safe_workspace_path(
            workspace, artifacts[0]["path"], "risk override artifact",
        )
        ensure_allowed_artifact_target(run, workspace, target)
        raw = target.read_bytes() if target.is_file() else b""
        if not raw or artifacts[0].get("digest") != digest_bytes(raw):
            raise ReceiptError(
                "risk override artifact digest does not match live bytes"
            )
    corrections = run.get("human_corrections")
    if not isinstance(corrections, list):
        raise ReceiptError("human_corrections must be a list")
    for index, correction in enumerate(corrections):
        if not isinstance(correction, dict):
            raise ReceiptError(f"human_corrections[{index}] must be an object")
        _reject_future_timestamp(
            correction.get("at"), f"human_corrections[{index}].at",
        )


def ensure_run_open(run: dict[str, Any]) -> None:
    if run_closed(run):
        raise ReceiptError("closed run is immutable")
