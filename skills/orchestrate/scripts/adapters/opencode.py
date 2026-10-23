"""OpenCode v2 (`opencode run`). v1 (`--dir`, `--variant`, `opencode export`) is not supported."""
import json
import re

CLI = "opencode"
MIN_MAJOR = 2
PROMPT_TRANSPORT = "argv"
STDIN = "null"
OUTPUT_FORMAT = "jsonl"
# v2 has no --variant flag: effort travels as `provider/model#variant` in --model.
EFFORT_FLAG = "#variant"
SESSION_KEYS = ("sessionID", "session_id")
MODEL_SOURCE = "opencode:session-export.info.model"
LOG_LEVEL = r"(?:^|\s|\[)(?:level=)?(DEBUG|INFO|WARN|ERROR)\b"
EXIT_AFTER_OUTPUT = True
EMPTY_FAILURE = "provider exited {exit_code} without final assistant output"
UPGRADE_FIX = "upgrade OpenCode to v2 (`opencode upgrade`); Fabric no longer drives v1"


def continues_turn(event):
    kind, part = event.get("type"), event.get("part")
    return kind in {"step_start", "tool_use"} or (
        kind == "step_finish" and isinstance(part, dict) and part.get("reason") == "tool-calls"
    )


def version_error(text):
    """A typed message when `opencode --version` output names a release before v2, else None."""
    match = re.search(r"(?<![\w.])v?(\d+)\.\d+(?:\.\d+)?(?![\w.])", text or "")
    if match and int(match[1]) < MIN_MAJOR:
        return f"OpenCode {match[0]} is unsupported; fix: {UPGRADE_FIX}"
    return None


def model_with_variant(model, effort):
    if not model or not effort or "#" in model:
        return model
    return model + "#" + effort


def argv(p):
    # --standalone: a private server per run, because the shared background service ignores the
    # per-dispatch OPENCODE_CONFIG_CONTENT permissions (a denied edit still wrote a file). The
    # process cwd (not a flag) selects the project. --print-logs keeps quota retries visible,
    # which can sleep for days without a JSON event.
    command = [CLI, "run", "--standalone", "--format", "json", "--auto", "--print-logs", "--log-level", "error"]
    if p["resume_session"]:
        command += ["-s", p["resume_session"]]
    if p["model"]:
        command += ["--model", model_with_variant(p["model"], p["effort"])]
    return command + [p["prompt"]]


def diagnostics(stderr):
    """Ignore startup chatter; prefer the first ERROR over the first WARN."""
    warning_line = ""
    diagnostic_lines = []
    error_line = ""
    for line in stderr.splitlines():
        level = re.search(LOG_LEVEL, line, re.I)
        if level and level[1].upper() in {"INFO", "DEBUG"}:
            continue
        diagnostic_lines.append(line)
        if not error_line and level and level[1].upper() == "ERROR":
            error_line = line.strip()
        if not warning_line and level and level[1].upper() == "WARN":
            warning_line = line.strip()
    diagnostics = "\n".join(diagnostic_lines)
    warning_line = error_line or warning_line
    return diagnostics, warning_line


def text_event(text):
    return {"type": "text", "part": {"text": text}}


def environment(plan):
    """Per-attempt permissions and startup controls for the standalone server."""
    bash_permission = {"*": "allow"} if plan["mode"] == "worktree_write" else {
        "*": "deny",
        "provenant-no-shell": "allow",
    }
    permission = {
        "edit": {"*": "allow" if plan["mode"] == "worktree_write" else "deny"},
        "bash": bash_permission,
        "question": "deny",
        "external_directory": {"*": "deny"},
        "webfetch": "deny" if plan.get("network_requested") is False else "allow",
    }
    # The sentinel matches no real command and exists only so Zen declares bash.
    opencode_config = {"permission": permission}
    if plan["applied"].get("confinement") == "sandbox-exec":
        # V2 replaces the whole server definition; a disabled entry still needs type and command.
        opencode_config["mcp"] = {"servers": {
            "fabric": {"type": "local", "command": ["provenant"], "disabled": True},
        }}
    environment = {"OPENCODE_CONFIG_CONTENT": json.dumps(opencode_config)}
    # v2 scans ~/.claude for Claude Code skills and CLAUDE.md, which the confinement profile
    # denies (startup fails with EPERM) and which holds the owner's private instructions.
    # Autoupdate and the filewatcher have no place in a supervised, sandboxed run.
    environment.update(OPENCODE_DISABLE_CLAUDE_CODE="1", OPENCODE_DISABLE_CLAUDE_CODE_SKILLS="1",
                       OPENCODE_DISABLE_AUTOUPDATE="1", OPENCODE_DISABLE_FILEWATCHER="1")
    return environment
