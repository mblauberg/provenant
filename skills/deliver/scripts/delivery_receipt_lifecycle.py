"""Lifecycle and review helpers for the delivery receipt producer."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def profile_judgement_gate(
    profile: str, profile_path: Path, error_type: type[ValueError],
) -> str:
    try:
        registry = json.loads(profile_path.read_text())
        gates = registry["profiles"][profile]["required_evidence"]["judgement"]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise error_type(f"delivery profile registry is unreadable: {exc}") from exc
    if not isinstance(gates, list) or not gates or not isinstance(gates[0], str):
        raise error_type(f"profile {profile} has no judgement evidence gate")
    return gates[0]


def add_review_artifact(
    run: dict[str, Any],
    workspace: Path,
    *,
    artifact_id: str,
    path: str,
    api: dict[str, Any],
) -> tuple[dict[str, Any], str, str]:
    api["require_identifier"](artifact_id, "review artifact id")
    target, relative = api["safe_workspace_path"](
        workspace, path, "review artifact path",
    )
    raw = target.read_bytes() if target.is_file() else b""
    if not raw:
        raise api["ReceiptError"](
            f"review artifact must be existing and non-empty: {path}"
        )
    digest = api["digest_bytes"](raw)
    existing = [
        item for item in run.get("artifacts", [])
        if isinstance(item, dict) and item.get("id") == artifact_id
    ]
    if existing:
        if existing[0].get("path") != relative or existing[0].get("digest") != digest:
            raise api["ReceiptError"](
                f"review artifact id conflicts with live bytes: {artifact_id}"
            )
        return existing[0], relative, digest
    item = {
        "id": artifact_id,
        "path": relative,
        "media_type": "application/json" if target.suffix == ".json" else "text/markdown",
        "artifact_type": "evidence",
        "digest": digest,
        "class": "evidence",
        "owner": "delivery-chair",
        "retention": "risk-policy",
    }
    run["artifacts"].append(item)
    return item, relative, digest


def command_checkpoint_set(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        checkpoint = run.get("checkpoint")
        if not isinstance(checkpoint, dict):
            raise api["ReceiptError"]("checkpoint must be an object")
        generation = checkpoint.get("generation")
        if (
            isinstance(generation, bool)
            or not isinstance(generation, int)
            or generation < 0
        ):
            raise api["ReceiptError"](
                "checkpoint.generation must be a non-negative integer"
            )
        in_flight = list(dict.fromkeys(args.in_flight))
        if any(not value for value in in_flight):
            raise api["ReceiptError"]("in-flight IDs must be non-empty")
        existing = checkpoint.get("artifact_paths")
        if not isinstance(existing, list):
            raise api["ReceiptError"]("checkpoint.artifact_paths must be a list")
        additions: list[str] = []
        for artifact in args.artifacts:
            target, relative = api["safe_workspace_path"](
                workspace, artifact, "checkpoint artifact",
            )
            if not target.is_file():
                raise api["ReceiptError"](
                    f"checkpoint artifact does not exist: {artifact}"
                )
            additions.append(relative)
        checkpoint.update({
            "generation": generation + 1,
            "current_slice": args.current_slice,
            "next_action": args.next_action,
            "in_flight": in_flight,
            "artifact_paths": list(dict.fromkeys([*existing, *additions])),
        })
        return {"generation": generation + 1}

    return api["mutate_run"](args.run_dir, apply)


def parse_utc(value: Any, field: str, error_type: type[ValueError]):
    if not isinstance(value, str) or not value.endswith("Z"):
        raise error_type(f"{field} must be an ISO UTC timestamp")
    try:
        return api_datetime().fromisoformat(value[:-1])
    except ValueError as exc:
        raise error_type(f"{field} must be an ISO UTC timestamp") from exc


def api_datetime():
    from datetime import datetime

    return datetime


def profile_requirements(
    run: dict[str, Any], profile_path: Path, error_type: type[ValueError],
) -> dict[str, Any]:
    try:
        return json.loads(profile_path.read_text())["profiles"][run["profile"]]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise error_type(f"delivery profile registry is unreadable: {exc}") from exc


def passing_evidence(
    run: dict[str, Any], *, gate: str, kind: str | None = None,
) -> list[dict[str, Any]]:
    return [
        item for item in run.get("evidence", [])
        if isinstance(item, dict)
        and item.get("gate") == gate
        and item.get("status") == "pass"
        and (kind is None or item.get("kind") == kind)
    ]


def transition_gate_error(
    run: dict[str, Any], target: str, cited: set[str], api: dict[str, Any],
) -> str | None:
    profile = profile_requirements(run, api["PROFILE_PATH"], api["ReceiptError"])
    if target == "approved":
        approval = run.get("intent", {}).get("approval", {})
        if (
            approval.get("status") != "approved"
            or not passing_evidence(run, gate="intent-approval", kind="human")
        ):
            return "approved gate is not satisfied"
        if run["risk_tier"] in {"substantial", "crucial", "terminal"}:
            design = run.get("design", {})
            if (
                design.get("status") != "approved"
                or not passing_evidence(run, gate="design-approval", kind="human")
            ):
                return "approved design gate is not satisfied"
    if target == "reviewing":
        required = set(profile["required_evidence"]["deterministic"])
        rows = [
            item for gate in required
            for item in passing_evidence(run, gate=gate, kind="deterministic")
        ]
        if {item["gate"] for item in rows} != required or not {
            item["id"] for item in rows
        } <= cited:
            return "reviewing deterministic gate is not satisfied"
    if target == "awaiting_acceptance":
        required_gates = {
            *profile["required_evidence"]["deterministic"],
            *profile["required_evidence"]["judgement"],
        }
        rows = [
            item for gate in required_gates
            for item in passing_evidence(run, gate=gate)
        ]
        measures = run.get("measures", {})
        measures_ready = all(
            set(profile["required_measures"][kind]) <= {
                item.get("id") for item in measures.get(kind, [])
                if isinstance(item, dict) and item.get("status") == "pass"
            }
            for kind in ("outcome", "trajectory")
        )
        passing_reviews = [
            item for item in run.get("reviews", [])
            if isinstance(item, dict) and item.get("status") == "pass"
        ]
        required_ids = {item["id"] for item in rows} | {
            item.get("evidence_id") for item in passing_reviews
        }
        assurance = run.get("assurance", {})
        stochastic_ready = (
            assurance.get("stochastic_required") is not True
            or any(
                isinstance(item, dict) and item.get("status") == "complete"
                for item in assurance.get("evaluations", [])
            )
        )
        if (
            {item["gate"] for item in rows} != required_gates
            or not measures_ready
            or not passing_reviews
            or not required_ids <= cited
            or not stochastic_ready
        ):
            return "awaiting_acceptance gate is not satisfied"
    if target == "accepted":
        gate = run.get("human_gates", {}).get("acceptance", {})
        rows = passing_evidence(run, gate="human-acceptance", kind="human")
        if (
            gate.get("status") != "approved"
            or not rows
            or gate.get("evidence") not in cited
        ):
            return "accepted human gate is not satisfied"
    if target == "observing":
        gate = run.get("human_gates", {}).get("release", {})
        rows = passing_evidence(run, gate="human-release", kind="human")
        if (
            gate.get("status") != "approved"
            or not rows
            or gate.get("evidence") not in cited
        ):
            return "observing release gate is not satisfied"
    if target == "closed":
        observation = run.get("observation", {})
        if (
            observation.get("status") != "complete"
            or not set(observation.get("evidence_ids", [])) <= cited
        ):
            return "closed observation gate is not satisfied"
    return None


def command_transition(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    evidence_ids = list(dict.fromkeys(args.evidence_ids))

    def apply(run: dict[str, Any], _run_dir: Path, _workspace: Path) -> dict[str, Any]:
        current = run.get("status")
        if args.target not in api["TRANSITIONS"].get(current, set()):
            raise api["ReceiptError"](
                f"invalid lifecycle transition {current} -> {args.target}"
            )
        known = {
            item.get("id") for item in run.get("evidence", [])
            if isinstance(item, dict)
        }
        unknown = set(evidence_ids) - known
        if unknown:
            raise api["ReceiptError"](
                f"transition references unknown evidence ids: {', '.join(sorted(unknown))}"
            )
        gate_error = transition_gate_error(run, args.target, set(evidence_ids), api)
        if gate_error:
            raise api["ReceiptError"](gate_error)
        history = run.get("state_history")
        if not isinstance(history, list) or not history:
            raise api["ReceiptError"]("state_history must be non-empty")
        timestamp = api["utc_now"]()
        if parse_utc(
            timestamp, "transition timestamp", api["ReceiptError"],
        ) <= parse_utc(
            history[-1].get("at"),
            "previous transition timestamp",
            api["ReceiptError"],
        ):
            raise api["ReceiptError"]("transition timestamp must strictly increase")
        history.append({
            "state": args.target,
            "at": timestamp,
            "evidence_ids": evidence_ids,
            "risk_tier": run["risk_tier"],
        })
        run["status"] = args.target
        run["repair_cycles"] = sum(
            isinstance(item, dict) and item.get("state") == "repairing"
            for item in history
        )
        checkpoint = run.get("checkpoint")
        if (
            not isinstance(checkpoint, dict)
            or not isinstance(checkpoint.get("generation"), int)
        ):
            raise api["ReceiptError"]("checkpoint is invalid")
        checkpoint["generation"] += 1
        checkpoint["current_slice"] = args.target.replace("_", "-")
        checkpoint["next_action"] = f"continue from {args.target}"
        return {"from": current, "to": args.target, "at": timestamp}

    return api["mutate_run"](args.run_dir, apply)


def command_show(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    run_dir, _workspace = api["resolve_run_dir"](args.run_dir)
    with api["run_lock"](run_dir):
        return {"_raw_receipt": api["load_run"](run_dir)}
