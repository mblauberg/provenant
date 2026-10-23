"""Dispatch input, route planning and harness-profile preflight."""

from __future__ import annotations

import argparse
from contextlib import redirect_stdout
import importlib.util
import io
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

SKILLS_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SKILLS_ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))
CF_DISPATCH = Path(__file__).with_name("cf_dispatch.py")

from _shared import harness_profile
from _shared.custody import OwnedFileError, OwnedLinkError, open_contained_regular
from layout import run_workspace
from fabric_records import TERMINAL_STATUSES
import run_records
import writer_lease
import provider_exec
import provider_output
import secret_scan
import exec_routing

TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")

ACCESS_MODES = ("read_only", "worktree_write")

# Provider deadlines sit slightly under the owner's own deadline so the provider
# reaches its limit first and exits through its normal path, flushing whatever
# result it has. Without the gap the owner kills the process group mid-write and
# the truncated result is diagnosed as a corrupt receipt rather than a timeout.
PROVIDER_TIMEOUT_MARGIN_SECONDS = 5.0

def provider_timeout_seconds(timeout_seconds: float) -> int:
    """Return the whole-second deadline handed to the provider CLI.

    Only the adapters whose CLI accepts a headless timeout consume it: on this
    machine that is agy (``--print-timeout``). Neither ``claude`` nor
    ``codex exec`` exposes a timeout flag, so on those arms the owner's deadline
    below remains the only bound.
    """
    margin = max(1.0, min(PROVIDER_TIMEOUT_MARGIN_SECONDS, timeout_seconds * 0.1))
    return max(1, int(timeout_seconds - margin))

FAST_PLAN_SHELL_ENV_READS = {
    "CF_DISPATCH_AGY_ADD_DIR", "CF_DISPATCH_ENABLE_KIRO",
    "CF_DISPATCH_ENABLE_COPILOT", "CF_DISPATCH_CURSOR_MODEL", "CF_DISPATCH_KIRO_MODEL",
    "CF_DISPATCH_COPILOT_MODEL", "CF_DISPATCH_OPENCODE_MODEL", "CF_DISPATCH_ENDPOINT",
    "CF_DISPATCH_CODEX_NETWORK", "CF_DISPATCH_AGY_SANDBOX",
}

FAST_PLAN_ROUTE_REQUIRED = (
    "resolved_model", "model_family", "endpoint_provider", "identity_source",
)

FAST_PLAN_ROUTE_FIELDS = (
    "status", "resolved_model", "model_family", "endpoint_provider",
    "identity_source", "requested_effort", "effort", "effort_source",
    "effort_capability_source", "effort_substitution", "substitution",
    "fallback_model", "catalog_model", "model_selection",
    "model_override_tier", "policy_override", "alias", "reason",
    "endpoint_profile", "endpoint_base_url", "endpoint_token_env", "endpoint_wire_api",
)

def parse_fast_route_json(raw: str) -> dict:
    """Apply the shell planner's parse_route_json field contract before fast planning."""
    def reject_duplicate_members(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate route member: {key}")
            value[key] = item
        return value

    route = json.loads(raw, object_pairs_hook=reject_duplicate_members)
    if not isinstance(route, dict):
        raise ValueError("route must be a JSON object")
    if route.get("status") == "ok":
        for key in FAST_PLAN_ROUTE_REQUIRED:
            value = route.get(key)
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"successful route requires non-empty {key}")
    for key in FAST_PLAN_ROUTE_FIELDS:
        value = route.get(key, "")
        if value is not None and (not isinstance(value, str) or "\0" in value):
            raise ValueError(f"route field {key} must be a NUL-free string")
    return route

PLANNER_MODEL_STATUSES = {"capability_model_unavailable", "no_candidate_available", "alias_unavailable",
                          "adapter_default_model_invalid", "model_required_for_broker",
                          "model_ambiguous", "model_not_found"}

PLANNER_TRANSIENT_STATUSES = {"capability_discovery_failed", "capability_snapshot_untrusted",
                              "capability_snapshot_stale", "probe_unavailable", "probe_cache_busy"}

def planner_result(planning):
    """The route plan, or a typed failure when the planner produced none."""
    try:
        return json.loads(planning.stdout)
    except ValueError:
        if planning.returncode in (-signal.SIGTERM, -signal.SIGINT, -signal.SIGHUP):
            return {"status": "interrupted", "fix": f"route planning stopped by signal {-planning.returncode}; retry"}
        if planning.returncode < 0:
            return {"status": "failed", "fix": f"route planner crashed (signal {-planning.returncode}); inspect stderr"}
        return {"status": "rejected", "fix": "route planner returned invalid JSON"}

def route_refusal(record, adapter=""):
    """A planner refusal in the router's vocabulary; provider-exec records carry provenance."""
    status = record.get("status")
    if not status or status in TERMINAL_STATUSES or "provenance" in record or record.get("schema") == "fabric.exec-plan.v1":
        return None
    return router_failure(str(status), adapter, record.get("message"))

def router_failure(signature, adapter="", message=None):
    """Fabric's status and fix for a router refusal; the router's own status stays the signature."""
    if signature in PLANNER_MODEL_STATUSES:
        status = "model_unavailable"
        fix = provider_output._model_unavailable_fix({"adapter": adapter, "route": {"identity_source": "passed-through"}})
        if signature in {"model_ambiguous", "model_not_found"} and isinstance(message, str) and message:
            fix = message[:600]
    elif signature in PLANNER_TRANSIENT_STATUSES:
        status, fix = "failed", f"provider capability check failed ({signature}); check the provider CLI login and retry"
    else:
        status, fix = "rejected", f"route planner refused the route ({signature}); change the requested route"
    return {"status": status, "error": signature, "fix": fix,
            "evidence": {"exit": None, "signal": None, "signature": signature, "excerpt": ""}}

def fast_fabric_plan(args, prompt_path: Path, result_path: Path, workspace: Path):
    """Plan a simple explicit route in the owner process using the same router and supervisor."""
    if (not args.model or args.tool not in {"claude", "codex"}
            or any(str(value).startswith("-") for value in (args.model, args.effort, args.role) if value)
            or any(value for key, value in os.environ.items()
                   if key.startswith("CF_DISPATCH_") and key != "CF_DISPATCH_IDLE_SECONDS")
            or not Path(os.environ.get("TMPDIR") or "/tmp").is_dir()
            or args.access_mode != "read_only"
            or args.task_class or args.alias or args.resume or getattr(args, "resume_session", None) or args.worktree
            or args.provider_cwd or args.sandbox or args.network is not None or args.add_dirs
            or getattr(args, "capabilities", None)
            or args.git_evidence or args.model_override_tier or args.orchestrator_family
            or args.intent != "ordinary" or args.fallback not in (None, False, "false")):
        return None
    executable = {"cursor": "cursor-agent", "kiro": "kiro-cli"}.get(args.tool, args.tool)
    if not shutil.which(executable):
        return None
    environment = routing_environment()
    product = Path(environment.get("AGENT_FABRIC_PRODUCT_ROOT") or SKILLS_ROOT.parent)
    path = product / "scripts/model_route.py"
    if not path.is_file():
        return None
    try:
        prompt = prompt_path.read_text(encoding="utf-8")
        if not prompt or "\0" in prompt:
            return None
        context_spec = importlib.util.spec_from_file_location("fabric_fast_worktree", product / "scripts/worktree.py")
        context_module = importlib.util.module_from_spec(context_spec)
        context_spec.loader.exec_module(context_module)
        try:
            context_module.validate_context(argparse.Namespace(repo=workspace, allow_non_git=True))
        except context_module.PolicyError:
            return None
        spec = importlib.util.spec_from_file_location("fabric_fast_model_route", path)
        module = importlib.util.module_from_spec(spec)
        overrides = {"FABRIC_ALIAS_IMPLIED": "1", "AGENT_FABRIC_PRODUCT_ROOT": str(product),
                     "AGENT_FABRIC_INSTANCE_ROOT": environment.get("AGENT_FABRIC_INSTANCE_ROOT")
                     or str(Path.home() / ".agents")}
        previous = {key: os.environ.get(key) for key in overrides}
        os.environ.update(overrides)
        try:
            with redirect_stdout(io.StringIO()) as output:
                spec.loader.exec_module(module)
                argv = ["resolve", "--adapter", args.tool, "--role", args.role,
                        "--lead-family", "", "--alias", "principal", f"--model={args.model}"]
                if args.effort:
                    argv += ["--effort", args.effort]
                if args.fallback is not None:
                    argv += ["--fallback", "false"]
                code = module.main(argv)
        finally:
            for key, value in previous.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
        if code != 0:
            return None
        route = parse_fast_route_json(output.getvalue())
        if route.get("status") != "ok":
            return None
        plan = provider_exec.build_plan(
            args.tool, route, prompt,
            cwd=workspace, workspace_root=workspace, read_roots=getattr(args, "read_roots", None) or [],
            mode=args.access_mode, timeout_seconds=provider_timeout_seconds(args.timeout_seconds),
            intent=args.intent, preface=args.preface, requested_model=args.model,
            requested_effort=args.effort or "", run_id=os.environ.get("PROVENANT_RUN_ID", ""),
            run_dir=result_path.parent, account=getattr(args, "account", None) if args.tool == "claude" else None,
            original_prompt_file=getattr(args, "original_prompt_file", None) or prompt_path,
            chair=os.environ.get("PROVENANT_CHAIR", ""),
            reviewer_id=args.reviewer_id or "", risk_tier=args.risk_tier or "",
            model_override_tier=args.model_override_tier or "", orchestrator_family="",
        )
        plan["output_path"] = str(result_path.absolute())
        return plan
    except (Exception, SystemExit):
        return None

def _read_prompt_once(workspace: Path, prompt_source: Path, max_bytes: int | None = None) -> bytes:
    """Read the validated prompt inode once for both retention and provider use."""
    try:
        relative = prompt_source.relative_to(workspace)
    except ValueError as exc:
        raise OwnedFileError("prompt file must be inside the current workspace") from exc
    fd, _relative, _target = open_contained_regular(
        workspace, relative, os.O_RDONLY, label="prompt file"
    )
    try:
        chunks: list[bytes] = []
        size = 0
        while True:
            chunk = os.read(fd, 1024 * 1024)
            if not chunk:
                return b"".join(chunks)
            size += len(chunk)
            if max_bytes is not None and size > max_bytes:
                raise PreflightError("request_too_large", "Remote prompt exceeds request limit")
            chunks.append(chunk)
    finally:
        os.close(fd)

PROMPT_PATH_FIX = ("Pass prompt_file=<readable regular file inside the workspace, a caller temp directory or a registered Fabric project>; "
                   "register another project by running `provenant fabric identity --json` there, or dispatch from it.")

class PreflightError(ValueError):
    def __init__(self, code: str, fix: str):
        super().__init__(fix)
        self.code = code

def read_prompt_input(prompt_file: Path, workspace: Path, run_dir: Path, read_roots=(), max_bytes=None) -> bytes:
    prompt_source = None
    if prompt_file is not None:
        prompt_source = prompt_file.expanduser()
        if not prompt_source.is_absolute():
            prompt_source = workspace / prompt_source
    prompt_bytes: bytes | None = None
    if prompt_source is not None:
        if not prompt_source.exists():
            raise PreflightError("prompt_unavailable", f"cannot read prompt file: {prompt_source}")
        roots = (run_dir, workspace, run_workspace(run_dir, workspace), *(Path(root) for root in read_roots),
                 *run_records.prompt_temp_roots())
        prompt_root = next((root for root in roots if prompt_source.is_relative_to(root)), None)
        if prompt_root is None:
            raise PreflightError("prompt_path_forbidden", PROMPT_PATH_FIX)
        sensitive_roots = {".ssh", ".aws", ".azure", ".gnupg"}
        sensitive_files = {
            ".env", ".env.local", ".env.production", "credentials.json",
            "application_default_credentials.json", "token.json",
        }
        parts = [part.casefold() for part in prompt_source.parts]
        config_auth_dirs = {"gcloud", "gh", "claude", "codex", "openai"}
        config_auth = any(
            part == ".config" and index + 1 < len(parts) and parts[index + 1] in config_auth_dirs
            for index, part in enumerate(parts)
        )
        # credential_path resolves the whole path, so a link into a store is refused too.
        if (sensitive_roots.intersection(parts) or prompt_source.name.casefold() in sensitive_files or config_auth
                or provider_exec.provider_confinement.credential_path(prompt_source)):
            raise PreflightError("credential_or_auth_store_denied", "prompt path is a credential or authentication store")
        try:
            prompt_bytes = _read_prompt_once(prompt_root, prompt_source, max_bytes=max_bytes)
        except OwnedLinkError as exc:
            raise PreflightError("prompt_hard_link_denied", str(exc))
        except OwnedFileError as exc:
            raise PreflightError("prompt_unavailable", str(exc))
    return prompt_bytes

def routing_environment(*, preserve_instance=False) -> dict[str, str]:
    """Keep discovery and worker routing on the same instance/product fallback."""
    env = os.environ.copy()
    instance = Path(env.get("AGENT_FABRIC_INSTANCE_ROOT") or Path.home() / ".agents").expanduser()
    # Routing may fall back to the product catalogue; the user's profile file stays the one receipts stamp.
    if not env.get(harness_profile.PROFILE_ROOT_ENV):  # empty counts as unset, as in user_path
        env[harness_profile.PROFILE_ROOT_ENV] = str(instance)
    # Only router invocations fall back to the product catalogue. Provider planning
    # retains the instance that owns telemetry accounts.
    if not preserve_instance:
        try:
            (instance / "config/model-routing.json").lstat()
        except FileNotFoundError:
            env["AGENT_FABRIC_INSTANCE_ROOT"] = env.get("AGENT_FABRIC_PRODUCT_ROOT", str(SKILLS_ROOT.parent))
        except OSError:
            pass
    return env

EFFORT_RANK = {"low": 0, "medium": 1, "high": 2, "xhigh": 3, "ultra": 4}

def effort_retry_plan(plan: dict[str, Any], adapter_record: dict[str, Any]) -> dict[str, Any] | None:
    """Retry a provider-rejected effort only when route capabilities prove a lower option."""
    evidence = adapter_record.get("evidence") if isinstance(adapter_record.get("evidence"), dict) else {}
    rejection = str(adapter_record.get("reason") or evidence.get("excerpt") or "")
    if (adapter_record.get("error") != "invalid_input"
            or not re.search(r"effort.{0,50}(?:not supported|unsupported)", rejection, re.I)):
        return None
    route = plan.get("route") if isinstance(plan.get("route"), dict) else {}
    supported = route.get("supported_efforts")
    current = plan.get("effort")
    if (not isinstance(supported, list) or current not in EFFORT_RANK
            or any(value not in EFFORT_RANK for value in supported)):
        return None
    lower = [value for value in supported if EFFORT_RANK[value] < EFFORT_RANK[current]]
    if not lower:
        return None
    effort = max(lower, key=EFFORT_RANK.__getitem__)
    warning = f"adapter rejected {current} effort; retrying at {effort}"
    retry = dict(plan)
    retry_route = dict(route, effort=effort, effort_applied=effort,
                       effort_substitution=warning)
    retry["route"] = retry_route
    retry["effort"] = effort
    retry["warnings"] = [*plan.get("warnings", []), warning]
    retry["argv"] = provider_exec.profile(plan["adapter"]).argv(retry)
    return retry

def preflight_tasks(tasks: list[dict[str, Any]], workspace_root: Path | None = None) -> dict[str, Any]:
    """Use the dispatcher router before creating custody or launching any task."""
    workspace = Path(workspace_root or Path.cwd()).resolve()
    product = Path(os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", SKILLS_ROOT.parent))
    instance = Path(routing_environment().get("AGENT_FABRIC_INSTANCE_ROOT") or Path.home() / ".agents").expanduser()
    catalog = instance / "config/model-routing.json"
    errors = []
    routes = []
    seen = set()
    writers = set()
    probes: dict[str, Path | None] = {}
    resolved_routes: dict[tuple[str, ...], dict[str, Any]] = {}
    with tempfile.TemporaryDirectory(prefix="fabric-preflight-") as scratch:
        for task in tasks:
            task_id = task.get("id", "task-1")
            try:
                if not isinstance(task_id, str) or not TASK_ID_RE.fullmatch(task_id) or task_id in seen:
                    raise PreflightError("invalid_task_id", "Pass unique task ids containing letters, numbers, '.', '_' or '-'.")
                seen.add(task_id)
                try:
                    exec_routing.validate_policy(task.get("fallback"))
                except ValueError as exc:
                    raise PreflightError("fallback_invalid", str(exc)) from exc
                if task.get("sandbox") not in {None,"read-only","workspace-write","full"}:
                    raise PreflightError("sandbox_invalid","Pass read-only, workspace-write or full.")
                if task.get("access_mode","read_only")=="read_only" and task.get("sandbox") not in {None,"read-only"}:
                    raise PreflightError("sandbox_forbidden","A read-only run requires a read-only sandbox.")
                if "network" in task and type(task["network"]) is not bool:
                    raise PreflightError("network_invalid","Pass network true or false.")
                if any(provider_exec.provider_confinement.credential_path(path) for path in task.get("add_dirs",[])):
                    raise PreflightError("credential_or_auth_store_denied","Additional directories must exclude credential stores.")
                if (task.get("prompt") is None) == (task.get("prompt_file") is None):
                    raise PreflightError("prompt_required", "Pass exactly one of prompt or prompt_file.")
                if task.get("prompt") is not None and not isinstance(task["prompt"], str):
                    raise PreflightError("prompt_invalid", "Pass prompt as text.")
                if "allow_secrets" in task and type(task["allow_secrets"]) is not bool:
                    raise PreflightError("allow_secrets_invalid", "Pass allow_secrets: true or false.")
                read_roots = task.get("read_roots") or []
                if not isinstance(read_roots, list) or not all(
                        isinstance(root, str) and Path(root).is_absolute() for root in read_roots):
                    raise PreflightError("read_roots_invalid", "Pass read_roots as a list of absolute directories.")
                if any(provider_exec.provider_confinement.credential_path(root) for root in read_roots):
                    raise PreflightError("credential_or_auth_store_denied", "Read roots must exclude credential stores.")
                read_roots = [str(Path(root).resolve()) for root in read_roots]
                if "confidential" in task and type(task["confidential"]) is not bool:
                    raise PreflightError("confidential_invalid", "Pass confidential: true or false.")
                prompt_bytes = (read_prompt_input(Path(task["prompt_file"]), workspace, workspace, read_roots)
                                if task.get("prompt_file") is not None else task["prompt"].encode())
                try:
                    scan = secret_scan.scan_inputs(
                        prompt_bytes, str(task.get("prompt_file") or "<prompt>"),
                        [str(workspace / Path(item).expanduser()) for item in task.get("add_dirs") or []],
                        workspace_root=workspace)
                except (OSError, subprocess.SubprocessError) as exc:
                    raise PreflightError("secret_scan_unavailable", f"Make dispatch inputs readable for the secret scan: {exc}") from exc
                if scan.budget_exceeded and not task.get("allow_secrets", False):
                    raise PreflightError("secret_scan_budget_exceeded", scan.fix())
                if scan.findings and not task.get("allow_secrets", False):
                    raise PreflightError("secret_detected", scan.fix())
                adapter = task["adapter"]
                mode = task.get("access_mode", "read_only")
                if mode not in ACCESS_MODES:
                    raise PreflightError("access_mode_invalid", "Pass mode read_only or worktree_write.")
                if "capabilities" in task:
                    try:
                        capabilities = provider_exec.capability_values(task["capabilities"])
                        sandbox = task.get("sandbox") or (
                            "workspace-write" if mode == "worktree_write" else "read-only"
                        )
                        provider_exec.validate_capabilities(
                            capabilities, adapter=adapter, mode=mode, sandbox=sandbox,
                            network=task.get("network"),
                        )
                    except provider_exec.CapabilityError as exc:
                        raise PreflightError(exc.code, str(exc)) from exc
                    except ValueError as exc:
                        raise PreflightError("capabilities_invalid", str(exc)) from exc
                if mode == "worktree_write":
                    if adapter not in writer_lease.WORKTREE_WRITE_ADAPTERS:
                        raise PreflightError("worktree_write_adapter_unsupported", "Pass mode read_only or adapter " + ", ".join(sorted(writer_lease.WORKTREE_WRITE_ADAPTERS)) + ".")
                    if not task.get("worktree"):
                        raise PreflightError("worktree_required", "Pass worktree=<registered Git worktree root> with mode worktree_write.")
                    worktree = writer_lease.resolve_writer_worktree(Path(task["worktree"]))
                    if worktree in writers:
                        raise PreflightError("worktree_conflict", "Pass a different registered worktree for each writer task.")
                    writers.add(worktree)
                elif task.get("worktree"):
                    raise PreflightError("worktree_not_applicable", "Pass mode worktree_write with worktree, or omit worktree.")
                command = [*exec_routing.route_command(product), "resolve",
                           "--adapter", adapter,
                           "--role", task.get("role") or "worker"]
                if task.get("task_class"):
                    command.extend(("--task-class", task["task_class"]))
                elif task.get("alias"):
                    command.extend(("--alias", task["alias"]))
                elif not task.get("model"):
                    command.extend(("--alias", "senior"))
                policy = exec_routing.validate_policy(task.get("fallback"))
                if policy is not None:
                    command.extend(("--fallback", "true" if isinstance(policy, list) else json.dumps(policy) if type(policy) is bool else policy))
                for key in ("model", "effort"):
                    if task.get(key):
                        command.extend(["--" + key, task[key]])
                if os.environ.get("CF_DISPATCH_ENDPOINT"):
                    command.extend(["--endpoint", os.environ["CF_DISPATCH_ENDPOINT"]])
                def resolve_route():
                    key = tuple(command)
                    if key in resolved_routes:
                        return resolved_routes[key]
                    result = subprocess.run(command, text=True, capture_output=True, timeout=15, env=routing_environment())
                    try:
                        route = json.loads(result.stdout)
                    except ValueError:
                        route = {"status": "interpreter_unavailable"}
                    # The wrapper reports a missing interpreter as JSON; it is the same repair.
                    if not isinstance(route, dict) or route.get("status") == "interpreter_unavailable":
                        raise PreflightError("model_routing_unavailable", "Restore the routing catalogue and the harness Python environment.")
                    resolved_routes[key] = route
                    return route
                route = resolve_route()
                if (adapter == "codex" and route.get("status") == "capability_discovery_failed") or (
                    adapter == "agy" and route.get("status") in {"ok", "model_required_for_broker"}
                ):
                    if adapter not in probes:
                        path = Path(scratch) / (adapter + ".json")
                        probe = subprocess.run([sys.executable, str(CF_DISPATCH.with_name("capabilities.py")),
                                                adapter, "--out", str(path)], capture_output=True, timeout=20)
                        probes[adapter] = path if probe.returncode == 0 else None
                    if probes[adapter] is not None:
                        command.extend(["--capabilities-file", str(probes[adapter])])
                        route = resolve_route()
                if route.get("status") != "ok":
                    code = route.get("status", "routing_record_invalid")
                    try:
                        routing_catalog = exec_routing.snapshot() or json.loads(catalog.read_text())
                    except (OSError, ValueError):
                        routing_catalog = {}
                    fixes = {
                        "model_required_for_broker": "Pass model=<provider/model id> for " + adapter + ".",
                        "effort_unsupported": "Pass an effort supported by the selected model, or omit effort.",
                        "capability_discovery_failed": "Pass an installed, authenticated adapter with a readable model catalogue.",
                    }
                    adapter_config = routing_catalog.get("adapters", {}).get(adapter, {})
                    family = adapter_config.get("fixed_model_family")
                    families = [family] if family else adapter_config.get("model_family_preferences", {}).get("preferred", [])
                    aliases: dict[str, list[str]] = {}
                    for name in families:
                        for alias, models in routing_catalog.get("families", {}).get(name, {}).get("aliases", {}).items():
                            aliases.setdefault(alias, []).extend(models)
                    choices = list(dict.fromkeys(model for models in aliases.values() for model in models))
                    fix = ("Pass alias " + ", ".join(aliases) + " or model " + ", ".join(choices) + "."
                           if choices else "Pass an explicit provider/model id supported by " + adapter + ".")
                    if "effort" in code:
                        fix = "Omit effort, or pass a supported level: low, medium, high, xhigh, ultra."
                    if code in {"model_ambiguous", "model_not_found"} and route.get("message"):
                        fix = str(route["message"])[:600]
                    raise PreflightError(code, fixes.get(code, fix))
                if task.get("confidential") is True:
                    risk = confidential_refusal(adapter, route.get("resolved_model") or task.get("model"), route)
                    if risk and not task.get("model"):
                        # An alias or task class may still hold a safe member: step to it before refusing.
                        selector = [part for index, part in enumerate(command)
                                    if part not in {"--alias", "--task-class"}
                                    and (index == 0 or command[index - 1] not in {"--alias", "--task-class"})]
                        for alternative in exec_routing.confidential_alternatives(
                                adapter, route.get("resolved_model"), route.get("alias")):
                            command = [*selector, "--model", alternative]
                            candidate = resolve_route()
                            if candidate.get("status") == "ok" and not confidential_refusal(
                                    adapter, candidate.get("resolved_model") or alternative, candidate):
                                route, risk = candidate, None
                                break
                    if risk:
                        raise PreflightError("confidential_route", risk)
                protected = provider_exec.provider_confinement.check_protected_inputs(
                    route, workspace, worktree if mode == "worktree_write" else task.get("cwd") or workspace,
                    worktree=worktree if mode == "worktree_write" else None,
                    prompt_file=task.get("prompt_file"), add_dirs=task.get("add_dirs", []), read_roots=read_roots)
                if protected and (adapter == "codex" or not provider_exec.provider_confinement._sandbox_exec_path()):
                    raise ValueError("protected paths require sandbox-exec read confinement; fix: use a non-training route")
                if task.get("prompt_file") is not None:
                    read_prompt_input(Path(task["prompt_file"]), workspace, workspace, read_roots)
                if os.environ.get('PROVENANT_REMOTE_LANE') == '1':
                    hosts = _fabric_hosts_owner()
                    diagnostic = hosts.adapter_doctor(adapter, workspace)
                    if diagnostic['signin']['status'] == 'unusable':
                        raise PreflightError('remote_signin_unusable', diagnostic['signin']['reason'])
                    plan = provider_exec.build_plan(adapter, route, '', mode=mode,
                        cwd=str(worktree if mode == 'worktree_write' else task.get('cwd') or workspace), workspace_root=str(workspace),
                        worktree=str(worktree) if mode == 'worktree_write' else None, sandbox=task.get('sandbox'), network=task.get('network'),
                        capabilities=task.get('capabilities', []), add_dirs=task.get('add_dirs', []),
                        read_roots=read_roots, account=task.get('account'))
                    if plan['applied'].get('confinement') in {None, 'none'}:
                        raise PreflightError('remote_confinement_degraded', 'Remote placement requires effective confinement')
                routes.append(route)
            except (PreflightError, writer_lease.WorktreeLeaseError, OSError, ValueError, subprocess.TimeoutExpired) as exc:
                prompt_fixes = {
                    "prompt_unavailable": "Pass prompt text or prompt_file=<readable regular file inside the workspace>.",
                    "prompt_path_forbidden": PROMPT_PATH_FIX,
                    "credential_or_auth_store_denied": "Pass prompt text or a workspace prompt file outside credential and authentication stores.",
                    "prompt_hard_link_denied": "Pass prompt_file=<workspace file with one hard link>.",
                }
                errors.append({"task_id": task_id, "error": getattr(exc, "code", "protected_path_denied" if isinstance(exc, ValueError) else "worktree_invalid" if isinstance(exc, writer_lease.WorktreeLeaseError) else "preflight_unavailable"),
                               "fix": prompt_fixes.get(exc.code, str(exc)) if isinstance(exc, PreflightError) else str(exc) if isinstance(exc, ValueError) else "Pass a readable prompt and registered Git worktree; check adapter availability."})
    return ({"status": "rejected", "error": errors[0]["error"], "fix": errors[0]["fix"], "errors": errors}
            if errors else {"status": "validated", "routes": routes})

def confidential_refusal(adapter, model, route):
    """The fix for a confidential task whose resolved route may train on its prompt."""
    risk = exec_routing.disclosure_risk(adapter, model, route)
    return (f"{adapter}/{model} {risk}; fix: pass a paid non-training model, alias or route, "
            "or drop confidential") if risk else None

def export_knob_overrides(args: argparse.Namespace) -> None:
    """Validate `--knob` overrides and export them, with the risk tier, so the routing subprocesses resolve the same profile."""
    assignments = getattr(args, "knobs", None) or []
    args._knob_overrides = {}
    if assignments:  # an explicit override must be valid; a missing registry rejects it too
        args._knob_overrides = harness_profile.parse_assignments(harness_profile.load_registry(), assignments)
        for item in assignments:
            name, _, raw = item.partition("=")
            os.environ[harness_profile.env_name(name.strip())] = raw.strip()
    if args.risk_tier:
        try:
            tiers = harness_profile.load_registry().tier_order
        except harness_profile.ProfileError:
            tiers = ()
        if args.risk_tier in tiers:
            os.environ[harness_profile.RISK_TIER_ENV] = args.risk_tier

def profile_resolution(args: argparse.Namespace) -> dict[str, Any]:
    """The attempt's harness-profile stamp, resolved once; a broken profile falls back to defaults and says why."""
    if getattr(args, "_profile_stamp", None) is None:
        resolution, error = harness_profile.lenient(
            task_class=getattr(args, "task_class", None), risk_tier=getattr(args, "risk_tier", None),
            dispatch=getattr(args, "_knob_overrides", None))
        args._profile_stamp = resolution.stamp() if resolution else {"preset": None, "digest": None, "error": error}
    return args._profile_stamp

def profile_knob(args: argparse.Namespace, name: str, fallback: Any = None) -> Any:
    return profile_resolution(args).get("knobs", {}).get(name, fallback)

def _fabric_hosts_owner():
    import importlib.util
    name = 'provenant_dispatch_hosts'
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, Path(__file__).resolve().parents[3] / 'scripts/fabric_hosts.py')
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]
