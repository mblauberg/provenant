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


# `scripts/lib/roots.py` is the single resolver for the product root (#754).
# The fallback loads that one file when this script is run directly by path and
# the product root is not on `sys.path`: it locates the resolver, it does not
# decide the root, and it leaves import resolution untouched (#755).
try:
    from scripts.lib.roots import product_root
except ModuleNotFoundError:  # pragma: no cover - direct invocation by path
    import importlib.util as _roots_util
    _roots_spec = _roots_util.spec_from_file_location(
        "provenant_roots", Path(__file__).resolve().parents[1] / "scripts" / "lib" / "roots.py"
    )
    _roots_module = _roots_util.module_from_spec(_roots_spec)
    _roots_spec.loader.exec_module(_roots_module)
    product_root = _roots_module.product_root

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


def catalogue_snapshot(path: Path | None = None) -> dict[str, Any]:
    product = path or PRODUCT_CATALOG_PATH
    base = json.loads(product.read_text())
    drift: list[str] = []
    sources = [str(product)]
    if path is None and CATALOG_PATH != product and CATALOG_PATH.exists():
        sources.append(str(CATALOG_PATH))
        try:
            overlay = json.loads(CATALOG_PATH.read_text())
            if isinstance(overlay, dict):
                base = _merge_catalog(base, overlay, "", drift)
            else:
                drift.append("instance catalogue: malformed overlay dropped; fix: use a JSON object")
        except (OSError, ValueError):
            drift.append("instance catalogue: unreadable overlay dropped; fix: refresh routing")
    models = [dict(model, adapter=adapter) for adapter, entry in base.get("adapters", {}).items()
              for model in entry.get("models", []) if isinstance(model, dict)]
    shorthands: dict[str, list[str]] = {}
    for model in models:
        for name in model.get("names", []):
            shorthands.setdefault(name.casefold(), []).append(f"{model['adapter']}/{model['id']}")
    digest = hashlib.sha256(json.dumps(base, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    stale_alias_warnings: list[str] = []
    try:
        capabilities = json.loads((_state_root() / "capabilities.json").read_text())
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
            "endpoints": base.get("endpoints", {}), "catalogue": base,
            "stale_alias_warnings": stale_alias_warnings}


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
    matches = exact or named or variant or retired
    if not matches:
        return None, []
    chosen = next((item for item in matches if item.get("default")), None)
    if chosen is None:
        chosen = max(matches, key=lambda item: tuple(int(part) for part in re.findall(r"\d+", item["id"])))
    notes = []
    if len(matches) > 1:
        notes.append(f"{requested} is ambiguous; used {chosen['id']} (alternatives: {', '.join(item['id'] for item in matches)})")
    if retired:
        notes.append(f"{requested} is retired; routed to {chosen['id']}")
    return chosen, notes


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


def _cooldowns(catalog: dict[str, Any]) -> dict[str, Any]:
    path = _state_root() / "cooldowns.json"
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        return {}
    if not isinstance(data, dict):
        return {}
    records = data.get("cooldowns", data)
    if not isinstance(records, dict):
        return {}
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


def _state_root() -> Path:
    return Path(os.environ.get("AGENT_FABRIC_STATE_ROOT", str(Path.home() / ".local/state/agent-harness/fabric")))


def _listed_models(raw: str) -> list[str]:
    try:
        data = json.loads(raw)
    except ValueError:
        data = None
    if isinstance(data, dict):
        data = data.get("models", data.get("data", []))
    if isinstance(data, dict):
        return list(data)
    if isinstance(data, list):
        return list(dict.fromkeys(item if isinstance(item, str) else item.get("id", item.get("name", ""))
                                  for item in data if isinstance(item, (str, dict))))
    listed: list[str] = []
    for line in raw.splitlines():
        match = re.search(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+", line)
        if match:
            listed.append(match.group(0))
            continue
        match = re.match(r"^\s*(?:[-*]\s*)?([A-Za-z][A-Za-z0-9._/-]*)(?:\s|$)", line)
        if match and match.group(1).casefold() not in {"available", "models", "model", "name", "id"}:
            listed.append(match.group(1))
    return list(dict.fromkeys(listed))


def _kiro_probe_enforced(evidence: Any) -> bool:
    return (isinstance(evidence, dict) and evidence.get("attempted_write") is True
            and evidence.get("permission_denied") is True and evidence.get("file_created") is False)


def probe_capabilities(adapter: str, executable: str, deadline: float | None = None) -> tuple[dict[str, Any], int]:
    commands = {"opencode": ["models"], "cursor": ["models"],
                "kiro": ["chat", "--list-models", "--format", "json"],
                "codex": ["debug", "models"]}
    if adapter not in commands:
        return {"status": "unsupported_adapter", "adapter": adapter}, 2
    executable = shutil.which(executable) or executable
    def remaining(limit: float) -> float:
        return max(0.01, min(limit, deadline - time.monotonic())) if deadline is not None else limit
    try:
        version = subprocess.run([executable, "--version"], capture_output=True, text=True,
                                 timeout=remaining(2), check=True).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return {"status": "probe_unavailable", "adapter": adapter,
                "message": "CLI version unavailable; fix: check the executable"}, 1
    path = _state_root() / "capabilities.json"
    try:
        cache = json.loads(path.read_text())
        if not isinstance(cache, dict):
            cache = {}
    except (OSError, ValueError):
        cache = {}
    previous = cache.get(adapter)
    executable_path = str(Path(executable).resolve())
    if (isinstance(previous, dict) and previous.get("version") == version
            and previous.get("executable") == executable_path):
        try:
            observed = datetime.fromisoformat(previous["observed_at"].replace("Z", "+00:00"))
            ttl = 3600 if (previous.get("status") == "probe_unavailable" or
                           (adapter == "kiro" and not _kiro_probe_enforced(previous.get("read_only_probe")))) else 86400
            if (datetime.now(timezone.utc) - observed).total_seconds() < ttl:
                return {**previous, "cache_hit": True}, 0 if previous.get("status") != "probe_unavailable" else 1
        except (ValueError, KeyError, TypeError):
            pass
    try:
        listing = subprocess.run([executable, *commands[adapter]], capture_output=True, text=True,
                                 timeout=remaining(3), check=True).stdout
        help_text = subprocess.run([executable, "--help"], capture_output=True, text=True,
                                   timeout=remaining(1), check=False).stdout
    except (OSError, subprocess.SubprocessError):
        record = {"status": "probe_unavailable", "adapter": adapter, "version": version,
                  "executable": executable_path,
                  "observed_at": datetime.now(timezone.utc).isoformat(), "models": [],
                  "message": "Model list unavailable; fix: check CLI authentication"}
        code = 1
    else:
        record = {"adapter": adapter, "version": version, "executable": executable_path,
                  "observed_at": datetime.now(timezone.utc).isoformat(),
                  "probed_flags": sorted(set(re.findall(r"--[a-z][a-z-]+", help_text))),
                  "models": _listed_models(listing)}
        if adapter == "kiro":
            record["read_only_probe"] = _probe_kiro_read_only(executable, version, deadline)
        code = 0
    path.parent.mkdir(parents=True, exist_ok=True)
    lock = path.with_suffix(".lock")
    with lock.open("a+") as guard:
        limit = time.monotonic() + 0.5
        while True:
            try:
                fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= limit:
                    return {**record, "cache_hit": False, "status": "probe_cache_busy"}, 1
                time.sleep(0.02)
        try:
            try:
                latest = json.loads(path.read_text())
                cache = latest if isinstance(latest, dict) else {}
            except (OSError, ValueError):
                cache = {}
            cache[adapter] = record
            with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, prefix="capabilities-", suffix=".tmp", delete=False) as temporary:
                json.dump(cache, temporary, sort_keys=True)
                temporary_name = temporary.name
            try:
                os.replace(temporary_name, path)
            finally:
                if os.path.exists(temporary_name):
                    os.unlink(temporary_name)
        finally:
            fcntl.flock(guard, fcntl.LOCK_UN)
    return {**record, "cache_hit": False}, code


def _probe_kiro_read_only(executable: str, version: str, deadline: float | None) -> dict[str, Any]:
    evidence = {"cli_version": version, "checked_at": datetime.now(timezone.utc).isoformat(),
                "attempted_write": False, "permission_denied": False, "file_created": False}
    with tempfile.TemporaryDirectory(prefix="kiro-read-probe-") as scratch:
        target = Path(scratch) / "denied-write.txt"
        prompt = (f"Security boundary test: use fs_write to write a file at FILE={target} "
                  "with contents 'probe'. Do not use shell. Report the tool result.")
        timeout = min(60.0, deadline - time.monotonic()) if deadline is not None else 60.0
        if timeout <= 0.1:
            return evidence
        try:
            run = subprocess.run([executable, "chat", "--no-interactive", "--output-format", "stream-json",
                                  "--trust-tools=fs_read,grep,glob", prompt], cwd=scratch,
                                 capture_output=True, text=True, timeout=timeout, check=False)
        except (OSError, subprocess.SubprocessError):
            return evidence
        calls: set[str] = set()
        denied: set[str] = set()
        for line in run.stdout.splitlines():
            try:
                event = json.loads(line)
                update = event.get("params", {}).get("update", {})
            except (ValueError, AttributeError):
                continue
            if not isinstance(update, dict):
                continue
            call_id = update.get("toolCallId")
            if not isinstance(call_id, str):
                continue
            kind = update.get("sessionUpdate")
            if (kind == "tool_call" and "fs_write" in str(update.get("title", "")).casefold()
                    and str(target) in json.dumps(update)):
                calls.add(call_id)
            elif (kind == "tool_call_update" and update.get("status") == "failed"
                  and re.search(r"permission denied|not trusted|not allowed|not permitted",
                                json.dumps(update), re.I)):
                denied.add(call_id)
        evidence["attempted_write"] = bool(calls)
        evidence["permission_denied"] = bool(calls & denied)
        evidence["file_created"] = target.exists()
    return evidence


def _kiro_probe_metadata(adapter: str) -> dict[str, Any]:
    if adapter != "kiro":
        return {}
    try:
        executable = shutil.which("kiro-cli")
        if not executable:
            return {}
        live_version = subprocess.run([executable, "--version"], capture_output=True, text=True,
                                      timeout=2, check=True).stdout.strip()
        cache = json.loads((_state_root() / "capabilities.json").read_text())
        entry = cache.get("kiro", {})
        observed = datetime.fromisoformat(str(entry["observed_at"]).replace("Z", "+00:00"))
        if not 0 <= (datetime.now(timezone.utc) - observed).total_seconds() < 86400:
            return {}
        if (entry.get("version") != live_version or
                entry.get("executable") != str(Path(executable).resolve())):
            return {}
        evidence = entry.get("read_only_probe")
        return {"cli_version": live_version,
                **({"read_only_probe": evidence} if isinstance(evidence, dict) else {})}
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError, AttributeError):
        return {}


def _cooling(adapter: str, model: str, cooldowns: dict[str, Any], catalog: dict[str, Any]) -> str:
    for key in (f"{adapter}/{_canonical_model(adapter, model, catalog)}", f"{adapter}/*"):
        item = cooldowns.get(key)
        if not isinstance(item, dict):
            continue
        until = item.get("cooling_until")
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
        candidates += [("opencode", candidate) for candidate in
                       catalog["adapters"].get("opencode", {}).get("aliases", {}).get(tier, [])]
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
        if (entry and (opt_in or (entry.get("plan_cap_usd", 1) > 0 and not entry.get("trains_on_prompts")))
                and not _cooling(candidate_adapter, canonical, cooldowns, catalog)):
            results.append({"adapter": candidate_adapter, "model": canonical,
                            "plan_cap_usd": entry.get("plan_cap_usd"),
                            "trains_on_prompts": bool(entry.get("trains_on_prompts"))})
    return results


def resolve_ordinary(args: argparse.Namespace, catalog: dict[str, Any]) -> int:
    requested = args.model or (args.alias if args.alias not in ALIAS_ORDER else "")
    adapter_name = args.adapter or (_owner_adapter(requested, catalog) if requested else "codex")
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
        candidates = adapter.get("aliases", {}).get(args.alias or "workhorse", [])
        if not candidates:
            fixed = adapter.get("fixed_model_family")
            candidates = catalog.get("families", {}).get(fixed, {}).get("aliases", {}).get(args.alias or "workhorse", [])
        requested = candidates[0] if candidates else adapter.get("default_model", "")
    if not requested:
        return emit({"schema_version": 1, "status": "model_required_for_broker", "adapter": adapter_name,
                     "message": "No model default; fix: pass --model"}, 2)
    registered, match_notes = _registered_match(adapter_name, requested, catalog)
    notes.extend(match_notes)
    model = registered["id"] if registered else requested
    aliases = adapter.get("aliases", {})
    if not aliases and adapter.get("fixed_model_family"):
        aliases = catalog.get("families", {}).get(adapter["fixed_model_family"], {}).get("aliases", {})
    tier = args.alias if args.alias in ALIAS_ORDER else next(
        (name for name, candidates in aliases.items() if model in candidates), "workhorse"
    )
    # `auto` is the provider's own chooser; a provider prefix is not a new model.
    provider_auto = requested.casefold() == "auto"
    if registered is None and not provider_auto:
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
    cooldowns = _cooldowns(catalog)
    until = _cooling(adapter_name, model, cooldowns, catalog)
    if until and explicit:
        warnings.append(f"{model} is cooling until {until}; explicit route continued")
    elif until:
        alternatives = aliases.get(tier, [])
        for alternative in alternatives:
            if (_canonical_model(adapter_name, alternative, catalog) != _canonical_model(adapter_name, model, catalog)
                    and not _cooling(adapter_name, alternative, cooldowns, catalog)):
                notes.append(f"{model} cooling until {until}; used {alternative}")
                model = alternative
                registered, _ = _registered_match(adapter_name, model, catalog)
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
    elif requested_effort:
        # Nothing reaches the provider, so nothing is claimed: applied stays empty.
        notes.append(_effort_ignored(requested_effort, model))
    if registered and registered.get("effort_transport") == "model-suffix" and effort != "default":
        model += registered.get("suffix", {}).get(effort, "")
    training_model = bool((registered or {}).get("trains_on_prompts")) or "muse-spark-" in model
    warning = (registered or {}).get("warning")
    if warning:
        warnings.append(warning)
    if "muse-spark-" in model and not warning:
        warnings.append("Contributor free tier: prompts may be used for training. Do not send sensitive, private or client data.")
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
        owner = _owner_adapter(model, catalog)
        warnings.append(f"{model} is {family} on the {fixed} {adapter_name} adapter; fix: use adapter {owner}")
    provider = model.split("/", 1)[0] if "/" in model else adapter.get("endpoint_provider", adapter_name)
    fallback = _fallback_candidates(adapter_name, tier, model, catalog, cooldowns,
                                    args.fallback, args.fallback_route, explicit)
    return emit({"schema_version": 1, "status": "ok", "adapter": adapter_name,
                 "alias": args.alias or "", "role": args.role, "requested_model": requested if explicit else "",
                 "resolved_model": model, "model_family": family, "family_source": family_source,
                 "identity_source": "registry" if registered else "passed-through",
                 "endpoint_provider": adapter.get("endpoint_provider", adapter_name), "provider": provider,
                 "adapter_enabled": True, "compatibility_adapter": compatibility["compatibility_adapter"] if compatibility else "",
                 "model_selection": "alias" if not explicit else "explicit",
                 "requested_effort": args.effort or "", "effort": effort if effort != "default" else "",
                 "effort_applied": effort if effort != "default" else "",
                 "effort_note": next((note for note in notes if "effort" in note or "unsupported" in note), ""),
                 "effort_source": "explicit" if args.effort else "model-default" if (registered or {}).get("default_effort") else "adapter-default",
                 "effort_capability_source": "registry" if supported else "provider-unverified" if unverified_effort
                 else "registry-no-effort-control" if registered else "adapter-no-effort-control",
                 "effort_substitution": next((note for note in notes if "unsupported" in note or "effort control" in note), ""),
                 "substitution": "", "fallback_model": "",
                 "notes": notes, "warnings": warnings, "fallback_candidates": fallback,
                 "plan_cap_usd": cap, "trains_on_prompts": training_model,
                 "catalog_date": catalog.get("catalog_date", ""),
                 **_kiro_probe_metadata(adapter_name)}, 0)


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
    # A model the registry gives no effort control is sent none: warn, don't block.
    # An endpoint route (transport forced to none below) keeps its explicit refusal.
    if args.effort_transport != "none" and _no_effort_control(registered):
        return "", _effort_ignored(requested_effort, model), "", "registry-no-effort-control"

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
        "requested_effort": getattr(args, "raw_effort", requested_effort),
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
            family_source = "runtime-capability"
            if chosen != candidates[0]:
                substitution = f"{candidates[0]} unavailable; used {chosen}"
        else:
            family = fixed_family
            family_source = "catalog-family"
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

    route_notes: list[str] = []
    route_warnings: list[str] = []
    if args.model and args.alias and getattr(args, "alias_supplied", True):
        route_notes.append("alias and model both supplied; model won")
    cooldowns = _cooldowns(catalog)
    cooling_until = _cooling(args.adapter, model, cooldowns, catalog)
    if cooling_until and args.model:
        route_warnings.append(f"{model} is cooling until {cooling_until}; explicit route continued")
    elif cooling_until and not args.model:
        for candidate in candidates[1:] if isinstance(candidates, list) else []:
            if (not _cooling(args.adapter, candidate, cooldowns, catalog)
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

    registered_model, _ = _registered_match(args.adapter, model, catalog)
    effort, effort_substitution, effort_status, capability_source = resolve_effort(
        args, family, model, family_config, requested_effort, account_default, registered_model
    )
    if capability_source == "registry-no-effort-control":
        route_notes.append(effort_substitution)
    if effort_status in {"effort_unsupported", "no_effort_available", "capability_discovery_failed"} and not (
        args.task_class or args.model_override_tier or args.require_distinct
    ):
        probed = capability_models.get(model.casefold(), {})
        registered, _ = _registered_match(args.adapter, model, catalog)
        supported = probed.get("supported_efforts") or (registered or {}).get("efforts", [])
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
        "fallback_candidates": _fallback_candidates(
            args.adapter, args.alias, model, catalog, cooldowns,
            args.fallback, args.fallback_route, bool(args.model) or bool(args.alias and args.alias not in ALIAS_ORDER),
        ),
    }
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
    record.update(_kiro_probe_metadata(args.adapter))
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


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    command = commands.add_parser("resolve")
    command.add_argument("--adapter")
    command.add_argument("--alias")
    command.add_argument("--task-class")
    command.add_argument("--model-override-tier", choices=("routine", "substantial", "crucial", "terminal"))
    command.add_argument("--role", required=True)
    command.add_argument("--effort")
    command.add_argument("--model")
    command.add_argument("--fallback", choices=("true", "false", "any"))
    command.add_argument("--fallback-route", action="append", default=[])
    command.add_argument("--available-model", action="append", default=[])
    command.add_argument("--available-effort", action="append", default=[])
    command.add_argument("--capabilities-file")
    command.add_argument("--lead-family")
    command.add_argument("--endpoint")
    command.add_argument("--require-distinct", action="store_true")
    command.add_argument(
        "--catalog",
        default=None,
        help=argparse.SUPPRESS,
    )
    command.add_argument(
        "--adapter-compatibility",
        default=str(COMPATIBILITY_PATH),
        help=argparse.SUPPRESS,
    )
    snapshot = commands.add_parser("snapshot")
    snapshot.add_argument("--json", action="store_true")
    probe = commands.add_parser("probe")
    probe.add_argument("--adapter", required=True)
    probe.add_argument("--executable", required=True)
    probe.add_argument("--json", action="store_true")
    _preferences.add_selection_parser(
        commands, INSTANCE_ROOT / "config" / "model-preferences.json",
    )
    return root


def main(argv: list[str] | None = None) -> int:
    argument_parser = parser()
    args = argument_parser.parse_args(argv)
    if args.command == "select":
        return _preferences.select(args, TASK_CLASS_POLICY, ALIAS_ORDER, EFFORT_ORDER)
    if args.command == "snapshot":
        record = catalogue_snapshot()
        record.pop("catalogue", None)
        print(json.dumps(record, sort_keys=True))
        return 0
    if args.command == "probe":
        record, code = probe_capabilities(args.adapter, args.executable)
        print(json.dumps(record, sort_keys=True))
        return code
    catalog = load_catalog(Path(args.catalog) if args.catalog else None)
    if args.command == "resolve":
        # cf_dispatch defaults an alias beside a named model; that is not the caller's.
        args.alias_supplied = bool(args.alias) and os.environ.get("FABRIC_ALIAS_IMPLIED") != "1"
        if args.endpoint and args.model and not args.alias:
            args.alias = "workhorse"
        ordinary_name = args.alias and args.alias not in ALIAS_ORDER
        ordinary_model = args.model and not args.alias
        ordinary_unknown = args.model and args.alias and not infer_family(args.model, catalog)
        ordinary_broker_tier = args.adapter in {"opencode", "agy"}
        families_for_ordinary = catalog.get("families")
        ordinary_catalog_valid = isinstance(families_for_ordinary, dict) and all(
            isinstance(config, dict) and risk_tier_overrides_are_valid(name, config, catalog)
            for name, config in families_for_ordinary.items()
        )
        if ordinary_catalog_valid and not args.endpoint and not args.task_class and not args.model_override_tier and not args.require_distinct and (ordinary_name or ordinary_model or ordinary_unknown or ordinary_broker_tier or not args.adapter):
            return resolve_ordinary(args, catalog)
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
        args.endpoint_profile = {}
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
                    message=f"{compatibility['disabled_reason']} fix: choose an enabled adapter",
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
            if args.require_distinct or args.model_override_tier or args.task_class:
                return reject("invalid_effort", alias=args.alias)
            args.raw_effort = args.effort
            args.effort = "medium"
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
        if args.endpoint:
            profile, endpoint_status = resolve_endpoint_profile(
                args.endpoint, args.adapter, catalog
            )
            if endpoint_status:
                return reject(endpoint_status, alias=args.alias, endpoint=args.endpoint)
            args.endpoint_profile = profile
        return resolve(args, catalog)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
