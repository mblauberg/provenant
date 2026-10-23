"""FIFO-controlled Claude CLI. Invoked by the real provider adapter."""

import json
import os
import subprocess
import sys
from pathlib import Path


def main():
    sys.stdin.read()
    control = Path(os.environ["LANE_FIXTURE_DIR"])
    with (control / "commands").open() as commands:
        (control / "provider.json").write_text(json.dumps({"pid": os.getpid()}))
        for command in commands:
            command = command.strip()
            if command == "nested":
                spec = json.loads((control / "nested.json").read_text())
                child = subprocess.Popen(
                    spec["argv"], cwd=spec["cwd"], env={**os.environ, **spec["env"]},
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                (control / "nested.pid").write_text(str(child.pid))
            elif command in {"ok", "terminal"}:
                print(json.dumps({"type": "result", "result": "DONE", "is_error": False}), flush=True)
                if command == "ok":
                    return 0
                # Keep ancestry intact until the supervisor performs cleanup.
            elif command == "fail":
                print("fixture provider failed", file=sys.stderr, flush=True)
                return 7
            else:
                raise ValueError(f"unknown fixture command: {command}")
    raise RuntimeError("fixture FIFO closed without an exit command")


if __name__ == "__main__":
    raise SystemExit(main())
