#!/usr/bin/env python3
"""Resolve durable harness model aliases into auditable concrete routes."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
from fnmatch import fnmatchcase
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
from typing import Any

import yaml


PRODUCT_ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[1])
).expanduser()
INSTANCE_ROOT = Path(
    os.environ.get("AGENT_FABRIC_INSTANCE_ROOT", PRODUCT_ROOT)
).expanduser()
CATALOG_PATH = INSTANCE_ROOT / "config" / "model-routing.json"
COMPATIBILITY_PATH = PRODUCT_ROOT / "config" / "adapter-compatibility.yaml"
COMPATIBILITY_ADAPTER_IDS = {
    "claude": "claude-agent-sdk",
    "codex": "codex-app-server",
    "agy": "agy",
    "cursor": "cursor-agent",
    "kiro": "kiro-acp",
    "opencode": "opencode-acp",
    "pi": "pi-rpc",
}
TRUSTED_CAPABILITY_SOURCES = {
    "codex debug models": "codex",
    "claude subscription canary": "claude",
    "agy models": "agy",
}
TASK_CLASS_POLICY = {
    "mechanical": {"minimum_alias": "scout", "minimum_effort": "low", "role": "worker"},
    "legwork": {"minimum_alias": "workhorse", "minimum_effort": "medium", "role": "worker"},
    "critical-review": {"minimum_alias": "flagship", "minimum_effort": "high", "role": "critical-review"},
    "orchestration": {"minimum_alias": "flagship", "minimum_effort": "high", "role": "orchestrator"},
}

# Path-loading avoids depending on `scripts/` being on `sys.path`. Tests reload
# this router by path, so each sibling must be reused rather than re-executed.
# Registering one module but binding names from another makes their globals
# diverge: rebinding `infer_family` on one would be invisible to functions on
# the other. Reuse each cached module for normal import-like identity.
_CATALOG_VALIDATION_PATH = Path(__file__).resolve().parent / "model_route_catalog.py"
_catalog_validation = sys.modules.get("model_route_catalog")
if _catalog_validation is None:
    _catalog_validation_spec = importlib.util.spec_from_file_location(
        "model_route_catalog", _CATALOG_VALIDATION_PATH
    )
    assert _catalog_validation_spec is not None and _catalog_validation_spec.loader is not None
    _catalog_validation = importlib.util.module_from_spec(_catalog_validation_spec)
    sys.modules["model_route_catalog"] = _catalog_validation
    _catalog_validation_spec.loader.exec_module(_catalog_validation)

_PREFERENCES_PATH = Path(__file__).resolve().parent / "model_route_preferences.py"
_preferences = sys.modules.get("model_route_preferences")
if _preferences is None:
    _preferences_spec = importlib.util.spec_from_file_location(
        "model_route_preferences", _PREFERENCES_PATH
    )
    assert _preferences_spec is not None and _preferences_spec.loader is not None
    _preferences = importlib.util.module_from_spec(_preferences_spec)
    sys.modules["model_route_preferences"] = _preferences
    _preferences_spec.loader.exec_module(_preferences)

EFFORT_ORDER = _catalog_validation.EFFORT_ORDER
ALIAS_ORDER = _catalog_validation.ALIAS_ORDER
infer_family = _catalog_validation.infer_family
model_has_alias = _catalog_validation.model_has_alias
capability_key_matches_model = _catalog_validation.capability_key_matches_model
ultra_eligible_roles_are_valid = _catalog_validation.ultra_eligible_roles_are_valid
risk_tier_override_is_well_formed = _catalog_validation.risk_tier_override_is_well_formed
family_alias_candidates = _catalog_validation.family_alias_candidates
risk_tier_overrides_are_valid = _catalog_validation.risk_tier_overrides_are_valid
override_scan_families = _catalog_validation.override_scan_families


def load_catalog(path: Path | None = None) -> dict[str, Any]:
    return json.loads((path or CATALOG_PATH).read_text())


def load_adapter_compatibility(
    adapter: str, path: Path | None = None,
) -> tuple[dict[str, Any] | None, str]:
    compatibility_id = COMPATIBILITY_ADAPTER_IDS.get(adapter)
    if compatibility_id is None:
        return None, "adapter_compatibility_unknown"
    try:
        data = yaml.safe_load((path or COMPATIBILITY_PATH).read_text())
    except (OSError, yaml.YAMLError):
        return None, "adapter_compatibility_unavailable"
    if not isinstance(data, dict) or data.get("schema_version") != 1:
        return None, "adapter_compatibility_invalid"
    adapters = data.get("adapters")
    entry = adapters.get(compatibility_id) if isinstance(adapters, dict) else None
    if not isinstance(entry, dict):
        return None, "adapter_compatibility_unknown"
    constraints = entry.get("model_family_constraints")
    allowed = constraints.get("allowed") if isinstance(constraints, dict) else None
    patterns = constraints.get("allowed_model_patterns", []) if isinstance(constraints, dict) else None
    enabled = entry.get("enabled")
    disabled_reason = entry.get("disabled_reason", "")
    if (
        not isinstance(enabled, bool)
        or (not enabled and (not isinstance(disabled_reason, str) or not disabled_reason.strip()))
        or not isinstance(allowed, list)
        or any(not isinstance(item, str) for item in allowed)
        or not isinstance(patterns, list)
        or any(not isinstance(item, str) for item in patterns)
    ):
        return None, "adapter_compatibility_invalid"
    return {
        "compatibility_adapter": compatibility_id,
        "enabled": enabled,
        "disabled_reason": disabled_reason.strip() if isinstance(disabled_reason, str) else "",
        "allowed_families": allowed,
        "allowed_model_patterns": patterns,
        # Fail closed on omission: only an explicit `false` opts an adapter
        # into account-default dispatch (#190).
        "requires_explicit_model": (constraints.get("requires_explicit_model") is not False)
        if isinstance(constraints, dict)
        else True,
    }, ""


def check_adapter_compatibility(
    compatibility: dict[str, Any], family: str, model: str
) -> tuple[str, str]:
    allowed = compatibility["allowed_families"]
    patterns = compatibility["allowed_model_patterns"]
    lowered_model = model.lower()
    pattern_match = not patterns or any(
        fnmatchcase(lowered_model, pattern.lower()) for pattern in patterns
    )

    compatibility_family = family if family in allowed else ""
    if not compatibility_family and patterns and "open-weight" in allowed and pattern_match:
        compatibility_family = "open-weight"
    if not compatibility_family:
        return "", "adapter_family_forbidden"
    if not pattern_match:
        return compatibility_family, "adapter_model_forbidden"
    return compatibility_family, ""


def emit(record: dict[str, Any], code: int) -> int:
    print(json.dumps(record, sort_keys=True))
    return code


def load_json(raw: str) -> Any:
    def reject_duplicate_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON member: {key}")
            result[key] = value
        return result

    return json.loads(raw, object_pairs_hook=reject_duplicate_members)


def load_capabilities(path: str | None, adapter: str, catalog: dict[str, Any]) -> tuple[dict[str, Any], str]:
    if not path:
        return {}, ""
    try:
        data = load_json(Path(path).read_text())
    except (OSError, json.JSONDecodeError, ValueError):
        return {}, "capability_discovery_failed"
    if not isinstance(data, dict) or data.get("schema_version") != 1 or not isinstance(data.get("models"), dict):
        return {}, "capability_discovery_failed"
    if TRUSTED_CAPABILITY_SOURCES.get(data.get("source")) != adapter:
        return {}, "capability_snapshot_untrusted"
    if adapter == "claude":
        provenance = data.get("provenance")
        if (
            not isinstance(provenance, dict)
            or provenance.get("kind") != "subscription_runtime_canary"
            or provenance.get("auth_method") != "claude.ai"
            or not isinstance(provenance.get("subscription_type"), str)
            or not provenance["subscription_type"]
        ):
            return {}, "capability_snapshot_untrusted"
    try:
        observed = datetime.fromisoformat(str(data.get("observed_at", "")).replace("Z", "+00:00"))
    except ValueError:
        return {}, "capability_snapshot_untrusted"
    if observed.tzinfo is None:
        return {}, "capability_snapshot_untrusted"
    age = (datetime.now(timezone.utc) - observed).total_seconds()
    if age < -60 or age > 300:
        return {}, "capability_snapshot_stale"
    models = data["models"]
    if not models:
        return {}, "capability_discovery_failed"
    adapter_config = catalog.get("adapters", {}).get(adapter, {})
    family = adapter_config.get("fixed_model_family") if isinstance(adapter_config, dict) else None
    family_config = catalog.get("families", {}).get(family, {}) if family else {}
    alias_candidates = family_alias_candidates(family, family_config, catalog) \
        if family and isinstance(family_config, dict) else set()
    alias_keys = {candidate.casefold() for candidate in alias_candidates}
    normalized_keys: set[str] = set()
    normalized_models: dict[str, Any] = {}
    for key, item in models.items():
        if not isinstance(key, str) or not key.strip() or not isinstance(item, dict):
            return {}, "capability_discovery_failed"
        normalized_key = key.casefold()
        resolved_model = item.get("resolved_model")
        efforts = item.get("supported_efforts")
        if normalized_key in normalized_keys:
            return {}, "capability_discovery_failed"
        if (
            not isinstance(resolved_model, str)
            or not resolved_model.strip()
            or not capability_key_matches_model(
                adapter, normalized_key, resolved_model, is_alias=normalized_key in alias_keys
            )
        ):
            return {}, "capability_discovery_failed"
        if adapter == "claude":
            if (
                "supported_efforts" in item
                or not isinstance(item.get("requested_effort"), str)
                or not item["requested_effort"].strip()
                or item.get("effort_verified") is not False
            ):
                return {}, "capability_discovery_failed"
        else:
            if (
                not isinstance(efforts, list)
                or not efforts
                or any(not isinstance(effort, str) or not effort.strip() for effort in efforts)
            ):
                return {}, "capability_discovery_failed"
        normalized_resolved_model = resolved_model.casefold()
        entry_keys = {normalized_key, normalized_resolved_model}
        if any(entry_key in normalized_keys for entry_key in entry_keys):
            return {}, "capability_discovery_failed"
        normalized_keys.update(entry_keys)
        normalized_models[normalized_resolved_model] = item
        normalized_models[normalized_key] = item
    return normalized_models, ""


def resolve_effort(
    args: argparse.Namespace,
    family: str,
    model: str,
    family_config: dict[str, Any],
    requested_effort: str,
    account_default: bool,
) -> tuple[str | None, str, str, str]:
    """Return effective effort, substitution, failure status, capability source."""
    openai_codex = args.adapter == "codex" and family == "openai"
    supported: set[str] | None = None
    capability_source = ""
    if openai_codex:
        if not args.capability_models:
            return None, "", "capability_discovery_failed", "runtime-discovery-failed"
        item = args.capability_models.get(model.lower())
        if not item:
            return None, "", "capability_model_unavailable", "runtime-model-catalog"
        supported = {value.lower() for value in item["supported_efforts"]}
        capability_source = "runtime-model-catalog"
    elif args.capability_models and model.lower() not in args.capability_models:
        return None, "", "capability_model_unavailable", "runtime-model-catalog"

    ultra_eligible = (
        openai_codex
        and args.alias == "flagship"
        and args.role in family_config.get("ultra_eligible_roles", [])
    )
    if requested_effort == "ultra" and not ultra_eligible:
        if args.effort:
            return None, "", "effort_unsupported", "policy"
        fallback = next(
            (
                item
                for item in family_config.get("effort_fallback_order", [])
                if supported is not None and item in supported
            ),
            None,
        )
        if openai_codex and not fallback:
            return None, "", "no_effort_available", capability_source
        fallback = fallback or "high"
        fallback_source = capability_source if supported is not None else "policy"
        return fallback, f"ultra unavailable (route is not ultra-eligible); used {fallback}", "", fallback_source

    if args.effort_transport == "model-id":
        normalized_model = re.sub(r"(?:^|[-_])extra[-_]high(?=$|[-_])", "-xhigh", model.lower())
        matches = re.findall(r"(?:^|[-_])(low|medium|high|xhigh|max|ultra)(?=$|[-_])", normalized_model)
        parenthetical = re.search(r"\((low|medium|high|xhigh|max|ultra)\)\s*$", normalized_model)
        derived = matches[-1] if matches else (parenthetical.group(1) if parenthetical else "")
        if args.effort and derived and args.effort != derived:
            return None, "", "adapter_effort_mismatch", "model-id"
        if args.effort and not derived:
            return None, "", "adapter_effort_unresolved", "model-id-unresolved"
        substitution = ""
        if derived and derived != requested_effort:
            substitution = f"adapter model id controls effort; used {derived}"
        return derived, substitution, "", "model-id" if derived else "model-id-unresolved"
    if args.effort_transport == "none":
        if args.effort:
            return None, "", "effort_unsupported", "adapter-no-effort-control"
        return "", "adapter does not expose effort control", "", "adapter-no-effort-control"

    capability_models = args.capability_models
    if supported is not None:
        pass
    elif capability_models:
        item = capability_models.get(model.lower())
        if not item:
            return None, "", "capability_model_unavailable", "runtime-model-catalog"
        if args.adapter == "claude" and item.get("effort_verified") is False:
            if item["requested_effort"].lower() != requested_effort:
                return None, "", "effort_capability_unverified", "provider-unverified"
            return requested_effort, "", "", "provider-unverified"
        supported = {value.lower() for value in item["supported_efforts"]}
        capability_source = "runtime-model-catalog"
    elif args.available_effort:
        supported = {item.lower() for item in args.available_effort}
        capability_source = "caller-runtime"
    else:
        return requested_effort, "", "", "provider-unverified"

    if requested_effort in supported:
        return requested_effort, "", "", capability_source
    if args.effort:
        return None, "", "effort_unsupported", capability_source
    fallback = next(
        (
            item
            for item in family_config.get("effort_fallback_order", [])
            if item in supported
            and EFFORT_ORDER[item] <= EFFORT_ORDER[requested_effort]
        ),
        None,
    )
    if not fallback:
        return None, "", "no_effort_available", capability_source
    if (
        args.task_class_effort
        and EFFORT_ORDER[fallback] < EFFORT_ORDER[args.task_class_effort]
    ):
        return None, "", "task_class_effort_below_floor", capability_source
    substitution = f"{requested_effort} unavailable (runtime/model capability); used {fallback}"
    return fallback, substitution, "", capability_source


def resolve(args: argparse.Namespace, catalog: dict[str, Any]) -> int:
    capability_models, capability_error = load_capabilities(
        args.capabilities_file, args.adapter, catalog
    )
    args.capability_models = capability_models
    adapter = catalog["adapters"].get(args.adapter)
    fixed_family = adapter.get("fixed_model_family") if adapter else None
    family_config = catalog["families"].get(fixed_family, {}) if fixed_family else {}
    # Normalise the alias table once, at its single load site, so no reader further
    # down dereferences a table that is not one. Several did, and each crashed with
    # no JSON for the caller instead of rejecting. Whether an absent alias table is
    # fatal depends on the route, and is decided below where that is known.
    # ``family_config`` itself needs no guard: a pinned family that is not a mapping
    # is rejected by the fixed-family validation in ``main`` before this runs.
    if not isinstance(family_config.get("aliases"), dict):
        family_config = {**family_config, "aliases": {}}
    role_effort = family_config.get("role_effort_defaults", {}).get(args.role, {}).get(args.alias)
    task_class_effort = args.task_class_effort
    if role_effort and role_effort not in EFFORT_ORDER:
        record = {
            "schema_version": 1, "status": "role_effort_config_invalid",
            "adapter": args.adapter, "alias": args.alias, "role": args.role,
            "requested_effort": task_class_effort or "", "effort": "",
        }
        if args.task_class:
            record.update({"task_class": args.task_class, "route_source": "task-class"})
        return emit(record, 2)
    model_override_effort = args.model_override.get("default_effort", "")
    if task_class_effort:
        if role_effort and EFFORT_ORDER[role_effort] > EFFORT_ORDER[task_class_effort]:
            requested_effort, effort_source = role_effort, "role-default"
        else:
            requested_effort, effort_source = task_class_effort, "task-class"
    else:
        requested_effort = args.effort or model_override_effort or role_effort or {
            "flagship": "high", "workhorse": "medium", "scout": "low"
        }[args.alias]
        effort_source = (
            "explicit" if args.effort else
            "model-override" if model_override_effort else
            "role-default" if role_effort else "alias-default"
        )
    base = {
        "schema_version": 1,
        "catalog_date": catalog["catalog_date"],
        "adapter": args.adapter,
        "alias": args.alias,
        "role": args.role,
        "requested_effort": requested_effort,
        "effort": requested_effort,
        "effort_source": effort_source,
        "lead_family": args.lead_family,
    }
    if args.task_class:
        base.update({"task_class": args.task_class, "route_source": "task-class"})
    elif args.model_override_tier:
        override_models = args.model_override.get("models", [])
        override_roles = args.model_override.get("roles", [])
        base.update({
            "model_override_tier": args.model_override_tier,
            "route_source": "model-override",
            "policy_override": (
                f"{args.model_override_tier}-{override_models[0]}-{'-'.join(override_roles)}"
            ),
        })
    if not adapter:
        return emit({**base, "status": "unknown_adapter"}, 2)
    args.effort_transport = adapter.get("effort_transport", "none")
    # account-default adapters dispatch on the provider account's default
    # model: the runtime rejects explicit model ids, so the resolver keeps the
    # catalog id for effort/audit lookups but emits an empty dispatch model.
    account_default = adapter.get("model_selection") == "account-default"

    def emit_route(record: dict[str, Any], code: int) -> int:
        """Emit, never exposing a catalog id as a dispatchable model (#190)."""
        resolved = record.get("resolved_model")
        if account_default and isinstance(resolved, str) and resolved:
            record = {
                **record,
                "resolved_model": "",
                "catalog_model": resolved,
                "model_selection": "account-default",
            }
        return emit(record, code)

    endpoint = adapter["endpoint_provider"]
    compatibility: dict[str, Any] | None = None
    compatibility_metadata: dict[str, Any] = {}
    if args.adapter in COMPATIBILITY_ADAPTER_IDS:
        compatibility, compatibility_status = load_adapter_compatibility(
            args.adapter, Path(args.adapter_compatibility),
        )
        if compatibility_status:
            return emit_route(
                {
                    **base,
                    "status": compatibility_status,
                    "endpoint_provider": endpoint,
                },
                2,
            )
        compatibility_metadata = {
            "compatibility_adapter": compatibility["compatibility_adapter"],
            "adapter_enabled": compatibility["enabled"],
        }
        if not compatibility["enabled"]:
            return emit_route(
                {
                    **base,
                    "status": "adapter_disabled",
                    "reason": compatibility["disabled_reason"],
                    "endpoint_provider": endpoint,
                    **compatibility_metadata,
                },
                1,
            )
        if account_default != (not compatibility["requires_explicit_model"]):
            # The routing catalogue and adapter policy must agree on
            # account-default dispatch in both directions (#190).
            return emit_route(
                {
                    **base,
                    "status": "account_default_conflicts_with_compatibility",
                    "endpoint_provider": endpoint,
                    **compatibility_metadata,
                },
                2,
            )
    substitution = ""
    fallback_model = ""
    identity_source = ""

    if args.model:
        if account_default:
            candidates = family_config.get("role_overrides", {}).get(args.role, {}).get(args.alias)
            candidates = candidates or family_config.get("aliases", {}).get(args.alias, [])
            return emit(
                {
                    **base,
                    "status": "adapter_account_default_only",
                    "endpoint_provider": endpoint,
                    "model_family": fixed_family,
                    "resolved_model": "",
                    "requested_model": args.model,
                    "catalog_model": candidates[0] if candidates else "",
                    "model_selection": "account-default",
                    "identity_source": "account-default",
                    **compatibility_metadata,
                },
                1,
            )
        model = args.model
        family = infer_family(model, catalog)
        identity_source = "model-pattern"
        if not family:
            return emit_route(
                {
                    **base,
                    "status": "model_family_unknown",
                    "endpoint_provider": endpoint,
                    "resolved_model": model,
                },
                1,
            )
        selected_override_model = (
            args.model_override.get("models", [""])[0] if args.model_override else ""
        )
        if args.model_override and not model_has_alias(model, selected_override_model):
            return emit_route({**base, "status": "risk_tier_model_mismatch"}, 1)
        if fixed_family and family != fixed_family:
            return emit_route(
                {
                    **base,
                    "status": "adapter_family_mismatch",
                    "endpoint_provider": endpoint,
                    "model_family": family,
                    "resolved_model": model,
                },
                1,
            )
    else:
        # An adapter whose pinned family the catalogue leaves undefined, or defines
        # without an alias table, has no alias to resolve against and must be given
        # an explicit model, exactly as a broker must. OpenCode is pinned to
        # ``generic-open``, which the catalogue deliberately omits: this path
        # crashed on the production catalogue rather than saying so.
        family_aliases = catalog["families"].get(fixed_family, {}) if fixed_family else {}
        family_aliases = (
            family_aliases.get("aliases") if isinstance(family_aliases, dict) else None
        )
        if not fixed_family or not isinstance(family_aliases, dict):
            if args.adapter != "agy" or not args.task_class or not capability_models:
                return emit_route(
                    {**base, "status": "model_required_for_broker", "endpoint_provider": endpoint},
                    2,
                )
            preferences = adapter.get("model_family_preferences", {}).get("preferred")
            if (
                not isinstance(preferences, list)
                or not preferences
                or any(not isinstance(item, str) or not item for item in preferences)
            ):
                return emit_route(
                    {**base, "status": "broker_preference_config_invalid", "endpoint_provider": endpoint},
                    2,
                )
            available = {
                key.lower(): (item["resolved_model"], "runtime-capability+catalog")
                for key, item in capability_models.items()
            }
            candidates = []
            chosen = None
            for preferred_family in preferences:
                preferred_config = catalog["families"].get(preferred_family)
                if not isinstance(preferred_config, dict):
                    continue
                aliases = preferred_config.get("aliases")
                if not isinstance(aliases, dict):
                    continue
                preferred_candidates = aliases.get(args.alias)
                if not isinstance(preferred_candidates, list):
                    continue
                candidates.extend(preferred_candidates)
                selected = next(
                    (
                        candidate for candidate in preferred_candidates
                        if isinstance(candidate, str) and candidate.lower() in available
                    ),
                    None,
                )
                if selected is not None:
                    family = preferred_family
                    family_config = preferred_config
                    chosen = selected
                    break
            if chosen is None:
                return emit_route(
                    {
                        **base,
                        "status": "no_candidate_available",
                        "endpoint_provider": endpoint,
                        "candidates": candidates,
                    },
                    1,
                )
            model, identity_source = available[chosen.lower()]
            substitution = ""
        else:
            family = fixed_family
            candidates = args.model_override.get("models")
            candidates = candidates or family_config.get("role_overrides", {}).get(args.role, {}).get(args.alias)
            candidates = candidates or family_config["aliases"].get(args.alias)
        if not candidates:
            return emit_route({**base, "status": "alias_unavailable", "model_family": family}, 1)
        if not fixed_family:
            pass
        elif account_default:
            model = candidates[0]
            fallback_model = candidates[1] if len(candidates) > 1 else ""
            identity_source = "account-default"
        else:
            available = {item.lower(): (item, "caller-runtime+catalog") for item in args.available_model}
            if capability_models:
                available.update(
                    {key.lower(): (item["resolved_model"], "runtime-capability+catalog") for key, item in capability_models.items()}
                )
            if available:
                chosen = next((candidate for candidate in candidates if candidate.lower() in available), None)
                if not chosen:
                    return emit_route(
                        {
                            **base,
                            "status": "no_candidate_available",
                            "endpoint_provider": endpoint,
                            "model_family": family,
                            "candidates": candidates,
                        },
                        1,
                    )
                model, identity_source = available[chosen.lower()]
                if chosen != candidates[0]:
                    substitution = f"{candidates[0]} unavailable; used {chosen}"
            else:
                model = candidates[0]
                fallback_model = candidates[1] if len(candidates) > 1 else ""
                identity_source = "dated-catalog"

    override_families = tuple(override_scan_families(model, catalog).values())
    configured_override_models = [
        candidate
        for override_family in override_families
        if isinstance(override_family, dict)
        for configured_overrides in (override_family.get("risk_tier_overrides"),)
        if isinstance(configured_overrides, dict)
        for configured_override in configured_overrides.values()
        if isinstance(configured_override, dict)
        for candidates in (configured_override.get("models"),)
        if isinstance(candidates, list)
        for candidate in candidates
        if isinstance(candidate, str) and candidate.strip()
    ]
    is_risk_override_model = any(
        model_has_alias(model, candidate) for candidate in configured_override_models
    )
    if is_risk_override_model and not args.model_override:
        return emit_route({**base, "status": "risk_tier_override_required"}, 1)
    compatibility_family = ""
    distinct = bool(args.lead_family and family != args.lead_family)
    if compatibility and args.require_distinct and not args.lead_family:
        return emit_route(
            {
                **base,
                "status": "lead_family_required",
                "endpoint_provider": endpoint,
                "model_family": family,
                "resolved_model": model,
                "identity_source": identity_source,
                **compatibility_metadata,
            },
            2,
        )
    if compatibility and args.require_distinct and not distinct:
        return emit_route(
            {
                **base,
                "status": "same_family_forbidden",
                "endpoint_provider": endpoint,
                "model_family": family,
                "resolved_model": model,
                "identity_source": identity_source,
                "distinct_from_lead": False,
                **compatibility_metadata,
            },
            1,
        )

    if compatibility:
        compatibility_family, compatibility_status = check_adapter_compatibility(
            compatibility, family, model
        )
        if compatibility_status:
            return emit_route(
                {
                    **base,
                    "status": compatibility_status,
                    "endpoint_provider": endpoint,
                    "model_family": family,
                    "resolved_model": model,
                    "identity_source": identity_source,
                    "compatibility_model_family": compatibility_family,
                    **compatibility_metadata,
                },
                1,
            )

    if capability_error:
        return emit_route(
            {
                **base,
                "status": capability_error,
                "effort": "",
                "effort_substitution": "",
                "effort_capability_source": "runtime-discovery-failed",
                "endpoint_provider": endpoint,
                "model_family": family,
                "resolved_model": model,
                "identity_source": identity_source,
            },
            1,
        )

    effort, effort_substitution, effort_status, capability_source = resolve_effort(
        args, family, model, family_config, requested_effort, account_default
    )
    # A Claude snapshot cannot evidence the effective effort, but its existence
    # does evidence that the CLI accepted the requested value: the canary fails
    # closed on the unknown-effort warning. Paired with runtime-verified model
    # identity that is enough to admit a task-class route at exactly the probed
    # effort. The receipt keeps the weaker `provider-unverified` provenance, and
    # resolve_effort has already rejected any other effort.
    claude_effort_unverified = (
        args.adapter == "claude"
        and capability_source == "provider-unverified"
        and isinstance(capability_models.get(model.lower()), dict)
        and capability_models[model.lower()].get("effort_verified") is False
    )
    if effort_status:
        return emit_route(
            {
                **base,
                "status": effort_status,
                "effort": "",
                "effort_substitution": "",
                "effort_capability_source": capability_source,
                "endpoint_provider": endpoint,
                "model_family": family,
                "resolved_model": model,
                "identity_source": identity_source,
            },
            1,
        )

    if args.task_class and (
        (not account_default and identity_source != "runtime-capability+catalog")
        or (capability_source != "runtime-model-catalog" and not claude_effort_unverified)
    ):
        return emit_route(
            {
                **base,
                "status": "task_class_capability_unverified",
                "effort": "",
                "effort_substitution": "",
                "effort_capability_source": capability_source,
                "endpoint_provider": endpoint,
                "model_family": family,
                "resolved_model": model,
                "identity_source": identity_source,
            },
            1,
        )

    record = {
        **base,
        "effort": effort,
        "effort_substitution": effort_substitution,
        "effort_capability_source": capability_source,
        "status": "ok",
        "endpoint_provider": endpoint,
        "model_family": family,
        "resolved_model": model,
        "identity_source": identity_source,
        "substitution": substitution,
        "fallback_model": fallback_model,
        "distinct_from_lead": distinct,
    }
    if account_default:
        record.update(
            {
                "resolved_model": "",
                "catalog_model": model,
                "model_selection": "account-default",
                "identity_source": "account-default",
            }
        )
    if compatibility:
        record.update(
            {
                **compatibility_metadata,
                "compatibility_model_family": compatibility_family,
            }
        )
    if args.require_distinct and not args.lead_family:
        return emit_route({**record, "status": "lead_family_required"}, 2)
    if args.require_distinct and not distinct:
        return emit_route({**record, "status": "same_family_forbidden"}, 1)
    return emit_route(record, 0)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    command = commands.add_parser("resolve")
    command.add_argument("--adapter", required=True)
    command.add_argument("--alias")
    command.add_argument("--task-class")
    command.add_argument("--model-override-tier", choices=("routine", "substantial", "crucial", "terminal"))
    command.add_argument("--role", required=True)
    command.add_argument("--effort")
    command.add_argument("--model")
    command.add_argument("--available-model", action="append", default=[])
    command.add_argument("--available-effort", action="append", default=[])
    command.add_argument("--capabilities-file")
    command.add_argument("--lead-family")
    command.add_argument("--require-distinct", action="store_true")
    command.add_argument(
        "--catalog",
        default=str(CATALOG_PATH),
        help=argparse.SUPPRESS,
    )
    command.add_argument(
        "--adapter-compatibility",
        default=str(COMPATIBILITY_PATH),
        help=argparse.SUPPRESS,
    )
    _preferences.add_selection_parser(
        commands, INSTANCE_ROOT / "config" / "model-preferences.json",
    )
    return root


def main(argv: list[str] | None = None) -> int:
    argument_parser = parser()
    args = argument_parser.parse_args(argv)
    if args.command == "select":
        return _preferences.select(args, TASK_CLASS_POLICY, ALIAS_ORDER, EFFORT_ORDER)
    catalog = load_catalog(Path(args.catalog))
    if args.command == "resolve":
        def reject(
            status: str,
            *,
            alias: str = "",
            effort: str = "",
            message: str = "",
            code: int = 2,
            **metadata: Any,
        ) -> int:
            record = {
                "schema_version": 1,
                "catalog_date": catalog.get("catalog_date", ""),
                "status": status,
                "adapter": args.adapter,
                "role": args.role,
                "alias": alias or args.alias or "",
                "requested_effort": effort or args.effort or "",
                "effort": "",
                "lead_family": args.lead_family,
            }
            if args.task_class:
                record.update({"task_class": args.task_class, "route_source": "task-class"})
            if message:
                record["message"] = message
            record.update(metadata)
            return emit(record, code)

        args.task_class_effort = ""
        args.model_override = {}
        if not args.alias and not args.task_class:
            return reject("route_input_missing")
        # A families table that is not a mapping reserves nothing, so a reservation
        # scan finds no occupant and would route a reserved model. It is also the
        # first thing every family lookup below dereferences. Reject it here, ahead
        # of those lookups: an unusable catalogue must fail closed with the router's
        # structured rejection, never fall open and never crash without one.
        families = catalog.get("families")
        if not isinstance(families, dict):
            return reject("risk_tier_config_invalid", alias=args.alias)
        adapter_config = catalog.get("adapters", {}).get(args.adapter, {})
        adapter_family = (
            adapter_config.get("fixed_model_family")
            if isinstance(adapter_config, dict)
            else None
        )
        if adapter_family and not risk_tier_overrides_are_valid(
            adapter_family, families.get(adapter_family, {}), catalog
        ):
            return reject("risk_tier_config_invalid", alias=args.alias)
        if adapter_family and not ultra_eligible_roles_are_valid(
            families.get(adapter_family, {})
        ):
            return reject(
                "effort_policy_config_invalid",
                alias=args.alias,
                message=(
                    "configuration error: ultra_eligible_roles must be a list "
                    "of non-empty role names"
                ),
            )
        # Validate exactly the families the reservation scan will consult, and only
        # when there is a model for it to scan against. An adapter without a fixed
        # model family validates nothing above, so without this a malformed
        # override was routed against instead of failing closed.
        if args.model:
            for scanned_family, scanned_config in override_scan_families(
                args.model, catalog
            ).items():
                if not risk_tier_overrides_are_valid(
                    scanned_family, scanned_config, catalog
                ):
                    return reject("risk_tier_config_invalid", alias=args.alias)
        # Once catalogue integrity is known, a configured execution gate is the
        # first route fact for a known adapter. Invalid selectors must not hide
        # `enabled: false` or its typed reason, and this preflight never invokes a
        # provider capability source.
        if args.adapter in COMPATIBILITY_ADAPTER_IDS:
            compatibility, compatibility_status = load_adapter_compatibility(
                args.adapter, Path(args.adapter_compatibility),
            )
            if compatibility_status:
                return reject(compatibility_status)
            assert compatibility is not None
            if not compatibility["enabled"]:
                endpoint = (
                    adapter_config.get("endpoint_provider", "")
                    if isinstance(adapter_config, dict)
                    else ""
                )
                return reject(
                    "adapter_disabled",
                    code=1,
                    reason=compatibility["disabled_reason"],
                    endpoint_provider=endpoint,
                    compatibility_adapter=compatibility["compatibility_adapter"],
                    adapter_enabled=False,
                )
        if args.task_class and args.model_override_tier:
            return reject("route_input_conflict")
        if bool(args.alias) == bool(args.task_class):
            return reject("route_input_conflict" if args.alias else "route_input_missing")
        if args.task_class:
            policy = TASK_CLASS_POLICY.get(args.task_class)
            route = catalog.get("task_class_routes", {}).get(args.task_class)
            if policy is None or route is None:
                return reject("unknown_task_class")
            if not isinstance(route, dict):
                return reject("task_class_config_invalid")
            route_alias = route.get("alias")
            route_effort = route.get("effort")
            route_role = route.get("role")
            if (
                route_alias not in ALIAS_ORDER
                or route_effort not in EFFORT_ORDER
                or ALIAS_ORDER[route_alias] < ALIAS_ORDER[policy["minimum_alias"]]
                or route_role != policy["role"]
            ):
                return reject(
                    "task_class_config_invalid",
                    alias=route_alias if isinstance(route_alias, str) else "",
                    effort=route_effort if isinstance(route_effort, str) else "",
                )
            if route_effort != policy["minimum_effort"]:
                return reject(
                    "task_class_config_invalid",
                    alias=route_alias,
                    effort=route_effort,
                    message=(
                        "configuration error: "
                        f"task_class_routes.{args.task_class}.effort {route_effort!r} "
                        "must equal probe policy minimum_effort "
                        f"{policy['minimum_effort']!r}"
                    ),
                )
            if args.effort:
                return reject("task_class_effort_conflict", alias=route_alias, effort=route_effort)
            if args.model:
                return reject("task_class_model_conflict", alias=route_alias, effort=route_effort)
            if args.role != route_role:
                return reject("task_class_role_mismatch", alias=route_alias, effort=route_effort)
            args.alias = route_alias
            args.task_class_effort = route_effort
        elif args.alias not in {"flagship", "workhorse", "scout"}:
            return reject("unknown_alias")
        if args.effort and args.effort not in EFFORT_ORDER:
            return reject("invalid_effort", alias=args.alias)
        if args.model_override_tier:
            adapter = catalog.get("adapters", {}).get(args.adapter, {})
            family = adapter.get("fixed_model_family")
            family_config = catalog.get("families", {}).get(family, {})
            override = family_config.get("risk_tier_overrides", {}).get(args.model_override_tier)
            if not isinstance(override, dict):
                return reject("risk_tier_override_unavailable", alias=args.alias)
            if not risk_tier_override_is_well_formed(override):
                return reject("risk_tier_config_invalid", alias=args.alias)
            maximum_effort = override["maximum_effort"]
            roles = override["roles"]
            if args.role not in roles:
                return reject("risk_tier_role_mismatch", alias=args.alias)
            if args.alias != override.get("alias"):
                return reject("risk_tier_alias_mismatch", alias=args.alias)
            if args.effort and EFFORT_ORDER[args.effort] > EFFORT_ORDER[maximum_effort]:
                return reject("risk_tier_effort_above_ceiling", alias=args.alias, effort=args.effort)
            args.model_override = override
        return resolve(args, catalog)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
