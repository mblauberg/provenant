import json
import os
from pathlib import Path
import re
import subprocess
import sys
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


def _discover_mcp_servers(cwd):
    """Effective MCP servers as Codex itself resolves them for `cwd`: user config, trusted project
    `.codex/config.toml` layers and plugin servers. Returns {name: is_stdio}, or None on any error."""
    try:
        done = subprocess.run([CLI, "mcp", "list", "--json"], cwd=cwd, capture_output=True, text=True,
                              timeout=30, stdin=subprocess.DEVNULL)
        if done.returncode != 0:
            return None
        return {entry["name"]: entry.get("transport", {}).get("type") == "stdio"
                for entry in json.loads(done.stdout) if entry.get("enabled", True)}
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
        return None


def mcp_inventory(p):
    """{name: is_stdio} for the lane's working directory; None when discovery failed. Held on the
    plan only while the argv is built (`argv` removes it, since the plan is serialised)."""
    if "_mcp_inventory" not in p:
        p["_mcp_inventory"] = _discover_mcp_servers(p.get("worktree") or p.get("cwd") or os.getcwd())
    return p["_mcp_inventory"]


def config_policy(p):
    """`inherited` loads the operator's Codex config (MCP servers, plugins, hooks, rules, subagent
    settings) so a lane matches a native subagent; `clean` ignores it. Assurance runs, capability
    lanes (which run under their own CODEX_HOME), CF_DISPATCH_CODEX_CONFIG=clean and a lane that is
    not a worktree writer and cannot enumerate its MCP servers stay clean (fail closed)."""
    chosen = os.environ.get("CF_DISPATCH_CODEX_CONFIG", "")
    if chosen not in {"", "clean", "inherited"}:
        raise ValueError("CF_DISPATCH_CODEX_CONFIG must be clean or inherited")
    if p.get("intent") == "assurance" or p["applied"].get("capabilities") or chosen == "clean":
        return "clean"
    if p.get("mode") != "worktree_write" and mcp_inventory(p) is None:
        p["applied"]["config_note"] = "MCP server discovery failed; a non-writer lane cannot inherit blind"
        return "clean"
    return "inherited"


def mcp_key(name):
    return "mcp_servers." + (name if re.fullmatch(r"[A-Za-z0-9_-]+", name) else json.dumps(name))


def configured_mcp_servers(p):
    """Names of the MCP servers Codex would load for the lane, which cannot prompt to approve them."""
    return sorted(mcp_inventory(p) or {})


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


def disabled_mcp_servers(p):
    """Inherited MCP servers a lane must not start: stdio servers run outside Codex's command
    sandbox, so only a worktree writer keeps them. A remote (url) server starts no local process."""
    if p.get("mode") == "worktree_write":
        return []
    return sorted(name for name, stdio in (mcp_inventory(p) or {}).items() if stdio)


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
        "-c", "default_permissions=" + json.dumps(name),
        "-c", "permissions." + name + ".extends=" + json.dumps(extends),
        "-c", "permissions." + name + ".network.enabled=" + str(bool(network)).lower(),
    ]
    if filesystem is not None:
        command += ["-c", "permissions." + name + ".filesystem={" + ", ".join(
            json.dumps(path) + " = " + json.dumps(access) for path, access in filesystem.items()) + "}"]
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
    inherited = config_policy(p) == "inherited"
    if not inherited:
        command += ["--ignore-user-config", "--ignore-rules"]
    else:
        # A lane has no one to approve a call, and the operator's notify hook is a desktop alert.
        command += ["-c", "notify=[]", "-c", 'web_search="live"']
        disabled = disabled_mcp_servers(p)
        kept = []
        for name in configured_mcp_servers(p):
            if name in disabled:
                command += ["-c", mcp_key(name) + ".enabled=false"]
            else:
                command += ["-c", mcp_key(name) + '.default_tools_approval_mode="approve"']
                kept.append(name)
        if disabled:
            p["applied"]["mcp_disabled"] = disabled
        if p.get("mode") == "worktree_write":
            # Accepted boundary (ADR 0023): as in a native Codex session, hooks and stdio MCP servers
            # run with the operator's privileges, outside the command sandbox.
            p["applied"]["hooks_unconfined"] = True
            stdio = [name for name in kept if (mcp_inventory(p) or {}).get(name)]
            if stdio:
                p["applied"]["mcp_unconfined"] = stdio
        else:
            # Hooks (user, project, plugin) run outside the permission profile; none may start.
            command += ["-c", "features.hooks=false"]
            p["applied"]["hooks_disabled"] = True
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
            + json.dumps("danger-full-access" if sandbox == "full" else sandbox),
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
                    + json.dumps(value),
                ]
        command += ["-c", 'model_provider="provenant_endpoint"']
    if p["model"]:
        command += ["-m", p["model"]]
    if p["effort"]:
        command += ["-c", EFFORT_FLAG + "=" + json.dumps(p["effort"])]
    if p.get("context_ceiling"):
        command += ["-c", "model_auto_compact_token_limit=" + str(int(p["context_ceiling"]))]
    return command + ["-"]
