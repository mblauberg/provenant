#!/usr/bin/env python3
"""Validate and normalise Delivery Authority V2 scopes."""

from __future__ import annotations

from datetime import datetime
from pathlib import PurePosixPath
import re
from typing import Any


class AuthorityMappingError(ValueError):
    pass


_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
_TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$")
_HOST = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.:-]{0,252}$")
_RFC3339 = re.compile(
    r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?(?:Z|([+-])(\d{2}):(\d{2}))$"
)
_PROVIDER_TOKEN_BUDGET = re.compile(
    r"^(?:input_tokens|output_tokens):[a-z0-9]+(?:[.-][a-z0-9]+)*$"
)
_GENERIC_BUDGET_UNITS = {
    "turns",
    "provider_calls",
    "concurrent_turns",
    "descendants",
    "message_bytes",
    "artifact_bytes",
    "wall_clock_milliseconds",
}
_MAX_SAFE_INTEGER = 2**53 - 1
_MAX_BUDGET_UNITS = 128
_MAX_ARRAY_ITEMS = 256
_MAX_PATH_BYTES = 4096

_SCOPE_FIELDS = {
    "schema_version",
    "workspace_roots",
    "allowed_source_paths",
    "allowed_artifact_paths",
    "denied_paths",
    "prohibited_actions",
    "disclosure",
    "secrets_access",
    "secret_refs",
    "deployment",
    "deployment_targets",
    "irreversible_actions",
    "irreversible_action_ids",
    "network",
    "expires_at",
    "budget",
}
_AUTHORITY_FIELDS = _SCOPE_FIELDS | {
    "approved_by",
    "evidence",
    "evidence_digest",
    "delegations",
}
_DELEGATION_FIELDS = _SCOPE_FIELDS | {"actor"}


def _fail(condition: bool, message: str) -> None:
    if condition:
        raise AuthorityMappingError(message)


def _object(value: Any, field: str) -> dict[str, Any]:
    _fail(not isinstance(value, dict), f"{field} must be an object")
    return value


def _closed(value: dict[str, Any], fields: set[str], field: str) -> None:
    missing = fields - set(value)
    unknown = set(value) - fields
    _fail(bool(missing or unknown), f"{field} fields are invalid; missing={sorted(missing)}, unknown={sorted(unknown)}")


def _token(value: Any, field: str) -> str:
    _fail(not isinstance(value, str) or not _TOKEN.fullmatch(value), f"{field} must be a bounded token")
    return value


def _path(value: Any, field: str) -> str:
    _fail(not isinstance(value, str) or not value, f"{field} must be a relative path")
    _fail(
        len(value.encode("utf-8")) > _MAX_PATH_BYTES
        or value.startswith("/")
        or re.match(r"^[A-Za-z]:", value) is not None
        or "\\" in value
        or "\0" in value
        or any(character in value for character in "*?[]{}"),
        f"{field} must be a canonical relative path",
    )
    path = PurePosixPath(value)
    _fail(value != "." and ("." in path.parts or ".." in path.parts), f"{field} must be a canonical relative path")
    canonical = path.as_posix().rstrip("/")
    _fail(canonical != value, f"{field} must be a canonical relative path")
    return canonical


def _strings(
    value: Any,
    field: str,
    item_parser,
    *,
    minimum: int = 0,
    maximum: int = _MAX_ARRAY_ITEMS,
) -> list[str]:
    _fail(not isinstance(value, list), f"{field} must be a list")
    parsed = [item_parser(item, f"{field}[{index}]") for index, item in enumerate(value)]
    _fail(len(parsed) < minimum, f"{field} must contain at least {minimum} item(s)")
    _fail(len(parsed) > maximum, f"{field} must contain at most {maximum} item(s)")
    _fail(len(set(parsed)) != len(parsed), f"{field} must contain unique values")
    return sorted(parsed)


def _timestamp_value(value: str) -> datetime:
    return datetime.fromisoformat(value[:-1] + "+00:00" if value.endswith("Z") else value)


def _timestamp_order(value: str) -> tuple[datetime, str]:
    match = _RFC3339.fullmatch(value)
    assert match is not None
    return _timestamp_value(value).replace(microsecond=0), (match.group(7) or "").ljust(9, "0")


def _timestamp(value: Any, field: str) -> str:
    match = _RFC3339.fullmatch(value) if isinstance(value, str) else None
    _fail(match is None, f"{field} must be a strict RFC3339 timestamp")
    assert match is not None
    year, month, day, hour, minute, second = (int(part) for part in match.groups()[:6])
    offset_hour = int(match.group(9)) if match.group(9) is not None else 0
    offset_minute = int(match.group(10)) if match.group(10) is not None else 0
    _fail(
        year < 1 or hour > 23 or minute > 59 or second > 59
        or offset_hour > 23 or offset_minute > 59,
        f"{field} must be a strict RFC3339 timestamp",
    )
    try:
        datetime(year, month, day, hour, minute, second)
        _timestamp_value(value)
    except ValueError as exc:
        raise AuthorityMappingError(f"{field} must be a strict RFC3339 timestamp") from exc
    return value


def _budget(value: Any, field: str) -> dict[str, int]:
    budget = _object(value, field)
    _fail(len(budget) > _MAX_BUDGET_UNITS, f"{field} must contain at most {_MAX_BUDGET_UNITS} units")
    result: dict[str, int] = {}
    for key, amount in budget.items():
        _fail(
            not isinstance(key, str)
            or (
                key not in _GENERIC_BUDGET_UNITS
                and not _PROVIDER_TOKEN_BUDGET.fullmatch(key)
            ),
            f"{field} contains an invalid budget unit",
        )
        _fail(
            isinstance(amount, bool)
            or not isinstance(amount, int)
            or amount < 0
            or amount > _MAX_SAFE_INTEGER,
            f"{field}.{key} must be a non-negative safe integer",
        )
        result[key] = amount
    return dict(sorted(result.items()))


def _disclosure(value: Any) -> dict[str, Any]:
    _fail(value not in {"local-only", "approved-providers", "public"}, "authority.disclosure is invalid")
    if value == "public":
        return {"level": "allowed"}
    scopes = ["local"] if value == "local-only" else ["approved-provider", "local"]
    return {"level": "scoped", "scopes": scopes}


def _map_scope(
    scope: dict[str, Any],
    approval: dict[str, str],
    *,
    field: str,
) -> dict[str, Any]:
    _fail(scope.get("schema_version") != 2, f"{field}.schema_version must be 2")
    workspace_roots = _strings(
        scope.get("workspace_roots"), f"{field}.workspace_roots", _path,
        minimum=1, maximum=64,
    )
    source_paths = _strings(scope.get("allowed_source_paths"), f"{field}.allowed_source_paths", _path)
    artifact_paths = _strings(scope.get("allowed_artifact_paths"), f"{field}.allowed_artifact_paths", _path)
    _fail(
        not _allowed_paths_contained(source_paths, workspace_roots),
        f"{field}.allowed_source_paths must be contained by {field}.workspace_roots",
    )
    _fail(
        not _allowed_paths_contained(artifact_paths, workspace_roots),
        f"{field}.allowed_artifact_paths must be contained by {field}.workspace_roots",
    )
    denied_paths = _strings(scope.get("denied_paths"), f"{field}.denied_paths", _path)
    prohibited_actions = _strings(scope.get("prohibited_actions"), f"{field}.prohibited_actions", _token)

    secret_refs = _strings(scope.get("secret_refs"), f"{field}.secret_refs", _token)
    secrets_access = scope.get("secrets_access")
    _fail(secrets_access not in {"none", "use-without-disclosure"}, f"{field}.secrets_access is invalid")
    _fail(secrets_access == "none" and bool(secret_refs), f"{field}.secret_refs must be empty when secrets access is none")
    _fail(secrets_access == "use-without-disclosure" and not secret_refs, f"{field}.secret_refs must be non-empty when secrets access is enabled")
    secrets = {"access": "none"} if secrets_access == "none" else {
        "access": "use-without-disclosure", "references": secret_refs,
    }

    deployment = scope.get("deployment")
    _fail(not isinstance(deployment, bool), f"{field}.deployment must be boolean")
    deployment_targets = _strings(scope.get("deployment_targets"), f"{field}.deployment_targets", _token)
    _fail(not deployment and bool(deployment_targets), f"{field}.deployment_targets must be empty when deployment is denied")
    _fail(deployment and not deployment_targets, f"{field}.deployment_targets must be non-empty when deployment is allowed")
    deployment_policy = {"allowed": False} if not deployment else {"allowed": True, "targets": deployment_targets}

    irreversible = scope.get("irreversible_actions")
    _fail(not isinstance(irreversible, bool), f"{field}.irreversible_actions must be boolean")
    action_ids = _strings(scope.get("irreversible_action_ids"), f"{field}.irreversible_action_ids", _token)
    _fail(not irreversible and bool(action_ids), f"{field}.irreversible_action_ids must be empty when irreversible actions are denied")
    _fail(irreversible and not action_ids, f"{field}.irreversible_action_ids must be non-empty when irreversible actions are allowed")
    irreversible_policy = {"allowed": False} if not irreversible else {"allowed": True, "actionIds": action_ids}

    network = _object(scope.get("network"), f"{field}.network")
    _closed(network, {"tool_egress", "allowed_hosts"}, f"{field}.network")
    tool_egress = network.get("tool_egress")
    _fail(tool_egress not in {"none", "allowlist"}, f"{field}.network.tool_egress is invalid")
    allowed_hosts = _strings(network.get("allowed_hosts"), f"{field}.network.allowed_hosts", lambda value, name: _host(value, name))
    _fail(tool_egress == "none" and bool(allowed_hosts), f"{field}.network.allowed_hosts must be empty when tool egress is none")
    _fail(tool_egress == "allowlist" and not allowed_hosts, f"{field}.network.allowed_hosts must be non-empty for an allowlist")
    network_policy = {"toolEgress": "none"} if tool_egress == "none" else {
        "toolEgress": "allowlist", "allowedHosts": allowed_hosts,
    }

    return {
        "schemaVersion": 2,
        "approval": dict(approval),
        "workspaceRoots": workspace_roots,
        "sourcePaths": source_paths,
        "artifactPaths": artifact_paths,
        "deniedPaths": denied_paths,
        "prohibitedActions": prohibited_actions,
        "disclosure": _disclosure(scope.get("disclosure")),
        "secrets": secrets,
        "deployment": deployment_policy,
        "irreversibleActions": irreversible_policy,
        "network": network_policy,
        "expiresAt": _timestamp(scope.get("expires_at"), f"{field}.expires_at"),
        "budget": _budget(scope.get("budget"), f"{field}.budget"),
    }


def _host(value: Any, field: str) -> str:
    _fail(not isinstance(value, str) or not _HOST.fullmatch(value), f"{field} must be a bounded host")
    return value


def map_delivery_authority(
    authority: Any,
) -> dict[str, Any]:
    """Normalise one closed Delivery Authority V2 object."""

    authority = _object(authority, "authority")
    _closed(authority, _AUTHORITY_FIELDS, "authority")
    approved_by = _token(authority.get("approved_by"), "authority.approved_by")
    evidence = _token(authority.get("evidence"), "authority.evidence")
    evidence_digest = authority.get("evidence_digest")
    _fail(not isinstance(evidence_digest, str) or not _DIGEST.fullmatch(evidence_digest), "authority.evidence_digest must be a sha256 digest")
    delegations = authority.get("delegations")
    _fail(not isinstance(delegations, list), "authority.delegations must be a list")
    approval = {
        "approvedBy": approved_by,
        "evidenceId": evidence,
        "evidenceDigest": evidence_digest,
    }
    return _map_scope(
        authority,
        approval,
        field="authority",
    )


def map_delivery_delegations(
    authority: Any,
) -> list[dict[str, Any]]:
    """Map complete Delivery delegation scopes and reject every widening."""

    authority = _object(authority, "authority")
    parent = map_delivery_authority(authority)
    mapped: list[dict[str, Any]] = []
    for index, raw in enumerate(authority["delegations"]):
        field = f"authority.delegations[{index}]"
        delegation = _object(raw, field)
        _closed(delegation, _DELEGATION_FIELDS, field)
        actor = _token(delegation.get("actor"), f"{field}.actor")
        child = _map_scope(
            delegation,
            parent["approval"],
            field=field,
        )
        _fail(not authority_contained(child, parent), f"delegation {index} broadens parent authority")
        mapped.append({"actor": actor, "authority": child})
    return mapped


def _inside(path: str, root: str) -> bool:
    return root == "." or path == root or path.startswith(root + "/")


def _allowed_paths_contained(child: list[str], parent: list[str]) -> bool:
    return all(any(_inside(path, root) for root in parent) for path in child)


def _denied_paths_preserved(child: list[str], parent: list[str]) -> bool:
    return all(any(_inside(path, denial) for denial in child) for path in parent)


def _union_contained(child: dict[str, Any], parent: dict[str, Any], enabled: str, values: str) -> bool:
    if not parent[enabled]:
        return not child[enabled]
    return not child[enabled] or set(child[values]) <= set(parent[values])


def authority_contained(child: dict[str, Any], parent: dict[str, Any]) -> bool:
    """Return whether a mapped child is no broader than its mapped parent."""

    disclosure_rank = {"allowed": 0, "scoped": 1, "forbidden": 2}
    child_disclosure = child["disclosure"]
    parent_disclosure = parent["disclosure"]
    disclosure_ok = disclosure_rank[child_disclosure["level"]] >= disclosure_rank[parent_disclosure["level"]]
    if child_disclosure["level"] == parent_disclosure["level"] == "scoped":
        disclosure_ok = set(child_disclosure["scopes"]) <= set(parent_disclosure["scopes"])

    child_secrets = child["secrets"]
    parent_secrets = parent["secrets"]
    secrets_ok = child_secrets["access"] == "none"
    if parent_secrets["access"] != "none" and child_secrets["access"] != "none":
        secrets_ok = set(child_secrets["references"]) <= set(parent_secrets["references"])

    child_network = child["network"]
    parent_network = parent["network"]
    network_ok = child_network["toolEgress"] == "none"
    if parent_network["toolEgress"] != "none" and child_network["toolEgress"] != "none":
        network_ok = set(child_network["allowedHosts"]) <= set(parent_network["allowedHosts"])

    return (
        child["approval"] == parent["approval"]
        and _allowed_paths_contained(child["workspaceRoots"], parent["workspaceRoots"])
        and _allowed_paths_contained(child["sourcePaths"], parent["sourcePaths"])
        and _allowed_paths_contained(child["artifactPaths"], parent["artifactPaths"])
        and _denied_paths_preserved(child["deniedPaths"], parent["deniedPaths"])
        and set(child["prohibitedActions"]) >= set(parent["prohibitedActions"])
        and disclosure_ok
        and secrets_ok
        and _union_contained(child["deployment"], parent["deployment"], "allowed", "targets")
        and _union_contained(child["irreversibleActions"], parent["irreversibleActions"], "allowed", "actionIds")
        and network_ok
        and _timestamp_order(child["expiresAt"]) <= _timestamp_order(parent["expiresAt"])
        and set(child["budget"]) <= set(parent["budget"])
        and all(child["budget"][key] <= parent["budget"][key] for key in child["budget"])
    )
