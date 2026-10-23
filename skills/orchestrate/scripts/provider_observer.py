"""Live provider semantics shared by the Python supervisor and lane observer."""

from collections import deque
import json
import re
import time
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from adapters import profile
import context_usage
import provider_output

MAX_EVENTS_BYTES = 20 * 1024 * 1024

# v2 logs are `timestamp=... level=ERROR ...`; v1 printed `ERROR ...` at line start.
OPENCODE_FATAL_LOG = r"^\s*ERROR\b|\blevel=ERROR\b|(?:monthly|weekly|5.hour) (?:usage )?limit"
OPENCODE_QUIET_LOG = r"\s*(?:WARN|INFO|DEBUG)\b|.*\blevel=(?:WARN|INFO|DEBUG)\b"


class LiveObserver:
    """Retain bounded semantic state without owning provider processes."""

    def __init__(self, plan):
        self.plan = plan
        self.continues_turn = getattr(profile(plan["adapter"]), "continues_turn", lambda event: False)
        self.terminal_at = None
        self.pending = b""
        self.dropping_line = False
        self.semantic = {}
        self.text_chunks, self.text_size = deque(), 0
        self.semantic_failures = {}
        self.live_failure = None
        self.diagnostic_pending = {"stdout": b"", "stderr": b""}
        self.retry_failure = None
        self.terminal_text = None
        self.text_truncated = False
        self.meter = context_usage.Meter(self.plan["adapter"])
        self.model_changes = []
        self.last_model = self.plan["model"]


    def observe_model(self, model):
        if model and model != "<synthetic>":
            if not provider_output._same_model(self.plan["adapter"], self.last_model, model):
                self.model_changes.append({"from": self.last_model, "to": model})
            self.last_model = model

    def feed_stderr(self, data, stream):
        adapter = self.plan["adapter"]
        if adapter == "agy" and stream == "stderr":
            self.diagnostic_pending[stream] = (self.diagnostic_pending[stream] + data)[-65536:]
            lines = self.diagnostic_pending[stream].split(b"\n")
            self.diagnostic_pending[stream] = lines.pop()
            for line in lines:
                model = provider_output.parse_output(adapter, "", line.decode(errors="replace")).get("observed_model")
                if model:
                    self.observe_model(model)
                    self.semantic["observed_model"] = model
            return
        if adapter not in {"opencode", "kiro"} or (adapter == "opencode" and stream != "stderr"):
            return
        # Preserve split diagnostics without buffering an unbounded log or interpreting answers.
        self.diagnostic_pending[stream] = (self.diagnostic_pending[stream] + data)[-65536:]
        text = self.diagnostic_pending[stream].decode(errors="replace")
        if adapter == "kiro":
            if re.search(r"(?m)^\s*Opening browser(?:\.\.\.|…)\s*$", text):
                self.live_failure = {"status": "auth_required", "signature": "auth_expired",
                                "excerpt": "Opening browser...", "reset_at": None, "retry_after": None}
        elif re.search(OPENCODE_FATAL_LOG, text, re.I | re.M):
            # Only an ERROR log or an explicit period limit is fatal; other retried noise is not.
            # Classify only the qualifying lines plus reset-hint lines; the rest of the window is not evidence.
            relevant = [line for line in text.splitlines()
                        if re.search(OPENCODE_FATAL_LOG, line, re.I)
                        or (re.search(r"\bresets?\b", line, re.I)
                            and not re.match(OPENCODE_QUIET_LOG, line, re.I))]
            parsed = provider_output.parse_output(adapter, "", "\n".join(relevant), 1)
            if parsed["status"] == "usage_limited":
                self.live_failure = parsed
        # Keep the unfinished line and a few before it, so a reset hint on its own line is not lost.
        self.diagnostic_pending[stream] = b"\n".join(self.diagnostic_pending[stream].split(b"\n")[-6:])

    def feed_stdout(self, data):
        if self.dropping_line:
            if b"\n" not in data:
                return
            data = data.split(b"\n", 1)[1]
            self.dropping_line = False
        self.pending += data
        while b"\n" in self.pending:
            line, self.pending = self.pending.split(b"\n", 1)
            try:
                event = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                continue
            if not isinstance(event, dict):
                continue
            self.meter.observe(event)
            if self.continues_turn(event):
                self.text_chunks.clear()
                self.text_size = 0
                self.terminal_at = None
            parsed_line = provider_output.parse_output(self.plan["adapter"], line.decode(errors="replace"))
            self.observe_model(parsed_line.get("observed_model"))
            for key in ("session_id", "observed_model", "reset_at", "retry_after"):
                if parsed_line.get(key) is not None:
                    self.semantic[key] = parsed_line[key]
            # A resumed Claude session's empty preamble result (no turns) is not
            # completion evidence; the real turn may start after the grace.
            preamble = (
                event.get("type") == "result"
                and event.get("num_turns") == 0
                and not event.get("result")
                and event.get("is_error") is False
            )
            if parsed_line["terminal"] and self.terminal_at is None and not preamble:
                self.terminal_at = time.monotonic()
            elif not parsed_line["terminal"] and (
                event.get("type") in {"assistant", "user"}
                or (event.get("type") == "system" and event.get("subtype") == "init")
            ):
                # A resumed Claude session settles leftover background tasks with
                # an empty result before its real turn; later activity reopens it.
                self.terminal_at = None
            if parsed_line["status"] not in {"ok", "input_required"} and parsed_line["signature"] not in {"empty_output", "provider_incomplete"}:
                failure = {key: parsed_line[key] for key in ("status", "signature", "excerpt", "reset_at", "retry_after")}
                if event.get("type") == "api_retry" or (self.plan["adapter"] == "codex" and event.get("type") == "error" and "reconnecting" in line.decode(errors="replace").lower()):
                    self.retry_failure = failure
                else:
                    self.semantic_failures[parsed_line["status"]] = failure
            elif (event.get("type") == "result" and event.get("is_error") is False
                  or self.plan["adapter"] == "codex" and event.get("type") == "turn.completed"):
                self.retry_failure = None
            if parsed_line["text"]:
                encoded = parsed_line["text"].encode()
                self.text_truncated |= len(encoded) > MAX_EVENTS_BYTES
                chunk = encoded[:MAX_EVENTS_BYTES]
                if event.get("type") == "result" or (self.plan["adapter"] == "agy" and "status" in event):
                    self.terminal_text = chunk.decode(errors="replace")
                else:
                    self.text_chunks.append(chunk)
                    self.text_size += len(chunk)
                    while self.text_size > MAX_EVENTS_BYTES:
                        self.text_truncated = True
                        self.text_size -= len(self.text_chunks.popleft())
        if len(self.pending) > MAX_EVENTS_BYTES:
            # Do not parse a clipped JSON suffix or certify preliminary text
            # when the final result event exceeded the semantic buffer.
            self.pending = b""
            self.dropping_line = True
            self.text_truncated = True


    def result(self):
        """Return the retained semantics after flushing an unfinished final line."""
        if self.pending:
            self.feed_stdout(b"\n")
        self.feed_stderr(b"\n", "stderr")
        return {
            **self.semantic,
            "terminal_event_seen": self.terminal_at is not None,
            "live_failure": self.live_failure,
            "semantic_failures": self.semantic_failures,
            "retry_failure": self.retry_failure,
            "terminal_text": self.terminal_text,
            "text_chunks": [chunk.decode(errors="replace") for chunk in self.text_chunks],
            "text_truncated": self.text_truncated,
            "model_changes": self.model_changes,
            "last_model": self.last_model,
            "context": self.meter.result(),
        }
