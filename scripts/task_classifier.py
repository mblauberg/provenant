#!/usr/bin/env python3
"""Advisory, configurable task-classification chain."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CONFIG = ROOT / "config/model-routing.json"


def _state_dir() -> Path:
    return Path(os.environ.get("AGENT_FABRIC_STATE_ROOT", Path.home() / ".local/state/agent-harness/fabric"))


def _classes(config: dict) -> dict:
    routes = config.get("task_class_routes", {})
    return {name: {"name": name, "description": route.get("description", "")}
            for name, route in routes.items() if isinstance(name, str) and isinstance(route, dict)}


def _rule_matches(when: dict, context: dict) -> bool:
    if when.get("mode") and when["mode"] != context.get("mode"):
        return False
    if when.get("role") and when["role"] != context.get("role"):
        return False
    prompt = str(context.get("prompt", "")).casefold()
    if when.get("prompt_any") and not any(str(term).casefold() in prompt for term in when["prompt_any"]):
        return False
    paths = [str(path).casefold() for path in context.get("paths", [])]
    if when.get("path_any") and not any(term.casefold() in path for term in when["path_any"] for path in paths):
        return False
    size = context.get("size")
    for key, op in (("min_files", lambda a, b: a >= b), ("max_files", lambda a, b: a <= b),
                    ("min_lines", lambda a, b: a >= b), ("max_lines", lambda a, b: a <= b)):
        metric = "files" if "files" in key else "lines"
        if key in when and (not isinstance(size, dict) or metric not in size or
                            not op(size.get(metric, -1), when[key])):
            return False
    return True


def _run_rules(config: dict, context: dict) -> dict | None:
    for rule in config.get("task_classification", {}).get("rules", []):
        if isinstance(rule, dict) and isinstance(rule.get("when"), dict) and _rule_matches(rule["when"], context):
            name = rule.get("class")
            if name in _classes(config):
                return {"class": name, "confidence": float(rule.get("confidence", 1.0)),
                        "reason": str(rule.get("reason", "matched configured rule"))}
    return None


def _run_model(config: dict, context: dict, *, utility=None, timeout=None) -> dict | None:
    options = config.get("task_classification", {}).get("model", {})
    choices = _classes(config)
    if not choices or not str(context.get("prompt", "")).strip():
        return None
    # Keep the utility request limited to the task summary and class descriptions.
    payload = {"task": str(context.get("prompt", "")), "classes": list(choices.values())}
    prompt = json.dumps(payload, sort_keys=True, ensure_ascii=False)
    digest = hashlib.sha256(prompt.encode()).hexdigest()
    cache = _state_dir() / "task-classifier" / f"{digest}.json"
    try:
        if cache.is_file():
            result = json.loads(cache.read_text(encoding="utf-8"))
        else:
            module = None
            if utility is None:
                spec = importlib.util.spec_from_file_location("provenant_task_classifier_utility", ROOT / "scripts/utility.py")
                module = importlib.util.module_from_spec(spec)
                sys.modules[spec.name] = module
                spec.loader.exec_module(module)
                utility = module.execute
            schema = {"type": "object", "properties": {
                "class": {"type": "string", "enum": list(choices)},
                "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                "reason": {"type": "string"}}, "required": ["class", "confidence", "reason"],
                "additionalProperties": False}
            routed_provider = None
            if module is not None:
                def routed_provider(call_prompt, _route, confidential):
                    return module.provider_call(call_prompt, options.get("alias", "utility"), confidential,
                                                timeout_seconds=timeout or options.get("timeout_s", 20))
            result = utility("extract", prompt, schema=schema,
                             timeout_seconds=timeout or options.get("timeout_s", 20), retry=False,
                             provider=routed_provider)
            if not isinstance(result, dict):
                return None
            result = {"class": result.get("class"), "confidence": result.get("confidence"),
                      "reason": result.get("reason", "utility model")}
            cache.parent.mkdir(parents=True, exist_ok=True)
            cache.write_text(json.dumps(result), encoding="utf-8")
        if result.get("class") not in choices or float(result.get("confidence", 0)) < float(options.get("min_confidence", 0.6)):
            return None
        return result
    except Exception as exc:  # classification failures are advisory
        print(f"task-classifier: model stage unavailable: {exc}", file=sys.stderr)
        return None


def classify(config: dict, context: dict | None = None, *, explicit: str | None = None,
             override: str | None = None, environ: dict | None = None, utility=None) -> dict:
    context = dict(context or {})
    env = os.environ if environ is None else environ
    task = config.get("task_classification", {})
    if not context.get("prompt"):
        context["prompt"] = env.get("PROVENANT_TASK_BRIEF", "")
    if not context.get("mode"):
        context["mode"] = env.get("PROVENANT_TASK_MODE", "")
    if not context.get("paths") and env.get("PROVENANT_TASK_PATHS"):
        try:
            paths = json.loads(env["PROVENANT_TASK_PATHS"])
            context["paths"] = paths if isinstance(paths, list) else [str(paths)]
        except ValueError:
            context["paths"] = env["PROVENANT_TASK_PATHS"].split(os.pathsep)
    if not isinstance(context.get("size"), dict):
        context["size"] = {}
    for key, variable in (("files", "PROVENANT_TASK_SIZE_FILES"), ("lines", "PROVENANT_TASK_SIZE_LINES")):
        if context["size"].get(key) is None and env.get(variable, "").isdigit():
            context["size"][key] = int(env[variable])
    chain = [part.strip() for part in (override or env.get("PROVENANT_TASK_CLASSIFY") or ",".join(task.get("chain", ["explicit", "rules", "model", "default"]))).split(",") if part.strip()]
    if explicit:
        chain = ["explicit", *(stage for stage in chain if stage != "explicit")]
    results = {}
    winner = None
    stages = {"explicit": lambda: ({"class": explicit, "confidence": 1.0, "reason": "caller override"} if explicit else None),
              "rules": lambda: _run_rules(config, context),
              "model": lambda: _run_model(config, context, utility=utility)}
    for stage in chain:
        if stage == "default":
            break
        if stage not in stages:
            continue
        try:
            result = stages[stage]()
        except Exception as exc:
            print(f"task-classifier: {stage} stage unavailable: {exc}", file=sys.stderr)
            result = None
        results[stage] = result
        if result:
            winner = {**result, "classified_by": stage}
            break
    else:
        winner = None
    shadow = {}
    for stage in task.get("shadow", []):
        if stage in stages and stage not in results:
            try:
                results[stage] = stages[stage]()
            except Exception as exc:
                results[stage] = None
                print(f"task-classifier: shadow {stage} unavailable: {exc}", file=sys.stderr)
        shadow[stage] = results.get(stage)
    if winner is None:
        name = task.get("default", "implementation")
        winner = {"class": name, "confidence": 0.0, "reason": "configured default", "classified_by": "default"}
        print(f"task-classifier: using default class {name}; check the classification", file=sys.stderr)
    winner["shadow_answers"] = shadow
    size = context.get("size") or {}
    bands = task.get("size_tiers", [])
    matched = [band for band in bands if isinstance(band, dict) and
               ((isinstance(size.get("files"), int) and size["files"] >= band.get("min_files", 0)) or
                (isinstance(size.get("lines"), int) and size["lines"] >= band.get("min_lines", 0)))]
    winner["size_tier"] = matched[-1].get("name") if matched else None
    return winner


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument("--prompt", default="")
    parser.add_argument("--mode", default="")
    parser.add_argument("--role", default="")
    parser.add_argument("--classify", dest="override")
    parser.add_argument("--explicit")
    args = parser.parse_args(argv)
    config = json.loads(args.config.read_text(encoding="utf-8"))
    print(json.dumps(classify(config, {"prompt": args.prompt, "mode": args.mode, "role": args.role},
                              explicit=args.explicit, override=args.override)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
