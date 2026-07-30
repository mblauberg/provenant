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


def _evidence(
    evidence_id: str, kind: str, gate: str, *, family: str | None = None,
) -> dict[str, Any]:
    item: dict[str, Any] = {
        "id": evidence_id,
        "kind": kind,
        "gate": gate,
        "status": "pass",
        "method": f"held-out-{gate}",
        "artifact_id": "evidence-bundle",
        "source_paths": [] if kind == "human" else ["inputs"],
    }
    if kind == "deterministic":
        item["result"] = {"exit_code": 0, "receipt_digest": DIGEST_B}
    if kind == "judgement":
        item["model_lineage"] = {
            "adapter": "native-subagent" if family == "openai" else "claude-code",
            "provider_family": family,
            "model": "held-out-model",
        }
    return item


def _compile_receipt(
    case: dict[str, Any], fixture: dict[str, Any], product_root: Path = PRODUCT_ROOT,
) -> dict[str, Any]:
    """Compile static held-out fixture data without reading production profiles."""
    profile = case["profile"]
    risk = case["risk_tier"]
    stochastic = case.get("stochastic", fixture["stochastic"])
    deterministic = list(fixture["deterministic_gates"])
    judgements = list(fixture["judgement_gates"])
    evidence = [_evidence(gate, "deterministic", gate) for gate in deterministic]
    judgement_ids: dict[str, list[str]] = {"openai": [], "anthropic": []}
    for gate in judgements:
        for family in ("openai", "anthropic"):
            evidence_id = f"{gate}-{family}"
            evidence.append(_evidence(evidence_id, "judgement", gate, family=family))
            judgement_ids[family].append(evidence_id)

    for gate in fixture["security_checks"]:
        if not any(item["gate"] == gate and item["kind"] == "deterministic" for item in evidence):
            evidence.append(_evidence(f"security-{gate}", "deterministic", gate))
    evidence.extend([
        _evidence("authority-approval", "human", "authority-approval"),
        _evidence("intent-approval", "human", "intent-approval"),
        _evidence("design-approval", "human", "design-approval"),
    ])

    if profile == "agent-product":
        evidence.append(_evidence("agentic-risk-tool-misuse", "deterministic", "agentic-risk:tool-misuse"))

    high_stakes_controls = None
    if case["high_stakes"]:
        evidence.extend([
            _evidence("high-source-authority", "human", "high-stakes:source-authority"),
            _evidence("high-privacy", "deterministic", "high-stakes:privacy"),
            _evidence("high-domain-review", "human", "high-stakes:qualified-domain-review"),
            _evidence("high-action", "human", "high-stakes:explicit-human-action"),
        ])
        high_stakes_controls = {
            "source_authority": {"status": "pass", "evidence_id": "high-source-authority", "authority": "named-source-owner"},
            "privacy": {"status": "pass", "evidence_id": "high-privacy", "privacy_boundary": "synthetic local fixtures"},
            "qualified_domain_review": {
                "status": "pass", "evidence_id": "high-domain-review", "domain": "held-out-domain",
                "reviewer": "named-reviewer", "qualification": "domain-credential",
            },
            "explicit_human_action_gate": {
                "status": "pass", "evidence_id": "high-action",
                "action": "approve-held-out-use", "approved_by": "human-owner",
            },
        }

    security_evidence = {item["gate"]: item["id"] for item in evidence if item["kind"] == "deterministic"}
    security_checks = [
        {"id": gate, "surface": surface, "status": "pass", "evidence_id": security_evidence[gate]}
        for surface in fixture["security_surfaces"]
        for gate in fixture["security_checks"]
        if (surface, gate) in {
            ("source", "secrets-scan"), ("source", "sast"),
            ("generated-artifact", "provenance"),
            ("destructive-boundary", "destructive-boundary-tests"),
            ("iac-container-config", "policy-scan"),
            ("agent-tools", "permission-check"), ("agent-tools", "tool-boundary-tests"),
            ("agent-tools", "prompt-injection-tests"),
        }
    ]
    review_evidence = [*deterministic, *judgement_ids["openai"], *judgement_ids["anthropic"]]
    states = ("draft", "scoped", "approved", "executing", "verifying", "reviewing", "awaiting_acceptance")
    first_judgement = judgement_ids["openai"][0]
    policy_path = product_root / "config" / "security-evidence.json"
    receipt: dict[str, Any] = {
        "schema_version": 1,
        "contract": "delivery-run",
        "run_id": f"HELD-{case['id'].upper()}",
        "profile": profile,
        "status": "awaiting_acceptance",
        "risk_tier": risk,
        "chair_family": "openai",
        "risk_assessment": {
            "blast_radius": "multi-module", "reversibility": "moderate",
            "data_sensitivity": "internal", "migration": "none", "oracle_quality": "mixed",
            "external_effects": "none", "critical_surface": "none",
        },
        "risk_override": {"status": "not-required", "approved_by": "", "evidence": "", "reason": ""},
        "high_stakes": case["high_stakes"],
        "intent": {
            "artifact": "outcome.bin", "digest": DIGEST_A, "decision_owner": "human-owner",
            "approval": {"status": "approved", "approver": "human-owner", "evidence": "intent-approval"},
        },
        "authority": {
            "schema_version": 2,
            "approved_by": "human-owner", "evidence": "authority-approval",
            "evidence_digest": DIGEST_B, "workspace_roots": ["."],
            "expires_at": "2027-07-10T00:00:00Z", "allowed_source_paths": ["inputs"],
            "allowed_artifact_paths": ["."],
            "allowed_fabric_operations": [], "denied_paths": [],
            "denied_fabric_operations": [],
            "prohibited_actions": ["external-release", "deployment", "irreversible-action"],
            "disclosure": "local-only", "secrets_access": "none", "secret_refs": [],
            "deployment": False, "deployment_targets": [],
            "irreversible_actions": False, "irreversible_action_ids": [],
            "network": {"tool_egress": "none", "allowed_hosts": []},
            "budget": {}, "delegations": [],
        },
        "artifacts": [
            {"id": "outcome", "path": "outcome.bin", "media_type": "application/octet-stream", "artifact_type": fixture["artifact_type"], "digest": DIGEST_A, "class": "canonical", "owner": "human-owner", "retention": "project-policy"},
            {"id": "evidence-bundle", "path": "evidence.json", "media_type": "application/json", "artifact_type": "evidence", "digest": DIGEST_B, "class": "evidence", "owner": "delivery-chair", "retention": "risk-policy"},
            *([{"id": "evaluation-receipt", "path": "evaluation/EVALUATION.json", "media_type": "application/json", "artifact_type": "evidence", "digest": DIGEST_B, "class": "evidence", "owner": "evaluation-chair", "retention": "risk-policy"}] if stochastic else []),
        ],
        "design": {
            "status": "approved", "artifact_id": "outcome", "digest": DIGEST_A,
            "approver": "human-owner", "evidence": "design-approval",
            "alternatives": ["do-nothing"], "failure_analysis": "held-out failure analysis",
            "containment": "discard the fixture", "one_way_doors": [],
        },
        "state_history": [
            {
                "state": state, "at": f"2026-07-10T00:{index:02d}:00Z",
                "evidence_ids": review_evidence if state == "awaiting_acceptance" else deterministic if state in {"verifying", "reviewing"} else [],
            }
            for index, state in enumerate(states)
        ],
        "evidence": evidence,
        "measures": {
            "outcome": [{"id": fixture["outcome_measure"], "status": "pass", "value": 1, "target": "pass", "aggregation": "held-out-case", "evidence_kind": "deterministic", "evidence_id": deterministic[0]}],
            "trajectory": [{"id": fixture["trajectory_measure"], "status": "pass", "value": 1, "target": "pass", "aggregation": "held-out-case", "evidence_kind": "deterministic", "evidence_id": deterministic[0]}],
        },
        "assurance": {
            "stochastic_required": stochastic,
            "reason": "held-out stochastic behaviour gate" if stochastic else "deterministic profile with independent review",
            "evaluations": ([{
                "status": "complete", "anchored_at": "2026-07-10T00:02:30Z",
                "evidence_id": first_judgement,
                "evaluation_artifact_id": "evaluation-receipt",
                "evaluation_id": "EVAL-REFERENCE",
                "evaluation_digest": DIGEST_B,
                "plan_digest": DIGEST_B,
            }] if stochastic else []),
        },
        "reviews": [
            {"role": "targeted", "provider_family": "openai", "adapter": "native-subagent", "model": "held-out-model", "independent_of_authorship": True, "lenses": ["correctness", "tests"], "status": "pass", "evidence_id": judgement_ids["openai"][0], "reason": ""},
            {"role": "other-primary", "provider_family": "anthropic", "adapter": "claude-code", "model": "held-out-model", "independent_of_authorship": True, "lenses": ["spec-alignment"], "status": "pass", "evidence_id": judgement_ids["anthropic"][0], "reason": ""},
            {"role": "distinct-family", "provider_family": "google", "adapter": "gemini", "model": "", "independent_of_authorship": True, "lenses": ["blind-spots"], "status": "unavailable", "evidence_id": "", "reason": "held-out case has no optional provider"},
        ],
        "security": {
            "status": "pass" if fixture["security_surfaces"] else "not_applicable",
            "reason": "" if fixture["security_surfaces"] else "held-out fixture has no changed technical surface",
            "policy_sha256": "sha256:" + hashlib.sha256(policy_path.read_bytes()).hexdigest(),
            "changed_surfaces": fixture["security_surfaces"],
            "artifact_surfaces": ([{"artifact_id": "outcome", "surfaces": fixture["security_surfaces"]}] if fixture["security_surfaces"] else []),
            "checks": security_checks,
            "agentic_risks": ([
                {"id": item, "status": "pass", "evidence_id": "agentic-risk-tool-misuse"}
                if item == "tool-misuse" else {"id": item, "status": "not_applicable", "reason": "not exercised by this held-out fixture"}
                for item in AGENTIC_RISKS
            ] if profile == "agent-product" else []),
        },
        "high_stakes_controls": high_stakes_controls,
        "human_gates": {
            "acceptance": {"status": "pending", "approver": "", "evidence": ""},
            "release": {"status": "pending", "approver": "", "evidence": ""},
        },
        "observation": {
            "status": "planned", "window": {"kind": "event-count", "minimum": 1},
            "signals": [fixture["observation_signal"]],
            "thresholds": {fixture["observation_signal"]: {"direction": "gte", "limit": 1}},
            "owner": "human-owner", "containment": "withdraw the artifact",
            "privacy": "aggregate-redacted", "close_condition": "threshold passes",
            "started_at": "", "ended_at": "", "observed_events": 0, "evidence_ids": [],
        },
        "incident": None, "retrospective": None, "repair_cycles": 0,
        "escaped_defect": False, "human_corrections": [],
        "checkpoint": {"generation": 0, "current_slice": "awaiting-acceptance", "next_action": "human acceptance", "in_flight": [], "artifact_paths": ["RUN.json"]},
        "degradation": None,
    }
    return receipt


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
    matched = 0
    attempted = 0
    for case in cases:
        fixture = copy.deepcopy(fixtures[case["profile"]])
        fixture.update(copy.deepcopy(case.get("fixture_overrides", {})))
        for repetition in range(case["repetitions"]):
            receipt = _compile_receipt(case, fixture, product_root)
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
