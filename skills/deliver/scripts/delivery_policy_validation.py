"""Validate delivery policy registries and authority bindings."""

from __future__ import annotations

import copy
from datetime import datetime
from functools import lru_cache
import hashlib
import importlib.util
import json
from pathlib import Path
import re
from typing import Any


AUTHORITY_MAPPING_PATH = Path(__file__).with_name("authority_mapping.py")
PROFILE_FIELDS = {
    "artifact_types", "required_evidence", "required_measures", "stochastic_policy",
    "security_surface_policy", "boundary_checks", "evidence_policy",
    "release_semantics", "observation_examples",
}
FABRIC_RELATIONSHIP_FIELDS = {
    "mode", "delivery_run_id", "project_session_id", "coordination_run_id",
    "workstream_id", "lead_agent_id",
}
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")


def _fail(condition: bool, message: str, invalid_type: type[ValueError]) -> None:
    if condition:
        raise invalid_type(message)


def _mapping(
    value: Any, field: str, invalid_type: type[ValueError],
) -> dict[str, Any]:
    _fail(not isinstance(value, dict), f"{field} must be an object", invalid_type)
    return value


def _utc(value: Any, field: str, invalid_type: type[ValueError]) -> datetime:
    _fail(
        not isinstance(value, str) or not value.endswith("Z"),
        f"{field} must be a UTC timestamp",
        invalid_type,
    )
    try:
        return datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise invalid_type(f"{field} must be an ISO UTC timestamp") from exc


def _digest(value: Any, field: str, invalid_type: type[ValueError]) -> None:
    _fail(
        not isinstance(value, str) or not DIGEST.fullmatch(value),
        f"{field} must be a sha256 digest",
        invalid_type,
    )


def _identifier(value: Any, field: str, invalid_type: type[ValueError]) -> str:
    _fail(
        not isinstance(value, str) or not IDENTIFIER.fullmatch(value),
        f"{field} must be a bounded stable identifier",
        invalid_type,
    )
    return value


def _safe_path(value: Any, field: str, invalid_type: type[ValueError]) -> str:
    _fail(
        not isinstance(value, str) or not value,
        f"{field} must be a non-empty path",
        invalid_type,
    )
    path = Path(value)
    _fail(
        path.is_absolute() or ".." in path.parts,
        f"{field} must be safe and relative",
        invalid_type,
    )
    return path.as_posix().rstrip("/")


def _validate_profile(
    name: str, profile: Any, invalid_type: type[ValueError],
) -> None:
    _fail(
        not isinstance(profile, dict)
        or set(profile) - {"conditional_evidence"} != PROFILE_FIELDS
        or not set(profile) <= PROFILE_FIELDS | {"conditional_evidence"},
        f"profile {name} contract is incomplete",
        invalid_type,
    )
    artifact_types = profile["artifact_types"]
    _fail(
        not isinstance(artifact_types, list)
        or not artifact_types
        or any(not isinstance(value, str) or not value for value in artifact_types)
        or len(set(artifact_types)) != len(artifact_types),
        f"profile {name} artifact_types must be a unique non-empty string list",
        invalid_type,
    )
    stochastic_policy = profile["stochastic_policy"]
    _fail(
        not isinstance(stochastic_policy, dict),
        f"profile {name} stochastic_policy must be an object",
        invalid_type,
    )
    classified_types = stochastic_policy.get("required_for_artifact_types")
    if classified_types is not None:
        _fail(
            not isinstance(classified_types, list)
            or not classified_types
            or any(not isinstance(value, str) or not value for value in classified_types)
            or len(set(classified_types)) != len(classified_types)
            or not set(classified_types) <= set(artifact_types),
            f"profile {name} required_for_artifact_types must be a unique non-empty subset of artifact_types",
            invalid_type,
        )
    _fail(
        not set(profile["boundary_checks"])
        <= set(profile["required_evidence"]["deterministic"]),
        f"profile {name} boundary checks are not deterministic gates",
        invalid_type,
    )
    conditional = profile.get("conditional_evidence", {})
    _fail(
        not isinstance(conditional, dict) or not set(conditional) <= set(artifact_types),
        f"profile {name} conditional evidence uses an unknown artifact type",
        invalid_type,
    )
    for artifact_type, requirements in conditional.items():
        _fail(
            not isinstance(requirements, dict)
            or set(requirements) != {"deterministic", "judgement"}
            or any(
                not isinstance(gates, list)
                or any(not isinstance(gate, str) or not gate for gate in gates)
                or len(gates) != len(set(gates))
                for gates in requirements.values()
            ),
            f"profile {name} conditional evidence for {artifact_type} is invalid",
            invalid_type,
        )


def load_profiles(root: Path, *, invalid_type: type[ValueError]) -> dict[str, Any]:
    try:
        data = json.loads((root / "config" / "delivery-profiles.json").read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise invalid_type(f"profile registry is unreadable: {exc}") from exc
    _fail(
        data.get("schema_version") != 1 or not isinstance(data.get("profiles"), dict),
        "profile registry is invalid",
        invalid_type,
    )
    for name, profile in data["profiles"].items():
        _validate_profile(name, profile, invalid_type)
    return data


def profile_evidence_requirements(
    profile: dict[str, Any], artifacts: dict[str, dict[str, Any]],
) -> dict[str, set[str]]:
    required = {
        kind: set(gates) for kind, gates in profile["required_evidence"].items()
    }
    conditional = profile.get("conditional_evidence", {})
    for artifact in artifacts.values():
        if artifact.get("class") != "canonical":
            continue
        for kind, gates in conditional.get(artifact.get("artifact_type"), {}).items():
            required[kind].update(gates)
    return required


def apply_project_policy(
    registry: dict[str, Any],
    run: dict[str, Any],
    *,
    project_policy_path: Path | None,
    workspace_root: Path | None,
    invalid_type: type[ValueError],
) -> dict[str, Any]:
    declared = run.get("project_policy")
    if project_policy_path is None:
        _fail(
            declared is not None,
            "declared project_policy requires --project-policy",
            invalid_type,
        )
        return registry
    _fail(
        not isinstance(declared, dict) or set(declared) != {"path", "digest"},
        "project_policy receipt binding is invalid",
        invalid_type,
    )
    clean = _safe_path(declared.get("path"), "project_policy.path", invalid_type)
    _digest(declared.get("digest"), "project_policy.digest", invalid_type)
    _fail(workspace_root is None, "project_policy requires workspace_root", invalid_type)
    expected_path = (workspace_root / clean).resolve()
    _fail(
        expected_path != project_policy_path.resolve(),
        "project_policy path does not match the receipt",
        invalid_type,
    )
    try:
        raw = project_policy_path.read_bytes()
        overlay = json.loads(raw)
    except (OSError, json.JSONDecodeError) as exc:
        raise invalid_type(f"project policy is unreadable: {exc}") from exc
    actual = "sha256:" + hashlib.sha256(raw).hexdigest()
    _fail(actual != declared["digest"], "project_policy digest does not match", invalid_type)
    _fail(
        not isinstance(overlay, dict)
        or set(overlay) != {"schema_version", "profiles"}
        or overlay.get("schema_version") != 1
        or not isinstance(overlay.get("profiles"), dict),
        "project policy schema is invalid",
        invalid_type,
    )
    strengthened = copy.deepcopy(registry)
    for profile_name, additions in overlay["profiles"].items():
        if profile_name not in strengthened["profiles"]:
            _fail(
                not isinstance(additions, dict) or set(additions) != PROFILE_FIELDS,
                f"new project profile {profile_name} must declare the complete profile contract",
                invalid_type,
            )
            _fail(
                not additions.get("artifact_types")
                or not additions.get("release_semantics")
                or not additions.get("observation_examples"),
                f"new project profile {profile_name} is incomplete",
                invalid_type,
            )
            _fail(
                any(
                    artifact_type not in strengthened["artifact_type_surfaces"]
                    for artifact_type in additions["artifact_types"]
                ),
                f"new project profile {profile_name} uses an unclassified artifact type",
                invalid_type,
            )
            for group, kinds in (
                ("required_evidence", {"deterministic", "judgement"}),
                ("required_measures", {"outcome", "trajectory"}),
            ):
                _fail(
                    not isinstance(additions.get(group), dict)
                    or set(additions[group]) != kinds
                    or any(
                        not isinstance(values, list) or not values
                        for values in additions[group].values()
                    ),
                    f"new project profile {profile_name} {group} is incomplete",
                    invalid_type,
                )
            strengthened["profiles"][profile_name] = copy.deepcopy(additions)
            continue
        _fail(
            not isinstance(additions, dict)
            or not set(additions) <= {"required_evidence", "required_measures"},
            f"project profile {profile_name} contains a non-additive field",
            invalid_type,
        )
        for group in ("required_evidence", "required_measures"):
            for kind, values in additions.get(group, {}).items():
                _fail(
                    kind not in strengthened["profiles"][profile_name][group],
                    f"project profile {profile_name} has invalid {group} kind",
                    invalid_type,
                )
                _fail(
                    not isinstance(values, list)
                    or any(not isinstance(value, str) or not value for value in values),
                    f"project profile {profile_name} {group}.{kind} is invalid",
                    invalid_type,
                )
                strengthened["profiles"][profile_name][group][kind] = sorted(
                    set(strengthened["profiles"][profile_name][group][kind]) | set(values)
                )
    for profile_name, profile in strengthened["profiles"].items():
        _validate_profile(profile_name, profile, invalid_type)
    return strengthened


def validate_fabric_relationships(
    run: dict[str, Any], *, invalid_type: type[ValueError],
) -> None:
    if "fabric_relationships" not in run:
        return
    relationships = _mapping(
        run["fabric_relationships"], "fabric_relationships", invalid_type,
    )
    _fail(
        set(relationships) != FABRIC_RELATIONSHIP_FIELDS,
        "fabric_relationships fields are invalid",
        invalid_type,
    )
    mode = relationships.get("mode")
    _fail(
        mode not in {"coordinated", "independent"},
        "fabric_relationships.mode is invalid",
        invalid_type,
    )
    delivery_run_id = _identifier(
        relationships.get("delivery_run_id"),
        "fabric_relationships.delivery_run_id",
        invalid_type,
    )
    _fail(
        delivery_run_id != run["run_id"],
        "fabric_relationships.delivery_run_id must match run_id",
        invalid_type,
    )
    relation_fields = (
        "project_session_id", "coordination_run_id", "workstream_id", "lead_agent_id",
    )
    if mode == "independent":
        _fail(
            any(relationships.get(field) != "not_applicable" for field in relation_fields),
            "independent relationships must be explicit not_applicable values",
            invalid_type,
        )
        return
    for field in relation_fields:
        identifier = _identifier(
            relationships.get(field), f"fabric_relationships.{field}", invalid_type,
        )
        _fail(
            identifier == "not_applicable",
            "coordinated relationships require concrete project-session, coordination-run, workstream and lead identifiers",
            invalid_type,
        )


def validate_risk(
    run: dict[str, Any],
    root: Path,
    *,
    risks: tuple[str, ...],
    invalid_type: type[ValueError],
) -> None:
    try:
        policy = json.loads((root / "config" / "risk-policy.json").read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise invalid_type(f"risk policy is unreadable: {exc}") from exc
    factors = policy.get("factors")
    order = policy.get("tier_order")
    _fail(
        not isinstance(factors, dict) or order != list(risks),
        "risk policy is invalid",
        invalid_type,
    )
    assessment = _mapping(run.get("risk_assessment"), "risk_assessment", invalid_type)
    _fail(
        set(assessment) != set(factors),
        "risk_assessment must cover every policy factor",
        invalid_type,
    )
    minimum_index = 0
    for factor, values in factors.items():
        value = assessment.get(factor)
        _fail(value not in values, f"risk_assessment.{factor} is invalid", invalid_type)
        minimum_index = max(minimum_index, risks.index(values[value]))
    declared_index = risks.index(run["risk_tier"])
    override = _mapping(run.get("risk_override"), "risk_override", invalid_type)
    _fail(
        override.get("status") not in {"not-required", "approved"},
        "risk_override.status is invalid",
        invalid_type,
    )
    if declared_index < minimum_index:
        _fail(
            override.get("status") != "approved"
            or not override.get("approved_by")
            or not override.get("evidence")
            or not override.get("reason"),
            f"risk downgrade below derived {risks[minimum_index]} requires human evidence",
            invalid_type,
        )


@lru_cache(maxsize=1)
def _authority_mapping_module():
    spec = importlib.util.spec_from_file_location(
        "delivery_authority_mapping", AUTHORITY_MAPPING_PATH,
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("authority mapper is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def validate_authority(
    authority: dict[str, Any],
    run: dict[str, Any],
    root: Path,
    *,
    invalid_type: type[ValueError],
) -> None:
    try:
        mapper = _authority_mapping_module()
    except RuntimeError as exc:
        raise invalid_type(str(exc)) from exc
    try:
        mapper.map_delivery_authority(authority)
        mapper.map_delivery_delegations(authority)
    except mapper.AuthorityMappingError as exc:
        raise invalid_type(str(exc)) from exc
    expiry = _utc(authority.get("expires_at"), "authority.expires_at", invalid_type)
    history_times = [
        _utc(item.get("at"), "state_history.at", invalid_type)
        for item in run.get("state_history", [])
        if isinstance(item, dict)
    ]
    _fail(
        bool(history_times) and expiry <= max(history_times),
        "authority must cover the current run checkpoint",
        invalid_type,
    )
