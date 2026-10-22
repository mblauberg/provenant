"""Pick models from the global weighted route pools (#848).

A route (the tiers `junior`, `senior`, `principal`, or the crafts `design`, `writing`) is a weighted pool of
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
PRIVATE_UNSAFE_TRAITS = frozenset({"trains-on-prompts"})
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


# Names the tiers had before they became junior, senior and principal. None is accepted any more.
RENAMED_TIERS = {"scout": "junior", "bulk": "junior", "workhorse": "senior", "strong": "senior",
                 "review": "senior", "flagship": "principal"}
TIER_ROUTES = ("utility", "junior", "senior", "principal")


class RenamedNameError(ValueError):
    """A route, alias or overlay key still uses a retired tier name."""


def renamed_message(kind: str, name: str) -> str:
    return f"{kind} '{name}' was renamed to '{RENAMED_TIERS[name.strip().casefold()]}'; fix: use the new name"


def _renamed(name: Any) -> bool:
    return isinstance(name, str) and name.strip().casefold() in RENAMED_TIERS


def reject_renamed(overlay: Any, label: str = "overlay") -> Any:
    """Refuse an overlay that still uses a retired tier name, naming its replacement.

    Checked: pool names under `routes`, synonym targets, task-class and risk-tier aliases,
    and the alias keys of adapter and family profiles.
    """
    if not isinstance(overlay, dict):
        return overlay
    routes = overlay.get("routes")
    for name in routes if isinstance(routes, dict) else ():
        if _renamed(name):
            raise RenamedNameError(label + " " + renamed_message("route", name))
    synonyms = overlay.get("route_synonyms")
    for target in synonyms.values() if isinstance(synonyms, dict) else ():
        if _renamed(target):
            raise RenamedNameError(label + " " + renamed_message("route_synonyms target", target))
    tasks = overlay.get("task_class_routes")
    for route in tasks.values() if isinstance(tasks, dict) else ():
        if isinstance(route, dict) and _renamed(route.get("alias")):
            raise RenamedNameError(label + " " + renamed_message("alias", route["alias"]))
    for section in ("families", "adapters"):
        block = overlay.get(section)
        for entry in block.values() if isinstance(block, dict) else ():
            if not isinstance(entry, dict):
                continue
            keyed = [entry.get("aliases")]
            for key in ("role_overrides", "role_effort_defaults"):
                roles = entry.get(key)
                keyed.extend(roles.values() if isinstance(roles, dict) else ())
            for mapping in keyed:
                for name in mapping if isinstance(mapping, dict) else ():
                    if _renamed(name):
                        raise RenamedNameError(label + " " + renamed_message("alias", name))
            tiers = entry.get("risk_tier_overrides")
            for value in tiers.values() if isinstance(tiers, dict) else ():
                if isinstance(value, dict) and _renamed(value.get("alias")):
                    raise RenamedNameError(label + " " + renamed_message("alias", value["alias"]))
    kept = overlay.get("route_synonyms")
    if isinstance(kept, dict):
        overlay = {**overlay, "route_synonyms": {name: target for name, target in kept.items()
                                                  if name not in {*TIER_ROUTES, "design", "writing"}}}
    return overlay


class MigrationConflict(ValueError):
    """An installed catalogue holds a retired name and its replacement with different values."""


def migrate_renamed(document: Any) -> tuple[Any, list[str], list[str]]:
    """Rewrite retired tier names in an installed catalogue, keeping their values; list what moved.

    Used by the routing refresh only. Routing itself still rejects these names. A key under its new
    holds the same value as the old one, or `MigrationConflict` is raised: no value is dropped.
    The third result lists the dotted paths of renamed alias keys and route pools, whose values the
    caller keeps.
    """
    if not isinstance(document, dict):
        return document, [], []
    document = json.loads(json.dumps(document))
    notes: list[str] = []
    moved: list[str] = []

    def new_name(name: str) -> str:
        return RENAMED_TIERS[name.strip().casefold()]

    def settle(target: dict[str, Any], name: str, value: Any, old_path: str, new_path: str) -> None:
        if name in target and target[name] != value:
            raise MigrationConflict(
                f"{old_path} = {json.dumps(value)} conflicts with {new_path} = {json.dumps(target[name])}; "
                "keep one, delete the other, then refresh again")
        target.setdefault(name, value)

    def rename_keys(mapping: Any, path: str) -> Any:
        if not isinstance(mapping, dict) or not any(_renamed(key) for key in mapping):
            return mapping
        result = {key: value for key, value in mapping.items() if not _renamed(key)}
        for key, value in mapping.items():
            if _renamed(key):
                notes.append(f"{path}.{key} -> {new_name(key)}")
                moved.append(f"{path}.{new_name(key)}")
                settle(result, new_name(key), value, f"{path}.{key}", f"{path}.{new_name(key)}")
        return result

    def rename_alias(holder: Any, path: str) -> None:
        if isinstance(holder, dict) and _renamed(holder.get("alias")):
            notes.append(f"{path}.alias {holder['alias']} -> {new_name(holder['alias'])}")
            holder["alias"] = new_name(holder["alias"])

    routes = document.get("routes")
    if isinstance(routes, dict) and any(_renamed(key) for key in routes):
        merged = {key: value for key, value in routes.items() if not _renamed(key)}
        for key, value in routes.items():
            if not _renamed(key):
                continue
            notes.append(f"routes.{key} -> {new_name(key)}")
            moved.append(f"routes.{new_name(key)}")
            settle(merged, new_name(key), value, f"routes.{key}", f"routes.{new_name(key)}")
        document["routes"] = merged
    synonyms = document.get("route_synonyms")
    if isinstance(synonyms, dict):
        kept = {}
        for name, target in synonyms.items():
            if _renamed(name):
                notes.append(f"route_synonyms.{name} dropped (the product maps task classes)")
                continue
            if _renamed(target):
                notes.append(f"route_synonyms.{name} {target} -> {new_name(target)}")
                target = new_name(target)
            kept[name] = target
        document["route_synonyms"] = kept
    tasks = document.get("task_class_routes")
    for name, route in (tasks.items() if isinstance(tasks, dict) else ()):
        rename_alias(route, f"task_class_routes.{name}")
    for section in ("families", "adapters"):
        block = document.get(section)
        for name, entry in (block.items() if isinstance(block, dict) else ()):
            if not isinstance(entry, dict):
                continue
            base = f"{section}.{name}"
            if "aliases" in entry:
                entry["aliases"] = rename_keys(entry["aliases"], f"{base}.aliases")
            for key in ("role_overrides", "role_effort_defaults"):
                roles = entry.get(key)
                if isinstance(roles, dict):
                    entry[key] = {role: rename_keys(value, f"{base}.{key}.{role}") for role, value in roles.items()}
            tiers = entry.get("risk_tier_overrides")
            for tier, value in (tiers.items() if isinstance(tiers, dict) else ()):
                rename_alias(value, f"{base}.risk_tier_overrides.{tier}")
    return document, notes, moved


def route_name(catalog: dict[str, Any], name: str) -> str:
    routes = catalog.get("routes", {})
    key = name.strip().casefold()
    if key in routes:
        return key
    if key in RENAMED_TIERS:
        raise PoolError("route_renamed", renamed_message("route", name.strip()))
    synonym = catalog.get("route_synonyms", {}).get(key)
    if isinstance(synonym, str) and synonym in routes:
        return synonym
    raise PoolError("route_invalid", "Pass route " + ", ".join(sorted(routes)) + ".")


def split_model(router: Any, catalog: dict[str, Any], text: str) -> tuple[str, str]:
    """Split `adapter/model` (or a bare model id) into its adapter and canonical id."""
    adapter, model, _ = resolve_model(router, catalog, text)
    return adapter, model


def resolve_model(router: Any, catalog: dict[str, Any], text: str) -> tuple[str, str, bool]:
    """Adapter, canonical id and whether the catalogue registers the model.

    `adapter/model` names its adapter; a bare id (including a provider path such
    as `opencode-go/...`) goes to the adapter that registers or can run it.
    """
    adapters = catalog.get("adapters", {})
    text = text.strip()
    head, separator, rest = text.partition("/")
    if separator and head in adapters:
        for candidate in (rest, text):
            match, _ = router._registered_match(head, candidate, catalog)
            if match:
                return head, match["id"], True
        # `opencode/big-pickle` is itself an OpenCode provider path; `opencode/<provider>/x` is prefixed.
        if known_provider_path(text) and "/" not in rest:
            return head, text, False
        return head, rest, False
    adapter = router._owner_adapter(text, catalog)
    match = router._registered_match(adapter, text, catalog)[0] if adapter in adapters else None
    return adapter, match["id"] if match else text, match is not None


def known_provider_path(text: str) -> bool:
    """A bare `provider/model` path the OpenCode adapter can run without a prefix."""
    return text.casefold().startswith(("opencode/", "opencode-go/", "openrouter/"))


def model_traits(router: Any, catalog: dict[str, Any], adapter: str, model: str) -> list[str]:
    traits: set[str] = set()
    table = catalog.get("model_traits", {})
    adapter_entry = catalog.get("adapters", {}).get(adapter, {})
    registered = router._registered_match(adapter, model, catalog)[0] if "models" in adapter_entry else None
    # A live id the catalogue lacks inherits its family's traits, so it is never less private than its family.
    names = (model, *(((registered or {}).get("id"),) if (registered or {}).get("id") else ()),
             *((registered or {}).get("inherits", [])))
    for name in names:
        for key in (f"{adapter}/{name}", name):
            value = table.get(key) if isinstance(table, dict) else None
            if isinstance(value, list):
                traits.update(item for item in value if isinstance(item, str))
    flag = router.training_flag(adapter_entry, registered, model)
    if flag is True:
        traits.add("trains-on-prompts")
    if ((isinstance(registered, dict) and registered.get("plan_cap_usd") == 0) or router.is_free_model(adapter_entry, model)
            or (registered is None and router.looks_free(adapter_entry, model))):
        traits.add("free")
    # Free implies training unless the model's own entry says otherwise (zero-retention free tiers).
    if "free" in traits and (registered or {}).get("trains_on_prompts") is not False:
        traits.add("trains-on-prompts")
    return sorted(traits)


def spread_family(router: Any, catalog: dict[str, Any], model: str) -> str:
    """The family used for council spread; unknown vendors count as their own."""
    family = router.infer_family(model, catalog)
    return family or router.model_slug_for_family(model).split("-", 1)[0]


def default_installed(adapter: str) -> bool:
    return shutil.which(BINARIES.get(adapter, adapter)) is not None


class Availability:
    """Installed, not cooling down and allowed (enabled adapter, compatible model, weight above zero)."""

    def __init__(self, router: Any, catalog: dict[str, Any],
                 installed: Callable[[str], bool] = default_installed,
                 cooldowns: dict[str, Any] | None = None) -> None:
        self.router = router
        self.catalog = catalog
        self.installed = installed
        self.cooldowns = router._cooldowns(catalog) if cooldowns is None else cooldowns
        self._quota: dict[str, Any] | None = None
        self._enabled: dict[str, str] = {}
        self._compatibility: dict[str, dict[str, Any] | None] = {}

    def _load(self, adapter: str) -> None:
        try:
            compatibility, status = self.router.load_adapter_compatibility(adapter)
        except ImportError:  # a stdlib-only interpreter cannot read the YAML; dispatch preflight still checks
            compatibility, status = None, ""
        self._compatibility[adapter] = compatibility
        self._enabled[adapter] = status or ("" if compatibility is None or compatibility["enabled"] else "disabled")

    def disabled(self, adapter: str) -> str:
        if adapter not in self._enabled:
            self._load(adapter)
        return self._enabled[adapter]

    def incompatible(self, adapter: str, model: str) -> str:
        """The router's family/model gate, applied before a pick rather than after it."""
        if adapter not in self._compatibility:
            if adapter in self._enabled:  # injected enablement: no compatibility record to check
                return ""
            self._load(adapter)
        compatibility = self._compatibility.get(adapter)
        if not compatibility:
            return ""
        # The router treats an unrecognised vendor as generic-open; so does this gate.
        family = self.router.infer_family(model, self.catalog) or "generic-open"
        _, status = self.router.check_adapter_compatibility(compatibility, family, model)
        return {"adapter_family_forbidden": f"{adapter} cannot run the {family} family",
                "adapter_model_forbidden": f"{adapter} does not allow {model}"}.get(status, status)

    def last_resort(self, adapter: str) -> bool:
        """The adapter's selected account is drawing on credits under the `last_resort` credits policy."""
        if self._quota is None:
            self._quota = getattr(self.router, "_quota_states", dict)()
        return bool(self._quota.get(adapter, {}).get("last_resort"))

    def reason(self, adapter: str, model: str, weight: float = 1.0) -> str:
        if weight <= 0:
            return "off"
        if adapter not in self.catalog.get("adapters", {}):
            return "unknown adapter"
        if self.disabled(adapter):
            return "adapter " + self.disabled(adapter)
        incompatible = self.incompatible(adapter, model)
        if incompatible:
            return incompatible
        if not self.installed(adapter):
            return "not installed"
        until = self.router._cooling(adapter, model, self.cooldowns, self.catalog)
        return f"cooling until {until}" if until else ""


def pool(router: Any, catalog: dict[str, Any], name: str, effort_order: dict[str, int]) -> list[dict[str, Any]]:
    """The configured pool for one route, one entry per canonical adapter/model.

    Spellings of one model (`codex/sol`, a retired `gpt-6-sol`) collapse onto
    its registered id; the earlier entry's fields win, so an overlay entry,
    which merges ahead of the product's, still disables or reweights the model.
    """
    combined: dict[str, dict[str, Any]] = {}
    for raw in catalog.get("routes", {}).get(name, []):
        if not entry_is_valid(raw, effort_order):
            continue
        adapter, model = split_model(router, catalog, raw["model"])
        key = f"{adapter}/{model}"
        combined[key] = {**raw, **combined[key]} if key in combined else dict(raw)
    entries = []
    for key, raw in combined.items():
        adapter, model = key.split("/", 1)
        entries.append({
            "key": key, "adapter": adapter, "model": model,
            "confidential": raw.get("confidential", True),
            "weight": weight_value(raw.get("weight")), "weight_label": raw.get("weight", DEFAULT_WEIGHT),
            "effort": effort_band(raw["effort"], effort_order) if "effort" in raw else None,
            "family": spread_family(router, catalog, model),
            "traits": model_traits(router, catalog, adapter, model),
        })
    return entries


def _effort(entry: dict[str, Any], requested: str | None, effort_order: dict[str, int],
            label: str, warnings: list[str]) -> str | None:
    """The caller's effort clamped into the entry's band; a move either way is a warning."""
    band = entry.get("effort")
    if not band:
        return requested
    if requested not in effort_order:
        return band[0]
    if effort_order[requested] < effort_order[band[0]]:
        warnings.append(f"effort {requested} raised to {band[0]} by the {label} band for {entry['key']}")
        return band[0]
    if effort_order[requested] > effort_order[band[-1]]:
        warnings.append(f"effort {requested} lowered to {band[-1]} by the {label} band for {entry['key']}")
        return band[-1]
    return requested


def _council(entries: list[dict[str, Any]], count: int, rng: random.Random) -> list[dict[str, Any]]:
    """Pick `count` entries: unused families first, weights as odds, repeats only once exhausted.

    A prompt-training model never claims a seat for a new family: it joins a council
    only by its odds, so a rare contributor model is not drawn into every council.
    """
    picks: list[dict[str, Any]] = []
    while len(picks) < count:
        remaining = list(entries)
        families: set[str] = set()
        while remaining and len(picks) < count:
            fresh = remaining if not families else [
                entry for entry in remaining
                if entry["family"] not in families
                and not PRIVATE_UNSAFE_TRAITS.intersection(entry["traits"])] or remaining
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


ROUTE_REQUIRED = "Pass route ({routes}) or models with rotate or council."


def _spawn(native: str, entries: list[dict[str, Any]], count: int) -> str:
    names = ", ".join(dict.fromkeys(entry["model"] for entry in entries))
    return f"spawn {count} native {native} member{'s' if count != 1 else ''} ({names})"


def pick(router: Any, catalog: dict[str, Any], request: dict[str, Any], *,
         availability: Availability, state_path: Path, effort_order: dict[str, int],
         rng: random.Random | None = None) -> dict[str, Any]:
    """Resolve one request into picks; raises PoolError only for a request that cannot run."""
    rng = rng or random.Random()
    council = request.get("council")
    # The caller's own adapter (Claude Code: claude, Codex: codex) runs as native subagents, not through Fabric.
    native = request.get("native") if isinstance(request.get("native"), str) else None
    models = request.get("models")
    if "confidential" in request and type(request["confidential"]) is not bool:
        raise PoolError("confidential_invalid", "confidential must be a boolean")
    confidential = request.get("confidential") is True
    allow_free = request.get("allow_free", True) is not False
    requested_effort = request.get("effort") or None
    if isinstance(requested_effort, str) and requested_effort.strip().casefold() == "max":
        raise PoolError("effort_invalid", "effort 'max' was retired; fix: use xhigh")
    warnings: list[str] = []
    adapters = catalog.get("adapters", {})
    if council is not None and (isinstance(council, bool) or not isinstance(council, int)
                                or not 1 <= council <= MAX_COUNCIL):
        raise PoolError("council_invalid", f"Pass council as an integer from 1 to {MAX_COUNCIL}.")
    if council is not None and request.get("rotate"):
        warnings.append("rotate ignored: council already spreads picks")
    if models is not None:
        if (not isinstance(models, list) or not 1 <= len(models) <= MAX_COUNCIL
                or any(not isinstance(item, str) or not item.strip() for item in models)):
            raise PoolError("models_invalid", f"Pass models as 1-{MAX_COUNCIL} adapter/model[@effort] strings.")
        if request.get("route"):
            warnings.append(f"route {request['route']} ignored: models bypasses routes")
        if request.get("adapter"):
            warnings.append(f"adapter {request['adapter']} ignored: each models entry names its adapter")
        entries = []
        for item in models:
            text, _, effort = item.strip().partition("@")
            head, separator, _ = text.partition("/")
            adapter, model, registered = resolve_model(router, catalog, text)
            if separator and head not in adapters and not registered and not known_provider_path(text):
                raise PoolError("models_invalid", f"{item} names no known adapter; fix: prefix it with one of "
                                + ", ".join(sorted(adapters)) + ", e.g. codex/gpt-6-luna@low.")
            if effort and effort not in effort_order:
                raise PoolError("effort_invalid", f"Pass a supported effort after @ in {item}: "
                                + ", ".join(effort_order) + ".")
            if not registered:
                warnings.append(f"{text} is not in the catalogue; passing it to {adapter} as given")
            effort = effort or (router.suffix_effort(adapter, text.partition("/")[2] if separator and head in adapters else text,
                                                    catalog) if adapter in adapters else "")
            entries.append({"key": f"{adapter}/{model}", "adapter": adapter, "model": model, "weight": 1.0,
                            "effort": [effort] if effort else None,
                            "family": spread_family(router, catalog, model),
                            "traits": model_traits(router, catalog, adapter, model)})
        label, mode = "ad-hoc", "council"
    else:
        if not request.get("route"):
            raise PoolError("route_required", ROUTE_REQUIRED.format(routes=", ".join(catalog.get("routes", {}))))
        label = route_name(catalog, str(request["route"]))
        defaults = catalog.get("route_defaults", {}).get(label, {})
        if "confidential" in defaults and type(defaults["confidential"]) is not bool:
            raise PoolError("confidential_invalid", f"{label} default confidential must be a boolean")
        confidential = request.get("confidential", defaults.get("confidential", False)) is True
        allow_free = request.get("allow_free", defaults.get("allow_free", True)) is not False
        entries = pool(router, catalog, label, effort_order)
        mode = "council" if council else "rotate" if request.get("rotate") else "top"
        if request.get("adapter"):
            entries = [entry for entry in entries if entry["adapter"] == request["adapter"]]
            if not entries:
                adapter = request["adapter"]
                containing = [name for name in catalog.get("routes", {})
                              if any(entry["adapter"] == adapter for entry in pool(router, catalog, name, effort_order))]
                raise PoolError("route_adapter_empty", f"{label} has no {adapter} entry; pools containing {adapter}: "
                                f"{', '.join(containing) or 'none'}; fix: pick one of those routes, omit adapter, "
                                f"or pass model: {adapter}/<model-id> instead of route.")
            native = None  # naming the native adapter is explicit; dispatch warns instead
    usable = []
    for entry in entries:
        why = availability.reason(entry["adapter"], entry["model"], entry["weight"])
        if not why and confidential and PRIVATE_UNSAFE_TRAITS.intersection(entry["traits"]):
            why = "trains on prompts; task is confidential"
        if not why and confidential and entry.get("confidential") is False:
            why = "public-only pool member; task is confidential"
        if not why and confidential and not allow_free and "free" in entry["traits"]:
            why = "free tier; confidential route requires a paid model"
        if why:
            if why != "off":
                warnings.append(f"skipped {entry['key']}: {why}")
            continue
        usable.append(entry)
    if models is None and not request.get("adapter"):
        # Credits are spent only when nothing with allowance qualifies; naming the adapter or model pins it.
        allowance = [entry for entry in usable if not availability.last_resort(entry["adapter"])]
        spare = [entry for entry in usable if entry not in allowance]
        if allowance:
            warnings.extend(f"skipped {entry['key']}: drawing on credits while others have allowance" for entry in spare)
            usable = allowance
        else:
            warnings.extend(f"{entry['key']} draws on credits: nothing with allowance is available" for entry in spare)
    if not usable:
        raise PoolError("route_unavailable", "No available model in " + label
                        + ("" if not warnings else " (" + "; ".join(warnings) + ")")
                        + "; fix: pass another route or models; `provenant routes` shows what is available.")
    if native and mode != "council" and models is None:
        own = [entry for entry in usable if entry["adapter"] == native]
        usable = [entry for entry in usable if entry["adapter"] != native]
        warnings.extend(f"{entry['key']} skipped: use a native subagent" for entry in own)
        if not usable:
            top = max(own, key=lambda entry: entry["weight"])
            raise PoolError("route_native_only", _spawn(native, [top], 1) + "; nothing else in " + label
                            + f" is available; pass adapter {native} to run it through Fabric anyway.")
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
        if native:
            # Pick across every family first, then hand the native members back so the council keeps its spread.
            own = [entry for entry in chosen if entry["adapter"] == native]
            chosen = [entry for entry in chosen if entry["adapter"] != native]
            if own and not chosen:
                raise PoolError("route_native_only", _spawn(native, own, len(own))
                                + f"; Fabric has no other member to run; pass adapter {native} to run them anyway.")
            if own:
                warnings.append(f"{_spawn(native, own, len(own))}; Fabric runs {len(chosen)} of {council}")
    picks = []
    for index, entry in enumerate(chosen, start=1):
        if PRIVATE_UNSAFE_TRAITS.intersection(entry["traits"]):
            warnings.append(f"{entry['key']} is a free tier that may train on prompts; "
                            "pass confidential: true to skip it")
        reason = f"{label} {mode}" + (f" {index}/{len(chosen)}" if mode == "council" else "")
        effort = _effort(entry, requested_effort, effort_order, label, warnings)
        picks.append({"adapter": entry["adapter"], "model": entry["model"], "family": entry["family"],
                      **({"effort": effort} if effort else {}), "reason": reason})
    return {"status": "ok", "route": label, "mode": mode, "picks": picks, "confidential": confidential,
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
    # Provider pace is report-only here; only an exhausted window changes availability (as a cooldown).
    quota = getattr(router, "_quota_states", dict)()
    for name in catalog.get("routes", {}):
        routes[name] = [{
            "model": entry["key"], "weight": entry["weight_label"], "family": entry["family"],
            **({"effort": "-".join(entry["effort"])} if entry["effort"] else {}),
            **({"traits": entry["traits"]} if entry["traits"] else {}),
            "availability": availability.reason(entry["adapter"], entry["model"], entry["weight"]) or "available",
            **({"quota": {key: quota[entry["adapter"]][key] for key in ("account", "state", "summary", "observed_at")}}
               if entry["adapter"] in quota else {}),
        } for entry in pool(router, catalog, name, effort_order)]
    return {"schema": "fabric.routes.v1", "routes": routes,
            "synonyms": dict(sorted(catalog.get("route_synonyms", {}).items()))}


def render(document: dict[str, Any], full: bool = False) -> str:
    """Every pool; by default unavailable and cooling models collapse to one line."""
    lines = []
    for name, entries in document["routes"].items():
        lines.append(name)
        hidden = 0
        cooling = []
        for entry in entries:
            if not full and entry["availability"] != "available":
                # Cooling models stay named, since they recover; the rest collapse to a count.
                hidden += 1
                if "cooling until " in entry["availability"]:
                    cooling.append((entry["availability"].split("cooling until ", 1)[1], entry["model"]))
                continue
            model = entry["model"] + (f"@{entry['effort']}" if "effort" in entry else "")
            caveat = f"  [{', '.join(entry['traits'])}]" if "traits" in entry else ""
            if entry.get("quota", {}).get("state") in ("deficit", "credit_only"):
                caveat += f"  [quota {entry['quota']['summary']}]"
            lines.append(f"  {model:<52} {str(entry['weight']):<8} {entry['availability']}{caveat}")
        if hidden:
            lines.append(f"  {hidden} unavailable or cooling"
                         + (f"; soonest cooling until {min(cooling)[0]}" if cooling else "")
                         + "; use --all")
            if cooling:
                lines.append("  cooling: " + ", ".join(model for _, model in sorted(cooling)))
    synonyms: dict[str, list[str]] = {}
    for alias, target in document["synonyms"].items():
        synonyms.setdefault(target, []).append(alias)
    if synonyms:
        lines.append("synonyms: " + "; ".join(f"{target} <- {', '.join(names)}"
                                              for target, names in sorted(synonyms.items())))
    return "\n".join(lines)
