"""Fallback policy over router-owned candidates, never a second catalogue merger."""

import json
import os
from pathlib import Path
import subprocess
import sys

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


def candidates(plan, policy=None, catalogue=None):
    """Return ordered route requests. Explicit lists may opt into training/free routes."""
    explicit_model = bool(plan.get("requested_model"))
    if policy is None:
        policy = not explicit_model
    if isinstance(policy, str) and policy not in {"any"}:
        try:
            policy = json.loads(policy)
        except ValueError:
            raise ValueError("fallback must be false, true, any or a JSON route list")
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
    seen = {(adapter, plan["model"])}
    answer = []
    for item in raw:
        if isinstance(item, str):
            item = {"adapter": adapter, "model": item}
        if not isinstance(item, dict):
            continue
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
                "effort": item.get("effort", plan.get("effort")),
            }
        )
    return answer


def cooling(adapter, model, records=None):
    records = read_cooldowns() if records is None else records
    return records.get(adapter + "/" + model) or records.get(adapter + "/*")
