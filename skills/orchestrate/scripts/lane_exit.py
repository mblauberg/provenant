"""Map Rust process records onto existing Python receipt settlement."""

import json
import os
from pathlib import Path
import stat
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import provider_observer


def read_json(path, limit=16384):
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("lane record is not a regular file")
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError("lane record exceeds its byte limit")
    record = json.loads(data)
    if not isinstance(record, dict):
        raise ValueError("lane record must be an object")
    return record


def receipt_fields(record):
    """Translate process evidence without changing the adapter receipt schema."""
    if record.get("schema") != "provenant.lane.exit.v1":
        raise ValueError("unsupported lane exit schema")
    forced = record.get("forced")
    if forced == "stalled":
        raise ValueError("silence cannot force a provider stop")
    for key in ("pid", "pgid", "exit", "signal"):
        if record.get(key) is not None and type(record[key]) is not int:
            raise ValueError("lane exit process fields must be integers")
    signum = record.get("signal")
    code = -signum if signum else record.get("exit")
    spared = record.get("spared") or []
    warnings = [*record.get("census_warnings", []), *record.get("io_errors", [])]
    return {
        "pid": record.get("pid"), "pgid": record.get("pgid"), "exit": code,
        "forced": {"wall_timeout": "timed_out", "supervisor_signal": "interrupted"}.get(forced, forced),
        "terminal_event_seen": bool(record.get("terminal_event_seen")),
        "reaped": record.get("reaped") or [], "spared": spared if isinstance(spared, int) else len(spared),
        "capture": record.get("capture") or {},
        "warnings": [value if isinstance(value, str) else json.dumps(value) for value in warnings],
        "started_at": record.get("started_at"), "ended_at": record.get("ended_at"),
        "last_progress_at": record.get("last_progress_at"),
    }


def settle_observer(plan, record, events_path, stderr_path):
    """Restore retained semantics, or replay captures when delivery was lost."""
    observer = provider_observer.LiveObserver(plan)
    if not record.get("observer_lost"):
        try:
            summary = read_json(Path(plan["run_dir"]) / "observer.json",
                                provider_observer.MAX_EVENTS_BYTES * 8 + 65536)
            observer.semantic = {key: summary[key] for key in ("session_id", "observed_model", "reset_at", "retry_after")
                                 if summary.get(key) is not None}
            observer.terminal_at = time.monotonic() if summary["terminal_event_seen"] else None
            for name in ("live_failure", "semantic_failures", "retry_failure", "terminal_text", "text_truncated", "model_changes", "last_model"):
                setattr(observer, name, summary[name])
            observer.text_chunks.extend(chunk.encode() for chunk in summary["text_chunks"])
            observer.text_size = sum(map(len, observer.text_chunks))
            observer.meter.value.update(summary["context"])
            observer.meter.models.update(summary.get("models", {}))
            return observer
        except (OSError, ValueError, KeyError, TypeError):
            observer = provider_observer.LiveObserver(plan)  # Discard a partially restored summary.
    for path, stream in ((events_path, "stdout"), (stderr_path, "stderr")):
        try:
            descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        except FileNotFoundError:
            continue
        with os.fdopen(descriptor, "rb") as capture:
            if not stat.S_ISREG(os.fstat(capture.fileno()).st_mode):
                raise ValueError("lane capture is not a regular file")
            remaining = provider_observer.MAX_EVENTS_BYTES
            while remaining and (data := capture.read(min(65536, remaining))):
                if stream == "stdout":
                    observer.feed_stdout(data)
                observer.feed_stderr(data, stream)
                remaining -= len(data)
    observer.result()
    return observer


class Capture:
    """Read Rust's bounded captures through the existing settlement interface."""

    def __init__(self, path):
        self.path = Path(path)
        self.total = 0
        self.write_error = None

    def write(self, data):
        self.total += len(data)
        try:
            descriptor = os.open(self.path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            with os.fdopen(descriptor, "ab") as stream:
                if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                    raise ValueError("lane capture is not a regular file")
                stream.write(data)
        except OSError as exc:
            self.write_error = exc

    def text(self):
        try:
            descriptor = os.open(self.path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        except FileNotFoundError:
            return ""
        with os.fdopen(descriptor, "rb") as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise ValueError("lane capture is not a regular file")
            self.total = max(self.total, os.fstat(stream.fileno()).st_size)
            return stream.read(provider_observer.MAX_EVENTS_BYTES).decode(errors="replace")

    def close(self):
        pass
