"""CPython oracle for the receipt and reference-evaluation workflow ports."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import sys
import tempfile
import copy
from contextlib import contextmanager
from types import SimpleNamespace


ROOT = pathlib.Path(sys.argv[1]).resolve()
UNIT = sys.argv[2]
CASES = json.load(sys.stdin)


def load(relative: str, name: str):
    path = ROOT / relative
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import oracle source: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@contextmanager
def template_root(case):
    if "template" not in case:
        yield ROOT / "skills"
        return
    with tempfile.TemporaryDirectory() as directory:
        skills = pathlib.Path(directory) / "skills"
        template = skills / "evaluate" / "templates" / "EVALUATION.template.json"
        template.parent.mkdir(parents=True)
        template.write_text(json.dumps(case["template"], ensure_ascii=True))
        yield skills


def reference_builder():
    module = load("skills/deliver/scripts/reference_evaluation.py", "reference_evaluation_oracle")
    output = []
    for case in CASES:
        with template_root(case) as skills:
            output.append(module.make_reference_evaluation(
                case["run_id"],
                ROOT,
                repetitions=case["repetitions"],
                sample_size=case["sample_size"],
                evaluation_id=case["evaluation_id"],
                status=case["status"],
                time_offset_minutes=case["offset"],
                skills_root=skills,
            ))
    return output


def evaluation_materialise():
    module = load("skills/deliver/scripts/reference_evaluation.py", "reference_materialise_oracle")
    output = []
    for case in CASES:
        with template_root(case) as skills:
            evaluation = module.make_reference_evaluation(
                case["run_id"],
                ROOT,
                repetitions=case["repetitions"],
                sample_size=case["sample_size"],
                evaluation_id=case["evaluation_id"],
                status=case["status"],
                time_offset_minutes=case["offset"],
                skills_root=skills,
            )
            with tempfile.TemporaryDirectory() as directory:
                workspace = pathlib.Path(directory)
                module._materialise_evaluation(evaluation, workspace)
                files = {
                    path.relative_to(workspace).as_posix(): path.read_bytes().hex()
                    for path in sorted(workspace.rglob("*"))
                    if path.is_file()
                }
        output.append({"evaluation": evaluation, "files": files})
    return output


def bundle_builder():
    module = load("skills/deliver/scripts/reference_evaluation.py", "reference_bundle_oracle")
    output = []
    for case in CASES:
        run = case["run"]
        artifact = case["artifact"]
        with tempfile.TemporaryDirectory() as directory:
            workspace = pathlib.Path(directory)
            module._materialise_deterministic_evidence_bundle(run, artifact, workspace)
            payload = (workspace / artifact["path"]).read_bytes()
        output.append({"run": run, "artifact": artifact, "payload_hex": payload.hex()})
    return output


def reference_run_materialise():
    sys.path.insert(0, str(ROOT / "skills"))
    runs = load("skills/deliver/scripts/reference_runs.py", "reference_runs_oracle")
    module = load("skills/deliver/scripts/reference_evaluation.py", "reference_run_oracle")
    output = []
    for case in CASES:
        run = runs.make_reference_run(case["profile"], ROOT)
        before = copy.deepcopy(run)
        with tempfile.TemporaryDirectory() as directory:
            workspace = pathlib.Path(directory)
            module.materialise_reference_run(
                run,
                workspace,
                ROOT,
                evaluation_repetitions=case["repetitions"],
                evaluation_sample_size=case["sample_size"],
                skills_root=ROOT / "skills",
            )
            files = {
                path.relative_to(workspace).as_posix(): path.read_bytes().hex()
                for path in sorted(workspace.rglob("*"))
                if path.is_file()
            }
        output.append({"before": before, "run": run, "files": files})
    return output


def command_helpers():
    module = load("skills/deliver/scripts/delivery_receipt_commands.py", "delivery_receipt_commands_oracle")
    registry_path = ROOT / "config" / "delivery-profiles.json"
    output = []
    for case in CASES:
        value = {
            "gate": module.profile_judgement_gate(case["profile"], registry_path, ValueError),
            "corroborates": module.artifact_corroborates_measurement(
                case["artifact"], case["gate_name"], case["measured_value"]
            ),
        }
        output.append(value)
    return output


def command_profile_errors():
    module = load("skills/deliver/scripts/delivery_receipt_commands.py", "delivery_profile_error_oracle")
    output = []
    for case in CASES:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "profiles.json"
            path.write_text(json.dumps(case["registry"]))
            try:
                result = {"gate": module.profile_judgement_gate(case["profile"], path, ValueError)}
            except ValueError as error:
                result = {"error": str(error)}
        output.append(result)
    return output


def command_repair():
    module = load("skills/deliver/scripts/delivery_receipt_commands.py", "delivery_repair_oracle")
    output = []
    for case in CASES:
        run = copy.deepcopy(case["run"])
        repairs = []

        def mutate_run(_run_dir, apply):
            return apply(run, ROOT, ROOT)

        api = {
            "ReceiptError": ValueError,
            "require_identifier": lambda value, _field: value,
            "mutate_run": mutate_run,
            "append_jsonl": lambda _path, row: repairs.append(row),
            "utc_now": lambda: "2026-10-23T00:00:00Z",
        }
        args = SimpleNamespace(run_dir="run", reason="repair", failing=["check-a"])
        try:
            result = module.command_repair(args, api)
        except ValueError as error:
            output.append({"error": str(error)})
        else:
            output.append({
                "result": result,
                "generation": str(run["checkpoint"]["generation"]),
                "repair_cycles": str(run["repair_cycles"]),
                "repairs": repairs,
            })
    return output


def run_closed():
    sys.path.insert(0, str(ROOT / "skills"))
    module = load("skills/_shared/run_gates.py", "run_gates_receipt_oracle")
    return [module.run_closed(case) for case in CASES]


if UNIT == "reference_builder":
    RESULT = reference_builder()
elif UNIT == "evaluation_materialise":
    RESULT = evaluation_materialise()
elif UNIT == "bundle_builder":
    RESULT = bundle_builder()
elif UNIT == "reference_run_materialise":
    RESULT = reference_run_materialise()
elif UNIT == "command_helpers":
    RESULT = command_helpers()
elif UNIT == "command_profile_errors":
    RESULT = command_profile_errors()
elif UNIT == "command_repair":
    RESULT = command_repair()
elif UNIT == "run_closed":
    RESULT = run_closed()
else:
    raise SystemExit(f"unknown oracle unit: {UNIT}")

print(json.dumps(RESULT, ensure_ascii=True, separators=(",", ":")))
