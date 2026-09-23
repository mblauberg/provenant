"""Fallback policy over router-owned candidates, never a second catalogue merger."""

import json
import importlib.util
import os
from pathlib import Path
import subprocess
import sys

try:
    from .fabric_records import read_cooldowns
except ImportError:
    from fabric_records import read_cooldowns


def snapshot():
    product = Path(
        os.environ.get("AGENT_FABRIC_PRODUCT_ROOT")
        or Path(__file__).resolve().parents[3]
    )
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(product / "scripts/model_route.py"),
                "snapshot",
                "--json",
            ],
            capture_output=True,
            text=True,
            timeout=15,
        )
        data = json.loads(result.stdout)
        return (
            data
            if isinstance(data, dict) and data.get("schema") == "fabric.catalogue.v1"
            else {}
        )
    except (OSError, ValueError, subprocess.SubprocessError):
        return {}


ADAPTERS = {"claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"}


def parse_candidate(item, adapter):
    if isinstance(item, str):
        model, separator, effort = item.partition("@")
        prefix, slash, rest = model.partition("/")
        if slash and prefix in ADAPTERS:
            adapter, model = prefix, rest
        item = {"adapter": adapter, "model": model}
        if separator:
            item["effort"] = effort
    if not isinstance(item, dict):
        raise ValueError("fallback entries must be adapter/model[@effort] or route objects")
    model = item.get("model") or item.get("resolved_model") or item.get("id")
    if (not isinstance(model, str) or not model.strip()
            or not isinstance(item.get("adapter", adapter), str)
            or item.get("adapter", adapter) not in ADAPTERS):
        raise ValueError("fallback entry requires an adapter and non-empty model")
    if item.get("effort") is not None and not isinstance(item["effort"], str):
        raise ValueError("fallback effort must be a string")
    return dict(item, adapter=item.get("adapter", adapter), model=model)


def validate_policy(policy):
    if isinstance(policy, str) and policy != "any":
        try:
            policy = json.loads(policy)
        except ValueError:
            raise ValueError("fallback must be false, true, any or a JSON route list") from None
    if policy is None or type(policy) is bool or policy == "any":
        return policy
    if not isinstance(policy, list):
        raise ValueError("fallback must be false, true, any or a JSON route list")
    for item in policy:
        parse_candidate(item, "codex")
    return policy


def registered_model(adapter, model, catalogue=None, warnings=None):
    """Cooldown identity: adapter plus registered id, never an effort selector."""
    model = model.partition("@")[0]
    if model == "*":
        return model
    catalogue = snapshot() if catalogue is None else catalogue
    try:
        if adapter not in catalogue.get("adapters", {}):
            return model
        # The router owns alias, retired-name and provider-suffix resolution.
        module = _model_route_module()
        entry, _ = module._registered_match(adapter, model, catalogue)
        return entry["id"] if entry else model
    except (KeyError, AttributeError, TypeError, ValueError) as exc:
        warning = f"catalogue malformed for {adapter}/{model}; using raw model ({type(exc).__name__})"
        if warnings is not None:
            warnings.append(warning)
        else:
            print(warning, file=sys.stderr)
        return model


def _model_route_module():
    product = Path(os.environ.get("AGENT_FABRIC_PRODUCT_ROOT") or Path(__file__).resolve().parents[3])
    spec = importlib.util.spec_from_file_location("fabric_model_route", product / "scripts/model_route.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def model_families(model, catalogue=None):
    """Return catalogue-pattern families for a model id, preserving ambiguity."""
    catalogue = snapshot() if catalogue is None else catalogue
    raw_catalogue = catalogue.get("catalogue", {})
    try:
        route = _model_route_module()
        if not isinstance(raw_catalogue, dict) or "model_patterns" not in raw_catalogue:
            raw_catalogue = route.load_catalog()
        return route.matching_model_families(model, raw_catalogue)
    except (AttributeError, KeyError, TypeError, ValueError):
        return ()



def candidates(plan, policy=None, catalogue=None):
    """Return ordered route requests. Explicit lists may opt into training/free routes."""
    explicit_model = bool(plan.get("requested_model"))
    if policy is None:
        policy = not explicit_model
    policy = validate_policy(policy)
    if policy is False:
        return []
    catalogue = catalogue if catalogue is not None else snapshot()
    route = plan["route"]
    adapter = plan["adapter"]
    alias = route.get("alias") or "workhorse"
    adapters = catalogue.get("adapters", {})
    if isinstance(adapters, list):
        adapters = {
            item.get("name", item.get("adapter", item.get("id"))): item
            for item in adapters
            if isinstance(item, dict)
        }
    raw = []
    explicit = isinstance(policy, list)
    if explicit:
        raw = policy
    elif "fallback_candidates" in route:
        raw = route["fallback_candidates"] if isinstance(route["fallback_candidates"], list) else []
    else:
        raw = list(route.get("candidates") or [])
        if route.get("fallback_model"):
            raw.append(route["fallback_model"])
        adapter_config = adapters.get(adapter, {})
        raw += adapter_config.get("aliases", {}).get(alias, [])
        # Cross-adapter aliases are resolved by the same CLI on the next attempt.
        if adapter != "opencode" and adapters.get("opencode"):
            cross = adapters["opencode"]
            cross_models = (
                cross.get("aliases", {}).get(alias)
                or cross.get("aliases", {}).get("workhorse")
                or [cross.get("default_model")]
            )
            raw += [
                {"adapter": "opencode", "model": model}
                for model in cross_models
                if model
            ]
    models = catalogue.get("models", {})
    if isinstance(models, list):
        models = {item.get("id"): item for item in models if isinstance(item, dict)}
    if not isinstance(models, dict):
        models = {}
    seen = {(adapter, plan["model"])}
    answer = []
    for item in raw:
        try:
            item = parse_candidate(item, adapter)
        except (ValueError, TypeError):
            if explicit:
                raise
            continue  # Router drift must not strand a finished attempt.
        candidate_adapter = item.get("adapter", adapter)
        model = item.get("model") or item.get("resolved_model") or item.get("id")
        if (
            not isinstance(model, str)
            or not model
            or candidate_adapter
            not in {"claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"}
        ):
            continue
        if (candidate_adapter, model) in seen:
            continue
        seen.add((candidate_adapter, model))
        meta = models.get(model, {})
        if not isinstance(meta, dict):
            meta = {}
        training = item.get("trains_on_prompts", meta.get("trains_on_prompts", False))
        free = (
            item.get("plan_cap_usd", meta.get("plan_cap_usd")) == 0
            or (model.endswith("-free") or model.endswith(":free"))
            or model.startswith("opencode/")
        )
        if not explicit and policy != "any" and (training or free):
            continue
        answer.append(
            {
                "adapter": candidate_adapter,
                "model": model,
                "effort": item.get("effort_applied", item.get("effort", plan.get("effort"))),
            }
        )
    return answer


def cooling(adapter, model, records=None):
    records = read_cooldowns() if records is None else records
    model = registered_model(adapter, model)
    return records.get(adapter + "/" + model) or records.get(adapter + "/*")
