"""Run source JSON interface operations for ordered Rust wire differentials."""

import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
scripts = root / "skills/orchestrate/scripts"
sys.path.insert(0, str(root / "skills"))
sys.path.insert(0, str(root / "scripts"))
sys.path.insert(0, str(scripts))

import dispatch_preflight
import provider_observer

unit = sys.argv[2]
cases = json.load(sys.stdin)
results = []


def wire_result(value):
    wire = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if any(0xD800 <= ord(character) <= 0xDFFF for character in wire):
        return {"surrogatepass_hex": wire.encode("utf-8", "surrogatepass").hex()}
    return {"wire": wire}


for case in cases:
    if unit in {"preflight_wire", "preflight_decode"}:
        try:
            route = dispatch_preflight.parse_fast_route_json(case["raw"])
            results.append(wire_result(route))
        except ValueError as error:
            results.append({"error": str(error)})
    elif unit == "observer_live_wire":
        plan = json.loads(case["plan_raw"]) if "plan_raw" in case else case["plan"]
        observer = provider_observer.LiveObserver(plan)
        for line in case.get("lines", []):
            observer.feed_stdout((json.dumps(line) + "\n").encode())
        for line in case.get("raw_lines", []):
            observer.feed_stdout((line + "\n").encode())
        for chunk in case.get("diagnostics", []):
            observer.feed_stderr(chunk["text"].encode(), chunk["stream"])
        summary = observer.result()
        results.append(wire_result(summary))
    else:
        raise ValueError(f"unknown oracle unit: {unit}")

sys.stdout.write(json.dumps(results, separators=(",", ":")))
sys.stdout.write("\n")
