"""Validate repeated held-out lifecycle-routing evaluation receipts."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
from typing import Any

import yaml


DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")


class Invalid(ValueError):
    pass


def fail(condition: bool, message: str) -> None:
    if condition:
        raise Invalid(message)


def canonical_packet(instruction: bytes, catalogue: bytes, cases: list[dict[str, Any]]) -> bytes:
    prompts = []
    for index, case in enumerate(cases, 1):
        prompt = case.get("prompt") if isinstance(case, dict) else None
        fail(not isinstance(prompt, str) or not prompt.strip(), f"dataset case {index} has no prompt")
        prompts.append(f"c{index:02d}: {prompt}")
    return instruction + b"\n## Skill catalogue\n\n" + catalogue + b"\n## Prompts\n\n" + ("\n".join(prompts) + "\n").encode()


def validate(receipt: Any, dataset_path: Path, *, evidence_root: Path | None = None) -> None:
    fail(not isinstance(receipt, dict), "receipt root must be an object")
    required = {"schema_version", "dataset_sha256", "harness_revision", "catalogue_sha256", "catalogue_artifact", "classifier_prompt_sha256", "classifier_prompt_artifact", "minimum_trials", "threshold", "invocations", "selections"}
    fail(set(receipt) != required, "receipt keys are invalid")
    fail(receipt["schema_version"] != 1, "unsupported receipt schema")
    expected_digest = "sha256:" + hashlib.sha256(dataset_path.read_bytes()).hexdigest()
    fail(receipt["dataset_sha256"] != expected_digest or not DIGEST.fullmatch(receipt["dataset_sha256"]), "dataset digest does not match")
    fail(not receipt["harness_revision"], "harness_revision is required")
    for field in ("catalogue_sha256", "classifier_prompt_sha256"):
        fail(not isinstance(receipt[field], str) or not DIGEST.fullmatch(receipt[field]), f"{field} is invalid")
    catalogue_names: set[str] = set()
    retained: dict[str, bytes] = {}
    if evidence_root is not None:
        for label, path_field, digest_field in (
            ("catalogue", "catalogue_artifact", "catalogue_sha256"),
            ("classifier prompt", "classifier_prompt_artifact", "classifier_prompt_sha256"),
        ):
            value = receipt[path_field]
            path = Path(value) if isinstance(value, str) else Path("..")
            fail(not isinstance(value, str) or path.is_absolute() or ".." in path.parts, f"{label} artifact is unsafe")
            target = evidence_root / path
            fail(not target.is_file(), f"{label} artifact is missing")
            retained[label] = target.read_bytes()
            fail("sha256:" + hashlib.sha256(retained[label]).hexdigest() != receipt[digest_field], f"{label} artifact digest does not match")
        for line in retained["catalogue"].decode("utf-8", errors="strict").splitlines():
            match = re.match(r"^- ([a-z0-9][a-z0-9-]*):", line)
            if match:
                catalogue_names.add(match.group(1))
        fail(not catalogue_names, "catalogue artifact contains no skill names")
    minimum = receipt["minimum_trials"]
    fail(isinstance(minimum, bool) or not isinstance(minimum, int) or minimum < 3, "minimum_trials must be at least three")
    try:
        dataset = yaml.safe_load(dataset_path.read_text())
    except (OSError, yaml.YAMLError) as exc:
        raise Invalid(f"dataset is unreadable: {exc}") from exc
    case_rows = dataset.get("cases", []) if isinstance(dataset, dict) else []
    fail(not isinstance(case_rows, list) or any(not isinstance(case, dict) or not case.get("id") for case in case_rows), "dataset cases are invalid")
    cases = {case["id"]: case for case in case_rows}
    fail(len(cases) != len(case_rows), "dataset case ids must be unique")
    fail(not cases, "dataset has no cases")
    expected_packet = canonical_packet(retained["classifier prompt"], retained["catalogue"], case_rows) if evidence_root is not None else b""
    invocations = receipt["invocations"]
    fail(not isinstance(invocations, list), "invocations must be a list")
    invocation_by_id: dict[str, dict[str, Any]] = {}
    invocation_trials: set[int] = set()
    output_by_invocation: dict[str, dict[str, Any]] = {}
    for index, invocation in enumerate(invocations):
        required_invocation = {"invocation_id", "trial", "adapter", "provider_family", "model", "input_packet_sha256", "input_packet_artifact", "output_artifact", "output_sha256", "parser_version"}
        fail(not isinstance(invocation, dict) or set(invocation) != required_invocation, f"invocation {index} keys are invalid")
        invocation_id = invocation["invocation_id"]
        trial_number = invocation["trial"]
        fail(not isinstance(invocation_id, str) or not invocation_id or invocation_id in invocation_by_id, f"invocation {index} identity is missing or duplicate")
        fail(isinstance(trial_number, bool) or not isinstance(trial_number, int) or not 1 <= trial_number <= minimum or trial_number in invocation_trials, f"invocation {index} trial is invalid or duplicate")
        fail(not invocation["adapter"] or not invocation["provider_family"] or not invocation["model"], f"invocation {index} lacks model lineage")
        for field in ("input_packet_sha256", "output_sha256"):
            fail(not isinstance(invocation[field], str) or not DIGEST.fullmatch(invocation[field]), f"invocation {index} {field} is invalid")
        fail(invocation["parser_version"] != "skill-name-exact-v1", f"invocation {index} parser version is unsupported")
        output_value = invocation["output_artifact"]
        output_path = Path(output_value) if isinstance(output_value, str) else Path("..")
        fail(not isinstance(output_value, str) or output_path.is_absolute() or ".." in output_path.parts, f"invocation {index} output artifact is unsafe")
        if evidence_root is not None:
            packet_value = invocation["input_packet_artifact"]
            packet_path = Path(packet_value) if isinstance(packet_value, str) else Path("..")
            fail(not isinstance(packet_value, str) or packet_path.is_absolute() or ".." in packet_path.parts, f"invocation {index} input packet artifact is unsafe")
            packet = evidence_root / packet_path
            fail(not packet.is_file(), f"invocation {index} input packet evidence is missing")
            packet_bytes = packet.read_bytes()
            fail(packet_bytes != expected_packet, f"invocation {index} input packet does not match retained components")
            fail("sha256:" + hashlib.sha256(packet_bytes).hexdigest() != invocation["input_packet_sha256"], f"invocation {index} input packet digest does not match")
            target = evidence_root / output_path
            fail(not target.is_file(), f"invocation {index} output evidence is missing")
            actual_sha = "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
            fail(actual_sha != invocation["output_sha256"], f"invocation {index} output evidence digest does not match")
            try:
                output = json.loads(target.read_text())
            except (OSError, json.JSONDecodeError) as exc:
                raise Invalid(f"invocation {index} output evidence is invalid: {exc}") from exc
            fail(not isinstance(output, dict) or set(output) != {"trial", "selections"} or output["trial"] != trial_number or not isinstance(output["selections"], list), f"invocation {index} output evidence shape is invalid")
            output_by_invocation[invocation_id] = output
        invocation_by_id[invocation_id] = invocation
        invocation_trials.add(trial_number)
    fail(invocation_trials != set(range(1, minimum + 1)), "invocation trials do not cover the declared repetitions")

    trials = receipt["selections"]
    fail(not isinstance(trials, list), "selections must be a list")
    seen: set[tuple[str, int]] = set()
    counts = {case_id: 0 for case_id in cases}
    passing = 0
    for index, trial in enumerate(trials):
        fail(not isinstance(trial, dict), f"trial {index} must be an object")
        required_trial = {"case_id", "trial", "invocation_id", "selected_skill", "status", "reason_code"}
        fail(set(trial) != required_trial, f"trial {index} keys are invalid")
        case = cases.get(trial["case_id"])
        fail(case is None, f"trial {index} uses an unknown case")
        key = (trial["case_id"], trial["trial"])
        fail(key in seen, f"duplicate trial identity at {index}")
        seen.add(key)
        invocation_id = trial["invocation_id"]
        invocation = invocation_by_id.get(invocation_id)
        fail(invocation is None or invocation["trial"] != trial["trial"], f"trial {index} invocation identity is missing or mismatched")
        fail(isinstance(trial["trial"], bool) or not isinstance(trial["trial"], int) or trial["trial"] < 1, f"trial {index} number is invalid")
        expected_pass = trial["selected_skill"] == case["expected_skill"]
        if evidence_root is not None:
            fail(trial["selected_skill"] not in catalogue_names, f"trial {index} selected a skill absent from the retained catalogue")
        fail(trial["status"] not in {"pass", "fail"}, f"trial {index} status is invalid")
        fail((trial["status"] == "pass") != expected_pass, f"trial {index} status disagrees with selected route")
        fail(trial["reason_code"] not in {"expected-route", "wrong-skill", "no-route", "ambiguous"}, f"trial {index} reason code is invalid")
        if evidence_root is not None:
            output_rows = output_by_invocation[invocation_id]["selections"]
            matches = [row for row in output_rows if isinstance(row, dict) and row.get("case_id") == trial["case_id"]]
            fail(len(matches) != 1 or matches[0] != {"case_id": trial["case_id"], "selected_skill": trial["selected_skill"]}, f"trial {index} output evidence does not match parsed selection")
        counts[trial["case_id"]] += 1
        passing += int(expected_pass)
    for case_id, count in counts.items():
        fail(count != minimum, f"case {case_id} trial count {count} does not equal {minimum}")
    threshold = receipt["threshold"]
    fail(not isinstance(threshold, dict) or set(threshold) != {"numerator", "denominator", "minimum_rate"}, "threshold keys are invalid")
    fail(threshold["numerator"] != passing, "threshold numerator does not match passing trials")
    fail(threshold["denominator"] != len(trials), "threshold denominator does not match trials")
    rate = passing / len(trials) if trials else 0
    fail(not isinstance(threshold["minimum_rate"], (int, float)) or isinstance(threshold["minimum_rate"], bool) or not 0 <= threshold["minimum_rate"] <= 1, "minimum_rate is invalid")
    fail(rate < threshold["minimum_rate"], f"routing rate {passing}/{len(trials)} is below threshold")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--dataset", type=Path, default=Path(__file__).resolve().parents[1] / "evals" / "lifecycle-routing.yaml")
    args = parser.parse_args(argv)
    try:
        validate(json.loads(args.receipt.read_text()), args.dataset, evidence_root=args.receipt.parent.resolve())
    except (OSError, json.JSONDecodeError, Invalid) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print("PASS: repeated lifecycle routing evaluation")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
