#!/usr/bin/env python3
"""`provenant profile`: list, inspect, switch and compare harness profiles.

Resolution lives in `skills/_shared/harness_profile.py`; this file only parses
arguments, prints and edits the user or project profile file. The
specification is `docs/specs/harness/profiles.md`.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import sys
from typing import Any


def _load_shared() -> Any:
    path = Path(__file__).resolve().parents[1] / "skills/_shared/harness_profile.py"
    spec = importlib.util.spec_from_file_location("provenant_harness_profile", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


profiles = _load_shared()


def _shown(value: Any) -> str:
    return json.dumps(value)


def _target(args: argparse.Namespace) -> Path:
    return profiles.project_path() if args.project else profiles.user_path()


def _layer_part(document: dict[str, Any], task_class: str | None) -> dict[str, Any]:
    if not task_class:
        return document
    return document.setdefault("task_classes", {}).setdefault(task_class, {})


def command_list(registry: Any, args: argparse.Namespace) -> int:
    if args.json:
        print(json.dumps({"knobs": registry.knobs, "presets": registry.presets}, indent=2))
        return 0
    print("knobs (default = today's behaviour):")
    for name, spec in registry.knobs.items():
        allowed = (" | ".join(spec["values"]) if spec["type"] == "enum"
                   else f"{spec['min']}..{spec['max']}" + (" | null" if spec.get("nullable") else "")
                   if spec["type"] == "int" else "true | false")
        floor = "".join(f" floor {tier}>={_shown(value)}" for tier, value in spec.get("floor", {}).items())
        print(f"  {name}  [{spec['status']}]  default {_shown(spec['default'])}  ({allowed}){floor}")
        print(f"      {spec['effect']}")
        print(f"      trade-off: {spec['trade_off']}")
    print("presets:")
    for name, preset in registry.presets.items():
        print(f"  {name}: {preset.get('description', '')}")
    return 0


def command_show(registry: Any, args: argparse.Namespace) -> int:
    if not args.resolved:
        layers = {"user": profiles.user_path(), "project": profiles.project_path()}
        report = {name: {"path": str(path), "profile": profiles.read_layer(registry, path)}
                  for name, path in layers.items()}
        if args.json:
            print(json.dumps(report, indent=2))
            return 0
        for name, entry in report.items():
            print(f"{name}: {entry['path']}")
            print("  " + (json.dumps(entry["profile"], sort_keys=True) if entry["profile"] else "(none)"))
        print("`provenant profile show --resolved` prints the values in force.")
        return 0
    resolution = profiles.current(task_class=args.task_class, risk_tier=args.risk_tier)
    if args.json:
        print(json.dumps({**resolution.stamp(), "sources": resolution.sources}, indent=2))
        return 0
    scope = "".join(f" {key} {value}" for key, value in
                    (("task-class", args.task_class), ("risk-tier", resolution.risk_tier)) if value)
    print(f"preset {resolution.preset}  digest {resolution.digest}{scope}")
    width = max(map(len, resolution.knobs))
    shown = {name: _shown(value) for name, value in resolution.knobs.items()}
    value_width = max(map(len, shown.values()))
    for name, value in shown.items():
        print(f"  {name.ljust(width)}  {value.ljust(value_width)}  {resolution.sources[name]}")
    return 0


def command_use(registry: Any, args: argparse.Namespace) -> int:
    if args.preset not in registry.presets:
        raise profiles.ProfileError(f"unknown preset {args.preset!r}; known: {', '.join(registry.presets)}")

    def change(document: dict[str, Any]) -> None:
        _layer_part(document, args.task_class)["preset"] = args.preset

    path = _target(args)
    profiles.edit_file(registry, path, change)
    print(f"{path}: preset {args.preset}" + (f" for {args.task_class}" if args.task_class else ""))
    return 0


def command_set(registry: Any, args: argparse.Namespace) -> int:
    assignments = profiles.parse_assignments(registry, args.assignments, "set")

    def change(document: dict[str, Any]) -> None:
        _layer_part(document, args.task_class).setdefault("knobs", {}).update(assignments)

    path = _target(args)
    profiles.edit_file(registry, path, change)
    for name, value in assignments.items():
        print(f"{path}: {name} = {_shown(value)}")
    return 0


def command_reset(registry: Any, args: argparse.Namespace) -> int:
    for name in args.knobs:
        if name != "preset" and name not in registry.knobs:
            registry.check(name, None, "reset")

    def change(document: dict[str, Any]) -> None:
        part = _layer_part(document, args.task_class)
        for name in args.knobs:
            if name == "preset":
                part.pop("preset", None)
            else:
                part.get("knobs", {}).pop(name, None)
        if "knobs" in part and not part["knobs"]:
            del part["knobs"]

    path = _target(args)
    profiles.edit_file(registry, path, change)
    print(f"{path}: reset {', '.join(args.knobs)}")
    return 0


def command_diff(registry: Any, args: argparse.Namespace) -> int:
    left, right = (profiles.resolve(registry, task_class=args.task_class, layers=[("diff", {"preset": name})])
                   for name in (args.left, args.right))
    rows = {name: [left.knobs[name], right.knobs[name]]
            for name in registry.knobs if left.knobs[name] != right.knobs[name]}
    if args.json:
        print(json.dumps({"left": left.stamp(), "right": right.stamp(), "differs": rows}, indent=2))
        return 0
    print(f"{args.left} ({left.digest}) -> {args.right} ({right.digest})")
    if not rows:
        print("  no knob differs")
    for name, (before, after) in rows.items():
        print(f"  {name}: {_shown(before)} -> {_shown(after)}  [{registry.knobs[name]['status']}]")
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(prog="provenant profile", description=__doc__.splitlines()[0])
    commands = root.add_subparsers(dest="command", required=True)

    listing = commands.add_parser("list", help="knobs and presets")
    listing.add_argument("--json", action="store_true")
    listing.set_defaults(run=command_list)

    show = commands.add_parser("show", help="profile files, or the values in force with --resolved")
    show.add_argument("--resolved", action="store_true")
    show.add_argument("--task-class")
    show.add_argument("--risk-tier")
    show.add_argument("--json", action="store_true")
    show.set_defaults(run=command_show)

    def writer(name: str, helptext: str) -> argparse.ArgumentParser:
        sub = commands.add_parser(name, help=helptext)
        sub.add_argument("--project", action="store_true", help="edit .agents/harness-profile.json in this checkout")
        sub.add_argument("--task-class", help="apply to one task class only")
        return sub

    use = writer("use", "select a preset")
    use.add_argument("preset")
    use.set_defaults(run=command_use)

    assign = writer("set", "set knobs: NAME=VALUE ...")
    assign.add_argument("assignments", nargs="+", metavar="NAME=VALUE")
    assign.set_defaults(run=command_set)

    reset = writer("reset", "drop knobs (or `preset`) from the profile file")
    reset.add_argument("knobs", nargs="+", metavar="NAME")
    reset.set_defaults(run=command_reset)

    diff = commands.add_parser("diff", help="knobs two presets set differently")
    diff.add_argument("left")
    diff.add_argument("right")
    diff.add_argument("--task-class")
    diff.add_argument("--json", action="store_true")
    diff.set_defaults(run=command_diff)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        registry = profiles.load_registry()
        if getattr(args, "task_class", None) and args.task_class not in registry.task_classes:
            raise profiles.ProfileError(
                f"unknown task class {args.task_class!r}; known: {', '.join(sorted(registry.task_classes))}")
        return args.run(registry, args)
    except (profiles.ProfileError, OSError) as exc:
        print(f"provenant profile: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
