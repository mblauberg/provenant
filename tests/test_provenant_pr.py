from pathlib import Path
import json
import os
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/provenant_pr.py"

# A stub gh that replays queued responses per subcommand; the last one repeats.
STUB = """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
state = Path(os.environ["GH_STUB_DIR"])
key = "api" if sys.argv[1] == "api" and "rules/branches" in sys.argv[2] else "none" if sys.argv[1] == "api" else "view" if sys.argv[2] == "view" else ("required" if "--required" in sys.argv else "checks")
queue = json.loads((state / f"{key}.json").read_text())
if key == "none":
    sys.exit(1)
index = int((state / f"{key}.n").read_text()) if (state / f"{key}.n").exists() else 0
(state / f"{key}.n").write_text(str(index + 1))
reply = queue[min(index, len(queue) - 1)]
sys.stdout.write(reply.get("out", ""))
sys.stderr.write(reply.get("err", ""))
sys.exit(reply.get("code", 0))
"""


def checks(*rows):
    return {"out": json.dumps([{"name": n, "bucket": b, "link": ""} for n, b in rows])}


def pr(head, state="OPEN"):
    return {"out": json.dumps({"number": 7, "headRefOid": head * 40, "state": state, "baseRefName": "main"})}


def run(tmp_path, *arguments, view, required, every=None, timeout="5", rules=None):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    gh = bin_dir / "gh"
    gh.write_text(STUB)
    gh.chmod(0o755)
    for name, queue in {"view": view, "required": required, "checks": every or [{"out": "[]"}], "api": rules or [{"out": "[]"}]}.items():
        (tmp_path / f"{name}.json").write_text(json.dumps(queue))
    env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "GH_STUB_DIR": str(tmp_path),
           "PROVENANT_PR_BACKOFF": "0.01,0.02"}
    return subprocess.run([sys.executable, "-I", str(SCRIPT), "checks", "7", "--wait", "--timeout", timeout, *arguments],
                          env=env, text=True, capture_output=True)


def test_all_green_after_pending_and_unregistered_checks(tmp_path):
    result = run(
        tmp_path,
        view=[pr("a")],
        required=[{"out": "", "err": "no checks reported on the 'x' branch\n", "code": 1},
                  checks(("build", "pending"), ("lint", "pass")), checks(("build", "pass"), ("lint", "pass"))],
    )
    assert result.returncode == 0, result.stderr
    assert "pass     build" in result.stdout and "pr #7" in result.stdout and result.stdout.endswith("pass\n")


def test_failure_exits_one_with_json(tmp_path):
    result = run(tmp_path, "--json", view=[pr("a")], required=[checks(("build", "fail"), ("lint", "pass"))])
    assert result.returncode == 1
    data = json.loads(result.stdout)
    assert data["outcome"] == "fail" and [row["name"] for row in data["checks"]] == ["build", "lint"]


def test_follows_head_that_moves_mid_wait(tmp_path):
    result = run(
        tmp_path,
        view=[pr("a"), pr("a"), pr("b"), pr("b"), pr("b")],
        required=[checks(("build", "pending")), checks(("build", "pending")), checks(("build", "pass"))],
    )
    assert result.returncode == 0, result.stdout
    assert "head moved" in result.stderr and f"{'b' * 12}" in result.stdout


def test_timeout_exits_124(tmp_path):
    result = run(tmp_path, view=[pr("a")], required=[checks(("build", "pending"))], timeout="0.2")
    assert result.returncode == 124
    assert result.stdout.endswith("timeout\n")


def test_falls_back_to_all_checks_when_none_required(tmp_path):
    result = run(
        tmp_path,
        view=[pr("a")],
        required=[{"out": "", "err": "no required checks reported on the 'x' branch\n", "code": 1}],
        every=[checks(("build", "pass"), ("docs", "skipping"))],
    )
    assert result.returncode == 0, result.stderr


def test_unknown_pr_exits_two(tmp_path):
    result = run(tmp_path, view=[{"err": "no pull requests found\n", "code": 1}], required=[checks()])
    assert result.returncode == 2 and "no pull requests found" in result.stderr


def rules(*contexts):
    return [{"out": json.dumps([{"type": "required_status_checks", "parameters": {
        "required_status_checks": [{"context": c} for c in contexts]}}])}]


def calls(tmp_path, key):
    return int((tmp_path / f"{key}.n").read_text())


def test_waits_for_required_context_that_has_not_registered(tmp_path):
    result = run(
        tmp_path,
        view=[pr("a")],
        rules=rules("build", "lint"),
        required=[checks(("build", "pass")), checks(("build", "pass")), checks(("build", "pass"), ("lint", "pass"))],
    )
    assert result.returncode == 0, result.stdout
    assert "lint" in result.stdout
    assert calls(tmp_path, "required") >= 3


def test_without_known_contexts_a_late_registration_resets_the_settle_window(tmp_path):
    result = run(
        tmp_path,
        view=[pr("a")],
        required=[checks(("build", "pass")), checks(("build", "pass"), ("lint", "pending")),
                  checks(("build", "pass"), ("lint", "pass"))],
    )
    assert result.returncode == 0, result.stdout
    assert "lint" in result.stdout
    assert calls(tmp_path, "required") >= 4


def test_missing_required_context_times_out(tmp_path):
    result = run(tmp_path, rules=rules("build", "lint"), view=[pr("a")], required=[checks(("build", "pass"))],
                 timeout="0.2")
    assert result.returncode == 124
