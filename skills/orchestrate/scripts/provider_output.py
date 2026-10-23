"""Provider output parsing, classification, and model identity helpers."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from adapters import profile

PERMISSION_DENIAL_PATTERN = r"permission denied|permission blocked|not allowed to|tool required the .+ permission|denied_actions"

INVALID_INPUT_PATTERN = r"invalid model selection|--effort is not supported for model"

# First matching diagnostic wins. Adapter rules keep their former priority,
# including the two permission patterns on either side of auth/rate limits.
FAILURE_PATTERNS = [
    ("invalid_input", INVALID_INPUT_PATTERN, ()),
    ("quota_window", r"RESOURCE_EXHAUSTED|quota exceeded", ()),
    ("usage_limited", r"you.ve hit your (?:usage|session|weekly) limit", ("claude",)),
    ("usage_limited", r"usage limit|try again at \d", ("codex",)),
    ("usage_limited", r"Resets in \d+h", ("agy",)),
    ("auth_required", r"Login expired", ("cursor",)),
    ("auth_required", r"not logged in|Login expired", ("kiro",)),
    ("auth_required", r"not logged in", ("copilot",)),
    ("permission_blocked", r"\bEPERM\b|operation not permitted|sandbox[^\n]*denied", ("opencode",)),
    ("invalid_input", r"Variant unavailable for [^\n]+", ("opencode",)),
    ("model_unavailable", r"FreeTierError|free tier can only be used from within OpenCode"
     r"|Model unavailable:|Invalid model reference|provider\.no-route", ("opencode",)),
    ("usage_limited", r"Individual quota reached|exhausted your capacity|(?:monthly|weekly|5.hour) usage limit reached", ("opencode",)),
    ("permission_blocked", PERMISSION_DENIAL_PATTERN, ()),
    ("usage_limited", r"you.ve hit your (?:usage|session|weekly) limit|usage limit|session limit|weekly limit|individual quota reached|insufficient_quota|FreeUsageLimitError|exhausted your capacity", ()),
    ("auth_required", r"authentication required|please sign in|please(?: run)? login|not logged in|not authenticated|unauthenticated|unauthorized|login expired|\b401\b", ()),
    ("rate_limited", r"rate.?limit|too many requests|overloaded|model[^\n]*\bat capacity\b|\b(?:429|529)\b|per[ -](?:minute|second)", ()),
    ("model_unavailable", r"model[^\n]*(?:unavailable|not available|not found|unsupported|does not exist)|unknown model", ()),
    ("permission_blocked", r"\b403\b|forbidden", ()),
    ("provider_transport", r"reconnecting|stream disconnected|failed to load workspace requirements|transport (?:error|fail)|connection (?:reset|closed)|error sending request", ()),
]

STATUS_ERROR_CLASSES = {
    "ok": "none", "succeeded": "none", "input_required": "none",
    "permission_blocked": "sandbox_denied", "usage_limited": "usage_limited",
    "auth_required": "auth", "rate_limited": "rate_limited", "model_unavailable": "model_unavailable",
    "timed_out": "timeout", "startup_timeout": "timeout", "stalled": "timeout",
    "lost": "provider_error", "cancelled": "cancelled",
    "provider_lost": "provider_error", "host_disk_full": "host_resource",
}

ERROR_CLASS_ALIASES = {
    "permission_blocked": "sandbox_denied", "usage_limited": "usage_limited", "quota_exhausted": "usage_limited",
    "auth_required": "auth", "auth_expired": "auth", "rate_limited": "rate_limited",
    "model_unavailable": "model_unavailable", "pinned_model_substitution": "model_unavailable",
    "provider_lost": "provider_error", "host_disk_full": "host_resource",
    "provider_transport": "transient", "timed_out": "timeout", "startup_timeout": "timeout",
    "stalled": "timeout", "print_timeout": "timeout", "idle_watchdog": "timeout",
    "startup_watchdog": "timeout", "wall_clock": "timeout", "lost": "provider_error",
    "cancelled": "cancelled", "empty_output": "empty_output", "fabric_mcp_unavailable": "fabric_mcp_unavailable",
}


def classify_failure(adapter: str, failure_text: str) -> str | None:
    """Select the ordered signature for provider diagnostics, never assistant prose."""
    for signature, pattern, adapters in FAILURE_PATTERNS:
        if (not adapters or adapter in adapters) and re.search(pattern, failure_text, re.I):
            if signature == "quota_window":
                return "rate_limited" if quota_window_is_short(failure_text) else "usage_limited"
            return signature
    return None


def error_class(status, signature=None):
    """Stable telemetry only; routing and health continue to use status/signature."""
    # Foreign evidence can carry any JSON; only strings name a class.
    status = status if isinstance(status, str) else None
    signature = signature if isinstance(signature, str) else None
    # Generic failure statuses fall back only after the more specific signature.
    fallback = "provider_error" if status in {"failed", "rejected", "tool_missing", "output_identity_invalid", "output_write_error"} else "unknown"
    return STATUS_ERROR_CLASSES.get(status, ERROR_CLASS_ALIASES.get(signature, fallback))


def _variant_fix(failure_text):
    """The fix for a rejected input; OpenCode's bad variant names the model and the variant to drop."""
    found = re.search(r"Variant unavailable for ([^\s:\"]+): ([^\s\"}]+)", failure_text)
    if found:
        return (f"{found[1]} has no `{found[2]}` variant; drop --effort or `#{found[2]}`, "
                "or pass a variant that model supports")
    return failure_text.strip()[:400]


def _model_unavailable_fix(plan):
    route = plan.get("route") or {}
    if route.get("identity_source") != "passed-through":
        return "choose another model"
    try:
        try:
            from . import exec_routing
        except ImportError:
            import exec_routing
        route = exec_routing._model_route_module()
        catalogue = route.load_catalog()
        adapter = catalogue.get("adapters", {}).get(plan.get("adapter"), {})
        registered = route.registered_model_ids(adapter)
    except Exception:
        registered = []
    listing = "; list every id with `opencode models`" if plan.get("adapter") == "opencode" else ""
    if registered:
        choices = ", ".join(registered[:6])
        if len(registered) > 6:
            choices += ", …"
        return "choose a registered model: " + choices + listing
    return "choose another model" + listing


def _objects(value):
    if isinstance(value, dict):
        yield value
        for item in value.values():
            yield from _objects(item)
    elif isinstance(value, list):
        for item in value:
            yield from _objects(item)


def reset_time(text, events, at=None):
    at = at or datetime.now().astimezone()
    for event in events:
        for item in _objects(event):
            epoch = item.get("resetsAt")
            if isinstance(epoch, (int, float)) and not isinstance(epoch, bool):
                try:
                    return (
                        datetime.fromtimestamp(epoch, UTC)
                        .isoformat()
                        .replace("+00:00", "Z")
                    )
                except (ValueError, OverflowError, OSError):
                    pass
    days = re.search(r"Resets in\s+(\d+)\s*d(?:ays?)?\b", text, re.I)
    if days:
        return (at + timedelta(days=int(days[1]))).astimezone(UTC).isoformat().replace("+00:00", "Z")
    hours = re.search(r"Resets in\s+(\d+)h(?:\s*(\d+)m)?", text, re.I)
    if hours:
        return (
            (at + timedelta(hours=int(hours[1]), minutes=int(hours[2] or 0)))
            .astimezone(UTC)
            .isoformat()
            .replace("+00:00", "Z")
        )
    local = re.search(r"try again at\s+(\d{1,2}):(\d{2})\s*(AM|PM)", text, re.I)
    if local:
        hour = int(local[1]) % 12 + (12 if local[3].upper() == "PM" else 0)
        candidate = at.replace(hour=hour, minute=int(local[2]), second=0, microsecond=0)
        if candidate <= at:
            candidate += timedelta(days=1)
        return candidate.astimezone(UTC).isoformat().replace("+00:00", "Z")
    return None


def quota_window_is_short(failure_text):
    """True when the quota phrase names a minute or second window and no longer one."""
    start = re.search(r"quota exceeded", failure_text, re.I)
    start = start or re.search(r"RESOURCE_EXHAUSTED", failure_text)
    # Bounded to the sentence or JSON string; a quoted metric name does not end it.
    phrase = re.split(r'\.\s|\n|"\s*[,}\]]', failure_text[start.start():][:240], maxsplit=1)[0] if start else ""
    long_window = (re.search(r"per[ -](?:day|hour)\b|/\s*(?:day|hr|hour)\b", phrase, re.I)
                   or re.search(r"Per(?:Day|Hour)(?![a-z])", failure_text))
    short_window = (re.search(r"per[ -](?:minute|second)\b|/\s*(?:min(?:ute)?|sec(?:ond)?)\b", phrase, re.I)
                    or re.search(r"Per(?:Minute|Second)(?![a-z])", failure_text))
    return bool(short_window and not long_window)


def parse_output(adapter, stdout, stderr="", exit_code=0, *, at=None):
    """Interpret structured failures before diagnostic signatures; never classify answer prose."""
    config = profile(adapter)
    result = {
        "status": "failed",
        "text": "",
        "session_id": None,
        "observed_model": None,
        "reset_at": None,
        "retry_after": None,
        "signature": None,
        "excerpt": "",
        "terminal": False,
        "question": None,
    }
    continues_turn = getattr(config, "continues_turn", lambda event: False)
    events, plain, parts, errors = [], [], [], []
    retry_errors = []
    duplicate_json = False

    def no_duplicates(pairs):
        nonlocal duplicate_json
        value = {}
        for key, item in pairs:
            if key in value:
                duplicate_json = True
            value[key] = item
        return value

    terminal_text = None
    provider_message = None
    partial = False
    try:
        whole = json.loads(stdout, object_pairs_hook=no_duplicates)
        if isinstance(whole, dict):
            events = [whole]
            if "type" not in whole and not (adapter == "agy" and "status" in whole):
                plain = [stdout]
        else:
            plain = []
    except ValueError:
        for line in stdout.splitlines(keepends=True):
            try:
                event = json.loads(line, object_pairs_hook=no_duplicates)
            except ValueError:
                plain.append(line)
                continue
            if isinstance(event, dict):
                events.append(event)
    if adapter == "agy" and (
        duplicate_json
        or "\ufffd" in stdout
        or (plain and any("status" in event for event in events))
    ):
        errors.append("invalid agy envelope")
    for event in events:
        kind = event.get("type") or ""
        if continues_turn(event):
            parts.clear()
            result["terminal"] = False
        if not kind and isinstance(event.get("event"), str):
            kind = event["event"]  # agy names the event kind `event`
        if adapter == "claude" and kind == "assistant" and event.get("parent_tool_use_id") is not None:
            continue
        if adapter == "kiro" and event.get("jsonrpc") == "2.0":
            for item in _objects(event):
                if item.get("sessionUpdate") == "agent_message_chunk":
                    content = item.get("content", {})
                    if isinstance(content, dict) and isinstance(
                        content.get("text"), str
                    ):
                        parts.append(content["text"])
                if item.get("category") == "model" and isinstance(
                    item.get("currentValue"), str
                ):
                    result["observed_model"] = item["currentValue"]
            if isinstance(event.get("result"), dict) and event["result"].get(
                "stopReason"
            ):
                result["terminal"] = True
            if event.get("error"):
                errors.append(json.dumps(event["error"]))
        # kiro-cli --agent-engine v2 wraps the same ACP updates in typed events.
        if adapter == "kiro" and kind in {"sessionUpdate", "runFinished"}:
            data = event.get("data") if isinstance(event.get("data"), dict) else {}
            update = data.get("update") if isinstance(data.get("update"), dict) else {}
            content = update.get("content") if isinstance(update.get("content"), dict) else {}
            if update.get("sessionUpdate") == "agent_message_chunk" and isinstance(content.get("text"), str):
                parts.append(content["text"])
            if kind == "runFinished":
                result["terminal"] = True
                if isinstance(data.get("finalText"), str):
                    terminal_text = data["finalText"]
                if str(data.get("status", "success")).lower() != "success":
                    errors.append(json.dumps(data, ensure_ascii=False)[:400])
        for item in _objects(event):
            # Claude nests tool output in its events, so only the event's own session id counts.
            for key in (config.SESSION_KEYS if adapter != "claude" or item is event else ()):
                if isinstance(item.get(key), str):
                    result["session_id"] = item[key]
            if isinstance(item.get("retry_after"), (int, float)):
                result["retry_after"] = item["retry_after"]
        if adapter in {"claude", "cursor", "agy", "kiro"}:
            if (
                kind
                in {
                    "init",
                    "system",
                    "session_start",
                    "session.created",
                    "message_start",
                }
            ):
                for item in _objects(event):
                    if isinstance(item.get("model"), str) and item["model"] != "<synthetic>":
                        result["observed_model"] = item["model"]
        if adapter == "claude" and kind == "assistant":
            message = event.get("message")
            if (isinstance(message, dict) and isinstance(message.get("model"), str)
                    and message["model"] != "<synthetic>"):
                result["observed_model"] = message["model"]
        error_event = kind in {
            "error",
            "turn.failed",
            "api_retry",
            "provider.quota",
            "AGY_ERROR",
        } or bool(event.get("is_error"))
        if kind == "rate_limit_event":
            # Allowed utilization notices are evidence, not failures.
            error_event = any(
                item.get("status") in {"rejected", "limited", "exceeded"}
                for item in _objects(event)
            )
            if error_event:
                errors.append("usage limit " + json.dumps(event))
        if error_event:
            # v2 may encode the response body as a JSON string. Keep its message,
            # rather than a truncated envelope, as the actionable failure reason.
            for item in _objects(event):
                body = item.get("body")
                if isinstance(body, str):
                    try:
                        body = json.loads(body)
                    except ValueError:
                        body = None
                for detail in _objects(body if isinstance(body, dict) else item):
                    if isinstance(detail.get("message"), str):
                        provider_message = detail["message"]
            error_text = json.dumps(event, ensure_ascii=False)
            retry = kind == "api_retry" or (adapter == "codex" and kind == "error" and "reconnecting" in error_text.lower())
            (retry_errors if retry else errors).append(error_text)
        if event.get("denied_actions") is not None and not isinstance(
            event["denied_actions"], list
        ):
            errors.append("invalid agy envelope: malformed denied actions field")
        if isinstance(event.get("denied_actions"), list) and event.get(
            "denied_actions"
        ):
            errors.append("denied_actions " + json.dumps(event["denied_actions"]))
        if kind == "result":
            if event.get("is_error") is False:
                retry_errors.clear()
            result["terminal"] = True
            text = event.get("result", event.get("text", event.get("response")))
            if isinstance(text, str):
                terminal_text = text
        if kind in {"turn.completed", "turn.failed", "session.completed", "done"}:
            result["terminal"] = True
        if adapter == "codex" and kind == "turn.completed":
            retry_errors.clear()
        if kind == "assistant":
            content = (
                event.get("message", {}).get("content", [])
                if isinstance(event.get("message"), dict)
                else []
            )
            if isinstance(content, str):
                parts.append(content)
            elif isinstance(content, list):
                parts.extend(
                    item["text"]
                    for item in content
                    if isinstance(item, dict)
                    and item.get("type") == "text"
                    and isinstance(item.get("text"), str)
                )
        if kind == "item.completed" and isinstance(event.get("item"), dict):
            item = event["item"]
            if item.get("type") == "agent_message" and isinstance(
                item.get("text"), str
            ):
                parts.append(item["text"])
        if kind in {"text", "text_delta", "assistant_message"}:
            text = event.get("text")
            if isinstance(event.get("part"), dict):
                text = event["part"].get("text", text)
            if isinstance(text, str):
                parts.append(text)
        if (
            kind == "step_finish"
            and isinstance(event.get("part"), dict)
            and event["part"].get("reason") == "stop"
        ):
            result["terminal"] = True
        # agy prints its envelope flat or nested under a `result` event.
        envelope = (
            event["result"]
            if adapter == "agy" and isinstance(event.get("result"), dict)
            else event
        )
        if adapter == "agy" and "status" in envelope and "response" in envelope:
            provider_status = envelope.get("status")
            response = envelope.get("response")
            error = envelope.get("error")
            if not isinstance(provider_status, str) or not isinstance(response, str):
                errors.append("invalid agy envelope")
                continue
            result["terminal"] = True
            terminal_text = response
            if provider_status.upper() == "SUCCESS" and exit_code != 0 and not error:
                errors.append("provider exited " + str(exit_code) + " despite SUCCESS")
            if provider_status.upper() != "SUCCESS" or error:
                if "timeout" in str(error).lower() or provider_status.upper() in {
                    "TIMEOUT",
                    "PARTIAL",
                }:
                    partial = bool(response.strip())
                    if not partial:
                        errors.append("print-timeout")
                else:
                    errors.append(
                        str(
                            error
                            or "provider returned a non-success status without an error message"
                        )
                    )
    if adapter == "agy":
        match = re.search(r'^\s*using model\b[=:]?\s*["\']?([\w./-]+)', stderr, re.I | re.M)
        if match and not result["observed_model"]:
            result["observed_model"] = match[1]
        if "AGY_ERROR" in stderr:
            errors.append(stderr)
        if re.search("print.?timeout", stderr, re.I):
            partial = bool(terminal_text or parts)
    result["text"] = (
        terminal_text
        if terminal_text is not None
        else "\n".join(parts)
        if parts
        else "".join(plain)
    )
    errors.extend(retry_errors)
    diagnostic_hook = getattr(config, "diagnostics", None)
    diagnostics, warning_line = diagnostic_hook(stderr) if diagnostic_hook else (stderr, "")
    failure_text = "\n".join(errors) if errors else diagnostics
    result["failure_text"] = failure_text
    # Permission denials in diagnostics invalidate a claimed success (Agy does this).
    denial = adapter == "agy" and re.search(PERMISSION_DENIAL_PATTERN, stderr, re.I)
    if denial:
        errors.insert(0, stderr)
        failure_text = stderr + "\n" + failure_text
    status = None
    if errors or exit_code != 0 or denial:
        signature = classify_failure(adapter, failure_text)
        if signature is not None:
            status = {"invalid_input": "rejected", "provider_transport": "failed"}.get(signature, signature)
            result["signature"] = signature
        status = status or (
            "timed_out" if "print-timeout" in failure_text else "failed"
        )
        result["excerpt"] = (provider_message or "\n".join(errors).strip() or warning_line or failure_text.strip() or (
            getattr(config, "EMPTY_FAILURE", "").format(exit_code=exit_code)))[:200]
    elif partial:
        status = "partial"
        result["signature"] = "print_timeout"
    elif adapter == "codex" and parts and not result["terminal"]:
        status = "failed"
        result["signature"] = "provider_incomplete"
        result["excerpt"] = "provider exited without a terminal result"
    elif result["text"].strip():
        status = "ok"
    else:
        status = "failed"
        result["signature"] = "empty_output"
        result["excerpt"] = ("no assistant text: " + stdout.strip())[:200]
    if (getattr(config, "EXIT_AFTER_OUTPUT", False) and status == "failed" and result["signature"] is None and exit_code == 1
            and parts and result["text"].strip() and not errors):
        status = "ok"
        result["signature"] = "exit_after_output"
        result["excerpt"] = (warning_line or f"provider exited {exit_code} after final assistant output")[:200]
    if status == "ok":
        question = re.search(
            r"```(?:[^\n`]*\n)?\s*QUESTION:\s*((?:(?!```).)*?)\s*```\s*\Z", result["text"], re.S
        )
        if question and question[1].strip():
            result["question"] = question[1].strip()[:4096]
            status = "input_required"
    result["status"] = status
    result["reset_at"] = reset_time(failure_text, events, at)
    return result


def _same_model(adapter, resolved, observed):
    """Aliases and display names name the same model: opus is claude-opus-5-5,
    and Cursor reports grok-4.7 as "Grok 4.7 256K High Fast"."""
    if not resolved or not observed or resolved == observed:
        return True
    norm = lambda value: re.sub(r"[\s_]+", "-", str(value).strip().lower())
    # Only a display name (spaces, e.g. Cursor) may extend the id with
    # descriptors; an id-shaped suffix such as -thinking is a different model.
    if " " in str(observed).strip() and norm(observed).startswith(norm(resolved) + "-"):
        return True
    try:
        try:
            from .exec_routing import registered_model
        except ImportError:
            from exec_routing import registered_model
        return registered_model(adapter, resolved) == registered_model(adapter, observed)
    except Exception:  # A malformed catalogue must not fail a finished attempt.
        return False


def _observed_model(plan, parsed, env):
    adapter, session = plan["adapter"], parsed["session_id"]
    if adapter == "codex" and session and re.fullmatch(r"[\w-]{1,128}", session):
        root = Path(env.get("CODEX_HOME") or Path.home() / ".codex") / "sessions"
        # File names contain thread_id; never inspect auth/config stores.
        for path in sorted(root.glob("**/*" + session + "*.jsonl"), reverse=True):
            try:
                observed = None
                with path.open() as stream:
                    for line in stream:
                        try:
                            event = json.loads(line)
                        except ValueError:
                            continue
                        if (
                            isinstance(event, dict)
                            and event.get("type") == "turn_context"
                        ):
                            payload = event.get("payload", {})
                            if isinstance(payload, dict) and isinstance(
                                payload.get("model"), str
                            ):
                                observed = payload["model"]
                if observed:
                    return observed, profile(adapter).MODEL_SOURCE
            except OSError:
                continue
    elif adapter == "opencode" and session:
        try:
            exported = subprocess.run(
                [profile(adapter).CLI, "session", "export", "--standalone", session],
                capture_output=True,
                text=True,
                timeout=20,
                env=env,
                cwd=plan["cwd"],
                stdin=subprocess.DEVNULL,
            )
            observed = (json.loads(exported.stdout).get("info") or {}).get("model") or {}
            model, provider = observed.get("id"), observed.get("providerID")
            if isinstance(model, str) and model:
                return (
                    provider + "/" + model if isinstance(provider, str) and provider and "/" not in model else model
                ), profile(adapter).MODEL_SOURCE
        except (OSError, ValueError, AttributeError, subprocess.SubprocessError):
            pass
    if parsed["observed_model"]:
        return parsed["observed_model"], profile(adapter).MODEL_SOURCE
    return None, None
