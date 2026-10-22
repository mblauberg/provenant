#!/usr/bin/env python3
"""Guard and run the explicit Python test-file shard manifest."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / ".github/workflows/python-test-shards.json"
WORKFLOW = ROOT / ".github/workflows/ci.yml"
EXTRA_TESTS = ("skills/implement/tests/test_checkpoint_run.py",)


def test_files() -> list[str]:
    files = {
        path.relative_to(ROOT).as_posix()
        for path in (ROOT / "tests").rglob("test_*.py")
        if path.is_file()
    }
    files.update(EXTRA_TESTS)
    return sorted(files)


def load_shards() -> dict[str, list[str]]:
    document = json.loads(MANIFEST.read_text(encoding="utf-8"))
    shards = document.get("shards")
    if not isinstance(shards, dict) or not shards:
        raise SystemExit("python test shard manifest must contain a non-empty shards object")
    if any(not isinstance(name, str) or not isinstance(files, list) for name, files in shards.items()):
        raise SystemExit("python test shard manifest must map shard names to file lists")
    if any(not isinstance(path, str) for files in shards.values() for path in files):
        raise SystemExit("python test shard file entries must be strings")
    return shards


def validate_files(shards: dict[str, list[str]]) -> None:
    if shards.get("fabric-hosts") != ["tests/test_fabric_hosts.py"]:
        raise SystemExit("the fabric-hosts shard must contain only tests/test_fabric_hosts.py")
    counts = Counter(path for files in shards.values() for path in files)
    expected = set(test_files())
    actual = set(counts)
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    duplicated = sorted(path for path, count in counts.items() if count != 1)
    if missing or unknown or duplicated:
        details = []
        if missing:
            details.append(f"unassigned test files: {', '.join(missing)}")
        if unknown:
            details.append(f"unknown manifest files: {', '.join(unknown)}")
        if duplicated:
            details.append(f"files assigned more than once: {', '.join(duplicated)}")
        raise SystemExit("; ".join(details))


def validate_matrix(shards: dict[str, list[str]]) -> None:
    workflow = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
    jobs = workflow.get("jobs", {}) if isinstance(workflow, dict) else {}
    test_job = jobs.get("python-tests", {}) if isinstance(jobs, dict) else {}
    strategy = test_job.get("strategy", {}) if isinstance(test_job, dict) else {}
    matrix = strategy.get("matrix", {}) if isinstance(strategy, dict) else {}
    names = matrix.get("shard", []) if isinstance(matrix, dict) else []
    if names != list(shards):
        raise SystemExit(
            f"workflow Python test matrix {names!r} does not match manifest shards {list(shards)!r}"
        )


def collect(files: list[str]) -> list[str]:
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "--collect-only", "-q", *files],
        cwd=ROOT,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    if result.returncode:
        sys.stdout.write(result.stdout)
        raise SystemExit(f"pytest collection failed (exit {result.returncode})")
    return [line for line in result.stdout.splitlines() if "::" in line]


def check_collections(shards: dict[str, list[str]]) -> None:
    full = collect(test_files())
    nodes_by_shard = {name: collect(files) for name, files in shards.items()}
    shard_nodes = [node for nodes in nodes_by_shard.values() for node in nodes]
    full_counts = Counter(full)
    shard_counts = Counter(shard_nodes)
    missing = sorted(full_counts.keys() - shard_counts.keys())
    extra = sorted(shard_counts.keys() - full_counts.keys())
    duplicated = sorted(node for node, count in shard_counts.items() if count != 1)
    if missing or extra or duplicated or full_counts != shard_counts:
        details = []
        if missing:
            details.append(f"not collected by a shard: {', '.join(missing[:10])}")
        if extra:
            details.append(f"not in full collection: {', '.join(extra[:10])}")
        if duplicated:
            details.append(f"collected by multiple shards: {', '.join(duplicated[:10])}")
        raise SystemExit("collection coverage mismatch: " + "; ".join(details))
    print(f"Collection coverage OK: {len(full)} test cases across {len(shards)} shards.")
    general_counts = [len(nodes_by_shard[name]) for name in ("python-a", "python-b") if name in nodes_by_shard]
    if len(general_counts) == 2 and max(general_counts) - min(general_counts) > sum(general_counts) // 10:
        raise SystemExit(f"general shard collections are unbalanced: {general_counts}")
    for name, nodes in nodes_by_shard.items():
        print(f"{name}: {len(nodes)} test cases")


def run_shard(name: str, files: list[str]) -> None:
    workers = ["-n", "auto"]
    command = [
        sys.executable,
        "-m",
        "pytest",
        "-q",
        "--durations=30",
        "--durations-min=1",
        *workers,
        *files,
    ]
    print(f"Running shard {name}: {len(files)} test files", flush=True)
    raise SystemExit(subprocess.run(command, cwd=ROOT, check=False).returncode)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="validate file and collection coverage")
    mode.add_argument("--run", metavar="SHARD", help="run one named shard")
    args = parser.parse_args()

    shards = load_shards()
    validate_files(shards)
    validate_matrix(shards)
    if args.check:
        check_collections(shards)
        for name, files in shards.items():
            print(f"{name}: {len(files)} files")
        return
    if args.run not in shards:
        raise SystemExit(f"unknown shard {args.run!r}; expected one of: {', '.join(shards)}")
    run_shard(args.run, shards[args.run])


if __name__ == "__main__":
    main()
