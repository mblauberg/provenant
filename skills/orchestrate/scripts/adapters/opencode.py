"""OpenCode v2 (`opencode run`). v1 (`--dir`, `--variant`, `opencode export`) is not supported."""
import re

CLI = "opencode"
MIN_MAJOR = 2
PROMPT_TRANSPORT = "argv"
STDIN = "null"
OUTPUT_FORMAT = "jsonl"
IDLE_READ = 600
IDLE_WRITE = 1800
# v2 has no --variant flag: effort travels as `provider/model#variant` in --model.
EFFORT_FLAG = "#variant"
SESSION_KEYS = ("sessionID", "session_id")
MODEL_SOURCE = "opencode:session-export.info.model"
UPGRADE_FIX = "upgrade OpenCode to v2 (`opencode upgrade`); Fabric no longer drives v1"
SIGNATURES = (
    # A bad `#variant` is the caller's input, not a missing model: keep it ahead of no-route.
    ("invalid_input", r"Variant unavailable for [^\n]+"),
    ("model_unavailable", r"FreeTierError|free tier can only be used from within OpenCode"
                          r"|Model unavailable:|Invalid model reference|provider\.no-route"),
    ("usage_limited", r"Individual quota reached|exhausted your capacity|(?:monthly|weekly|5.hour) usage limit reached"),
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
