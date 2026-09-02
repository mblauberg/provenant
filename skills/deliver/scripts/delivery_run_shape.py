"""Flat shape check for the delivery run receipt.

RUN.json is a flat record of what a run did: who approved what, which
artifacts and evidence exist, which human gates are closed. It is not a state
machine. Ordering claims used to be certified by a 12-state transition graph
that the same agent wrote in the same turn, which certified nothing. This
module replaces that with the only check a self-written document can support:
the receipt has the fields it claims to have, of the types it claims.

Both the producer and the validator use it, so a receipt cannot be written in a
shape the reader rejects.
"""

from __future__ import annotations

from typing import Any

# Every top-level field of a flat delivery receipt, with the JSON types it may
# take. `None` in a tuple means the field may be null.
FIELD_TYPES: dict[str, tuple[type | None, ...]] = {
    "schema_version": (int,),
    "contract": (str,),
    "run_id": (str,),
    "fabric_relationships": (dict,),
    "profile": (str,),
    "risk_tier": (str,),
    "initial_risk_tier": (str,),
    "chair_family": (str,),
    "risk_assessment": (dict,),
    "risk_override": (dict,),
    "high_stakes": (bool,),
    "intent": (dict,),
    "authority": (dict,),
    "artifacts": (list,),
    "design": (dict,),
    "evidence": (list,),
    "measures": (dict,),
    "assurance": (dict,),
    "reviews": (list,),
    "security": (dict,),
    "high_stakes_controls": (dict, None),
    "human_gates": (dict,),
    "observation": (dict,),
    "incident": (dict, None),
    "retrospective": (dict, None),
    "repair_cycles": (int,),
    "escaped_defect": (bool,),
    "human_corrections": (list,),
    "checkpoint": (dict,),
    "degradation": (dict, None),
    "project_policy": (dict,),
    "software_delivery": (dict,),
}

# `fabric_relationships` predates the flat receipt; `project_policy` and
# `software_delivery` appear only for the runs that bind them.
OPTIONAL_FIELDS = frozenset({
    "fabric_relationships", "project_policy", "software_delivery",
})
REQUIRED_FIELDS = frozenset(FIELD_TYPES) - OPTIONAL_FIELDS
KNOWN_FIELDS = frozenset(FIELD_TYPES)

# The flat gate fields that replace the transition graph. Each says what the
# run recorded, not what order it happened in.
GATE_STATUSES = ("pending", "approved", "not-required")


TYPE_NAMES = {
    bool: "a boolean", int: "an integer", str: "a string",
    dict: "an object", list: "a list", None: "null",
}


def _type_names(allowed: tuple[type | None, ...]) -> str:
    return " or ".join(TYPE_NAMES[item] for item in allowed)


def shape_errors(run: Any) -> list[str]:
    """Return every shape complaint about `run`, in field order."""
    if not isinstance(run, dict):
        return ["RUN root must be an object"]
    errors: list[str] = []
    if run.get("contract") != "delivery-run" or run.get("schema_version") != 1:
        errors.append(
            "delivery receipt must use contract delivery-run schema_version 1",
        )
    for field, allowed in FIELD_TYPES.items():
        if field not in run:
            if field not in OPTIONAL_FIELDS:
                errors.append(f"{field} is required")
            continue
        value = run[field]
        if value is None:
            if None not in allowed:
                errors.append(
                    f"{field} must be {_type_names(allowed)}, got NoneType",
                )
            continue
        types = tuple(item for item in allowed if item is not None)
        # bool is an int subclass, so an integer field must reject booleans.
        if isinstance(value, bool) and bool not in types:
            errors.append(
                f"{field} must be {_type_names(allowed)}, got {type(value).__name__}",
            )
        elif not isinstance(value, types):
            errors.append(
                f"{field} must be {_type_names(allowed)}, got {type(value).__name__}",
            )
    unknown = sorted(set(run) - KNOWN_FIELDS)
    if unknown:
        errors.append(f"RUN.json declares unknown fields: {', '.join(unknown)}")
    return errors


def check_shape(run: Any, error_type: type[Exception]) -> dict[str, Any]:
    """Raise `error_type` on the first shape complaint, else return the run."""
    errors = shape_errors(run)
    if errors:
        raise error_type(errors[0])
    return run


def gate_approved(run: dict[str, Any], *path: str) -> bool:
    value: Any = run
    for key in path:
        if not isinstance(value, dict):
            return False
        value = value.get(key)
    return isinstance(value, dict) and value.get("status") == "approved"


def intent_approved(run: dict[str, Any]) -> bool:
    return gate_approved(run, "intent", "approval")


def acceptance_approved(run: dict[str, Any]) -> bool:
    return gate_approved(run, "human_gates", "acceptance")


def release_approved(run: dict[str, Any]) -> bool:
    return gate_approved(run, "human_gates", "release")


def run_closed(run: dict[str, Any]) -> bool:
    """A run is closed when release is approved and observation has settled."""
    observation = run.get("observation")
    settled = (
        isinstance(observation, dict)
        and observation.get("status") in {"pass", "not_applicable"}
    )
    return release_approved(run) and settled


# Timestamp fields a receipt records. The newest of them is the run's clock:
# the flat replacement for "the time of the last state transition".
TIMESTAMP_FIELDS = ("recorded_at", "finished_at", "observed_at", "at")


def recorded_timestamps(run: dict[str, Any]) -> list[str]:
    """Every UTC timestamp the receipt records, unordered and unparsed."""
    found: list[str] = []

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            for key, child in value.items():
                if key in TIMESTAMP_FIELDS and isinstance(child, str) and child:
                    found.append(child)
                else:
                    walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    walk(run.get("evidence"))
    walk(run.get("human_corrections"))
    observation = run.get("observation")
    if isinstance(observation, dict):
        for field in ("started_at", "ended_at"):
            value = observation.get(field)
            if isinstance(value, str) and value:
                found.append(value)
    return found
