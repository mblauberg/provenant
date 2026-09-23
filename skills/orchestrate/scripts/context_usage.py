"""Session context size from provider streams, the context ceiling, and resume advice.

Field paths were read from one live turn per adapter (tests/fixtures/fabric-context).
`observed` means the provider reported the size of its latest request; `estimated`
means only turn totals exist, which overstate context when a turn made several
requests. Missing evidence stays null.
"""

import json
import os
import re
from pathlib import Path

DEFAULT_CEILING = 300000
MIN_CEILING = 100000
MAX_CEILING = 1000000
# Adapters whose CLI takes a per-invocation auto-compaction threshold.
CEILING_CONTROL = {"claude", "codex"}
FIELDS = ("context_tokens", "input_tokens", "output_tokens", "cached_input_tokens",
          "context_window_tokens", "context_percent", "source")


def _int(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def _sum(*values):
    numbers = [_int(value) for value in values]
    return sum(value for value in numbers if value is not None) if any(v is not None for v in numbers) else None


class Meter:
    """Fold a provider's JSON events into one context record."""

    def __init__(self, adapter):
        self.adapter = adapter
        self.value = dict.fromkeys(FIELDS)
        self.totals = {"input_tokens": None, "output_tokens": None, "cached_input_tokens": None}
        self.models = {"init": None, "answered": [], "final": None, "usage": []}

    def _add(self, input_tokens, output_tokens, cached):
        for key, value in (("input_tokens", input_tokens), ("output_tokens", output_tokens),
                           ("cached_input_tokens", cached)):
            if _int(value) is not None:
                self.totals[key] = (self.totals[key] or 0) + value

    def observe(self, event):
        if not isinstance(event, dict):
            return
        try:
            getattr(self, "_" + self.adapter, lambda _event: None)(event)
        except (AttributeError, TypeError, KeyError, IndexError):
            pass  # A malformed usage field never fails an attempt.

    def _claude(self, event):
        kind, models = event.get("type"), self.models
        if kind == "system" and event.get("subtype") == "init" and isinstance(event.get("model"), str):
            models["init"] = event["model"]
        if kind == "assistant" and isinstance(event.get("message"), dict):
            message = event["message"]
            model = message.get("model")
            if isinstance(model, str) and model:
                if model not in models["answered"]:
                    models["answered"].append(model)
                if event.get("parent_tool_use_id") is None:
                    models["final"] = model
            self._claude_request(message.get("usage"))
        if kind == "result":
            usage = event.get("usage") or {}
            self.value.update(
                input_tokens=_sum(usage.get("input_tokens"), usage.get("cache_creation_input_tokens"),
                                  usage.get("cache_read_input_tokens")),
                output_tokens=_int(usage.get("output_tokens")),
                cached_input_tokens=_int(usage.get("cache_read_input_tokens")),
            )
            iterations = [item for item in usage.get("iterations") or [] if isinstance(item, dict)]
            if iterations:
                self._claude_request(iterations[-1])
            usage_models = event.get("modelUsage") if isinstance(event.get("modelUsage"), dict) else {}
            models["usage"] = list(usage_models)
            answering = self.answering_model()[0]
            chosen = usage_models.get(answering) if answering else None
            self.value["context_window_tokens"] = _int(chosen.get("contextWindow")) if isinstance(chosen, dict) else None

    def answering_model(self):
        """(model, source): the model that answered, then a lone modelUsage key; init only as a last resort."""
        models = self.models
        if models["answered"]:
            return models["final"] or models["answered"][-1], "claude:assistant.message.model"
        if len(models["usage"]) == 1:
            return models["usage"][0], "claude:result.modelUsage"
        return models["init"], "claude:init.model" if models["init"] else None

    def _claude_request(self, usage):
        if isinstance(usage, dict):
            size = _sum(usage.get("input_tokens"), usage.get("cache_creation_input_tokens"),
                        usage.get("cache_read_input_tokens"), usage.get("output_tokens"))
            if size is not None:
                self.value.update(context_tokens=size, source="observed")

    def _codex(self, event):
        if event.get("type") == "turn.completed" and isinstance(event.get("usage"), dict):
            usage = event["usage"]
            self.value.update(input_tokens=_int(usage.get("input_tokens")),
                              output_tokens=_int(usage.get("output_tokens")),
                              cached_input_tokens=_int(usage.get("cached_input_tokens")),
                              context_tokens=_sum(usage.get("input_tokens"), usage.get("output_tokens")),
                              source="estimated")

    def _cursor(self, event):
        if event.get("type") == "result" and isinstance(event.get("usage"), dict):
            usage = event["usage"]
            total = _sum(usage.get("inputTokens"), usage.get("cacheReadTokens"), usage.get("cacheWriteTokens"))
            self.value.update(input_tokens=total, output_tokens=_int(usage.get("outputTokens")),
                              cached_input_tokens=_int(usage.get("cacheReadTokens")),
                              context_tokens=_sum(total, usage.get("outputTokens")), source="estimated")

    def _opencode(self, event):
        part = event.get("part")
        if event.get("type") == "step_finish" and isinstance(part, dict) and isinstance(part.get("tokens"), dict):
            tokens = part["tokens"]
            cache = tokens.get("cache") if isinstance(tokens.get("cache"), dict) else {}
            self._add(_sum(tokens.get("input"), cache.get("read"), cache.get("write")), tokens.get("output"),
                      cache.get("read"))
            size = _int(tokens.get("total")) or _sum(tokens.get("input"), tokens.get("output"),
                                                      cache.get("read"), cache.get("write"))
            self.value.update(context_tokens=size, source="observed", **self.totals)

    def _agy(self, event):
        step = event.get("step_update")
        if isinstance(step, dict) and isinstance(step.get("usage"), dict):
            usage = step["usage"]
            self.value.update(context_tokens=_sum(usage.get("input_tokens"), usage.get("output_tokens")),
                              source="observed")
        result = event.get("result") if event.get("event") == "result" else None
        if isinstance(result, dict) and isinstance(result.get("usage"), dict) and _int(result["usage"].get("input_tokens")):
            usage = result["usage"]
            self.value.update(input_tokens=_int(usage.get("input_tokens")),
                              output_tokens=_int(usage.get("output_tokens")),
                              cached_input_tokens=_int(usage.get("cache_read_tokens")))

    def _kiro(self, event):
        data = event.get("data")
        if event.get("type") == "metadata" and isinstance(data, dict):
            percent = data.get("contextUsagePercentage")
            if isinstance(percent, (int, float)) and not isinstance(percent, bool) and 0 <= percent <= 100:
                self.value.update(context_percent=round(float(percent), 1), source="observed")

    def result(self):
        return dict(self.value)


def with_codex_rollout(value, session, env):
    """Codex's local rollout holds the latest request's usage and the model window."""
    if not session or not re.fullmatch(r"[\w-]{1,128}", session):
        return value
    root = Path(env.get("CODEX_HOME") or Path.home() / ".codex") / "sessions"
    for path in sorted(root.glob("**/*" + session + "*.jsonl"), reverse=True):
        info = None
        try:
            with path.open() as stream:
                for line in stream:
                    if '"token_count"' not in line:
                        continue
                    try:
                        payload = json.loads(line).get("payload") or {}
                    except (ValueError, AttributeError):
                        continue
                    if payload.get("type") == "token_count" and isinstance(payload.get("info"), dict):
                        info = payload["info"]
        except OSError:
            continue
        if info:
            last = info.get("last_token_usage") or {}
            size = _int(last.get("total_tokens")) or _sum(last.get("input_tokens"), last.get("output_tokens"))
            if size is not None:
                value = {**value, "context_tokens": size, "source": "observed"}
            window = _int(info.get("model_context_window"))
            return {**value, "context_window_tokens": window} if window else value
    return value


def _product_root():
    return Path(os.environ.get("AGENT_FABRIC_PRODUCT_ROOT") or Path(__file__).resolve().parents[3])


def default_ceiling():
    try:
        value = json.loads((_product_root() / "config/model-routing.json").read_text())["context"]["ceiling_tokens"]
        return value if _int(value) else DEFAULT_CEILING
    except (OSError, ValueError, KeyError, TypeError):
        return DEFAULT_CEILING


def clamp_ceiling(value):
    """Return (tokens, warning). Out-of-range requests are clamped, never rejected."""
    if value is None:
        return default_ceiling(), None
    try:
        requested = int(round(float(value)))
    except (TypeError, ValueError, OverflowError):
        return default_ceiling(), f"context_ceiling {value!r} ignored; using {default_ceiling()}"
    tokens = min(MAX_CEILING, max(MIN_CEILING, requested))
    if tokens != requested:
        return tokens, f"context_ceiling {requested} clamped to {tokens}"
    return tokens, None


# contextWindow of the answering model in live turns, 2026-09-23 (Claude Code 2.1.280). Haiku's
# 200k window remains the ceiling bound if a session answers with a larger-window model.
CLAUDE_WINDOWS = {"opus": 1000000, "opus-5.5": 1000000, "claude-opus-5-5": 1000000, "sonnet": 1000000,
                  "claude-sonnet-5": 1000000, "fable": 1000000, "claude-fable-5-1": 1000000,
                  "haiku": 200000, "claude-haiku-4-5": 200000, "claude-haiku-4-5-20251001": 200000}


def _codex_point(model, env):
    """Codex compacts at effective_context_window_percent of the model window in its models cache."""
    try:
        cache = json.loads((Path(env.get("CODEX_HOME") or Path.home() / ".codex") / "models_cache.json").read_text())
        entry = next((item for item in cache.get("models") or []
                      if isinstance(item, dict) and model and item.get("slug") == model), {})
    except (OSError, ValueError, AttributeError):
        entry = {}
    window, percent = _int(entry.get("context_window")), _int(entry.get("effective_context_window_percent"))
    if not window or not percent:
        return None, None
    return window * percent // 100, "codex models_cache effective window"


def _claude_point(model, env):
    """Claude compacts at its model window, or earlier at the user's autoCompactWindow.

    Dispatched --safe-mode runs load ~/.claude/settings.json (verified live 2026-09-23)."""
    name = str(model or "")
    window = CLAUDE_WINDOWS.get(name) or (1000000 if name.endswith("[1m]") else None)
    try:
        home = Path(env.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
        settings = json.loads((home / "settings.json").read_text())
    except (OSError, ValueError):
        settings = {}
    user = _int(settings.get("autoCompactWindow")) if isinstance(settings, dict) else None
    if user and settings.get("autoCompactEnabled") is not False and (window is None or user < window):
        return user, "claude user settings autoCompactWindow"
    return window, "claude model window" if window else None


def effective_ceiling(applied):
    """The compaction point in force, else the requested ceiling as a warning threshold."""
    applied = applied or {}
    return _int(applied.get("context_ceiling_tokens")) or _int(applied.get("context_ceiling_requested")) \
        or default_ceiling()


def apply_ceiling(plan, value, argv=None, env=None):
    """A ceiling only lowers a provider's compaction point; it never raises it."""
    tokens, warning = clamp_ceiling(value)
    adapter, applied = plan["adapter"], plan["applied"]
    applied.pop("context_ceiling_source", None)
    applied["context_ceiling_requested"] = tokens
    plan["context_ceiling"] = None
    if adapter in CEILING_CONTROL:
        point, source = (_codex_point if adapter == "codex" else _claude_point)(plan.get("model"), env or os.environ)
        if point is None:
            # Without a known point, lower-only cannot be proven: pass nothing, claim no number.
            applied.update(context_ceiling="provider_default", context_ceiling_tokens=None)
            unknown = f"context_ceiling not applied: {adapter} compaction point for {plan.get('model')} unknown"
            if unknown not in plan["warnings"]:
                plan["warnings"].append(unknown)
        elif tokens < point:
            plan["context_ceiling"] = tokens
            applied.update(context_ceiling="enforced", context_ceiling_tokens=tokens)
        else:
            applied.update(context_ceiling="provider_default", context_ceiling_tokens=point,
                           context_ceiling_source=source)
    else:
        applied.update(context_ceiling="unsupported", context_ceiling_tokens=tokens)
    if warning and warning not in plan["warnings"]:
        plan["warnings"].append(warning)
    if argv:
        plan["argv"] = argv(plan)
    return plan


def short(tokens):
    if tokens >= 1000000:
        return f"{tokens / 1000000:.1f}".rstrip("0").rstrip(".") + "M"
    return f"{round(tokens / 1000)}k"


def marker(value):
    """A compact digest suffix, empty when nothing was measured."""
    value = value or {}
    tokens, window = _int(value.get("context_tokens")), _int(value.get("context_window_tokens"))
    if tokens is not None:
        text = ("~" if value.get("source") == "estimated" else "") + short(tokens)
        return " · ctx " + text + ("/" + short(window) if window else "")
    percent = value.get("context_percent")
    if isinstance(percent, (int, float)) and not isinstance(percent, bool):
        return f" · ctx {round(percent)}%"
    return ""


def resume_warning(previous, adapter, ceiling, run_id, task_id=None):
    """Warn, don't block: name the cheaper fresh handoff when a resume may be costly."""
    context = previous.get("context") or {}
    tokens = _int(context.get("context_tokens"))
    if tokens is not None and tokens <= ceiling:
        return None
    if tokens is None and adapter in CEILING_CONTROL:
        return None
    size = f"a ~{short(tokens)}-token session" if tokens is not None else "a session of unknown size"
    target = f'handoff:"{run_id}"' + (f', task_id:"{task_id}"' if task_id else "")
    return f"resuming {size}; fresh: fabric_dispatch{{prompt, {target}}}"
