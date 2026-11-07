#!/usr/bin/env python3
"""Generate the pre-cut terminal fixture from a supplied immutable binary."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def capture(binary, git, old):
    with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as scratch:
        root = Path(scratch).resolve()
        project, state, home = (root / name for name in ("project", "state", "home"))
        project.mkdir()
        home.mkdir()
        env = {
            "HOME": str(home),
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "PROVENANT_GIT": str(git),
            "PROVENANT_STATE_DIR": str(state),
            "PROVENANT_ROOT": str(project),
            "PROVENANT_AGENT_LABEL": "fixture",
            "PROVENANT_DURABILITY": "relaxed",
            "TMPDIR": str(root),
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_COUNT": "2",
            "GIT_CONFIG_KEY_0": "core.hooksPath",
            "GIT_CONFIG_VALUE_0": "/dev/null",
            "GIT_CONFIG_KEY_1": "commit.gpgsign",
            "GIT_CONFIG_VALUE_1": "false",
            "GIT_AUTHOR_NAME": "Fixture",
            "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
            "GIT_COMMITTER_NAME": "Fixture",
            "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        }
        subprocess.run([git, "init", "-q"], cwd=project, env=env, check=True)
        subprocess.run([git, "commit", "--allow-empty", "-qm", "fixture"],
                       cwd=project, env=env, check=True)
        document = {
            "schema_version": 1,
            "graph_id": "old-terminal-fixture",
            "nodes": [{"id": "budget", "kind": "gate", "access": "read_only",
                       "condition": {"type": "budget"}}],
        }
        (project / "graph.json").write_bytes(canonical(document))
        args = (["fabric", "graph", "run", "--document", "graph.json"] if old
                else ["run", "--file", "graph.json"])
        started = time.monotonic_ns()
        result = subprocess.run([binary, *args, "--json"], cwd=project,
                                env=env, check=True, capture_output=True)
        elapsed_us = (time.monotonic_ns() - started) // 1000
        reply = json.loads(result.stdout)
        assert reply["data"]["status"] == "complete", reply
        assert reply["data"]["checkpoint"]["nodes"][0]["state"] == "succeeded", reply
        files = list((state / "fabric" / "graphs").rglob("graph-old-terminal-fixture.json"))
        assert len(files) == 1, files
        raw = files[0].read_bytes()
        envelope = json.loads(raw)
        version = subprocess.check_output([binary, "--version"], cwd=project, env=env).decode().strip()
        return envelope, {
            "version": version,
            "binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
            "elapsed_microseconds": elapsed_us,
            "checkpoint_bytes": len(raw),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--current", type=Path)
    parser.add_argument("--git", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    envelope, measurements = capture(args.baseline.resolve(), args.git.resolve(), True)
    # The pre-cut checkpoint payload stays byte-for-byte canonical-identical.
    # Only its execution directory and the envelope's integrity digest change.
    payload_digest = hashlib.sha256(canonical(envelope["checkpoint"])).hexdigest()
    envelope["execution"]["directory"] = "/synthetic/graph-cutover"
    framed = b"provenant-record\0fabric-graph-checkpoint\0" + b"2\0"
    framed += canonical([envelope["checkpoint"], envelope["execution"]])
    envelope["digest"] = "sha256:" + hashlib.sha256(framed).hexdigest()
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "old-terminal-checkpoint.json").write_bytes(canonical(envelope) + b"\n")
    provenance = {
        "schema_version": 1,
        "generator": "generate-terminal.py",
        "baseline_version": measurements["version"],
        "baseline_binary_sha256": measurements["binary_sha256"],
        "checkpoint_payload_sha256": payload_digest,
        "normalisation": "execution.directory rebound; envelope digest recomputed; checkpoint unchanged",
    }
    (args.output / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    report = {"baseline": measurements}
    if args.current:
        _, report["current"] = capture(args.current.resolve(), args.git.resolve(), False)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
