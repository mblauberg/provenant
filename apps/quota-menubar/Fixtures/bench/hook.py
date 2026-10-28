#!/usr/bin/env python3
"""Measure the Claude pre-tool hook with and without a session lookup.

Use a disposable state root. A populated fixture exercises a registered session
and answer; repeated delivery can change its outbox state.
"""
import argparse
import json
import math
import os
import resource
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", help="absolute path to the CLI under test")
parser.add_argument("--state-dir", help="disposable populated state root (default: empty temporary root)")
parser.add_argument("--cwd", default=os.getcwd(), help="project directory for session lookup")
parser.add_argument("--session", default="benchmark-session")
parser.add_argument("--samples", type=int, default=100)
args = parser.parse_args()
environment = os.environ.copy()
for key in ("PROVENANT_RUN_STATE_DIR", "PROVENANT_LANE_DEPTH", "PROVENANT_PARENT_ATTEMPT_ID", "PROVENANT_AGENT_LABEL"):
    environment.pop(key, None)  # The fixture represents a chair, even when run from a lane.
if args.samples < 20 or not os.path.isabs(args.binary) or not os.path.isabs(args.cwd):
    parser.error("use at least 20 samples and absolute binary and cwd paths")


def p95(values):
    return sorted(values)[math.ceil(len(values) * 0.95) - 1]


def measure(root):
    command = [args.binary, "--state-dir", root, "hook", "claude", "pre-tool-use"]
    event = {"hook_event_name": "PreToolUse", "cwd": args.cwd, "tool_name": "Read", "tool_input": {"file_path": "/dev/null"}}
    durations = {"baseline": [], "session": []}
    diagnostics = {"baseline": set(), "session": set()}
    output_bytes = {"baseline": 0, "session": 0}
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    # Alternate ordering to reduce drift. The baseline follows the same hook path
    # but omits the session ID, so it skips the answer lookup.
    for index in range(args.samples):
        for name in (["baseline", "session"] if index % 2 == 0 else ["session", "baseline"]):
            payload = dict(event)
            if name == "session":
                payload["session_id"] = args.session
            start = time.monotonic_ns()
            result = subprocess.run(command, input=json.dumps(payload).encode(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, env=environment)
            durations[name].append((time.monotonic_ns() - start) / 1e6)
            output_bytes[name] = max(output_bytes[name], len(result.stdout))
            if result.stderr:
                diagnostics[name].add(result.stderr.decode(errors="replace").strip())
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    if args.state_dir and not output_bytes["session"]:
        raise RuntimeError("provided fixture did not deliver an answer; timings cannot represent delivery")
    paired = [session - base for session, base in zip(durations["session"], durations["baseline"])]
    print(json.dumps({
        "samples_per_case": args.samples,
        "fixture": "provided state root" if args.state_dir else "empty, unregistered session",
        "baseline_wall_p95_ms": round(p95(durations["baseline"]), 3),
        "session_wall_p95_ms": round(p95(durations["session"]), 3),
        "paired_added_wall_p95_ms": round(p95(paired), 3),
        "child_cpu_seconds_total": round(after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 3),
        "child_max_rss_bytes": after.ru_maxrss,
        "max_stdout_bytes": output_bytes,
        "diagnostics": {name: sorted(values) for name, values in diagnostics.items()},
    }, indent=2))


if args.state_dir:
    if not os.path.isabs(args.state_dir):
        parser.error("state root must be absolute")
    measure(args.state_dir)
else:
    with tempfile.TemporaryDirectory(prefix="provenant-hook-bench-") as root:
        measure(root)
