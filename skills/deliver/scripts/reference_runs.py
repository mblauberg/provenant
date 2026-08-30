#!/usr/bin/env python3
"""Generate deterministic canonical reference runs for every delivery profile."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from typing import Any


ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[3])
).expanduser()
AGENTIC_RISKS = (
    "goal-hijack", "tool-misuse", "excessive-privilege", "supply-chain",
    "code-execution", "memory-context-poisoning", "insecure-inter-agent-communication",
    "cascading-failures", "human-trust-exploitation",
)


def make_reference_run(profile_name: str, root: Path = ROOT, *, high_stakes: bool = False) -> dict[str, Any]:
    registry = json.loads((root / "config" / "delivery-profiles.json").read_text())
    profile = registry["profiles"][profile_name]
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
                    "method": "scripts/check-harness" if gate == "tests" else f"reference-{gate}",
                    "artifact_id": "evidence-bundle",
                    "source_paths": ["input"],
                }
                if kind == "deterministic":
                    item["result"] = {"exit_code": 0, "receipt_digest": digest_b}
                if family:
                    item["model_lineage"] = {"adapter": "native-subagent" if family == "openai" else "claude-code", "provider_family": family, "model": "runtime-resolved"}
                    item["route_receipt"] = {
                        "path": f"review-route-{family}.json", "digest": digest_b,
                    }
                    item["source_paths"].append(f"review-route-{family}.json")
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
                    linked = {"id": f"security-{check}", "kind": "deterministic", "gate": check, "status": "pass", "method": f"reference-{check}", "artifact_id": "evidence-bundle", "source_paths": ["input"], "result": {"exit_code": 0, "receipt_digest": digest_b}}
                    evidence.append(linked)
                security_checks.append({"id": check, "surface": surface, "status": "pass", "evidence_id": linked["id"]})
    else:
        changed_surfaces = []
    deterministic_ids = [item["id"] for item in evidence if item["kind"] == "deterministic"]
    deterministic_id = deterministic_ids[0]
    agentic_risks = []
    if profile_name == "agent-product":
        evidence.append({"id": "agentic-risk-tool-misuse", "kind": "deterministic", "gate": "agentic-risk:tool-misuse", "status": "pass", "method": "inert tool-boundary fixture", "artifact_id": "evidence-bundle", "source_paths": ["input"], "result": {"exit_code": 0, "receipt_digest": digest_b}})
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
            {"id": "high-privacy", "kind": "deterministic", "gate": "high-stakes:privacy", "status": "pass", "method": "inert privacy boundary fixture", "artifact_id": "evidence-bundle", "source_paths": ["input"], "result": {"exit_code": 0, "receipt_digest": digest_b}},
            {"id": "high-domain-review", "kind": "human", "gate": "high-stakes:qualified-domain-review", "status": "pass", "method": "named qualified reference reviewer", "artifact_id": "evidence-bundle", "source_paths": []},
            {"id": "high-action", "kind": "human", "gate": "high-stakes:explicit-human-action", "status": "pass", "method": "explicit reference action decision", "artifact_id": "evidence-bundle", "source_paths": []},
        ])
        high_stakes_controls = {
            "source_authority": {"status": "pass", "evidence_id": "high-source-authority", "authority": "reference-source-owner"},
            "privacy": {"status": "pass", "evidence_id": "high-privacy", "privacy_boundary": "local synthetic fixtures only"},
            "qualified_domain_review": {"status": "pass", "evidence_id": "high-domain-review", "domain": "reference-domain", "reviewer": "qualified-reference-reviewer", "qualification": "scenario-fixture-authority"},
            "explicit_human_action_gate": {"status": "pass", "evidence_id": "high-action", "action": "accept-reference-artifact", "approved_by": "human-maintainer"},
        }
    history_states = ("draft", "scoped", "approved", "executing", "verifying", "reviewing", "awaiting_acceptance")
    return {
        "schema_version": 1,
        "contract": "delivery-run",
        "run_id": f"REF-{profile_name.upper()}",
        "fabric_relationships": {
            "mode": "independent",
            "delivery_run_id": f"REF-{profile_name.upper()}",
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
            "allowed_source_paths": ["input", "review-route-openai.json", "review-route-anthropic.json"],
            "allowed_artifact_paths": ["."],
            "denied_paths": [],
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
            *[{
                "id": f"review-route-{family}", "path": f"review-route-{family}.json",
                "media_type": "application/json", "artifact_type": "evidence",
                "digest": digest_b, "class": "evidence", "owner": "delivery-chair",
                "retention": "risk-policy",
            } for family in ("openai", "anthropic")],
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
            {"role": "targeted", "provider_family": "openai", "adapter": "native-subagent", "model": "runtime-resolved", "reviewer_id": "reference-openai-reviewer", "independent_of_authorship": True, "lenses": ["correctness-spec", "tests"], "status": "pass", "evidence_id": judgement_by_family["openai"][0], "reason": "", "route_receipt_digest": digest_b},
            {"role": "other-primary", "provider_family": "anthropic", "adapter": "claude-code", "model": "runtime-resolved", "reviewer_id": "reference-anthropic-reviewer", "independent_of_authorship": True, "lenses": ["architecture-evidence"], "status": "pass", "evidence_id": judgement_by_family["anthropic"][0], "reason": "", "route_receipt_digest": digest_b},
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
