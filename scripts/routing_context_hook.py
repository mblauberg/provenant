#!/usr/bin/env python3
"""Claude hooks for routing guidance and a soft model-roster warning."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]


def read_config(path=None):
    path = Path(path or os.environ.get("PROVENANT_ROUTING_CONFIG", ROOT / "config/model-routing.json"))
    return json.loads(path.read_text(encoding="utf-8"))


def routing_context(config):
    routes = config.get("task_class_routes", {})
    rows = []
    for name, route in routes.items():
        if not isinstance(route, dict):
            continue
        models = [f"{adapter}/{model}" for adapter, values in (route.get("models") or {}).items()
                  for model in values if isinstance(model, str)]
        rows.append(f"{name}: {route.get('alias', 'unset')} [{', '.join(models)}] ({route.get('description', '')})")
    tiers = "\n".join(rows)
    size = config.get("task_classification", {}).get("size_table", [
        {"label": "small", "files": "1-5", "lines": "up to 200"},
        {"label": "medium", "files": "6-20", "lines": "201-1000"},
        {"label": "large", "files": "over 20", "lines": "over 1000"},
    ])
    return "Task routing is advisory. Class and size determine tier; role is a label.\nTier roster:\n" + tiers + "\nSize guide:\n" + json.dumps(size, ensure_ascii=False)


def allowed_models(config, role):
    allowed = set()
    defaults = config.get("task_classification", {}).get("role_defaults", {})
    alias = defaults.get(role)
    if alias is None and role == "scout":
        alias = defaults.get("junior")
    for entry in config.get("routes", {}).get(alias, []):
        if isinstance(entry, dict) and isinstance(entry.get("model"), str):
            allowed.add(entry["model"].casefold())
            allowed.add(entry["model"].split("/", 1)[-1].casefold())
    for route in config.get("task_class_routes", {}).values():
        if not isinstance(route, dict) or route.get("role", "worker") != role:
            continue
        for models in (route.get("models") or {}).values():
            allowed.update(model.casefold() for model in models if isinstance(model, str))
    return allowed


def soft_warning(config, event):
    tool = event.get("tool_name") or event.get("name") or ""
    if tool != "Agent" and not str(tool).endswith("fabric_dispatch"):
        return None
    values = event.get("tool_input") or event.get("input") or {}
    role = values.get("role") or values.get("route_role")
    model = values.get("model") or values.get("model_name")
    if not role or not model:
        return None
    allowed = allowed_models(config, role)
    normalized = str(model).casefold()
    if any(normalized == entry or normalized.endswith("/" + entry) for entry in allowed):
        return None
    return f"Routing advisory: model {model!r} is outside the configured roster for role {role!r}; confirm the task class and size."


def main(argv=None):
    config = read_config()
    event = json.loads(sys.stdin.read() or "{}")
    if "--pre-tool" in (argv or sys.argv[1:]):
        warning = soft_warning(config, event)
        if warning:
            print(json.dumps({"systemMessage": warning}))
        return 0
    context = routing_context(config)
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": context}}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
