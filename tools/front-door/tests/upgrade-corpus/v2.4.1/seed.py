#!/usr/bin/env python3
"""Materialise v2.4.1 custody records with identities from this filesystem."""
import json
import os
import pathlib
import sys

state, project = pathlib.Path(sys.argv[1]), sys.argv[2]
corpus = pathlib.Path(__file__).resolve().parent
attempt_id = "02020202020202020202020202020202"
slot = state / "worktrees" / project / "legacy-writer"
evidence = slot / attempt_id
attempt = state / "attempts" / attempt_id
# Match the private directories made by the run store and custody writers.
# In particular, pathlib's implicit parents otherwise use 0777 before umask.
for directory in (state / "worktrees", state / "worktrees" / project, slot,
                  evidence, state / "attempts", attempt):
    directory.mkdir(mode=0o700, exist_ok=True)
for path in (slot / "writer.lock", evidence / "guardian.lock", attempt / "custody.lock"):
    path.touch(mode=0o600)

def identity(path):
    st = path.stat()
    return {"device": st.st_dev, "inode": st.st_ino}

def load(name):
    return json.loads((corpus / name).read_text())

def write_record(path, value):
    # atomic_write's published files are private (0600).
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as f:
        f.write(json.dumps(value) + "\n")

def save(name, value):
    write_record(evidence / name, value)

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
settled.mkdir(mode=0o700)
settlement = load("settlement.json")
settlement["scope"]["attempt"] = settled.name
write_record(settled / "settlement.json", settlement)
