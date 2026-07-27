"""Select only among hard-admissible model routes using soft preferences."""

from __future__ import annotations

import argparse
from copy import deepcopy
import fcntl
import json
import os
from pathlib import Path
from typing import Any


AFFINITY_FIELDS = {
    "family_affinity": ("model_family", "unknown_family"),
    "model_affinity": ("model", "unknown_model"),
    "adapter_affinity": ("adapter", "unknown_adapter"),
}
DEPRIORITISE_FIELDS = {
    "families": "model_family",
    "models": "model",
    "adapters": "adapter",
}
DEPRIORITISE_KINDS = {
    "families": ("family_deprioritise", "model_family", "unknown_family"),
    "models": ("model_deprioritise", "model", "unknown_model"),
    "adapters": ("adapter_deprioritise", "adapter", "unknown_adapter"),
}
TOP_LEVEL_FIELDS = {
    "schema_version",
    "task_classes",
    "roles",
    "deprioritise",
    "avoid",
    "spreading",
}


def add_selection_parser(
    commands: argparse._SubParsersAction[argparse.ArgumentParser],
    default_preferences_path: Path,
) -> None:
    command = commands.add_parser("select")
    command.add_argument("--task-class", required=True)
    command.add_argument("--role", required=True)
    command.add_argument("--candidates-file", required=True)
    command.add_argument(
        "--preferences-file",
        default=str(default_preferences_path),
    )
    command.add_argument("--spread-state-file", required=True)


def _read_json(path: Path) -> Any:
    return json.loads(path.read_text())


def _route_model(route: dict[str, Any]) -> str:
    catalog_model = route.get("catalog_model")
    if isinstance(catalog_model, str) and catalog_model:
        return catalog_model
    model = route.get("resolved_model")
    return model if isinstance(model, str) else ""


def _availability(value: Any) -> dict[str, str] | None:
    if isinstance(value, dict):
        observation = value.get("observation")
        if observation == "Observed" and value.get("value") in {
            "available",
            "unavailable",
        }:
            return {"observation": "Observed", "value": value["value"]}
        if observation == "Unknown" and isinstance(value.get("reason"), str):
            return {"observation": "Unknown", "reason": value["reason"]}
        if observation == "Unobserved":
            return {"observation": "Unobserved"}
        return None
    return {
        "observation": "Unknown",
        "reason": "AvailabilityNotObserved",
    }


def _normalise_candidates(
    raw: Any,
    task_class: str,
    role: str,
    policy: dict[str, str],
    alias_order: dict[str, int],
    effort_order: dict[str, int],
) -> tuple[list[dict[str, Any]], str]:
    if not isinstance(raw, dict) or raw.get("schema_version") != 1:
        return [], "candidate_set_invalid"
    candidates = raw.get("candidates")
    if not isinstance(candidates, list):
        return [], "candidate_set_invalid"
    normalised: list[dict[str, Any]] = []
    identifiers: set[str] = set()
    for candidate in candidates:
        if not isinstance(candidate, dict):
            return [], "candidate_set_invalid"
        candidate_id = candidate.get("candidate_id")
        route = candidate.get("route")
        if (
            not isinstance(candidate_id, str)
            or not candidate_id
            or candidate_id in identifiers
            or not isinstance(route, dict)
        ):
            return [], "candidate_set_invalid"
        identifiers.add(candidate_id)
        availability = _availability(candidate.get("availability"))
        if availability is None:
            return [], "candidate_set_invalid"
        alias = route.get("alias")
        requested_effort = route.get("requested_effort")
        effort = route.get("effort")
        hard_policy_match = (
            route.get("schema_version") == 1
            and route.get("status") == "ok"
            and route.get("task_class") == task_class
            and route.get("route_source") == "task-class"
            and route.get("role") == role == policy["role"]
            and alias in alias_order
            and alias_order[alias] >= alias_order[policy["minimum_alias"]]
            and requested_effort in effort_order
            and effort in effort_order
            and effort_order[requested_effort]
            >= effort_order[policy["minimum_effort"]]
            and effort_order[effort] >= effort_order[policy["minimum_effort"]]
        )
        if route.get("status") != "ok":
            disposition = str(route.get("status") or "hard_route_rejected")
        elif not hard_policy_match:
            disposition = "hard_policy_mismatch"
        elif (
            availability.get("observation") == "Observed"
            and availability.get("value") == "unavailable"
        ):
            disposition = "observed_unavailable"
        else:
            disposition = "admissible"
        normalised.append({
            "candidate_id": candidate_id,
            "adapter": route.get("adapter") if isinstance(route.get("adapter"), str) else "",
            "model_family": (
                route.get("model_family")
                if isinstance(route.get("model_family"), str)
                else ""
            ),
            "model": _route_model(route),
            "route": deepcopy(route),
            "availability": availability,
            "admissible": disposition == "admissible",
            "selected": False,
            "disposition": disposition,
        })
    return normalised, ""


def _load_preferences(path: Path) -> tuple[dict[str, Any], str]:
    try:
        value = _read_json(path)
    except json.JSONDecodeError:
        return {}, "corrupt"
    except OSError:
        return {}, "unavailable"
    if not isinstance(value, dict):
        return {}, "invalid"
    return value, "loaded"


def _string_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, str) and item]


def _preference_items(
    preferences: dict[str, Any],
    task_class: str,
    role: str,
) -> list[dict[str, str]]:
    items: list[dict[str, str]] = []
    for scope, table_name, key in (
        ("task_class", "task_classes", task_class),
        ("role", "roles", role),
    ):
        table = preferences.get(table_name)
        scoped = table.get(key) if isinstance(table, dict) else None
        if not isinstance(scoped, dict):
            continue
        for kind in AFFINITY_FIELDS:
            for value in _string_list(scoped.get(kind)):
                items.append({
                    "kind": kind,
                    "scope": scope,
                    "value": value,
                })
    for section_name in ("deprioritise", "avoid"):
        section = preferences.get(section_name)
        if not isinstance(section, dict):
            continue
        for field, (kind, _, _) in DEPRIORITISE_KINDS.items():
            for value in _string_list(section.get(field)):
                items.append({
                    "kind": kind,
                    "scope": section_name,
                    "value": value,
                })
    return items


def _configuration_ignored(
    preferences: dict[str, Any],
    task_class: str,
    role: str,
) -> list[dict[str, str]]:
    ignored = [
        {
            "kind": key,
            "scope": "top_level",
            "value": key,
            "reason": "unknown_key",
        }
        for key in sorted(set(preferences) - TOP_LEVEL_FIELDS)
    ]
    for scope, table_name, key in (
        ("task_class", "task_classes", task_class),
        ("role", "roles", role),
    ):
        table = preferences.get(table_name)
        scoped = table.get(key) if isinstance(table, dict) else None
        if isinstance(scoped, dict):
            ignored.extend(
                {
                    "kind": item,
                    "scope": scope,
                    "value": item,
                    "reason": "unknown_key",
                }
                for item in sorted(set(scoped) - set(AFFINITY_FIELDS))
            )
    for section_name in ("deprioritise", "avoid"):
        section = preferences.get(section_name)
        if isinstance(section, dict):
            ignored.extend(
                {
                    "kind": item,
                    "scope": section_name,
                    "value": item,
                    "reason": "unknown_key",
                }
                for item in sorted(set(section) - set(DEPRIORITISE_KINDS))
            )
    spreading = preferences.get("spreading")
    if isinstance(spreading, dict):
        ignored.extend(
            {
                "kind": item,
                "scope": "spreading",
                "value": item,
                "reason": "unknown_key",
            }
            for item in sorted(set(spreading) - {"policy"})
        )
        policy = spreading.get("policy")
        if policy is not None and policy != "fair-round-robin":
            ignored.append({
                "kind": "spreading_policy",
                "scope": "spreading",
                "value": str(policy),
                "reason": "unsupported_policy",
            })
    return ignored


def _deprioritised(preferences: dict[str, Any]) -> dict[str, set[str]]:
    result = {attribute: set() for attribute in DEPRIORITISE_FIELDS.values()}
    for section_name in ("deprioritise", "avoid"):
        section = preferences.get(section_name)
        if not isinstance(section, dict):
            continue
        for field, attribute in DEPRIORITISE_FIELDS.items():
            result[attribute].update(_string_list(section.get(field)))
    return result


def _rank_maps(
    items: list[dict[str, str]],
) -> dict[tuple[str, str], dict[str, int]]:
    ranks: dict[tuple[str, str], dict[str, int]] = {}
    for item in items:
        if item["kind"] not in AFFINITY_FIELDS:
            continue
        key = (item["scope"], AFFINITY_FIELDS[item["kind"]][0])
        values = ranks.setdefault(key, {})
        values.setdefault(item["value"], len(values))
    return ranks


def _preference_rank(
    candidate: dict[str, Any],
    ranks: dict[tuple[str, str], dict[str, int]],
    deprioritised: dict[str, set[str]],
) -> tuple[Any, ...]:
    fallback = 1_000_000
    affinity = tuple(
        ranks.get((scope, attribute), {}).get(candidate[attribute], fallback)
        for scope in ("task_class", "role")
        for attribute in ("model_family", "adapter", "model")
    )
    avoided = tuple(
        int(candidate[attribute] in deprioritised[attribute])
        for attribute in ("model_family", "adapter", "model")
    )
    return (
        *affinity,
        *avoided,
        candidate["model_family"],
        candidate["adapter"],
        candidate["model"],
        candidate["candidate_id"],
    )


def _preference_receipt(
    items: list[dict[str, str]],
    candidates: list[dict[str, Any]],
    chosen: dict[str, Any],
    least_used: list[dict[str, Any]],
) -> tuple[list[dict[str, str]], list[dict[str, str]], list[dict[str, str]]]:
    applied: list[dict[str, str]] = []
    not_honoured: list[dict[str, str]] = []
    ignored: list[dict[str, str]] = []
    for item in items:
        if item["kind"] in AFFINITY_FIELDS:
            attribute, unknown_reason = AFFINITY_FIELDS[item["kind"]]
            deprioritise = False
        else:
            _, attribute, unknown_reason = next(
                value
                for value in DEPRIORITISE_KINDS.values()
                if value[0] == item["kind"]
            )
            deprioritise = True
        matching = [
            candidate
            for candidate in candidates
            if candidate[attribute] == item["value"]
        ]
        if not matching:
            ignored.append({**item, "reason": unknown_reason})
        elif not any(candidate["admissible"] for candidate in matching):
            if deprioritise:
                applied.append(item)
                continue
            if item["kind"] == "adapter_affinity" and any(
                candidate["disposition"] == "adapter_disabled"
                for candidate in matching
            ):
                reason = "adapter_disabled"
            elif item["kind"] in {"family_affinity", "family_deprioritise"}:
                reason = "no_admissible_candidate"
            else:
                reason = "outside_admissible_set"
            not_honoured.append({**item, "reason": reason})
        elif deprioritise and chosen[attribute] != item["value"]:
            applied.append(item)
        elif not deprioritise and chosen[attribute] == item["value"]:
            applied.append(item)
        else:
            admissible = [candidate for candidate in candidates if candidate["admissible"]]
            if deprioritise and len(admissible) == 1:
                reason = "sole_admissible_candidate"
            elif deprioritise:
                alternatives = [
                    candidate
                    for candidate in admissible
                    if candidate[attribute] != item["value"]
                ]
                reason = (
                    "lost_to_higher_preference"
                    if any(candidate in least_used for candidate in alternatives)
                    else "fair_spreading"
                )
            else:
                reason = (
                    "lost_to_higher_preference"
                    if any(candidate in least_used for candidate in matching)
                    else "fair_spreading"
                )
            not_honoured.append({**item, "reason": reason})
    return applied, not_honoured, ignored


def _load_spread_state(path: Path) -> tuple[dict[str, Any], str]:
    if not path.exists():
        return {"schema_version": 1, "assignments": {}, "selection_count": 0}, "initialised"
    try:
        value = _read_json(path)
    except (OSError, json.JSONDecodeError):
        return {}, "invalid"
    assignments = value.get("assignments") if isinstance(value, dict) else None
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or not isinstance(assignments, dict)
        or any(
            not isinstance(family, str)
            or not family
            or not isinstance(count, int)
            or isinstance(count, bool)
            or count < 0
            for family, count in assignments.items()
        )
        or value.get("selection_count") != sum(assignments.values())
    ):
        return {}, "invalid"
    return {
        "schema_version": 1,
        "assignments": dict(sorted(assignments.items())),
        "selection_count": value["selection_count"],
    }, "loaded"


def _write_state(path: Path, state: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(f"{json.dumps(state, indent=2, sort_keys=True)}\n")
    temporary.replace(path)


def _select_unlocked(
    args: argparse.Namespace,
    policy_table: dict[str, dict[str, str]],
    alias_order: dict[str, int],
    effort_order: dict[str, int],
) -> int:
    policy = policy_table.get(args.task_class)
    if policy is None:
        print(json.dumps({
            "schema_version": 1,
            "status": "unknown_task_class",
            "task_class": args.task_class,
            "role": args.role,
        }, sort_keys=True))
        return 2
    try:
        raw_candidates = _read_json(Path(args.candidates_file))
    except (OSError, json.JSONDecodeError):
        raw_candidates = None
    candidates, candidate_error = _normalise_candidates(
        raw_candidates,
        args.task_class,
        args.role,
        policy,
        alias_order,
        effort_order,
    )
    preferences, preferences_status = _load_preferences(Path(args.preferences_file))
    state, state_status = _load_spread_state(Path(args.spread_state_file))
    base = {
        "schema_version": 1,
        "task_class": args.task_class,
        "role": args.role,
        "selection_source": "hard-admissible-preference",
    }
    if candidate_error:
        print(json.dumps({**base, "status": candidate_error}, sort_keys=True))
        return 2
    if state_status == "invalid":
        print(json.dumps({**base, "status": "spread_state_invalid"}, sort_keys=True))
        return 2
    admissible = [candidate for candidate in candidates if candidate["admissible"]]
    if not admissible:
        print(json.dumps({
            **base,
            "status": "no_admissible_candidate",
            "candidates": candidates,
        }, sort_keys=True))
        return 1

    items = _preference_items(preferences, args.task_class, args.role)
    ranks = _rank_maps(items)
    deprioritised = _deprioritised(preferences)
    assignments = state["assignments"]
    minimum = min(assignments.get(candidate["model_family"], 0) for candidate in admissible)
    least_used = [
        candidate
        for candidate in admissible
        if assignments.get(candidate["model_family"], 0) == minimum
    ]
    chosen = min(
        least_used,
        key=lambda candidate: _preference_rank(candidate, ranks, deprioritised),
    )
    chosen["selected"] = True
    chosen["disposition"] = "selected"
    for candidate in admissible:
        if candidate is not chosen:
            candidate["disposition"] = "passed_over"

    state_before = deepcopy(state)
    next_assignments = dict(assignments)
    family = chosen["model_family"]
    next_assignments[family] = next_assignments.get(family, 0) + 1
    state_after = {
        "schema_version": 1,
        "assignments": dict(sorted(next_assignments.items())),
        "selection_count": state["selection_count"] + 1,
    }
    _write_state(Path(args.spread_state_file), state_after)
    applied, not_honoured, ignored = _preference_receipt(
        items,
        candidates,
        chosen,
        least_used,
    )
    ignored.extend(
        _configuration_ignored(preferences, args.task_class, args.role)
    )
    receipt = {
        **base,
        "status": "ok",
        "chosen_candidate_id": chosen["candidate_id"],
        "chosen_route": deepcopy(chosen["route"]),
        "candidates": candidates,
        "preference": {
            "file_status": preferences_status,
            "applied": applied,
            "not_honoured": not_honoured,
            "ignored": ignored,
        },
        "spreading": {
            "policy": "fair-round-robin",
            "state_status": state_status,
            "state_before": state_before,
            "state_after": state_after,
        },
    }
    print(json.dumps(receipt, sort_keys=True))
    return 0


def select(
    args: argparse.Namespace,
    policy_table: dict[str, dict[str, str]],
    alias_order: dict[str, int],
    effort_order: dict[str, int],
) -> int:
    state_path = Path(args.spread_state_file)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    lock_path = state_path.with_name(f".{state_path.name}.lock")
    with lock_path.open("a+") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            return _select_unlocked(
                args,
                policy_table,
                alias_order,
                effort_order,
            )
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
