"""Pick the next batch of ready port units for one crate.

Usage: python3 docs/specs/rust-port/ready.py CRATE [MAX_LANE_DAYS] [EXTRA_LEDGER]

A unit is ready when it targets CRATE, is not listed in ported.txt (or the
optional extra ledger of in-flight ids), and every dependency is done or
already in the batch. Partial units stay ready, so a later batch continues them.
Retired units are outside the port; their deletion criteria remain in the graph.
"""
import json
import sys
from pathlib import Path

here = Path(__file__).parent
graph = json.loads((here / "graph.json").read_text())
crate = sys.argv[1]
budget = float(sys.argv[2]) if len(sys.argv) > 2 else 3.0
ledgers = [here / "ported.txt"] + [Path(p) for p in sys.argv[3:4]]
taken = {
    line.strip()
    for ledger in ledgers
    for line in ledger.read_text().splitlines()
    if line.strip() and not line.startswith("#")
}
nodes = {node["id"]: node for node in graph["nodes"]}
done = taken | {i for i in nodes if i.startswith("prerequisite:") or nodes[i].get("action") == "retired"} | {"workspace:manifest-and-digest"}
batch, spent = [], 0.0
for unit in (i for wave in graph["leaves_first_waves"] for i in wave):
    node = nodes[unit]
    if node.get("target_crate") != crate or unit in done or node.get("dispatchable") is False:
        continue
    if not set(node["depends_on"]) <= done | set(batch):
        continue
    if batch and spent + node["lane_days"] > budget:
        break
    batch.append(unit)
    spent += node["lane_days"]
for unit in batch:
    node = nodes[unit]
    spans = "; ".join(
        f"{s['start']}-{s['end']} {','.join(s.get('symbols', []))[:120]}"
        for s in node.get("source_spans", [])
    )
    line = f"- `{unit}` ({node['kind']}, {node['action']}, risk {node['risk']}): "
    line += ", ".join(node["source_files"][:6])
    if spans:
        line += f" lines {spans}"
    if node.get("description"):
        line += f". {node['description']}"
    print(line)
print(f"{len(batch)} units, {spent} lane-days", file=sys.stderr)
