"""Resolve durable harness model aliases into auditable concrete routes."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys
import time
from typing import Any


class _ModuleGlobals:
    """Keep CLI reads and writes on the caller, including unregistered path loads."""

    def __init__(self, namespace: dict[str, Any]) -> None:
        object.__setattr__(self, "_namespace", namespace)

    def __getattr__(self, name: str) -> Any:
        try:
            return self._namespace[name]
        except KeyError:
            raise AttributeError(name) from None

    def __setattr__(self, name: str, value: Any) -> None:
        self._namespace[name] = value


def parser(model_route: Any) -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    command = commands.add_parser("resolve")
    command.add_argument("--adapter")
    command.add_argument("--alias")
    command.add_argument("--task-class")
    command.add_argument("--classify", help="comma-separated classifier stages for this call")
    command.add_argument("--brief", default="", help=argparse.SUPPRESS)
    command.add_argument("--mode", default="", help=argparse.SUPPRESS)
    command.add_argument("--path", action="append", default=[], help=argparse.SUPPRESS)
    command.add_argument("--size-files", type=int, help=argparse.SUPPRESS)
    command.add_argument("--size-lines", type=int, help=argparse.SUPPRESS)
    command.add_argument("--principal-reason", default="", help="record why this route escalates to principal")
    command.add_argument("--model-override-tier", choices=("routine", "substantial", "crucial", "terminal"))
    command.add_argument("--role", default="")
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
        default=str(model_route.COMPATIBILITY_PATH),
        help=argparse.SUPPRESS,
    )
    snapshot = commands.add_parser("snapshot")
    snapshot.add_argument("--json", action="store_true")
    health = commands.add_parser("health")
    health.add_argument("--json", action="store_true")
    probe = commands.add_parser("probe")
    probe.add_argument("--adapter", required=True)
    probe.add_argument("--executable", required=True)
    probe.add_argument("--json", action="store_true")
    probe.add_argument("--listing-only", action="store_true",
                       help="list models only, within a 30-second deadline (discovery)")
    model_route._preferences.add_selection_parser(
        commands, model_route.INSTANCE_ROOT / "config" / "model-preferences.json",
    )
    routes = commands.add_parser(
        "routes", help="print each route pool with live availability",
        description="Print every global route pool (utility, junior, senior, principal, design, writing) with each model's weight, "
                    "effort band, caveats and live availability. By default available models print in full and the "
                    "unavailable or cooling ones collapse to a per-pool count line; `--all` lists every entry.")
    routes.add_argument("--all", action="store_true",
                        help="print every pool entry, including unavailable and cooling models")
    routes.add_argument("--json", action="store_true", help="print one fabric.routes.v1 JSON document")
    routes.add_argument("--health", action="store_true",
                        help="also print task-class routes and recent route health (the `route health` view)")
    cooldowns = commands.add_parser("cooldowns", help="show cooldowns for the selected accounts on this host")
    cooldowns.add_argument("--json", action="store_true")
    reset = commands.add_parser("reset", help="clear cooldowns: all, one adapter, or adapter/model")
    reset.add_argument("selector", nargs="?", help="adapter or adapter/model; omit to clear every cooldown")
    reset.add_argument("--account", help="clear only this account on this host")
    reset.add_argument("--json", action="store_true")
    pick = commands.add_parser("pick", help="pick models from route pools; JSON requests on stdin")
    pick.add_argument("--seed", type=int, help=argparse.SUPPRESS)
    return root



def _health_record(catalog: dict[str, Any], model_route: Any) -> dict[str, Any]:
    """Task-class routes plus the recent outcome health Fabric records."""
    record = {"schema": "fabric.route-health.v1", "task_class_routes": catalog.get("task_class_routes", {})}
    try:
        record["routes"] = model_route._fabric_records().read_route_health()
    except (ImportError, OSError, AttributeError, TypeError, ValueError):
        record["routes"] = {}
    return record



def _main(argv: list[str] | None, model_route: Any) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    argument_parser = parser(model_route)
    subcommands = next(action.choices for action in argument_parser._actions
                       if isinstance(action, argparse._SubParsersAction))
    if argv and not argv[0].startswith("-") and argv[0] not in subcommands:
        result = model_route.route_pick([{"route": argv[0]}])
        pick_result = result.get("results", [{}])[0]
        if pick_result.get("status") == "ok":
            picks = pick_result.get("picks", [])
            quota_states = model_route.model_route_live._quota_states()
            for pick in picks:
                quota = quota_states.get(pick["adapter"])
                if quota:
                    pick["quota"] = {key: quota[key] for key in
                                      ("account", "state", "summary", "observed_at") if key in quota}
                    if quota.get("state") in ("ahead_of_pace", "deficit", "credit_only"):
                        pick_result.setdefault("warnings", []).append(
                            f"{pick['adapter']} quota: {quota['summary']}"
                        )
        print(json.dumps(result, sort_keys=True))
        return 0 if pick_result.get("status") == "ok" else 2
    args = argument_parser.parse_args(argv)
    renamed = model_route._pools.RENAMED_TIERS.get(str(getattr(args, "alias", "") or "").casefold())
    if renamed:
        print(f"model-route: alias '{args.alias}' was renamed to '{renamed}'; fix: pass {renamed}", file=sys.stderr)
        return 2
    if isinstance(getattr(args, "alias", None), str) and args.alias.casefold() in model_route.ALIAS_ORDER:
        args.alias = args.alias.casefold()
    if str(getattr(args, "effort", "") or "").casefold() == "max" and args.command in {"resolve", "select"}:
        return model_route.emit(model_route._max_retired(), 2)
    if args.command == "select":
        catalog = model_route.load_catalog(Path(args.catalog) if args.catalog else None)
        task_class_policy = {name: {"minimum_alias": route.get("alias"),
                                    "minimum_effort": route.get("minimum_effort", route.get("effort")),
                                    "role": args.role or "worker"}
                             for name, route in catalog.get("task_class_routes", {}).items()
                             if isinstance(route, dict)}
        return model_route._preferences.select(args, task_class_policy, model_route.ALIAS_ORDER, model_route.EFFORT_ORDER)
    if args.command == "snapshot":
        record = model_route.catalogue_snapshot()
        record.pop("catalogue", None)
        print(json.dumps(record, sort_keys=True))
        return 0
    if args.command == "health":
        record = _health_record(model_route.load_catalog(), model_route)
        print(json.dumps(record, sort_keys=True) if args.json else json.dumps(record, indent=2, sort_keys=True))
        return 0
    if args.command == "routes":
        catalog = model_route.load_catalog()
        document = model_route._pools.describe(
            model_route._router(), catalog, model_route._pools.Availability(model_route._router(), catalog), model_route.EFFORT_ORDER)
        if args.health:
            document["health"] = _health_record(catalog, model_route)
        if args.json:
            print(json.dumps(document, sort_keys=True))
        else:
            print(model_route._pools.render(document, full=args.all))
            if args.health:
                print("\ntask-class routes and recent health (route health --json):")
                print(json.dumps(document["health"], indent=2, sort_keys=True))
        return 0
    if args.command == "cooldowns":
        record = {"schema": "fabric.cooldowns.active.v1", "cooldowns": model_route._fabric_records().read_cooldowns()}
        print(json.dumps(record, sort_keys=True) if args.json else json.dumps(record, indent=2, sort_keys=True))
        return 0
    if args.command == "reset":
        try:
            removed = model_route._fabric_records().clear_cooldowns(args.selector, account=args.account)
        except ValueError as exc:
            print(f"model-route reset: {exc}", file=sys.stderr)
            return 2
        print(json.dumps({"schema": "fabric.cooldown-reset.v1", "removed": removed}, sort_keys=True) if args.json
              else "\n".join(removed) or "no cooldowns matched")
        return 0
    if args.command == "pick":
        try:
            requests = json.loads(sys.stdin.read() or "{}").get("requests")
        except (ValueError, AttributeError):
            requests = None
        print(json.dumps(model_route.route_pick(requests, args.seed), sort_keys=True))
        return 0
    if args.command == "probe":
        record, code = model_route.model_route_probe.probe_capabilities(
            args.adapter, args.executable, time.monotonic() + 30 if args.listing_only else None,
            listing_only=args.listing_only)
        print(json.dumps(record, sort_keys=True))
        return code
    snapshot = model_route.catalogue_snapshot(Path(args.catalog) if args.catalog else None)
    catalog = snapshot["catalogue"]
    if getattr(args, "available_effort", None):
        args.available_effort = model_route._without_max({"efforts": list(args.available_effort)})["efforts"]
    model_route.ROUTE_WARNINGS = ([
        "routing catalogue was stale; resolved against the fresh product snapshot; "
        "fix: use the current release routing catalogue; reinstall the current release if needed"
    ] if args.command == "resolve" and snapshot.get("stale_routing") else [])
    if args.command == "resolve":
        model_route.ROUTE_CLASSIFICATION = {}
        if not args.role:
            args.role = "worker"
            model_route.ROUTE_WARNINGS.append("missing role; using worker label")
        prompt = args.brief or os.environ.get("PROVENANT_TASK_BRIEF", "")
        classify_context = bool(args.classify or prompt or os.environ.get("PROVENANT_TASK_CLASSIFY")
                                or args.mode or args.path or args.size_files is not None or args.size_lines is not None
                                or os.environ.get("PROVENANT_TASK_MODE") or os.environ.get("PROVENANT_TASK_SIZE_FILES")
                                or os.environ.get("PROVENANT_TASK_SIZE_LINES") or os.environ.get("PROVENANT_TASK_PATHS"))
        if args.task_class or classify_context:
            classification = model_route.task_classifier.classify(
                catalog,
                {"prompt": prompt, "mode": args.mode or os.environ.get("PROVENANT_TASK_MODE", ""),
                 "role": args.role, "paths": args.path,
                 "size": {"files": args.size_files, "lines": args.size_lines}},
                explicit=args.task_class, override=args.classify,
            )
            if not args.alias and not args.model and not args.model_override_tier:
                args.task_class = classification["class"]
            model_route.ROUTE_CLASSIFICATION = {
                "task_class": classification["class"],
                "classified_by": classification["classified_by"],
                "classifier_confidence": classification["confidence"],
                "shadow_answers": classification["shadow_answers"],
                "size_tier": classification.get("size_tier"),
            }
        if args.principal_reason:
            model_route.ROUTE_CLASSIFICATION["principal_reason"] = args.principal_reason
            print(f"model-route: principal escalation: {args.principal_reason}", file=sys.stderr)
        # cf_dispatch defaults an alias beside a named model; that is not the caller's.
        args.alias_supplied = bool(args.alias) and os.environ.get("FABRIC_ALIAS_IMPLIED") != "1"
        if args.endpoint and args.model and not args.alias:
            args.alias = "senior"
        ordinary_name = args.alias and args.alias not in model_route.ALIAS_ORDER
        ordinary_model = args.model and not args.alias
        ordinary_unknown = args.model and args.alias and not model_route.infer_family(args.model, catalog)
        ordinary_broker_tier = args.adapter in {"opencode", "agy"}
        families_for_ordinary = catalog.get("families")
        ordinary_catalog_valid = isinstance(families_for_ordinary, dict) and all(
            isinstance(config, dict) and model_route.risk_tier_overrides_are_valid(name, config, catalog)
            for name, config in families_for_ordinary.items()
        )
        if ordinary_catalog_valid and not args.endpoint and not args.task_class and not args.model_override_tier and not args.require_distinct and (ordinary_name or ordinary_model or ordinary_unknown or ordinary_broker_tier or not args.adapter):
            return model_route.resolve_ordinary(args, catalog)
        # A plain route with an implied alias beside a model still gets a concrete id for a family name or newer live id.
        latest_adapter = (catalog.get("adapters", {}).get(args.adapter) or {}).get("latest_aliases") is True
        if (args.model and latest_adapter and not args.endpoint and not args.task_class
                and not args.model_override_tier and not args.require_distinct):
            family_match, family_notes = model_route.model_route_live._latest_in_family(args.adapter, args.model, catalog)
            if not family_match:
                family_match, family_notes = model_route.model_route_live._live_id(args.adapter, args.model, catalog)
            if family_match:
                args.model = family_match["id"]
                model_route.ROUTE_WARNINGS.extend(family_notes)
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
            return model_route.emit(record, code)

        args.task_class_effort = ""
        args.task_class_effort_floor = ""
        args.task_class_effort_shift = 0
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
        if adapter_family and not model_route.risk_tier_overrides_are_valid(
            adapter_family, families.get(adapter_family, {}), catalog
        ):
            return reject("risk_tier_config_invalid", alias=args.alias)
        if adapter_family and not model_route.ultra_eligible_roles_are_valid(
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
            for scanned_family, scanned_config in model_route.override_scan_families(
                args.model, catalog
            ).items():
                if not model_route.risk_tier_overrides_are_valid(
                    scanned_family, scanned_config, catalog
                ):
                    return reject("risk_tier_config_invalid", alias=args.alias)
        # Once catalogue integrity is known, a configured execution gate is the
        # first route fact for a known adapter. Invalid selectors must not hide
        # `enabled: false` or its typed reason, and this preflight never invokes a
        # provider capability source.
        if args.adapter in model_route.COMPATIBILITY_ADAPTER_IDS:
            compatibility, compatibility_status = model_route.load_adapter_compatibility(
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
            route = catalog.get("task_class_routes", {}).get(args.task_class)
            if route is None:
                return reject("unknown_task_class")
            if not isinstance(route, dict):
                return reject("task_class_config_invalid")
            route_alias = route.get("alias")
            configured_effort = route.get("effort")
            minimum_effort = route.get("minimum_effort", configured_effort)
            route_effort = configured_effort
            size_tier = next((band for band in catalog.get("task_classification", {}).get("size_tiers", [])
                              if band.get("name") == model_route.ROUTE_CLASSIFICATION.get("size_tier")), None)
            size_escalated = False
            if size_tier and size_tier.get("alias") in model_route.ALIAS_ORDER and route_alias in model_route.ALIAS_ORDER:
                if model_route.ALIAS_ORDER[size_tier["alias"]] > model_route.ALIAS_ORDER[route_alias]:
                    route_alias = size_tier["alias"]
                    size_escalated = True
                tier_effort = size_tier.get("effort")
                if tier_effort in model_route.EFFORT_ORDER and route_effort in model_route.EFFORT_ORDER:
                    if model_route.EFFORT_ORDER[tier_effort] > model_route.EFFORT_ORDER[route_effort]:
                        route_effort = tier_effort
                        size_escalated = True
            if (
                route_alias not in model_route.ALIAS_ORDER
                or route_effort not in model_route.EFFORT_ORDER
                or minimum_effort not in model_route.EFFORT_ORDER
            ):
                return reject(
                    "task_class_config_invalid",
                    alias=route_alias if isinstance(route_alias, str) else "",
                    effort=route_effort if isinstance(route_effort, str) else "",
                )
            if model_route.EFFORT_ORDER[route_effort] < model_route.EFFORT_ORDER[minimum_effort]:
                return reject("task_class_config_invalid", alias=route_alias, effort=route_effort)
            if args.effort:
                return reject("task_class_effort_conflict", alias=route_alias, effort=route_effort)
            if args.model:
                return reject("task_class_model_conflict", alias=route_alias, effort=route_effort)
            model_routes = route.get("models")
            if not isinstance(args.adapter, str) or args.adapter not in catalog.get("adapters", {}):
                return reject("unknown_adapter")
            configured_models = model_routes.get(args.adapter) if isinstance(model_routes, dict) else None
            if (not isinstance(configured_models, list) or not configured_models
                    or any(not isinstance(model, str) or not model.strip() for model in configured_models)
                    or len(set(configured_models)) != len(configured_models)):
                return reject("task_class_config_invalid", alias=route_alias, effort=route_effort)
            resolved_models, resolved_entries = [], []
            for configured_model in configured_models:
                entry, _ = model_route.model_route_live._registered_match(args.adapter, configured_model, catalog)
                if (entry is None or configured_model != entry["id"]
                        or entry.get("effort_transport") == "none"
                        or configured_effort not in entry.get("efforts", [])):
                    return reject("task_class_config_invalid", alias=route_alias, effort=route_effort)
                resolved_models.append(entry["id"])
                resolved_entries.append(entry)
            args.task_class_configured_models = list(resolved_models)
            if args.task_class in {"review", "critical-review", "second-opinion"}:
                failed_models = model_route.model_route_live._recent_failed_task_class_models(args.adapter, args.task_class)
                failed_in_roster = [model for model in resolved_models if model in failed_models]
                if failed_in_roster:
                    alternate = next((model for model in resolved_models if model not in failed_models), None)
                    resolved_models = [alternate] if alternate else []
            args.task_class_models = None if size_escalated else resolved_models
            args.alias = route_alias
            args.task_class_effort, args.task_class_effort_shift = (
                route_effort, 0) if size_escalated else model_route.shifted_task_class_effort(
                    args.task_class, configured_effort, minimum_effort, resolved_entries)
            # A runtime fallback may not go below the lower of the configured and shifted effort.
            args.task_class_effort_floor = min(route_effort, args.task_class_effort, key=model_route.EFFORT_ORDER.__getitem__)
        elif args.alias not in {"principal", "senior", "junior"}:
            return reject("unknown_alias")
        if args.effort and args.effort not in model_route.EFFORT_ORDER:
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
            if not model_route.risk_tier_override_is_well_formed(override):
                return reject("risk_tier_config_invalid", alias=args.alias)
            maximum_effort = override["maximum_effort"]
            roles = override["roles"]
            if args.role not in roles:
                return reject("risk_tier_role_mismatch", alias=args.alias)
            if args.alias != override.get("alias"):
                return reject("risk_tier_alias_mismatch", alias=args.alias)
            if args.effort and model_route.EFFORT_ORDER[args.effort] > model_route.EFFORT_ORDER[maximum_effort]:
                return reject("risk_tier_effort_above_ceiling", alias=args.alias, effort=args.effort)
            args.model_override = override
        if args.endpoint:
            profile, endpoint_status = model_route.resolve_endpoint_profile(
                args.endpoint, args.adapter, catalog
            )
            if endpoint_status:
                return reject(endpoint_status, alias=args.alias, endpoint=args.endpoint)
            args.endpoint_profile = profile
        return model_route.resolve(args, catalog)
    return 2

