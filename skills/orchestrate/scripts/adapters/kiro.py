CLI = "kiro-cli"
PROMPT_TRANSPORT = "argv"
STDIN = "null"
OUTPUT_FORMAT = "stream-json"
IDLE_READ = 600
IDLE_WRITE = 1800
EFFORT_FLAG = "--effort"
SESSION_KEYS = ("sessionId", "session_id", "conversation_id", "conversationId")
MODEL_SOURCE = "kiro:stream"
SIGNATURES = (("auth_required", r"not logged in|Login expired"),)


def argv(p):
    command = [CLI, "chat", "--no-interactive", "--output-format", OUTPUT_FORMAT]
    command += (
        ["--trust-all-tools"]
        if p["mode"] == "worktree_write"
        else ["--trust-tools=fs_read,grep,glob"]
    )
    if p["resume_session"]:
        command += ["--resume-id", p["resume_session"]]
    if p["model"]:
        command += ["--model", p["model"]]
    if p["effort"]:
        command += [EFFORT_FLAG, p["effort"]]
    return command + [p["boundary_prompt"] + "\n\n" + p["prompt"]]


def read_only_guarantee(route):
    """Only a fresh router-supplied negative probe can establish enforcement."""
    from datetime import UTC, datetime, timedelta

    probe = route.get("read_only_probe") or {}
    try:
        age = datetime.now(UTC) - datetime.fromisoformat(
            probe["checked_at"].replace("Z", "+00:00")
        )
        valid = (
            probe.get("cli_version") == route.get("cli_version")
            and bool(route.get("cli_version"))
            and timedelta(0) <= age < timedelta(hours=24)
            and probe.get("attempted_write") is True
            and probe.get("permission_denied") is True
            and probe.get("file_created") is False
        )
    except (ValueError, TypeError, KeyError, AttributeError):
        valid = False
    return "enforced" if valid else "prompt_only"
