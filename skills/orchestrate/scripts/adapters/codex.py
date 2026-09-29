import json
from pathlib import Path
import sys

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


def read_only_writable_dirs(p):
    """Read-only add_dirs Codex may write: never one holding the cwd under review, nor one whose
    name Codex would read as a pattern."""
    cwd = Path(p["cwd"])
    return [path for path in p["applied"]["add_dirs"]
            if not cwd.is_relative_to(path) and not PERMISSION_PATTERN_CHARACTERS.intersection(path)]


def argv(p):
    command = [CLI, "exec"]
    if p["resume_session"]:
        command += ["resume", p["resume_session"]]
    command += [
        "--json",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
        "-c",
        'service_tier="default"',
        "-c",
        'approval_policy="never"',
    ]
    sandbox, network = p["applied"]["sandbox"], p["applied"]["network"]
    capabilities = p["applied"].get("capabilities", [])
    if sys.platform == "darwin" and sandbox != "full":
        command += ["-c", "allow_login_shell=false"]
    # exec resume inherits its session sandbox; config overrides apply to both forms.
    if capabilities:
        if p["resume_session"]:
            command += ["-c", 'sandbox_mode="danger-full-access"']
        else:
            command += ["-s", "danger-full-access"]
    elif sandbox == "read-only":
        # :read-only reads everywhere; the profile adds writes to the attempt's TMPDIR and to
        # add_dirs (a shared lock directory, say), so a reviewer can run a targeted test.
        writable = {":tmpdir": "write", **dict.fromkeys(read_only_writable_dirs(p), "write")}
        command += [
            "-c",
            'default_permissions="provenant-read-only"',
            "-c",
            'permissions.provenant-read-only.extends=":read-only"',
            "-c",
            "permissions.provenant-read-only.filesystem={"
            + ", ".join(json.dumps(path, ensure_ascii=False) + " = " + json.dumps(mode) for path, mode in writable.items())
            + "}",
        ]
        if network:
            command += ["-c", "permissions.provenant-read-only.network.enabled=true"]
    elif p["resume_session"]:
        command += [
            "-c",
            "sandbox_mode="
            + json.dumps("danger-full-access" if sandbox == "full" else sandbox),
        ]
    else:
        command += ["-s", "danger-full-access" if sandbox == "full" else sandbox]
    if sandbox == "workspace-write" and not capabilities:
        command += [
            "-c",
            "sandbox_workspace_write.network_access=" + str(network).lower(),
            # TMPDIR is the attempt's tmp, which holds XDG_CACHE_HOME and COREPACK_HOME.
            "-c",
            "sandbox_workspace_write.exclude_tmpdir_env_var=false",
        ]
        command += [
            "-c",
            "sandbox_workspace_write.writable_roots="
            + json.dumps(p["applied"]["add_dirs"]),
        ]
    if not p["resume_session"]:
        for directory in p["applied"]["add_dirs"]:
            command += ["--add-dir", directory]
        if p["worktree"]:
            command += ["--cd", p["worktree"]]
    route = p["route"]
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
