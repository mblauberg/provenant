CLI = "opencode"
PROMPT_TRANSPORT = "argv"
STDIN = "null"
OUTPUT_FORMAT = "jsonl"
IDLE_READ = 600
IDLE_WRITE = 1800
EFFORT_FLAG = "--variant"
SESSION_KEYS = ("sessionID", "session_id")
MODEL_SOURCE = "opencode:export.modelID"
SIGNATURES = (
    ("model_unavailable", r"FreeTierError|free tier can only be used from within OpenCode"),
    ("usage_limited", r"Individual quota reached|exhausted your capacity"),
)


def argv(p):
    command = [CLI, "run", "--format", "json", "--auto"]
    command += ["--dir", p["cwd"]]
    if p["resume_session"]:
        command += ["-s", p["resume_session"]]
    if p["model"]:
        command += ["--model", p["model"]]
    if p["effort"]:
        command += [EFFORT_FLAG, p["effort"]]
    return command + [p["prompt"]]
