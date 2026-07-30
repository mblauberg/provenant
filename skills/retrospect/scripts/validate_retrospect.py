#!/usr/bin/env python3
"""Validate evidence-linked retrospective proposals and recurrence claims."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import os
import re
import sys
from datetime import datetime
from pathlib import Path, PurePosixPath
from typing import Any


ROOT_KEYS = {
    "schema_version", "retrospect_id", "updated_at", "status", "scope",
    "sources", "coverage", "metrics", "findings", "proposals",
    "durable_promotions", "limitations",
}
COVERAGE_KEYS = {
    "outcome", "trajectory", "human_attention", "verification_review",
    "routing_tools", "context_memory", "skills", "documentation", "cost_latency",
}
COVERAGE_VALUES = {"measured", "partial", "unknown", "not-applicable"}
STATUSES = {"no-change", "proposed", "monitoring", "closed", "inconclusive"}
DECISIONS = {"pending", "improved", "unchanged", "regressed", "inconclusive"}
PROPOSAL_STATUSES = {"awaiting-approval", "rejected", "accepted", "implemented", "monitoring", "closed"}
SOURCE_KINDS = {"scope", "design", "delivery", "evaluation", "review", "release", "observation", "incident", "telemetry", "check"}
DESTINATIONS = {
    "specification", "architecture-decision", "runbook", "project-instructions",
    "state-context-digest", "test-suite", "versioned-eval-suite", "skill",
    "routing-policy", "improvement-register", "harness-scope",
}
FORBIDDEN_KEYS = {"prompt", "prompts", "message", "messages", "response", "responses", "transcript", "transcripts", "tool_arguments", "raw_content"}
HASH = re.compile(r"^[0-9a-f]{64}$")
PRODUCT_ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[3])
).expanduser()
SKILLS_ROOT = Path(__file__).resolve().parents[2]


class Invalid(ValueError):
    pass


def fail(condition: bool, message: str) -> None:
    if condition:
        raise Invalid(message)


def utc(value: Any, field: str) -> datetime:
    fail(not isinstance(value, str) or not value.endswith("Z"), f"{field} must be a UTC timestamp")
    try:
        return datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise Invalid(f"{field} is not an ISO timestamp") from exc


def digest(value: Any, field: str, *, allow_empty: bool = False) -> None:
    if allow_empty and value == "":
        return
    fail(not isinstance(value, str) or not HASH.fullmatch(value), f"{field} must be a SHA-256 hex digest")


def safe_relative(value: Any, field: str, *, allow_empty: bool = False) -> None:
    if allow_empty and value == "":
        return
    fail(not isinstance(value, str) or not value, f"{field} must be a relative path")
    fail("\\" in value or "\x00" in value, f"{field} must use a safe portable relative path")
    path = PurePosixPath(value)
    fail(path.is_absolute() or ".." in path.parts, f"{field} must not escape the run directory")


def number(value: Any, field: str) -> None:
    fail(isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value), f"{field} must be a finite number")


def non_negative_int(value: Any, field: str, *, positive: bool = False) -> None:
    fail(isinstance(value, bool) or not isinstance(value, int) or value < (1 if positive else 0), f"{field} must be a {'positive' if positive else 'non-negative'} integer")


def no_raw_content(value: Any, prefix: str = "$") -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            fail(key.lower() in FORBIDDEN_KEYS, f"forbidden raw-content key at {prefix}.{key}")
            no_raw_content(child, f"{prefix}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            no_raw_content(child, f"{prefix}[{index}]")


def cycle_or_reason(value: Any, field: str) -> None:
    fail(not isinstance(value, dict) or set(value) != {"cycle_ids", "absence_reason"}, f"invalid {field}")
    fail(not isinstance(value["cycle_ids"], list) or not all(isinstance(v, str) and v for v in value["cycle_ids"]), f"{field}.cycle_ids must be strings")
    has_cycles = bool(value["cycle_ids"])
    has_reason = bool(value["absence_reason"])
    fail(has_cycles == has_reason, f"{field} needs exactly one of cycle_ids or absence_reason")
    fail(len(value["cycle_ids"]) != len(set(value["cycle_ids"])), f"{field}.cycle_ids must be unique")


def validate(
    data: Any, gate: str, *, expected_cycle_id: str | None = None,
    expected_profile: str | None = None,
) -> None:
    fail(not isinstance(data, dict), "retrospect root must be an object")
    no_raw_content(data)
    fail(set(data) != ROOT_KEYS, f"root keys must be exactly {sorted(ROOT_KEYS)}")
    fail(data["schema_version"] != 1, "unsupported schema_version")
    fail(data["status"] not in STATUSES, "invalid status")
    allowed_status_by_gate = {
        "propose": {"no-change", "proposed"},
        "monitor": {"monitoring", "inconclusive"},
        "close": {"no-change", "closed", "inconclusive"},
    }
    fail(data["status"] not in allowed_status_by_gate[gate], f"status {data['status']} is invalid for {gate} gate")
    utc(data["updated_at"], "updated_at")
    scope = data["scope"]
    scope_keys = {"cycle_ids", "profile", "comparability_key", "authority_mode", "disclosure", "evidence_window", "baseline", "comparator"}
    fail(not isinstance(scope, dict) or set(scope) != scope_keys, "invalid scope keys")
    fail(not scope["cycle_ids"] or not all(isinstance(v, str) and v for v in scope["cycle_ids"]), "scope.cycle_ids must be non-empty strings")
    fail(not scope["comparability_key"], "comparability_key is required")
    fail(not scope["profile"], "profile is required")
    if expected_cycle_id is not None:
        fail(expected_cycle_id not in scope["cycle_ids"], "retrospective scope does not include the current delivery cycle")
    if expected_profile is not None:
        fail(scope["profile"] != expected_profile, "retrospective scope profile does not match the delivery profile")
    fail(scope["authority_mode"] != "read-only", "retrospective authority must remain read-only")
    fail(scope["disclosure"] != "local-only", "retrospective disclosure must remain local-only until separately authorised")
    window = scope["evidence_window"]
    fail(not isinstance(window, dict) or set(window) != {"started_at", "ended_at"}, "invalid evidence_window")
    window_start = utc(window["started_at"], "scope.evidence_window.started_at")
    window_end = utc(window["ended_at"], "scope.evidence_window.ended_at")
    fail(window_start >= window_end or window_end > utc(data["updated_at"], "updated_at"), "evidence_window must end by updated_at")
    cycle_or_reason(scope["baseline"], "baseline")
    cycle_or_reason(scope["comparator"], "comparator")

    fail(not isinstance(data["sources"], list) or not data["sources"], "sources are required")
    source_ids: set[str] = set()
    for index, source in enumerate(data["sources"]):
        required = {"kind", "id", "path", "sha256", "schema_version"}
        fail(not isinstance(source, dict) or set(source) != required, f"invalid source keys at {index}")
        fail(not source["id"] or source["id"] in source_ids, f"source {index} has missing or duplicate id")
        source_ids.add(source["id"])
        fail(source["kind"] not in SOURCE_KINDS, f"invalid source kind at {index}")
        safe_relative(source["path"], f"sources[{index}].path")
        digest(source["sha256"], f"sources[{index}].sha256")
    for field, cycle_ids in (
        ("scope.cycle_ids", scope["cycle_ids"]),
        ("scope.baseline.cycle_ids", scope["baseline"]["cycle_ids"]),
        ("scope.comparator.cycle_ids", scope["comparator"]["cycle_ids"]),
    ):
        fail(any(cycle not in source_ids for cycle in cycle_ids), f"{field} contains an unverified cycle")
        cycle_sources = [source for source in data["sources"] if source["id"] in cycle_ids]
        fail(any(source["kind"] != "delivery" for source in cycle_sources), f"{field} must reference delivery receipts")
        identities = [(source["path"], source["sha256"]) for source in cycle_sources]
        fail(len(identities) != len(set(identities)), f"{field} aliases the same delivery receipt")

    fail(not isinstance(data["coverage"], dict) or set(data["coverage"]) != COVERAGE_KEYS, "all coverage dimensions are required")
    fail(any(value not in COVERAGE_VALUES for value in data["coverage"].values()), "invalid coverage value")

    metric_ids: set[str] = set()
    for index, metric in enumerate(data["metrics"]):
        required = {"id", "definition", "value", "numerator", "denominator", "unit", "source_ids", "quality", "limitations"}
        fail(not isinstance(metric, dict) or set(metric) != required, f"invalid metric keys at {index}")
        fail(not metric["id"] or metric["id"] in metric_ids, f"metric {index} has missing or duplicate id")
        metric_ids.add(metric["id"])
        fail(not metric["source_ids"] or any(v not in source_ids for v in metric["source_ids"]), f"metric {index} has invalid sources")
        number(metric["value"], f"metrics[{index}].value")
        if metric["unit"] == "ratio":
            number(metric["numerator"], f"metrics[{index}].numerator")
            number(metric["denominator"], f"metrics[{index}].denominator")
            fail(metric["denominator"] <= 0, f"ratio metric {index} needs a positive denominator")
            fail(abs(metric["value"] - metric["numerator"] / metric["denominator"]) > 1e-9, f"ratio metric {index} value does not match numerator/denominator")
        fail(metric["quality"] not in {"observed", "proxy", "self-reported"}, f"invalid metric quality at {index}")

    finding_ids: set[str] = set()
    for index, finding in enumerate(data["findings"]):
        required = {"id", "category", "evidence_ids", "statement", "confidence", "escaped_defect"}
        fail(not isinstance(finding, dict) or set(finding) != required, f"invalid finding keys at {index}")
        fail(not finding["id"] or finding["id"] in finding_ids, f"finding {index} has missing or duplicate id")
        finding_ids.add(finding["id"])
        fail(not finding["evidence_ids"] or any(v not in source_ids and v not in metric_ids for v in finding["evidence_ids"]), f"finding {index} lacks valid evidence")
        fail(not finding["statement"], f"finding {index} needs a statement")

    if data["status"] == "no-change":
        fail(not data["metrics"] or not data["findings"], "no-change requires evidence-backed metrics and findings")

    fail(data["status"] != "no-change" and not data["proposals"], "non-no-change retrospective needs a proposal")
    proposal_ids_seen: set[str] = set()
    for index, proposal in enumerate(data["proposals"]):
        required = {
            "id", "finding_ids", "hypothesis", "destination", "owner", "risk_tier",
            "status", "authority_required", "intervention", "regression_gate", "recurrence",
        }
        fail(not isinstance(proposal, dict) or set(proposal) != required, f"invalid proposal keys at {index}")
        fail(not proposal["id"] or proposal["id"] in proposal_ids_seen, f"proposal {index} has missing or duplicate id")
        proposal_ids_seen.add(proposal["id"])
        fail(not proposal["finding_ids"] or any(v not in finding_ids for v in proposal["finding_ids"]), f"proposal {index} lacks valid findings")
        for field in ("hypothesis", "destination", "owner", "authority_required"):
            fail(not proposal[field], f"proposal {index} needs {field}")
        fail(proposal["destination"] not in DESTINATIONS, f"proposal {index} destination is not a canonical owner")
        fail(proposal["risk_tier"] not in {"routine", "substantial", "crucial", "terminal"}, f"invalid proposal risk at {index}")
        fail(proposal["status"] not in PROPOSAL_STATUSES, f"invalid proposal status at {index}")
        intervention = proposal["intervention"]
        fail(set(intervention) != {"receipt", "sha256", "version_or_revision"}, f"invalid intervention at {index}")
        safe_relative(intervention["receipt"], f"proposals[{index}].intervention.receipt", allow_empty=True)
        digest(intervention["sha256"], f"proposals[{index}].intervention.sha256", allow_empty=True)
        regression = proposal["regression_gate"]
        fail(set(regression) != {"fixture_ids", "commands", "evidence_ids", "status"}, f"invalid regression gate at {index}")
        fail(regression["status"] not in {"pending", "pass", "fail"}, f"invalid regression status at {index}")
        fail(not isinstance(regression["fixture_ids"], list) or not all(isinstance(v, str) and v for v in regression["fixture_ids"]), f"invalid regression fixtures at {index}")
        fail(not isinstance(regression["commands"], list) or not all(isinstance(v, list) and v and all(isinstance(arg, str) and arg for arg in v) for v in regression["commands"]), f"invalid regression commands at {index}")
        fail(not isinstance(regression["evidence_ids"], list) or any(v not in source_ids for v in regression["evidence_ids"]), f"invalid regression evidence at {index}")
        if regression["status"] == "pass":
            fail(not regression["fixture_ids"] and not regression["commands"], f"proposal {index} passing regression needs a fixture or command")
            fail(not regression["evidence_ids"], f"proposal {index} passing regression needs verified result evidence")
            fail(any(next(source for source in data["sources"] if source["id"] == evidence_id)["kind"] not in {"evaluation", "check"} for evidence_id in regression["evidence_ids"]), f"proposal {index} regression evidence must be an evaluation or check")
        recurrence = proposal["recurrence"]
        recurrence_keys = {
            "signature", "baseline_cycle_ids", "baseline_value", "direction", "target",
            "regression_limits", "minimum_comparable_cycles", "minimum_denominator",
            "due_after", "observed_cycle_ids", "observed_denominator", "observed_value", "observed_guard_values",
            "decision", "confounders",
        }
        fail(set(recurrence) != recurrence_keys, f"invalid recurrence at {index}")
        fail(recurrence["direction"] not in {"lte", "gte"}, f"invalid recurrence direction at {index}")
        fail(recurrence["decision"] not in DECISIONS, f"invalid recurrence decision at {index}")
        fail(len(recurrence["baseline_cycle_ids"]) != len(set(recurrence["baseline_cycle_ids"])), f"proposal {index} has duplicate baseline cycles")
        fail(len(recurrence["observed_cycle_ids"]) != len(set(recurrence["observed_cycle_ids"])), f"proposal {index} has duplicate observed cycles")
        fail(not set(recurrence["baseline_cycle_ids"]).isdisjoint(recurrence["observed_cycle_ids"]), f"proposal {index} observed cycles must be later than and distinct from baseline cycles")
        fail(any(cycle not in source_ids for cycle in recurrence["baseline_cycle_ids"]), f"proposal {index} baseline cycles lack verified sources")
        fail(any(cycle not in source_ids for cycle in recurrence["observed_cycle_ids"]), f"proposal {index} observed cycles lack verified sources")
        for label, cycle_ids in (("baseline", recurrence["baseline_cycle_ids"]), ("observed", recurrence["observed_cycle_ids"])):
            cycle_sources = [source for source in data["sources"] if source["id"] in cycle_ids]
            fail(any(source["kind"] != "delivery" for source in cycle_sources), f"proposal {index} {label} cycles must reference delivery receipts")
            identities = [(source["path"], source["sha256"]) for source in cycle_sources]
            fail(len(identities) != len(set(identities)), f"proposal {index} {label} cycles alias the same delivery receipt")
        for field in ("baseline_value", "target"):
            number(recurrence[field], f"proposals[{index}].recurrence.{field}")
        non_negative_int(recurrence["minimum_comparable_cycles"], f"proposals[{index}].recurrence.minimum_comparable_cycles", positive=True)
        non_negative_int(recurrence["minimum_denominator"], f"proposals[{index}].recurrence.minimum_denominator", positive=True)
        non_negative_int(recurrence["observed_denominator"], f"proposals[{index}].recurrence.observed_denominator")
        if recurrence["observed_value"] is not None:
            number(recurrence["observed_value"], f"proposals[{index}].recurrence.observed_value")
        fail(not isinstance(recurrence["regression_limits"], dict), f"invalid regression limits at {index}")
        for key, value in recurrence["regression_limits"].items():
            number(value, f"proposals[{index}].recurrence.regression_limits.{key}")
        fail(not isinstance(recurrence["observed_guard_values"], dict), f"invalid observed guards at {index}")
        for key, value in recurrence["observed_guard_values"].items():
            number(value, f"proposals[{index}].recurrence.observed_guard_values.{key}")
        utc(recurrence["due_after"], f"proposals[{index}].recurrence.due_after")
        if gate in {"monitor", "close"}:
            fail(proposal["status"] not in {"implemented", "monitoring", "closed"}, f"proposal {index} has no implemented intervention")
            fail(not intervention["receipt"] or not intervention["sha256"], f"proposal {index} needs an approved intervention receipt")
            fail(regression["status"] != "pass", f"proposal {index} regression gate has not passed")
        if gate == "close":
            fail(recurrence["decision"] == "pending", f"proposal {index} recurrence decision is pending")
        if gate == "close" and recurrence["decision"] == "improved":
            fail(len(recurrence["observed_cycle_ids"]) < recurrence["minimum_comparable_cycles"], f"proposal {index} lacks comparable cycles")
            fail(recurrence["observed_denominator"] < recurrence["minimum_denominator"], f"proposal {index} is underpowered")
            fail(recurrence["observed_value"] is None or recurrence["confounders"], f"proposal {index} cannot claim improvement with missing or confounded evidence")
            fail(set(recurrence["observed_guard_values"]) != set(recurrence["regression_limits"]), f"proposal {index} lacks guard metrics")
            fail(any(recurrence["observed_guard_values"][key] > limit for key, limit in recurrence["regression_limits"].items()), f"proposal {index} breached a guard metric")
            if recurrence["direction"] == "lte":
                fail(recurrence["observed_value"] > recurrence["target"], f"proposal {index} missed its target")
            else:
                fail(recurrence["observed_value"] < recurrence["target"], f"proposal {index} missed its target")

    fail(not isinstance(data["durable_promotions"], list), "durable_promotions must be a list")
    proposal_ids = {proposal["id"] for proposal in data["proposals"]}
    for index, promotion in enumerate(data["durable_promotions"]):
        fail(not isinstance(promotion, dict) or set(promotion) != {"proposal_id", "destination", "status"}, f"invalid durable promotion at {index}")
        fail(promotion["status"] not in {"proposed", "applied", "rejected"}, f"invalid promotion status at {index}")
        fail(promotion["proposal_id"] not in proposal_ids, f"promotion {index} references an unknown proposal")
        fail(promotion["destination"] not in DESTINATIONS, f"promotion {index} destination is not a canonical owner")
    fail(not isinstance(data["limitations"], list) or not all(isinstance(v, str) for v in data["limitations"]), "limitations must be strings")


def _delivery_validator():
    path = SKILLS_ROOT / "deliver" / "scripts" / "validate_delivery.py"
    spec = importlib.util.spec_from_file_location("retrospect_delivery_validator", path)
    fail(not spec or not spec.loader, "canonical delivery validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _delivery_project_policy_path(
    receipt: dict[str, Any], source_id: str, workspace_root: Path | None,
) -> Path | None:
    declared = receipt.get("project_policy")
    if declared is None:
        return None
    fail(workspace_root is None, f"delivery source {source_id} project policy requires workspace_root")
    fail(
        not isinstance(declared, dict) or set(declared) != {"path", "digest"},
        f"delivery source {source_id} project policy binding is invalid",
    )
    relative = declared.get("path")
    safe_relative(relative, f"delivery source {source_id} project_policy.path")
    workspace = workspace_root.resolve()
    target = (workspace / relative).resolve()
    fail(
        target != workspace and workspace not in target.parents,
        f"delivery source {source_id} project policy escapes workspace_root",
    )
    fail(not target.is_file(), f"delivery source {source_id} project policy is missing: {relative}")
    return target


def verify_hashes(
    data: dict[str, Any], base_dir: Path, *, expected_cycle_id: str | None = None,
    expected_profile: str | None = None, workspace_root: Path | None = None,
    product_root: Path = PRODUCT_ROOT,
) -> None:
    """Verify linked evidence and intervention receipts against the run directory."""
    base = base_dir.resolve()
    links: list[tuple[str, str, str]] = [
        (source["path"], source["sha256"], f"source {source['id']}")
        for source in data["sources"]
    ]
    links.extend(
        (
            proposal["intervention"]["receipt"],
            proposal["intervention"]["sha256"],
            f"intervention {proposal['id']}",
        )
        for proposal in data["proposals"]
        if proposal["intervention"]["receipt"]
    )
    for relative, expected, label in links:
        target = (base / relative).resolve()
        fail(target != base and base not in target.parents, f"{label} escapes the run directory")
        fail(not target.is_file(), f"{label} is missing: {relative}")
        actual = hashlib.sha256(target.read_bytes()).hexdigest()
        fail(actual != expected, f"{label} hash mismatch")
    delivery_validator = None
    for source in data["sources"]:
        if source["kind"] != "delivery":
            continue
        target = (base / source["path"]).resolve()
        try:
            receipt = json.loads(target.read_text())
        except (OSError, json.JSONDecodeError) as exc:
            raise Invalid(f"delivery source {source['id']} is not a JSON receipt") from exc
        embedded_id = receipt.get("run_id") if isinstance(receipt, dict) else None
        fail(embedded_id != source["id"], f"delivery source {source['id']} embedded identity mismatch")
        fail(
            source["schema_version"] != 1
            or receipt.get("schema_version") != source["schema_version"]
            or receipt.get("contract") != "delivery-run",
            f"delivery source {source['id']} is not a canonical delivery receipt",
        )
        if delivery_validator is None:
            delivery_validator = _delivery_validator()
        project_policy_path = _delivery_project_policy_path(receipt, source["id"], workspace_root)
        try:
            delivery_validator.validate(
                receipt, product_root, receipt_dir=target.parent,
                workspace_root=workspace_root or base,
                project_policy_path=project_policy_path,
                verify_hashes=False, validate_retrospective=False,
            )
        except delivery_validator.Invalid as exc:
            raise Invalid(f"delivery source {source['id']} is not a valid canonical delivery receipt: {exc}") from exc
        fail(receipt.get("profile") != data["scope"]["profile"],
             f"delivery source {source['id']} profile does not match retrospective scope")
        if expected_cycle_id is not None and source["id"] == expected_cycle_id:
            fail(receipt.get("profile") != expected_profile,
                 "current delivery source profile does not match the enclosing delivery")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--gate", choices=("propose", "monitor", "close"), default="propose")
    parser.add_argument("--schema-only", action="store_true", help="validate shape without resolving linked run evidence")
    parser.add_argument(
        "--workspace-root", type=Path, default=Path.cwd(),
        help="workspace root used to resolve digest-bound project policies",
    )
    parser.add_argument("--product-root", type=Path, default=PRODUCT_ROOT)
    args = parser.parse_args()
    try:
        data = json.loads(args.receipt.read_text())
        validate(data, args.gate)
        if not args.schema_only:
            verify_hashes(
                data, args.receipt.parent,
                workspace_root=args.workspace_root.resolve(),
                product_root=args.product_root.resolve(),
            )
    except (OSError, json.JSONDecodeError, Invalid) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print(f"PASS: retrospective {args.gate} gate")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
