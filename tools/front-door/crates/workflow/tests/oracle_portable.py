"""Run unchanged Python units used by portable workflow differentials."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile


root, unit = Path(sys.argv[1]), sys.argv[2]
sys.path.insert(0, str(root))

SOURCES = {
    "portable_workflow": root / "skills/_shared/portable_workflow.py",
    "review_terminal": root / "skills/_shared/review_terminal.py",
    "idle_pause": root / "skills/autopilot/scripts/validate_idle_pause.py",
    "bootstrap": root / "skills/autopilot/scripts/bootstrap-autopilot.py",
}
source = SOURCES[unit]
if unit == "review_terminal":
    from skills._shared import review_terminal as module
else:
    spec = importlib.util.spec_from_file_location("port_oracle", source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

cases = json.load(sys.stdin)
results = []


def type_error(error):
    kind = "list" if "'list'" in str(error) else "dict" if "'dict'" in str(error) else None
    if kind is None:
        raise error
    return {"error_type": "TypeError", "unhashable_type": kind}


for case in cases:
    if unit == "portable_workflow":
        if isinstance(case, dict) and case.get("op") == "execute":
            sys.argv = [
                str(source),
                "--skill-root", case["skill"],
                "--context", case["context"],
                "--output", case["output"],
            ]
            try:
                module.main()
                results.append({"artifact": Path(case["output"]).read_text()})
            except (ValueError, KeyError) as error:
                results.append({"error": str(error)})
        else:
            try:
                module.validate_artifact_identity(case)
                results.append(None)
            except ValueError as error:
                results.append(str(error))
    elif unit == "review_terminal":
        try:
            results.append(module.normalise_dispatch_review(
                case["dispatch"], case["terminal"],
                review_verdict=case["verdict"],
                chair_family=case["chair"],
                transcript_available=case["transcript"],
                dispatcher_output_available=case["output"],
            ))
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "idle_pause":
        if case.get("op") == "invalid_utf8":
            with tempfile.TemporaryDirectory() as directory:
                state = Path(directory) / "STATE.md"
                queue = Path(directory) / "QUEUE.md"
                if case.get("where") == "state":
                    state.write_bytes(b"\xff")
                    queue.write_text("## Tier 0\n")
                else:
                    state.write_text(case["state"])
                    queue.write_bytes(b"\xff")
                try:
                    module.validate(state, queue)
                except UnicodeDecodeError as error:
                    results.append({"error_type": type(error).__name__})
                else:
                    results.append({"error_type": None})
        elif case.get("op") == "paths":
            state = Path(case["state"])
            queue = Path(case["queue"])
            if case.get("text") is not None:
                state.write_text(case["text"])
            if case.get("queue_text") is not None:
                queue.write_text(case["queue_text"])
            results.append(module.validate(state, queue))
        else:
            with tempfile.TemporaryDirectory() as directory:
                state = Path(directory) / "STATE.md"
                queue = Path(directory) / "QUEUE.md"
                state.write_text(case.get("text", ""))
                queue.write_text(case.get("queue", ""))
                if case.get("op") == "cli_args":
                    stdout, stderr = io.StringIO(), io.StringIO()
                    sys.argv = [str(SOURCES["idle_pause"]), *case["argv"]]
                    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                        try:
                            exit_code = module.main()
                        except SystemExit as error:
                            exit_code = error.code
                    results.append({"exit": exit_code, "stdout": stdout.getvalue(), "stderr": stderr.getvalue()})
                elif case.get("op") == "cli":
                    stdout, stderr = io.StringIO(), io.StringIO()
                    argv = [str(state), "--queue", str(queue)]
                    if case.get("quiet"):
                        argv.append("--quiet")
                    sys.argv = [str(SOURCES["idle_pause"]), *argv]
                    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                        exit_code = module.main()
                    results.append({"exit": exit_code, "stdout": stdout.getvalue(), "stderr": stderr.getvalue()})
                else:
                    results.append(module.validate(state, queue))
    elif unit == "bootstrap":
        if case["op"] == "args":
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                try:
                    values, rest = module.parse_args(case["args"])
                    result = {"values": values, "rest": rest}
                except SystemExit as error:
                    result = {
                        "exit": error.code,
                        "stdout": stdout.getvalue(),
                        "stderr": stderr.getvalue(),
                    }
            results.append(result)
        elif case["op"] == "fallback":
            results.append(module.fallback_content(case["name"], case["domain"]))
        elif case["op"] == "knobs":
            results.append(module.parse_knobs(case["goal"]))
        elif case["op"] == "usage":
            results.append(module.usage())
        elif case["op"] == "main":
            stdout, stderr = io.StringIO(), io.StringIO()
            previous_cwd = Path.cwd()
            repo_root = Path(case["repo_root"])
            datetime_module = module.dt
            datetime_type = datetime_module.datetime
            timezone_type = datetime_module.timezone
            random_bytes = module.os.urandom
            previous_path = os.environ.get("PATH")

            class FixedDateTime:
                timezone = timezone_type

                @classmethod
                def now(cls, tz=None):
                    return datetime_type(2026, 10, 23, 12, 19, tzinfo=tz)

            datetime_module.datetime = FixedDateTime
            module.os.urandom = lambda count: bytes.fromhex("a1b2c3")[:count]
            os.environ["PATH"] = ""
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                try:
                    os.chdir(repo_root)
                    exit_code = module.main(case["args"])
                except SystemExit as error:
                    exit_code = error.code
                finally:
                    os.chdir(previous_cwd)
                    datetime_module.datetime = datetime_type
                    module.os.urandom = random_bytes
                    if previous_path is None:
                        os.environ.pop("PATH", None)
                    else:
                        os.environ["PATH"] = previous_path
            missions = sorted((repo_root / ".agent-run/runs").glob("*-mission-*-??????"))
            mission = missions[0] if missions else None
            if mission is None:
                parsed, _ = module.parse_args(case["args"])
                slug = module.re.sub(r"[._]", "-", parsed["mission_id"].lower())[:32]
                mission = repo_root / ".agent-run/runs" / f"20261023-1219-mission-{slug}-a1b2c3"
            captured_stdout = stdout.getvalue()
            captured_stderr = stderr.getvalue()
            captured_stdout = captured_stdout.replace(str(mission), "<MISSION>")
            captured_stderr = captured_stderr.replace(str(mission), "<MISSION>")
            files = {}
            if mission is not None:
                for path in sorted(mission.rglob("*")):
                    if path.is_file():
                        files[str(path.relative_to(mission))] = {
                            "text": path.read_text(),
                            "mode": path.stat().st_mode & 0o777,
                        }
            results.append({
                "exit": exit_code,
                "stdout": captured_stdout,
                "stderr": captured_stderr,
                "files": files,
            })

json.dump(results, sys.stdout, ensure_ascii=False)
