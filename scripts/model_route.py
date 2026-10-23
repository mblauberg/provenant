#!/usr/bin/env python3
"""Resolve durable harness model aliases into auditable concrete routes."""

from __future__ import annotations

import argparse
import difflib
import fcntl
import hashlib
from datetime import datetime, timezone
from fnmatch import fnmatchcase
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any


def _sibling(name: str) -> Any:
    module = sys.modules.get(name)
    if module is not None:
        return module
    path = Path(__file__).resolve().parent / f"{name}.py"
    if name == "provenant_roots":
        path = Path(__file__).resolve().parents[1] / "scripts" / "lib" / "roots.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


# `scripts/lib/roots.py` is the single resolver for the product root (#754).
# The fallback loads that one file when this script is run directly by path and
# the product root is not on `sys.path`: it locates the resolver, it does not
# decide the root, and it leaves import resolution untouched (#755).
try:
    from scripts.lib.roots import product_root
except ModuleNotFoundError:  # pragma: no cover - direct invocation by path
    product_root = _sibling("provenant_roots").product_root

PRODUCT_ROOT = product_root()
INSTANCE_ROOT = Path(
    os.environ.get("AGENT_FABRIC_INSTANCE_ROOT") or Path.home() / ".agents"
).expanduser()
CATALOG_PATH = INSTANCE_ROOT / "config" / "model-routing.json"
PRODUCT_CATALOG_PATH = PRODUCT_ROOT / "config" / "model-routing.json"
COMPATIBILITY_PATH = PRODUCT_ROOT / "config" / "adapter-compatibility.yaml"
COMPATIBILITY_ADAPTER_IDS = {
    "claude": "claude-agent-sdk",
    "codex": "codex-app-server",
    "agy": "agy",
    "cursor": "cursor-agent",
    "copilot": "copilot",
    "kiro": "kiro-acp",
    "opencode": "opencode-acp",
    "pi": "pi-rpc",
}
# The wire formats a provider CLI can be told to speak. Anything else is a
# configuration error rather than a value to pass through to the CLI.
ENDPOINT_WIRE_APIS = frozenset({"responses", "chat"})
ROUTE_WARNINGS: list[str] = []
TASK_CLASS_POLICY = {
    "mechanical": {"minimum_alias": "junior", "minimum_effort": "low", "role": "worker"},
    "legwork": {"minimum_alias": "senior", "minimum_effort": "medium", "role": "worker"},
    "implementation": {"minimum_alias": "senior", "minimum_effort": "high", "role": "worker"},
    "ui-taste": {"minimum_alias": "principal", "minimum_effort": "high", "role": "worker"},
    "screenshots": {"minimum_alias": "senior", "minimum_effort": "high", "role": "worker"},
    "review": {"minimum_alias": "principal", "minimum_effort": "high", "role": "reviewer"},
    "second-opinion": {"minimum_alias": "principal", "minimum_effort": "high", "role": "reviewer"},
    "research": {"minimum_alias": "senior", "minimum_effort": "medium", "role": "worker"},
    "bulk": {"minimum_alias": "junior", "minimum_effort": "low", "role": "worker"},
    "critical-review": {"minimum_alias": "principal", "minimum_effort": "high", "role": "critical-review"},
    "orchestration": {"minimum_alias": "principal", "minimum_effort": "high", "role": "orchestrator"},
}

# Path-loading avoids depending on `scripts/` being on `sys.path`. Tests reload
# this router by path, so each sibling must be reused rather than re-executed.
# Registering one module but binding names from another makes their globals
# diverge: rebinding `infer_family` on one would be invisible to functions on
# the other. Reuse each cached module for normal import-like identity.
_catalog_validation = _sibling("model_route_catalog")
_preferences = _sibling("model_route_preferences")
_pools = _sibling("model_route_pools")
model_route_probe = _sibling("model_route_probe")
model_route_live = _sibling("model_route_live")
model_route_live.model_route_probe = model_route_probe

EFFORT_ORDER = _catalog_validation.EFFORT_ORDER
# The effort ladder `effort.shift` steps along. `ultra` is opt-in only, so a shift never reaches it.
SHIFT_LADDER = tuple(name for name in EFFORT_ORDER if name != "ultra")


def _harness_profile() -> Any:
    """The shared profile resolver, loaded by path on first use and only for task-class routes."""
    module = sys.modules.get("provenant_harness_profile")
    if module is None:
        spec = importlib.util.spec_from_file_location(
            "provenant_harness_profile", Path(__file__).resolve().parents[1] / "skills/_shared/harness_profile.py")
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        sys.modules["provenant_harness_profile"] = module
        spec.loader.exec_module(module)
    return module


def shifted_task_class_effort(task_class: str, effort: str, minimum: str, entries: list[dict[str, Any]]) -> tuple[str, int]:
    """Apply the profile's `effort.shift` one step, or keep `effort` when the step leaves policy or a model's efforts."""
    shift = _harness_profile().value("effort.shift", 0, task_class=task_class)
    if not shift or effort not in SHIFT_LADDER:
        return effort, 0
    target = SHIFT_LADDER.index(effort) + shift
    if not 0 <= target < len(SHIFT_LADDER):
        return effort, 0
    candidate = SHIFT_LADDER[target]
    if (EFFORT_ORDER[candidate] < EFFORT_ORDER[minimum]
            or any(candidate not in entry.get("efforts", []) for entry in entries)):
        return effort, 0
    return candidate, shift
ALIAS_ORDER = _catalog_validation.ALIAS_ORDER
infer_family = _catalog_validation.infer_family
matching_model_families = _catalog_validation.matching_model_families
model_slug_for_family = _catalog_validation.model_slug_for_family
family_is_assurance_eligible = _catalog_validation.family_is_assurance_eligible
attribute_model_family = _catalog_validation.attribute_model_family
model_has_alias = _catalog_validation.model_has_alias
risk_tier_override_reserves_model = _catalog_validation.risk_tier_override_reserves_model
capability_key_matches_model = _catalog_validation.capability_key_matches_model
ultra_eligible_roles_are_valid = _catalog_validation.ultra_eligible_roles_are_valid
risk_tier_override_is_well_formed = _catalog_validation.risk_tier_override_is_well_formed
family_alias_candidates = _catalog_validation.family_alias_candidates
risk_tier_overrides_are_valid = _catalog_validation.risk_tier_overrides_are_valid
override_scan_families = _catalog_validation.override_scan_families


def _merge_catalog(base: Any, overlay: Any, path: str, drift: list[str]) -> Any:
    if isinstance(base, dict):
        if not isinstance(overlay, dict):
            drift.append(f"{path}: malformed overlay entry dropped; fix: use an object")
            return base
        merged = dict(base)
        for key, value in overlay.items():
            child = f"{path}.{key}" if path else key
            if key in base:
                merged[key] = _merge_catalog(base[key], value, child, drift)
            elif path in {"adapters", "families", "endpoints"} and not isinstance(value, dict):
                drift.append(f"{child}: malformed overlay entry dropped; fix: use an object")
            elif path == "routes" and not (
                isinstance(value, list) and value
                and all(_pools.entry_is_valid(entry, EFFORT_ORDER) for entry in value)
            ):
                drift.append(f"{child}: malformed overlay entry dropped; fix: list adapter/model entries with weights")
            elif path == "model_traits" and not (
                isinstance(value, list) and all(isinstance(trait, str) and trait for trait in value)
            ):
                drift.append(f"{child}: malformed overlay entry dropped; fix: use a list of trait names")
            elif path == "route_synonyms" and not isinstance(value, str):
                drift.append(f"{child}: malformed overlay entry dropped; fix: name a route")
            elif path == "adapters" and not (
                isinstance(value.get("endpoint_provider"), str)
                and (value.get("fixed_model_family") is None or isinstance(value.get("fixed_model_family"), str))
                and isinstance(value.get("effort_transport"), str)
                and isinstance(value.get("models", []), list)
                and all(isinstance(item, dict) and isinstance(item.get("id"), str)
                        and isinstance(item.get("names", []), list)
                        and all(isinstance(name, str) for name in item.get("names", []))
                        and isinstance(item.get("efforts", []), list)
                        and all(effort in EFFORT_ORDER for effort in item.get("efforts", []))
                        for item in value.get("models", []))
            ):
                drift.append(f"{child}: malformed overlay entry dropped; fix: complete the adapter profile")
            elif path == "families" and not (
                isinstance(value.get("aliases", {}), dict)
                and all(isinstance(models, list) and all(isinstance(model, str) for model in models)
                        for models in value.get("aliases", {}).values())
            ):
                drift.append(f"{child}: malformed overlay entry dropped; fix: use alias model lists")
            elif path == "endpoints" and not (
                isinstance(value.get("base_url"), str)
                and isinstance(value.get("token_env"), str)
                and isinstance(value.get("model_family"), str)
                and isinstance(value.get("adapters"), list)
            ):
                drift.append(f"{child}: malformed overlay entry dropped; fix: complete the endpoint profile")
            else:
                merged[key] = value
        return merged
    if isinstance(base, list) and re.fullmatch(r"adapters\.[^.]+\.models", path):
        if not isinstance(overlay, list):
            drift.append(f"{path}: malformed overlay entry dropped; fix: use a model list")
            return base
        merged = {item["id"]: item for item in base if isinstance(item, dict) and isinstance(item.get("id"), str)}
        for index, item in enumerate(overlay):
            if not isinstance(item, dict) or not isinstance(item.get("id"), str) or not item["id"].strip():
                drift.append(f"{path}[{index}]: malformed overlay entry dropped; fix: provide a string id")
                continue
            if ("names" in item and (not isinstance(item["names"], list) or
                                      any(not isinstance(name, str) for name in item["names"]))) or (
                "efforts" in item and (not isinstance(item["efforts"], list) or
                                       any(effort not in EFFORT_ORDER for effort in item["efforts"]))
            ):
                drift.append(f"{path}[{index}]: malformed overlay entry dropped; fix: use string names and supported efforts")
                continue
            name = item["id"]
            merged[name] = _merge_catalog(merged[name], item, f"{path}.{name}", drift) if name in merged else item
        return list(merged.values())
    if isinstance(base, list) and re.fullmatch(r"routes\.[^.]+", path):
        # An overlay pool reorders and reweights by model; unlisted entries keep their place after it.
        if not isinstance(overlay, list):
            drift.append(f"{path}: malformed overlay entry dropped; fix: use a list of pool entries")
            return base
        return _pools.merge_route(base, overlay, path, drift, EFFORT_ORDER)
    if isinstance(base, list) and path.endswith(".names"):
        if not isinstance(overlay, list) or any(not isinstance(value, str) or not value for value in overlay):
            drift.append(f"{path}: malformed overlay entry dropped; fix: use string names")
            return base
        return list(dict.fromkeys([*base, *overlay]))
    if base is None:
        if path.endswith((".fixed_model_family", ".default_model")) and overlay is not None and not isinstance(overlay, str):
            drift.append(f"{path}: malformed overlay entry dropped; fix: use a string or null")
            return base
        return overlay
    if (isinstance(base, (int, float)) and not isinstance(base, bool)
            and isinstance(overlay, (int, float)) and not isinstance(overlay, bool)):
        return overlay
    if type(base) is not type(overlay):
        drift.append(f"{path}: malformed overlay entry dropped; fix: use {type(base).__name__}")
        return base
    return overlay


FREE_TIER_WARNING = "Free tier: prompts may be used for training. Do not send sensitive, private or client data."


def is_free_model(adapter: dict[str, Any], model: str) -> bool:
    """A model id matching the adapter's `free_pattern`: free. Training risk is the model's own
    `trains_on_prompts` flag, which the pattern defaults to true (`_apply_free_pattern`)."""
    pattern = adapter.get("free_pattern") if isinstance(adapter, dict) else None
    return isinstance(pattern, str) and re.search(pattern, model) is not None


def looks_free(adapter: dict[str, Any], model: str) -> bool:
    """Free by any sign the dispatch gate also uses: the adapter's pattern, a `-free` or `:free` id,
    or an OpenCode Zen path. An unregistered id that looks free has unknown data terms."""
    return (is_free_model(adapter, model) or model.endswith(("-free", ":free"))
            or model.startswith("opencode/"))


model_route_live.looks_free = looks_free


def _apply_free_pattern(adapter: dict[str, Any]) -> None:
    """Free models are listed by id alone; the pattern supplies the cap and a default
    training flag; the warning follows the resolved flag, so an explicit `false` (zero-retention) wins."""
    for entry in adapter.get("models", []) if isinstance(adapter, dict) else []:
        if isinstance(entry, dict) and is_free_model(adapter, entry.get("id", "")):
            entry.setdefault("plan_cap_usd", 0)
            entry.setdefault("trains_on_prompts", True)
            if entry["trains_on_prompts"] is True:
                entry.setdefault("warning", FREE_TIER_WARNING)


_EFFORT_VALUE_KEYS = {"effort", "default_effort", "maximum_effort", "minimum_effort"}


def _reject_max(value: Any, path: str = "", key: str = "", container: str = "") -> None:
    """Refuse a catalogue (usually an old instance overlay) that still offers the retired `max` effort."""
    if isinstance(value, dict):
        for name, item in value.items():
            _reject_max(item, f"{path}.{name}" if path else name, name, key)
        return
    is_max = lambda item: isinstance(item, str) and item.strip().lower() == "max"  # noqa: E731
    if isinstance(value, list):
        if key in {*_EFFORT_LIST_KEYS, "effort"} or container == "per_model_efforts":
            if any(is_max(item) for item in value):
                raise _pools.RenamedNameError(f"catalogue {path}: effort 'max' was retired; fix: use xhigh")
            return
        for index, item in enumerate(value):
            _reject_max(item, f"{path}[{index}]", key, container)
        return
    if is_max(value) and (key in _EFFORT_VALUE_KEYS or "role_effort_defaults" in path.split(".")):
        raise _pools.RenamedNameError(f"catalogue {path}: effort 'max' was retired; fix: use xhigh")


def catalogue_snapshot(path: Path | None = None) -> dict[str, Any]:
    product = path or PRODUCT_CATALOG_PATH
    base = json.loads(product.read_text())
    _pools.reject_renamed(base, "catalogue")
    drift: list[str] = []
    sources = [str(product)]
    stale_routing = False
    if path is None and CATALOG_PATH != product and CATALOG_PATH.exists():
        sources.append(str(CATALOG_PATH))
        try:
            overlay = json.loads(CATALOG_PATH.read_text())
            if isinstance(overlay, dict):
                current = base
                installed_date, current_date = overlay.get("catalog_date"), current.get("catalog_date")
                stale_routing = (
                    isinstance(installed_date, str) and isinstance(current_date, str)
                    and installed_date < current_date
                )
                # Date-less instance files are supported as small preference
                # overlays (often only a model alias or nullable override).
                # Field absence is evidence of a stale full snapshot only
                # when the file identifies itself as a catalog snapshot.
                if isinstance(installed_date, str):
                    for adapter, product_entry in current.get("adapters", {}).items():
                        installed_entry = overlay.get("adapters", {}).get(adapter, {})
                        stale_routing = stale_routing or (
                            "trains_on_prompts" in product_entry and "trains_on_prompts" not in installed_entry
                        )
                        old_models = {model.get("id"): model for model in installed_entry.get("models", [])
                                      if isinstance(model, dict)}
                        for model in product_entry.get("models", []):
                            if not isinstance(model, dict):
                                continue
                            old_model = old_models.get(model.get("id"), {})
                            stale_routing = stale_routing or (
                                "trains_on_prompts" in model and "trains_on_prompts" not in old_model
                            )
                if stale_routing:
                    drift.append("instance catalogue is older than product routing; ignored until refreshed")
                else:
                    base = _merge_catalog(base, _pools.reject_renamed(overlay), "", drift)
            else:
                drift.append("instance catalogue: malformed overlay dropped; fix: use a JSON object")
        except _pools.RenamedNameError:
            raise
        except (OSError, ValueError):
            drift.append("instance catalogue: unreadable overlay dropped; fix: refresh routing")
    _reject_max(base)
    for entry in base.get("adapters", {}).values():
        _apply_free_pattern(entry)
    models = [dict(model, adapter=adapter) for adapter, entry in base.get("adapters", {}).items()
              for model in entry.get("models", []) if isinstance(model, dict)]
    shorthands: dict[str, list[str]] = {}
    for model in models:
        for name in model.get("names", []):
            shorthands.setdefault(name.casefold(), []).append(f"{model['adapter']}/{model['id']}")
    digest = hashlib.sha256(json.dumps(base, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    stale_alias_warnings: list[str] = []
    try:
        capabilities = json.loads((model_route_probe._state_root() / "capabilities.json").read_text())
    except (OSError, ValueError):
        capabilities = {}
    if isinstance(capabilities, dict) and isinstance(base.get("adapters"), dict):
        for adapter, entry in base.get("adapters", {}).items():
            probed = capabilities.get(adapter, {})
            listed = probed.get("models", []) if isinstance(probed, dict) else []
            if not isinstance(listed, list):
                continue
            try:
                observed = datetime.fromisoformat(str(probed["observed_at"]).replace("Z", "+00:00"))
                if (datetime.now(timezone.utc) - observed).total_seconds() > 86400:
                    continue
            except (KeyError, ValueError, TypeError):
                continue
            aliases = entry.get("aliases", {}) if isinstance(entry, dict) else {}
            if adapter == "codex":
                families = base.get("families")
                openai = families.get("openai") if isinstance(families, dict) else None
                aliases = openai.get("aliases", {}) if isinstance(openai, dict) else {}
            if not isinstance(aliases, dict):
                continue
            for tier, candidates in aliases.items():
                if not isinstance(candidates, list) or not candidates or not isinstance(candidates[0], str):
                    continue
                current = candidates[0]
                match = re.fullmatch(r"gpt-(\d+(?:\.\d+)?)-(.+)", current)
                if not match:
                    continue
                version = float(match.group(1))
                for observed in listed:
                    newer = re.fullmatch(r"gpt-(\d+(?:\.\d+)?)-" + re.escape(match.group(2)), str(observed))
                    if newer and float(newer.group(1)) > version:
                        stale_alias_warnings.append(f"{adapter} {tier} resolves to {current}; newer {observed} observed; fix: refresh routing")
                        break
    return {"schema": "fabric.catalogue.v1", "sha256": digest, "sources": sources,
            "drift": drift, "adapters": base.get("adapters", {}), "models": models,
            "shorthands": shorthands, "families": base.get("families", {}),
            "endpoints": base.get("endpoints", {}),
            "task_class_routes": base.get("task_class_routes", {}), "catalogue": base,
            "stale_alias_warnings": stale_alias_warnings, "stale_routing": stale_routing}


def load_catalog(path: Path | None = None) -> dict[str, Any]:
    return catalogue_snapshot(path)["catalogue"]


def registered_model_ids(adapter: dict[str, Any]) -> list[str]:
    """Return adapter model ids first, then alias-only ids, in catalogue order."""
    ids: list[str] = []
    models = adapter.get("models", []) if isinstance(adapter, dict) else []
    entries = models.values() if isinstance(models, dict) else models if isinstance(models, list) else []
    for entry in entries:
        model = entry.get("id") if isinstance(entry, dict) else entry
        if isinstance(model, str) and model not in ids:
            ids.append(model)
    aliases = adapter.get("aliases", {}) if isinstance(adapter, dict) else {}
    if isinstance(aliases, dict):
        for candidates in aliases.values():
            if isinstance(candidates, list):
                for model in candidates:
                    if isinstance(model, str) and model not in ids:
                        ids.append(model)
    return ids


_EFFORT_LIST_KEYS = {"efforts", "effort_fallback_order", "supported_efforts"}


def _without_max(value: Any) -> Any:
    """Drop `max` from provider-reported effort lists: no model is routed at it."""
    if isinstance(value, dict):
        return {key: ([item for item in item_list if not (isinstance(item, str) and item.lower() == "max")]
                      if key in _EFFORT_LIST_KEYS and isinstance(item_list, list) else _without_max(item_list))
                for key, item_list in value.items()}
    if isinstance(value, list):
        return [_without_max(item) for item in value]
    return value


def _max_retired(subject: str = "effort") -> dict[str, Any]:
    return {"schema_version": 1, "status": "invalid_effort",
            "message": f"{subject} 'max' was retired; fix: use xhigh"}


def resolve_ordinary(args: argparse.Namespace, catalog: dict[str, Any]) -> int:
    requested = args.model or (args.alias if args.alias not in ALIAS_ORDER else "")
    adapter_name = args.adapter or (model_route_live._owner_adapter(requested, catalog) if requested else "codex")
    adapter = catalog.get("adapters", {}).get(adapter_name)
    if not isinstance(adapter, dict):
        return emit({"schema_version": 1, "status": "unknown_adapter", "adapter": adapter_name,
                     "message": f"Unknown adapter {adapter_name}; fix: choose a listed adapter"}, 2)
    compatibility, status = load_adapter_compatibility(adapter_name, Path(args.adapter_compatibility))
    if status:
        return emit({"schema_version": 1, "status": status, "adapter": adapter_name,
                     "message": "Adapter compatibility unavailable; fix: refresh product configuration"}, 2)
    if compatibility and not compatibility["enabled"]:
        return emit({"schema_version": 1, "status": "adapter_disabled", "adapter": adapter_name,
                     "message": f"{compatibility['disabled_reason']} fix: choose an enabled adapter",
                     "reason": compatibility["disabled_reason"],
                     "adapter_enabled": False, "compatibility_adapter": compatibility["compatibility_adapter"]}, 1)
    notes: list[str] = []
    warnings: list[str] = []
    explicit = bool(args.model) or bool(args.alias and args.alias not in ALIAS_ORDER)
    if args.model and args.alias and args.alias != args.model and getattr(args, "alias_supplied", True):
        notes.append("alias and model both supplied; model won")
    if not requested:
        candidates = adapter.get("aliases", {}).get(args.alias or "senior", [])
        if not candidates:
            fixed = adapter.get("fixed_model_family")
            candidates = catalog.get("families", {}).get(fixed, {}).get("aliases", {}).get(args.alias or "senior", [])
        requested = candidates[0] if candidates else adapter.get("default_model", "")
        if not candidates and requested and args.alias in ALIAS_ORDER:
            warnings.append(f"{adapter_name} has no {args.alias} tier; using adapter default {requested}")
    if not requested:
        return emit({"schema_version": 1, "status": "model_required_for_broker", "adapter": adapter_name,
                     "message": "No model default; fix: pass --model"}, 2)
    requested_input = requested
    if adapter_name == "opencode" and "#" in requested:
        # `provider/model#variant`: the variant is OpenCode's own effort knob, sent as given.
        requested, _, variant = requested.partition("#")
        if variant.lower() == "max":
            return emit(_max_retired("opencode variant"), 2)
        if variant and not args.effort:
            args.effort = variant
            args.explicit_variant = True
            explicit = True
        elif variant and args.effort == variant:
            args.explicit_variant = True
        elif variant:
            notes.append(f"#{variant} in the model id ignored: explicit effort {args.effort} wins")
    registered, match_notes = model_route_live._registered_match(adapter_name, requested, catalog)
    notes.extend(match_notes)
    live_models: list[str] = []
    live_resolved = False
    if adapter_name == "opencode" and registered is None and requested.casefold() != "auto":
        discovery_deadline = time.monotonic() + model_route_live.OPENCODE_DISCOVERY_BUDGET
        live_models, from_cache = model_route_live._opencode_live_models(deadline=discovery_deadline)
        if not live_models and "/" not in requested:
            notes.append("the live opencode model listing is unavailable, so the name was not checked; "
                         "run `opencode models` and pass a full provider/model id if the model is rejected")
        resolved_name, name_notes, refusal = model_route_live._resolve_opencode_name(requested, live_models)
        if refusal and refusal["status"] == "model_not_found" and from_cache:
            # A cached listing can predate the model; look once without the cache.
            fresh, _ = model_route_live._opencode_live_models(refresh=True, deadline=discovery_deadline)
            if fresh:  # a skipped or failed refresh keeps the cached listing
                live_models = fresh
            resolved_name, name_notes, refusal = model_route_live._resolve_opencode_name(requested, live_models)
        if refusal:
            return emit({"schema_version": 1, "adapter": adapter_name, **refusal}, 2)
        if resolved_name:
            requested = resolved_name
            live_resolved = True
            notes.extend(name_notes)
            registered, match_notes = model_route_live._registered_match(adapter_name, requested, catalog)
            notes.extend(match_notes)
    model = registered["id"] if registered else requested
    aliases = adapter.get("aliases", {})
    if not aliases and adapter.get("fixed_model_family"):
        aliases = catalog.get("families", {}).get(adapter["fixed_model_family"], {}).get("aliases", {})
    tier = args.alias if args.alias in ALIAS_ORDER else next(
        (name for name, candidates in aliases.items() if model in candidates), "senior"
    )
    # `auto` is the provider's own chooser; a provider prefix is not a new model.
    provider_auto = requested.casefold() == "auto"
    if registered is None and not provider_auto and live_resolved:
        pass  # already named by the live listing; no registry warning
    elif registered is None and not provider_auto and adapter_name == "opencode" and requested in live_models:
        notes.append(f"{requested} is not in the Fabric catalogue but is in the live opencode listing; run as given")
    elif registered is None and not provider_auto:
        registered_ids = registered_model_ids(adapter)
        registry = ", ".join(registered_ids[:6])
        if len(registered_ids) > 6:
            registry += ", …"
        details = f" (registered: {registry}" if registry else ""
        closest = difflib.get_close_matches(requested, registered_ids, n=1)
        if closest:
            details += ("; " if registry else " (") + f"closest: {closest[0]}"
        if details:
            details += ")"
        notes.append(
            f"{requested} is not in the {adapter_name} registry{details}; passed through as given"
        )
    elif (registered is not None and explicit and model.casefold() != requested.casefold() and not match_notes
          and not model.casefold().endswith("/" + requested.casefold())
          and not any(requested.casefold() == (model + suffix).casefold()
                      for suffix in registered.get("suffix", {}).values())):
        notes.append(f"{requested} routed to {model}")
    cooldowns = model_route_live._cooldowns(catalog)
    until = model_route_live._cooling(adapter_name, model, cooldowns, catalog)
    if until and explicit:
        warnings.append(f"{model} is cooling until {until}; explicit route continued")
    elif until:
        alternatives = aliases.get(tier, [])
        for alternative in alternatives:
            if (model_route_live._canonical_model(adapter_name, alternative, catalog) != model_route_live._canonical_model(adapter_name, model, catalog)
                    and not model_route_live._cooling(adapter_name, alternative, cooldowns, catalog)):
                notes.append(f"{model} cooling until {until}; used {alternative}")
                model = alternative
                registered, _ = model_route_live._registered_match(adapter_name, model, catalog)
                break
        else:
            notes.append(f"{model} cooling until {until}; no available alternative in {tier} alias")
    requested_variant = next((level for level, suffix in (registered or {}).get("suffix", {}).items()
                              if requested.casefold() == (registered["id"] + suffix).casefold()), "")
    if args.effort and requested_variant and args.effort != requested_variant:
        notes.append(f"effort {args.effort} overrode {requested_variant} in model id")
    requested_effort = args.effort or requested_variant or (registered or {}).get("default_effort", "")
    if requested_effort in ("minimal", "none"):
        requested_effort = "low"
    supported = (registered or {}).get("efforts", [])
    effort = "default"
    unverified_effort = False
    if requested_effort and supported:
        rank = EFFORT_ORDER.get(requested_effort, EFFORT_ORDER["medium"])
        effort = max((value for value in supported if EFFORT_ORDER[value] <= rank),
                     key=lambda value: EFFORT_ORDER[value], default=min(supported, key=lambda value: EFFORT_ORDER[value]))
        if effort != requested_effort:
            notes.append(f"{requested_effort} unsupported by {model}; ran at {effort}")
    elif requested_effort and registered is None and adapter_name in {"agy", "claude", "codex"}:
        effort = requested_effort if requested_effort in EFFORT_ORDER else "medium"
        unverified_effort = True
        notes.append(f"{effort} effort passed through to {model}; provider support unverified")
    elif requested_effort and adapter_name == "opencode" and (registered is None or getattr(args, "explicit_variant", False)):
        # OpenCode variants are model-specific and not listed by the CLI: send the one asked for;
        # OpenCode refuses an unknown variant with a typed error.
        effort = requested_effort
        unverified_effort = True
        notes.append(f"{effort} sent as variant #{effort} to {model}; OpenCode rejects variants the model lacks")
    elif requested_effort:
        # Nothing reaches the provider, so nothing is claimed: applied stays empty.
        notes.append(model_route_live._effort_ignored(requested_effort, model)
                     + ("; to force a variant pass model#variant" if adapter_name == "opencode" else ""))
    if registered and registered.get("effort_transport") == "model-suffix" and effort != "default":
        model += registered.get("suffix", {}).get(effort, "")
    training_model = model_route_live.training_flag(adapter, registered)
    warning = (registered or {}).get("warning")
    if warning:
        warnings.append(warning)
    if is_free_model(adapter, model) and not warning and training_model is not False:
        warnings.append(FREE_TIER_WARNING)
    cap = (registered or {}).get("plan_cap_usd")
    if explicit and cap == 15:
        notes.append(f"{model} has a $15 plan cap")
    family, family_source = attribute_model_family(model, catalog)
    if not family:
        family, family_source = "generic-open", "unknown-passed-through"
        if model.casefold() != "auto":
            notes.append(f"{model} family unknown; treated as generic-open")
    fixed = adapter.get("fixed_model_family")
    if fixed and family != fixed:
        owner = model_route_live._owner_adapter(model, catalog)
        warnings.append(f"{model} is {family} on the {fixed} {adapter_name} adapter; fix: use adapter {owner}")
    provider = model.split("/", 1)[0] if "/" in model else adapter.get("endpoint_provider", adapter_name)
    fallback = model_route_live._fallback_candidates(adapter_name, tier, model, catalog, cooldowns,
                                    args.fallback, args.fallback_route, explicit)
    return emit({"schema_version": 1, "status": "ok", "adapter": adapter_name,
                 "alias": "" if args.model and not getattr(args, "alias_supplied", True) else args.alias or "",
                 "role": args.role, "requested_model": requested if explicit else "",
                 "requested_input": requested_input if explicit else "",
                 "resolved_model": model, "model_family": family, "family_source": family_source,
                 "identity_source": "registry" if registered else "passed-through",
                 "endpoint_provider": adapter.get("endpoint_provider", adapter_name), "provider": provider,
                 "adapter_enabled": True, "compatibility_adapter": compatibility["compatibility_adapter"] if compatibility else "",
                 "model_selection": "alias" if not explicit else "explicit",
                 "requested_effort": args.effort or "",
                 "effort": effort if effort != "default" else "",
                 "effort_applied": effort if effort != "default" else "",
                 "supported_efforts": list(supported),
                 "effort_note": next((note for note in notes if "effort" in note or "unsupported" in note), ""),
                 "effort_source": "explicit" if args.effort else "model-default" if (registered or {}).get("default_effort") else "adapter-default",
                 "effort_capability_source": "registry" if supported else "provider-unverified" if unverified_effort
                 else "registry-no-effort-control" if registered else "adapter-no-effort-control",
                 "effort_substitution": next(
                     (note for note in notes if "unsupported" in note or "effort control" in note), ""),
                 "substitution": "", "fallback_model": "",
                 "notes": notes, "warnings": warnings, "fallback_candidates": fallback,
                 "plan_cap_usd": cap, "trains_on_prompts": training_model,
                 "catalog_date": catalog.get("catalog_date", ""),
                 **model_route_probe._kiro_probe_metadata(adapter_name)}, 0)


def load_adapter_compatibility(
    adapter: str, path: Path | None = None,
) -> tuple[dict[str, Any] | None, str]:
    import yaml
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
        # Fail closed on omission: only an explicit `false` permits dispatch
        # without a caller-supplied model, via an account or catalogue default.
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


def resolve_endpoint_profile(
    name: str, adapter: str, catalog: dict[str, Any]
) -> tuple[dict[str, Any], str]:
    """Resolve a named third-party endpoint into route fields.

    The profile carries the base URL and the name of the environment variable
    holding the token. The token itself is never read into the route: the
    dispatcher reads the named variable when it builds the provider command, so
    no credential reaches a record, a run file or the catalogue.

    ``wire_api`` is optional and names the request format the endpoint speaks.
    An Anthropic-compatible endpoint omits it; an OpenAI-compatible one declares
    it, because the Codex CLI needs the wire format stated in the provider
    configuration it is handed.
    """
    endpoints = catalog.get("endpoints")
    profile = endpoints.get(name) if isinstance(endpoints, dict) else None
    if not isinstance(profile, dict):
        return {}, "unknown_endpoint"
    base_url = profile.get("base_url")
    token_env = profile.get("token_env")
    family = profile.get("model_family")
    adapters = profile.get("adapters")
    wire_api = profile.get("wire_api")
    if (
        not isinstance(base_url, str)
        or not base_url.startswith("https://")
        or not isinstance(token_env, str)
        or not token_env.strip()
        or not isinstance(family, str)
        or not family.strip()
        or not isinstance(adapters, list)
        or not all(isinstance(item, str) and item.strip() for item in adapters)
        or (wire_api is not None and wire_api not in ENDPOINT_WIRE_APIS)
    ):
        return {}, "endpoint_config_invalid"
    if adapter not in adapters:
        return {}, "endpoint_adapter_unsupported"
    if not os.environ.get(token_env, "").strip():
        return {}, "endpoint_token_missing"
    resolved = {
        "endpoint_profile": name,
        "endpoint_base_url": base_url,
        "endpoint_token_env": token_env,
        "model_family": family,
    }
    if wire_api is not None:
        resolved["endpoint_wire_api"] = wire_api
    return resolved, ""


def emit(record: dict[str, Any], code: int) -> int:
    if record.get("status") == "ok" and ROUTE_WARNINGS:
        record["warnings"] = list(dict.fromkeys([*(record.get("warnings") or []), *ROUTE_WARNINGS]))
    warnings = record.get("warnings", [])
    newer = [note for note in warnings if " is newer than the catalogue;" in note]
    if newer:
        record["notes"] = list(dict.fromkeys([*record.get("notes", []), *newer]))
        record["warnings"] = [note for note in warnings if note not in newer or _daily_newer_warning(note)]
    print(json.dumps(record, sort_keys=True))
    return code


def _daily_newer_warning(note: str) -> bool:
    """Demote repeat catalogue notices to receipt notes; state is best effort."""
    try:
        path = model_route_probe._state_root() / "newer-model-warnings.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a+") as stream:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            stream.seek(0)
            try:
                seen = json.load(stream)
            except ValueError:
                seen = {}
            today = datetime.now(timezone.utc).date().isoformat()
            seen = {key: day for key, day in seen.items() if day == today} if isinstance(seen, dict) else {}
            model = note.split(" is newer than the catalogue;", 1)[0]
            if model in seen:
                return False
            seen[model] = today
            stream.seek(0)
            stream.truncate()
            json.dump(seen, stream)
            return True
    except (OSError, ValueError, TypeError):
        return False






def resolve_effort(
    args: argparse.Namespace,
    family: str,
    model: str,
    family_config: dict[str, Any],
    requested_effort: str,
    account_default: bool,
    registered: dict[str, Any] | None = None,
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
    # A model with no effort control is sent none: warn, don't block.
    if (args.effort_transport != "none" and model_route_live._no_effort_control(registered)
            and not getattr(args, "explicit_variant", False)):
        return "", model_route_live._effort_ignored(requested_effort, model), "", "registry-no-effort-control"

    ultra_eligible = (
        openai_codex
        and args.alias == "principal"
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

    # A registered model whose id carries the effort (cursor grok-4.7-high) is not
    # an adapter-wide model-id route: its catalogue lists the efforts it takes.
    if (args.effort_transport == "model-id" and registered
            and registered.get("effort_transport") == "model-suffix" and requested_effort):
        carried = next((level for level, suffix in registered.get("suffix", {}).items()
                        if model.casefold() == (registered["id"] + suffix).casefold()), None)
        if carried and not args.effort:
            requested_effort = carried  # the id names its effort; only an explicit effort overrides it
        offered = [value.lower() for value in registered.get("efforts", []) if value.lower() in EFFORT_ORDER]
        if requested_effort in offered:
            return requested_effort, "", "", "registry-model-suffix"
        if offered:
            rank = EFFORT_ORDER.get(requested_effort, EFFORT_ORDER["medium"])
            used = max((value for value in offered if EFFORT_ORDER[value] <= rank),
                       key=lambda value: EFFORT_ORDER[value], default=min(offered, key=lambda value: EFFORT_ORDER[value]))
            return used, f"{requested_effort} unsupported by {model}; ran at {used}", "", "registry-model-suffix"

    if args.effort_transport == "model-id":
        normalized_model = re.sub(r"(?:^|[-_])extra[-_]high(?=$|[-_])", "-xhigh", model.lower())
        matches = re.findall(r"(?:^|[-_])(low|medium|high|xhigh|ultra)(?=$|[-_])", normalized_model)
        parenthetical = re.search(r"\((low|medium|high|xhigh|ultra)\)\s*$", normalized_model)
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
            return "", model_route_live._effort_ignored(requested_effort, model), "", "adapter-no-effort-control"
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
        # No runtime probe: the registry catalogue still bounds what the model takes.
        listed = {value.lower() for value in (registered or {}).get("efforts", []) if value.lower() in EFFORT_ORDER}
        if not listed or requested_effort in listed:
            return requested_effort, "", "", "provider-unverified"
        rank = EFFORT_ORDER.get(requested_effort, EFFORT_ORDER["medium"])
        used = max((value for value in listed if EFFORT_ORDER[value] <= rank),
                   key=lambda value: EFFORT_ORDER[value], default=min(listed, key=lambda value: EFFORT_ORDER[value]))
        return used, f"{requested_effort} unsupported by {model}; ran at {used}", "", "registry"

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
        and EFFORT_ORDER[fallback] < EFFORT_ORDER[getattr(args, "task_class_effort_floor", "") or args.task_class_effort]
    ):
        return None, "", "task_class_effort_below_floor", capability_source
    substitution = f"{requested_effort} unavailable (runtime/model capability); used {fallback}"
    return fallback, substitution, "", capability_source


def resolve(args: argparse.Namespace, catalog: dict[str, Any]) -> int:
    capability_models, capability_error = model_route_probe.load_capabilities(
        args.capabilities_file, args.adapter, catalog
    )
    capability_models = _without_max(capability_models)
    args.capability_models = capability_models
    adapter = catalog["adapters"].get(args.adapter)
    endpoint_profile = getattr(args, "endpoint_profile", {})
    fixed_family = adapter.get("fixed_model_family") if adapter else None
    # An endpoint profile repoints the adapter's CLI at a provider's
    # Anthropic-compatible base URL, so the family it may run is the endpoint's,
    # not the adapter's default. Every other gate stays in force.
    if endpoint_profile:
        fixed_family = endpoint_profile["model_family"]
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
        requested_effort, effort_source = task_class_effort, "task-class"
    else:
        requested_effort = args.effort or model_override_effort or role_effort or {
            "principal": "high", "senior": "medium", "junior": "low"
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
        "alias": "" if args.model and not getattr(args, "alias_supplied", True) else args.alias,
        "role": args.role,
        "requested_effort": getattr(args, "raw_effort", requested_effort),
        "effort": requested_effort,
        "effort_source": effort_source,
        "lead_family": args.lead_family,
    }
    if args.task_class:
        base.update({"task_class": args.task_class, "route_source": "task-class",
                     "configured_models": getattr(args, "task_class_configured_models", [])})
        if getattr(args, "task_class_effort_shift", 0):
            base["effort_shift"] = args.task_class_effort_shift
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
        # Product policy outranks the instance catalogue: an adapter absent
        # from the catalogue but known to product policy (pi maps to pi-rpc,
        # which is absent from the catalogue, not stubbed there) reports its
        # policy state instead of unknown_adapter. Anything policy-unknown is
        # still unknown_adapter.
        if args.adapter in COMPATIBILITY_ADAPTER_IDS:
            compatibility, compatibility_status = load_adapter_compatibility(
                args.adapter, Path(args.adapter_compatibility),
            )
            if compatibility_status:
                return emit({**base, "status": compatibility_status, "endpoint_provider": ""}, 2)
            assert compatibility is not None
            if not compatibility["enabled"]:
                return emit({**base, "status": "adapter_disabled",
                             "reason": compatibility["disabled_reason"],
                             "message": f"{compatibility['disabled_reason']} fix: choose an enabled adapter",
                             "endpoint_provider": "",
                             "compatibility_adapter": compatibility["compatibility_adapter"],
                             "adapter_enabled": False}, 1)
        return emit({**base, "status": "unknown_adapter"}, 2)
    # A third-party endpoint exposes no reasoning-effort control on the Anthropic
    # wire format, so an endpoint route carries no effort rather than a claimed one.
    args.effort_transport = (
        "none" if endpoint_profile else adapter.get("effort_transport", "none")
    )
    # account-default adapters dispatch on the provider account's default
    # model: the runtime rejects explicit model ids, so the resolver keeps the
    # catalog id for effort/audit lookups but emits an empty dispatch model.
    account_default = adapter.get("model_selection") == "account-default"

    def emit_route(record: dict[str, Any], code: int) -> int:
        """Emit, never exposing a catalog id as a dispatchable model (#190)."""
        if record.get("status") in {
            "account_default_conflicts_with_compatibility", "adapter_family_forbidden",
            "adapter_model_forbidden", "adapter_default_model_invalid", "alias_unavailable",
            "adapter_compatibility_invalid", "adapter_compatibility_unavailable",
        } and not record.get("message"):
            record["message"] = (
                f"{record['status']}; fix: refresh instance routing and check "
                "config/adapter-compatibility.yaml"
            )
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
                    "message": f"{compatibility['disabled_reason']} fix: choose an enabled adapter",
                    "endpoint_provider": endpoint,
                    **compatibility_metadata,
                },
                1,
            )
        has_default_model = isinstance(adapter.get("default_model"), str) and bool(adapter["default_model"])
        permits_implicit_model = not compatibility["requires_explicit_model"]
        if (account_default or (has_default_model and permits_implicit_model)) != permits_implicit_model:
            # A catalogue default may satisfy an adapter's implicit-model
            # policy; retain the existing account-default drift check.
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
    family_source = ""
    candidates: list[str] = []

    adapter_default = adapter.get("default_model") if not args.model else None
    if adapter_default is not None and (not isinstance(adapter_default, str) or not adapter_default.strip()):
        return emit_route({**base, "status": "adapter_default_model_invalid"}, 2)
    selected_model = args.model or adapter_default
    if selected_model:
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
        model = selected_model
        # Brokers and gateways: attribute upstream family from the slug.
        # Endpoint profile family is only a fallback when inference fails.
        endpoint_family = endpoint_profile.get("model_family") if endpoint_profile else None
        if args.adapter == "cursor" and model == "auto":
            family, family_source = "generic-open", "broker-default"
        else:
            family, family_source = attribute_model_family(
                model, catalog, endpoint_family=endpoint_family,
            )
        identity_source = (
            "endpoint-profile" if family_source.startswith("endpoint-profile") else "model-pattern"
        )
        if not family:
            return emit_route(
                {
                    **base,
                    "status": "model_family_unknown",
                    "endpoint_provider": endpoint,
                    "resolved_model": model,
                    "family_source": family_source,
                },
                1,
            )
        # Pinned single-family adapters (claude/codex without a gateway) still
        # reject cross-family slugs. Brokers and endpoint routes do not.
        if fixed_family and not endpoint_profile and family != fixed_family:
            return emit_route(
                {
                    **base,
                    "status": "adapter_family_mismatch",
                    "endpoint_provider": endpoint,
                    "model_family": family,
                    "resolved_model": model,
                    "family_source": family_source,
                },
                1,
            )
        selected_override_model = (
            args.model_override.get("models", [""])[0] if args.model_override else ""
        )
        model_matches_override = (
            model.casefold() == selected_override_model.casefold()
            if selected_override_model.casefold().startswith("claude-")
            else model_has_alias(model, selected_override_model)
        )
        if args.model_override and not model_matches_override:
            return emit_route({**base, "status": "risk_tier_model_mismatch"}, 1)
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
            if args.adapter != "agy" or not capability_models:
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
                task_class_models = getattr(args, "task_class_models", None)
                if task_class_models is not None:
                    preferred_candidates = [
                        candidate for candidate in task_class_models
                        if infer_family(candidate, catalog) == preferred_family
                    ]
                else:
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
                        "effort": "",
                        "endpoint_provider": endpoint,
                        "candidates": candidates,
                    },
                    1,
                )
            model, identity_source = available[chosen.lower()]
            family_source = "runtime-capability"
            if chosen != candidates[0]:
                substitution = f"{candidates[0]} unavailable; used {chosen}"
        else:
            family = fixed_family
            family_source = "catalog-family"
            task_class_models = getattr(args, "task_class_models", None)
            if task_class_models is not None:
                candidates = task_class_models
            else:
                candidates = args.model_override.get("models")
                candidates = candidates or family_config.get("role_overrides", {}).get(args.role, {}).get(args.alias)
                candidates = candidates or family_config["aliases"].get(args.alias)
        if not candidates:
            if getattr(args, "task_class_models", None) is not None:
                return emit_route(
                    {**base, "status": "no_candidate_available", "effort": "",
                     "model_family": family, "candidates": []},
                    1,
                )
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
                            "effort": "",
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

    route_notes: list[str] = []
    route_warnings: list[str] = []
    if args.model and args.alias and getattr(args, "alias_supplied", True):
        route_notes.append("alias and model both supplied; model won")
    cooldowns = model_route_live._cooldowns(catalog)
    cooling_until = model_route_live._cooling(args.adapter, model, cooldowns, catalog)
    if cooling_until and args.model:
        route_warnings.append(f"{model} is cooling until {cooling_until}; explicit route continued")
    elif cooling_until and not args.model:
        for candidate in candidates[1:] if isinstance(candidates, list) else []:
            if (not model_route_live._cooling(args.adapter, candidate, cooldowns, catalog)
                    and (not capability_models or candidate.casefold() in capability_models)):
                route_notes.append(f"{model} cooling until {cooling_until}; used {candidate}")
                model = candidate
                break
        else:
            route_notes.append(f"{model} cooling until {cooling_until}; no available alternative in {args.alias} alias")
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
        risk_tier_override_reserves_model(model, candidate)
        for candidate in configured_override_models
    )
    if is_risk_override_model and not args.model_override:
        return emit_route({**base, "status": "risk_tier_override_required"}, 1)
    compatibility_family = ""
    assurance_ok = family_is_assurance_eligible(family, family_source)
    lead_ok = family_is_assurance_eligible(args.lead_family or "", "catalog-family")
    distinct = bool(
        args.lead_family
        and assurance_ok
        and lead_ok
        and family != args.lead_family
    )
    if compatibility and args.require_distinct and not args.lead_family:
        return emit_route(
            {
                **base,
                "status": "lead_family_required",
                "endpoint_provider": endpoint,
                "model_family": family,
                "resolved_model": model,
                "identity_source": identity_source,
                "family_source": family_source,
                **compatibility_metadata,
            },
            2,
        )
    if compatibility and args.require_distinct and (not assurance_ok or not lead_ok):
        return emit_route(
            {
                **base,
                "status": "family_not_assurance_eligible",
                "endpoint_provider": endpoint,
                "model_family": family,
                "resolved_model": model,
                "identity_source": identity_source,
                "family_source": family_source,
                "distinct_from_lead": False,
                **compatibility_metadata,
            },
            1,
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
                "family_source": family_source,
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

    registered_model, _ = model_route_live._registered_match(args.adapter, model, catalog)
    probed = capability_models.get(model.casefold(), {})
    supported = probed.get("supported_efforts") or (registered_model or {}).get("efforts", [])
    effort, effort_substitution, effort_status, capability_source = resolve_effort(
        args, family, model, family_config, requested_effort, account_default, registered_model
    )
    if capability_source == "registry-no-effort-control" or (
        capability_source == "adapter-no-effort-control" and args.effort
    ):
        route_notes.append(effort_substitution)
    if effort_status in {"effort_unsupported", "no_effort_available", "capability_discovery_failed"} and not (
        args.task_class or args.model_override_tier or args.require_distinct
    ):
        if supported:
            rank = EFFORT_ORDER.get(requested_effort, EFFORT_ORDER["medium"])
            effort = max((candidate for candidate in supported if EFFORT_ORDER[candidate] <= rank),
                         key=lambda candidate: EFFORT_ORDER[candidate],
                         default=min(supported, key=lambda candidate: EFFORT_ORDER[candidate]))
            effort_substitution = (
                f"{requested_effort} unsupported by {model}; ran at {effort}" if effort != requested_effort else ""
            )
            capability_source = "runtime-model-catalog" if probed.get("supported_efforts") else "registry"
            effort_status = ""
    if not effort_status and getattr(args, "raw_effort", None):
        effort_substitution = f"{args.raw_effort} unknown; ran at {effort or 'default'}"
    # The effort reaches a suffix model only through its id, so the id must carry it.
    suffix_model = args.effort_transport == "model-id" and (registered_model or {}).get("effort_transport") == "model-suffix"
    suffix = (registered_model.get("suffix", {}) if suffix_model else {}).get(effort or "", "")
    if not effort_status and suffix:
        model = registered_model["id"] + suffix  # replaces any suffix the request carried
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
        or (capability_source not in {"runtime-model-catalog", "registry-no-effort-control"}
            and not claude_effort_unverified)
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
        "effort_applied": effort,
        "supported_efforts": list(supported),
        "effort_note": effort_substitution,
        "effort_capability_source": capability_source,
        "status": "ok",
        "endpoint_provider": endpoint,
        "model_family": family,
        "resolved_model": model,
        "identity_source": identity_source,
        "family_source": family_source,
        "substitution": substitution,
        "fallback_model": fallback_model,
        "distinct_from_lead": distinct,
        "notes": route_notes,
        "warnings": route_warnings,
        "fallback_candidates": model_route_live._fallback_candidates(
            args.adapter, args.alias, model, catalog, cooldowns,
            args.fallback, args.fallback_route, bool(args.model) or bool(args.alias and args.alias not in ALIAS_ORDER),
        ),
        "trains_on_prompts": model_route_live.training_flag(adapter, registered_model, model),
    }
    if args.task_class in {"review", "critical-review", "second-opinion"}:
        failed_models = model_route_live._recent_failed_task_class_models(args.adapter, args.task_class)
        alternate = next((candidate for candidate in args.task_class_configured_models
                          if candidate != model and candidate not in failed_models), None)
        record["fallback_candidates"] = (
            [{"adapter": args.adapter, "model": alternate, "effort": effort}]
            if alternate else []
        )
    if adapter_default:
        record["model_selection"] = "adapter-default"
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
    record.update(model_route_probe._kiro_probe_metadata(args.adapter))
    if endpoint_profile:
        record.update(
            {
                key: value
                for key, value in endpoint_profile.items()
                if key != "model_family"
            }
        )
    if args.require_distinct and not args.lead_family:
        return emit_route({**record, "status": "lead_family_required"}, 2)
    if args.require_distinct and (not assurance_ok or not lead_ok):
        return emit_route({**record, "status": "family_not_assurance_eligible"}, 1)
    if args.require_distinct and not distinct:
        return emit_route({**record, "status": "same_family_forbidden"}, 1)
    return emit_route(record, 0)


def _router() -> Any:
    """This module as the pool picker's router, whether run as a script or loaded by path."""
    import types

    return sys.modules.get(__name__) or types.SimpleNamespace(**globals())


def _fabric_records() -> Any:
    # Load code beside this router, even when its catalogue comes from an overlay.
    path = Path(__file__).resolve().parents[1] / "skills" / "orchestrate" / "scripts" / "fabric_records.py"
    spec = importlib.util.spec_from_file_location(
        "provenant_fabric_records", path, submodule_search_locations=[str(path.parent)]
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


model_route_live._fabric_records = _fabric_records


def route_pick(requests: Any, seed: int | None = None) -> dict[str, Any]:
    """Answer the Fabric front door's pool requests in one call."""
    import random

    catalog = load_catalog()
    availability = _pools.Availability(_router(), catalog)
    if not isinstance(requests, list):
        return {"status": "rejected", "error": "invalid_input", "fix": "Pass a requests list."}
    results = _pools.pick_many(
        _router(), catalog, requests, availability=availability,
        state_path=model_route_probe._state_root() / "route-rotation.json", effort_order=EFFORT_ORDER,
        rng=random.Random(seed),
    )
    return {"status": "ok", "results": results}


def main(argv: list[str] | None = None) -> int:
    try:
        import model_route_cli
    except ModuleNotFoundError:
        model_route_cli = _sibling("model_route_cli")
    try:
        return model_route_cli._main(argv, model_route_cli._ModuleGlobals(globals()))
    except _pools.RenamedNameError as exc:
        print(f"model-route: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
