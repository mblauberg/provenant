import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

CLI = "codex"
PROMPT_TRANSPORT = "stdin"
STDIN = "prompt"
OUTPUT_FORMAT = "jsonl"
IDLE_READ = 600
IDLE_WRITE = 1800
EFFORT_FLAG = "model_reasoning_effort"
SESSION_KEYS = ("thread_id",)
MODEL_SOURCE = "codex:rollout.turn_context.model"
SIGNATURES = (("usage_limited", r"usage limit|try again at \d"),)


# Codex reads these in a permission path as a pattern (it strips a trailing /** outright), so a
# directory spelt with them could grant more than itself.
PERMISSION_PATTERN_CHARACTERS = frozenset("*?[]{}")


# Discovery must finish well inside the planner's own deadline (30s), or a hang starves the plan.
DISCOVERY_SECONDS = 8
# Transport fields that carry data, not a command. Any other populated field (an
# `http_headers_helper`, or one a later Codex adds) is treated as something that executes.
INERT_TRANSPORT_FIELDS = frozenset({"type", "url", "bearer_token_env_var", "http_headers", "env_http_headers"})


def _toml(value):
    # A JSON scalar is a TOML one, except that ensure_ascii would spell a non-BMP character as a
    # surrogate pair, which TOML rejects.
    return json.dumps(value, ensure_ascii=False)

def _executes(transport):
    """A server whose transport starts a local process, directly (stdio) or through a command field."""
    if transport.get("type") not in {"stdio", "streamable_http"}:
        return True
    if transport["type"] == "stdio":
        return True
    return any(value for key, value in transport.items() if key not in INERT_TRANSPORT_FIELDS)


def _discover_mcp_servers(cwd):
    """Effective MCP servers as Codex itself resolves them for `cwd`: user config, trusted project
    `.codex/config.toml` layers and plugin servers. Returns {name: {"exec": bool, "plugin": bool}},
    or None on any error or when the shared DISCOVERY_SECONDS budget runs out. A plugin server is one
    that vanishes under `features.plugins=false`; it has no config-table transport to override."""
    deadline = time.monotonic() + DISCOVERY_SECONDS

    def listing(*extra):
        done = subprocess.run([CLI, "mcp", "list", "--json", *extra], cwd=cwd, capture_output=True,
                              text=True, timeout=max(0.1, deadline - time.monotonic()),
                              stdin=subprocess.DEVNULL)
        if done.returncode != 0:
            raise ValueError("codex mcp list failed")
        return {entry["name"]: _executes(entry.get("transport") or {})
                for entry in json.loads(done.stdout) if entry.get("enabled", True)}

    try:
        everything = listing()
        without_plugins = listing("-c", "features.plugins=false")
        return {name: {"exec": executes, "plugin": name not in without_plugins}
                for name, executes in everything.items()}
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
        return None


def mcp_inventory(p):
    """{name: {"exec", "plugin"}} for the lane's working directory; None when discovery failed. Held
    on the plan only while the argv is built (`argv` removes it, since the plan is serialised)."""
    if "_mcp_inventory" not in p:
        p["_mcp_inventory"] = _discover_mcp_servers(p.get("worktree") or p.get("cwd") or os.getcwd())
    return p["_mcp_inventory"]


def _decide(p):
    """Choose the lane's config policy once: `inherited` loads the operator's Codex config (MCP
    servers, plugins, hooks, rules, subagent settings) so a lane matches a native subagent; `clean`
    ignores it. Assurance runs, capability lanes (their own CODEX_HOME) and CF_DISPATCH_CODEX_CONFIG=clean
    stay clean, as does a non-writer lane that cannot enumerate its MCP servers (fail closed).

    Returns {"config", "args", "applied"}: the argv segment and receipt fields that go together."""
    chosen = os.environ.get("CF_DISPATCH_CODEX_CONFIG", "")
    if chosen not in {"", "clean", "inherited"}:
        raise ValueError("CF_DISPATCH_CODEX_CONFIG must be clean or inherited")
    clean = {"config": "clean", "args": ["--ignore-user-config", "--ignore-rules"],
             "applied": {"config": "clean"}}
    if p.get("intent") == "assurance" or p["applied"].get("capabilities") or chosen == "clean":
        return clean
    writer = p.get("mode") == "worktree_write"
    inventory = mcp_inventory(p)
    applied = {"config": "inherited"}
    if inventory is None:
        if not writer:
            clean["applied"]["config_note"] = "MCP server discovery failed; a non-writer lane cannot inherit blind"
            return clean
        applied["config_note"] = "MCP server discovery failed; no server approvals set"
        inventory = {}
    # A lane has no one to approve a call, and the operator's notify hook is a desktop alert.
    args = ["-c", "notify=[]", "-c", 'web_search="live"']
    entries, plugin_servers, disabled, unconfined = {}, [], [], []
    for name in sorted(inventory):
        info = inventory[name]
        if not _toml_key_safe(name):
            # Cannot be spelt in a -c override: a non-writer lane would run it unconfined.
            if writer:
                applied.setdefault("mcp_unapproved", []).append(name)
                continue
            clean["applied"]["config_note"] = "MCP server name cannot be overridden: " + json.dumps(name)
            return clean
        if info["plugin"]:
            plugin_servers.append(name)  # no config-table transport: only features.plugins reaches it
        elif info["exec"] and not writer:
            entries[name] = {"enabled": False}
            disabled.append(name)
        else:
            entries[name] = {"default_tools_approval_mode": "approve"}
        if writer and info["exec"]:
            unconfined.append(name)
    if entries:
        args += ["-c", "mcp_servers=" + _toml_table(entries)]
    if writer:
        # Accepted boundary (ADR 0023): as in a native Codex session, hooks and stdio MCP servers
        # run with the operator's privileges, outside the command sandbox.
        applied["hooks_unconfined"] = True
        if unconfined:
            applied["mcp_unconfined"] = unconfined
        if plugin_servers:
            applied.setdefault("mcp_unapproved", []).extend(plugin_servers)
    else:
        # Hooks (user, project, plugin) run outside the permission profile; none may start.
        args += ["-c", "features.hooks=false"]
        applied["hooks_disabled"] = True
        if any(inventory[name]["exec"] for name in plugin_servers):
            args += ["-c", "features.plugins=false"]
            applied["plugins_disabled"] = True
            disabled += [name for name in plugin_servers if inventory[name]["exec"]]
    if disabled:
        applied["mcp_disabled"] = sorted(disabled)
    return {"config": "inherited", "args": args, "applied": applied}


def _toml_key_safe(name):
    return name.isprintable()


def _toml_table(entries):
    """An inline TOML table. A parent-table override is used because Codex splits a dotted `-c` key
    on every dot, quoted or not, so `mcp_servers."a.b".enabled=false` cannot name a dotted server."""
    return "{" + ", ".join(
        _toml(name) + " = {" + ", ".join(
            key + " = " + _toml(value)  # JSON booleans and strings are TOML ones
            for key, value in fields.items()) + "}"
        for name, fields in entries.items()) + "}"


def config_decision(p):
    """The lane's config decision, taken on first use and reused by every rebuild of the argv (a
    context-ceiling change, an effort retry, a resume relaunch), so receipts cannot contradict argv."""
    if "codex_config" not in p:
        p["codex_config"] = _decide(p)
    p["applied"].update(p["codex_config"]["applied"])
    return p["codex_config"]


def config_policy(p):
    return config_decision(p)["config"]


def provenant_state_dir(p):
    """Provenant's own per-user state (admission lock): a lane's nested dispatch needs to write it.

    Returns (path, note). The grant is skipped, with a note, when the directory is a link, holds a
    glob character, or equals, contains or sits inside the workspace, cwd, worktree or any other
    root the lane reads or writes under its own rules. The directory is created at execution time.
    """
    state_home = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state")
    if not state_home.is_absolute():
        state_home = Path.home() / ".local/state"
    path = state_home / "provenant"
    if path.is_symlink():
        return None, "state directory is a symlink"
    path = path.resolve()
    if path.exists() and not path.is_dir():
        return None, "state directory is not a directory"
    if PERMISSION_PATTERN_CHARACTERS.intersection(str(path)):
        return None, "state directory holds a glob character"
    roots = [p.get("cwd"), p.get("workspace_root"), p.get("worktree"), *p["applied"].get("add_dirs", []),
             *p["applied"].get("protected_paths", []), *p.get("protected_paths", [])]
    for root in filter(None, roots):
        root = Path(root).expanduser().resolve()
        if path == root or path.is_relative_to(root) or root.is_relative_to(path):
            return None, "state directory overlaps " + str(root)
    return str(path), None


def read_only_writable_dirs(p):
    """Read-only add_dirs Codex may write: never one holding the cwd under review, nor one whose
    name Codex would read as a pattern."""
    cwd = Path(p["cwd"])
    return [path for path in p["applied"]["add_dirs"]
            if not cwd.is_relative_to(path) and not PERMISSION_PATTERN_CHARACTERS.intersection(path)]


def permissions_profile(p, extends, network, filesystem=None):
    """Select a per-plan permissions profile built only from these overrides.

    Codex merges config tables, so a fixed profile name would inherit any filesystem grant a
    system or user config adds under that name; a name unique to the plan has none to inherit.
    """
    name = p["applied"].get("write_boundary", {}).get("profile") or "provenant-" + uuid.uuid4().hex
    command = [
        "-c", "default_permissions=" + _toml(name),
        "-c", "permissions." + name + ".extends=" + _toml(extends),
        "-c", "permissions." + name + ".network.enabled=" + str(bool(network)).lower(),
    ]
    if filesystem is not None:
        command += ["-c", "permissions." + name + ".filesystem={" + ", ".join(
            _toml(path) + " = " + _toml(access) for path, access in filesystem.items()) + "}"]
    return command


def argv(p):
    try:
        return _argv(p)
    finally:
        p.pop("_mcp_inventory", None)


def _argv(p):
    command = [CLI, "exec"]
    if p["resume_session"]:
        command += ["resume", p["resume_session"]]
    command += [
        "--json",
        "--skip-git-repo-check",
    ]
    decision = config_decision(p)
    inherited = decision["config"] == "inherited"
    command += decision["args"]
    command += [
        "-c",
        'service_tier="default"',
        "-c",
        'approval_policy="never"',
    ]
    sandbox, network = p["applied"]["sandbox"], p["applied"]["network"]
    capabilities = p["applied"].get("capabilities", [])
    if sys.platform == "darwin" and sandbox != "full" and not inherited:
        command += ["-c", "allow_login_shell=false"]
    state_dir, state_note = provenant_state_dir(p)
    boundary = p["applied"].get("write_boundary")
    if boundary is not None and (state_dir or state_note):
        boundary.update({"state_dir": state_dir} if state_dir else {"state_dir_skipped": state_note})
    # exec resume inherits its session sandbox; config overrides apply to both forms.
    if capabilities:
        if p["resume_session"]:
            command += ["-c", 'sandbox_mode="danger-full-access"']
        else:
            command += ["-s", "danger-full-access"]
    elif sandbox == "read-only":
        # :read-only reads everywhere; the profile adds writes to the attempt's TMPDIR and to
        # add_dirs (a shared lock directory, say), so a reviewer can run a targeted test.
        writable = {":tmpdir": "write", **dict.fromkeys(read_only_writable_dirs(p), "write"),
                    **({state_dir: "write"} if state_dir else {})}
        command += permissions_profile(p, ":read-only", network, writable)
    elif sandbox == "workspace-write":
        # A permissions profile names single Git paths inside the common directory, which
        # sandbox_workspace_write.writable_roots cannot; the nearest entry wins.
        filesystem = {**(p["applied"].get("write_boundary", {}).get("filesystem") or {}),
                      **({state_dir: "write"} if state_dir else {})}
        command += permissions_profile(p, ":workspace", network, filesystem)
    elif p["resume_session"]:
        command += [
            "-c",
            "sandbox_mode="
            + _toml("danger-full-access" if sandbox == "full" else sandbox),
        ]
    else:
        command += ["-s", "danger-full-access" if sandbox == "full" else sandbox]
    if not p["resume_session"]:
        for directory in p["applied"]["add_dirs"]:
            command += ["--add-dir", directory]
        if p["worktree"]:
            command += ["--cd", p["worktree"]]
    route = p["route"]
    if not (route.get("endpoint_base_url") and route.get("endpoint_token_env")):
        # A user's model_provider or openai_base_url must not redirect a native route.
        command += ["-c", 'model_provider="openai"', "-c", 'openai_base_url=""']
    if route.get("endpoint_base_url") and route.get("endpoint_token_env"):
        for key, value in [
            ("name", route.get("endpoint_profile", "endpoint")),
            ("base_url", route["endpoint_base_url"]),
            ("env_key", route["endpoint_token_env"]),
            ("wire_api", route.get("endpoint_wire_api")),
        ]:
            if value:
                command += [
                    "-c",
                    "model_providers.provenant_endpoint."
                    + key
                    + "="
                    + _toml(value),
                ]
        command += ["-c", 'model_provider="provenant_endpoint"']
    if p["model"]:
        command += ["-m", p["model"]]
    if p["effort"]:
        command += ["-c", EFFORT_FLAG + "=" + _toml(p["effort"])]
    if p.get("context_ceiling"):
        command += ["-c", "model_auto_compact_token_limit=" + str(int(p["context_ceiling"]))]
    return command + ["-"]
