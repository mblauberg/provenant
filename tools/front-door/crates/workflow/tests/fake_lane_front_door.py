#!/usr/bin/env python3
"""Inert process fixture for the Python lane-launch differential, never live."""
import json
import os
from pathlib import Path
import sys

json.load(sys.stdin)
case = json.loads(Path(os.environ["PORT_FIXTURE_PATH"]).read_text(encoding="utf-8"))
if case.get("published") is not None:
    Path(case["attempt_dir"], "supervisor.exit.json").write_text(case["published"], encoding="utf-8")
sys.stdout.buffer.write(case["stdout"].encode())
sys.stderr.buffer.write(case.get("stderr", "").encode())
sys.exit(case.get("returncode", 0))
