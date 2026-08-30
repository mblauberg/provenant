#!/usr/bin/env python3
"""Catalogue-validation unit for `model_route.py`.

The cohesive family/alias/risk-tier-override validators that `model_route.py`
consults before it will route against a catalog. Extracted as its own module
purely to keep `model_route.py` under the repository's 1,000-line source cap
(#422); it has no callers outside `model_route.py` and its tests.

`model_route.py` loads this module by path (see the loader block near the top
of that file) and re-binds these names into its own namespace, so it remains
the single place callers (the `scripts/model-route` shim, `main`, `resolve`)
reach for them. `EFFORT_ORDER` and `ALIAS_ORDER` live here, not there, only
because these validators need them and a module loaded by path cannot import
back from the module that loaded it without a cycle; `model_route.py` imports
both back unchanged.
"""

from __future__ import annotations

import re
from typing import Any


EFFORT_ORDER = {name: index for index, name in enumerate(("low", "medium", "high", "xhigh", "max", "ultra"))}
ALIAS_ORDER = {name: index for index, name in enumerate(("scout", "workhorse", "flagship"))}


def infer_family(model: str, catalog: dict[str, Any]) -> str | None:
    lowered = model.lower()
    for item in catalog["model_patterns"]:
        if re.search(item["pattern"], lowered):
            return item["family"]
    return None


def model_has_alias(model: str, alias: str) -> bool:
    return alias.casefold() in model.casefold()


def capability_key_matches_model(
    adapter: str, key: str, resolved_model: str, *, is_alias: bool
) -> bool:
    """Bind a snapshot key to the resolved identity using the adapter's ID shape."""
    normalized_key = key.casefold()
    normalized_model = resolved_model.casefold()
    if adapter == "claude":
        if is_alias:
            return normalized_model.startswith(f"claude-{normalized_key}-")
        return (
            normalized_model.startswith("claude-")
            and normalized_key in normalized_model.split("-")
        )
    return normalized_model == normalized_key


def ultra_eligible_roles_are_valid(family_config: Any) -> bool:
    """Validate the role collection before routing uses membership as a gate."""
    if not isinstance(family_config, dict):
        return False
    roles = family_config.get("ultra_eligible_roles", [])
    return isinstance(roles, list) and all(
        isinstance(role, str) and bool(role.strip()) and role == role.strip()
        for role in roles
    )


def risk_tier_override_is_well_formed(override: Any) -> bool:
    """Structural validity of one risk-tier override block, independent of catalogue context.

    The single source of the shape rules. Both the catalogue-wide validation pass and
    the ``--model-override-tier`` selection branch consult this, so a block can never be usable
    in one and malformed in the other.
    """
    if not isinstance(override, dict):
        return False
    alias = override.get("alias")
    default_effort = override.get("default_effort")
    maximum_effort = override.get("maximum_effort")
    roles = override.get("roles")
    models = override.get("models")
    return (
        isinstance(alias, str)
        and alias in ALIAS_ORDER
        and isinstance(default_effort, str)
        and default_effort in EFFORT_ORDER
        and isinstance(maximum_effort, str)
        and maximum_effort in EFFORT_ORDER
        and EFFORT_ORDER[default_effort] <= EFFORT_ORDER[maximum_effort]
        and isinstance(roles, list)
        and len(roles) == 2
        and all(
            isinstance(role, str) and role.strip() and role == role.strip() for role in roles
        )
        and len(set(roles)) == 2
        and isinstance(models, list)
        and len(models) == 1
        and isinstance(models[0], str)
        and bool(models[0].strip())
        and models[0] == models[0].strip()
    )


def family_alias_candidates(family: str, family_config: dict[str, Any], catalog: dict[str, Any]) -> set[str]:
    """Every model name alias routing can reach within one family, including adapter-prefixed forms."""
    aliases = family_config.get("aliases", {})
    candidates = {
        candidate
        for candidate_list in (aliases.values() if isinstance(aliases, dict) else ())
        if isinstance(candidate_list, list)
        for candidate in candidate_list
        if isinstance(candidate, str)
    }
    role_overrides = family_config.get("role_overrides", {})
    candidates.update(
        candidate
        for role_aliases in (role_overrides.values() if isinstance(role_overrides, dict) else ())
        if isinstance(role_aliases, dict)
        for candidate_list in role_aliases.values()
        if isinstance(candidate_list, list)
        for candidate in candidate_list
        if isinstance(candidate, str)
    )
    adapter_prefixes = {
        adapter_name
        for adapter_name, adapter_config in catalog.get("adapters", {}).items()
        if isinstance(adapter_config, dict)
        and adapter_config.get("fixed_model_family") == family
    }
    candidates.update(
        f"{adapter_prefix}-{candidate}"
        for adapter_prefix in adapter_prefixes
        for candidate in tuple(candidates)
    )
    return candidates


def risk_tier_overrides_are_valid(
    family: str, family_config: Any, catalog: dict[str, Any]
) -> bool:
    """Every risk-tier override under ``family`` is well-formed and outside alias reach.

    An occupant that alias routing can also resolve would be simultaneously gated and
    freely reachable, so the catalogue is rejected rather than routed against.
    """
    if not isinstance(family_config, dict):
        return False
    overrides = family_config.get("risk_tier_overrides", {})
    if not isinstance(overrides, dict):
        return False
    if not overrides:
        return True
    candidates = family_alias_candidates(family, family_config, catalog)
    for override in overrides.values():
        if not risk_tier_override_is_well_formed(override):
            return False
        occupant = override["models"][0]
        if (
            occupant in ALIAS_ORDER
            or infer_family(occupant, catalog) is None
            or any(
                model_has_alias(candidate, occupant) or model_has_alias(occupant, candidate)
                for candidate in candidates
            )
        ):
            return False
    return True


def override_scan_families(model: str, catalog: dict[str, Any]) -> dict[str, Any]:
    """The families whose risk-tier overrides can reserve an occupant for ``model``.

    The reservation scan and the catalogue validation pass must agree on this set.
    While they disagreed, a malformed override in a family the scan consulted but
    the validation skipped silently unreserved its occupant: an adapter with no
    ``fixed_model_family`` skipped validation entirely, yet still scanned the
    model's own family and routed against its broken overrides.
    """
    families = catalog.get("families", {})
    if not isinstance(families, dict):
        return {}
    family = infer_family(model, catalog) if model else None
    configured = families.get(family) if family else None
    if isinstance(configured, dict) and configured:
        return {family: configured}
    return families
