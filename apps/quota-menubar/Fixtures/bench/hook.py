#!/usr/bin/env python3
"""Measure the Claude pre-tool hook using an empty or disposable populated state root."""
import argparse
import json
import math
import os
import resource
import subprocess
import sys
import tempfile
import time


def p95(values):
    return sorted(values)[math.ceil(len(values) * 0.95) - 1]


def delivered(output, session):
    """Count only a hook response containing a non-empty answer-offer page."""
    try:
        wire = json.loads(output)
        hook = wire["hookSpecificOutput"]
        if hook["hookEventName"] != "PreToolUse":
            return False
        context = hook["additionalContext"]
        prefix, suffix = "<untrusted_answer_data>\n", "\n</untrusted_answer_data>"
        if context.count(prefix) != 1 or not context.endswith(suffix):
            return False
        page = json.loads(context.split(prefix, 1)[1][:-len(suffix)])
        return isinstance(page["items"], list) and any(
            isinstance(item, dict) and isinstance(item.get("answer"), dict)
            and isinstance(item.get("delivery"), dict)
            and isinstance(item["answer"].get("id"), str) and bool(item["answer"]["id"])
            and item["answer"]["id"] == item["delivery"].get("answer_id")
            and item["delivery"].get("target_session") == session
            for item in page["items"]
        )
    except (ValueError, KeyError, TypeError, AttributeError):
        return False


def measure(args, root):
    environment = os.environ.copy()
    for key in ("PROVENANT_RUN_STATE_DIR", "PROVENANT_LANE_DEPTH", "PROVENANT_PARENT_ATTEMPT_ID", "PROVENANT_AGENT_LABEL"):
        environment.pop(key, None)  # Represent a chair, even when run from a lane.
    command = [args.binary, "--state-dir", root, "hook", "claude", "pre-tool-use"]
    event = {"hook_event_name": "PreToolUse", "cwd": args.cwd, "tool_name": "Read", "tool_input": {"file_path": "/dev/null"}}
    durations = {"baseline": [], "session": []}
    diagnostics = {"baseline": set(), "session": set()}
    output_bytes = {"baseline": 0, "session": 0}
    delivered_samples = 0
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    # Alternate ordering to reduce drift; the baseline omits the session ID.
    for index in range(args.samples):
        for name in (["baseline", "session"] if index % 2 == 0 else ["session", "baseline"]):
            payload = dict(event)
            if name == "session":
                payload["session_id"] = args.session
            start = time.monotonic_ns()
            result = subprocess.run(command, input=json.dumps(payload).encode(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, env=environment)
            durations[name].append((time.monotonic_ns() - start) / 1e6)
            output_bytes[name] = max(output_bytes[name], len(result.stdout))
            if name == "session" and delivered(result.stdout, args.session):
                delivered_samples += 1
            if result.stderr:
                diagnostics[name].add(result.stderr.decode(errors="replace").strip())
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    if args.state_dir and delivered_samples != args.samples:
        raise RuntimeError(f"provided fixture delivered {delivered_samples}/{args.samples} session samples "
                           f"(delivered_samples={delivered_samples}); refusing delivery timings")
    paired = [session - base for session, base in zip(durations["session"], durations["baseline"])]
    result = {
        "samples_per_case": args.samples,
        "fixture": "provided state root" if args.state_dir else "empty, unregistered session",
        "delivered_samples": delivered_samples,
        "baseline_wall_p95_ms": round(p95(durations["baseline"]), 3),
        "child_cpu_seconds_total": round(after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 3),
        "max_stdout_bytes": output_bytes,
        "diagnostics": {name: sorted(values) for name, values in diagnostics.items()},
    }
    # getrusage reports a lifetime maximum, in bytes on macOS and KiB on Linux.
    if sys.platform == "darwin":
        result["children_lifetime_max_rss_bytes"] = after.ru_maxrss
    elif sys.platform.startswith("linux"):
        result["children_lifetime_max_rss_bytes"] = after.ru_maxrss * 1024
    if args.state_dir:
        result["delivery_wall_p95_ms"] = round(p95(durations["session"]), 3)
        result["delivery_paired_added_wall_p95_ms"] = round(p95(paired), 3)
    else:
        result["unregistered_lookup_wall_p95_ms"] = round(p95(durations["session"]), 3)
        result["unregistered_lookup_paired_added_wall_p95_ms"] = round(p95(paired), 3)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", help="absolute path to the CLI under test")
    parser.add_argument("--state-dir", help="disposable populated state root (default: empty temporary root)")
    parser.add_argument("--cwd", default=os.getcwd(), help="project directory for session lookup")
    parser.add_argument("--session", default="benchmark-session")
    parser.add_argument("--samples", type=int, default=100)
    args = parser.parse_args()
    if args.samples < 20 or not os.path.isabs(args.binary) or not os.path.isabs(args.cwd):
        parser.error("use at least 20 samples and absolute binary and cwd paths")
    if args.state_dir:
        if not os.path.isabs(args.state_dir):
            parser.error("state root must be absolute")
        result = measure(args, args.state_dir)
    else:
        with tempfile.TemporaryDirectory(prefix="provenant-hook-bench-") as root:
            result = measure(args, root)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
