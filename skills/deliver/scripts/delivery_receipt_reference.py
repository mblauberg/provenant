#!/usr/bin/env python3
"""Generate deterministic canonical reference runs for every delivery profile."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
from typing import Any


ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[3])
).expanduser()
PRODUCT_ROOT = ROOT
SKILLS_ROOT = ROOT / "skills"
DIGEST_A = "sha256:" + "a" * 64
DIGEST_B = "sha256:" + "b" * 64

AGENTIC_RISKS = (
    "goal-hijack", "tool-misuse", "excessive-privilege", "supply-chain",
    "code-execution", "memory-context-poisoning", "insecure-inter-agent-communication",
    "cascading-failures", "human-trust-exploitation",
)


def _reference_execution_result(
    run_id: str, gate: str, source_paths: list[str], *, receipt: str = "RUN.json",
) -> dict[str, Any]:
    argv = ["reference-check", gate]
    empty_digest = "sha256:" + hashlib.sha256(b"").hexdigest()
    environment = {
        "platform": {"system": "reference", "release": "reference", "machine": "reference"},
        "python": {"executable": "reference", "version": "reference"},
        "variables": {name: "reference" for name in ("PATH", "VIRTUAL_ENV", "CONDA_PREFIX", "PYTHONHOME", "NODE_PATH")},
    }
    environment["digest"] = "sha256:" + hashlib.sha256(
        json.dumps(environment, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    streams = {
        stream: {
            "digest": empty_digest, "bytes": 0, "retained_bytes": 0,
            "truncated": False, "complete": True,
            "captured_b64": base64.b64encode(b"").decode(),
            "retained_b64": base64.b64encode(b"").decode(),
        }
        for stream in ("stdout", "stderr")
    }
    report_path = f"evidence/reference-{gate.replace(':', '-').replace('/', '-')}-gate-report.json"
    if report_path not in source_paths:
        source_paths.append(report_path)
    source_digests = [{"path": path, "digest": DIGEST_A, "bytes": 0} for path in source_paths]
    return {
        "exit_code": 0, "argv": argv,
        "gate_identity": {"id": gate, "argv": argv, "scope": "full"},
        "counts": {"scope": "full", "collected": 1, "passed": 1, "failed": 0, "skipped": 0, "expected_collected": 1},
        "gate_report": {
            "path": report_path, "digest": DIGEST_A,
            "baseline": {"kind": "structured-runner", "expected_collected": 1},
        },
        "environment": environment, "cwd": "reference",
        "run_identity": {"run_id": run_id, "receipt": receipt},
        "source_digests": source_digests, "source_digests_after": source_digests,
        "git": {"before": {"available": False, "reason": "reference fixture"}, "after": {"available": False, "reason": "reference fixture"}},
        **streams, "signal": None, "timed_out": False,
        "custody": {"status": "posix-process-group-cleanup", "cleanup": {"strategy": "posix-process-group-cleanup", "term_sent": False, "kill_sent": False, "grace_seconds": 0.1}, "unsupported": "Commands that daemonise or call setsid are unsupported."},
        "started_at": "2026-07-10T00:00:00Z", "finished_at": "2026-07-10T00:00:01Z",
        "receipt_digest": DIGEST_B,
    }


def make_reference_run(profile_name: str, root: Path = ROOT, *, high_stakes: bool = False) -> dict[str, Any]:
    registry = json.loads((root / "config" / "delivery-profiles.json").read_text())
    profile = registry["profiles"][profile_name]
    run_id = f"REF-{profile_name.upper()}"
    digest_a = "sha256:" + "a" * 64
    digest_b = "sha256:" + "b" * 64
    evidence = []
    judgement_by_family: dict[str, list[str]] = {"openai": [], "anthropic": []}
    for kind, gates in profile["required_evidence"].items():
        for gate in gates:
            families = ("openai", "anthropic") if kind == "judgement" else (None,)
            for family in families:
                evidence_id = f"{gate}-{family}" if family else gate
                item = {
                    "id": evidence_id,
                    "kind": kind,
                    "gate": gate,
                    "status": "pass",
                    "method": f"reference-{gate}",
                    "artifact_id": "evidence-bundle",
                    "source_paths": ["input"],
                }
                if kind == "deterministic":
                    item["result"] = _reference_execution_result(run_id, gate, item["source_paths"])
                    item["started_at"] = item["result"]["started_at"]
                    item["finished_at"] = item["result"]["finished_at"]
                if family:
                    item["model_lineage"] = {"adapter": "native-subagent" if family == "openai" else "claude-code", "provider_family": family, "model": "runtime-resolved"}
                    judgement_by_family[family].append(evidence_id)
                evidence.append(item)
    evidence.extend([
        {"id": "authority-approval", "kind": "human", "gate": "authority-approval", "status": "pass", "method": "explicit reference authority", "artifact_id": "evidence-bundle", "source_paths": []},
        {"id": "risk-override-approval", "kind": "human", "gate": "risk-override", "status": "pass", "method": "explicit reference risk decision", "artifact_id": "evidence-bundle", "source_paths": []},
        {"id": "intent-approval", "kind": "human", "gate": "intent-approval", "status": "pass", "method": "explicit reference approval", "artifact_id": "evidence-bundle", "source_paths": []},
        {"id": "design-approval", "kind": "human", "gate": "design-approval", "status": "pass", "method": "explicit reference approval", "artifact_id": "evidence-bundle", "source_paths": []},
        {"id": "acceptance-approval", "kind": "human", "gate": "human-acceptance", "status": "pass", "method": "reference acceptance placeholder", "artifact_id": "evidence-bundle", "source_paths": []},
        {"id": "release-approval", "kind": "human", "gate": "human-release", "status": "pass", "method": "reference release placeholder", "artifact_id": "evidence-bundle", "source_paths": []},
    ])
    security_checks = []
    security_status = "not_applicable"
    security_reason = f"no changed technical surface in the {profile_name} reference"
    if profile_name in {"software", "agent-product"}:
        security_status = "pass"
        security_reason = ""
        changed_surfaces = ["source"] if profile_name == "software" else ["agent-tools"]
        policy = json.loads((root / "config" / "security-evidence.json").read_text())
        security_checks = []
        for surface in changed_surfaces:
            for check in policy["surfaces"][surface]:
                linked = next((item for item in evidence if item["kind"] == "deterministic" and item["gate"] == check), None)
                if linked is None:
                    linked = {"id": f"security-{check}", "kind": "deterministic", "gate": check, "status": "pass", "method": f"reference-{check}", "artifact_id": "evidence-bundle", "source_paths": ["input"]}
                    linked["result"] = _reference_execution_result(run_id, check, linked["source_paths"])
                    linked["started_at"] = linked["result"]["started_at"]
                    linked["finished_at"] = linked["result"]["finished_at"]
                    evidence.append(linked)
                security_checks.append({"id": check, "surface": surface, "status": "pass", "evidence_id": linked["id"]})
    else:
        changed_surfaces = []
    deterministic_ids = [item["id"] for item in evidence if item["kind"] == "deterministic"]
    deterministic_id = deterministic_ids[0]
    agentic_risks = []
    if profile_name == "agent-product":
        agentic_evidence = {"id": "agentic-risk-tool-misuse", "kind": "deterministic", "gate": "agentic-risk:tool-misuse", "status": "pass", "method": "inert tool-boundary fixture", "artifact_id": "evidence-bundle", "source_paths": ["input"]}
        agentic_evidence["result"] = _reference_execution_result(run_id, "agentic-risk:tool-misuse", agentic_evidence["source_paths"])
        agentic_evidence["started_at"] = agentic_evidence["result"]["started_at"]
        agentic_evidence["finished_at"] = agentic_evidence["result"]["finished_at"]
        evidence.append(agentic_evidence)
        agentic_risks = [
            {
                "id": risk,
                "status": "pass" if risk == "tool-misuse" else "not_applicable",
                **({"evidence_id": "agentic-risk-tool-misuse"} if risk == "tool-misuse" else {"reason": "not exercised by the inert reference artifact"}),
            }
            for risk in AGENTIC_RISKS
        ]
    high_stakes_controls = None
    if high_stakes:
        evidence.extend([
            {"id": "high-source-authority", "kind": "human", "gate": "high-stakes:source-authority", "status": "pass", "method": "named reference source authority", "artifact_id": "evidence-bundle", "source_paths": []},
            {"id": "high-privacy", "kind": "deterministic", "gate": "high-stakes:privacy", "status": "pass", "method": "inert privacy boundary fixture", "artifact_id": "evidence-bundle", "source_paths": ["input"]},
            {"id": "high-domain-review", "kind": "human", "gate": "high-stakes:qualified-domain-review", "status": "pass", "method": "named qualified reference reviewer", "artifact_id": "evidence-bundle", "source_paths": []},
            {"id": "high-action", "kind": "human", "gate": "high-stakes:explicit-human-action", "status": "pass", "method": "explicit reference action decision", "artifact_id": "evidence-bundle", "source_paths": []},
        ])
        high_privacy = next(item for item in evidence if item["id"] == "high-privacy")
        high_privacy["result"] = _reference_execution_result(run_id, high_privacy["gate"], high_privacy["source_paths"])
        high_privacy["started_at"] = high_privacy["result"]["started_at"]
        high_privacy["finished_at"] = high_privacy["result"]["finished_at"]
        high_stakes_controls = {
            "source_authority": {"status": "pass", "evidence_id": "high-source-authority", "authority": "reference-source-owner"},
            "privacy": {"status": "pass", "evidence_id": "high-privacy", "privacy_boundary": "local synthetic fixtures only"},
            "qualified_domain_review": {"status": "pass", "evidence_id": "high-domain-review", "domain": "reference-domain", "reviewer": "qualified-reference-reviewer", "qualification": "scenario-fixture-authority"},
            "explicit_human_action_gate": {"status": "pass", "evidence_id": "high-action", "action": "accept-reference-artifact", "approved_by": "human-maintainer"},
        }
    route_path = "other-primary.route.json"
    other_primary_evidence = next(
        item for item in evidence
        if item.get("kind") == "judgement"
        and item.get("model_lineage", {}).get("provider_family") == "anthropic"
    )
    other_primary_evidence["source_paths"].append(route_path)
    other_primary_evidence["route_receipt"] = {"path": route_path, "digest": digest_b}
    for item in evidence:
        if item.get("kind") == "deterministic":
            result = item["result"]
            item.setdefault("started_at", result["started_at"])
            item.setdefault("finished_at", result["finished_at"])
    history_states = ("draft", "scoped", "approved", "executing", "verifying", "reviewing", "awaiting_acceptance")
    return {
        "schema_version": 1,
        "contract": "delivery-run",
        "execution_contract": "delivery-v1",
        "run_id": run_id,
        "fabric_relationships": {
            "mode": "independent",
            "delivery_run_id": run_id,
            "project_session_id": "not_applicable",
            "coordination_run_id": "not_applicable",
            "workstream_id": "not_applicable",
            "lead_agent_id": "not_applicable",
        },
        "profile": profile_name,
        "status": "awaiting_acceptance",
        "risk_tier": "substantial",
        "chair_family": "openai",
        "risk_assessment": {
            "blast_radius": "multi-module",
            "reversibility": "moderate",
            "data_sensitivity": "internal",
            "migration": "none",
            "oracle_quality": "mixed",
            "external_effects": "none",
            "critical_surface": "none",
        },
        "risk_override": {"status": "not-required", "approved_by": "", "evidence": "", "reason": ""},
        "high_stakes": high_stakes,
        "intent": {
            "artifact": "intent.md",
            "digest": digest_a,
            "decision_owner": "human-maintainer",
            "approval": {"status": "approved", "approver": "human-maintainer", "evidence": "intent-approval"},
        },
        "authority": {
            "schema_version": 2,
            "approved_by": "human-maintainer",
            "evidence": "authority-approval",
            "evidence_digest": digest_b,
            "workspace_roots": ["."],
            "expires_at": "2027-07-10T00:00:00Z",
            "allowed_source_paths": ["."],
            "allowed_artifact_paths": ["."],
            "allowed_fabric_operations": [],
            "denied_paths": [],
            "denied_fabric_operations": [],
            "prohibited_actions": ["external-release", "deployment", "irreversible-action"],
            "disclosure": "local-only",
            "secrets_access": "none",
            "secret_refs": [],
            "deployment": False,
            "deployment_targets": [],
            "irreversible_actions": False,
            "irreversible_action_ids": [],
            "network": {"tool_egress": "none", "allowed_hosts": []},
            "budget": {},
            "delegations": [],
        },
        "artifacts": [
            {"id": "intent", "path": "intent.md", "media_type": "text/markdown", "artifact_type": profile["artifact_types"][0], "digest": digest_a, "class": "canonical", "owner": "human-maintainer", "retention": "project-policy"},
            {"id": "evidence-bundle", "path": "evidence.json", "media_type": "application/json", "artifact_type": "evidence", "digest": digest_b, "class": "evidence", "owner": "delivery-chair", "retention": "risk-policy"},
            {"id": "other-primary-route", "path": route_path, "media_type": "application/json", "artifact_type": "evidence", "digest": digest_b, "class": "evidence", "owner": "delivery-chair", "retention": "risk-policy"},
            *([{"id": "evaluation-receipt", "path": "evaluation/EVALUATION.json", "media_type": "application/json", "artifact_type": "evidence", "digest": digest_b, "class": "evidence", "owner": "evaluation-chair", "retention": "risk-policy"}] if profile_name == "agent-product" else []),
        ],
        "design": {
            "status": "approved",
            "artifact_id": "intent",
            "digest": digest_a,
            "approver": "human-maintainer",
            "evidence": "design-approval",
            "alternatives": ["retain-current-specialised-flow"],
            "failure_analysis": "reference failure analysis",
            "containment": "discard inert reference artifacts",
            "one_way_doors": [],
        },
        "state_history": [
            {
                "state": state,
                "at": f"2026-07-10T00:{index:02d}:00Z",
                "evidence_ids": (
                    [*deterministic_ids, *judgement_by_family["openai"], *judgement_by_family["anthropic"]]
                    if state == "awaiting_acceptance"
                    else deterministic_ids if state in {"verifying", "reviewing"} else []
                ),
            }
            for index, state in enumerate(history_states)
        ],
        "evidence": evidence,
        "measures": {
            "outcome": [{"id": profile["required_measures"]["outcome"][0], "status": "pass", "value": 1, "target": "pass", "aggregation": "single-reference", "evidence_kind": "deterministic", "evidence_id": deterministic_id}],
            "trajectory": [{"id": profile["required_measures"]["trajectory"][0], "status": "pass", "value": 1, "target": "pass", "aggregation": "single-reference", "evidence_kind": "deterministic", "evidence_id": deterministic_id}],
        },
        "assurance": {
            "stochastic_required": profile_name == "agent-product",
            "reason": "reference deliberately exercises stochastic agent behaviour" if profile_name == "agent-product" else "profile reference uses deterministic and independent-review evidence",
            "evaluations": ([{
                "status": "complete",
                "anchored_at": "2026-07-10T00:02:30Z",
                "evidence_id": judgement_by_family["openai"][0],
                "evaluation_artifact_id": "evaluation-receipt",
                "evaluation_id": "EVAL-REFERENCE",
                "evaluation_digest": digest_b,
                "plan_digest": digest_b,
            }] if profile_name == "agent-product" else []),
        },
        "reviews": [
            {"role": "targeted", "provider_family": "openai", "adapter": "native-subagent", "model": "runtime-resolved", "independent_of_authorship": True, "lenses": ["correctness-spec", "tests"], "status": "pass", "evidence_id": judgement_by_family["openai"][0], "reason": ""},
            {"role": "other-primary", "provider_family": "anthropic", "adapter": "claude-code", "model": "runtime-resolved", "reviewer_id": "reference-anthropic", "independent_of_authorship": True, "lenses": ["architecture-evidence"], "status": "pass", "evidence_id": judgement_by_family["anthropic"][0], "reason": ""},
            {"role": "distinct-family", "provider_family": "google", "adapter": "gemini", "model": "", "independent_of_authorship": True, "lenses": ["blind-spots"], "status": "unavailable", "evidence_id": "", "reason": "reference run does not invoke optional providers"},
        ],
        "security": {
            "status": security_status,
            "reason": security_reason,
            "policy_sha256": "sha256:" + hashlib.sha256((root / "config" / "security-evidence.json").read_bytes()).hexdigest(),
            "changed_surfaces": changed_surfaces,
            "artifact_surfaces": ([{"artifact_id": "intent", "surfaces": changed_surfaces}] if profile_name in {"software", "agent-product"} else []),
            "checks": security_checks,
            "agentic_risks": agentic_risks,
        },
        "high_stakes_controls": high_stakes_controls,
        "human_gates": {
            "acceptance": {"status": "pending", "approver": "", "evidence": ""},
            "release": {"status": "pending", "approver": "", "evidence": ""},
        },
        "observation": {
            "status": "planned",
            "window": {"kind": "event-count", "minimum": 1},
            "signals": profile["observation_examples"][:1],
            "thresholds": {profile["observation_examples"][0]: {"direction": "gte", "limit": 1}},
            "owner": "human-maintainer",
            "containment": "withdraw or revert the artifact",
            "privacy": "aggregate-redacted",
            "close_condition": "declared threshold passes for the window",
            "started_at": "",
            "ended_at": "",
            "observed_events": 0,
            "evidence_ids": [],
        },
        "incident": None,
        "retrospective": None,
        "repair_cycles": 0,
        "escaped_defect": False,
        "human_corrections": [],
        "checkpoint": {"generation": 0, "current_slice": "awaiting-acceptance", "next_action": "human acceptance", "in_flight": [], "artifact_paths": ["RUN.json"]},
        "degradation": None,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    registry = json.loads((args.root / "config" / "delivery-profiles.json").read_text())
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for profile in registry["profiles"]:
        (args.output_dir / f"{profile}.json").write_text(json.dumps(make_reference_run(profile, args.root), indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


def _evidence(
    evidence_id: str, kind: str, gate: str, *, family: str | None = None,
    run_id: str = "HELD-REFERENCE",
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
        item["result"] = _reference_execution_result(run_id, gate, item["source_paths"])
        item["started_at"] = item["result"]["started_at"]
        item["finished_at"] = item["result"]["finished_at"]
    if kind == "judgement":
        item["model_lineage"] = {
            "adapter": "native-subagent" if family == "openai" else "claude-code",
            "provider_family": family,
            "model": "held-out-model",
        }
    return item


def build_scenario_receipt(
    case: dict[str, Any], fixture: dict[str, Any], product_root: Path = PRODUCT_ROOT,
) -> dict[str, Any]:
    """Compile static held-out fixture data without reading production profiles."""
    profile = case["profile"]
    risk = case["risk_tier"]
    stochastic = case.get("stochastic", fixture["stochastic"])
    run_id = f"HELD-{case['id'].upper()}"
    deterministic = list(fixture["deterministic_gates"])
    judgements = list(fixture["judgement_gates"])
    evidence = [_evidence(gate, "deterministic", gate, run_id=run_id) for gate in deterministic]
    judgement_ids: dict[str, list[str]] = {"openai": [], "anthropic": []}
    for gate in judgements:
        for family in ("openai", "anthropic"):
            evidence_id = f"{gate}-{family}"
            evidence.append(_evidence(evidence_id, "judgement", gate, family=family, run_id=run_id))
            judgement_ids[family].append(evidence_id)
    route_path = "other-primary.route.json"
    other_primary_evidence = next(
        item for item in evidence
        if item.get("kind") == "judgement"
        and item.get("model_lineage", {}).get("provider_family") == "anthropic"
    )
    other_primary_evidence["source_paths"].append(route_path)
    other_primary_evidence["route_receipt"] = {"path": route_path, "digest": DIGEST_B}

    for gate in fixture["security_checks"]:
        if not any(item["gate"] == gate and item["kind"] == "deterministic" for item in evidence):
            evidence.append(_evidence(f"security-{gate}", "deterministic", gate, run_id=run_id))
    evidence.extend([
        _evidence("authority-approval", "human", "authority-approval", run_id=run_id),
        _evidence("intent-approval", "human", "intent-approval", run_id=run_id),
        _evidence("design-approval", "human", "design-approval", run_id=run_id),
    ])

    if profile == "agent-product":
        evidence.append(_evidence("agentic-risk-tool-misuse", "deterministic", "agentic-risk:tool-misuse", run_id=run_id))

    high_stakes_controls = None
    if case["high_stakes"]:
        evidence.extend([
            _evidence("high-source-authority", "human", "high-stakes:source-authority", run_id=run_id),
            _evidence("high-privacy", "deterministic", "high-stakes:privacy", run_id=run_id),
            _evidence("high-domain-review", "human", "high-stakes:qualified-domain-review", run_id=run_id),
            _evidence("high-action", "human", "high-stakes:explicit-human-action", run_id=run_id),
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
        "execution_contract": "delivery-v1",
        "run_id": run_id,
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
            "expires_at": "2027-07-10T00:00:00Z", "allowed_source_paths": ["."],
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
            {"id": "other-primary-route", "path": route_path, "media_type": "application/json", "artifact_type": "evidence", "digest": DIGEST_B, "class": "evidence", "owner": "delivery-chair", "retention": "risk-policy"},
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
            {"role": "other-primary", "provider_family": "anthropic", "adapter": "claude-code", "model": "held-out-model", "reviewer_id": "held-out-anthropic", "independent_of_authorship": True, "lenses": ["spec-alignment"], "status": "pass", "evidence_id": judgement_ids["anthropic"][0], "reason": ""},
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
