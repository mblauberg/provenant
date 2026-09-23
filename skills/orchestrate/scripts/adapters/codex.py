import json

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
    # exec resume inherits its session sandbox; config overrides apply to both forms.
    if sandbox == "read-only" and network:
        command += [
            "-c",
            'default_permissions="provenant-read-only-network"',
            "-c",
            'permissions.provenant-read-only-network.extends=":read-only"',
            "-c",
            "permissions.provenant-read-only-network.network.enabled=true",
        ]
    elif p["resume_session"]:
        command += [
            "-c",
            "sandbox_mode="
            + json.dumps("danger-full-access" if sandbox == "full" else sandbox),
        ]
    else:
        command += ["-s", "danger-full-access" if sandbox == "full" else sandbox]
    if sandbox == "workspace-write":
        command += [
            "-c",
            "sandbox_workspace_write.network_access=" + str(network).lower(),
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
