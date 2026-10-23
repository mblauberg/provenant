CLI = "cursor-agent"
PROMPT_TRANSPORT = "argv"
# An open, non-TTY pipe avoids Cursor treating /dev/null as an ended interactive session.
STDIN = "pipe"
OUTPUT_FORMAT = "stream-json"
EFFORT_FLAG = None
SESSION_KEYS = ("session_id", "sessionId")
MODEL_SOURCE = "cursor:init.model"


def argv(p):
    command = [CLI, "-p", "--output-format", OUTPUT_FORMAT, "--trust", "--approve-mcps"]
    command += (
        ["--force"]
        if p["mode"] == "worktree_write"
        else ["--mode", "ask", "--sandbox", "enabled"]
    )
    if p["resume_session"]:
        command += ["--resume", p["resume_session"]]
    if p["model"]:
        command += ["--model", p["model"]]
    return command + [p["prompt"]]
