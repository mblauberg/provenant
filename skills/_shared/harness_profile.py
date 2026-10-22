"""Harness profiles: the knob registry, presets and the one resolver.

A knob is a named cost-versus-quality choice in `config/harness-knobs.json`
whose default reproduces the harness as it behaved before profiles existed. A
preset is a partial knob map. Resolution, lowest first:

    registry default < preset < user file < project file < environment < dispatch

The user file is `<instance root>/config/harness-profile.json`, the project file
`<project root>/.agents/harness-profile.json`. The environment names a preset in
`PROVENANT_PROFILE` and knobs in `PROVENANT_KNOB_<NAME>`. Within each layer a
task-class entry beats the layer's general one, and the preset applied is the
one named by the highest layer that names any. A knob marked
`"project": "strengthen"` (the review knobs) may only be made more rigorous by
the project file, because a lane's own checkout writes that file. A registry
`floor` then raises a value below the minimum for the risk tier and every tier
above it.

The specification is `docs/specs/harness/profiles.md`. Python and the CLI share
this module; nothing else re-implements resolution.
"""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
import tempfile
from typing import Any, Callable, Mapping


REGISTRY = "config/harness-knobs.json"
RISK_POLICY = "config/risk-policy.json"
ROUTING = "config/model-routing.json"
USER_FILE = "config/harness-profile.json"
PROJECT_FILE = ".agents/harness-profile.json"
DEFAULT_PRESET = "balanced"
PRESET_ENV = "PROVENANT_PROFILE"
KNOB_ENV_PREFIX = "PROVENANT_KNOB_"
RISK_TIER_ENV = "PROVENANT_RISK_TIER"
# The instance root whose user file applies, pinned by dispatch before routing may
# repoint AGENT_FABRIC_INSTANCE_ROOT at the product catalogue.
PROFILE_ROOT_ENV = "PROVENANT_PROFILE_ROOT"
TYPES = {"int", "enum", "bool"}
STATUSES = {"wired", "advisory"}
KNOB_FIELDS = {"type", "default", "status", "reader", "effect", "trade_off",
               "min", "max", "nullable", "values", "floor", "project"}
LAYER_FIELDS = {"schema_version", "description", "preset", "knobs", "task_classes"}
CLASS_FIELDS = {"preset", "knobs"}


class ProfileError(ValueError):
    """A registry, profile file or override that does not validate."""


def _product_root() -> Path:
    # roots.py decides the product root; load it by path so this module works
    # whether or not `_shared` is importable from the caller.
    module = sys.modules.get("provenant_shared_roots")
    if module is None:
        spec = importlib.util.spec_from_file_location(
            "provenant_shared_roots", Path(__file__).with_name("roots.py"))
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        sys.modules["provenant_shared_roots"] = module
        spec.loader.exec_module(module)
    return module.product_root()


def env_name(knob: str) -> str:
    """`review.cross_family` -> `PROVENANT_KNOB_REVIEW_CROSS_FAMILY`."""
    return KNOB_ENV_PREFIX + re.sub(r"[^A-Za-z0-9]", "_", knob).upper()


def user_path(environ: Mapping[str, str] | None = None) -> Path:
    environ = os.environ if environ is None else environ
    instance = (environ.get(PROFILE_ROOT_ENV) or environ.get("AGENT_FABRIC_INSTANCE_ROOT")
                or str(Path.home() / ".agents"))
    return Path(instance).expanduser() / USER_FILE


def project_path(start: Path | None = None) -> Path:
    """The project file under the nearest checkout root (a `.git` entry), else `start`."""
    start = (start or Path.cwd()).resolve()
    for directory in (start, *start.parents):
        if (directory / ".git").exists():
            return directory / PROJECT_FILE
    return start / PROJECT_FILE


def _read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise
    except (OSError, UnicodeError, ValueError) as exc:
        raise ProfileError(f"{path}: unreadable JSON ({exc})") from None


@dataclass(frozen=True)
class Registry:
    knobs: dict[str, dict[str, Any]]
    presets: dict[str, dict[str, Any]]
    tier_order: tuple[str, ...]
    task_classes: frozenset[str]

    def check(self, name: str, value: Any, origin: str = "value") -> Any:
        """Return `value` if it is valid for knob `name`, else raise."""
        spec = self.knobs.get(name)
        if spec is None:
            raise ProfileError(f"{origin}: unknown knob {name!r}; `provenant profile list` names them")
        if value is None and spec.get("nullable"):
            return None
        kind = spec["type"]
        if kind == "int":
            if (isinstance(value, bool) or not isinstance(value, int)
                    or not spec["min"] <= value <= spec["max"]):
                raise ProfileError(f"{origin}: {name} takes an integer from {spec['min']} to {spec['max']}"
                                   + (" or null" if spec.get("nullable") else ""))
        elif kind == "enum":
            if value not in spec["values"]:
                raise ProfileError(f"{origin}: {name} takes one of {', '.join(spec['values'])}")
        elif not isinstance(value, bool):
            raise ProfileError(f"{origin}: {name} takes true or false")
        return value

    def parse(self, name: str, raw: str, origin: str = "value") -> Any:
        """Parse a knob value written as text (environment, CLI)."""
        spec = self.knobs.get(name)
        if spec is None:
            return self.check(name, raw, origin)
        text = raw.strip()
        if spec.get("nullable") and text.lower() in {"null", "none", "default"}:
            return None
        if spec["type"] == "int":
            try:
                return self.check(name, int(text), origin)
            except ValueError:
                return self.check(name, text, origin)
        if spec["type"] == "bool":
            truth = {"true": True, "1": True, "on": True, "false": False, "0": False, "off": False}
            return self.check(name, truth.get(text.lower(), text), origin)
        return self.check(name, text, origin)

    def rank(self, name: str, value: Any) -> int:
        """Order values from least to most rigorous, for floors."""
        spec = self.knobs[name]
        return spec["values"].index(value) if spec["type"] == "enum" else int(value)

    def check_layer(self, layer: Any, origin: str, *, allow_presets: bool = True) -> dict[str, Any]:
        """Validate a preset or profile file: optional preset, knobs and task-class entries."""
        if not isinstance(layer, dict):
            raise ProfileError(f"{origin}: must be a JSON object")
        unknown = set(layer) - LAYER_FIELDS
        if unknown:
            raise ProfileError(f"{origin}: unknown field(s) {', '.join(sorted(unknown))}")
        self._check_part(layer, origin, allow_presets)
        classes = layer.get("task_classes", {})
        if not isinstance(classes, dict):
            raise ProfileError(f"{origin}: task_classes must be an object")
        for task_class, part in classes.items():
            where = f"{origin} task_classes.{task_class}"
            if task_class not in self.task_classes:
                raise ProfileError(f"{where}: unknown task class; known: {', '.join(sorted(self.task_classes))}")
            if not isinstance(part, dict) or set(part) - CLASS_FIELDS:
                raise ProfileError(f"{where}: takes only preset and knobs")
            self._check_part(part, where, allow_presets)
        return layer

    def _check_part(self, part: dict[str, Any], origin: str, allow_presets: bool) -> None:
        if "preset" in part:
            if not isinstance(part["preset"], str):
                raise ProfileError(f"{origin}: preset must be a preset name")
            if not allow_presets:
                raise ProfileError(f"{origin}: a preset cannot name another preset")
            if part["preset"] not in self.presets:
                raise ProfileError(f"{origin}: unknown preset {part['preset']!r}; known: {', '.join(self.presets)}")
        knobs = part.get("knobs", {})
        if not isinstance(knobs, dict):
            raise ProfileError(f"{origin}: knobs must be an object")
        for name, value in knobs.items():
            self.check(name, value, origin)


def _check_knob(name: str, spec: Any, tier_order: tuple[str, ...]) -> None:
    where = f"{REGISTRY} knobs.{name}"
    if not isinstance(spec, dict) or set(spec) - KNOB_FIELDS:
        raise ProfileError(f"{where}: unknown fields")
    if spec.get("type") not in TYPES or spec.get("status") not in STATUSES:
        raise ProfileError(f"{where}: type must be int, enum or bool and status wired or advisory")
    if not all(isinstance(spec.get(key), str) and spec[key] for key in ("reader", "effect", "trade_off")):
        raise ProfileError(f"{where}: reader, effect and trade_off are required")
    if spec["type"] == "int" and not all(isinstance(spec.get(key), int) for key in ("min", "max")):
        raise ProfileError(f"{where}: an int knob needs min and max")
    if spec["type"] == "enum" and not (isinstance(spec.get("values"), list) and spec["values"]):
        raise ProfileError(f"{where}: an enum knob needs values, least rigorous first")
    floor = spec.get("floor", {})
    if not isinstance(floor, dict) or set(floor) - set(tier_order):
        raise ProfileError(f"{where}: floor keys must be risk tiers ({', '.join(tier_order)})")
    if floor and spec.get("nullable"):
        raise ProfileError(f"{where}: a nullable knob cannot carry a floor")
    if spec.get("project", "strengthen") != "strengthen" or ("project" in spec and spec.get("nullable")):
        raise ProfileError(f"{where}: project takes only \"strengthen\", on a knob that is not nullable")


def load_registry(root: Path | None = None) -> Registry:
    """Read and validate the registry, its presets, the risk tiers and the task classes."""
    root = root or _product_root()
    try:
        document = _read_json(root / REGISTRY)
        tier_order = tuple(_read_json(root / RISK_POLICY)["tier_order"])
        task_classes = frozenset(_read_json(root / ROUTING).get("task_class_routes", {}))
    except FileNotFoundError as exc:
        raise ProfileError(f"harness profile registry unavailable: {exc.filename}") from None
    except (KeyError, TypeError, AttributeError):
        raise ProfileError(f"{RISK_POLICY}: tier_order missing") from None
    if not isinstance(document, dict) or not isinstance(document.get("knobs"), dict) \
            or not isinstance(document.get("presets"), dict):
        raise ProfileError(f"{REGISTRY}: needs knobs and presets objects")
    for name, spec in document["knobs"].items():
        _check_knob(name, spec, tier_order)
    registry = Registry(document["knobs"], document["presets"], tier_order, task_classes)
    for name, spec in registry.knobs.items():
        registry.check(name, spec["default"], f"{REGISTRY} knobs.{name}.default")
        for tier, value in spec.get("floor", {}).items():
            registry.check(name, value, f"{REGISTRY} knobs.{name}.floor.{tier}")
    if DEFAULT_PRESET not in registry.presets or registry.presets[DEFAULT_PRESET].get("knobs"):
        raise ProfileError(f"{REGISTRY}: preset {DEFAULT_PRESET!r} must exist and change nothing")
    for name, preset in registry.presets.items():
        registry.check_layer(preset, f"{REGISTRY} presets.{name}", allow_presets=False)
    return registry


@dataclass(frozen=True)
class Resolution:
    """A resolved knob map with where each value came from."""

    preset: str
    task_class: str | None
    risk_tier: str | None
    knobs: dict[str, Any]
    sources: dict[str, str]
    overrides: dict[str, Any]
    floored: tuple[str, ...]

    @property
    def digest(self) -> str:
        """12 hex of the knobs that differ from their defaults; equal behaviour, equal digest."""
        canonical = json.dumps(self.overrides, sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(canonical.encode()).hexdigest()[:12]

    def stamp(self) -> dict[str, Any]:
        """The record each attempt carries so outcomes compare per combination."""
        return {"preset": self.preset, "digest": self.digest, "task_class": self.task_class,
                "risk_tier": self.risk_tier, "knobs": dict(self.knobs),
                **({"floored": list(self.floored)} if self.floored else {})}


def _class_part(layer: Mapping[str, Any], task_class: str | None) -> Mapping[str, Any]:
    return (layer.get("task_classes") or {}).get(task_class, {}) if task_class else {}


def _merge(registry: Registry, task_class: str | None,
           layers: list[tuple[str, Mapping[str, Any]]]) -> tuple[str, dict[str, Any], dict[str, str]]:
    """The preset, knob values and sources from `layers`, before guards and floors."""
    preset = DEFAULT_PRESET
    for _source, layer in reversed(layers):
        named = _class_part(layer, task_class).get("preset") or layer.get("preset")
        if named:
            preset = named
            break
    if preset not in registry.presets:
        raise ProfileError(f"unknown preset {preset!r}; known: {', '.join(registry.presets)}")
    knobs = {name: spec["default"] for name, spec in registry.knobs.items()}
    sources = dict.fromkeys(knobs, "default")
    for source, layer in [(f"preset:{preset}", registry.presets[preset]), *layers]:
        for part in (layer, _class_part(layer, task_class)):
            for name, value in (part.get("knobs") or {}).items():
                knobs[name] = registry.check(name, value, source)
                sources[name] = source + (f" ({task_class})" if part is not layer else "")
    return preset, knobs, sources


def resolve(registry: Registry, *, task_class: str | None = None, risk_tier: str | None = None,
            layers: list[tuple[str, Mapping[str, Any]]] | None = None) -> Resolution:
    """Resolve knobs from layers given lowest first, e.g. user, project, env, dispatch."""
    layers = layers or []
    if risk_tier is not None and risk_tier not in registry.tier_order:
        raise ProfileError(f"unknown risk tier {risk_tier!r}; known: {', '.join(registry.tier_order)}")
    preset, knobs, sources = _merge(registry, task_class, layers)
    guarded = [name for name, spec in registry.knobs.items() if spec.get("project") == "strengthen"]
    if guarded and any(source == "project" for source, _ in layers):
        # Resolve again without the project file; a guarded knob keeps the more rigorous result.
        _, trusted, trusted_sources = _merge(registry, task_class, [item for item in layers if item[0] != "project"])
        for name in guarded:
            if registry.rank(name, trusted[name]) > registry.rank(name, knobs[name]):
                knobs[name], sources[name] = trusted[name], trusted_sources[name] + " (project may only strengthen)"
    floored = []
    if risk_tier is not None:
        position = registry.tier_order.index(risk_tier)
        for name, spec in registry.knobs.items():
            floors = [value for tier, value in spec.get("floor", {}).items()
                      if registry.tier_order.index(tier) <= position]
            if not floors:
                continue
            floor = max(floors, key=lambda value: registry.rank(name, value))
            if registry.rank(name, knobs[name]) < registry.rank(name, floor):
                knobs[name], sources[name] = floor, f"floor:{risk_tier}"
                floored.append(name)
    overrides = {name: value for name, value in knobs.items() if value != registry.knobs[name]["default"]}
    return Resolution(preset, task_class, risk_tier, knobs, sources, overrides, tuple(floored))


def read_layer(registry: Registry, path: Path) -> dict[str, Any]:
    """A profile file, or {} when it does not exist."""
    try:
        document = _read_json(path)
    except FileNotFoundError:
        return {}
    return registry.check_layer(document, str(path))


def env_layer(registry: Registry, environ: Mapping[str, str]) -> dict[str, Any]:
    names = {env_name(name): name for name in registry.knobs}
    unknown = sorted(key for key in environ if key.startswith(KNOB_ENV_PREFIX) and key not in names)
    if unknown:
        raise ProfileError(f"unknown knob variable(s) {', '.join(unknown)}; `provenant profile list` names the knobs")
    layer: dict[str, Any] = {"knobs": {names[key]: registry.parse(names[key], environ[key], key)
                                       for key in environ if key in names}}
    if environ.get(PRESET_ENV):
        layer["preset"] = environ[PRESET_ENV]
    return registry.check_layer(layer, PRESET_ENV)


def parse_assignments(registry: Registry, assignments: list[str] | None,
                      origin: str = "--knob") -> dict[str, Any]:
    """`NAME=VALUE` overrides; unknown knobs and bad values raise."""
    knobs = {}
    for item in assignments or []:
        name, separator, raw = item.partition("=")
        if not separator:
            raise ProfileError(f"{origin} {item!r}: pass NAME=VALUE")
        knobs[name.strip()] = registry.parse(name.strip(), raw, origin)
    return knobs


def current(*, task_class: str | None = None, risk_tier: str | None = None,
            dispatch: Mapping[str, Any] | None = None, cwd: Path | None = None,
            environ: Mapping[str, str] | None = None, registry: Registry | None = None) -> Resolution:
    """Resolve from the files and environment in force for `cwd`."""
    environ = os.environ if environ is None else environ
    registry = registry or load_registry()
    layers = [("user", read_layer(registry, user_path(environ))),
              ("project", read_layer(registry, project_path(cwd))),
              ("env", env_layer(registry, environ)),
              ("dispatch", {"knobs": dict(dispatch or {})})]
    return resolve(registry, task_class=task_class or None,
                   risk_tier=risk_tier or environ.get(RISK_TIER_ENV) or None, layers=layers)


def lenient(**kwargs: Any) -> tuple[Resolution | None, str | None]:
    """For runtime readers: a bad profile never blocks a lane; it falls back to defaults with a reason."""
    try:
        return current(**kwargs), None
    except (ProfileError, OSError) as exc:
        return None, f"harness profile ignored: {exc}"


def value(name: str, fallback: Any, **kwargs: Any) -> Any:
    """One knob's resolved value, or `fallback` (the reader's built-in default) when unresolvable."""
    resolution, _error = lenient(**kwargs)
    return resolution.knobs.get(name, fallback) if resolution else fallback


def edit_file(registry: Registry, path: Path, change: Callable[[dict[str, Any]], None]) -> dict[str, Any]:
    """Apply `change` to a profile file, validate and replace it atomically."""
    try:
        document = _read_json(path)
    except FileNotFoundError:
        document = {"schema_version": 1}
    registry.check_layer(document, str(path))  # repair a malformed file by hand before editing it
    change(document)
    for task_class in list(document.get("task_classes", {})):
        part = document["task_classes"][task_class]
        if not part.get("knobs"):
            part.pop("knobs", None)
        if not part:
            del document["task_classes"][task_class]
    if not document.get("task_classes"):
        document.pop("task_classes", None)
    registry.check_layer(document, str(path))
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", dir=path.parent, prefix=".harness-profile.", delete=False,
                                     encoding="utf-8") as handle:
        json.dump(document, handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(handle.name, path)
    return document
