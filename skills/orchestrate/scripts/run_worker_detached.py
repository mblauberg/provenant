#!/usr/bin/env python3
"""Run a worker with durable completion evidence for a detached caller."""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import time


STARTUP_GRACE_SECONDS = 2


def read_single_value(path: Path) -> str | None:
    try:
        content = path.read_text()
    except OSError:
        return None
    if content.count("\n") != 1:
        return None
    value = content.split("\n", 1)[0]
    return value or None


def process_exists(pid: str) -> bool:
    try:
        os.kill(int(pid), 0)
    except (OSError, ValueError):
        return False
    return True


def within_startup_grace(path: Path) -> bool:
    try:
        age = time.time() - path.stat().st_mtime
    except OSError:
        return False
    return 0 <= age <= STARTUP_GRACE_SECONDS


def validate_run(run_dir: Path) -> int:
    done = run_dir / "done"
    wrapper_path = run_dir / "wrapper.pid"
    worker_path = run_dir / "worker.pid"
    wrapper_pid = read_single_value(wrapper_path)
    if wrapper_pid is None:
        if not done.exists() and within_startup_grace(run_dir):
            return 1
        print("completion evidence missing wrapper PID", file=sys.stderr)
        return 2
    if not wrapper_pid.isdigit():
        print("completion evidence has invalid wrapper PID", file=sys.stderr)
        return 2

    worker_pid = read_single_value(worker_path)
    if worker_pid is None:
        if process_exists(wrapper_pid):
            try:
                if done.stat().st_size == 0:
                    return 1
            except OSError:
                return 1
        print("completion evidence missing worker PID", file=sys.stderr)
        return 2
    if not worker_pid.isdigit():
        print("completion evidence has invalid worker PID", file=sys.stderr)
        return 2

    try:
        done_text = done.read_text()
    except OSError:
        done_text = ""
    if not done_text:
        if process_exists(wrapper_pid):
            return 1
        print("completion evidence missing: wrapper exited without marker", file=sys.stderr)
        return 2

    lines = done_text.splitlines()
    exit_values = re.findall(r"^exit=([0-9]+)$", done_text, re.MULTILINE)
    if not exit_values:
        print("completion evidence marker has invalid exit status", file=sys.stderr)
        return 2
    values: dict[str, list[str]] = {"wrapper_pid": [], "worker_pid": [], "exit": []}
    for line in lines:
        key, separator, value = line.partition("=")
        if separator and key in values and value.isdigit():
            values[key].append(value)
    if (
        done_text.count("\n") != 3
        or len(lines) != 3
        or any(len(values[key]) != 1 for key in values)
        or values["wrapper_pid"][0] != wrapper_pid
        or values["worker_pid"][0] != worker_pid
    ):
        print(
            "completion evidence marker is malformed or does not match persisted PIDs",
            file=sys.stderr,
        )
        return 2
    exit_status = values["exit"][0]
    if process_exists(wrapper_pid):
        return 1
    if process_exists(worker_pid):
        print("completion evidence worker PID is still live", file=sys.stderr)
        return 2
    print(worker_pid, wrapper_pid, exit_status)
    return 0


def write_atomic(path: Path, value: str) -> bool:
    temporary = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_text(value + "\n")
        os.replace(temporary, path)
    except OSError:
        temporary.unlink(missing_ok=True)
        return False
    return True


def parse_args(args: list[str]) -> tuple[bool, str | None, list[str]]:
    validate = False
    run_dir: str | None = None
    index = 0
    while index < len(args):
        argument = args[index]
        if argument == "--validate":
            validate = True
            index += 1
        elif argument == "--run-dir":
            if index + 1 >= len(args):
                print("--run-dir needs a value", file=sys.stderr)
                raise SystemExit(2)
            run_dir = args[index + 1]
            index += 2
        elif argument == "--":
            return validate, run_dir, args[index + 1 :]
        else:
            print(f"unknown argument: {argument}", file=sys.stderr)
            raise SystemExit(2)
    return validate, run_dir, []


def main(args: list[str] | None = None) -> int:
    validate, run_dir_arg, command = parse_args(sys.argv[1:] if args is None else args)
    if not run_dir_arg:
        print("--run-dir is required", file=sys.stderr)
        return 2
    run_dir = Path(run_dir_arg)
    if validate:
        if not run_dir.is_dir():
            print(f"run directory does not exist: {run_dir}", file=sys.stderr)
            return 2
        if command:
            print("--validate does not accept a worker command", file=sys.stderr)
            return 2
        return validate_run(run_dir)
    if not command:
        print("worker command is required after --", file=sys.stderr)
        return 2

    parent = run_dir.parent
    try:
        parent.mkdir(parents=True, exist_ok=True)
    except OSError:
        print(f"cannot create run-directory parent: {parent}", file=sys.stderr)
        return 2
    try:
        run_dir.mkdir()
    except OSError:
        print(f"run directory already exists or cannot be claimed: {run_dir}", file=sys.stderr)
        return 2
    if not os.access(run_dir, os.W_OK):
        print(f"run directory is not writable: {run_dir}", file=sys.stderr)
        return 2

    wrapper_pid = str(os.getpid())
    worker_pid_path = run_dir / "worker.pid"
    wrapper_pid_path = run_dir / "wrapper.pid"
    if not write_atomic(wrapper_pid_path, wrapper_pid):
        print("cannot persist wrapper PID before launch", file=sys.stderr)
        return 2
    transcript = run_dir / "transcript.txt"
    try:
        transcript.write_text("")
    except OSError:
        print(f"run transcript is not writable: {transcript}", file=sys.stderr)
        return 2

    with transcript.open("ab") as output:
        try:
            worker = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        except OSError as exc:
            # The shell helper's failed exec diagnostic is part of the worker
            # transcript, just like output from a successfully launched child.
            output.write(
                f"{sys.argv[0]}: line 172: {command[0]}: {exc.strerror}\n".encode()
            )
            worker = subprocess.Popen(
                ["/bin/bash", "-c", "exit 127" if isinstance(exc, FileNotFoundError) else "exit 126"],
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            worker_pid = str(worker.pid)
            if not write_atomic(worker_pid_path, worker_pid):
                print(
                    "cannot persist worker PID after launch; terminating worker",
                    file=sys.stderr,
                )
                worker.terminate()
                worker.wait()
                return 1
            return_code = worker.wait()
        else:
            worker_pid = str(worker.pid)
            if not write_atomic(worker_pid_path, worker_pid):
                print(
                    "cannot persist worker PID after launch; terminating worker",
                    file=sys.stderr,
                )
                worker.terminate()
                worker.wait()
                return 1
            return_code = worker.wait()
            if return_code < 0:
                return_code = 128 + -return_code

    temporary = run_dir / f"done.tmp.{os.getpid()}"
    try:
        temporary.write_text(
            f"wrapper_pid={wrapper_pid}\nworker_pid={worker_pid}\nexit={return_code}\n"
        )
        os.replace(temporary, run_dir / "done")
    except OSError:
        temporary.unlink(missing_ok=True)
        print("cannot persist durable completion evidence", file=sys.stderr)
        return 1
    return return_code


if __name__ == "__main__":
    raise SystemExit(main())
