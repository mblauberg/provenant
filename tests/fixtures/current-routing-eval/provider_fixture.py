#!/usr/bin/env python3
"""Provider-free adapter for the current routing evaluation tracer."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import time
from pathlib import Path


def _counter(state: Path, delta: int) -> int:
    state.mkdir(parents=True, exist_ok=True)
    with (state / "lock").open("a+") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        active_path = state / "active"
        maximum_path = state / "maximum"
        active = int(active_path.read_text()) if active_path.exists() else 0
        active += delta
        active_path.write_text(str(active), encoding="utf-8")
        maximum = int(maximum_path.read_text()) if maximum_path.exists() else 0
        if active > maximum:
            maximum_path.write_text(str(active), encoding="utf-8")


parser = argparse.ArgumentParser()
parser.add_argument("--tool", required=True)
parser.add_argument("--prompt-file", required=True, type=Path)
parser.add_argument("--out", required=True, type=Path)
parser.add_argument("--intent", required=True)
args, _ = parser.parse_known_args()

values = dict(
    line.split("=", 1)
    for line in args.prompt_file.read_text(encoding="utf-8").splitlines()
    if "=" in line
)
family = values["fixture_family"]
status = values.get("fixture_status", "succeeded")
state = Path(os.environ["CURRENT_ROUTING_FIXTURE_STATE"])
_counter(state, 1)
try:
    time.sleep(float(values.get("fixture_sleep", "0")))
finally:
    _counter(state, -1)

output_digest = ""
if status == "succeeded":
    args.out.write_text("deterministic fixture result\n", encoding="utf-8")
    output_digest = "sha256:" + hashlib.sha256(args.out.read_bytes()).hexdigest()

record = {
    "tool": args.tool,
    "adapter": args.tool,
    "execution_intent": args.intent,
    "resolved_model": f"fixture-{family}-model",
    "provider_family": family,
    "model_family": family,
    "endpoint_provider": f"fixture-{family}",
    "identity_source": "deterministic-fixture",
    "status": "ok" if status == "succeeded" else "error",
    "exit": 0 if status == "succeeded" else 1,
    "output_path": str(args.out) if status == "succeeded" else "",
    "output_digest": output_digest,
    "read_only_guarantee": "fixture",
    "cross_family": False,
    "certification_eligible": False,
}
print(json.dumps(record, sort_keys=True))
raise SystemExit(record["exit"])
