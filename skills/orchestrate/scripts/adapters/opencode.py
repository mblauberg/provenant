CLI = "opencode"
PROMPT_TRANSPORT = "argv"
STDIN = "null"
OUTPUT_FORMAT = "jsonl"
IDLE_READ = 600
IDLE_WRITE = 1800
EFFORT_FLAG = "--variant"
SESSION_KEYS = ("sessionID", "session_id")
MODEL_SOURCE = "opencode:export.modelID"
SIGNATURES = (("usage_limited", r"Individual quota reached|exhausted your capacity"),)


def argv(p):
    command = [CLI, "run", "--format", "json", "--auto"]
    if p["worktree"]:
        command += ["--dir", p["worktree"]]
    if p["resume_session"]:
        command += ["-s", p["resume_session"]]
    if p["model"]:
        command += ["--model", p["model"]]
    if p["effort"]:
        command += [EFFORT_FLAG, p["effort"]]
    return command + [p["prompt"]]
