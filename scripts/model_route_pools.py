"""Pick models from the global weighted route pools (#848).

A route (`strong`, `bulk`, `design`, `writing`) is a weighted pool of
`adapter/model` entries across adapters. This module filters a pool on
availability and picks from it in one of four modes: single (highest weight),
rotate (smooth weighted round-robin per project), council (N picks, families
spread first, weights as odds) and an explicit `models` list. It only chooses;
`model_route.py resolve` still resolves each pick into a concrete route.
"""

from __future__ import annotations

import fcntl
import json
import os
from pathlib import Path
import random
import shutil
from typing import Any, Callable


WEIGHTS = {"high": 8, "normal": 4, "sparing": 1, "off": 0}
DEFAULT_WEIGHT = "normal"
BINARIES = {"cursor": "cursor-agent", "kiro": "kiro-cli"}
PRIVATE_UNSAFE_TRAITS = frozenset({"free", "trains-on-prompts"})
MAX_COUNCIL = 8


class PoolError(ValueError):
    def __init__(self, code: str, fix: str) -> None:
        super().__init__(fix)
        self.code = code
        self.fix = fix


def weight_value(raw: Any) -> float | None:
    """Map the tiny weight vocabulary (or a non-negative number) to a number."""
    if raw is None:
        return float(WEIGHTS[DEFAULT_WEIGHT])
    if isinstance(raw, bool):
        return None
    if isinstance(raw, (int, float)):
        return float(raw) if raw >= 0 else None
    if isinstance(raw, str):
        value = WEIGHTS.get(raw.strip().casefold())
        return None if value is None else float(value)
    return None


def effort_band(raw: Any, effort_order: dict[str, int]) -> list[str] | None:
    """Return a one- or two-level effort band, or None when malformed."""
    band = [raw] if isinstance(raw, str) else raw
    if (not isinstance(band, list) or not 1 <= len(band) <= 2
            or any(not isinstance(item, str) or item not in effort_order for item in band)):
        return None
    return sorted(band, key=lambda item: effort_order[item])


def entry_is_valid(entry: Any, effort_order: dict[str, int]) -> bool:
    return (isinstance(entry, dict) and isinstance(entry.get("model"), str)
            and "/" in entry["model"].strip("/")
            and weight_value(entry.get("weight")) is not None
            and ("effort" not in entry or effort_band(entry["effort"], effort_order) is not None))


def merge_route(base: list[Any], overlay: list[Any], path: str, drift: list[str],
                effort_order: dict[str, int]) -> list[Any]:
    """Overlay a pool: listed entries come first in overlay order, fields merge by model."""
    by_model = {entry["model"]: entry for entry in base if isinstance(entry, dict) and "model" in entry}
    merged: list[dict[str, Any]] = []
    for index, item in enumerate(overlay):
        candidate = {**by_model.get(item.get("model"), {}), **item} if isinstance(item, dict) else item
        if not entry_is_valid(candidate, effort_order):
            drift.append(f"{path}[{index}]: malformed overlay entry dropped; fix: use model adapter/id, "
                         "weight high|normal|sparing|off or a number, optional effort band")
            continue
        merged = [entry for entry in merged if entry["model"] != candidate["model"]] + [candidate]
    listed = {entry["model"] for entry in merged}
    return merged + [entry for entry in base if isinstance(entry, dict) and entry.get("model") not in listed]


def route_name(catalog: dict[str, Any], name: str) -> str:
    routes = catalog.get("routes", {})
    key = name.strip().casefold()
    if key in routes:
        return key
    synonym = catalog.get("route_synonyms", {}).get(key)
    if isinstance(synonym, str) and synonym in routes:
        return synonym
    raise PoolError("route_invalid", "Pass route " + ", ".join(sorted(routes)) + ".")


def split_model(router: Any, catalog: dict[str, Any], text: str) -> tuple[str, str]:
    """Split `adapter/model` (or a bare model id) into its adapter and canonical id."""
    adapters = catalog.get("adapters", {})
    head, separator, rest = text.strip().partition("/")
    if separator and head in adapters:
        for candidate in (rest, text.strip()):
            match, _ = router._registered_match(head, candidate, catalog)
            if match:
                return head, match["id"]
        return head, rest
    adapter = router._owner_adapter(text.strip(), catalog)
    match = router._registered_match(adapter, text.strip(), catalog)[0] if adapter in adapters else None
    return adapter, match["id"] if match else text.strip()


def model_traits(router: Any, catalog: dict[str, Any], adapter: str, model: str) -> list[str]:
    traits: set[str] = set()
    table = catalog.get("model_traits", {})
    for key in (f"{adapter}/{model}", model):
        value = table.get(key) if isinstance(table, dict) else None
        if isinstance(value, list):
            traits.update(item for item in value if isinstance(item, str))
    adapter_entry = catalog.get("adapters", {}).get(adapter, {})
    registered = router._registered_match(adapter, model, catalog)[0] if "models" in adapter_entry else None
    if router.training_flag(adapter_entry, registered) is True:
        traits.add("trains-on-prompts")
    if isinstance(registered, dict) and registered.get("plan_cap_usd") == 0:
        traits.add("free")
    return sorted(traits)


def spread_family(router: Any, catalog: dict[str, Any], model: str) -> str:
    """The family used for council spread; unknown vendors count as their own."""
    family = router.infer_family(model, catalog)
    return family or router.model_slug_for_family(model).split("-", 1)[0]


def default_installed(adapter: str) -> bool:
    return shutil.which(BINARIES.get(adapter, adapter)) is not None


class Availability:
    """Installed, not cooling down and allowed (enabled adapter, weight above zero)."""

    def __init__(self, router: Any, catalog: dict[str, Any],
                 installed: Callable[[str], bool] = default_installed,
                 cooldowns: dict[str, Any] | None = None) -> None:
        self.router = router
        self.catalog = catalog
        self.installed = installed
        self.cooldowns = router._cooldowns(catalog) if cooldowns is None else cooldowns
        self._enabled: dict[str, str] = {}

    def disabled(self, adapter: str) -> str:
        if adapter not in self._enabled:
            try:
                compatibility, status = self.router.load_adapter_compatibility(adapter)
            except ImportError:  # a stdlib-only interpreter cannot read the YAML; dispatch preflight still checks
                compatibility, status = {"enabled": True}, ""
            self._enabled[adapter] = (
                status or ("" if compatibility["enabled"] else "disabled"))
        return self._enabled[adapter]

    def reason(self, adapter: str, model: str, weight: float = 1.0) -> str:
        if weight <= 0:
            return "off"
        if adapter not in self.catalog.get("adapters", {}):
            return "unknown adapter"
        if self.disabled(adapter):
            return "adapter " + self.disabled(adapter)
        if not self.installed(adapter):
            return "not installed"
        until = self.router._cooling(adapter, model, self.cooldowns, self.catalog)
        return f"cooling until {until}" if until else ""


def pool(router: Any, catalog: dict[str, Any], name: str, effort_order: dict[str, int]) -> list[dict[str, Any]]:
    """The configured pool for one route, each entry resolved to adapter, model and family."""
    entries = []
    for raw in catalog.get("routes", {}).get(name, []):
        if not entry_is_valid(raw, effort_order):
            continue
        adapter, model = split_model(router, catalog, raw["model"])
        entries.append({
            "key": f"{adapter}/{model}", "adapter": adapter, "model": model,
            "weight": weight_value(raw.get("weight")), "weight_label": raw.get("weight", DEFAULT_WEIGHT),
            "effort": effort_band(raw["effort"], effort_order) if "effort" in raw else None,
            "family": spread_family(router, catalog, model),
            "traits": model_traits(router, catalog, adapter, model),
        })
    return entries


def _effort(entry: dict[str, Any], requested: str | None, effort_order: dict[str, int]) -> str | None:
    band = entry.get("effort")
    if not band:
        return requested
    if requested not in effort_order:
        return band[0]
    # A caller's effort is clamped into the entry's band.
    if effort_order[requested] < effort_order[band[0]]:
        return band[0]
    return band[-1] if effort_order[requested] > effort_order[band[-1]] else requested


def _council(entries: list[dict[str, Any]], count: int, rng: random.Random) -> list[dict[str, Any]]:
    """Pick `count` entries: unused families first, weights as odds, repeats only once exhausted."""
    picks: list[dict[str, Any]] = []
    while len(picks) < count:
        remaining = list(entries)
        families: set[str] = set()
        while remaining and len(picks) < count:
            fresh = [entry for entry in remaining if entry["family"] not in families] or remaining
            chosen = rng.choices(fresh, weights=[entry["weight"] for entry in fresh])[0]
            picks.append(chosen)
            families.add(chosen["family"])
            remaining.remove(chosen)
    return picks


def _rotate(entries: list[dict[str, Any]], state_path: Path, project: str, route: str) -> dict[str, Any]:
    """Smooth weighted round-robin; the per-project cursor lives in the Fabric state root."""
    state_path.parent.mkdir(parents=True, exist_ok=True)
    with state_path.with_name(f".{state_path.name}.lock").open("a+") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            try:
                state = json.loads(state_path.read_text())
            except (OSError, ValueError):
                state = {}
            if not isinstance(state, dict) or state.get("schema_version") != 1:
                state = {"schema_version": 1, "projects": {}}
            cursor = state.setdefault("projects", {}).setdefault(project, {}).setdefault(route, {})
            total = sum(entry["weight"] for entry in entries)
            for entry in entries:
                cursor[entry["key"]] = float(cursor.get(entry["key"], 0)) + entry["weight"]
            chosen = max(entries, key=lambda entry: cursor[entry["key"]])
            cursor[chosen["key"]] -= total
            temporary = state_path.with_name(f".{state_path.name}.{os.getpid()}.tmp")
            temporary.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
            temporary.replace(state_path)
            return chosen
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def pick(router: Any, catalog: dict[str, Any], request: dict[str, Any], *,
         availability: Availability, state_path: Path, effort_order: dict[str, int],
         rng: random.Random | None = None) -> dict[str, Any]:
    """Resolve one request into picks; raises PoolError for an unusable request."""
    rng = rng or random.Random()
    council = request.get("council")
    models = request.get("models")
    confidential = request.get("confidential") is True
    requested_effort = request.get("effort") or None
    warnings: list[str] = []
    if council is not None and (isinstance(council, bool) or not isinstance(council, int)
                                or not 1 <= council <= MAX_COUNCIL):
        raise PoolError("council_invalid", f"Pass council as an integer from 1 to {MAX_COUNCIL}.")
    if models is not None:
        if (not isinstance(models, list) or not 1 <= len(models) <= MAX_COUNCIL
                or any(not isinstance(item, str) or not item.strip() for item in models)):
            raise PoolError("models_invalid", f"Pass models as 1-{MAX_COUNCIL} adapter/model[@effort] strings.")
        if request.get("route"):
            raise PoolError("route_conflict", "Pass route or models, not both.")
        entries = []
        for item in models:
            text, _, effort = item.partition("@")
            adapter, model = split_model(router, catalog, text)
            if effort and effort not in effort_order:
                raise PoolError("effort_invalid", f"Pass a supported effort after @ in {item}.")
            entries.append({"key": f"{adapter}/{model}", "adapter": adapter, "model": model, "weight": 1.0,
                            "effort": [effort] if effort else None,
                            "family": spread_family(router, catalog, model),
                            "traits": model_traits(router, catalog, adapter, model)})
        label, mode = "ad-hoc", "council"
    else:
        if not request.get("route"):
            raise PoolError("route_required", "Pass route with rotate or council.")
        label = route_name(catalog, str(request["route"]))
        entries = pool(router, catalog, label, effort_order)
        mode = "council" if council else "rotate" if request.get("rotate") else "top"
    if request.get("adapter"):
        entries = [entry for entry in entries if entry["adapter"] == request["adapter"]]
        if not entries:
            raise PoolError("route_adapter_empty", f"Omit adapter or pick a route with a {request['adapter']} entry.")
    usable = []
    for entry in entries:
        why = availability.reason(entry["adapter"], entry["model"], entry["weight"])
        if not why and confidential and PRIVATE_UNSAFE_TRAITS.intersection(entry["traits"]):
            why = "trains on prompts; task is confidential"
        if why:
            if why != "off":
                warnings.append(f"skipped {entry['key']}: {why}")
            continue
        usable.append(entry)
    if not usable:
        raise PoolError("route_unavailable", "No available model in " + label
                        + ("" if not warnings else " (" + "; ".join(warnings) + ")")
                        + "; fix: install or wait for a listed adapter, or pass models.")
    if mode == "top":
        top = max(entry["weight"] for entry in usable)
        chosen = [next(entry for entry in usable if entry["weight"] == top)]
    elif mode == "rotate":
        chosen = [_rotate(usable, state_path, str(request.get("project") or ""), label)]
    elif models is not None:
        chosen = usable
    else:
        chosen = _council(usable, council, rng)
        if council > len(usable):
            warnings.append(f"council {council} exceeds {len(usable)} available models; some repeat")
    picks = []
    for index, entry in enumerate(chosen, start=1):
        if PRIVATE_UNSAFE_TRAITS.intersection(entry["traits"]):
            warnings.append(f"{entry['key']} is a free tier that may train on prompts; "
                            "pass confidential: true to skip it")
        reason = f"{label} {mode}" + (f" {index}/{len(chosen)}" if mode == "council" else "")
        effort = _effort(entry, requested_effort, effort_order)
        picks.append({"adapter": entry["adapter"], "model": entry["model"], "family": entry["family"],
                      **({"effort": effort} if effort else {}), "reason": reason})
    return {"status": "ok", "route": label, "mode": mode, "picks": picks,
            "warnings": list(dict.fromkeys(warnings))}


def pick_many(router: Any, catalog: dict[str, Any], requests: list[Any], *,
              availability: Availability, state_path: Path, effort_order: dict[str, int],
              rng: random.Random | None = None) -> list[dict[str, Any]]:
    results = []
    for request in requests:
        try:
            if not isinstance(request, dict):
                raise PoolError("invalid_input", "Pass each pick request as an object.")
            results.append(pick(router, catalog, request, availability=availability,
                                state_path=state_path, effort_order=effort_order, rng=rng))
        except PoolError as exc:
            results.append({"status": "rejected", "error": exc.code, "fix": exc.fix})
    return results


def describe(router: Any, catalog: dict[str, Any], availability: Availability,
             effort_order: dict[str, int]) -> dict[str, Any]:
    """Every pool with live availability, for `provenant routes`."""
    routes = {}
    for name in catalog.get("routes", {}):
        routes[name] = [{
            "model": entry["key"], "weight": entry["weight_label"], "family": entry["family"],
            **({"effort": "-".join(entry["effort"])} if entry["effort"] else {}),
            **({"traits": entry["traits"]} if entry["traits"] else {}),
            "availability": availability.reason(entry["adapter"], entry["model"], entry["weight"]) or "available",
        } for entry in pool(router, catalog, name, effort_order)]
    return {"schema": "fabric.routes.v1", "routes": routes,
            "synonyms": dict(sorted(catalog.get("route_synonyms", {}).items()))}


def render(document: dict[str, Any]) -> str:
    lines = []
    for name, entries in document["routes"].items():
        lines.append(name)
        for entry in entries:
            model = entry["model"] + (f"@{entry['effort']}" if "effort" in entry else "")
            caveat = f"  [{', '.join(entry['traits'])}]" if "traits" in entry else ""
            lines.append(f"  {model:<52} {str(entry['weight']):<8} {entry['availability']}{caveat}")
    synonyms: dict[str, list[str]] = {}
    for alias, target in document["synonyms"].items():
        synonyms.setdefault(target, []).append(alias)
    if synonyms:
        lines.append("synonyms: " + "; ".join(f"{target} <- {', '.join(names)}"
                                              for target, names in sorted(synonyms.items())))
    return "\n".join(lines)
