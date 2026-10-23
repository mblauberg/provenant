#!/usr/bin/env python3
"""Live model matching and advisory route state used by model_route."""

from __future__ import annotations

import difflib
import importlib.util
import json
from datetime import datetime, timezone
from pathlib import Path
import re
import time
from typing import Any

model_route_probe: Any = None
_fabric_records: Any = None


def _key(text: str) -> str:
    """Case- and punctuation-blind comparison key: GPT_SOL, gpt-sol and s_o_l agree with gptsol and sol."""
    return re.sub(r"[^a-z0-9]", "", text.casefold())


def _family_parts(model_id: str) -> list[str]:
    parts = [part for part in re.split(r"[-_\s]+", model_id.casefold()) if part and not re.fullmatch(r"v?\d+(?:\.\d+)*", part)]
    return parts or [""]


def _family_key(model_id: str) -> str:
    """The id with its version tokens removed: gpt-6.1-sol -> gptsol, claude-sonnet-5-5 -> claudesonnet."""
    return _key("".join(_family_parts(model_id)))


def _version(model_id: str) -> tuple[int, ...]:
    return tuple(int(part) for part in re.findall(r"\d+", model_id))


def _cached_live_models(adapter: str) -> list[str]:
    """Model ids from the fresh (under a day old) capabilities cache; never probes."""
    try:
        probed = json.loads((model_route_probe._state_root() / "capabilities.json").read_text()).get(adapter, {})
        observed = datetime.fromisoformat(str(probed["observed_at"]).replace("Z", "+00:00"))
        if (datetime.now(timezone.utc) - observed).total_seconds() > 86400:
            return []
        return [item for item in probed.get("models", []) if isinstance(item, str)]
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        return []


_NEWER_NOTE = "{} is newer than the catalogue; run provenant refresh-routing or add it to config/model-routing.json"


def _live_entry(base: dict[str, Any], model: str) -> dict[str, Any]:
    """A live model the catalogue lacks: it inherits its family's newest entry, so every gate and trait still applies."""
    return {**base, "id": model, "names": [], "default": False, "inherits": [base["id"], *base.get("names", [])]}


def _live_base(adapter: str, model: str, catalog: dict[str, Any]) -> str:
    """A live id without the effort suffix the catalogue defines for the adapter (gemini-3.9-flash-high -> gemini-3.9-flash)."""
    suffixes = sorted({value for entry in catalog["adapters"][adapter].get("models", [])
                       if entry.get("effort_transport") == "model-suffix" for value in entry.get("suffix", {}).values()},
                      key=len, reverse=True)
    return next((model[:-len(suffix)] for suffix in suffixes if suffix and model.casefold().endswith(suffix.casefold())), model)


def suffix_effort(adapter: str, requested: str, catalog: dict[str, Any]) -> str:
    """The effort an id's catalogue-defined suffix names (gemini-3.9-flash-low -> low), or ''."""
    tail = requested[len(_live_base(adapter, requested, catalog)):].casefold()
    levels = {level: value.casefold() for entry in catalog["adapters"][adapter].get("models", [])
              if entry.get("effort_transport") == "model-suffix" for level, value in entry.get("suffix", {}).items()}
    return next((level for level, value in levels.items() if tail and value == tail), "")


def _latest_in_family(adapter: str, requested: str, catalog: dict[str, Any]) -> tuple[dict[str, Any] | None, list[str]]:
    """Resolve a version-free name to the newest model of its family, catalogue or fresh live listing."""
    entries = catalog["adapters"][adapter].get("models", [])
    token = _key(requested)
    if not token or re.search(r"\d", requested):
        return None, []
    named = [entry for entry in entries if token in (_key(name) for name in entry.get("names", []))]
    pool = named or [entry for entry in entries if token in (_family_key(entry["id"]), _key(_family_parts(entry["id"])[-1]))]
    keys = {_family_key(entry["id"]) for entry in pool}
    if len(keys) != 1:
        return None, []
    key = keys.pop()
    best = max((entry for entry in entries if _family_key(entry["id"]) == key), key=lambda item: _version(item["id"]))
    live = [base for base in (_live_base(adapter, model, catalog) for model in _cached_live_models(adapter))
            if _family_key(base) == key and _version(base) > _version(best["id"])]
    if not live:
        return best, []
    newest = max(live, key=_version)
    return _live_entry(best, newest), [_NEWER_NOTE.format(newest)]


def _live_id(adapter: str, requested: str, catalog: dict[str, Any]) -> tuple[dict[str, Any] | None, list[str]]:
    """An explicit id the catalogue lacks but the fresh live listing has: it inherits its family's catalogue entry."""
    entries = catalog["adapters"][adapter].get("models", [])
    base = _live_base(adapter, requested, catalog)
    if any(entry["id"].casefold() == base.casefold() for entry in entries):
        return None, []  # Registered already, so not newer than the catalogue.
    live = base if any(_live_base(adapter, model, catalog).casefold() == base.casefold()
                       for model in _cached_live_models(adapter)) else None
    family = [entry for entry in entries if live and _family_key(entry["id"]) == _family_key(live)]
    if not family:
        return None, []
    return _live_entry(max(family, key=lambda item: _version(item["id"])), live), [_NEWER_NOTE.format(live)]


def _registered_match(adapter: str, requested: str, catalog: dict[str, Any]) -> tuple[dict[str, Any] | None, list[str]]:
    entries = catalog["adapters"][adapter].get("models", [])
    token = requested.casefold()
    exact = [entry for entry in entries if entry["id"].casefold() == token]
    named = [entry for entry in entries if token in (name.casefold() for name in entry.get("names", []))]
    variant = [entry for entry in entries if entry.get("effort_transport") == "model-suffix" and
               any(token == (entry["id"] + suffix).casefold() for suffix in entry.get("suffix", {}).values())]
    retired = []
    if not exact and not named and not variant:
        version_match = re.fullmatch(r"gpt-(\d+(?:\.\d+)*)-(.+)", token)
        if adapter == "codex" and version_match:
            requested_version = tuple(int(part) for part in version_match.group(1).split("."))
            for entry in entries:
                registered = re.fullmatch(r"gpt-(\d+(?:\.\d+)*)-(.+)", entry["id"].casefold())
                if (registered and registered.group(2) == version_match.group(2)
                        and requested_version < tuple(int(part) for part in registered.group(1).split("."))):
                    retired.append(entry)
    if not exact and not variant and catalog["adapters"][adapter].get("latest_aliases") is True:
        latest, latest_notes = _latest_in_family(adapter, requested, catalog)
        if latest is not None:
            if len(named) > 1:
                latest_notes.insert(0, f"{requested} is ambiguous; used {latest['id']} "
                                       f"(alternatives: {', '.join(item['id'] for item in named)})")
            return latest, latest_notes
    matches = exact or named or variant or retired
    if not matches:
        return _live_id(adapter, requested, catalog) if catalog["adapters"][adapter].get("latest_aliases") is True else (None, [])
    chosen = next((item for item in matches if item.get("default")), None)
    if chosen is None:
        chosen = max(matches, key=lambda item: tuple(int(part) for part in re.findall(r"\d+", item["id"])))
    notes = []
    if len(matches) > 1:
        notes.append(f"{requested} is ambiguous; used {chosen['id']} (alternatives: {', '.join(item['id'] for item in matches)})")
    if retired:
        notes.append(f"{requested} is retired; routed to {chosen['id']}")
    return chosen, notes


def training_flag(adapter: dict[str, Any], registered: dict[str, Any] | None, model: str = "") -> bool | None:
    """The model's own flag, else the adapter's; an adapter-wide `false` never clears a free model,
    whose terms only its own entry can vouch for."""
    own = (registered or {}).get("trains_on_prompts")
    if type(own) is bool:
        return own
    value = adapter.get("trains_on_prompts")
    if value is False and model and looks_free(adapter, model):
        return None
    return value if type(value) is bool else None


def _no_effort_control(entry: dict[str, Any] | None) -> bool:
    """A registered model without an effort list takes none, whatever its adapter's transport."""
    return isinstance(entry, dict) and (entry.get("effort_transport") == "none" or not entry.get("efforts"))


def _effort_ignored(effort: str, model: str) -> str:
    return f"effort {effort} ignored: {model} has no effort control"


def _owner_adapter(requested: str, catalog: dict[str, Any]) -> str:
    token = requested.casefold()
    if token.startswith(("opencode/", "opencode-go/", "openrouter/")):
        return "opencode"
    for adapter in ("codex", "claude", "agy", "cursor", "opencode"):
        if adapter in catalog["adapters"] and _registered_match(adapter, requested, catalog)[0]:
            return adapter
    if token.startswith(("gpt-", "o1", "o3", "o4")):
        return "codex"
    if token.startswith("claude-"):
        return "claude"
    if token.startswith("gemini-"):
        return "agy"
    if token.startswith(("grok-", "cursor-", "composer-")):
        return "cursor"
    return "opencode"


def _canonical_model(adapter: str, model: str, catalog: dict[str, Any]) -> str:
    if model == "*":
        return model
    if adapter not in catalog.get("adapters", {}):
        return model
    match, _ = _registered_match(adapter, model, catalog)
    return match["id"] if match else model


_QUOTA_PATH = Path(__file__).resolve().parent / "provider_quota.py"
_quota: Any = None


def _provider_quota() -> Any:
    """The provider quota module, loaded once; None when it cannot load, so routing never fails on it."""
    global _quota
    if _quota is None:
        try:
            spec = importlib.util.spec_from_file_location("provenant_provider_quota", _QUOTA_PATH)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            _quota = module
        except Exception:  # noqa: BLE001 - quota is advisory input to routing
            _quota = False
    return _quota or None


def _with_quota(records: dict[str, Any]) -> dict[str, Any]:
    """Cool exhausted providers until reset and drop usage cooldowns a newer quota reading contradicts."""
    quota = _provider_quota()
    try:
        return quota.apply_to_cooldowns(records) if quota else records
    except Exception:  # noqa: BLE001 - quota is advisory input to routing
        return records


def _quota_states() -> dict[str, Any]:
    """Cached pace state per route adapter, for `provenant routes`; never fetches."""
    quota = _provider_quota()
    try:
        states = quota.route_states() if quota else {}
    except Exception:  # noqa: BLE001 - quota is advisory input to routing
        return {}
    return {state["adapter"]: state for state in states.values() if state.get("adapter")}


def _cooldowns(catalog: dict[str, Any]) -> dict[str, Any]:
    records = _fabric_records().read_cooldowns()
    records = _with_quota(records)
    normalised: dict[str, Any] = {}
    for key, record in records.items():
        if not isinstance(key, str) or "/" not in key:
            continue
        adapter, model = key.split("/", 1)
        canonical = f"{adapter}/{_canonical_model(adapter, model, catalog)}"
        if not isinstance(record, dict):
            continue
        previous = normalised.get(canonical)
        if not isinstance(previous, dict) or str(record.get("cooling_until", "")) > str(previous.get("cooling_until", "")):
            normalised[canonical] = record
    return normalised


def _recent_failed_task_class_models(adapter: str, task_class: str) -> set[str]:
    path = model_route_probe._state_root() / "route-health.json"
    try:
        document = json.loads(path.read_text())
        routes = document.get("routes", {}) if document.get("schema") == "fabric.route-health.v1" else {}
        now = datetime.now(timezone.utc)
        failed = set()
        for record in routes.values():
            if not isinstance(record, dict) or record.get("adapter") != adapter or record.get("task_class") != task_class:
                continue
            recent = record.get("recent", [])
            if not isinstance(recent, list) or not recent:
                continue
            latest = recent[0]
            if not isinstance(latest, dict):
                continue
            observed = datetime.fromisoformat(str(latest.get("at", "")).replace("Z", "+00:00"))
            age = (now - observed).total_seconds()
            if (0 <= age <= 7 * 86400
                    and latest.get("status") in {"failed", "empty_output"}):
                model = record.get("model")
                if isinstance(model, str):
                    failed.add(model)
        return failed
    except (OSError, ValueError, TypeError, AttributeError):
        return set()














def _cooling(adapter: str, model: str, cooldowns: dict[str, Any], catalog: dict[str, Any]) -> str:
    canonical = _canonical_model(adapter, model, catalog)
    for key in (f"{adapter}/{canonical}", f"{adapter}/*"):
        item = cooldowns.get(key)
        if not isinstance(item, dict):
            continue
        until = item.get("cooling_until")
        covers = item.get("covers")
        # A plan's quota cooldown covers only the models billed to that plan.
        if covers and not any(canonical.startswith(prefix) for prefix in covers):
            continue
        try:
            if datetime.fromisoformat(str(until).replace("Z", "+00:00")) > datetime.now(timezone.utc):
                return str(until)
        except (ValueError, TypeError):
            pass
    return ""


def _fallback_candidates(
    adapter_name: str, tier: str, model: str, catalog: dict[str, Any],
    cooldowns: dict[str, Any], fallback: str | None = None,
    requested_routes: list[str] | None = None, explicit: bool = False,
) -> list[dict[str, Any]]:
    adapter = catalog["adapters"][adapter_name]
    tiers = adapter.get("aliases", {})
    if not tiers and adapter.get("fixed_model_family"):
        tiers = catalog.get("families", {}).get(adapter["fixed_model_family"], {}).get("aliases", {})
    own = tiers.get(tier, [])
    if fallback == "false" or (explicit and fallback is None and not requested_routes):
        return []
    current = _canonical_model(adapter_name, model, catalog)
    candidates = [(adapter_name, candidate) for candidate in own]
    if adapter_name != "opencode":
        # OpenCode has no principal tier, so a principal route falls back to its senior list.
        opencode_tiers = catalog["adapters"].get("opencode", {}).get("aliases", {})
        candidates += [("opencode", candidate) for candidate in
                       opencode_tiers.get(tier) or opencode_tiers.get("senior", [])]
    if fallback == "any":
        candidates += [("opencode", item["id"]) for item in
                       catalog["adapters"].get("opencode", {}).get("models", [])]
    for route in requested_routes or []:
        candidate_adapter = next((name for name in catalog["adapters"] if route.startswith(name + "/")), adapter_name)
        candidate = route[len(candidate_adapter) + 1:] if route.startswith(candidate_adapter + "/") else route
        candidates.append((candidate_adapter, candidate))
    results = []
    seen = {(adapter_name, current)}
    for candidate_adapter, candidate in candidates:
        canonical = _canonical_model(candidate_adapter, candidate, catalog)
        if (candidate_adapter, canonical) in seen:
            continue
        seen.add((candidate_adapter, canonical))
        entry, _ = _registered_match(candidate_adapter, candidate, catalog)
        opt_in = fallback == "any" or f"{candidate_adapter}/{candidate}" in (requested_routes or []) or candidate in (requested_routes or [])
        flag = training_flag(catalog["adapters"].get(candidate_adapter, {}), entry)
        # Implicit fallbacks are paid or explicitly non-training (zero-retention free tiers).
        if (entry and (opt_in or flag is False or (entry.get("plan_cap_usd", 1) > 0 and flag is not True))
                and not _cooling(candidate_adapter, canonical, cooldowns, catalog)):
            results.append({"adapter": candidate_adapter, "model": canonical,
                            "plan_cap_usd": entry.get("plan_cap_usd"),
                            "trains_on_prompts": flag})
    return results


# Discovery shares one budget that fits inside dispatch preflight's 15 s routing timeout.
OPENCODE_DISCOVERY_BUDGET = 12.0
OPENCODE_REFRESH_MIN_REMAINING = 4.0


def _opencode_live_models(refresh: bool = False, deadline: float | None = None) -> tuple[list[str], bool]:
    """Ids from the cached `opencode models` probe and whether the cache answered.

    An unavailable probe or an empty listing means unknown, so the list is empty (and the cache
    keeps such a record for an hour only).
    """
    if deadline is None:
        deadline = time.monotonic() + OPENCODE_DISCOVERY_BUDGET
    if refresh and deadline - time.monotonic() < OPENCODE_REFRESH_MIN_REMAINING:
        return [], False
    try:
        record, _ = model_route_probe.probe_capabilities("opencode", "opencode", deadline, listing_only=True,
                                       refresh=refresh)
    except Exception:  # noqa: BLE001 - discovery must never break routing
        return [], False
    if not isinstance(record, dict) or record.get("status") == "probe_unavailable":
        return [], False
    models = record.get("models")
    found = [item for item in models if isinstance(item, str)] if isinstance(models, list) else []
    return found, bool(record.get("cache_hit"))


def _resolve_opencode_name(requested: str, live: list[str]) -> tuple[str | None, list[str], dict[str, Any] | None]:
    """Resolve a bare uncatalogued name (`kimi`, `qwen3.8-flash`) against the live listing.

    Returns (model, notes, refusal). An exact name after the provider prefix wins; a unique
    name-prefix match is allowed with a note; several matches or none are typed refusals.
    """
    token = requested.casefold()
    if not live or "/" in requested or "#" in requested or not token:
        return None, [], None
    tails = [(item, item.split("/", 1)[-1].casefold()) for item in live]
    exact = [item for item, tail in tails if tail == token]
    near = exact or [item for item, tail in tails if tail.startswith(token)]
    if len(near) == 1:
        how = "" if exact else " (unique prefix match)"
        return near[0], [f"{requested} resolved to {near[0]} from the live opencode listing{how}"], None
    if near:
        return None, [], {"status": "model_ambiguous", "candidates": near[:12],
                          "message": f"{requested} matches several opencode models: {', '.join(near[:12])}; "
                                     "fix: pass the full provider/model id"}
    closest = difflib.get_close_matches(token, [tail for _, tail in tails], n=3)
    names = [item for item, tail in tails if tail in closest]
    hint = f" Closest: {', '.join(names)}." if names else ""
    return None, [], {"status": "model_not_found", "candidates": names,
                      "message": f"{requested} is not an opencode model.{hint} "
                                 "fix: run `opencode models` and pass a full provider/model id"}
