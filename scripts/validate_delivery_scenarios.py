#!/usr/bin/env python3
"""Run independently authored delivery-profile cases against the kernel."""

from __future__ import annotations

import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from typing import Any

import yaml


PRODUCT_ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[1])
).expanduser()
SKILLS_ROOT = Path(
    os.environ.get("PROVENANT_SKILLS_ROOT", PRODUCT_ROOT / "skills")
).expanduser()
PROFILES = {"software", "research", "analysis", "document", "agent-product"}
CASE_TYPES = {"positive", "negative", "boundary"}
AGENTIC_RISKS = (
    "goal-hijack", "tool-misuse", "excessive-privilege", "supply-chain",
    "code-execution", "memory-context-poisoning", "insecure-inter-agent-communication",
    "cascading-failures", "human-trust-exploitation",
)
DIGEST_A = "sha256:" + "a" * 64
DIGEST_B = "sha256:" + "b" * 64


def load_kernel(skills_root: Path = SKILLS_ROOT):
    path = skills_root / "deliver" / "scripts" / "validate_delivery.py"
    spec = importlib.util.spec_from_file_location("held_out_delivery_kernel", path)
    if not spec or not spec.loader:
        raise ValueError("delivery kernel is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_evaluation_materializer(skills_root: Path = SKILLS_ROOT):
    path = skills_root / "deliver" / "scripts" / "reference_evaluation.py"
    spec = importlib.util.spec_from_file_location("held_out_evaluation_materializer", path)
    if not spec or not spec.loader:
        raise ValueError("evaluation materializer is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module





def load_receipt_producer(skills_root: Path = SKILLS_ROOT):
    path = skills_root / "deliver" / "scripts" / "delivery_receipt.py"
    spec = importlib.util.spec_from_file_location("scenario_delivery_receipt_producer", path)
    if not spec or not spec.loader:
        raise ValueError("delivery receipt producer is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

def _pointer_parent(document: Any, pointer: str) -> tuple[Any, str]:
    if not isinstance(pointer, str) or not pointer.startswith("/"):
        raise ValueError(f"invalid patch path {pointer!r}")
    parts = [part.replace("~1", "/").replace("~0", "~") for part in pointer[1:].split("/")]
    parent = document
    for part in parts[:-1]:
        parent = parent[int(part)] if isinstance(parent, list) else parent[part]
    return parent, parts[-1]


def _apply_patches(receipt: dict[str, Any], patches: list[dict[str, Any]]) -> None:
    for patch in patches:
        if not isinstance(patch, dict) or patch.get("op") not in {"replace", "remove"} or not patch.get("path"):
            raise ValueError("scenario patch is invalid")
        parent, key = _pointer_parent(receipt, patch["path"])
        index: int | str = int(key) if isinstance(parent, list) else key
        if patch["op"] == "replace":
            if "value" not in patch:
                raise ValueError("replace patch requires value")
            parent[index] = patch["value"]
        else:
            del parent[index]


def _apply_tamper(workspace_root: Path, tamper: Any) -> None:
    if tamper is None:
        return
    if not isinstance(tamper, dict) or set(tamper) != {"path", "append"}:
        raise ValueError("scenario tamper instruction is invalid")
    path, append = tamper["path"], tamper["append"]
    if not isinstance(path, str) or not path or not isinstance(append, str) or not append:
        raise ValueError("scenario tamper instruction requires path and append text")
    target = (workspace_root / path).resolve()
    try:
        target.relative_to(workspace_root.resolve())
    except ValueError as exc:
        raise ValueError("scenario tamper path escapes its workspace") from exc
    if not target.is_file():
        raise ValueError("scenario tamper target does not exist")
    target.write_bytes(target.read_bytes() + append.encode())


def _validate_dataset(data: Any) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    if not isinstance(data, dict) or data.get("schema_version") != 1:
        raise ValueError("invalid scenario dataset")
    fixtures = data.get("profile_fixtures")
    cases = data.get("cases")
    thresholds = data.get("thresholds")
    if not isinstance(fixtures, dict) or set(fixtures) != PROFILES or not isinstance(cases, list) or not isinstance(thresholds, dict):
        raise ValueError("scenario dataset is incomplete")
    if thresholds.get("minimum_expectation_match_rate") != 1.0:
        raise ValueError("minimum_expectation_match_rate must be 1.0")
    minimum_cases = thresholds.get("minimum_cases_per_profile")
    minimum_high_stakes = thresholds.get("minimum_high_stakes_cases")
    if isinstance(minimum_cases, bool) or not isinstance(minimum_cases, int) or minimum_cases < 2:
        raise ValueError("minimum_cases_per_profile must be at least 2")
    if isinstance(minimum_high_stakes, bool) or not isinstance(minimum_high_stakes, int) or minimum_high_stakes < 2:
        raise ValueError("minimum_high_stakes_cases must be at least 2")
    ids: set[str] = set()
    for case in cases:
        if not isinstance(case, dict) or not case.get("id") or case["id"] in ids:
            raise ValueError("case ids must be non-empty and unique")
        ids.add(case["id"])
        if case.get("profile") not in PROFILES or case.get("case_type") not in CASE_TYPES:
            raise ValueError(f"case {case['id']} has invalid classification")
        if case.get("expected") not in {"pass", "fail"} or (case["expected"] == "fail" and not case.get("expected_error")):
            raise ValueError(f"case {case['id']} has invalid expectation")
        repetitions = case.get("repetitions")
        if isinstance(repetitions, bool) or not isinstance(repetitions, int) or repetitions < 1:
            raise ValueError(f"case {case['id']} repetitions must be positive")
        if "stochastic" in case and not isinstance(case["stochastic"], bool):
            raise ValueError(f"case {case['id']} stochastic must be boolean")
        if case.get("tamper") is not None:
            tamper = case["tamper"]
            if not isinstance(tamper, dict) or set(tamper) != {"path", "append"}:
                raise ValueError(f"case {case['id']} tamper instruction is invalid")
        if "pre_materialize_patches" in case and not isinstance(case["pre_materialize_patches"], list):
            raise ValueError(f"case {case['id']} pre_materialize_patches must be a list")
    for profile in PROFILES:
        profile_cases = [case for case in cases if case["profile"] == profile]
        if len(profile_cases) < minimum_cases or not {"pass", "fail"} <= {case["expected"] for case in profile_cases}:
            raise ValueError(f"profile {profile} lacks positive and negative held-out coverage")
    if len([case for case in cases if case.get("high_stakes") is True]) < minimum_high_stakes:
        raise ValueError("high-stakes held-out coverage is below threshold")
    return fixtures, cases


def validate(
    dataset: Path, *, product_root: Path = PRODUCT_ROOT,
    skills_root: Path = SKILLS_ROOT,
) -> dict[str, Any]:
    try:
        data = yaml.safe_load(dataset.read_text())
    except (OSError, yaml.YAMLError) as exc:
        raise ValueError(f"scenario dataset is unreadable: {exc}") from exc
    fixtures, cases = _validate_dataset(data)
    kernel = load_kernel(skills_root)
    materializer = load_evaluation_materializer(skills_root)
    producer = load_receipt_producer(skills_root)
    matched = 0
    attempted = 0
    for case in cases:
        fixture = copy.deepcopy(fixtures[case["profile"]])
        fixture.update(copy.deepcopy(case.get("fixture_overrides", {})))
        for repetition in range(case["repetitions"]):
            receipt = producer.build_scenario_receipt(case, fixture, product_root)
            error = ""
            with tempfile.TemporaryDirectory(prefix="delivery-scenario-") as temporary:
                workspace_root = Path(temporary)
                _apply_patches(receipt, copy.deepcopy(case.get("pre_materialize_patches", [])))
                materializer.materialise_reference_run(
                    receipt, workspace_root, product_root, skills_root=skills_root,
                )
                _apply_patches(receipt, copy.deepcopy(case.get("patches", [])))
                _apply_tamper(workspace_root, copy.deepcopy(case.get("tamper")))
                try:
                    kernel.validate(
                        receipt, product_root, workspace_root=workspace_root,
                        verify_hashes=True,
                    )
                except kernel.Invalid as exc:
                    error = str(exc)
            actual = "fail" if error else "pass"
            expected_error = case.get("expected_error", "")
            matches = actual == case["expected"] and (actual == "pass" or expected_error in error)
            attempted += 1
            if matches:
                matched += 1
                continue
            raise ValueError(
                f"expectation mismatch for {case['id']} repetition {repetition + 1}: "
                f"expected {case['expected']} {expected_error!r}, got {actual} {error!r}"
            )
    rate = matched / attempted if attempted else 0.0
    threshold = data["thresholds"]["minimum_expectation_match_rate"]
    if rate < threshold:
        raise ValueError(f"expectation match rate {rate:.3f} is below {threshold:.3f}")
    return {"cases": len(cases), "attempted": attempted, "matched": matched, "match_rate": rate}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dataset", nargs="?", type=Path, default=PRODUCT_ROOT / "evals" / "delivery-profile-scenarios.yaml")
    parser.add_argument("--product-root", type=Path, default=PRODUCT_ROOT)
    parser.add_argument("--skills-root", type=Path, default=SKILLS_ROOT)
    args = parser.parse_args(argv)
    try:
        report = validate(
            args.dataset,
            product_root=args.product_root.resolve(),
            skills_root=args.skills_root.resolve(),
        )
    except ValueError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print(
        f"PASS: {report['matched']}/{report['attempted']} held-out attempts matched "
        f"across {report['cases']} cases ({report['match_rate']:.0%})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
