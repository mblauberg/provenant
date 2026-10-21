CLI = "agy"
PROMPT_TRANSPORT = "argv"
STDIN = "null"
OUTPUT_FORMAT = "stream-json"
IDLE_READ = 600
IDLE_WRITE = 1800
EFFORT_FLAG = "--effort"
SESSION_KEYS = ("session_id", "conversation_id", "conversationId")
MODEL_SOURCE = "agy:init.model|log"
SIGNATURES = (("usage_limited", r"RESOURCE_EXHAUSTED|Resets in \d+h"),)


def argv(p):
    command = [
        CLI,
        "--output-format",
        OUTPUT_FORMAT,
        "--disable-slash-commands",
        "--print-timeout",
        str(max(1, int(p["timeout_seconds"]))) + "s",
    ]
    if p["mode"] == "worktree_write":
        command += ["--dangerously-skip-permissions"]
    elif p["agy_sandbox"]:
        command += ["--sandbox"]
    if p["resume_session"]:
        command += ["--conversation", p["resume_session"]]
    if p["model"]:
        command += ["--model", p["model"]]
    if p["effort"]:
        command += [EFFORT_FLAG, p["effort"]]
    for directory in p["applied"]["add_dirs"]:
        command += ["--add-dir", directory]
    return command + ["--print", p["boundary_prompt"] + "\nTask:\n" + p["prompt"]]
