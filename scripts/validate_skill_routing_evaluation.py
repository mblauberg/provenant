#!/usr/bin/env python3
"""Validate candidate, comparison, and failed skill-routing evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
from typing import Any, Callable

import yaml


DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
FROZEN_CURRENT_PROTOCOL_DIGEST = "sha256:94fd8c01aa8a30e3387c4100d84011868798f941dd229653fe0de0f56b9b75fe"
FROZEN_CURRENT_SOURCE_DIGEST = "sha256:a98ce4d24e783869cfcd787d4c8d89b7591bd2602742f1e8caad84ab3363e5db"
FROZEN_CURRENT_DEPENDENCIES = ["https://github.com/mblauberg/provenant/issues/330"]
ARM_ROLES = {"candidate", "without-skill", "previous-package"}
REQUIRED_ATTEMPT_IDS = [
    "schema-type-rejection", "schema-unique-items-rejection",
    "semantic-f0f34f4", "semantic-70f2a05",
]
REQUIRED_INVALID_ATTEMPTS = {
    "schema-type-rejection": {
        "candidate_commit": "f0f34f4007f1c6a7a183b5a57ffff5ef9bf6c60c",
        "candidate_tree": "9b0dddd5a535ae2529c41623569790d98839b4aa",
        "model": "gpt-5.6-luna",
        "reason": "response schema omitted an explicit type for schema_version",
    },
    "schema-unique-items-rejection": {
        "candidate_commit": "f0f34f4007f1c6a7a183b5a57ffff5ef9bf6c60c",
        "candidate_tree": "9b0dddd5a535ae2529c41623569790d98839b4aa",
        "model": "gpt-5.6-luna",
        "reason": "response schema used unsupported uniqueItems",
    },
}


class Invalid(ValueError):
    pass


def fail(condition: bool, message: str) -> None:
    if condition:
        raise Invalid(message)


def digest(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def git(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=root, check=False, capture_output=True, text=True,
    )
    if result.returncode:
        raise Invalid(result.stderr.strip() or "git evidence lookup failed")
    return result.stdout


def artifact(root: Path, value: Any, label: str) -> bytes:
    fail(
        not isinstance(value, dict) or set(value) != {"path", "sha256"},
        f"{label} artifact declaration is invalid",
    )
    path = PurePosixPath(value["path"]) if isinstance(value["path"], str) else PurePosixPath("..")
    fail(path.is_absolute() or ".." in path.parts, f"{label} artifact path is unsafe")
    target = root / Path(*path.parts)
    fail(not target.is_file(), f"{label} artifact is missing")
    fail(not target.resolve().is_relative_to(root.resolve()), f"{label} artifact path is unsafe")
    content = target.read_bytes()
    fail(
        not DIGEST.fullmatch(str(value["sha256"])) or digest(content) != value["sha256"],
        f"{label} artifact digest does not match",
    )
    return content


def skill_rows_at_commit(root: Path, commit: str) -> list[tuple[str, str]]:
    paths = [
        line for line in git(root, "ls-tree", "-r", "--name-only", commit, "skills").splitlines()
        if re.fullmatch(r"skills/[a-z0-9][a-z0-9-]*/SKILL\.md", line)
    ]
    rows: list[tuple[str, str]] = []
    for path in sorted(paths):
        text = git(root, "show", f"{commit}:{path}")
        match = re.match(r"^---\n(.*?)\n---\n", text, re.DOTALL)
        fail(match is None, f"candidate skill frontmatter is invalid: {path}")
        frontmatter = yaml.safe_load(match.group(1))
        fail(not isinstance(frontmatter, dict), f"candidate skill frontmatter is invalid: {path}")
        rows.append((frontmatter["name"], frontmatter["description"]))
    fail(not rows, "candidate catalogue is empty")
    return rows


def catalogue_at_commit(root: Path, commit: str) -> bytes:
    return ("\n".join(f"- {name}: {description}" for name, description in skill_rows_at_commit(root, commit)) + "\n").encode()


def names_at_commit(root: Path, commit: str) -> bytes:
    return ("\n".join(f"- {name}" for name, _ in skill_rows_at_commit(root, commit)) + "\n").encode()


def packet(instruction: bytes, catalogue: bytes, cases: list[dict[str, Any]]) -> bytes:
    prompts = [{"case_id": row["id"], "prompt": row["prompt"]} for row in cases]
    return (
        instruction.rstrip()
        + b"\n\n## Skill catalogue\n\n"
        + catalogue.rstrip()
        + b"\n\n## Cases\n\n"
        + json.dumps(prompts, sort_keys=True, separators=(",", ":")).encode()
        + b"\n"
    )


def validate_candidate_cases(
    dataset: list[dict[str, Any]], read_at_commit: Callable[[str], str],
) -> None:
    loaded: dict[str, dict[str, Any]] = {}
    seen: set[str] = set()
    for row in dataset:
        fail(
            not isinstance(row, dict)
            or set(row) != {"id", "source_path", "prompt", "expected"},
            "dataset case keys are invalid",
        )
        fail(row["id"] in seen, "dataset case id is duplicate")
        seen.add(row["id"])
        path = row["source_path"]
        if path not in loaded:
            value = yaml.safe_load(read_at_commit(path))
            cases = value.get("cases") if isinstance(value, dict) else None
            fail(not isinstance(cases, list), f"candidate fixture is invalid: {path}")
            loaded[path] = {case.get("id"): case for case in cases if isinstance(case, dict)}
        canonical = loaded[path].get(row["id"])
        fail(
            canonical is None
            or canonical.get("prompt") != row["prompt"]
            or canonical.get("expected") != row["expected"],
            f"dataset case {row['id']} does not match candidate commit",
        )


def score_trial(
    expected: dict[str, dict[str, Any]], selections: list[dict[str, Any]],
) -> tuple[int, int]:
    fail(not isinstance(selections, list), "trial selections must be a list")
    actual: dict[str, dict[str, Any]] = {}
    for row in selections:
        fail(
            not isinstance(row, dict)
            or set(row) != {"case_id", "primary_skill", "companion_skills"},
            "selection keys are invalid",
        )
        case_id = row["case_id"]
        fail(case_id in actual, "duplicate selection case")
        actual[case_id] = {
            "primary_skill": row["primary_skill"],
            "companion_skills": row["companion_skills"],
        }
    fail(set(actual) != set(expected), "trial case coverage is incomplete or extra")
    return sum(actual[case_id] == route for case_id, route in expected.items()), len(expected)


def validate_invocation(
    invocation: Any, expected: dict[str, dict[str, Any]], evidence_root: Path, label: str,
) -> tuple[int, int]:
    fields = {"trial", "provider_family", "adapter", "model", "reasoning_effort", "output"}
    fail(not isinstance(invocation, dict) or set(invocation) != fields, f"{label} invocation is invalid")
    for field in ("provider_family", "adapter", "model", "reasoning_effort"):
        fail(not isinstance(invocation[field], str) or not invocation[field], f"{label} {field} is missing")
    output = json.loads(artifact(evidence_root, invocation["output"], f"{label} output"))
    fail(
        not isinstance(output, dict)
        or set(output) != {"schema_version", "selections"}
        or output["schema_version"] != 1,
        f"{label} output is invalid",
    )
    return score_trial(expected, output["selections"])


def validate_attempts(
    content: bytes, evidence_root: Path, required_ids: list[str], repository_root: Path,
) -> None:
    value = json.loads(content)
    fail(
        not isinstance(value, dict) or set(value) != {"schema_version", "attempts"}
        or value["schema_version"] != 2 or not isinstance(value["attempts"], list),
        "attempts manifest is invalid",
    )
    attempts = value["attempts"]
    fail([row.get("id") for row in attempts if isinstance(row, dict)] != required_ids, "attempt set or order does not match receipt")
    for row in attempts:
        common = {"id", "candidate_commit", "candidate_tree", "status", "model"}
        fail(not isinstance(row, dict) or not common <= set(row), "attempt row is invalid")
        fail(not isinstance(row["model"], str) or not row["model"].strip(), "attempt model lineage is missing")
        commit = row["candidate_commit"]
        fail(not isinstance(commit, str) or not COMMIT.fullmatch(commit), "attempt candidate commit is invalid")
        tree = git(repository_root, "rev-parse", f"{commit}^{{tree}}").strip()
        fail(row["candidate_tree"] != tree, "attempt candidate tree does not match")
        if row["status"] == "invalid-pre-inference":
            fail(
                set(row) != common | {"reason", "raw_available"}
                or not isinstance(row["reason"], str) or not row["reason"]
                or row["raw_available"] is not False,
                "invalid pre-inference attempt is not honestly declared",
            )
            expected_invalid = REQUIRED_INVALID_ATTEMPTS.get(row["id"])
            fail(
                expected_invalid is None
                or {field: row[field] for field in expected_invalid} != expected_invalid,
                "invalid pre-inference attempt does not match retained history",
            )
            continue
        fail(row["status"] != "fail", "semantic attempt status must be fail")
        required = common | {"dataset", "catalogue", "classifier", "packet", "invocations", "score"}
        fail(set(row) != required, "semantic attempt keys are invalid")
        dataset_bytes = artifact(evidence_root, row["dataset"], f"attempt {row['id']} dataset")
        catalogue = artifact(evidence_root, row["catalogue"], f"attempt {row['id']} catalogue")
        instruction = artifact(evidence_root, row["classifier"], f"attempt {row['id']} classifier")
        retained_packet = artifact(evidence_root, row["packet"], f"attempt {row['id']} packet")
        data = yaml.safe_load(dataset_bytes)
        cases = data.get("cases") if isinstance(data, dict) and data.get("schema_version") == 1 else None
        fail(not isinstance(cases, list) or not cases, "semantic attempt dataset is invalid")
        validate_candidate_cases(cases, lambda path: git(repository_root, "show", f"{commit}:{path}"))
        fail(catalogue != catalogue_at_commit(repository_root, commit), "semantic attempt catalogue does not match candidate")
        fail(retained_packet != packet(instruction, catalogue, cases), "semantic attempt packet does not match inputs")
        expected = {case["id"]: case["expected"] for case in cases}
        passed = total = 0
        invocations = row["invocations"]
        fail(not isinstance(invocations, list) or not invocations, "semantic attempt invocations are missing")
        for index, invocation in enumerate(invocations, start=1):
            fail(invocation.get("trial") != index, "semantic attempt trials are not contiguous")
            scored, count = validate_invocation(invocation, expected, evidence_root, f"attempt {row['id']} trial {index}")
            passed += scored
            total += count
        fail(row["score"] != {"numerator": passed, "denominator": total}, "semantic attempt score is incorrect")
        fail(passed == total, "semantic attempt labelled fail has a passing score")


def validate_current_fixture(root: Path) -> None:
    """Validate the current fixture without turning it into a generic schema."""
    def load_json(path: Path, label: str) -> Any:
        try:
            return json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as exc:
            raise Invalid(f"{label} is invalid") from exc

    def exact(value: Any, expected: Any, message: str) -> None:
        fail(value != expected, message)

    plan_path = root / "routing-protocol.json"
    result_path = root / "routing-result.json"
    plan = load_json(plan_path, "current protocol")
    fail(not isinstance(plan, dict), "current protocol is invalid")
    dataset = plan.get("dataset")
    schedule = plan.get("schedule")
    execution = plan.get("execution")
    fail(not isinstance(dataset, dict) or not isinstance(schedule, dict), "current protocol is invalid")
    fail(not isinstance(dataset.get("path"), str), "current protocol is invalid")
    holdout_path = root / dataset["path"]
    frozen_plan = {key: value for key, value in plan.items() if key != "execution"}
    exact(digest(json.dumps(frozen_plan, sort_keys=True, separators=(",", ":")).encode()),
          FROZEN_CURRENT_PROTOCOL_DIGEST, "current protocol digest is not frozen")
    exact(digest(holdout_path.read_bytes()), FROZEN_CURRENT_SOURCE_DIGEST,
          "current source digest is not frozen")
    fail(not isinstance(execution, dict), "current execution block is invalid")
    status = execution.get("status")
    fail(status not in {"planned-unexecuted", "completed"}, "current execution status is invalid")

    summary_doc = load_json(root / "summary.json", "current summary")
    summary = summary_doc.get("current_routing_regression") if isinstance(summary_doc, dict) else None
    fail(not isinstance(summary, dict), "current summary protocol is invalid")
    if status == "planned-unexecuted":
        exact(execution, {
            "attempts_started": 0,
            "blocked_reason": "FABRIC-ROUNDTRIP-UNAVAILABLE",
            "dependencies": FROZEN_CURRENT_DEPENDENCIES,
            "status": status,
        }, "planned current dependencies are invalid")
        fail(summary.get("dependencies") != FROZEN_CURRENT_DEPENDENCIES,
             "planned current dependencies are invalid")
        fail(summary.get("cases") != dataset.get("cases"), "summary cases are invalid")
        fail(summary.get("catalogue_owner_count") != plan["catalogue"]["owner_count"],
             "summary owner count is invalid")
        fail(summary.get("attempts_started") != 0
             or summary.get("planned_attempts") != schedule.get("attempts_planned")
             or summary.get("planned_case_rows") != schedule.get("case_rows_planned")
             or summary.get("blocked_reason") != execution["blocked_reason"]
             or summary.get("evaluation_id") != plan["evaluation_id"]
             or summary.get("status") != "outstanding",
             "planned current summary is invalid")
        fail(result_path.exists(), "planned current evaluation must not have a result")
        return

    exact(set(execution), {"attempts_started", "status"}, "completed execution shape is invalid")
    attempts_planned = schedule.get("attempts_planned")
    case_rows_planned = schedule.get("case_rows_planned")
    exact(execution.get("attempts_started"), attempts_planned,
          "completed current evaluation does not have exactly six attempts")
    exact(attempts_planned, 6, "completed current evaluation does not have exactly six attempts")
    fail(summary.get("attempts_started") != execution["attempts_started"]
         or summary.get("planned_attempts") != attempts_planned
         or summary.get("planned_case_rows") != schedule.get("case_rows_planned")
         or summary.get("repetitions") != schedule.get("repetitions")
         or summary.get("evaluation_id") != plan["evaluation_id"]
         or summary.get("status") != "completed",
         "completed current summary is invalid")
    fail(summary.get("cases") != dataset.get("cases"), "summary cases are invalid")
    fail(summary.get("catalogue_owner_count") != plan["catalogue"]["owner_count"],
         "summary owner count is invalid")
    fail(summary.get("dependencies") != FROZEN_CURRENT_DEPENDENCIES,
         "summary dependencies are invalid")
    fail(not result_path.is_file(), "completed current evaluation requires a result")

    result = load_json(result_path, "current result")
    exact(set(result) if isinstance(result, dict) else None, {
        "schema_version", "evaluation_id", "protocol", "source", "dataset",
        "catalogue", "schedule", "attempts", "case_results", "results",
    }, "current result keys are invalid")
    exact(result.get("schema_version"), 2, "current result schema is invalid")
    exact(result.get("evaluation_id"), plan.get("evaluation_id"), "current result protocol is invalid")
    for name, path, frozen in (
        ("protocol", "routing-protocol.json", FROZEN_CURRENT_PROTOCOL_DIGEST),
        ("source", dataset.get("path"), FROZEN_CURRENT_SOURCE_DIGEST),
    ):
        binding = result.get(name)
        exact(set(binding) if isinstance(binding, dict) else None,
              {"path", "sha256", "frozen_sha256"}, "current result protocol binding is invalid")
        exact(binding.get("path"), path, "current result protocol binding is invalid")
        exact(binding.get("frozen_sha256"), frozen, "current result protocol binding is invalid")
        content = artifact(root, {"path": binding["path"], "sha256": binding["sha256"]},
                           f"current {name}")
        expected = plan_path.read_bytes() if name == "protocol" else holdout_path.read_bytes()
        exact(content, expected, f"current result {name} digest does not match")

    holdout = yaml.safe_load(holdout_path.read_bytes())
    cases = holdout.get("cases") if isinstance(holdout, dict) else None
    fail(not isinstance(cases, list) or not all(isinstance(case, dict) for case in cases),
         "current result case coverage is invalid")
    case_ids = [case.get("id") for case in cases]
    exact(result["dataset"], {"cases": len(case_ids), "case_ids": case_ids},
          "current result case coverage is invalid")
    exact((len(case_ids), dataset.get("cases")), (18, 18), "current result case coverage is invalid")
    exact(result["catalogue"], {"owner_count": plan["catalogue"]["owner_count"]},
          "current result owner count is invalid")
    exact(result["catalogue"]["owner_count"], 33, "current result owner count is invalid")
    providers = schedule["providers"]
    expected_schedule = {
        "attempts": attempts_planned,
        "case_rows": case_rows_planned,
        "families": [provider["family"] for provider in providers],
        "repetitions": schedule["repetitions"],
    }
    exact(result["schedule"], expected_schedule, "current result schedule is invalid")

    expected_cells = [
        (f"attempt-{index}", repetition, provider)
        for index, (repetition, provider) in enumerate(
            ((repetition, provider)
             for repetition in range(1, schedule["repetitions"] + 1)
             for provider in providers), start=1
        )
    ]
    attempts = result["attempts"]
    fail(not isinstance(attempts, list), "current attempt evidence is invalid")
    exact(len(attempts), len(expected_cells), "current result does not retain exactly six attempts")
    exact([row.get("id") if isinstance(row, dict) else None for row in attempts],
          [cell[0] for cell in expected_cells], "current attempt evidence is invalid")
    terminal_attempts = {"success", "timed-out", "invalid-output", "tool-error", "skipped", "excluded"}
    dispositions = {"used", "substituted", "unavailable", "failed"}
    route_ids: set[str] = set()
    receipt_ids: set[str] = set()
    receipt_paths: set[str] = set()
    for attempt, (_, repetition, provider) in zip(attempts, expected_cells):
        exact(set(attempt) if isinstance(attempt, dict) else None,
              {"id", "repetition", "status", "disposition", "lineage", "route_receipt"},
              "current attempt evidence is invalid")
        fail(isinstance(attempt["repetition"], bool)
             or not isinstance(attempt["repetition"], int)
             or attempt["repetition"] != repetition, "current attempt schedule is invalid")
        fail(attempt["status"] not in terminal_attempts, "current attempt status is invalid")
        fail(attempt["disposition"] not in dispositions, "current attempt disposition is invalid")
        lineage = attempt["lineage"]
        lineage_fields = {
            "adapter", "family", "requested_adapter", "requested_family",
            "requested_model", "actual_model", "requested_effort",
            "effective_effort", "substitution_reason",
        }
        exact(set(lineage) if isinstance(lineage, dict) else None, lineage_fields,
              "current provider lineage is invalid")
        fail(any(not isinstance(lineage[field], str) or not lineage[field].strip()
                 for field in lineage_fields - {"substitution_reason"}),
             "current provider lineage is invalid")
        fail(not isinstance(lineage["substitution_reason"], str),
             "current provider substitution reason is invalid")
        exact(tuple(lineage[field] for field in (
            "requested_adapter", "requested_family", "requested_model", "requested_effort")),
              tuple(provider[field] for field in ("adapter", "family", "model", "effort")),
              "current provider lineage does not match the frozen request")
        substituted = any(lineage[actual] != lineage[requested] for actual, requested in (
            ("adapter", "requested_adapter"), ("family", "requested_family"),
            ("actual_model", "requested_model"), ("effective_effort", "requested_effort")))
        fail(substituted and not lineage["substitution_reason"].strip(),
             "current provider substitution reason is required")
        fail(not substituted and lineage["substitution_reason"],
             "current provider substitution reason is undeclared")
        fail(substituted != (attempt["disposition"] == "substituted"),
             "current provider substitution disposition is invalid")
        fail(attempt["status"] == "success" and attempt["disposition"] not in {"used", "substituted"},
             "successful attempt disposition is invalid")
        fail(attempt["status"] != "success" and attempt["disposition"] == "used",
             "non-success attempt disposition is invalid")

        route = attempt["route_receipt"]
        exact(set(route) if isinstance(route, dict) else None,
              {"route_id", "receipt_id", "artifact"}, "current route receipt is invalid")
        fail(any(not isinstance(route[field], str) or not route[field].strip()
                 for field in ("route_id", "receipt_id")), "current route receipt is invalid")
        artifact_ref = route["artifact"]
        artifact_path = artifact_ref.get("path") if isinstance(artifact_ref, dict) else None
        fail(not isinstance(artifact_path, str) or artifact_path in receipt_paths
             or route["route_id"] in route_ids or route["receipt_id"] in receipt_ids,
             "current route receipt identity is duplicated")
        route_ids.add(route["route_id"])
        receipt_ids.add(route["receipt_id"])
        receipt_paths.add(artifact_path)
        receipt_bytes = artifact(root, artifact_ref, f"current {attempt['id']} route receipt")
        try:
            receipt = json.loads(receipt_bytes)
        except json.JSONDecodeError as exc:
            raise Invalid(f"current {attempt['id']} route receipt is invalid") from exc
        exact(receipt, {
            "evaluation_id": plan["evaluation_id"], "attempt_id": attempt["id"],
            "route_id": route["route_id"], "receipt_id": route["receipt_id"], "lineage": lineage,
        }, f"current {attempt['id']} route receipt is invalid")

    case_by_id = {case_id: case for case_id, case in zip(case_ids, cases)}
    attempt_status = {attempt["id"]: attempt["status"] for attempt in attempts}
    mapped_status = {
        "timed-out": "timed-out", "invalid-output": "invalid", "tool-error": "tool-error",
        "skipped": "skipped", "excluded": "excluded",
    }
    expected_rows = [(attempt["id"], case_id) for attempt in attempts for case_id in case_ids]
    case_results = result["case_results"]
    fail(not isinstance(case_results, list), "current case-result rows are invalid")
    exact(len(case_results), len(expected_rows), "current case-result rows are invalid")
    exact([(row.get("attempt_id"), row.get("case_id")) if isinstance(row, dict) else None
           for row in case_results], expected_rows, "current case-result rows are invalid")
    statuses = {"pass", "fail", "omitted", *mapped_status.values()}
    for row in case_results:
        exact(set(row) if isinstance(row, dict) else None,
              {"attempt_id", "case_id", "status", "primary_correct", "companion_correct"},
              "current case-result rows are invalid")
        fail(row["case_id"] not in case_by_id or row["status"] not in statuses,
             "current case-result rows are invalid")
        expected_status = mapped_status.get(attempt_status[row["attempt_id"]])
        fail(expected_status is not None and row["status"] != expected_status,
             "current case-result terminal state does not match its attempt")
        fail(not isinstance(row["primary_correct"], bool)
             or not isinstance(row["companion_correct"], bool),
             "current case-result rows are invalid")
        correct = row["primary_correct"] and row["companion_correct"]
        fail(row["status"] == "pass" and not correct, "current case-result rows are invalid")
        fail(row["status"] == "fail" and correct, "current case-result rows are invalid")
        fail(row["status"] not in {"pass", "fail"} and (row["primary_correct"] or row["companion_correct"]),
             "current case-result rows are invalid")

    results = result["results"]
    exact(set(results) if isinstance(results, dict) else None,
          {"accounting", "attempt_accounting", "metrics"}, "current result accounting is invalid")
    case_names = {
        "planned": None, "passed": "pass", "failed": "fail", "omitted": "omitted",
        "skipped": "skipped", "excluded": "excluded", "timed_out": "timed-out",
        "invalid": "invalid", "tool_errors": "tool-error",
    }
    accounting = results["accounting"]
    exact(set(accounting) if isinstance(accounting, dict) else None, set(case_names),
          "current case accounting is invalid")
    fail(any(isinstance(value, bool) or not isinstance(value, int) or value < 0
             for value in accounting.values()), "current case accounting is invalid")
    observed = {name: len(case_results) if state is None else sum(row["status"] == state for row in case_results)
                for name, state in case_names.items()}
    exact(accounting, observed, "current case accounting is invalid")

    attempt_names = {
        "planned": None, "base_planned": None, "retries": None, "succeeded": "success",
        "timed_out": "timed-out", "invalid_output": "invalid-output", "tool_errors": "tool-error",
        "skipped": "skipped", "excluded": "excluded",
    }
    attempt_accounting = results["attempt_accounting"]
    exact(set(attempt_accounting) if isinstance(attempt_accounting, dict) else None, set(attempt_names),
          "current attempt accounting is invalid")
    fail(any(isinstance(value, bool) or not isinstance(value, int) or value < 0
             for value in attempt_accounting.values()), "current attempt accounting is invalid")
    expected_attempt_accounting = {
        "planned": len(attempts), "base_planned": len(attempts), "retries": 0,
        **{name: sum(attempt["status"] == state for attempt in attempts)
           for name, state in attempt_names.items() if state is not None},
    }
    exact(attempt_accounting, expected_attempt_accounting, "current attempt accounting is invalid")

    metrics = results["metrics"]
    exact(set(metrics) if isinstance(metrics, dict) else None,
          {"primary_accuracy", "companion_fidelity", "critical_case_failures"},
          "current metrics are invalid")
    for name, key in (("primary_accuracy", "primary_correct"), ("companion_fidelity", "companion_correct")):
        metric = metrics[name]
        exact(set(metric) if isinstance(metric, dict) else None,
              {"numerator", "denominator", "value", "threshold", "passed"},
              "current metrics are invalid")
        numerator = sum(row[key] for row in case_results)
        denominator = len(case_results)
        value = numerator / denominator
        fail(isinstance(metric["numerator"], bool) or not isinstance(metric["numerator"], int)
             or isinstance(metric["denominator"], bool) or not isinstance(metric["denominator"], int)
             or not isinstance(metric["passed"], bool)
             or metric["numerator"] != numerator or metric["denominator"] != denominator
             or isinstance(metric["value"], bool) or not isinstance(metric["value"], (int, float))
             or not math.isfinite(float(metric["value"])) or metric["value"] != value
             or metric["threshold"] != plan["rubric"]["primary_threshold" if name == "primary_accuracy" else "companion_threshold"]
             or metric["passed"] != (value >= metric["threshold"]), "current metrics are invalid")
    critical_ids = {case_id for case_id, case in case_by_id.items() if case.get("critical")}
    fail(isinstance(metrics["critical_case_failures"], bool)
         or not isinstance(metrics["critical_case_failures"], int), "current metrics are invalid")
    exact(metrics["critical_case_failures"], sum(
        not row["primary_correct"] for row in case_results if row["case_id"] in critical_ids
    ), "current metrics are invalid")


def validate(receipt_path: Path, repository_root: Path) -> tuple[int, int]:
    receipt = json.loads(receipt_path.read_text())
    required = {
        "schema_version", "candidate_commit", "candidate_tree", "dataset", "classifier",
        "response_schema", "arms", "attempts", "required_attempt_ids", "threshold", "claim",
        "cross_primary_family_gate", "status",
    }
    fail(not isinstance(receipt, dict) or set(receipt) != required, "receipt keys are invalid")
    fail(receipt["schema_version"] != 2, "unsupported receipt schema")
    candidate = receipt["candidate_commit"]
    fail(not isinstance(candidate, str) or not COMMIT.fullmatch(candidate), "candidate commit is invalid")
    candidate_tree = git(repository_root, "rev-parse", f"{candidate}^{{tree}}").strip()
    fail(receipt["candidate_tree"] != candidate_tree, "candidate tree does not match candidate commit")
    evidence_root = receipt_path.parent
    dataset_bytes = artifact(evidence_root, receipt["dataset"], "dataset")
    instruction = artifact(evidence_root, receipt["classifier"], "classifier")
    artifact(evidence_root, receipt["response_schema"], "response schema")
    data = yaml.safe_load(dataset_bytes)
    cases = data.get("cases") if isinstance(data, dict) and data.get("schema_version") == 1 else None
    fail(not isinstance(cases, list) or not cases, "dataset is invalid")
    validate_candidate_cases(cases, lambda path: git(repository_root, "show", f"{candidate}:{path}"))
    expected = {row["id"]: row["expected"] for row in cases}

    arms = receipt["arms"]
    fail(not isinstance(arms, list) or len(arms) != 3, "receipt must contain three routing arms")
    by_role = {arm.get("role"): arm for arm in arms if isinstance(arm, dict)}
    fail(set(by_role) != ARM_ROLES, "receipt routing arm roles are invalid or duplicate")
    scores: dict[str, tuple[int, int]] = {}
    trial_scores: dict[str, list[tuple[int, int]]] = {}
    for role in ("candidate", "without-skill", "previous-package"):
        arm = by_role[role]
        fields = {
            "id", "role", "package_commit", "package_tree", "catalogue", "packet",
            "minimum_trials", "invocations", "score",
        }
        fail(set(arm) != fields or arm["id"] != role, f"{role} arm is invalid")
        package_commit = arm["package_commit"]
        if role == "without-skill":
            fail(package_commit is not None or arm["package_tree"] is not None, "without-skill arm must not bind a package")
            expected_catalogue = names_at_commit(repository_root, candidate)
        else:
            fail(not isinstance(package_commit, str) or not COMMIT.fullmatch(package_commit), f"{role} package commit is invalid")
            tree = git(repository_root, "rev-parse", f"{package_commit}^{{tree}}").strip()
            fail(arm["package_tree"] != tree, f"{role} package tree does not match")
            if role == "candidate":
                fail(package_commit != candidate or tree != candidate_tree, "candidate arm package is not the candidate")
            else:
                previous_package = git(repository_root, "rev-parse", f"{candidate}^2").strip()
                fail(package_commit != previous_package, "previous-package arm is not the candidate's merged baseline")
            expected_catalogue = catalogue_at_commit(repository_root, package_commit)
        catalogue = artifact(evidence_root, arm["catalogue"], f"{role} catalogue")
        fail(catalogue != expected_catalogue, f"{role} catalogue does not match its package")
        retained_packet = artifact(evidence_root, arm["packet"], f"{role} packet")
        fail(retained_packet != packet(instruction, catalogue, cases), f"{role} packet does not match retained inputs")
        minimum = arm["minimum_trials"]
        expected_minimum = 3 if role == "candidate" else 1
        fail(minimum != expected_minimum, f"{role} minimum_trials is invalid")
        invocations = arm["invocations"]
        fail(not isinstance(invocations, list) or len(invocations) != minimum, f"{role} invocation count is invalid")
        passed = total = 0
        arm_trial_scores: list[tuple[int, int]] = []
        for index, invocation in enumerate(invocations, start=1):
            fail(invocation.get("trial") != index, f"{role} trials are not contiguous")
            scored, count = validate_invocation(invocation, expected, evidence_root, f"{role} trial {index}")
            passed += scored
            total += count
            arm_trial_scores.append((scored, count))
        fail(arm["score"] != {"numerator": passed, "denominator": total}, f"{role} score is incorrect")
        scores[role] = (passed, total)
        trial_scores[role] = arm_trial_scores

    candidate_passed, candidate_total = scores["candidate"]
    threshold = receipt["threshold"]
    fail(
        threshold != {"numerator": candidate_passed, "denominator": candidate_total, "minimum_rate": 0.95},
        "candidate threshold does not match outputs",
    )
    fail(candidate_passed / candidate_total < threshold["minimum_rate"], "candidate exact routing is below threshold")
    candidate_pair = trial_scores["candidate"][0]
    previous_pair = trial_scores["previous-package"][0]
    fail(candidate_pair[0] / candidate_pair[1] < previous_pair[0] / previous_pair[1], "paired candidate regresses previous-package exact routing")
    fail(receipt["claim"] != "current-candidate-correctness-and-paired-non-regression", "receipt claim is invalid")
    fail(
        receipt["cross_primary_family_gate"] != {
            "status": "unmet",
            "covered_provider_families": ["openai"],
            "missing_provider_families": ["anthropic"],
            "reason": "claude-subscription-quota-unavailable",
        },
        "cross-primary-family promotion gate must be declared unmet",
    )
    required_attempt_ids = receipt["required_attempt_ids"]
    fail(
        required_attempt_ids != REQUIRED_ATTEMPT_IDS,
        "required attempt ids are invalid",
    )
    attempts_content = artifact(evidence_root, receipt["attempts"], "attempts")
    validate_attempts(attempts_content, evidence_root, required_attempt_ids, repository_root)
    fail(receipt["status"] != "evaluation-pass-promotion-gate-unmet", "receipt status is invalid")
    return candidate_passed, candidate_total


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--repository-root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args(argv)
    try:
        passed, total = validate(args.receipt.resolve(), args.repository_root.resolve())
    except (OSError, json.JSONDecodeError, yaml.YAMLError, Invalid) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print(
        f"PASS: exact candidate-bound routing {passed}/{total}; "
        "cross-primary-family promotion gate unmet"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
