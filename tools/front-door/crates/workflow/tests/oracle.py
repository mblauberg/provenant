"""Run unchanged Python units on explicit JSON fixtures for Rust differentials."""
import importlib.util
import json
from pathlib import Path
import sys

root, unit = Path(sys.argv[1]), sys.argv[2]
source = root / ("skills/autopilot/scripts/bootstrap-autopilot.py" if unit == "bootstrap" else f"skills/_shared/{unit}.py")
spec = importlib.util.spec_from_file_location("oracle", source)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
cases = json.load(sys.stdin)
results = []

def type_error(error):
    # CPython 3.14 changed built-in TypeError prose. Compare the exception type
    # and the exact offending JSON container kind across interpreter versions.
    kind = "list" if "'list'" in str(error) else "dict" if "'dict'" in str(error) else None
    if kind is None:
        raise error
    return {"error_type": "TypeError", "unhashable_type": kind}

for case in cases:
    if unit == "custody":
        try:
            relative, target = module.contained_regular_path(Path(case["root"]), case["path"], "file")
            results.append({"relative": relative, "target": str(target)})
        except module.OwnedFileError as error:
            results.append({"error": str(error), "hardlink": isinstance(error, module.OwnedLinkError)})
    elif unit == "review_ladder":
        try:
            results.append(module.check_review_ladder(case["risk"], case["legs"], chair_family=case.get("chair")))
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "review_terminal":
        try:
            results.append(module.normalise_dispatch_review(case["dispatch"], case["terminal"], review_verdict=case["verdict"], chair_family=case["chair"], transcript_available=case["transcript"], dispatcher_output_available=case["output"]))
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "run_gates":
        try:
            results.append([module.gate_approved(case, "human_gates", "release"), module.release_approved(case), module.run_closed(case)])
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "portable_workflow":
        if isinstance(case, dict) and case.get("op") == "execute":
            sys.argv = [str(source), "--skill-root", case["skill"], "--context", case["context"], "--output", case["output"]]
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
    elif unit == "bootstrap":
        if case["op"] == "args":
            import contextlib, io
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                try:
                    values, rest = module.parse_args(case["args"])
                    result = {"values": values, "rest": rest}
                except SystemExit as error:
                    result = {"exit": error.code, "stdout": out.getvalue(), "stderr": err.getvalue()}
            results.append(result)
        elif case["op"] == "fallback":
            results.append(module.fallback_content(case["name"], case["domain"]))
        elif case["op"] == "knobs":
            results.append(module.parse_knobs(case["goal"]))
        elif case["op"] == "usage":
            results.append(module.usage())
json.dump(results, sys.stdout)
