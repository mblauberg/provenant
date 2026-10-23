import os

CLI = "claude"
PROMPT_TRANSPORT = "stdin"
STDIN = "prompt"
OUTPUT_FORMAT = "stream-json"
EFFORT_FLAG = "--effort"
SESSION_KEYS = ("session_id",)
MODEL_SOURCE = "claude:init.model"  # fallback; answering models are read from the stream


def uses_api_key(route):
    """A bare or endpoint lane signs in with a key; any other lane uses Claude's OAuth sign-in."""
    return bool(os.environ.get("ANTHROPIC_API_KEY") or route.get("endpoint_base_url"))


def argv(p):
    confined_shell = (
        p["mode"] == "read_only"
        and p["applied"].get("confinement") == "sandbox-exec"
        and p["applied"].get("guarantee") == "enforced"
    )
    boundary = p["boundary_prompt"]
    command = [
        CLI,
        "-p",
        "--bare" if uses_api_key(p["route"]) else "--safe-mode",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--permission-prompts",
        "none",
        "--output-format",
        OUTPUT_FORMAT,
        "--verbose",
    ]
    if p["resume_session"]:
        command += ["--resume", p["resume_session"]]
    else:
        command += ["--session-id", p["session_id"]]
    if p["mode"] == "worktree_write":
        command += [
            "--add-dir",
            p["cwd"],
            "--permission-mode",
            "acceptEdits",
            "--allowedTools",
            "Bash,Edit,Write,MultiEdit,NotebookEdit,Read,Grep,Glob",
        ]
    else:
        tools = "Read,Grep,Glob,Bash" if confined_shell else "Read,Grep,Glob"
        command += ["--permission-mode", "default", "--tools", tools]
        if confined_shell:
            # Listing Bash exposes it; default mode still needs approval under -p.
            command += ["--allowedTools", "Bash"]
            boundary = boundary.replace(
                "Use file-reading tools only.",
                "Read-only shell commands, including diffs and targeted tests, are allowed.",
            )
    command += ["--system-prompt", boundary]
    for directory in p["applied"]["add_dirs"]:
        command += ["--add-dir", directory]
    if p["model"]:
        command += ["--model", p["model"]]
    if p["effort"]:
        command += [EFFORT_FLAG, p["effort"]]
    if p.get("context_ceiling"):
        command += ["--autocompact", str(p["context_ceiling"])]
    return command
