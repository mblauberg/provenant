"""Build and execute the S2 launch wire contract."""

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


def front_door(environment):
    """Use the pointer-owned installed front door's bin directory."""
    directory = Path(environment.get("PROVENANT_BIN_DIR") or Path.home() / ".local/bin").expanduser()
    if not directory.is_absolute():
        raise ValueError("PROVENANT_BIN_DIR must be absolute")
    return str(directory / "provenant")


class ProviderProcess:
    """Expose provider identity to today's owner callbacks, without signal authority."""

    def __init__(self, pid):
        self.pid = pid
        self.returncode = None
        self.stdin = self.stdout = self.stderr = None

    def poll(self):
        return self.returncode

    def wait(self, timeout=None):
        if self.returncode is None:
            raise RuntimeError("provider is still held by lane run")
        return self.returncode


def run(spec, environment, *, on_start=None, on_progress=None, cancelled=None, signal_requested=None):
    """Wait in the owner group; forward intent only to the supervisor child."""
    import logging
    import selectors
    import signal
    import subprocess
    import tempfile
    from types import SimpleNamespace
    import lane_exit

    provider, terminal_record = None, None
    started_at = None
    pending = b""
    sent_signal = False
    cancelled_request = False
    with tempfile.TemporaryFile() as errors, selectors.DefaultSelector() as selector:
        process = subprocess.Popen([front_door(environment), "lane", "run"], env=environment,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        try:
            try:
                process.stdin.write((json.dumps(spec) + "\n").encode())
                process.stdin.close()
            except BrokenPipeError:
                process.stdin.close()
            os.set_blocking(process.stdout.fileno(), False)
            selector.register(process.stdout, selectors.EVENT_READ)
            while process.poll() is None or selector.get_map():
                requested = signal_requested() if signal_requested else None
                if not sent_signal and (requested or cancelled and cancelled()):
                    cancelled_request = not bool(requested)
                    if cancelled_request:
                        import run_records
                        attempt = Path(spec["attempt_dir"])
                        try:
                            run_records.create_cancellation_marker(attempt, attempt)
                        except (OSError, ValueError):
                            requested = signal.SIGTERM  # Cancellation still authorises an interrupt to lane run.
                    if requested:
                        try:
                            process.send_signal(requested)
                        except ProcessLookupError:
                            pass
                    sent_signal = True
                for key, _mask in selector.select(0.05):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    pending += data
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        if len(line) > 65536:
                            continue
                        try:
                            event = json.loads(line)
                        except ValueError:
                            continue
                        if not isinstance(event, dict):
                            continue
                        kind = event.get("event", event.get("type"))
                        if kind == "started" and provider is None:
                            provider = ProviderProcess(event["pid"])
                            started_at = event.get("started_at")
                            if on_start:
                                try:
                                    on_start(provider)
                                except OSError as exc:
                                    logging.warning("provider start bookkeeping unavailable: %s", exc)
                        elif kind == "progress" and on_progress:
                            try:
                                on_progress(event["last_progress_at"])
                            except OSError as exc:
                                logging.warning("provider progress bookkeeping unavailable: %s", exc)
                        elif event.get("schema") == "provenant.lane.exit.v1":
                            terminal_record = event  # Publication failure (75) retains evidence on stdout.
                    if len(pending) > 65536:
                        pending = b""
            process.wait()
        finally:
            process.stdout.close()
            process.stdin.close()
            # No exception path acquires authority to signal the provider.
            process.wait()
        errors.seek(0)
        error_text = errors.read(65536).decode(errors="replace")
    if process.returncode == 64 and provider is None:
        return SimpleNamespace(unsupported=True, warning=error_text)
    try:
        record = lane_exit.read_json(Path(spec["attempt_dir"]) / "supervisor.exit.json")
    except (OSError, ValueError):
        if terminal_record is None:
            raise ValueError(f"lane run exited {process.returncode} without a valid exit record: {error_text[-300:]}") from None
        record = terminal_record
    fields = lane_exit.receipt_fields(record)
    if cancelled_request and fields["forced"] == "interrupted":
        fields["forced"] = "cancelled"
    if provider is None and fields["pid"]:
        provider = ProviderProcess(fields["pid"])
    if provider:
        provider.returncode = fields["exit"]
    return SimpleNamespace(unsupported=False, record=record, fields=fields, provider=provider,
                           started_at=started_at, warning=error_text)
