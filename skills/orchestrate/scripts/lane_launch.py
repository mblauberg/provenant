"""Legacy launch-spec facts retained only by workflow contract tests."""

import json
import math
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))


def milliseconds(seconds):
    """Round positive seconds up so conversion cannot shorten a deadline."""
    if isinstance(seconds, bool) or not isinstance(seconds, (int, float)) or not math.isfinite(seconds) or seconds <= 0:
        raise ValueError("lane durations must be finite positive seconds")
    return math.ceil(seconds * 1000)


def build_spec(plan, environment, *, argv, stdin_path, events_path, stderr_path, max_bytes,
               silence_seconds=None):
    """Describe one provider launch; callers send this directly over stdin."""
    attempt = Path(plan["run_dir"]).absolute()
    identity = plan.get("attempt_identity") or {}
    run = Path(identity.get("run_dir") or attempt).absolute()
    batch_id = plan.get("batch_id")
    if not batch_id:
        # The dispatch owner publishes batch identity before execute, without adding it to the plan.
        try:
            with (attempt / "attempt.json").open() as stream:
                active = json.loads(stream.read(65537))
            batch_id = active.get("batch_id")
        except (OSError, ValueError, AttributeError):
            pass
    markers = [str(attempt / "cancel.request")]
    if batch_id:
        if not isinstance(batch_id, str) or not batch_id.startswith("batch-") or not batch_id[6:].isdigit():
            raise ValueError("invalid batch cancel identity")
        markers.append(str(run / "dispatch/batches" / batch_id / "cancel.request"))
    capture = {}
    for key, path in (("events", events_path), ("stderr", stderr_path)):
        path = Path(path).absolute()
        capture[key] = os.path.relpath(path, attempt)
    capture["max_bytes"] = max_bytes
    if type(max_bytes) is not int or max_bytes <= 0:
        raise ValueError("capture limit must be a positive integer")
    observer = [sys.executable, str(Path(__file__).with_name("provider_exec.py").resolve()),
                "observe", "--adapter", plan["adapter"], "--model", plan["model"],
                "--out", str(attempt / "observer.json"), "--max-bytes", str(max_bytes)]
    return {
        "schema": "provenant.lane.launch.v1", "argv": list(argv), "cwd": plan["cwd"],
        "env": dict(environment), "stdin": {"path": str(Path(stdin_path).absolute())} if stdin_path else {},
        "attempt_dir": str(attempt), "run_token": plan.get("_lineage_token") or "",
        "capture": capture,
        "silence_observation_ms": milliseconds(plan.get("silence_seconds", 300) if silence_seconds is None else silence_seconds),
        "timeouts_ms": {"startup": milliseconds(plan.get("startup_seconds", 300)),
                        "wall": milliseconds(plan["timeout_seconds"]),
                        "settle": milliseconds(plan["grace_seconds"]), "child_grace": 500, "root_flush": 2000},
        "progress": {"writer_tree": {"root": plan["cwd"], "exclude": [".git", "node_modules"]}}
                    if plan["mode"] == "worktree_write" else {},
        "cancel_markers": markers, "observer": {"argv": observer}, "spare": {"code_mode_host": True},
    }
