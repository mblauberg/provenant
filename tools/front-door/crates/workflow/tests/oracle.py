"""Run unchanged Python units on explicit JSON fixtures for Rust differentials."""
import importlib.util
import json
from pathlib import Path
import sys

root, unit = Path(sys.argv[1]), sys.argv[2]
source = root / f"skills/_shared/{unit}.py"
if unit in {"git_evidence_packet", "git_evidence_header"}:
    source = root / "skills/orchestrate/scripts/git_evidence_packet.py"
if unit in {"preflight_contract", "preflight_wire", "preflight_decode"}:
    source = root / "skills/orchestrate/scripts/dispatch_preflight.py"
if unit in {"lane_exit_contract", "lane_exit_raw"}:
    source = root / "skills/orchestrate/scripts/lane_exit.py"
if unit in {"lane_launch_contract", "lane_duration", "lane_batch"}:
    source = root / "skills/orchestrate/scripts/lane_launch.py"
if unit in {"observer_contract", "observer_contract_wire"}:
    source = root / "skills/orchestrate/scripts/provider_observer.py"
if unit == "authority_mapping":
    source = root / "skills/deliver/scripts/authority_mapping.py"
if unit == "writer_branch":
    source = root / "skills/orchestrate/scripts/writer_lease.py"
if unit == "style_lint":
    source = root / "skills/natural-writing/scripts/style_lint.py"
spec = importlib.util.spec_from_file_location("oracle", source)
module = importlib.util.module_from_spec(spec)
if unit != "review_panel":
    spec.loader.exec_module(module)
if unit == "review_panel":
    sys.path.insert(0, str(root))
    from skills._shared import review_panel as module
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
    if unit == "style_lint":
        import tempfile
        operation = case.get("op", "scan")
        if operation == "spans":
            results.append(module.ignored_spans(case["text"], case["suffix"], latex=case.get("latex", False)))
        else:
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / ("fixture" + case["suffix"])
                path.write_text(case["text"], encoding="utf-8")
                if operation == "wordcount":
                    results.append(module.prose_word_count(path, latex=case.get("latex", False)))
                else:
                    results.append(module.scan(path, latex=case.get("latex", False)))
    elif unit == "writer_branch":
        results.append(None if len(case) <= 48 and module.BRANCH_PATTERN.fullmatch(case) else f"branch {case} is outside <type>/<area>-<slug> (or land/, proto/); rename for repository convention")
    elif unit == "authority_mapping":
        try:
            if case["op"] == "map":
                result = module.map_delivery_authority(case["authority"])
            elif case["op"] == "delegations":
                result = module.map_delivery_delegations(case["authority"])
            elif case["op"] == "contained":
                result = module.authority_contained(case["child"], case["parent"])
            results.append({"ok": result})
        except module.AuthorityMappingError as error:
            results.append({"error_type":"AuthorityMappingError", "error":str(error)})
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "review_panel":
        try:
            op = case["op"]
            if op == "raw_shortfall":
                spec = json.loads(case["raw"])
                results.append({"errors": module.validate_panel(spec, []), "wire": json.dumps(module.panel_shortfall(spec, []), separators=(",", ":"))})
            elif op == "resolve":
                results.append(module.resolve_panel(case["preset"], case.get("overrides"), **case.get("axis_overrides", {})))
            elif op == "validate":
                results.append(module.validate_panel(case["spec"], case["members"]))
            elif op == "result":
                results.append(module.panel_result(case["spec"], case["members"], case["output"]))
            elif op == "validate_result":
                results.append(module.validate_panel_result(case["spec"], case["members"], case["result"]))
            elif op == "reduce":
                results.append(module.REDUCERS[case["name"]](case["output"]))
        except ValueError as error:
            results.append({"error": str(error)})
    elif unit in {"observer_contract", "observer_contract_wire"}:
        observer = module.LiveObserver(case["plan"])
        for line in case["lines"]:
            observer.feed_stdout((json.dumps(line) + "\n").encode())
        for line in case.get("raw_lines", []):
            observer.feed_stdout((line + "\n").encode())
        for chunk in case.get("diagnostics", []):
            observer.feed_stderr(chunk["text"].encode(), chunk["stream"])
        summary = observer.result()
        results.append({"wire": json.dumps(summary, ensure_ascii=False, separators=(",", ":")), "failure_keys": list(summary["semantic_failures"])} if unit == "observer_contract_wire" else summary)
    elif unit == "lane_launch_contract":
        directory = Path(case["root"])
        plan = {"adapter":"claude","model":"m","cwd":str(directory),"run_dir":str(directory),"mode":case["mode"],"timeout_seconds":1.125,"startup_seconds":0.025,"grace_seconds":0.050,"silence_seconds":0.125,"_lineage_token":"token","batch_id":"batch-001","attempt_identity":{"run_dir":str(directory / "run")}}
        built = module.build_spec(plan, {"PATH":"/bin","SECRET_REFERENCE":"synthetic-value"}, argv=["/bin/claude","-p"], stdin_path=directory / "prompt.md" if case["stdin"] else None, events_path=directory / "events.jsonl", stderr_path=directory / "stderr.log", max_bytes=2048)
        results.append(json.dumps(built, ensure_ascii=False, separators=(",", ":")))
    elif unit == "lane_duration":
        try:
            results.append({"wire": str(module.milliseconds(json.loads(case["raw"])))})
        except ValueError as error:
            results.append({"error": str(error)})
    elif unit == "lane_batch":
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            plan = {"adapter":"claude","model":"m","cwd":directory,"run_dir":directory,"mode":"read_only","timeout_seconds":1,"grace_seconds":1,"batch_id":case["batch_id"]}
            try:
                module.build_spec(plan, {}, argv=["fixture"], stdin_path=None, events_path=path/"e", stderr_path=path/"s", max_bytes=1)
                results.append(None)
            except ValueError as error:
                results.append(str(error))
    elif unit == "lane_exit_raw":
        try:
            results.append({"wire": json.dumps(module.receipt_fields(json.loads(case["raw"])), ensure_ascii=False, separators=(",", ":"))})
        except ValueError as error:
            results.append({"error": str(error)})
    elif unit == "lane_exit_contract":
        results.append(json.dumps(module.receipt_fields(case), ensure_ascii=False, separators=(",", ":")))
    elif unit == "preflight_decode":
        try:
            results.append({"wire": json.dumps(module.parse_fast_route_json(case["raw"]), ensure_ascii=False, separators=(",", ":"))})
        except ValueError as error:
            results.append({"error": str(error)})
    elif unit == "preflight_wire":
        results.append(json.dumps(module.parse_fast_route_json(case["raw"]), ensure_ascii=False, separators=(",", ":")))
    elif unit == "preflight_contract":
        results.append(module.parse_fast_route_json(json.dumps(case)))
    elif unit == "git_evidence_packet":
        scanner = module.PacketScanner(ValueError, case["limit"], validate_on_feed=case["validate"])
        try:
            for chunk in case["chunks"]:
                scanner.feed(bytes(chunk))
            results.append({"metadata": scanner.finish(), "status": scanner.has_status, "diff": scanner.has_diff})
        except ValueError as error:
            results.append({"error": str(error)})
    elif unit == "git_evidence_header":
        try:
            results.append(module.validate_header(case))
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "custody":
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
json.dump(results, sys.stdout)
