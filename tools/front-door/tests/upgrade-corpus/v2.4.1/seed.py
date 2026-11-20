#!/usr/bin/env python3
"""Materialise v2.4.1 custody records with identities from this filesystem."""
import json
import pathlib
import sys

state, project = pathlib.Path(sys.argv[1]), sys.argv[2]
corpus = pathlib.Path(__file__).resolve().parent
attempt_id = "02020202020202020202020202020202"
slot = state / "worktrees" / project / "legacy-writer"
evidence = slot / attempt_id
attempt = state / "attempts" / attempt_id
evidence.mkdir(parents=True)
attempt.mkdir(parents=True)
for path in (slot / "writer.lock", evidence / "guardian.lock", attempt / "custody.lock"):
    path.touch(mode=0o600)

def identity(path):
    st = path.stat()
    return {"device": st.st_dev, "inode": st.st_ino}

def load(name):
    return json.loads((corpus / name).read_text())

def save(name, value):
    (evidence / name).write_text(json.dumps(value) + "\n")

reservation = load("reservation.json")
reservation["attempt_device"] = identity(attempt)["device"]
reservation["attempt_inode"] = identity(attempt)["inode"]
reservation["reservation_device"] = identity(evidence)["device"]
reservation["reservation_inode"] = identity(evidence)["inode"]
save("reservation.json", reservation)
guardian = load("guardian-lock.json")
for key, path in (("attempt", attempt), ("guardian", evidence / "guardian.lock"),
                  ("writer", slot / "writer.lock"), ("custody", attempt / "custody.lock")):
    guardian[key] = identity(path)
save("guardian-lock.json", guardian)
# v2.4.1 clear_reservation writes the settlement and removes the reservation.
# Keep this as a separate settled attempt, alongside the retained legacy one.
settled = slot / "03030303030303030303030303030303"
settled.mkdir()
settlement = load("settlement.json")
settlement["scope"]["attempt"] = settled.name
(settled / "settlement.json").write_text(json.dumps(settlement) + "\n")
