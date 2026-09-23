import os

CLI = "claude"
PROMPT_TRANSPORT = "stdin"
STDIN = "prompt"
OUTPUT_FORMAT = "stream-json"
IDLE_READ = 600
IDLE_WRITE = 1800
EFFORT_FLAG = "--effort"
SESSION_KEYS = ("session_id",)
MODEL_SOURCE = "claude:init.model"  # fallback; answering models are read from the stream
SIGNATURES = (("usage_limited", r"you.ve hit your (?:usage|session|weekly) limit"),)


def argv(p):
    command = [
        CLI,
        "-p",
        "--bare" if os.environ.get("ANTHROPIC_API_KEY") or p["route"].get("endpoint_base_url") else "--safe-mode",
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
        command += ["--permission-mode", "plan", "--tools", "Read,Grep,Glob"]
    command += ["--system-prompt", p["boundary_prompt"]]
    for directory in p["applied"]["add_dirs"]:
        command += ["--add-dir", directory]
    if p["model"]:
        command += ["--model", p["model"]]
    if p["effort"]:
        command += [EFFORT_FLAG, p["effort"]]
    if p.get("context_ceiling"):
        command += ["--autocompact", str(p["context_ceiling"])]
    return command
