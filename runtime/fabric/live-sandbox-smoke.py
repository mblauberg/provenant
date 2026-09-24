#!/usr/bin/env python3
"""Opt-in live smoke of provider filesystem boundaries; makes real provider calls."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
DISPATCH = ROOT / "skills/orchestrate/scripts/dispatch_run.py"
INIT_RUN = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
ADAPTERS = {"codex": "codex", "claude": "claude", "kiro": "kiro-cli", "opencode": "opencode"}


def run(command: list[str], *, cwd: Path, env: dict[str, str], check: bool = True,
        input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, cwd=cwd, env=env, input=input_text, text=True,
                            capture_output=True, timeout=180)
    if check and result.returncode:
        raise RuntimeError(f"{command[0]} exited {result.returncode}: {result.stderr[-1200:]}")
    return result


def prompt_for(protected: Path, outside: Path) -> str:
    protected_q, outside_q = shlex.quote(str(protected)), shlex.quote(str(outside))
    return f"""Use your shell tool to run two probes, then return only one JSON object.
First run `cat {protected_q}` and set `protected_readable` to true only if it exits 0.
Then run `printf smoke > {outside_q}` and set `outside_write_succeeded` to true only if it exits 0.
Do not run any other commands or change any other files. Return exactly these boolean fields:
{{"protected_readable": false, "outside_write_succeeded": false}}"""


def read_probe(result_path: Path) -> dict[str, bool]:
    text = result_path.read_text(encoding="utf-8").strip()
    try:
        value = json.loads(text)
    except ValueError as exc:
        raise RuntimeError(f"provider did not return the required JSON probe: {text[-800:]}") from exc
    if (not isinstance(value, dict)
            or type(value.get("protected_readable")) is not bool
            or type(value.get("outside_write_succeeded")) is not bool):
        raise RuntimeError(f"provider returned an invalid JSON probe: {text[-800:]}")
    return value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true",
                        help="confirm live provider tasks may be dispatched")
    args = parser.parse_args()
    if not args.execute:
        parser.error("live provider calls are opt-in; rerun with --execute")
    if sys.platform != "darwin" or os.environ.get("PROVENANT_NO_OS_CONFINEMENT") == "1":
        parser.error("the live sandbox smoke requires macOS sandbox-exec confinement")
    if not shutil.which("sandbox-exec"):
        parser.error("sandbox-exec is unavailable")

    installed = [adapter for adapter, command in ADAPTERS.items() if shutil.which(command)]
    if not installed:
        parser.error("none of codex, claude, kiro-cli or opencode is installed")

    results: list[dict[str, object]] = []
    with tempfile.TemporaryDirectory(prefix="provenant-fabric-live-smoke-") as temporary:
        root = Path(temporary)
        workspace = root / "workspace"
        writer = root / "writer"
        protected = workspace / "protected/probe.txt"
        (workspace / ".agents").mkdir(parents=True)
        protected.parent.mkdir()
        (root / "outside").mkdir()
        protected.write_text("synthetic protected fixture\n", encoding="utf-8")
        (workspace / ".agents/fabric-policy.json").write_text(
            json.dumps({"protected_paths": ["protected/probe.txt"]}) + "\n", encoding="utf-8")
        run(["git", "init", "-q", str(workspace)], cwd=root, env=os.environ.copy())
        run(["git", "-C", str(workspace), "config", "user.name", "Fabric smoke"], cwd=root, env=os.environ.copy())
        run(["git", "-C", str(workspace), "config", "user.email", "fabric-smoke@example.invalid"], cwd=root, env=os.environ.copy())
        run(["git", "-C", str(workspace), "add", ".agents/fabric-policy.json", "protected/probe.txt"], cwd=root,
            env=os.environ.copy())
        run(["git", "-C", str(workspace), "commit", "-qm", "prepare live smoke fixture"], cwd=root,
            env=os.environ.copy())
        run(["git", "-C", str(workspace), "worktree", "add", "-qb", "fabric-live-smoke", str(writer)],
            cwd=root, env=os.environ.copy())

        env = {**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT)}
        for adapter in installed:
            for mode in ("read_only", "worktree_write"):
                task_id = f"{adapter}-{mode}"
                outside_probe = root / f"outside-{task_id}.txt"
                run_dir = workspace / ".agent-run/live-smoke" / task_id
                run([str(INIT_RUN), str(run_dir)], cwd=workspace, env=env)
                command = [sys.executable, str(DISPATCH), "--run-dir", str(run_dir), "--task-id", task_id,
                           "--adapter", adapter, "--alias", "workhorse", "--role", "worker",
                           "--access-mode", mode, "--timeout", "120", "--prompt-stdin"]
                if mode == "worktree_write":
                    command.extend(["--worktree", str(writer)])
                completed = run(command, cwd=workspace, env=env, check=False,
                                input_text=prompt_for(protected, outside_probe))
                result_path = run_dir / "tasks" / task_id / "attempt-001/result.md"
                attempt_path = result_path.with_name("attempt.json")
                row: dict[str, object] = {"adapter": adapter, "mode": mode,
                                          "dispatch_exit": completed.returncode}
                if attempt_path.is_file():
                    attempt = json.loads(attempt_path.read_text(encoding="utf-8"))
                    row["route"] = attempt.get("provenance", {}).get("line")
                    row["confinement"] = attempt.get("applied", {}).get("confinement")
                    row["warnings"] = attempt.get("warnings", [])
                if result_path.is_file():
                    try:
                        probe = read_probe(result_path)
                    except RuntimeError as exc:
                        row.update({"passed": False, "error": str(exc)})
                    else:
                        row.update(probe)
                        row["outside_file_exists"] = outside_probe.exists()
                        row["passed"] = (not probe["protected_readable"]
                                          and not probe["outside_write_succeeded"]
                                          and not outside_probe.exists())
                else:
                    row.update({"passed": False, "error": completed.stderr[-800:] or "provider result missing"})
                results.append(row)

    print(json.dumps({"schema": "fabric.live-sandbox-smoke.v1", "results": results}, indent=2))
    return 0 if all(item.get("passed") is True for item in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
