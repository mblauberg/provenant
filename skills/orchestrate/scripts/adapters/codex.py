import json
import os
from pathlib import Path
import re
import sys
import tomllib
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


def config_policy(p):
    """`inherited` loads the operator's Codex config (MCP servers, plugins, hooks, rules, subagent
    settings) so a lane matches a native subagent; `clean` ignores it. Assurance runs, capability
    lanes (which run under their own CODEX_HOME) and CF_DISPATCH_CODEX_CONFIG=clean stay clean."""
    chosen = os.environ.get("CF_DISPATCH_CODEX_CONFIG", "")
    if chosen not in {"", "clean", "inherited"}:
        raise ValueError("CF_DISPATCH_CODEX_CONFIG must be clean or inherited")
    if p.get("intent") == "assurance" or p["applied"].get("capabilities") or chosen == "clean":
        return "clean"
    return "inherited"


def _mcp_server_table():
    path = Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex").expanduser() / "config.toml"
    try:
        servers = tomllib.loads(path.read_text(encoding="utf-8")).get("mcp_servers", {})
    except (OSError, tomllib.TOMLDecodeError):
        return {}
    return {name: server for name, server in servers.items()
            if isinstance(server, dict) and re.fullmatch(r"[A-Za-z0-9_-]+", name)}


def configured_mcp_servers():
    """Names of the MCP servers in the operator's Codex config, which a lane cannot prompt to approve."""
    return sorted(_mcp_server_table())


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
    return sorted(name for name, server in _mcp_server_table().items() if "url" not in server)


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
        for name in configured_mcp_servers():
            if name in disabled:
                command += ["-c", "mcp_servers." + name + ".enabled=false"]
            else:
                command += ["-c", "mcp_servers." + name + '.default_tools_approval_mode="approve"']
        if disabled:
            p["applied"]["mcp_disabled"] = disabled
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
