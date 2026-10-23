#!/usr/bin/env python3
"""Small foreground S2 protocol peer; production process policy belongs to Rust."""

import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import time
from datetime import UTC, datetime


def now():
    return datetime.now(UTC).isoformat()


def main():
    assert sys.argv[1:] == ["lane", "run"]
    mode = os.environ.get("LANE_STUB_MODE", "run")
    spec = json.loads(sys.stdin.readline())
    if mode == "unsupported":
        return 64
    assert spec["schema"] == "provenant.lane.launch.v1"
    attempt = Path(spec["attempt_dir"])
    redacted = {**spec, "env": {name: name for name in spec["env"]}}
    (attempt / "launch.json").write_text(json.dumps(redacted))
    paths = {name: attempt / spec["capture"][name] for name in ("events", "stderr")}
    buffers = {name: bytearray() for name in paths}
    totals = dict.fromkeys(paths, 0)
    pending = {name: b"" for name in paths}
    limit = spec["capture"]["max_bytes"]
    input_file = open(spec["stdin"]["path"], "rb") if spec["stdin"].get("path") else None
    start, started_at = time.monotonic(), now()
    process = subprocess.Popen(spec["argv"], cwd=spec["cwd"], env=spec["env"],
                               stdin=input_file or subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    if input_file:
        input_file.close()
    (attempt / "pgid").write_text(str(process.pid) + "\n")
    observer = subprocess.Popen(spec["observer"]["argv"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    observer_lost = mode == "observer-lost"
    if observer_lost:
        observer.terminate()
        observer.communicate(timeout=5)
    print(json.dumps({"event": "started", "pid": process.pid, "pgid": process.pid, "started_at": started_at}), flush=True)
    print(json.dumps({"event": "progress", "last_progress_at": now()}), flush=True)
    requested_signal = None

    def requested(signum, _frame):
        nonlocal requested_signal
        requested_signal = signum

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, requested)
    terminal_at, forced = None, None
    provider_stdout = False
    observer_pending = b""

    def observe(name, data, *, eof=False):
        nonlocal observer_lost
        if observer_lost:
            return
        pending[name] += data
        while b"\n" in pending[name] or eof and pending[name]:
            if b"\n" in pending[name]:
                line, pending[name] = pending[name].split(b"\n", 1)
            else:
                line, pending[name] = pending[name], b""
            try:
                observer.stdin.write((json.dumps({"s": "out" if name == "events" else "err",
                                                  "line": line.decode(errors="replace")}) + "\n").encode())
                observer.stdin.flush()
            except BrokenPipeError:
                observer_lost = True
                break

    with selectors.DefaultSelector() as selector:
        for stream, name in ((process.stdout, "events"), (process.stderr, "stderr"), (observer.stdout, "observer")):
            if name != "observer" or not observer_lost:
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, name)
        while process.poll() is None or any(key.data != "observer" for key in selector.get_map().values()):
            for key, _mask in selector.select(0.01):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    if key.data != "observer":
                        observe(key.data, b"", eof=True)
                    continue
                if key.data == "observer":
                    observer_pending += data
                    while b"\n" in observer_pending:
                        line, observer_pending = observer_pending.split(b"\n", 1)
                        request = json.loads(line)
                        if request.get("terminal"):
                            terminal_at = time.monotonic()
                        if request.get("stop"):
                            assert request["stop"] != "stalled"
                            forced = request["stop"]
                    continue
                name = key.data
                provider_stdout |= name == "events"
                totals[name] += len(data)
                buffers[name].extend(data)
                if len(buffers[name]) > limit:
                    half = limit // 2
                    buffers[name][:] = buffers[name][:half] + buffers[name][-(limit - half):]
                observe(name, data)
            elapsed_ms = (time.monotonic() - start) * 1000
            if requested_signal:
                forced = "supervisor_signal"
            elif any(Path(marker).is_file() for marker in spec["cancel_markers"]):
                forced = "cancelled"
            elif elapsed_ms >= spec["timeouts_ms"]["wall"]:
                forced = "wall_timeout"
            elif not provider_stdout and elapsed_ms >= spec["timeouts_ms"]["startup"]:
                forced = "startup_timeout"
            grace = terminal_at is not None and (time.monotonic() - terminal_at) * 1000 >= spec["timeouts_ms"]["settle"]
            if process.poll() is None and (forced or grace):
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=0.2)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
        process.wait()
    process.stdout.close()
    process.stderr.close()
    try:
        observer.stdin.close()
    except BrokenPipeError:
        observer_lost = True
    if not observer_lost:
        os.set_blocking(observer.stdout.fileno(), True)
        while observer.stdout.read(65536):
            pass
    observer.wait(timeout=5)
    observer_lost |= observer.returncode != 0
    observer.stdout.close()
    for name, path in paths.items():
        path.write_bytes(buffers[name])
    record = {"schema": "provenant.lane.exit.v1", "pid": process.pid, "pgid": process.pid,
              "started_at": started_at, "ended_at": now(), "last_progress_at": now(),
              "exit": process.returncode if process.returncode >= 0 else None,
              "signal": -process.returncode if process.returncode < 0 else None,
              "forced": forced, "terminal_event_seen": terminal_at is not None,
              "reaped": [], "spared": [], "census_warnings": [], "observer_lost": observer_lost,
              "capture": {name: {"bytes": totals[name], "truncated": totals[name] > limit} for name in paths},
              "io_errors": ["exit publication unavailable"] if mode == "stdout-exit" else []}
    if mode != "stdout-exit":
        (attempt / "supervisor.exit.json").write_text(json.dumps(record))
    else:
        print(json.dumps(record), flush=True)
    print(json.dumps({"event": "exited"}), flush=True)
    return 75 if mode == "stdout-exit" else 0


if __name__ == "__main__":
    raise SystemExit(main())
