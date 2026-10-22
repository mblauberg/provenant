#!/usr/bin/env python3
"""Median wall time of `provenant root` and `provenant help`, per role.

Not a pass/fail gate and not collected by pytest. Record one run per
implementation and compare the JSON::

    python tests/front_door_parity/bench_front_door.py --impl python --out python.json
    python tests/front_door_parity/bench_front_door.py --impl rust --out rust.json
    python tests/front_door_parity/bench_front_door.py --compare python.json rust.json

Each figure is the median of `--runs` (default 30) process launches after one
warm-up, with `/usr/bin/true` timed the same way as the fork+exec floor.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
VERBS = ("root", "help")


def timed(command: list[str], env: dict[str, str], cwd: Path, runs: int) -> dict[str, float]:
    samples = []
    for attempt in range(runs + 1):
        started = time.perf_counter()
        done = subprocess.run(
            command, env=env, cwd=cwd, stdin=subprocess.DEVNULL, capture_output=True, check=False
        )
        elapsed = (time.perf_counter() - started) * 1000
        if done.returncode != 0:
            raise SystemExit(f"{' '.join(command)} exited {done.returncode}: {done.stderr.decode()[:300]}")
        if attempt:  # the first launch is the warm-up
            samples.append(elapsed)
    samples.sort()
    return {
        "median_ms": round(statistics.median(samples), 2),
        "mean_ms": round(statistics.fmean(samples), 2),
        "min_ms": round(samples[0], 2),
        "p90_ms": round(samples[int(0.9 * (len(samples) - 1))], 2),
        "max_ms": round(samples[-1], 2),
    }


def measure(runs: int) -> dict:
    from parity_support import IMPL, ROLES, FrontDoor

    results: dict = {
        "impl": IMPL,
        "runs": runs,
        "host": f"{platform.system()} {platform.release()} {platform.machine()}",
        "python": platform.python_version(),
        "roles": {},
    }
    with tempfile.TemporaryDirectory() as scratch:
        scratch_path = Path(scratch)
        results["floor_true_ms"] = timed(["/usr/bin/true"], dict(os.environ), scratch_path, runs)
        for role in ROLES:
            workdir = scratch_path / role
            workdir.mkdir()
            door = FrontDoor(workdir, role)
            env = door.env(AGENT_FABRIC_PRODUCT_ROOT=str(door.checkout))
            results["roles"][role] = {
                verb: timed([str(door.command), verb], env, door.cwd, runs) for verb in VERBS
            }
    return results


def compare(python_file: Path, rust_file: Path) -> None:
    python, rust = (json.loads(path.read_text()) for path in (python_file, rust_file))
    print(f"{'role':<11}{'verb':<6}{'python ms':>11}{'rust ms':>10}{'speed-up':>10}")
    for role in python["roles"]:
        for verb in VERBS:
            before = python["roles"][role][verb]["median_ms"]
            after = rust["roles"][role][verb]["median_ms"]
            print(f"{role:<11}{verb:<6}{before:>11.1f}{after:>10.1f}{before / after:>9.1f}x")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument("--impl", choices=("python", "rust"))
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--compare", nargs=2, type=Path, metavar=("PYTHON_JSON", "RUST_JSON"))
    args = parser.parse_args()
    if args.compare:
        compare(*args.compare)
        return 0
    if args.impl:
        os.environ["PROVENANT_PARITY_IMPL"] = args.impl
    results = measure(args.runs)
    text = json.dumps(results, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.write_text(text)
    print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
