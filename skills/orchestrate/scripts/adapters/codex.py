import json
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
    elif sandbox == "read-only" and network:
        command += permissions_profile(p, ":read-only", True)
    elif sandbox == "workspace-write":
        # A permissions profile names single Git paths inside the common directory, which
        # sandbox_workspace_write.writable_roots cannot; the nearest entry wins.
        filesystem = p["applied"].get("write_boundary", {}).get("filesystem") or {}
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
