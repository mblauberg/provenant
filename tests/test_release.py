import importlib.util
import hashlib
import json
from pathlib import Path
import re

import pytest


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "skills" / "release" / "scripts" / "validate_release.py"
SPEC = importlib.util.spec_from_file_location("validate_release", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


@pytest.fixture(autouse=True)
def _detach_deleted_fabric_authority_registry(monkeypatch):
    policy = MODULE.DELIVERY_VALIDATOR._policy_validation_module()
    monkeypatch.setattr(
        policy, "_fabric_operations", lambda _root, _invalid_type: frozenset()
    )
    monkeypatch.setattr(
        policy, "_fabric_cost_pattern", lambda _root, _invalid_type: re.compile(r"(?!)")
    )


def write_accepted_document_delivery(tmp_path, status="awaiting_release"):
    reference_path = ROOT / "skills" / "deliver" / "scripts" / "reference_runs.py"
    spec = importlib.util.spec_from_file_location("release_document_fixture", reference_path)
    assert spec and spec.loader
    reference = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reference)
    delivery = reference.make_reference_run("document", ROOT)
    assert delivery["authority"]["allowed_fabric_operations"] == []
    assert delivery["authority"]["denied_fabric_operations"] == []
    assert not any(key.startswith("cost:") for key in delivery["authority"]["budget"])

    materializer_path = ROOT / "skills" / "deliver" / "scripts" / "reference_evaluation.py"
    materializer_spec = importlib.util.spec_from_file_location(
        "release_document_materializer", materializer_path,
    )
    assert materializer_spec and materializer_spec.loader
    materializer = importlib.util.module_from_spec(materializer_spec)
    materializer_spec.loader.exec_module(materializer)
    materializer.materialise_reference_run(delivery, tmp_path, ROOT)

    intent = b"accepted document\n"
    (tmp_path / "intent.md").write_bytes(intent)
    intent_digest = "sha256:" + hashlib.sha256(intent).hexdigest()
    delivery["artifacts"][0]["digest"] = intent_digest
    delivery["intent"]["digest"] = intent_digest
    delivery["design"]["digest"] = intent_digest
    delivery["human_gates"]["acceptance"] = {
        "status": "approved", "approver": "human", "evidence": "acceptance-approval",
    }
    delivery["state_history"].extend([
        {"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "awaiting_release", "at": "2026-07-10T00:10:00Z", "evidence_ids": ["acceptance-approval"]},
    ])
    if status == "observing":
        delivery["human_gates"]["release"] = {
            "status": "approved", "approver": "human", "evidence": "release-approval",
        }
        delivery["state_history"].append({
            "state": "observing", "at": "2026-07-10T00:11:00Z",
            "evidence_ids": ["release-approval"],
        })
        delivery["observation"]["status"] = "active"
    delivery["checkpoint"].update({
        "current_slice": "observing" if status == "observing" else "awaiting-release",
        "next_action": "observe promotion" if status == "observing" else "await release authority",
        "in_flight": [],
    })
    delivery["status"] = status
    (tmp_path / "RUN.json").write_text(json.dumps(delivery))
    return delivery


def valid_receipt(status="awaiting-promotion"):
    return {
        "schema_version": 2,
        "release_id": "REL-DOC-1",
        "updated_at": "2026-07-11T04:00:00Z",
        "status": status,
        "action_type": "send",
        "target": {
            "id": "recipient:approved-counsel",
            "kind": "recipient",
            "environment_tier": "not-applicable",
            "disclosure": "restricted",
        },
        "artifact": {
            "id": "accepted-report",
            "digest": "sha256:" + "a" * 64,
            "acceptance_receipt": "RUN.json",
        },
        "owner": "promotion-owner",
        "release_authority": {
            "approved_by": "human",
            "expires_at": "2026-07-12T04:00:00Z",
            "action_types": ["send"],
            "target_ids": ["recipient:approved-counsel"],
            "target_environment_tiers": ["not-applicable"],
            "artifact_ids": ["accepted-report"],
            "allowed_execution_modes": ["connector"],
            "allowed_operations": ["send-via-approved-channel", "recall-delivery"],
            "allowed_command_prefixes": [],
            "secrets_access": "none",
            "external_communication": True,
            "public_disclosure": False,
            "irreversible_action": False,
        },
        "data_policy": {
            "classification": "confidential",
            "allowed_disclosure": "accepted report to named counsel only",
            "secret_handling": "none",
            "retention_or_expiry": "recipient matter policy",
        },
        "change_impact": {
            "state_change": "none",
            "compatibility": "not-applicable",
            "ordered_steps": [],
            "compatibility_window": "not-applicable",
            "recovery_point": "not-applicable",
            "readiness_evidence": [],
        },
        "readiness_checks": [{
            "id": "recipient-verified",
            "status": "pass",
            "evidence": ["approved-recipient-register"],
            "checked_at": "2026-07-11T03:59:00Z",
        }],
        "promotion_plan": {
            "plan": "send once through the approved matter channel",
            "exposure_cap": "one named recipient",
            "stop_conditions": ["recipient identity mismatch"],
        },
        "reversal": {
            "mode": "recall",
            "tested": False,
            "irreversible": False,
            "plan": "recall through the approved channel and notify the owner",
            "owner": "promotion-owner",
            "time_bound": "10m",
            "limitations": "recipient may already have opened the message",
        },
        "proof": {
            "plan": "capture provider delivery confirmation and artifact digest",
            "owner": "proof-owner",
            "close_condition": "delivery and digest requirements pass",
            "observation_window": "",
            "window_started_at": "",
            "window_ended_at": "",
            "requirements": [
                {"id": "delivered", "description": "approved channel confirms delivery"},
                {"id": "digest", "description": "delivered artifact digest matches acceptance"},
            ],
            "checks": [],
        },
        "human_promotion": {"status": "pending", "approved_by": "", "approved_at": ""},
        "execution": {"operations": []},
        "reversal_execution": {"operations": [], "checks": []},
        "outcome": {"status": "pending", "evidence": [], "follow_up_owner": ""},
    }


def complete_receipt():
    receipt = valid_receipt("complete")
    receipt["updated_at"] = "2026-07-11T04:05:00Z"
    receipt["human_promotion"] = {
        "status": "approved",
        "approved_by": "human",
        "approved_at": "2026-07-11T04:01:00Z",
    }
    receipt["execution"]["operations"] = [{
        "mode": "connector",
        "operation": "send-via-approved-channel",
        "actor": "promotion-owner",
        "status": "succeeded",
        "evidence": ["provider-operation-id"],
        "started_at": "2026-07-11T04:02:00Z",
        "finished_at": "2026-07-11T04:03:00Z",
    }]
    receipt["proof"]["checks"] = [
        {
            "requirement_id": "delivered",
            "status": "pass",
            "evidence": ["provider-delivery-receipt"],
            "observed_at": "2026-07-11T04:04:00Z",
        },
        {
            "requirement_id": "digest",
            "status": "pass",
            "evidence": ["sha256:a..."],
            "observed_at": "2026-07-11T04:04:00Z",
        },
    ]
    receipt["outcome"] = {
        "status": "complete",
        "evidence": ["delivery requirements passed"],
        "follow_up_owner": "",
    }
    return receipt


def validate_policy(receipt, gate):
    """Exercise receipt policy without claiming live artifact verification."""
    return MODULE.validate(receipt, gate, structural_only=True)


def bind_accepted_artifact(receipt, delivery):
    receipt["artifact"] = {
        "id": "intent",
        "digest": delivery["artifacts"][0]["digest"],
        "acceptance_receipt": "RUN.json",
    }
    receipt["release_authority"]["artifact_ids"] = ["intent"]


def test_programmatic_gate_requires_a_live_verification_root():
    errors = MODULE.validate(valid_receipt(), "ready")
    assert (
        "release ready/complete gate requires base_dir or workspace_root "
        "for canonical accepted-artifact verification"
    ) in errors


def test_noncanonical_schema_fails_closed():
    receipt = valid_receipt()
    receipt["schema_version"] = 1
    assert "schema_version must be 2" in validate_policy(receipt, "ready")


def test_ready_gate_supports_authorised_send_without_shell_commands():
    assert validate_policy(valid_receipt(), "ready") == []


@pytest.mark.parametrize("extra", [None, False, "", [], {}, {"repository": "."}])
def test_release_artifact_identity_union_uses_closed_field_presence(extra):
    receipt = valid_receipt()
    receipt["artifact"]["git_revision"] = extra
    errors = validate_policy(receipt, "ready")
    assert "artifact fields must be exactly one digest or git_revision shape" in errors


def test_release_git_revision_shape_rejects_digest_and_unknown_fields_even_when_empty():
    receipt = valid_receipt()
    receipt["artifact"] = {
        "id": "accepted-source",
        "git_revision": {"repository": ".", "commit": "1" * 40, "tree": "2" * 40},
        "digest": "",
        "acceptance_receipt": "RUN.json",
    }
    errors = validate_policy(receipt, "ready")
    assert "artifact fields must be exactly one digest or git_revision shape" in errors

    receipt["artifact"].pop("digest")
    receipt["artifact"]["unexpected"] = False
    errors = validate_policy(receipt, "ready")
    assert "artifact fields must be exactly one digest or git_revision shape" in errors


def test_release_git_revision_rejects_mixed_object_widths():
    receipt = valid_receipt()
    receipt["artifact"] = {
        "id": "accepted-source",
        "git_revision": {"repository": ".", "commit": "1" * 40, "tree": "2" * 64},
        "acceptance_receipt": "RUN.json",
    }
    errors = validate_policy(receipt, "ready")
    assert "artifact.git_revision must contain an exact repository, commit and tree" in errors


def test_cli_accepts_a_canonical_nonsoftware_delivery(tmp_path):
    delivery = write_accepted_document_delivery(tmp_path)
    receipt = valid_receipt()
    bind_accepted_artifact(receipt, delivery)
    path = tmp_path / "RELEASE.json"
    path.write_text(json.dumps(receipt))
    assert MODULE.main([
        "--gate", "ready", "--workspace-root", str(tmp_path), str(path),
    ]) == 0


def test_api_can_use_workspace_root_as_the_verification_root(tmp_path):
    delivery = write_accepted_document_delivery(tmp_path)
    receipt = valid_receipt()
    bind_accepted_artifact(receipt, delivery)
    assert MODULE.validate(receipt, "ready", workspace_root=tmp_path) == []


def test_artifact_digest_and_live_delivery_bytes_are_bound(tmp_path):
    delivery = write_accepted_document_delivery(tmp_path)
    receipt = valid_receipt()
    bind_accepted_artifact(receipt, delivery)
    assert MODULE.validate(receipt, "ready", workspace_root=tmp_path) == []

    receipt["artifact"]["digest"] = "sha256:" + "0" * 64
    assert "artifact.digest must match the accepted delivery artifact digest" in MODULE.validate(
        receipt, "ready", workspace_root=tmp_path,
    )

    bind_accepted_artifact(receipt, delivery)
    (tmp_path / "intent.md").write_text("tampered\n")
    assert "artifact.acceptance_receipt must be a valid neutral delivery receipt" in MODULE.validate(
        receipt, "ready", workspace_root=tmp_path,
    )


def test_terminal_promotion_requires_release_gate_and_active_observation(tmp_path):
    delivery = write_accepted_document_delivery(tmp_path)
    receipt = complete_receipt()
    bind_accepted_artifact(receipt, delivery)
    assert "terminal promotion requires canonical observing state and approved release gate" in MODULE.validate(
        receipt, "complete", workspace_root=tmp_path,
    )

    delivery = write_accepted_document_delivery(tmp_path, status="observing")
    bind_accepted_artifact(receipt, delivery)
    assert MODULE.validate(receipt, "complete", workspace_root=tmp_path) == []

    delivery["observation"]["status"] = "planned"
    (tmp_path / "RUN.json").write_text(json.dumps(delivery))
    assert "terminal promotion requires active or passing canonical observation" in MODULE.validate(
        receipt, "complete", workspace_root=tmp_path,
    )


def test_release_receipt_reference_cannot_escape_verification_root(tmp_path):
    workspace = tmp_path / "workspace"
    outside = tmp_path / "outside"
    workspace.mkdir()
    outside.mkdir()
    delivery = write_accepted_document_delivery(workspace)
    outside_receipt = outside / "RUN.json"
    (workspace / "RUN.json").replace(outside_receipt)

    receipt = valid_receipt()
    bind_accepted_artifact(receipt, delivery)
    receipt["artifact"]["acceptance_receipt"] = str(outside_receipt)
    assert "artifact.acceptance_receipt must remain inside the verification root" in MODULE.validate(
        receipt, "ready", base_dir=workspace, workspace_root=workspace,
    )

    symlink = workspace / "outside-run.json"
    symlink.symlink_to(outside_receipt)
    receipt["artifact"]["acceptance_receipt"] = symlink.name
    assert "artifact.acceptance_receipt must remain inside the verification root" in MODULE.validate(
        receipt, "ready", base_dir=workspace, workspace_root=workspace,
    )


def test_complete_gate_requires_target_visible_proof():
    receipt = complete_receipt()
    assert validate_policy(receipt, "complete") == []
    receipt["proof"]["checks"].pop()
    assert "complete promotion must pass every proof requirement" in validate_policy(receipt, "complete")


def test_terminal_evidence_cannot_postdate_its_receipt():
    receipt = complete_receipt()
    receipt["proof"]["checks"][0]["observed_at"] = "2026-07-11T04:06:00Z"
    assert "proof.checks[0] postdates the terminal receipt" in validate_policy(receipt, "complete")


def test_public_promotion_requires_public_and_irreversible_authority():
    receipt = valid_receipt()
    receipt["action_type"] = "publish"
    receipt["target"] = {
        "id": "audience:public-web", "kind": "audience",
        "environment_tier": "not-applicable", "disclosure": "public",
    }
    receipt["release_authority"]["action_types"] = ["publish"]
    receipt["release_authority"]["target_ids"] = ["audience:public-web"]
    errors = validate_policy(receipt, "ready")
    assert "public target requires explicit public disclosure authority" in errors
    assert "public promotion must acknowledge irreversible redistribution risk" in errors

    receipt["release_authority"].update({"public_disclosure": True, "irreversible_action": True})
    receipt["reversal"].update({
        "mode": "revoke",
        "irreversible": True,
        "limitations": "cached or copied publications cannot be recalled",
    })
    assert validate_policy(receipt, "ready") == []


def test_command_mode_requires_token_bound_command_authority():
    receipt = valid_receipt()
    receipt["action_type"] = "deploy"
    receipt["target"] = {
        "id": "production", "kind": "environment",
        "environment_tier": "production", "disclosure": "internal",
    }
    receipt["release_authority"].update({
        "action_types": ["deploy"],
        "target_ids": ["production"],
        "target_environment_tiers": ["production"],
        "allowed_execution_modes": ["command"],
        "allowed_operations": ["deploy-production"],
        "external_communication": False,
    })
    receipt["reversal"].update({"mode": "rollback", "tested": True})
    assert "command execution requires safe allowed_command_prefixes" in validate_policy(receipt, "ready")
    receipt["release_authority"]["allowed_command_prefixes"] = [["deploy", "--target", "production"]]
    assert validate_policy(receipt, "ready") == []


def test_command_execution_rejects_unsafe_shape_and_invalid_lifecycle_fields():
    receipt = complete_receipt()
    receipt["release_authority"].update({
        "allowed_execution_modes": ["command"],
        "allowed_operations": ["deploy"],
        "allowed_command_prefixes": [["deploy"]],
    })
    receipt["execution"]["operations"][0].update({
        "mode": "command",
        "operation": "delete-production",
        "command": "deploy-malware; rm -rf /",
        "exit_code": False,
        "started_at": "2026-07-11T04:03:00Z",
        "finished_at": "2026-07-11T04:02:00Z",
    })

    errors = validate_policy(receipt, "complete")
    assert "execution.operations[0].operation is outside promotion authority" in errors
    assert "execution.operations[0].command must be argv or shell-free text" in errors
    assert "execution.operations[0].exit_code must be an integer" in errors
    assert "execution.operations[0] cannot finish before it starts" in errors


def test_command_execution_enforces_token_prefix_and_approval_order():
    receipt = complete_receipt()
    receipt["release_authority"].update({
        "allowed_execution_modes": ["command"],
        "allowed_operations": ["deploy"],
        "allowed_command_prefixes": [["deploy"]],
    })
    receipt["execution"]["operations"][0].update({
        "mode": "command",
        "operation": "deploy",
        "command": ["deploy-malware"],
        "exit_code": 0,
        "started_at": "2026-07-11T04:00:00Z",
        "finished_at": "2026-07-11T04:03:00Z",
    })

    errors = validate_policy(receipt, "complete")
    assert "execution.operations[0].command is outside promotion authority" in errors
    assert "promotion execution cannot start before human approval" in errors


def test_production_tier_does_not_depend_on_literal_target_id():
    receipt = valid_receipt()
    receipt["action_type"] = "deploy"
    receipt["target"] = {
        "id": "provider:prod-au", "kind": "environment",
        "environment_tier": "production", "disclosure": "internal",
    }
    receipt["release_authority"].update({
        "action_types": ["deploy"],
        "target_ids": ["provider:prod-au"],
        "target_environment_tiers": ["production"],
        "external_communication": False,
    })
    receipt["reversal"].update({"mode": "rollback", "tested": False})
    assert "production environment reversal must be tested" in validate_policy(receipt, "ready")
    receipt["reversal"]["tested"] = True
    assert validate_policy(receipt, "ready") == []


def test_literal_production_id_does_not_override_nonproduction_tier():
    receipt = valid_receipt()
    receipt["action_type"] = "deploy"
    receipt["target"] = {
        "id": "production", "kind": "environment",
        "environment_tier": "staging", "disclosure": "internal",
    }
    receipt["release_authority"].update({
        "action_types": ["deploy"],
        "target_ids": ["production"],
        "target_environment_tiers": ["staging"],
        "external_communication": False,
    })
    receipt["reversal"].update({"mode": "rollback", "tested": False})
    errors = validate_policy(receipt, "ready")
    assert "production environment reversal must be tested" not in errors
    assert errors == []


def test_environment_tier_must_be_explicitly_authorised():
    receipt = valid_receipt()
    receipt["action_type"] = "deploy"
    receipt["target"] = {
        "id": "provider:prod-au", "kind": "environment",
        "environment_tier": "production", "disclosure": "internal",
    }
    receipt["release_authority"].update({
        "action_types": ["deploy"],
        "target_ids": ["provider:prod-au"],
        "target_environment_tiers": ["staging"],
        "external_communication": False,
    })
    receipt["reversal"].update({"mode": "rollback", "tested": True})
    assert (
        "release_authority does not include target environment tier"
        in validate_policy(receipt, "ready")
    )


def test_malformed_tier_and_impact_enums_fail_closed_without_crashing():
    receipt = valid_receipt()
    receipt["action_type"] = ["deploy"]
    receipt["target"]["kind"] = ["environment"]
    receipt["target"]["environment_tier"] = ["production"]
    receipt["target"]["disclosure"] = ["internal"]
    receipt["change_impact"]["state_change"] = ["destructive"]
    receipt["change_impact"]["compatibility"] = ["non-backward-compatible"]
    receipt["reversal"]["mode"] = ["rollback"]
    receipt["release_authority"]["allowed_execution_modes"] = [{}]
    receipt["release_authority"]["secrets_access"] = []
    errors = validate_policy(receipt, "ready")
    assert "action_type is invalid" in errors
    assert "target.kind is invalid" in errors
    assert "target.disclosure is invalid" in errors
    assert "change_impact.state_change is invalid" in errors
    assert "change_impact.compatibility is invalid" in errors
    assert "reversal.mode is invalid" in errors
    assert "release_authority.allowed_execution_modes is invalid" in errors
    assert "release_authority.secrets_access is invalid" in errors


def test_malformed_terminal_operation_and_proof_enums_fail_closed():
    receipt = complete_receipt()
    receipt["execution"]["operations"][0]["mode"] = ["connector"]
    receipt["execution"]["operations"][0]["status"] = ["succeeded"]
    receipt["proof"]["checks"][0]["requirement_id"] = ["delivered"]
    receipt["proof"]["checks"][0]["status"] = ["pass"]
    errors = validate_policy(receipt, "complete")
    assert "execution.operations[0].mode is outside promotion authority" in errors
    assert "execution.operations[0].status is invalid" in errors
    assert "proof.checks[0] must reference one unique requirement" in errors
    assert "proof.checks[0].status is invalid" in errors


def test_readiness_evidence_cannot_postdate_receipt():
    receipt = valid_receipt()
    receipt["readiness_checks"][0]["checked_at"] = "2099-01-01T00:00:00Z"
    assert (
        "readiness_checks[0].checked_at cannot postdate the receipt"
        in validate_policy(receipt, "ready")
    )


def test_generic_passing_check_cannot_masquerade_as_change_or_recovery_evidence():
    receipt = valid_receipt()
    receipt["release_authority"]["irreversible_action"] = True
    receipt["change_impact"] = {
        "state_change": "destructive",
        "compatibility": "backward-compatible",
        "ordered_steps": ["migrate", "verify"],
        "compatibility_window": "not-applicable",
        "recovery_point": "recipient-verified",
        "readiness_evidence": ["recipient-verified"],
    }
    errors = validate_policy(receipt, "ready")
    assert "change_impact readiness evidence must reference purpose-typed passing checks" in errors
    assert "state change requires passing state-change readiness evidence" in errors
    assert "change_impact recovery point must reference a passing recovery check" in errors


def test_stateful_change_requires_real_sequence_recovery_and_evidence():
    receipt = valid_receipt()
    receipt["change_impact"] = {
        "state_change": "reversible",
        "compatibility": "backward-compatible",
        "ordered_steps": ["N/A"],
        "compatibility_window": "not-applicable",
        "recovery_point": [],
        "readiness_evidence": ["TBD"],
    }
    errors = validate_policy(receipt, "ready")
    assert "stateful change requires non-empty ordered change_impact steps" in errors
    assert "stateful change requires non-empty change_impact readiness evidence" in errors
    assert "stateful change requires a verified recovery point" in errors

    receipt["change_impact"].update({
        "ordered_steps": ["write the new state", "verify readers"],
        "recovery_point": "tested snapshot restore-20260711",
        "readiness_evidence": ["migration-rehearsal", "restore-test"],
    })
    errors = validate_policy(receipt, "ready")
    assert "change_impact readiness evidence must reference purpose-typed passing checks" in errors
    assert "change_impact recovery point must reference a passing recovery check" in errors
    receipt["readiness_checks"].extend([
        {
            "id": "state-change-ready",
            "purpose": "state-change",
            "status": "pass",
            "evidence": ["migration-rehearsal"],
            "checked_at": "2026-07-11T03:59:30Z",
        },
        {
            "id": "recovery-ready",
            "purpose": "recovery",
            "status": "pass",
            "evidence": ["restore-test", "tested snapshot restore-20260711"],
            "checked_at": "2026-07-11T03:59:30Z",
        },
    ])
    assert validate_policy(receipt, "ready") == []


def test_non_backward_compatible_change_requires_window_and_authority():
    receipt = valid_receipt()
    receipt["change_impact"] = {
        "state_change": "none",
        "compatibility": "non-backward-compatible",
        "ordered_steps": ["switch writers", "switch readers"],
        "compatibility_window": "not-applicable",
        "recovery_point": "tested snapshot restore-20260711",
        "readiness_evidence": ["mixed-version-rehearsal", "restore-test"],
    }
    errors = validate_policy(receipt, "ready")
    assert "non-backward-compatible change requires a compatibility window" in errors
    assert (
        "destructive or non-backward-compatible change requires explicit irreversible-action authority"
        in errors
    )

    receipt["change_impact"]["compatibility_window"] = "old readers supported for 24h"
    receipt["release_authority"]["irreversible_action"] = True
    receipt["readiness_checks"].extend([
        {
            "id": "compatibility-ready",
            "purpose": "compatibility",
            "status": "pass",
            "evidence": ["mixed-version-rehearsal"],
            "checked_at": "2026-07-11T03:59:30Z",
        },
        {
            "id": "recovery-ready",
            "purpose": "recovery",
            "status": "pass",
            "evidence": ["restore-test", "tested snapshot restore-20260711"],
            "checked_at": "2026-07-11T03:59:30Z",
        },
    ])
    assert validate_policy(receipt, "ready") == []


def test_destructive_migration_keeps_global_compatibility_and_recovery_gates():
    receipt = valid_receipt()
    receipt["action_type"] = "deploy"
    receipt["target"] = {
        "id": "provider:prod-au", "kind": "environment",
        "environment_tier": "production", "disclosure": "internal",
    }
    receipt["release_authority"].update({
        "action_types": ["deploy"],
        "target_ids": ["provider:prod-au"],
        "target_environment_tiers": ["production"],
        "external_communication": False,
        "irreversible_action": False,
    })
    receipt["reversal"].update({"mode": "rollback", "tested": True})
    receipt["change_impact"] = {
        "state_change": "destructive",
        "compatibility": "non-backward-compatible",
        "ordered_steps": ["expand", "migrate", "contract"],
        "compatibility_window": "old and new readers supported through the observation window",
        "recovery_point": "verified snapshot restore-20260711",
        "readiness_evidence": ["migration rehearsal", "restore test"],
    }
    errors = validate_policy(receipt, "ready")
    assert (
        "destructive or non-backward-compatible change requires explicit irreversible-action authority"
        in errors
    )
    receipt["release_authority"]["irreversible_action"] = True
    receipt["readiness_checks"].extend([
        {
            "id": "migration-ready",
            "purpose": "state-change",
            "status": "pass",
            "evidence": ["migration rehearsal"],
            "checked_at": "2026-07-11T03:59:30Z",
        },
        {
            "id": "compatibility-ready",
            "purpose": "compatibility",
            "status": "pass",
            "evidence": ["migration rehearsal"],
            "checked_at": "2026-07-11T03:59:30Z",
        },
        {
            "id": "recovery-ready",
            "purpose": "recovery",
            "status": "pass",
            "evidence": ["restore test", "verified snapshot restore-20260711"],
            "checked_at": "2026-07-11T03:59:30Z",
        },
    ])
    assert validate_policy(receipt, "ready") == []


def test_reversed_outcome_requires_authorised_reversal_and_restoration_proof():
    receipt = complete_receipt()
    receipt["status"] = "reversed"
    receipt["execution"]["operations"][0]["status"] = "failed"
    receipt["proof"]["checks"] = []
    receipt["reversal_execution"] = {
        "operations": [{
            "mode": "connector",
            "operation": "recall-delivery",
            "actor": "promotion-owner",
            "status": "succeeded",
            "evidence": ["provider-recall-id"],
            "started_at": "2026-07-11T04:04:00Z",
            "finished_at": "2026-07-11T04:04:30Z",
        }],
        "checks": [{
            "id": "recall-confirmed",
            "status": "pass",
            "evidence": ["provider-recall-confirmation"],
            "checked_at": "2026-07-11T04:04:40Z",
        }],
    }
    receipt["outcome"] = {
        "status": "reversed",
        "evidence": ["recall confirmed"],
        "follow_up_owner": "promotion-owner",
    }
    assert validate_policy(receipt, "complete") == []
    receipt["reversal_execution"]["operations"][0]["started_at"] = "2026-07-11T04:02:30Z"
    assert (
        "reversal execution cannot start before promotion execution finishes"
        in validate_policy(receipt, "complete")
    )


def test_reversal_proof_must_follow_reversal_and_fit_receipt_time():
    receipt = complete_receipt()
    receipt["status"] = "reversed"
    receipt["execution"]["operations"][0]["status"] = "failed"
    receipt["proof"]["checks"] = []
    receipt["reversal_execution"] = {
        "operations": [{
            "mode": "connector",
            "operation": "recall-delivery",
            "actor": "promotion-owner",
            "status": "succeeded",
            "evidence": ["provider-recall-id"],
            "started_at": "2026-07-11T04:04:00Z",
            "finished_at": "2026-07-11T04:04:30Z",
        }],
        "checks": [{
            "id": "recall-confirmed",
            "status": "pass",
            "evidence": ["provider-recall-confirmation"],
            "checked_at": "2026-07-11T03:00:00Z",
        }],
    }
    receipt["outcome"] = {
        "status": "reversed",
        "evidence": ["recall confirmed"],
        "follow_up_owner": "promotion-owner",
    }
    assert "reversal_execution.checks[0] predates reversal completion" in validate_policy(receipt, "complete")

    receipt["reversal_execution"]["checks"][0]["checked_at"] = "2026-07-11T04:06:00Z"
    assert "reversal_execution.checks[0] postdates the terminal receipt" in validate_policy(receipt, "complete")

    receipt["reversal_execution"]["checks"][0]["checked_at"] = "2026-07-11T04:04:40Z"
    receipt["reversal"]["mode"] = "none"
    assert "reversed outcome requires an actionable reversal mode" in validate_policy(receipt, "complete")
