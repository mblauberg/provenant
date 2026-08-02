#!/usr/bin/env python3
"""Validate and terminalise a bounded multi-agent run directory."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sys


sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from _shared.review_ladder import (
    REVIEW_PLAN_STATUSES,
    SKIPPED_STATUSES,
    check_review_ladder,
)
from _shared.review_panel import PANEL_RECORD_KEYS, validate_panel_result
from _shared.review_terminal import normalise_dispatch_review
from _shared.no_follow import NoFollowError, read_file, relative_path
from _shared.worker_outcome import accept_worker_outcome


TERMINAL = {"succeeded", "failed", "cancelled"}
STATUSES = {"draft", "verified", "superseded", "retired"}
RETENTION = {"capsule", "evidence", "ephemeral"}
SCAFFOLD = {
    "MANIFEST.md",
    "RUN_RECEIPT.json",
    "SYNTHESIS.md",
    "FINAL_GATE.md",
    "decisions.md",
    "traces/README.md",
}
REQUIRED_GATES = {
    "Worker artifacts listed in MANIFEST.md",
    "Run terminalisation inputs and retention policy verified",
    "Duplicate findings merged or superseded",
    "Contradictions resolved or recorded unresolved",
    "P0/P1 findings triaged or explicitly deferred",
    "Objective checks run with command/source locators",
    "Cross-family verifier record has status=ok, cross_family=true, and read_only_guarantee=enforced/oauth_safe_mode, or is marked scout only",
    "CROSS-FAMILY-NOT-RUN reasons recorded when cross-family verification was unavailable",
    "Advisory cross-family findings triaged and either verified or rejected",
    "Document update wave run or explicitly N/A",
    "Updated docs verified against current source/artifacts",
    "High-stakes/low-oracle work has two family passes or CROSS-FAMILY-NOT-RUN reasons",
    "No unauthorised shared-state writes",
    "Run-owned panes/resources closed or explicitly handed off",
    "Human-authority gates listed",
    "Final claims have source/test/file anchors",
    "Context hygiene classified: durable outputs retained; owned ephemeral payload archived/removed",
}
CERTIFYING_PROVIDER_ASSURANCE = frozenset({
    "full-vendor-identity",
    "lockfile-install-attestation",
})


def _inside(root: Path, candidate: Path) -> bool:
    try:
        candidate.resolve().relative_to(root.resolve())
        return True
    except ValueError:
        return False


def _utc_timestamp(value: object) -> bool:
    if not isinstance(value, str) or not value.endswith("Z"):
        return False
    try:
        datetime.fromisoformat(value[:-1] + "+00:00")
        return True
    except ValueError:
        return False


def _table(text: str, required: list[str]) -> list[dict[str, str]]:
    lines = [line for line in text.splitlines() if line.strip().startswith("|")]
    for index, line in enumerate(lines):
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if cells == required:
            rows: list[dict[str, str]] = []
            for row in lines[index + 2 :]:
                values = [cell.strip() for cell in row.strip().strip("|").split("|")]
                if len(values) != len(required):
                    continue
                rows.append(dict(zip(required, values)))
            return rows
    raise ValueError("required table header not found: " + ", ".join(required))


def _dispatch_terminal_reference(run_dir: Path, route_value: dict[str, object]) -> tuple[dict[str, str] | None, str | None]:
    path_value = route_value.get("terminal_artifact_path")
    digest = route_value.get("terminal_artifact_sha256")
    if not isinstance(path_value, str) or not path_value or not isinstance(digest, str):
        return None, "dispatcher terminal artifact reference is missing"
    try:
        relative = relative_path(run_dir, path_value, allow_absolute=True).as_posix()
    except (NoFollowError, OSError, RuntimeError, ValueError):
        return None, "dispatcher terminal artifact escapes the run directory"
    return {"path": relative, "digest": digest}, None


def _read_bound_bytes(
    run_dir: Path,
    value: object,
    label: str,
) -> tuple[Path | None, bytes | None, str | None]:
    if not isinstance(value, dict) or set(value) != {"path", "digest"}:
        return None, None, f"{label} reference is invalid"
    path_value = value.get("path")
    digest = value.get("digest")
    if not isinstance(path_value, str) or not isinstance(digest, str) or not digest.startswith("sha256:"):
        return None, None, f"{label} reference is invalid"
    try:
        relative = relative_path(run_dir, path_value, allow_absolute=False)
        data, _identity, logical = read_file(
            relative,
            label,
            root=run_dir,
            allow_absolute=False,
        )
    except (NoFollowError, OSError, RuntimeError, ValueError) as exc:
        return None, None, f"{label} is missing or does not match: {exc}"
    if "sha256:" + hashlib.sha256(data).hexdigest() != digest:
        return None, None, f"{label} is missing or does not match"
    return logical, data, None


def _read_bound_json(
    run_dir: Path,
    value: object,
    label: str,
) -> tuple[Path | None, dict[str, object] | None, str | None]:
    logical, data, error = _read_bound_bytes(run_dir, value, label)
    if error:
        return None, None, error
    assert data is not None
    try:
        parsed = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None, None, f"{label} is invalid JSON"
    if not isinstance(parsed, dict):
        return None, None, f"{label} must be a JSON object"
    return logical, parsed, None


def _direct_worker_leg(
    run_dir: Path,
    review: dict[str, object],
    route_ref: dict[str, object],
    route_value: dict[str, object],
    terminal_ref: object,
    terminal_value: object,
    chair_family: object,
    risk: object,
    worktree_receipt: object = None,
) -> tuple[str | None, dict[str, object] | None]:
    """Consume the common outcome join only for the direct-CLI producer."""
    outcome_reference: dict[str, object] = {
        "id": review["id"],
        "dispatch_receipt": route_ref,
        "terminal_artifact": terminal_ref,
        "worktree_receipt": worktree_receipt,
    }
    dispatch_terminal_ref, dispatch_error = _dispatch_terminal_reference(run_dir, route_value)
    if dispatch_error:
        return dispatch_error, None
    assert dispatch_terminal_ref is not None
    outcome_reference["dispatch_terminal_artifact"] = dispatch_terminal_ref
    outcome = accept_worker_outcome(run_dir, outcome_reference)
    if outcome.get("status") != "accepted":
        return str(outcome.get("reason", "invalid outcome")), None
    derived = outcome.get("derived_evidence")
    if not isinstance(derived, dict):
        return "worker outcome did not return derived evidence", None
    semantic_result = derived.get("semantic_result")
    human_answer = derived.get("human_answer")
    dispatcher_output_available = isinstance(human_answer, dict)
    transcript_available = isinstance(semantic_result, dict)
    if review.get("status") == "complete" and outcome.get("certifying") is not True:
        return str(outcome.get("reason", "worker outcome is not certifying")), None
    return None, normalise_dispatch_review(
        route_value,
        semantic_result,
        review_verdict=review.get("verdict"),
        chair_family=chair_family,
        transcript_available=transcript_available,
        dispatcher_output_available=dispatcher_output_available,
    )


def _validate_review_plan(raw: object, run_dir: Path | None = None) -> list[str]:
    errors: list[str] = []
    if not isinstance(raw, dict) or set(raw) != {
        "risk_tier", "chair_family", "concurrency_ceiling", "reviews", "panels",
    }:
        return ["receipt review_plan must use the closed review topology schema"]
    risk = raw.get("risk_tier")
    if not isinstance(risk, str) or risk not in {"routine", "substantial", "crucial", "terminal"}:
        errors.append("receipt review_plan.risk_tier is invalid")
    ceiling = raw.get("concurrency_ceiling")
    if not isinstance(ceiling, int) or isinstance(ceiling, bool) or not 1 <= ceiling <= 32:
        errors.append("receipt review_plan.concurrency_ceiling must be between 1 and 32")
    reviews = raw.get("reviews")
    if not isinstance(reviews, list):
        return errors + ["receipt review_plan.reviews must be a list"]
    keys = {
        "id", "scope", "lens", "family", "tier", "status",
        "substitution_for", "evidence", "reason", "verdict", "terminal_result", "wave",
        "adapter", "model", "catalog_model", "route_receipt",
        "reviewer_id", "adapter_gate",
    }
    seen: set[str] = set()
    checked: list[dict[str, object]] = []
    normalized_legs: dict[str, dict[str, object]] = {}
    for index, review in enumerate(reviews):
        if not isinstance(review, dict) or set(review) != keys:
            errors.append(f"receipt review_plan.reviews[{index}] must use the closed review record schema")
            continue
        if not isinstance(review["id"], str) or not review["id"] or review["id"] in seen:
            errors.append(f"receipt review_plan.reviews[{index}].id must be non-empty and unique")
        else:
            seen.add(review["id"])
        if not isinstance(review["scope"], str) or review["scope"] not in {"targeted", "primary"}:
            errors.append(f"receipt review_plan.reviews[{index}].scope is invalid")
        if not isinstance(review["tier"], str) or review["tier"] not in {"scout", "workhorse", "flagship"}:
            errors.append(f"receipt review_plan.reviews[{index}].tier is invalid")
        if not isinstance(review["status"], str) or review["status"] not in REVIEW_PLAN_STATUSES:
            errors.append(f"receipt review_plan.reviews[{index}].status is invalid")
        for field in ("lens", "family"):
            if not isinstance(review[field], str) or not review[field]:
                errors.append(f"receipt review_plan.reviews[{index}].{field} is required")
        if not isinstance(review["adapter"], str) or not review["adapter"]:
            errors.append(f"receipt review_plan.reviews[{index}].adapter is required")
        if (
            not isinstance(review["adapter_gate"], str)
            or review["adapter_gate"] not in {"fabric", "direct-cli"}
        ):
            errors.append(f"receipt review_plan.reviews[{index}].adapter_gate is invalid")
        if not isinstance(review["reviewer_id"], str) or not review["reviewer_id"]:
            errors.append(f"receipt review_plan.reviews[{index}].reviewer_id is required")
        if review["status"] == "complete" and not any(
            isinstance(review[field], str) and review[field] for field in ("model", "catalog_model")
        ):
            errors.append(f"receipt review_plan.reviews[{index}] requires resolved or catalog model identity")
        if review["status"] == "complete" and (
            not isinstance(review["verdict"], str) or not review["verdict"].strip()
        ):
            errors.append(f"receipt review_plan.reviews[{index}] requires an explicit verdict")
        terminal_result_ref = review["terminal_result"]
        terminal_result_value: object = None
        terminal_path: Path | None = None
        if review["status"] != "omitted":
            if not isinstance(terminal_result_ref, dict) or set(terminal_result_ref) != {"path", "digest"}:
                errors.append(f"receipt review_plan.reviews[{index}].terminal_result is invalid")
            else:
                terminal_path = Path(terminal_result_ref["path"]) if isinstance(terminal_result_ref["path"], str) else Path("..")
                terminal_digest = terminal_result_ref["digest"]
                if (
                    terminal_path.is_absolute()
                    or ".." in terminal_path.parts
                    or not isinstance(terminal_digest, str)
                    or not terminal_digest.startswith("sha256:")
                ):
                    errors.append(f"receipt review_plan.reviews[{index}].terminal_result is invalid")
                elif run_dir is not None:
                    _terminal_target, terminal_result_value, terminal_error = _read_bound_json(
                        run_dir,
                        terminal_result_ref,
                        f"receipt review_plan.reviews[{index}].terminal_result",
                    )
                    if terminal_error:
                        errors.append(terminal_error)
        elif terminal_result_ref is not None:
            errors.append(f"receipt review_plan.reviews[{index}].omitted leg must not have terminal_result")
        evidence = review["evidence"]
        if review["status"] == "omitted":
            if evidence is not None:
                errors.append(f"receipt review_plan.reviews[{index}].omitted leg must not have evidence")
        elif not isinstance(evidence, dict) or set(evidence) != {"path", "digest"}:
            errors.append(f"receipt review_plan.reviews[{index}].evidence is invalid")
        else:
            path = Path(evidence["path"]) if isinstance(evidence["path"], str) else Path("..")
            digest = evidence["digest"]
            if path.is_absolute() or ".." in path.parts or not isinstance(digest, str) or not digest.startswith("sha256:"):
                errors.append(f"receipt review_plan.reviews[{index}].evidence is invalid")
            elif run_dir is not None:
                _evidence_target, _evidence_data, evidence_error = _read_bound_bytes(
                    run_dir,
                    evidence,
                    f"receipt review_plan.reviews[{index}].evidence",
                )
                if evidence_error:
                    errors.append(evidence_error)
        if isinstance(review["status"], str) and review["status"] in SKIPPED_STATUSES and (
            not isinstance(review["reason"], str) or not review["reason"]
        ):
            errors.append(f"receipt review_plan.reviews[{index}].reason is required for an incomplete leg")
        route = review["route_receipt"]
        if review["status"] == "omitted":
            if route is not None:
                errors.append(f"receipt review_plan.reviews[{index}].omitted leg must not have route_receipt")
        elif not isinstance(route, dict) or set(route) != {"path", "digest"}:
            errors.append(f"receipt review_plan.reviews[{index}].route_receipt is invalid")
        else:
            route_path = Path(route["path"]) if isinstance(route["path"], str) else Path("..")
            route_digest = route["digest"]
            if route_path.is_absolute() or ".." in route_path.parts or not isinstance(route_digest, str) or not route_digest.startswith("sha256:"):
                errors.append(f"receipt review_plan.reviews[{index}].route_receipt is invalid")
            elif run_dir is not None:
                _route_target, route_value, route_error = _read_bound_json(
                    run_dir,
                    route,
                    f"receipt review_plan.reviews[{index}].route_receipt",
                )
                if route_error:
                    errors.append(route_error)
                else:
                    if not isinstance(route_value, dict):
                        errors.append(f"receipt review_plan.reviews[{index}].route_receipt identity does not match")
                    elif (
                        not isinstance(route_value.get("status"), str)
                        or not route_value.get("status")
                        or route_value.get("adapter") != review["adapter"]
                        or route_value.get("adapter_gate") != review["adapter_gate"]
                        or route_value.get("reviewer_id") != review["reviewer_id"]
                        or (
                            review["model"]
                            and route_value.get("resolved_model", route_value.get("model", "")) != review["model"]
                        )
                        or (
                            review["catalog_model"]
                            and route_value.get("catalog_model", "") != review["catalog_model"]
                        )
                        or route_value.get("model_family") != review["family"]
                        or (
                            risk in {"substantial", "crucial", "terminal"}
                            and (
                                not isinstance(route_value.get("provider_family"), str)
                                or not route_value.get("provider_family", "").strip()
                                or not isinstance(route_value.get("endpoint_provider"), str)
                                or not route_value.get("endpoint_provider", "").strip()
                            )
                        )
                        or (
                            isinstance(raw.get("chair_family"), str)
                            and bool(raw.get("chair_family"))
                            and route_value.get("orchestrator_family") != raw.get("chair_family")
                        )
                    ):
                        errors.append(f"receipt review_plan.reviews[{index}].route_receipt identity does not match")
                    if isinstance(route_value, dict):
                        route_alias = route_value.get("route_alias", route_value.get("alias"))
                        if review["tier"] == "flagship" and route_alias != "flagship":
                            errors.append(f"receipt review_plan.reviews[{index}].route_receipt does not prove flagship strength")
                        if review["scope"] == "primary" and (
                            route_value.get("cross_family") is not True
                            or route_value.get("provider_assurance") not in CERTIFYING_PROVIDER_ASSURANCE
                        ):
                            errors.append(f"receipt review_plan.reviews[{index}].route_receipt is not certification eligible")
                        if route_value.get("adapter_gate") in {"direct-cli", "fabric"}:
                            outcome_error, leg = _direct_worker_leg(
                                run_dir, review, route, route_value, terminal_result_ref,
                                terminal_result_value, raw.get("chair_family"), risk,
                            )
                            if outcome_error:
                                errors.append(
                                    f"receipt review_plan.reviews[{index}] worker outcome rejected: {outcome_error}"
                                )
                            elif leg is not None:
                                normalized_legs[review["id"]] = leg
                                expected_status = "pass" if review["status"] == "complete" else review["status"]
                                if leg["status"] != expected_status:
                                    errors.append(
                                        f"receipt review_plan.reviews[{index}] status is not derived from joined evidence: "
                                        f"declared {review['status']}, observed {leg['status']}"
                                    )
                                if review["status"] == "complete" and leg["status"] != "pass":
                                    errors.append(
                                        f"receipt review_plan.reviews[{index}] is not a certifying review leg: {leg['reason']}"
                                    )
                                other_primary_family = "anthropic" if raw.get("chair_family") == "openai" else "openai"
                                if (
                                    review["scope"] == "primary"
                                    and review["family"] == other_primary_family
                                    and not review["substitution_for"]
                                    and not leg["certifying_vote"]
                                ):
                                    errors.append(
                                        f"receipt review_plan.reviews[{index}] route is not certification eligible: "
                                        f"{leg['reason'] or 'provider-lineage'}"
                                    )
                        else:
                            errors.append(
                                f"receipt review_plan.reviews[{index}] adapter gate is not supported by the worker outcome join"
                            )
        if not isinstance(review["wave"], int) or isinstance(review["wave"], bool) or review["wave"] < 0:
            errors.append(f"receipt review_plan.reviews[{index}].wave must be a non-negative integer")
        if all(isinstance(review[field], str) for field in (
            "id", "scope", "lens", "family", "tier", "status", "substitution_for",
            "reason", "adapter", "model", "catalog_model",
            "reviewer_id", "adapter_gate",
        )) and isinstance(review["wave"], int) and not isinstance(review["wave"], bool):
            checked.append(review)
        if not isinstance(review["substitution_for"], str):
            errors.append(f"receipt review_plan.reviews[{index}].substitution_for must be a string")

    panels = raw.get("panels")
    if not isinstance(panels, list):
        errors.append("receipt review_plan.panels must be a list")
    else:
        panel_ids: set[str] = set()
        reviews_by_id = {
            review["id"]: review
            for review in checked
            if isinstance(review.get("id"), str) and review["id"]
        }
        for index, panel in enumerate(panels):
            prefix = f"receipt review_plan.panels[{index}]"
            if not isinstance(panel, dict) or set(panel) != PANEL_RECORD_KEYS:
                errors.append(f"{prefix} must use the closed panel record schema")
                continue
            panel_id = panel["id"]
            if (
                not isinstance(panel_id, str)
                or not panel_id
                or panel_id in panel_ids
            ):
                errors.append(f"{prefix}.id must be non-empty and unique")
            else:
                panel_ids.add(panel_id)

            spec = panel["spec"]
            membership = spec.get("membership") if isinstance(spec, dict) else None
            members: list[dict[str, object]] = []
            if isinstance(membership, list):
                for member_index, review_id in enumerate(membership):
                    if isinstance(review_id, str) and review_id in reviews_by_id:
                        members.append(reviews_by_id[review_id])
                    elif isinstance(review_id, str):
                        errors.append(
                            f"{prefix}.spec.membership[{member_index}] references unknown review id"
                        )
            panel_errors = validate_panel_result(spec, members, panel["result"])
            errors.extend(f"{prefix}: {error}" for error in panel_errors)
    if not isinstance(risk, str) or risk not in {"substantial", "crucial", "terminal"}:
        return errors
    chair_value = raw.get("chair_family")
    if not isinstance(chair_value, str) or chair_value not in {"openai", "anthropic"}:
        errors.append("substantial review_plan requires an openai or anthropic chair_family")
    chair = chair_value if isinstance(chair_value, str) else ""
    other_primary = "anthropic" if chair == "openai" else "openai"
    for review in checked:
        if (
            review["scope"] == "primary"
            and review["family"] == other_primary
            and not review["substitution_for"]
            and review["tier"] != "flagship"
        ):
            errors.append("other-primary review must use flagship strength")
    targeted = [r for r in checked if r["scope"] == "targeted" and r["status"] == "complete"]
    if len({r["reviewer_id"] for r in targeted}) != len(targeted):
        errors.append("completed targeted reviews require distinct reviewer_id values")
    for field in ("evidence", "route_receipt"):
        identities = [
            (r[field].get("path"), r[field].get("digest"))
            for r in targeted
            if isinstance(r[field], dict)
            and isinstance(r[field].get("path"), str)
            and isinstance(r[field].get("digest"), str)
        ]
        digests = [identity[1] for identity in identities]
        if len(identities) != len(targeted) or len(set(digests)) != len(targeted):
            errors.append(f"completed targeted reviews require distinct {field} artifacts")
    legs: list[dict[str, object]] = []
    for review in checked:
        if review["scope"] == "targeted":
            role = "targeted"
        elif (
            review["scope"] == "primary"
            and review["family"] == other_primary
            and not review["substitution_for"]
        ):
            role = "other-primary"
        elif (
            review["scope"] == "primary"
            and review["family"] not in {chair, other_primary}
            and (review["tier"] == "flagship" or review["status"] != "complete")
        ):
            role = "distinct-family"
        else:
            continue
        legs.append({
            "role": role,
            "family": review["family"],
            "status": (
                normalized_legs[review["id"]]["status"]
                if review["id"] in normalized_legs
                else "pass" if review["status"] == "complete" else review["status"]
            ),
            "lenses": [review["lens"]],
            "reason": (
                normalized_legs[review["id"]]["reason"]
                if review["id"] in normalized_legs
                else review["reason"]
            ),
            "substitution_for": review["substitution_for"],
        })
    errors.extend(check_review_ladder(risk, legs, chair_family=chair))
    waves: dict[int, int] = {}
    for review in checked:
        if review["status"] != "omitted" and isinstance(review["wave"], int):
            waves[review["wave"]] = waves.get(review["wave"], 0) + 1
    if isinstance(ceiling, int) and any(count > ceiling for count in waves.values()):
        errors.append("review_plan observed wave exceeds concurrency_ceiling")
    return errors


def validate(run_dir: Path, terminal_status: str, reason: str | None) -> tuple[list[str], list[dict[str, str]]]:
    errors: list[str] = []
    run_dir = run_dir.expanduser().absolute()
    if terminal_status not in TERMINAL:
        return ["status must be succeeded, failed, or cancelled"], []
    if terminal_status in {"failed", "cancelled"} and not reason:
        errors.append("failed/cancelled finalisation requires --reason")
    for name in ("MANIFEST.md", "RUN_RECEIPT.json", "SYNTHESIS.md", "FINAL_GATE.md"):
        if not (run_dir / name).is_file():
            errors.append(f"missing {name}")
    if errors:
        return errors, []

    try:
        receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text())
    except (OSError, json.JSONDecodeError) as exc:
        return [f"invalid RUN_RECEIPT.json: {exc}"], []
    if not isinstance(receipt, dict):
        return ["invalid RUN_RECEIPT.json: root must be an object"], []
    if receipt.get("schema_version") != 1:
        errors.append("receipt schema_version must be 1")
    if not _utc_timestamp(receipt.get("created_at")):
        errors.append("receipt created_at must be a UTC timestamp")
    if receipt.get("status") not in {"active", terminal_status}:
        errors.append(f"receipt status {receipt.get('status')!r} cannot transition to {terminal_status}")
    if receipt.get("status") == "active" and receipt.get("closed_at") is not None:
        errors.append("active receipt closed_at must be null")
    if receipt.get("status") in TERMINAL and not _utc_timestamp(receipt.get("closed_at")):
        errors.append("terminal receipt closed_at must be a UTC timestamp")
    if not isinstance(receipt.get("owner"), str) or not receipt.get("owner"):
        errors.append("receipt owner is required")
    for field in ("owned_panes", "closed_panes", "handed_off_panes", "unclassified_paths", "pruned_paths"):
        if not isinstance(receipt.get(field), list):
            errors.append(f"receipt {field} must be a list")
    for field in ("unclassified_paths", "pruned_paths"):
        if isinstance(receipt.get(field), list) and any(not isinstance(item, str) for item in receipt[field]):
            errors.append(f"receipt {field} entries must be strings")
    if not receipt.get("retention_policy"):
        errors.append("receipt retention_policy is required")
    if receipt.get("owned_panes"):
        errors.append("run-owned panes/resources must be closed or handed off before terminalisation")
    for index, raw in enumerate(receipt.get("handed_off_panes", [])):
        if not isinstance(raw, dict):
            errors.append(f"receipt handed_off_panes[{index}] must be a structured handoff record")
            continue
        required = ("pane_id", "role", "provider_family", "model", "handoff_target", "owner", "status", "handed_off_at", "lease_generation")
        if any(not raw.get(field) for field in required if field != "lease_generation"):
            errors.append(f"receipt handed_off_panes[{index}] is missing identity or handoff evidence")
        if raw.get("status") != "acknowledged" or not _utc_timestamp(raw.get("handed_off_at")):
            errors.append(f"receipt handed_off_panes[{index}] must be acknowledged with UTC timestamp")
        if not isinstance(raw.get("lease_generation"), int) or isinstance(raw.get("lease_generation"), bool) or raw.get("lease_generation") < 0:
            errors.append(f"receipt handed_off_panes[{index}].lease_generation must be non-negative")
        evidence_value = raw.get("evidence_path")
        evidence_path = run_dir / evidence_value if isinstance(evidence_value, str) else run_dir / "missing"
        if not evidence_path.is_file() or hashlib.sha256(evidence_path.read_bytes()).hexdigest() != raw.get("evidence_sha256"):
            errors.append(f"receipt handed_off_panes[{index}] requires matching provider evidence")
    for index, raw in enumerate(receipt.get("closed_panes", [])):
        if not isinstance(raw, dict):
            errors.append(f"receipt closed_panes[{index}] must be a structured closure record")
            continue
        required = ("pane_id", "role", "provider_family", "model", "closed_by", "closed_at")
        if any(not raw.get(field) for field in required) or raw.get("status") != "verified" or not _utc_timestamp(raw.get("closed_at")):
            errors.append(f"receipt closed_panes[{index}] requires verified identity and closure evidence")
        evidence_value = raw.get("evidence_path")
        evidence_path = run_dir / evidence_value if isinstance(evidence_value, str) else run_dir / "missing"
        if not evidence_path.is_file() or hashlib.sha256(evidence_path.read_bytes()).hexdigest() != raw.get("evidence_sha256"):
            errors.append(f"receipt closed_panes[{index}] requires matching provider evidence")

    pair = receipt.get("pair")
    if not isinstance(pair, dict):
        errors.append("receipt pair must be an object")
    elif pair.get("mode") not in {"solo", "paired-primary"}:
        errors.append("receipt pair.mode must be solo or paired-primary")
    elif pair.get("mode") == "paired-primary":
        if {pair.get("chair_family"), pair.get("peer_family")} != {"anthropic", "openai"}:
            errors.append("paired receipt requires anthropic/openai chair and peer")
        if not pair.get("chair_id") or not pair.get("peer_id") or pair.get("chair_id") == pair.get("peer_id"):
            errors.append("paired receipt requires distinct chair_id and peer_id")
        if pair.get("status") not in {"complete", "degraded"}:
            errors.append("paired receipt must be complete or degraded at terminalisation")
        if pair.get("status") == "degraded" and not pair.get("degradation_reason"):
            errors.append("degraded paired receipt requires degradation_reason")
        for field in ("lease_generation", "checkpoint_generation"):
            value = pair.get(field)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                errors.append(f"paired receipt {field} must be non-negative")
        if not pair.get("current_stage") or pair.get("in_flight") != []:
            errors.append("paired receipt needs a current_stage and empty in_flight at terminalisation")
        if not isinstance(pair.get("handoff_generation"), int) or isinstance(pair.get("handoff_generation"), bool):
            errors.append("paired receipt handoff_generation must be an integer")
        artifacts = pair.get("assignment_artifacts")
        if not isinstance(artifacts, list) or not artifacts:
            errors.append("paired receipt requires assignment_artifacts")
        else:
            artifact_paths: list[str] = []
            for raw in artifacts:
                value = raw.get("path") if isinstance(raw, dict) else None
                digest = raw.get("sha256") if isinstance(raw, dict) else None
                path = Path(value) if isinstance(value, str) else Path("..")
                target = path if path.is_absolute() else run_dir / path
                if not isinstance(value, str) or path.is_absolute() or ".." in path.parts or not target.is_file():
                    errors.append("paired receipt assignment_artifacts must be existing run-relative files")
                elif not isinstance(digest, str) or len(digest) != 64 or hashlib.sha256(target.read_bytes()).hexdigest() != digest:
                    errors.append("paired receipt assignment_artifacts require matching SHA-256")
                else:
                    artifact_paths.append(value)
            if len(artifact_paths) != len(set(artifact_paths)):
                errors.append("paired receipt assignment_artifacts must be distinct")
        stages = pair.get("stage_ledger")
        if not isinstance(stages, list) or not stages:
            errors.append("paired receipt requires a stage_ledger")
        else:
            owners: set[str] = set()
            prior = ""
            stage_artifact_paths: list[str] = []
            for index, stage in enumerate(stages):
                if not isinstance(stage, dict):
                    errors.append(f"paired receipt stage_ledger[{index}] must be an object")
                    continue
                owner = stage.get("owner_family")
                if owner not in {"anthropic", "openai"}:
                    errors.append(f"paired receipt stage_ledger[{index}] owner is invalid")
                else:
                    owners.add(owner)
                    expected_peer = "anthropic" if owner == "openai" else "openai"
                    if stage.get("peer_family") != expected_peer:
                        errors.append(f"paired receipt stage_ledger[{index}] peer is invalid")
                if stage.get("generation") != index + 1 or stage.get("status") != "complete":
                    errors.append(f"paired receipt stage_ledger[{index}] generation/status is invalid")
                if stage.get("acknowledged") is not True:
                    errors.append(f"paired receipt stage_ledger[{index}] must be acknowledged")
                for kind in ("assignment", "acknowledgement", "output"):
                    value = stage.get(f"{kind}_path")
                    digest = stage.get(f"{kind}_sha256")
                    path = Path(value) if isinstance(value, str) else Path("..")
                    target = run_dir / path
                    if not isinstance(value, str) or path.is_absolute() or ".." in path.parts or not target.is_file():
                        errors.append(f"paired receipt stage_ledger[{index}] {kind} artifact is invalid")
                    elif not isinstance(digest, str) or hashlib.sha256(target.read_bytes()).hexdigest() != digest:
                        errors.append(f"paired receipt stage_ledger[{index}] {kind} SHA-256 does not match")
                    else:
                        stage_artifact_paths.append(value)
                checks = stage.get("checks")
                if not isinstance(checks, list) or not checks or any(
                    not isinstance(check, dict) or not check.get("command") or check.get("exit_code") != 0
                    for check in checks
                ):
                    errors.append(f"paired receipt stage_ledger[{index}] requires passing objective checks")
                if not isinstance(stage.get("human_gates"), list):
                    errors.append(f"paired receipt stage_ledger[{index}].human_gates must be a list")
                if prior and stage.get("base_revision") != prior:
                    errors.append(f"paired receipt stage_ledger[{index}] breaks revision continuity")
                prior = stage.get("result_revision", prior)
            if len(stage_artifact_paths) != len(set(stage_artifact_paths)):
                errors.append("paired receipt stage artifacts must use distinct paths")
            if pair.get("status") == "complete" and owners != {"anthropic", "openai"}:
                errors.append("complete paired receipt requires stage ownership by both primaries")
            if pair.get("current_stage") != stages[-1].get("stage"):
                errors.append("paired receipt current_stage must match the final ledger stage")
            if pair.get("checkpoint_generation", -1) < len(stages):
                errors.append("paired receipt checkpoint_generation trails the stage ledger")
        lease_value = pair.get("lease_path")
        lease_path = run_dir / lease_value if isinstance(lease_value, str) else run_dir / "missing"
        try:
            lease = json.loads(lease_path.read_text())
        except (OSError, json.JSONDecodeError):
            errors.append("paired receipt must reference a readable lease")
        else:
            if lease.get("generation") != pair.get("lease_generation") or lease.get("status") != "released":
                errors.append("paired receipt must bind the released terminal lease generation")
            if lease.get("previous_holder") != pair.get("chair_id"):
                errors.append("paired receipt terminal lease must have been released by the chair")
        for index, handoff in enumerate(receipt.get("handed_off_panes", [])):
            if isinstance(handoff, dict) and handoff.get("lease_generation") != pair.get("lease_generation"):
                errors.append(f"receipt handed_off_panes[{index}] does not match pair lease generation")
    if terminal_status == "succeeded" and not receipt.get("task"):
        errors.append("successful finalisation requires receipt task")
    if terminal_status == "succeeded" and (reason or receipt.get("terminal_reason")):
        errors.append("successful finalisation cannot record a terminal failure reason")
    if terminal_status == "succeeded" and not (run_dir / "SYNTHESIS.md").read_text().strip():
        errors.append("successful finalisation requires non-empty SYNTHESIS.md")
    if terminal_status == "succeeded":
        errors.extend(_validate_review_plan(receipt.get("review_plan"), run_dir))

    columns = ["id", "path", "topic", "produced_by", "date", "status", "retention", "supersedes"]
    try:
        rows = _table((run_dir / "MANIFEST.md").read_text(), columns)
    except (OSError, ValueError) as exc:
        return errors + [f"invalid MANIFEST.md: {exc}"], []
    ids = {row["id"] for row in rows if row["id"]}
    listed: set[str] = set()
    for row in rows:
        artifact_id = row["id"]
        rel = row["path"]
        if not artifact_id or not rel:
            errors.append("manifest rows require id and path")
            continue
        if row["status"] not in STATUSES:
            errors.append(f"{artifact_id}: invalid status {row['status']!r}")
        if row["retention"] not in RETENTION:
            errors.append(f"{artifact_id}: invalid retention {row['retention']!r}")
        path = Path(rel)
        target = run_dir / path
        if path.is_absolute() or ".." in path.parts or not _inside(run_dir, target):
            errors.append(f"{artifact_id}: path escapes run directory")
            continue
        if path.as_posix() in listed:
            errors.append(f"{artifact_id}: duplicate manifest path: {rel}")
        listed.add(path.as_posix())
        if row["status"] != "retired" and not target.is_file():
            errors.append(f"{artifact_id}: manifest path does not exist: {rel}")
        supersedes = row["supersedes"]
        if supersedes and supersedes != "-" and supersedes not in ids:
            errors.append(f"{artifact_id}: supersedes must reference an artifact id")
        if terminal_status == "succeeded" and row["status"] == "draft":
            errors.append(f"{artifact_id}: draft artifact blocks successful finalisation")

    payloads = {
        path.relative_to(run_dir).as_posix()
        for path in run_dir.rglob("*")
        if path.is_file() and path.relative_to(run_dir).as_posix() not in SCAFFOLD
    }
    if terminal_status == "succeeded":
        for rel in sorted(payloads - listed):
            errors.append(f"unmanifested payload: {rel}")

    if terminal_status == "succeeded":
        try:
            gates = _table((run_dir / "FINAL_GATE.md").read_text(), ["gate", "status", "evidence"])
        except (OSError, ValueError) as exc:
            errors.append(f"invalid FINAL_GATE.md: {exc}")
        else:
            names = [gate["gate"] for gate in gates]
            if len(names) != len(set(names)):
                errors.append("FINAL_GATE.md contains duplicate gate rows")
            missing = REQUIRED_GATES - set(names)
            unknown = set(names) - REQUIRED_GATES
            if missing:
                errors.append("FINAL_GATE.md missing gates: " + ", ".join(sorted(missing)))
            if unknown:
                errors.append("FINAL_GATE.md has unknown gates: " + ", ".join(sorted(unknown)))
            for gate in gates:
                if gate["status"].upper() not in {"PASS", "N/A"}:
                    errors.append(f"final gate not closed: {gate['gate']}")
                if not gate["evidence"]:
                    errors.append(f"final gate lacks evidence: {gate['gate']}")
    return errors, rows


def prune_candidates(run_dir: Path, rows: list[dict[str, str]]) -> list[Path]:
    root = run_dir.resolve()
    retained_files = [root / name for name in {"SYNTHESIS.md", "FINAL_GATE.md", "decisions.md"} if (root / name).is_file()]
    retained_files.extend(
        root / row["path"]
        for row in rows
        if row["retention"] in {"capsule", "evidence"}
        and row["status"] != "retired"
        and (root / row["path"]).is_file()
    )

    def referenced(candidate: Path, manifest_path: str) -> bool:
        for retained in retained_files:
            needles = {
                manifest_path,
                candidate.name,
            }
            try:
                needles.add(candidate.resolve().relative_to(retained.parent.resolve()).as_posix())
            except ValueError:
                pass
            try:
                tokens = [needle.encode() for needle in needles]
                tail = max((len(token) for token in tokens), default=1) - 1
                carry = b""
                with retained.open("rb") as handle:
                    while chunk := handle.read(65536):
                        data = carry + chunk
                        if any(token in data for token in tokens):
                            return True
                        carry = data[-tail:] if tail else b""
            except OSError:
                return True
        return False

    candidates: list[Path] = []
    retained_resolved = {path.resolve() for path in retained_files}
    for row in rows:
        if row["retention"] != "ephemeral" or row["status"] not in {"superseded", "retired"}:
            continue
        path = root / row["path"]
        if path.is_file() and path.resolve() not in retained_resolved and not referenced(path, row["path"]) and _inside(root, path):
            candidates.append(path)
    return candidates


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path)
    parser.add_argument("--status", required=True, choices=sorted(TERMINAL))
    parser.add_argument("--reason")
    parser.add_argument("--prune-ephemeral", action="store_true", help="list safe candidates (dry-run)")
    parser.add_argument("--apply", action="store_true", help="apply --prune-ephemeral after validation")
    args = parser.parse_args(argv)
    if args.apply and not args.prune_ephemeral:
        print("--apply requires --prune-ephemeral", file=sys.stderr)
        return 2
    errors, rows = validate(args.run_dir, args.status, args.reason)
    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1
    candidates = prune_candidates(args.run_dir, rows) if args.prune_ephemeral else []
    pruned: list[str] = []
    for path in candidates:
        rel = path.resolve().relative_to(args.run_dir.resolve()).as_posix()
        print(("PRUNE" if args.apply else "WOULD-PRUNE") + f": {rel}")
        if args.apply:
            path.unlink()
            pruned.append(rel)
    receipt_path = args.run_dir / "RUN_RECEIPT.json"
    receipt = json.loads(receipt_path.read_text())
    listed = {row["path"] for row in rows}
    unclassified = sorted(
        path.relative_to(args.run_dir).as_posix()
        for path in args.run_dir.rglob("*")
        if path.is_file()
        and path.relative_to(args.run_dir).as_posix() not in SCAFFOLD
        and path.relative_to(args.run_dir).as_posix() not in listed
    )
    for rel in unclassified:
        print(f"RETAIN-UNCLASSIFIED: {rel}")
    receipt.update({
        "status": args.status,
        "closed_at": receipt.get("closed_at") or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "terminal_reason": args.reason,
        "unclassified_paths": unclassified,
        "pruned_paths": sorted(set(receipt.get("pruned_paths", [])) | set(pruned)),
    })
    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"PASS: run terminalised as {args.status}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
