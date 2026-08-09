"""Lifecycle and review helpers for the delivery receipt producer."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any

TRANSITIONS = {
    "draft": {"scoped"}, "scoped": {"approved"}, "approved": {"executing"},
    "executing": {"verifying"}, "verifying": {"reviewing", "executing"},
    "reviewing": {"repairing", "awaiting_acceptance"}, "repairing": {"verifying"},
    "awaiting_acceptance": {"accepted", "repairing"}, "accepted": {"awaiting_release"},
    "awaiting_release": {"observing"}, "observing": {"closed"}, "closed": set(),
}


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
) -> tuple[dict[str, Any], str, str, bytes]:
    api["require_identifier"](artifact_id, "review artifact id")
    target, relative = api["safe_workspace_path"](
        workspace, path, "review artifact path",
    )
    api["ensure_allowed_source_target"](run, workspace, target)
    api["ensure_allowed_artifact_target"](run, workspace, target)
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
        return existing[0], relative, digest, raw
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
    return item, relative, digest, raw


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


def artifact_corroborates_measurement(
    value: Any, gate: str, measured_value: float,
) -> bool:
    if not isinstance(value, dict) or value.get("gate") != gate:
        return False
    recorded = value.get("measured_value")
    return (
        not isinstance(recorded, bool)
        and isinstance(recorded, (int, float))
        and math.isfinite(recorded)
        and recorded == measured_value
    )


def command_evidence_observation(
    args: Any, api: dict[str, Any],
) -> dict[str, Any]:
    evidence_id = api["require_identifier"](args.evidence_id, "evidence id")
    if not math.isfinite(args.measured_value):
        raise api["ReceiptError"]("observation measured value must be finite")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        api["ensure_new_evidence_id"](run, evidence_id)
        artifact = api["find_artifact"](run, args.artifact_id)
        if not artifact.get("path"):
            raise api["ReceiptError"]("observation evidence must use a local artifact")
        target, _relative = api["safe_workspace_path"](
            workspace, artifact["path"], "observation evidence artifact",
        )
        raw = target.read_bytes() if target.is_file() else b""
        if not raw or artifact.get("digest") != api["digest_bytes"](raw):
            raise api["ReceiptError"](
                "observation evidence artifact must match existing non-empty bytes"
            )
        try:
            artifact_content = json.loads(raw)
        except (TypeError, UnicodeDecodeError, json.JSONDecodeError):
            artifact_content = None
        if not artifact_corroborates_measurement(
            artifact_content, args.gate, args.measured_value,
        ):
            raise api["ReceiptError"](
                "observation evidence artifact does not corroborate measured value "
                f"{args.measured_value:g}"
            )
        sources: list[str] = []
        for source in args.sources:
            source_target, relative = api["safe_workspace_path"](
                workspace, source, "observation evidence source",
            )
            api["ensure_allowed_source_target"](run, workspace, source_target)
            if not source_target.exists():
                raise api["ReceiptError"](
                    f"observation evidence source does not exist: {source}"
                )
            sources.append(relative)
        run["evidence"].append({
            "id": evidence_id,
            "kind": "observation",
            "gate": args.gate,
            "status": "pass",
            "method": "recorded observation measurement",
            "artifact_id": args.artifact_id,
            "source_paths": list(dict.fromkeys(sources)),
            "observed_at": api["utc_now"](),
            "measured_value": args.measured_value,
        })
        return {"evidence_id": evidence_id, "status": "pass"}

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


def artifact_map(run: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {
        item["id"]: item for item in run.get("artifacts", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    }


def review_ladder_error(run: dict[str, Any]) -> str | None:
    legs = [{
        "role": item.get("role"),
        "family": item.get("provider_family"),
        "status": "omitted" if item.get("status") == "skipped" else item.get("status"),
        "lenses": item.get("lenses", []),
        "reason": item.get("reason"),
    } for item in run.get("reviews", []) if isinstance(item, dict)]
    risk_tier = run.get("risk_tier")
    if risk_tier not in {"substantial", "crucial", "terminal"}:
        return None
    targeted = [
        item for item in legs
        if item.get("role") == "targeted" and item.get("status") == "pass"
    ]
    lenses = {
        lens for item in targeted for lens in item.get("lenses", [])
        if isinstance(lens, str) and lens
    }
    minimum = 3 if risk_tier == "terminal" else 2
    if len(lenses) < minimum:
        return f"{risk_tier} review requires at least {minimum} targeted lenses"
    primary = [
        item for item in legs
        if item.get("role") == "other-primary" and item.get("status") == "pass"
    ]
    chair_family = run.get("chair_family")
    if not primary:
        return "substantial+ review requires passing other-primary coverage"
    if primary[0].get("family") not in {"openai", "anthropic"}:
        return "other-primary review must use a primary family"
    if chair_family and primary[0].get("family") == chair_family:
        return "other-primary review must use a distinct primary family"
    distinct = [
        item for item in legs
        if item.get("role") == "distinct-family" and item.get("status") == "pass"
    ]
    if any(item.get("family") in {"openai", "anthropic", chair_family} for item in distinct):
        return "distinct-family review must use a non-primary family"
    if risk_tier == "terminal" and not any(
        marker in lens.lower() for lens in lenses for marker in ("adversarial", "challenge")
    ):
        return "terminal review requires adversarial targeted pressure"
    if risk_tier in {"crucial", "terminal"} and not distinct and not any(
        item.get("role") == "distinct-family"
        and item.get("status") in {"failed", "unavailable", "omitted"}
        and item.get("reason")
        for item in legs
    ):
        return f"{risk_tier} review requires a distinct-family review or recorded skip"
    errors = []
    return errors[0] if errors else None


def required_profile_evidence(
    run: dict[str, Any], profile: dict[str, Any],
) -> dict[str, set[str]]:
    required = {
        kind: set(gates) for kind, gates in profile["required_evidence"].items()
    }
    conditional = profile.get("conditional_evidence", {})
    for artifact in artifact_map(run).values():
        if artifact.get("class") != "canonical":
            continue
        for kind, gates in conditional.get(artifact.get("artifact_type"), {}).items():
            required[kind].update(gates)
    return required


def measures_gate_ready(
    run: dict[str, Any], profile: dict[str, Any], cited: set[str],
) -> bool:
    evidence = {
        item.get("id"): item for item in run.get("evidence", [])
        if isinstance(item, dict)
    }
    measures = run.get("measures", {})
    if not isinstance(measures, dict):
        return False
    for kind in ("outcome", "trajectory"):
        rows = measures.get(kind)
        if not isinstance(rows, list) or any(
            not isinstance(item, dict) for item in rows
        ):
            return False
        ids = [item.get("id") for item in rows]
        if (
            any(not item_id for item_id in ids)
            or len(ids) != len(set(ids))
            or not set(profile["required_measures"][kind]) <= set(ids)
        ):
            return False
        for item in rows:
            linked = evidence.get(item.get("evidence_id"))
            if (
                item.get("status") != "pass"
                or not linked
                or linked.get("status") != "pass"
                or item.get("evidence_kind") != linked.get("kind")
                or item.get("evidence_id") not in cited
                or "value" not in item
                or not item.get("target")
                or not item.get("aggregation")
            ):
                return False
    return True


def observation_gate_ready(
    run: dict[str, Any], cited: set[str], proposed_at: str | None,
) -> bool:
    observation = run.get("observation", {})
    evidence = {
        item.get("id"): item for item in run.get("evidence", [])
        if isinstance(item, dict)
    }
    evidence_ids = observation.get("evidence_ids")
    signals = observation.get("signals")
    thresholds = observation.get("thresholds")
    if (
        observation.get("status") != "pass"
        or not isinstance(evidence_ids, list)
        or not evidence_ids
        or not set(evidence_ids) <= cited
        or not isinstance(signals, list)
        or not signals
        or any(not isinstance(signal, str) or not signal for signal in signals)
        or len(signals) != len(set(signals))
        or not isinstance(thresholds, dict)
        or set(thresholds) != set(signals)
    ):
        return False
    try:
        started = parse_utc(observation.get("started_at"), "observation.started_at", ValueError)
        ended = parse_utc(observation.get("ended_at"), "observation.ended_at", ValueError)
        observing = parse_utc(
            next(
                item["at"] for item in run.get("state_history", [])
                if isinstance(item, dict) and item.get("state") == "observing"
            ),
            "observing transition",
            ValueError,
        )
        proposed = parse_utc(proposed_at, "closed transition", ValueError)
    except (StopIteration, ValueError):
        return False
    if ended <= started or started < observing or ended > proposed:
        return False
    window = observation.get("window")
    if not isinstance(window, dict):
        return False
    if window.get("kind") == "duration":
        minimum = window.get("minimum_seconds")
        if (
            isinstance(minimum, bool)
            or not isinstance(minimum, int)
            or minimum < 1
            or (ended - started).total_seconds() < minimum
        ):
            return False
    elif window.get("kind") == "event-count":
        minimum = window.get("minimum")
        observed = observation.get("observed_events")
        if (
            isinstance(minimum, bool)
            or not isinstance(minimum, int)
            or minimum < 1
            or isinstance(observed, bool)
            or not isinstance(observed, int)
            or observed < minimum
        ):
            return False
    else:
        return False
    covered: set[str] = set()
    for evidence_id in evidence_ids:
        item = evidence.get(evidence_id)
        if (
            not item
            or item.get("kind") != "observation"
            or item.get("status") != "pass"
        ):
            return False
        try:
            observed = parse_utc(
                item.get("observed_at"), f"observation evidence {evidence_id}", ValueError,
            )
        except ValueError:
            return False
        if observed < started or observed > ended:
            return False
        threshold = thresholds.get(item.get("gate"), {})
        value = item.get("measured_value")
        limit = threshold.get("limit") if isinstance(threshold, dict) else None
        if (
            set(threshold) != {"direction", "limit"}
            or threshold.get("direction") not in {"gte", "lte", "eq"}
            or isinstance(value, bool)
            or not isinstance(value, (int, float))
            or not math.isfinite(value)
            or isinstance(limit, bool)
            or not isinstance(limit, (int, float))
            or not math.isfinite(limit)
        ):
            return False
        direction = threshold.get("direction")
        passed = (
            value >= limit if direction == "gte"
            else value <= limit if direction == "lte"
            else value == limit if direction == "eq"
            else False
        )
        if not passed:
            return False
        covered.add(item["gate"])
    return set(signals) <= covered


def transition_gate_error(
    run: dict[str, Any], target: str, cited: set[str], api: dict[str, Any],
    *, proposed_at: str | None = None, workspace: Path | None = None,
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
            design_approval_row = next((
                item for item in passing_evidence(
                    run, gate="design-approval", kind="human",
                )
                if item.get("id") == design.get("evidence")
            ), None)
            design_artifact = artifact_map(run).get(design.get("artifact_id"))
            live_digest = (
                design_artifact.get("digest")
                if isinstance(design_artifact, dict)
                else None
            )
            if workspace is not None and isinstance(design_artifact, dict):
                path = design_artifact.get("path")
                if isinstance(path, str) and path:
                    try:
                        target_path, _relative = api["safe_workspace_path"](
                            workspace, path, "design artifact",
                        )
                        raw = target_path.read_bytes() if target_path.is_file() else b""
                    except OSError:
                        raw = b""
                    live_digest = api["digest_bytes"](raw) if raw else None
            if (
                design.get("status") != "approved"
                or design_approval_row is None
                or design_artifact is None
                or design_approval_row.get("artifact_id") != design.get("artifact_id")
                or design_artifact.get("digest") != live_digest
                or design.get("digest") != live_digest
                or design_approval_row.get("artifact_digest") != live_digest
            ):
                return "approved design gate is not satisfied"
    if target == "reviewing":
        required = required_profile_evidence(run, profile)["deterministic"]
        rows = [
            item for item in run.get("evidence", [])
            if isinstance(item, dict)
            and item.get("status") == "pass"
            and item.get("gate") in required
            and item.get("id") in cited
        ]
        if (
            {item["gate"] for item in rows} != required
            or any(item.get("kind") != "deterministic" for item in rows)
        ):
            return "reviewing deterministic gate is not satisfied"
        latest_repair = next((
            item for item in reversed(run.get("state_history", []))
            if isinstance(item, dict) and item.get("state") == "repairing"
        ), None)
        if latest_repair is not None:
            repair_at = parse_utc(
                latest_repair.get("at"), "repairing transition timestamp",
                api["ReceiptError"],
            )
            for item in rows:
                if parse_utc(
                    item.get("finished_at"),
                    f"deterministic evidence {item['id']} finished_at",
                    api["ReceiptError"],
                ) < repair_at:
                    return (
                        f"reviewing deterministic gate {item['gate']} evidence "
                        f"predates repairing transition at {latest_repair['at']}"
                    )
    if target == "awaiting_acceptance":
        profile_evidence = required_profile_evidence(run, profile)
        required_gates = {
            *profile_evidence["deterministic"],
            *profile_evidence["judgement"],
        }
        rows = [
            item for item in run.get("evidence", [])
            if isinstance(item, dict)
            and item.get("status") == "pass"
            and item.get("gate") in required_gates
        ]
        wrong_kind = any(
            item.get("kind") != (
                "deterministic"
                if item.get("gate") in profile_evidence["deterministic"]
                else "judgement"
            )
            for item in rows
        )
        measures_ready = measures_gate_ready(run, profile, cited)
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
            or wrong_kind
            or not measures_ready
            or not passing_reviews
            or not required_ids <= cited
            or not stochastic_ready
            or review_ladder_error(run) is not None
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
        observation_status = run.get("observation", {}).get("status")
        if (
            gate.get("status") != "approved"
            or not rows
            or gate.get("evidence") not in cited
        ):
            return "observing release gate is not satisfied"
        if observation_status not in {"active", "pass"}:
            return "observing requires observation status active or pass"
    if target == "closed" and not observation_gate_ready(run, cited, proposed_at):
        return "closed observation gate is not satisfied"
    return None


def command_review_add(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    require_identifier = api["require_identifier"]
    receipt_error = api["ReceiptError"]
    review_id = require_identifier(args.review_id, "review id")
    if not args.adapter:
        raise receipt_error("review requires non-empty --adapter")
    if not args.provider_family:
        raise receipt_error("review requires non-empty --provider-family")
    if args.role == "distinct-family" and args.provider_family in {"openai", "anthropic"}:
        raise receipt_error("distinct-family review must use a non-primary family")
    if args.status == "pass":
        missing = [
            option for option in ("artifact", "route_receipt", "reviewer_id", "model")
            if not getattr(args, option)
        ]
        if missing:
            raise receipt_error(
                "passing review requires " + ", ".join(
                    f"--{option.replace('_', '-')}" for option in missing
                )
            )
        require_identifier(args.reviewer_id, "reviewer-id")
    elif not args.reason:
        raise receipt_error(f"{args.status} review requires --reason")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        if any(
            isinstance(item, dict) and item.get("id") == review_id
            for item in [*run.get("reviews", []), *run.get("evidence", [])]
        ):
            raise receipt_error(f"review id already exists: {review_id}")
        if not args.lenses or any(not lens for lens in args.lenses):
            raise receipt_error("review requires at least one non-empty lens")
        if args.status != "pass":
            run["reviews"].append({
                "id": review_id, "role": args.role,
                "provider_family": args.provider_family, "adapter": args.adapter,
                "model": args.model or "", "reviewer_id": args.reviewer_id or "",
                "independent_of_authorship": True,
                "lenses": list(dict.fromkeys(args.lenses)), "status": args.status,
                "evidence_id": "", "reason": args.reason,
                "route_receipt_digest": "",
            })
            return {"review_id": review_id, "evidence_id": ""}
        review_artifact_id = f"{review_id}.artifact"
        route_artifact_id = f"{review_id}.route"
        _artifact, review_path, review_digest, _review_raw = api["add_review_artifact"](
            run, workspace, artifact_id=review_artifact_id, path=args.artifact,
        )
        _route, route_path, route_digest, route_raw = api["add_review_artifact"](
            run, workspace, artifact_id=route_artifact_id, path=args.route_receipt,
        )
        try:
            route = json.loads(route_raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise receipt_error(f"route receipt must be readable JSON: {exc}") from exc
        if (
            not isinstance(route, dict) or route.get("status") != "ok"
            or route.get("adapter") != args.adapter
            or route.get("reviewer_id") != args.reviewer_id
            or route.get("resolved_model", route.get("model")) != args.model
            or route.get("model_family") != args.provider_family
            or route.get("certification_eligible") is not True
        ):
            raise receipt_error("route receipt identity does not match review lineage")
        if args.role == "other-primary" and route.get("cross_family") is not True:
            raise receipt_error("other-primary review requires a cross-family route receipt")
        lineage = {
            "adapter": args.adapter, "provider_family": args.provider_family,
            "model": args.model,
        }
        run["evidence"].append({
            "id": review_id, "kind": "judgement",
            "gate": api["profile_judgement_gate"](run["profile"]), "status": "pass",
            "method": f"independent review artifact by {args.reviewer_id}",
            "artifact_id": review_artifact_id, "source_paths": [review_path, route_path],
            "model_lineage": lineage, "reviewer_id": args.reviewer_id,
            "review_artifact_digest": review_digest,
            "route_receipt": {"path": route_path, "digest": route_digest},
            "recorded_at": api["utc_now"](),
        })
        run["reviews"].append({
            "id": review_id, "role": args.role,
            "provider_family": args.provider_family, "adapter": args.adapter,
            "model": args.model, "reviewer_id": args.reviewer_id,
            "independent_of_authorship": True,
            "lenses": list(dict.fromkeys(args.lenses)), "status": "pass",
            "evidence_id": review_id, "reason": "",
            "route_receipt_digest": route_digest,
        })
        return {"review_id": review_id, "evidence_id": review_id}

    return api["mutate_run"](args.run_dir, apply)


def build_parser(api: dict[str, Any]) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=api["__doc__"])
    subcommands = parser.add_subparsers(dest="command", required=True)
    init = subcommands.add_parser("init")
    for name in ("run-dir", "run-id", "profile", "chair-family", "risk-assessment", "intent", "authority"):
        init.add_argument(f"--{name}", required=True)
    init.add_argument("--fabric-relationships")
    init.add_argument("--risk-tier", choices=api["RISKS"])
    init.add_argument("--risk-override")
    init.set_defaults(handler=api["command_init"])

    bind = subcommands.add_parser("bind")
    bind.add_argument("--run-dir", required=True)
    bind.add_argument("--section", choices=sorted(api["BIND_SECTIONS"]), required=True)
    bind.add_argument("--from", dest="from_json", required=True)
    bind.set_defaults(handler=api["command_bind"])

    artifact = subcommands.add_parser("artifact")
    artifact_commands = artifact.add_subparsers(dest="artifact_command", required=True)
    artifact_add = artifact_commands.add_parser("add")
    artifact_add.add_argument("--run-dir", required=True)
    artifact_add.add_argument("--id", dest="artifact_id", required=True)
    artifact_add.add_argument("--path", required=True)
    artifact_add.add_argument("--class", dest="artifact_class", choices=sorted(api["SAFE_CLASSES"]), required=True)
    artifact_add.add_argument("--media-type", required=True)
    artifact_add.add_argument("--artifact-type", required=True)
    artifact_add.add_argument("--owner", required=True)
    artifact_add.add_argument("--retention", required=True)
    artifact_add.set_defaults(handler=api["command_artifact_add"])

    evidence = subcommands.add_parser("evidence")
    evidence_commands = evidence.add_subparsers(dest="evidence_command", required=True)
    evidence_run = evidence_commands.add_parser("run")
    evidence_run.add_argument("--run-dir", required=True)
    evidence_run.add_argument("--id", dest="evidence_id", required=True)
    evidence_run.add_argument("--gate", required=True)
    evidence_run.add_argument("--artifact-id", required=True)
    evidence_run.add_argument("--source", dest="sources", action="append", required=True)
    evidence_run.add_argument("command_args", nargs=argparse.REMAINDER)
    evidence_run.set_defaults(handler=api["command_evidence_run"])

    evidence_human = evidence_commands.add_parser("human")
    evidence_human.add_argument("--run-dir", required=True)
    evidence_human.add_argument("--id", dest="evidence_id", required=True)
    evidence_human.add_argument("--gate", required=True)
    evidence_human.add_argument("--artifact-id", required=True)
    evidence_human.add_argument("--approver", required=True)
    evidence_human.add_argument("--source", dest="sources", action="append", default=[])
    evidence_human.set_defaults(handler=api["command_evidence_human"])

    observation = evidence_commands.add_parser("observation")
    observation.add_argument("--run-dir", required=True)
    observation.add_argument("--id", dest="evidence_id", required=True)
    observation.add_argument("--gate", required=True)
    observation.add_argument("--artifact-id", required=True)
    observation.add_argument("--measured-value", type=float, required=True)
    observation.add_argument("--source", dest="sources", action="append", required=True)
    observation.set_defaults(handler=lambda args: command_evidence_observation(args, api))

    remove = evidence_commands.add_parser("remove")
    remove.add_argument("--run-dir", required=True)
    remove.add_argument("--id", dest="evidence_id", required=True)
    remove.set_defaults(handler=api["command_evidence_remove"])

    rebuild = evidence_commands.add_parser("rebuild")
    rebuild.add_argument("--run-dir", required=True)
    rebuild.add_argument("--artifact-id", required=True)
    rebuild.set_defaults(handler=api["command_evidence_rebuild"])

    review = subcommands.add_parser("review")
    review_commands = review.add_subparsers(dest="review_command", required=True)
    review_add = review_commands.add_parser("add")
    review_add.add_argument("--run-dir", required=True)
    review_add.add_argument("--id", dest="review_id", required=True)
    review_add.add_argument("--role", choices=("distinct-family", "other-primary", "targeted"), required=True)
    review_add.add_argument("--artifact")
    review_add.add_argument("--route-receipt")
    review_add.add_argument("--reviewer-id")
    review_add.add_argument("--adapter", required=True)
    review_add.add_argument("--provider-family", required=True)
    review_add.add_argument("--model")
    review_add.add_argument("--lens", dest="lenses", action="append", required=True)
    review_add.add_argument("--status", choices=("pass", "failed", "unavailable", "skipped"), default="pass")
    review_add.add_argument("--reason", default="")
    review_add.set_defaults(handler=lambda args: command_review_add(args, api))

    checkpoint = subcommands.add_parser("checkpoint")
    checkpoint_commands = checkpoint.add_subparsers(dest="checkpoint_command", required=True)
    checkpoint_set = checkpoint_commands.add_parser("set")
    checkpoint_set.add_argument("--run-dir", required=True)
    checkpoint_set.add_argument("--current-slice", required=True)
    checkpoint_set.add_argument("--next-action", required=True)
    checkpoint_set.add_argument("--in-flight", action="append", default=[])
    checkpoint_set.add_argument("--artifact", dest="artifacts", action="append", default=[])
    checkpoint_set.set_defaults(handler=lambda args: command_checkpoint_set(args, api))

    transition = subcommands.add_parser("transition")
    transition.add_argument("--run-dir", required=True)
    transition.add_argument("--to", dest="target", required=True)
    transition.add_argument("--evidence", dest="evidence_ids", action="append", default=[])
    transition.set_defaults(handler=lambda args: command_transition(args, api))

    show = subcommands.add_parser("show")
    show.add_argument("--run-dir", required=True)
    show.set_defaults(handler=lambda args: command_show(args, api))
    return parser


def command_transition(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    evidence_ids = list(dict.fromkeys(args.evidence_ids))

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        current = run.get("status")
        if args.target not in TRANSITIONS.get(current, set()):
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
        gate_error = transition_gate_error(
            run, args.target, set(evidence_ids), api, proposed_at=timestamp,
            workspace=workspace,
        )
        if gate_error:
            raise api["ReceiptError"](gate_error)
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
        if args.target == "closed":
            checkpoint["in_flight"] = []
        return {"from": current, "to": args.target, "at": timestamp}

    return api["mutate_run"](args.run_dir, apply)


def command_show(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    run_dir, _workspace = api["resolve_run_dir"](args.run_dir)
    with api["run_lock"](run_dir):
        return {"_raw_receipt": api["load_run"](run_dir)}
