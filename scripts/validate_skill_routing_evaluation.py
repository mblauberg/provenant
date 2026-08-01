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
    """Validate the checked-in current routing plan in either allowed state."""
    plan_path = root / "routing-protocol.json"
    result_path = root / "routing-result.json"
    plan = json.loads(plan_path.read_text())
    holdout_path = root / plan["dataset"]["path"]
    frozen_plan = dict(plan)
    frozen_plan.pop("execution", None)
    fail(
        digest(json.dumps(frozen_plan, sort_keys=True, separators=(",", ":")).encode())
        != FROZEN_CURRENT_PROTOCOL_DIGEST,
        "current protocol digest is not frozen",
    )
    fail(digest(holdout_path.read_bytes()) != FROZEN_CURRENT_SOURCE_DIGEST,
         "current source digest is not frozen")
    execution = plan.get("execution")
    fail(not isinstance(execution, dict), "current execution block is invalid")
    status = execution.get("status")
    fail(status not in {"planned-unexecuted", "completed"}, "current execution status is invalid")
    summary = json.loads((root / "summary.json").read_text())["current_routing_regression"]
    fail(summary.get("evaluation_id") != plan["evaluation_id"], "current summary protocol is invalid")

    if status == "planned-unexecuted":
        fail(execution != {
            "attempts_started": 0,
            "blocked_reason": "FABRIC-ROUNDTRIP-UNAVAILABLE",
            "dependencies": FROZEN_CURRENT_DEPENDENCIES,
            "status": "planned-unexecuted",
        }, "planned current dependencies are invalid")
        fail(summary.get("dependencies") != FROZEN_CURRENT_DEPENDENCIES,
             "planned current dependencies are invalid")
        fail(
            summary.get("attempts_started") != 0
            or summary.get("cases") != plan["dataset"]["cases"]
            or summary.get("catalogue_owner_count") != plan["catalogue"]["owner_count"]
            or summary.get("planned_attempts") != plan["schedule"]["attempts_planned"]
            or summary.get("planned_case_rows") != plan["schedule"]["case_rows_planned"]
            or summary.get("blocked_reason") != execution["blocked_reason"]
            or summary.get("status") != "outstanding",
            "planned current summary is invalid",
        )
        fail(result_path.exists(), "planned current evaluation must not have a result")
        return

    fail(set(execution) != {"attempts_started", "status"}, "completed execution shape is invalid")
    fail(
        execution.get("attempts_started") != plan["schedule"]["attempts_planned"]
        or execution.get("attempts_started") != 6,
        "completed current evaluation does not have exactly six attempts",
    )
    fail(not result_path.is_file(), "completed current evaluation requires a result")
    result = json.loads(result_path.read_text())
    required = {
        "schema_version", "evaluation_id", "protocol", "source", "dataset",
        "catalogue", "schedule", "attempts", "case_results", "results",
    }
    fail(not isinstance(result, dict) or set(result) != required, "current result keys are invalid")
    fail(result["schema_version"] != 2, "current result schema is invalid")
    fail(result["evaluation_id"] != plan["evaluation_id"], "current result protocol is invalid")
    fail(
        not isinstance(result["protocol"], dict)
        or set(result["protocol"]) != {"path", "sha256", "frozen_sha256"}
        or result["protocol"]["path"] != "routing-protocol.json"
        or result["protocol"]["frozen_sha256"] != FROZEN_CURRENT_PROTOCOL_DIGEST,
        "current result protocol binding is invalid",
    )
    fail(
        not isinstance(result["source"], dict)
        or set(result["source"]) != {"path", "sha256", "frozen_sha256"}
        or result["source"]["path"] != plan["dataset"]["path"]
        or result["source"]["frozen_sha256"] != FROZEN_CURRENT_SOURCE_DIGEST,
        "current result source binding is invalid",
    )
    protocol = artifact(root, {
        "path": result["protocol"]["path"],
        "sha256": result["protocol"]["sha256"],
    }, "current protocol")
    fail(protocol != plan_path.read_bytes(), "current result protocol digest does not match")
    source = artifact(root, {
        "path": result["source"]["path"],
        "sha256": result["source"]["sha256"],
    }, "current source")
    fail(source != holdout_path.read_bytes(), "current result source digest does not match")

    holdout = yaml.safe_load(source)
    expected_case_ids = [case["id"] for case in holdout["cases"]]
    fail(
        result["dataset"] != {"cases": len(expected_case_ids), "case_ids": expected_case_ids}
        or len(expected_case_ids) != plan["dataset"]["cases"]
        or len(expected_case_ids) != 18,
        "current result case coverage is invalid",
    )
    fail(result["catalogue"] != {"owner_count": plan["catalogue"]["owner_count"]}
         or result["catalogue"]["owner_count"] != 33,
         "current result owner count is invalid")

    expected_schedule = {
        "attempts": plan["schedule"]["attempts_planned"],
        "case_rows": plan["schedule"]["case_rows_planned"],
        "families": [provider["family"] for provider in plan["schedule"]["providers"]],
        "repetitions": plan["schedule"]["repetitions"],
    }
    fail(result["schedule"] != expected_schedule, "current result schedule is invalid")

    fail(summary.get("attempts_started") != execution["attempts_started"],
         "completed current summary attempts are invalid")
    fail(summary.get("cases") != result["dataset"]["cases"],
         "completed current summary cases are invalid")
    fail(summary.get("catalogue_owner_count") != result["catalogue"]["owner_count"],
         "completed current summary owner count is invalid")
    fail(summary.get("dependencies") != FROZEN_CURRENT_DEPENDENCIES,
         "completed current summary dependencies are invalid")
    fail(summary.get("status") != "completed", "completed current summary status is invalid")

    attempts = result["attempts"]
    fail(not isinstance(attempts, list), "current attempt evidence is invalid")
    expected_cells = [
        (index, repetition, provider)
        for index, (repetition, provider) in enumerate(
            ((repetition, provider)
             for repetition in range(1, expected_schedule["repetitions"] + 1)
             for provider in plan["schedule"]["providers"]),
            start=1,
        )
    ]
    fail(len(attempts) != len(expected_cells), "current result does not retain exactly six attempts")
    attempt_ids = [row.get("id") if isinstance(row, dict) else None for row in attempts]
    fail(attempt_ids != [f"attempt-{index}" for index, _, _ in expected_cells],
         "current attempt evidence is invalid")

    terminal_attempt_statuses = {"success", "timed-out", "invalid-output", "tool-error", "skipped", "excluded"}
    dispositions = {"used", "substituted", "unavailable", "failed"}
    route_ids: set[str] = set()
    receipt_ids: set[str] = set()
    for attempt, (index, repetition, provider) in zip(attempts, expected_cells):
        fail(not isinstance(attempt, dict) or set(attempt) != {
            "id", "repetition", "status", "disposition", "lineage", "route_receipt",
        }, "current attempt evidence is invalid")
        fail(isinstance(attempt["repetition"], bool)
             or not isinstance(attempt["repetition"], int)
             or attempt["repetition"] != repetition, "current attempt schedule is invalid")
        fail(attempt["status"] not in terminal_attempt_statuses, "current attempt status is invalid")
        fail(attempt["disposition"] not in dispositions, "current attempt disposition is invalid")

        lineage = attempt["lineage"]
        fail(not isinstance(lineage, dict) or set(lineage) != {
            "adapter", "family", "requested_adapter", "requested_family",
            "requested_model", "actual_model", "requested_effort",
            "effective_effort", "substitution_reason",
        }, "current provider lineage is invalid")
        for field in ("adapter", "family", "requested_adapter", "requested_family",
                      "requested_model", "actual_model", "requested_effort", "effective_effort"):
            fail(not isinstance(lineage[field], str) or not lineage[field].strip(),
                 f"current provider lineage {field} is invalid")
        fail(
            lineage["requested_adapter"] != provider["adapter"]
            or lineage["requested_family"] != provider["family"]
            or lineage["requested_model"] != provider["model"]
            or lineage["requested_effort"] != provider["effort"],
            "current provider lineage does not match the frozen request",
        )
        substituted = any([
            lineage["adapter"] != lineage["requested_adapter"],
            lineage["family"] != lineage["requested_family"],
            lineage["actual_model"] != lineage["requested_model"],
            lineage["effective_effort"] != lineage["requested_effort"],
        ])
        reason = lineage["substitution_reason"]
        fail(not isinstance(reason, str), "current provider substitution reason is invalid")
        fail(substituted and not reason.strip(), "current provider substitution reason is required")
        fail(not substituted and reason != "", "current provider substitution reason is undeclared")
        fail(substituted != (attempt["disposition"] == "substituted"),
             "current provider substitution disposition is invalid")
        fail(attempt["status"] == "success" and attempt["disposition"] not in {"used", "substituted"},
             "successful attempt disposition is invalid")

        route_receipt = attempt["route_receipt"]
        fail(not isinstance(route_receipt, dict) or set(route_receipt) != {
            "route_id", "receipt_id", "artifact",
        }, "current route receipt is invalid")
        for field in ("route_id", "receipt_id"):
            fail(not isinstance(route_receipt[field], str) or not route_receipt[field].strip(),
                 "current route receipt is invalid")
        fail(route_receipt["route_id"] in route_ids or route_receipt["receipt_id"] in receipt_ids,
             "current route receipt identity is duplicated")
        route_ids.add(route_receipt["route_id"])
        receipt_ids.add(route_receipt["receipt_id"])
        receipt_bytes = artifact(root, route_receipt["artifact"], f"current {attempt['id']} route receipt")
        try:
            receipt = json.loads(receipt_bytes)
        except json.JSONDecodeError as exc:
            raise Invalid(f"current {attempt['id']} route receipt is invalid") from exc
        fail(
            not isinstance(receipt, dict)
            or set(receipt) != {"evaluation_id", "attempt_id", "route_id", "receipt_id", "lineage"}
            or receipt["evaluation_id"] != plan["evaluation_id"]
            or receipt["attempt_id"] != attempt["id"]
            or receipt["route_id"] != route_receipt["route_id"]
            or receipt["receipt_id"] != route_receipt["receipt_id"]
            or receipt["lineage"] != lineage,
            f"current {attempt['id']} route receipt is invalid",
        )

    holdout_cases = {case["id"]: case for case in holdout["cases"]}
    attempt_statuses = {attempt["id"]: attempt["status"] for attempt in attempts}
    terminal_case_for_attempt = {
        "timed-out": "timed-out",
        "invalid-output": "invalid",
        "tool-error": "tool-error",
        "skipped": "skipped",
        "excluded": "excluded",
    }
    expected_rows = [
        (attempt["id"], case["id"])
        for attempt in attempts
        for case in holdout["cases"]
    ]
    case_results = result["case_results"]
    fail(not isinstance(case_results, list) or len(case_results) != 108,
         "current case-result rows are invalid")
    fail(
        [(row.get("attempt_id"), row.get("case_id")) if isinstance(row, dict) else None
         for row in case_results] != expected_rows,
        "current case-result rows are invalid",
    )
    case_statuses = {"pass", "fail", "omitted", "skipped", "excluded", "timed-out", "invalid", "tool-error"}
    for row in case_results:
        fail(not isinstance(row, dict) or set(row) != {
            "attempt_id", "case_id", "status", "primary_correct", "companion_correct",
        }, "current case-result rows are invalid")
        fail(row["case_id"] not in holdout_cases or row["status"] not in case_statuses,
             "current case-result rows are invalid")
        expected_terminal_status = terminal_case_for_attempt.get(attempt_statuses[row["attempt_id"]])
        fail(expected_terminal_status is not None and row["status"] != expected_terminal_status,
             "current case-result terminal state does not match its attempt")
        fail(not isinstance(row["primary_correct"], bool)
             or not isinstance(row["companion_correct"], bool),
             "current case-result rows are invalid")
        fail(
            (row["status"] == "pass" and not (row["primary_correct"] and row["companion_correct"]))
            or (row["status"] != "pass" and (row["primary_correct"] or row["companion_correct"])),
            "current case-result rows are invalid",
        )

    results = result["results"]
    fail(not isinstance(results, dict) or set(results) != {
        "accounting", "attempt_accounting", "metrics",
    }, "current result accounting is invalid")
    accounting = results["accounting"]
    accounting_keys = {
        "planned", "passed", "failed", "omitted", "skipped", "excluded",
        "timed_out", "invalid", "tool_errors",
    }
    fail(not isinstance(accounting, dict) or set(accounting) != accounting_keys,
         "current case accounting is invalid")
    fail(any(isinstance(value, bool) or not isinstance(value, int) for value in accounting.values()),
         "current case accounting is invalid")
    status_counts = {
        "planned": len(case_results),
        "passed": sum(row["status"] == "pass" for row in case_results),
        "failed": sum(row["status"] == "fail" for row in case_results),
        "omitted": sum(row["status"] == "omitted" for row in case_results),
        "skipped": sum(row["status"] == "skipped" for row in case_results),
        "excluded": sum(row["status"] == "excluded" for row in case_results),
        "timed_out": sum(row["status"] == "timed-out" for row in case_results),
        "invalid": sum(row["status"] == "invalid" for row in case_results),
        "tool_errors": sum(row["status"] == "tool-error" for row in case_results),
    }
    fail(accounting != status_counts, "current case accounting is invalid")

    attempt_accounting = results["attempt_accounting"]
    attempt_accounting_keys = {
        "planned", "base_planned", "retries", "succeeded", "timed_out",
        "invalid_output", "tool_errors", "skipped", "excluded",
    }
    fail(not isinstance(attempt_accounting, dict) or set(attempt_accounting) != attempt_accounting_keys,
         "current attempt accounting is invalid")
    fail(any(isinstance(value, bool) or not isinstance(value, int) for value in attempt_accounting.values()),
         "current attempt accounting is invalid")
    expected_attempt_accounting = {
        "planned": len(attempts),
        "base_planned": len(attempts),
        "retries": 0,
        "succeeded": sum(row["status"] == "success" for row in attempts),
        "timed_out": sum(row["status"] == "timed-out" for row in attempts),
        "invalid_output": sum(row["status"] == "invalid-output" for row in attempts),
        "tool_errors": sum(row["status"] == "tool-error" for row in attempts),
        "skipped": sum(row["status"] == "skipped" for row in attempts),
        "excluded": sum(row["status"] == "excluded" for row in attempts),
    }
    fail(attempt_accounting != expected_attempt_accounting, "current attempt accounting is invalid")

    metrics = results["metrics"]
    fail(not isinstance(metrics, dict) or set(metrics) != {
        "primary_accuracy", "companion_fidelity", "critical_case_failures",
    }, "current metrics are invalid")
    metric_values = {
        "primary_accuracy": sum(row["primary_correct"] for row in case_results),
        "companion_fidelity": sum(row["companion_correct"] for row in case_results),
    }
    metric_thresholds = {
        "primary_accuracy": plan["rubric"]["primary_threshold"],
        "companion_fidelity": plan["rubric"]["companion_threshold"],
    }
    for name, numerator in metric_values.items():
        metric = metrics[name]
        fail(not isinstance(metric, dict) or set(metric) != {
            "numerator", "denominator", "value", "threshold", "passed",
        }, "current metrics are invalid")
        denominator = len(case_results)
        value = numerator / denominator
        fail(
            metric["numerator"] != numerator
            or metric["denominator"] != denominator
            or not isinstance(metric["value"], (int, float))
            or isinstance(metric["value"], bool)
            or not math.isfinite(float(metric["value"]))
            or metric["value"] != value
            or metric["threshold"] != metric_thresholds[name]
            or metric["passed"] != (value >= metric_thresholds[name]),
            "current metrics are invalid",
        )
    critical_ids = {case_id for case_id, case in holdout_cases.items() if case.get("critical")}
    expected_critical_failures = sum(
        not row["primary_correct"] for row in case_results if row["case_id"] in critical_ids
    )
    fail(
        isinstance(metrics["critical_case_failures"], bool)
        or not isinstance(metrics["critical_case_failures"], int)
        or metrics["critical_case_failures"] != expected_critical_failures,
         "current metrics are invalid")


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
