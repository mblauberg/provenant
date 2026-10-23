"""FIFO-controlled Claude CLI. Invoked by the real provider adapter."""

import fcntl
import json
import os
import select
import signal
import subprocess
import sys
from pathlib import Path


def identity(path):
    path.write_text(json.dumps({"pid": os.getpid(), "pgid": os.getpgrp(), "ppid": os.getppid()}))


def result():
    adapter = os.environ["LANE_FIXTURE_ADAPTER"]
    if adapter == "codex":
        print(json.dumps({"type": "item.completed", "item": {"type": "agent_message", "text": "DONE"}}), flush=True)
        print(json.dumps({"type": "turn.completed"}), flush=True)
    else:
        print(json.dumps({"type": "result", "result": "DONE", "is_error": False}), flush=True)


def main():
    control = Path(os.environ["LANE_FIXTURE_DIR"])
    if "--grandchild" in sys.argv:
        with (control / "descendant-commands").open() as commands:
            identity(control / "grandchild.json")
            commands.readline()
        return 0
    if "--middle" in sys.argv:
        child = subprocess.Popen([sys.executable, __file__, "--grandchild"], start_new_session=True,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        identity(control / "middle.json")
        child.wait()
        return 0
    if "--version" in sys.argv:
        if os.environ.get("LANE_FIXTURE_PREFLIGHT_GATE") == "1":
            with (control / "descendant-commands").open() as commands:
                identity(control / "preflight.json")
                commands.readline()
        # Kiro's engine unpack check only applies when OS confinement is on.
        print("fixture 2.0.0")
        return 0
    if "mcp" in sys.argv or "models" in sys.argv:
        print("[]")
        return 0
    adapter = os.environ["LANE_FIXTURE_ADAPTER"]
    if adapter in {"claude", "codex"}:
        sys.stdin.read()
    ignore_term = False

    def received(signum, _frame):
        with (control / "signals.jsonl").open("a") as stream:
            stream.write(json.dumps({"signal": signum}) + "\n")
        if not (ignore_term and signum == signal.SIGTERM):
            # Preserve the provider's actual signal exit for receipt assertions.
            signal.signal(signum, signal.SIG_DFL)
            os.kill(os.getpid(), signum)

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, received)
    # Deliberately leave this descriptor open until the kernel closes it at
    # exit. The test can order a late cancel while the supervisor is stopped.
    exit_fd = os.open(control / "provider-exit", os.O_WRONLY | os.O_NONBLOCK)
    os.write(exit_fd, b"ready\n")
    fd = os.open(control / "commands", os.O_RDONLY | os.O_NONBLOCK)
    fcntl.fcntl(fd, fcntl.F_SETFL, fcntl.fcntl(fd, fcntl.F_GETFL) & ~os.O_NONBLOCK)
    with os.fdopen(fd) as commands:
        identity(control / "provider.json")
        for command in commands:
            command = command.strip()
            if command == "nested":
                spec = json.loads((control / "nested.json").read_text())
                child = subprocess.Popen(
                    spec["argv"], cwd=spec["cwd"], env={**os.environ, **spec["env"]},
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                (control / "nested.pid").write_text(str(child.pid))
            elif command in {"ok", "terminal", "terminal-stdin"}:
                identity(control / (command + ".json"))
                if command == "terminal-stdin":
                    readable, _, _ = select.select([sys.stdin], [], [], 0)
                    (control / "stdin.json").write_text(json.dumps({"open": not readable}))
                result()
                if command == "ok":
                    return 0
                if command == "terminal-stdin":
                    sys.stdin.read()
                    (control / "stdin-eof.json").write_text(json.dumps({"eof": True}))
                    return 0
                # Keep ancestry intact until the supervisor performs cleanup.
            elif command == "activity":
                print(json.dumps({"type": "fixture_activity"}), flush=True)
                (control / "activity.json").write_text(json.dumps({"emitted": True}))
            elif command == "write":
                Path("partial.txt").write_text("unfinished change\n")
                identity(control / "write.json")
            elif command == "ping":
                identity(control / "ping.json")
            elif command == "ignore-term":
                ignore_term = True
                identity(control / "ignore-term.json")
            elif command == "grandchild":
                subprocess.Popen([sys.executable, __file__, "--middle"], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            elif command == "over-cap":
                # More than both the legacy 20 MiB cap and S2's 8 MiB cap.
                chunk = (json.dumps({"type": "fixture_noise", "padding": "x" * 8100}) + "\n").encode()
                for _ in range((24 * 1024 * 1024) // len(chunk) + 1):
                    sys.stdout.buffer.write(chunk)
                    sys.stderr.buffer.write(chunk)
                sys.stdout.flush()
                sys.stderr.flush()
                result()
                return 0
            elif command == "exit":
                return 0
            elif command == "fail":
                print("fixture provider failed", file=sys.stderr, flush=True)
                return 7
            else:
                raise ValueError(f"unknown fixture command: {command}")
    raise RuntimeError("fixture FIFO closed without an exit command")


if __name__ == "__main__":
    raise SystemExit(main())
