#!/usr/bin/env python3
"""Probe provider model capabilities and validate capability snapshots."""

from __future__ import annotations

import fcntl
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
from typing import Any

import model_route_catalog as _catalog_validation

EFFORT_ORDER = _catalog_validation.EFFORT_ORDER
family_alias_candidates = _catalog_validation.family_alias_candidates
capability_key_matches_model = _catalog_validation.capability_key_matches_model
TRUSTED_CAPABILITY_SOURCES = {
    "codex debug models": "codex",
    "claude subscription canary": "claude",
    "agy models": "agy",
}
OPENCODE_REFRESH_FAILURE_TTL = 30


def _state_root() -> Path:
    return Path(os.environ.get("AGENT_FABRIC_STATE_ROOT", str(Path.home() / ".local/state/agent-harness/fabric")))


def _listed_models(raw: str, adapter: str | None = None) -> list[str]:
    """Accept model-list shapes, never arbitrary help or error text."""
    try:
        data = json.loads(raw)
    except ValueError:
        data = None
    if data is not None and adapter not in {None, "codex", "kiro"}:
        return []
    if isinstance(data, dict):
        data = data.get("models", data.get("data"))
    if isinstance(data, dict):
        if not all(isinstance(value, dict) for value in data.values()):
            return []
        data = list(data)
    if isinstance(data, list):
        models = [item if isinstance(item, str) else
                  item.get("id", item.get("model_id", item.get("slug", item.get("name")))) if isinstance(item, dict) else None
                  for item in data]
        if all(isinstance(model, str) and re.fullmatch(r"[A-Za-z0-9_./:~-]+", model) for model in models):
            return list(dict.fromkeys(models))
        return []
    if adapter in {"codex", "kiro"}:
        return []  # These commands promise structured JSON.
    listed: list[str] = []
    gap = False
    for line in raw.splitlines():
        line = line.strip()
        if not line:
            gap = bool(listed)
            continue
        if line in {"Fetching available models...", "Available models", "Available models:", "Models:"} or line.startswith("Tip: use --model "):
            continue
        # IDs alone, or an ID followed by a display-name column.
        match = re.fullmatch(r"(?:[-*]\s+)?([A-Za-z](?:[A-Za-z0-9._/~-]*[A-Za-z0-9])?(?::[A-Za-z0-9._~-]+)*)(?:(?:\t| {2,}| - )\S.*)?", line)
        if not match or (match[1] != "auto" and not re.search(r"[-/.\d]", match[1])):
            # Prose after the IDs and a blank line is a footer; anywhere else it is an error or notice.
            if gap:
                break
            return []
        if adapter == "opencode" and "/" not in match[1]:
            return []
        listed.append(match[1])
        gap = False
    return list(dict.fromkeys(listed))


def _kiro_probe_enforced(evidence: Any) -> bool:
    return (isinstance(evidence, dict) and evidence.get("attempted_write") is True
            and evidence.get("permission_denied") is True and evidence.get("file_created") is False)


def probe_capabilities(adapter: str, executable: str, deadline: float | None = None,
                       listing_only: bool = False, refresh: bool = False,
                       env: dict[str, str] | None = None,
                       cache_failures: bool = True) -> tuple[dict[str, Any], int]:
    """Probe an adapter CLI. `listing_only` (discovery) lists models without Kiro's
    model-backed safety chat, and keeps safety evidence cached for the same executable
    and version; a full probe re-runs the chat once that evidence has aged out."""
    commands = {"opencode": ["models"], "cursor": ["models"], "agy": ["models"],
                "kiro": ["chat", "--list-models", "--format", "json"],
                "codex": ["debug", "models"]}
    # agy and opencode refresh their lists over the network and routinely take several seconds.
    listing_timeout = 20.0
    if adapter not in commands:
        return {"status": "unsupported_adapter", "adapter": adapter}, 2
    executable = shutil.which(executable, path=env.get("PATH") if env is not None else None) or executable
    def remaining(limit: float) -> float:
        return max(0.01, min(limit, deadline - time.monotonic())) if deadline is not None else limit
    path = _state_root() / "capabilities.json"
    try:
        cache = json.loads(path.read_text())
        if not isinstance(cache, dict):
            cache = {}
    except (OSError, ValueError):
        cache = {}
    previous = cache.get(adapter)
    executable_path = str(Path(executable).resolve())
    last_good = None
    refresh_failure_cached = False
    cache_superseded = False
    if (cache_failures and (listing_only or adapter == "opencode")
            and isinstance(previous, dict) and previous.get("listing_format") == 1
            and previous.get("executable") == executable_path
            and previous.get("status") != "probe_unavailable" and previous.get("models")):
        last_good = {**previous, "cache_hit": False,
                     "refresh_failed": "Model list refresh failed; using the last good listing"}
    if refresh and adapter == "opencode" and last_good is not None:
        try:
            failed_at = datetime.fromisoformat(
                str(previous.get("refresh_failed_at", "")).replace("Z", "+00:00")
            )
            if 0 <= (datetime.now(timezone.utc) - failed_at).total_seconds() < OPENCODE_REFRESH_FAILURE_TTL:
                return {**last_good, "cache_hit": True}, 0
        except (ValueError, TypeError):
            pass
    try:
        version = subprocess.run([executable, "--version"], capture_output=True, text=True,
                                 timeout=remaining(5), check=True, env=env).stdout.strip()
        if not re.search(r"\d+\.\d+", version):
            raise ValueError("CLI did not report a version")
    except (OSError, ValueError, subprocess.SubprocessError):
        if last_good is not None:
            return last_good, 0
        return {"status": "probe_unavailable", "adapter": adapter,
                "message": "CLI version unavailable; fix: check the executable"}, 1
    safety_missing = adapter == "kiro" and not listing_only and isinstance(previous, dict) \
        and "read_only_probe" not in previous and previous.get("status") != "probe_unavailable"
    if (isinstance(previous, dict) and previous.get("version") == version and not refresh
            and (previous.get("listing_format") == 1 or previous.get("status") == "probe_unavailable")
            and previous.get("executable") == executable_path and not safety_missing):
        try:
            # A full Kiro probe is only as fresh as its safety evidence, which a listing refresh keeps.
            evidence = previous.get("read_only_probe")
            stamp = evidence["checked_at"] if (adapter == "kiro" and not listing_only and isinstance(evidence, dict)
                                               and "checked_at" in evidence) else previous["observed_at"]
            observed = datetime.fromisoformat(str(stamp).replace("Z", "+00:00"))
            ttl = 3600 if (previous.get("status") == "probe_unavailable" or
                           (adapter == "opencode" and not previous.get("models")) or
                           (adapter == "kiro" and not _kiro_probe_enforced(previous.get("read_only_probe")))) else 86400
            if (datetime.now(timezone.utc) - observed).total_seconds() < ttl:
                return {**previous, "cache_hit": True}, 0 if previous.get("status") != "probe_unavailable" else 1
        except (ValueError, KeyError, TypeError):
            pass
    try:
        listing = subprocess.run([executable, *commands[adapter]], capture_output=True, text=True,
                                 timeout=remaining(listing_timeout), check=True, env=env).stdout
        models = _listed_models(listing, adapter)
        if not models:
            raise ValueError("Malformed or empty model listing")
        help_text = subprocess.run([executable, "--help"], capture_output=True, text=True,
                                   timeout=remaining(3), check=False, env=env).stdout
    except (OSError, ValueError, subprocess.SubprocessError):
        if last_good is not None:
            if adapter == "opencode" and refresh:
                # Keep the last good listing and briefly cache the failed refresh.
                record = {**previous, "refresh_failed_at": datetime.now(timezone.utc).isoformat()}
                refresh_failure_cached = True
                code = 0
            else:
                return last_good, 0
        else:
            record = {"status": "probe_unavailable", "adapter": adapter, "version": version,
                      "executable": executable_path,
                      "observed_at": datetime.now(timezone.utc).isoformat(), "models": [],
                      "message": "Model list unavailable; fix: check CLI authentication"}
            code = 1
    else:
        record = {"adapter": adapter, "version": version, "executable": executable_path,
                  "listing_format": 1,
                  "observed_at": datetime.now(timezone.utc).isoformat(),
                  "probed_flags": sorted(set(re.findall(r"--[a-z][a-z-]+", help_text))),
                  "models": models}
        if adapter == "kiro" and not listing_only:
            record["read_only_probe"] = _probe_kiro_read_only(executable, version, deadline, env)
        elif (adapter == "kiro" and isinstance(previous, dict) and previous.get("version") == version
              and previous.get("executable") == executable_path and "read_only_probe" in previous):
            record["read_only_probe"] = previous["read_only_probe"]
        code = 0
    if code and not cache_failures:
        return {**record, "cache_hit": False}, code
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
            latest_record = cache.get(adapter)
            if (refresh_failure_cached and isinstance(latest_record, dict)
                    and latest_record.get("listing_format") == 1
                    and latest_record.get("executable") == executable_path
                    and latest_record.get("models")):
                try:
                    previous_at = datetime.fromisoformat(str(previous.get("observed_at", "")).replace("Z", "+00:00"))
                    latest_at = datetime.fromisoformat(str(latest_record.get("observed_at", "")).replace("Z", "+00:00"))
                    if latest_at > previous_at:
                        record = latest_record
                        refresh_failure_cached = False
                        cache_superseded = True
                except (ValueError, TypeError):
                    pass
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
    result = {**record, "cache_hit": cache_superseded}
    if refresh_failure_cached:
        result["refresh_failed"] = last_good["refresh_failed"]
    return result, code


def _probe_kiro_read_only(executable: str, version: str, deadline: float | None,
                         env: dict[str, str] | None = None) -> dict[str, Any]:
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
                                 capture_output=True, text=True, timeout=timeout, check=False, env=env)
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

