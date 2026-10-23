CLI = "copilot"
PROMPT_TRANSPORT = "argv"
STDIN = "null"
OUTPUT_FORMAT = "json"
EFFORT_FLAG = "--effort"
SESSION_KEYS = ("session_id",)
MODEL_SOURCE = None


def argv(p):
    command = [
        CLI,
        "-p",
        p["prompt"],
        "--output-format",
        OUTPUT_FORMAT,
        "--no-ask-user",
    ]
    command += (
        ["--allow-all"]
        if p["mode"] == "worktree_write"
        else [
            "--mode",
            "plan",
            "--disable-builtin-mcps",
            "--available-tools=",
            "--disallow-temp-dir",
        ]
    )
    if p["model"]:
        command += ["--model", p["model"]]
    if p["effort"]:
        command += [EFFORT_FLAG, p["effort"]]
    return command
