"""Differential oracle for the authority mapper and shared style linter."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import re
import sys
import tempfile


root, unit = Path(sys.argv[1]), sys.argv[2]
if unit == "authority_mapping":
    source = root / "skills/deliver/scripts/authority_mapping.py"
elif unit == "style_lint":
    source = root / "skills/natural-writing/scripts/style_lint.py"
else:
    raise SystemExit(f"unknown oracle unit: {unit}")

spec = importlib.util.spec_from_file_location("port_oracle", source)
if spec is None or spec.loader is None:
    raise SystemExit(f"cannot load Python source: {source}")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
cases = json.load(sys.stdin)
results = []


def authority_outcome(operation, authority=None, raw=None, child=None, parent=None):
    try:
        if operation == "raw_map":
            result = module.map_delivery_authority(json.loads(raw))
        elif operation == "raw_delegations":
            result = module.map_delivery_delegations(json.loads(raw))
        elif operation == "map":
            result = module.map_delivery_authority(authority)
        elif operation == "delegations":
            result = module.map_delivery_delegations(authority)
        elif operation == "contained":
            result = module.authority_contained(child, parent)
        else:
            return {"error_type": "ValueError", "error": "unknown operation"}
        outcome = {"ok": result}
        if operation in {"raw_map", "raw_delegations"}:
            outcome["wire"] = json.dumps(result, ensure_ascii=False, separators=(",", ":"))
        return outcome
    except json.JSONDecodeError:
        return {"error_type": "JSONDecodeError"}
    except module.AuthorityMappingError as error:
        return {"error_type": "AuthorityMappingError", "error": str(error)}
    except TypeError as error:
        kind = "list" if "'list'" in str(error) else "dict" if "'dict'" in str(error) else None
        if kind is None:
            raise
        return {"error_type": "TypeError", "unhashable_type": kind}


def style_patterns(case):
    entries = case.get("patterns")
    if entries is None:
        return None
    return [
        (entry["label"], re.compile(entry["expression"], entry.get("flags", 0)))
        for entry in entries
    ]


def style_phrases(case):
    groups = case.get("phrases")
    if isinstance(groups, list):
        return {group["label"]: group["phrases"] for group in groups}
    return groups


def cli_result(case):
    with tempfile.TemporaryDirectory() as directory:
        base = Path(directory)
        for name, text in case.get("files", {}).items():
            path = base / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        for name, contents in case.get("binary_files", {}).items():
            path = base / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(bytes(contents))
        argv = [str(base / argument[7:]) if argument.startswith("<file>/") else
                str(base / argument[10:]) if argument.startswith("<missing>/") else argument
                for argument in case["argv"]]
        old_argv = sys.argv
        stdout = io.StringIO()
        stderr = io.StringIO()
        status = 0
        try:
            sys.argv = argv
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = module.run_cli(
                    case.get("description", ""),
                    phrases=style_phrases(case),
                    patterns=style_patterns(case),
                    latex=case.get("latex", False),
                )
        except SystemExit as error:
            status = error.code if isinstance(error.code, int) else 1
        except UnicodeDecodeError:
            return {"error_type": "UnicodeDecodeError"}
        finally:
            sys.argv = old_argv
        return {
            "exit_code": status,
            "stdout": stdout.getvalue().replace(directory, "<tmp>"),
            "stderr": stderr.getvalue().replace(directory, "<tmp>"),
        }


for case in cases:
    if unit == "authority_mapping":
        results.append(authority_outcome(
            case["op"],
            authority=case.get("authority"),
            raw=case.get("raw"),
            child=case.get("child"),
            parent=case.get("parent"),
        ))
        continue

    if case.get("op") == "cli":
        results.append(cli_result(case))
        continue
    operation = case.get("op", "scan")
    text = case["text"]
    suffix = case.get("suffix", ".md")
    latex = case.get("latex", False)
    if operation == "spans":
        results.append(module.ignored_spans(text, suffix, latex=latex))
    else:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ("fixture" + suffix)
            path.write_text(text, encoding="utf-8")
            if operation == "wordcount":
                results.append(module.prose_word_count(path, latex=latex))
            else:
                results.append(module.scan(
                    path,
                    phrases=style_phrases(case),
                    patterns=style_patterns(case),
                    latex=latex,
                ))

print(json.dumps(results, ensure_ascii=False, separators=(",", ":")))
