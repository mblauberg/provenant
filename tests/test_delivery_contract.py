import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR_PATH = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"
REFERENCE_RUNS_PATH = ROOT / "skills" / "deliver" / "scripts" / "reference_runs.py"
REFERENCE_EVALUATION_PATH = ROOT / "skills" / "deliver" / "scripts" / "reference_evaluation.py"
RUN_TEMPLATE_PATH = ROOT / "skills" / "deliver" / "templates" / "RUN.template.json"
AUTHORITY_MAPPER_PATH = ROOT / "skills" / "deliver" / "scripts" / "authority_mapping.py"
POLICY_VALIDATOR_PATH = ROOT / "skills" / "deliver" / "scripts" / "delivery_policy_validation.py"
AUTHORITY_FIXTURE_ROOT = ROOT / "tests" / "fixtures" / "authority-envelope-v2"


def load_validator():
    spec = importlib.util.spec_from_file_location("validate_delivery", VALIDATOR_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def fixture(profile="software", workspace_root=None, **materialise_kwargs):
    module = load(REFERENCE_RUNS_PATH, f"reference_runs_{profile}")
    run = module.make_reference_run(profile, ROOT)
    if workspace_root is not None:
        materialiser = load(REFERENCE_EVALUATION_PATH, f"reference_evaluation_{profile}")
        materialiser.materialise_reference_run(
            run, workspace_root, ROOT, **materialise_kwargs,
        )
    return run


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def delivery_authority_fixture():
    authority = json.loads((AUTHORITY_FIXTURE_ROOT / "delivery-authority.json").read_text())
    fields = (
        "schema_version", "approved_by", "evidence", "evidence_digest",
        "workspace_roots", "allowed_source_paths", "allowed_artifact_paths",
        "denied_paths", "prohibited_actions", "disclosure", "secrets_access",
        "secret_refs", "deployment", "deployment_targets",
        "irreversible_actions", "irreversible_action_ids", "network",
        "expires_at", "budget", "delegations",
    )
    return {field: authority[field] for field in fields}


def mapped_authority_fixture():
    authority = json.loads((AUTHORITY_FIXTURE_ROOT / "fabric-authority.json").read_text())
    fields = (
        "schemaVersion", "approval", "workspaceRoots", "sourcePaths",
        "artifactPaths", "deniedPaths", "prohibitedActions", "disclosure",
        "secrets", "deployment", "irreversibleActions", "network",
        "expiresAt", "budget",
    )
    return {field: authority[field] for field in fields}


def terminalise_reference_evaluation(run, status="failed"):
    binding = run["assurance"]["evaluations"][0]
    evidence_id = f"evaluation-{status}-receipt"
    binding.update({
        "status": status,
        "evaluation_id": f"EVAL-{status.upper()}",
        "evidence_id": evidence_id,
    })
    run["evidence"].append({
        "id": evidence_id,
        "kind": "deterministic",
        "gate": f"evaluation-{status}",
        "status": "pass",
        "method": "canonical terminal receipt validation",
        "artifact_id": binding["evaluation_artifact_id"],
        "source_paths": ["input"],
        "result": {
            "exit_code": 0,
            "receipt_digest": "sha256:" + "f" * 64,
        },
    })
    return binding


def with_repair_cycles(candidate, cycles):
    candidate["state_history"] = candidate["state_history"][:-1]
    minute = 6
    for _ in range(cycles):
        candidate["state_history"].extend([
            {
                "state": "repairing",
                "at": f"2026-07-10T00:{minute:02d}:00Z",
                "evidence_ids": ["tests"],
            },
            {
                "state": "verifying",
                "at": f"2026-07-10T00:{minute + 1:02d}:00Z",
                "evidence_ids": ["tests"],
            },
            {
                "state": "reviewing",
                "at": f"2026-07-10T00:{minute + 2:02d}:00Z",
                "evidence_ids": ["tests"],
            },
        ])
        minute += 3
    candidate["state_history"].append({
        "state": "awaiting_acceptance",
        "at": f"2026-07-10T00:{minute:02d}:00Z",
        "evidence_ids": [item["id"] for item in candidate["evidence"]],
    })
    candidate["repair_cycles"] = cycles
    return candidate


def at_risk_tier(candidate, risk_tier):
    candidate["risk_tier"] = risk_tier
    if risk_tier == "routine":
        candidate["risk_assessment"] = {
            "blast_radius": "local",
            "reversibility": "easy",
            "data_sensitivity": "public",
            "migration": "none",
            "oracle_quality": "strong",
            "external_effects": "none",
            "critical_surface": "none",
        }
    if risk_tier == "terminal":
        targeted = next(
            review for review in candidate["reviews"]
            if review["role"] == "targeted"
        )
        targeted["lenses"].extend(["adversarial"])
    return candidate


def test_delivery_authority_v2_maps_to_the_cross_language_golden():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping")
    delivery = delivery_authority_fixture()
    expected = mapped_authority_fixture()

    actual = module.map_delivery_authority(delivery)
    def canonical(value):
        return json.dumps(value, sort_keys=True, separators=(",", ":"))
    assert canonical(actual) == canonical(expected)


def test_delivery_authority_timestamp_validation_matches_protocol_vectors():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_timestamp_vectors")
    delivery = delivery_authority_fixture()
    vectors = json.loads((AUTHORITY_FIXTURE_ROOT / "timestamp-vectors.json").read_text())

    for timestamp in vectors["accepted"]:
        delivery["expires_at"] = timestamp
        mapped = module.map_delivery_authority(delivery)
        assert mapped["expiresAt"] == timestamp

    for timestamp in vectors["rejected"]:
        delivery["expires_at"] = timestamp
        with pytest.raises(module.AuthorityMappingError, match="strict RFC3339 timestamp"):
            module.map_delivery_authority(delivery)

    authority = mapped_authority_fixture()
    for vector in vectors["ordering"]:
        child = {**authority, "expiresAt": vector["child"]}
        parent = {**authority, "expiresAt": vector["parent"]}
        assert module.authority_contained(child, parent) is vector["contained"]


def test_delivery_authority_v2_is_closed_and_rejects_invalid_budget_units():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_closed")
    delivery = delivery_authority_fixture()

    missing = copy.deepcopy(delivery)
    del missing["network"]
    with pytest.raises(module.AuthorityMappingError, match="missing=.*network"):
        module.map_delivery_authority(missing)

    unknown = copy.deepcopy(delivery)
    unknown["implicit_default"] = True
    with pytest.raises(module.AuthorityMappingError, match="unknown=.*implicit_default"):
        module.map_delivery_authority(unknown)

    unknown_currency = copy.deepcopy(delivery)
    unknown_currency["budget"] = {"cost:ZZZ": 1}
    with pytest.raises(module.AuthorityMappingError, match="invalid budget unit"):
        module.map_delivery_authority(unknown_currency)


@pytest.mark.parametrize(
    "budget",
    [
        {"turns": -1},
        {"turns": 2**53},
        {
            **{f"input_tokens:provider-{index}": 1 for index in range(123)},
            **{unit: 1 for unit in ("turns", "provider_calls", "concurrent_turns", "descendants", "message_bytes", "artifact_bytes")},
        },
    ],
)
def test_delivery_authority_budget_matches_canonical_bounds(budget):
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_budget_bounds")
    delivery = delivery_authority_fixture()
    delivery["budget"] = budget

    with pytest.raises(module.AuthorityMappingError, match="budget"):
        module.map_delivery_authority(delivery)


def test_delivery_authority_budget_accepts_canonical_128_key_boundary():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_budget_boundary")
    delivery = delivery_authority_fixture()
    delivery["budget"] = {
        **{f"input_tokens:provider-{index}": 1 for index in range(122)},
        **{unit: 1 for unit in ("turns", "provider_calls", "concurrent_turns", "descendants", "message_bytes", "artifact_bytes")},
    }
    mapped = module.map_delivery_authority(delivery)
    assert len(mapped["budget"]) == 128


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (lambda authority: authority.update(workspace_roots=[f"root-{index}" for index in range(65)]), "workspace_roots"),
        (lambda authority: authority.update(allowed_source_paths=[f"src-{index}" for index in range(257)]), "allowed_source_paths"),
        (lambda authority: authority.update(allowed_artifact_paths=[f"artifact-{index}" for index in range(257)]), "allowed_artifact_paths"),
        (lambda authority: authority.update(denied_paths=[f"denied-{index}" for index in range(257)]), "denied_paths"),
        (lambda authority: authority.update(prohibited_actions=[f"action-{index}" for index in range(257)]), "prohibited_actions"),
        (lambda authority: authority.update(secret_refs=[f"secret-{index}" for index in range(257)]), "secret_refs"),
        (lambda authority: authority.update(deployment_targets=[f"target-{index}" for index in range(257)]), "deployment_targets"),
        (lambda authority: authority.update(irreversible_action_ids=[f"action-{index}" for index in range(257)]), "irreversible_action_ids"),
        (lambda authority: authority["network"].update(allowed_hosts=[f"host-{index}.test" for index in range(257)]), "allowed_hosts"),
        (lambda authority: authority.update(allowed_source_paths=["input\0escape"]), "allowed_source_paths"),
        (lambda authority: authority.update(allowed_source_paths=["x" * 4097]), "allowed_source_paths"),
        (lambda authority: authority.update(expires_at="2026-07-20T00:00Z"), "expires_at"),
        (lambda authority: authority.update(expires_at="2026-02-30T00:00:00Z"), "expires_at"),
        (lambda authority: authority.update(workspace_roots=["workspace"], allowed_source_paths=["outside"]), "allowed_source_paths"),
        (
            lambda authority: authority.update(
                workspace_roots=["workspace"],
                allowed_source_paths=["workspace/input"],
                allowed_artifact_paths=["outside"],
            ),
            "allowed_artifact_paths",
        ),
    ],
)
def test_delivery_authority_mapper_rejects_the_canonical_codec_negative_boundaries(mutate, message):
    module = load(AUTHORITY_MAPPER_PATH, f"authority_mapping_parity_{message}")
    delivery = delivery_authority_fixture()
    mutate(delivery)

    with pytest.raises(module.AuthorityMappingError, match=message):
        module.map_delivery_authority(delivery)


def test_complete_delivery_delegation_inherits_approval_and_must_narrow_every_dimension():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_delegation")
    delivery = delivery_authority_fixture()
    delegation = {
        "actor": "worker-1",
        "schema_version": 2,
        "workspace_roots": ["."],
        "allowed_source_paths": ["input"],
        "allowed_artifact_paths": ["output/worker-1"],
        "denied_paths": [".git"],
        "prohibited_actions": ["deployment", "external-release"],
        "disclosure": "local-only",
        "secrets_access": "none",
        "secret_refs": [],
        "deployment": False,
        "deployment_targets": [],
        "irreversible_actions": False,
        "irreversible_action_ids": [],
        "network": {"tool_egress": "none", "allowed_hosts": []},
        "expires_at": "2026-07-19T00:00:00Z",
        "budget": {"turns": 2},
    }
    delivery["delegations"] = [delegation]

    mapped = module.map_delivery_delegations(delivery)
    parent = module.map_delivery_authority(delivery)
    assert len(mapped) == 1
    assert mapped[0]["actor"] == "worker-1"
    assert mapped[0]["authority"]["artifactPaths"] == ["output/worker-1"]
    assert mapped[0]["authority"]["approval"] == parent["approval"]

    widened = copy.deepcopy(delivery)
    widened["delegations"][0]["network"] = {
        "tool_egress": "allowlist",
        "allowed_hosts": ["api.example.test", "outside.example.test"],
    }
    with pytest.raises(module.AuthorityMappingError, match="broadens parent authority"):
        module.map_delivery_delegations(widened)

    widened_budget = copy.deepcopy(delivery)
    widened_budget["delegations"][0]["budget"] = {"wall_clock_milliseconds": 1}
    with pytest.raises(module.AuthorityMappingError, match="broadens parent authority"):
        module.map_delivery_delegations(widened_budget)


def test_review_delegation_separates_source_reads_and_artifacts():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_review_delegation")
    delivery = delivery_authority_fixture()
    delivery["delegations"] = [{
        "actor": "reviewer-1",
        "schema_version": 2,
        "workspace_roots": ["."],
        "allowed_source_paths": ["input"],
        "allowed_artifact_paths": ["output/reviews/reviewer-1"],
        "denied_paths": [".git"],
        "prohibited_actions": ["deployment", "external-release"],
        "disclosure": "approved-providers",
        "secrets_access": "none",
        "secret_refs": [],
        "deployment": False,
        "deployment_targets": [],
        "irreversible_actions": False,
        "irreversible_action_ids": [],
        "network": {"tool_egress": "none", "allowed_hosts": []},
        "expires_at": "2026-07-19T00:00:00Z",
        "budget": {"turns": 2},
    }]

    mapped = module.map_delivery_delegations(delivery)[0]["authority"]

    assert mapped["sourcePaths"] == ["input"]
    assert mapped["artifactPaths"] == ["output/reviews/reviewer-1"]
    assert set(mapped["sourcePaths"]).isdisjoint(mapped["artifactPaths"])


def test_authority_evidence_digest_must_bind_the_linked_approval_artifact():
    module = load_validator()
    candidate = fixture()
    candidate["authority"]["evidence_digest"] = "sha256:" + "c" * 64

    with pytest.raises(module.Invalid, match="evidence_digest must bind"):
        module.validate(candidate, ROOT)


def test_reference_run_for_every_profile_passes(tmp_path):
    module = load_validator()
    for profile in ("software", "research", "analysis", "document", "agent-product"):
        workspace_root = tmp_path / profile
        module.validate(
            fixture(profile, workspace_root), ROOT,
            workspace_root=workspace_root, verify_hashes=True,
        )


def test_delivery_v1_records_typed_fabric_relationships_without_breaking_legacy_receipts():
    module = load_validator()
    candidate = fixture()
    assert candidate["fabric_relationships"] == {
        "mode": "independent",
        "delivery_run_id": candidate["run_id"],
        "project_session_id": "not_applicable",
        "coordination_run_id": "not_applicable",
        "workstream_id": "not_applicable",
        "lead_agent_id": "not_applicable",
    }
    module.validate(candidate, ROOT)

    legacy = copy.deepcopy(candidate)
    del legacy["fabric_relationships"]
    module.validate(legacy, ROOT)

    coordinated = copy.deepcopy(candidate)
    coordinated["fabric_relationships"] = {
        "mode": "coordinated",
        "delivery_run_id": coordinated["run_id"],
        "project_session_id": "ps_01",
        "coordination_run_id": "run_01",
        "workstream_id": "workstream_01",
        "lead_agent_id": "agent_lead_01",
    }
    module.validate(coordinated, ROOT)


def test_run_template_declares_an_explicit_independent_fabric_relationship_state():
    template = json.loads(RUN_TEMPLATE_PATH.read_text())
    assert template["fabric_relationships"] == {
        "mode": "independent",
        "delivery_run_id": template["run_id"],
        "project_session_id": "not_applicable",
        "coordination_run_id": "not_applicable",
        "workstream_id": "not_applicable",
        "lead_agent_id": "not_applicable",
    }


@pytest.mark.parametrize(
    ("relationships", "message"),
    [
        (None, "must be an object"),
        (
            {
                "mode": "coordinated",
                "delivery_run_id": "REF-SOFTWARE",
                "project_session_id": "ps_01",
                "coordination_run_id": "run_01",
                "workstream_id": "workstream_01",
            },
            "fields are invalid",
        ),
        (
            {
                "mode": "coordinated",
                "delivery_run_id": "REF-SOFTWARE",
                "project_session_id": "ps_01",
                "coordination_run_id": "run_01",
                "workstream_id": "workstream_01",
                "lead_agent_id": "agent_lead_01",
                "chair_agent_id": "agent_chair_01",
            },
            "fields are invalid",
        ),
        (
            {
                "mode": "isolated",
                "delivery_run_id": "REF-SOFTWARE",
                "project_session_id": "not_applicable",
                "coordination_run_id": "not_applicable",
                "workstream_id": "not_applicable",
                "lead_agent_id": "not_applicable",
            },
            "mode is invalid",
        ),
        (
            {
                "mode": "coordinated",
                "delivery_run_id": "REF-SOFTWARE",
                "project_session_id": "ps_01",
                "coordination_run_id": "run_01",
                "workstream_id": "not_applicable",
                "lead_agent_id": "agent_lead_01",
            },
            "coordinated relationships require",
        ),
        (
            {
                "mode": "independent",
                "delivery_run_id": "REF-SOFTWARE",
                "project_session_id": "ps_invented",
                "coordination_run_id": "not_applicable",
                "workstream_id": "not_applicable",
                "lead_agent_id": "not_applicable",
            },
            "independent relationships must be explicit not_applicable",
        ),
        (
            {
                "mode": "coordinated",
                "delivery_run_id": "ANOTHER-DELIVERY",
                "project_session_id": "ps_01",
                "coordination_run_id": "run_01",
                "workstream_id": "workstream_01",
                "lead_agent_id": "agent_lead_01",
            },
            "delivery_run_id must match run_id",
        ),
        (
            {
                "mode": "coordinated",
                "delivery_run_id": "REF-SOFTWARE",
                "project_session_id": "project session with spaces",
                "coordination_run_id": "run_01",
                "workstream_id": "workstream_01",
                "lead_agent_id": "agent_lead_01",
            },
            "bounded stable identifier",
        ),
    ],
)
def test_fabric_relationships_reject_partial_unknown_or_cross_inconsistent_forms(
    relationships, message,
):
    module = load_validator()
    candidate = fixture()
    candidate["fabric_relationships"] = relationships
    with pytest.raises(module.Invalid, match=message):
        module.validate(candidate, ROOT)


def test_approved_intent_requires_bound_artifact_digest_owner_and_evidence():
    module = load_validator()
    candidate = fixture()
    for path in (
        ("intent", "digest"),
        ("intent", "decision_owner"),
        ("intent", "approval", "approver"),
        ("intent", "approval", "evidence"),
    ):
        broken = copy.deepcopy(candidate)
        target = broken
        for key in path[:-1]:
            target = target[key]
        target[path[-1]] = ""
        with pytest.raises(module.Invalid, match="intent"):
            module.validate(broken, ROOT)


def test_live_hash_check_rejects_artifact_changed_after_approval(tmp_path):
    module = load_validator()
    candidate = fixture()
    (tmp_path / "intent.md").write_text("changed")
    candidate["intent"]["artifact"] = "intent.md"
    candidate["artifacts"][0]["path"] = "intent.md"
    with pytest.raises(module.Invalid, match="digest does not match"):
        module.validate(candidate, ROOT, receipt_dir=tmp_path, verify_hashes=True)


def test_state_history_cannot_jump_a_mandatory_gate():
    module = load_validator()
    candidate = fixture()
    candidate["state_history"] = [
        {"state": "draft", "at": "2026-07-10T00:00:00Z", "evidence_ids": []},
        {"state": "executing", "at": "2026-07-10T00:01:00Z", "evidence_ids": []},
        {"state": "awaiting_acceptance", "at": "2026-07-10T00:02:00Z", "evidence_ids": ["tests"]},
    ]
    with pytest.raises(module.Invalid, match="invalid lifecycle transition"):
        module.validate(candidate, ROOT)


def test_history_must_start_at_draft_and_honest_draft_needs_no_future_results():
    module = load_validator()
    candidate = fixture()
    candidate["state_history"] = [candidate["state_history"][-1]]
    with pytest.raises(module.Invalid, match="start at draft"):
        module.validate(candidate, ROOT)

    candidate = fixture()
    candidate["status"] = "draft"
    candidate["state_history"] = [candidate["state_history"][0]]
    candidate["intent"]["approval"] = {"status": "pending", "approver": "", "evidence": ""}
    candidate["design"]["status"] = "draft"
    candidate["evidence"] = [item for item in candidate["evidence"] if item["kind"] == "human"]
    candidate["reviews"] = []
    candidate["measures"] = {"outcome": [], "trajectory": []}
    candidate["assurance"] = {"stochastic_required": False, "reason": "not decided", "evaluations": []}
    candidate["security"] = {"status": "pending", "reason": "", "policy_sha256": candidate["security"]["policy_sha256"], "changed_surfaces": [], "checks": [], "agentic_risks": []}
    module.validate(candidate, ROOT)


def test_recoverable_side_state_records_its_own_resume_contract():
    module = load_validator()
    candidate = fixture()
    candidate["status"] = "executing"
    candidate["state_history"] = candidate["state_history"][:4] + [
        {"state": "blocked", "at": "2026-07-10T00:04:00Z", "evidence_ids": [], "reason": "provider unavailable", "recovery": "retry local route", "resume_state": "executing"},
        {"state": "executing", "at": "2026-07-10T00:05:00Z", "evidence_ids": []},
    ]
    candidate["evidence"] = [item for item in candidate["evidence"] if item["kind"] == "human"]
    candidate["reviews"] = []
    candidate["measures"] = {"outcome": [], "trajectory": []}
    candidate["security"]["status"] = "pending"
    for check in candidate["security"]["checks"]:
        check["status"] = "pending"
        check["evidence_id"] = ""
    module.validate(candidate, ROOT)

    jumped = fixture()
    jumped["status"] = "blocked"
    jumped["state_history"].append({
        "state": "blocked", "at": "2026-07-10T00:09:00Z", "evidence_ids": [],
        "reason": "blocked", "recovery": "resume review", "resume_state": "closed",
    })
    jumped["degradation"] = {"reason": "blocked", "recovery": "resume review"}
    with pytest.raises(module.Invalid, match="resume the state"):
        module.validate(jumped, ROOT)


def test_repair_cycle_count_must_match_history():
    module = load_validator()
    candidate = fixture()
    candidate["state_history"] = candidate["state_history"][:-1] + [
        {"state": "repairing", "at": "2026-07-10T00:06:30Z", "evidence_ids": ["tests"]},
        {"state": "verifying", "at": "2026-07-10T00:07:00Z", "evidence_ids": ["tests"]},
        {"state": "reviewing", "at": "2026-07-10T00:08:00Z", "evidence_ids": ["tests"]},
        {"state": "awaiting_acceptance", "at": "2026-07-10T00:09:00Z", "evidence_ids": [item["id"] for item in candidate["evidence"]]},
    ]
    candidate["repair_cycles"] = 0
    with pytest.raises(module.Invalid, match="repair_cycles"):
        module.validate(candidate, ROOT)


@pytest.mark.parametrize(
    ("risk_tier", "cycles"),
    [
        pytest.param("routine", 2, id="routine-at-2"),
        pytest.param("substantial", 4, id="substantial-at-4"),
        pytest.param("crucial", 5, id="crucial-at-5"),
        pytest.param("terminal", 5, id="terminal-at-5"),
    ],
)
def test_repair_cycle_budget_accepts_tier_boundary(risk_tier, cycles):
    module = load_validator()
    candidate = at_risk_tier(with_repair_cycles(fixture(), cycles), risk_tier)

    module.validate(candidate, ROOT)


@pytest.mark.parametrize(
    ("risk_tier", "cycles", "budget"),
    [
        pytest.param("routine", 3, 2, id="routine-at-3"),
        pytest.param("substantial", 5, 4, id="substantial-at-5"),
        pytest.param("crucial", 6, 5, id="crucial-at-6"),
        pytest.param("terminal", 6, 5, id="terminal-at-6"),
    ],
)
def test_repair_cycle_budget_rejects_first_cycle_over_tier_limit(
    risk_tier, cycles, budget,
):
    module = load_validator()
    candidate = at_risk_tier(with_repair_cycles(fixture(), cycles), risk_tier)
    message = (
        rf"repair_cycles {cycles} exceeds budget for {risk_tier} tier "
        rf"\(max {budget}\)"
    )

    with pytest.raises(module.Invalid, match=message):
        module.validate(candidate, ROOT)


@pytest.mark.parametrize(
    ("cycles", "message"),
    [
        pytest.param(True, "repair_cycles must be an integer, got bool", id="bool"),
        pytest.param(None, "repair_cycles must be an integer, got NoneType", id="none"),
        pytest.param("2", "repair_cycles must be an integer, got str", id="string"),
        pytest.param(-1, "repair_cycles must be non-negative, got -1", id="negative"),
    ],
)
def test_repair_cycles_shape_errors_are_distinct_from_budget_overrun(cycles, message):
    module = load_validator()
    candidate = fixture()
    candidate["repair_cycles"] = cycles

    with pytest.raises(module.Invalid, match=re.escape(message)) as excinfo:
        module.validate(candidate, ROOT)
    assert "exceeds budget" not in str(excinfo.value)


def test_partial_delivery_delegation_is_rejected():
    module = load_validator()
    candidate = fixture()
    candidate["authority"]["delegations"] = [{
        "actor": "worker-1",
        "allowed_source_paths": ["input"],
        "allowed_artifact_paths": ["worker-1"],
    }]
    with pytest.raises(module.Invalid, match="delegations.*fields are invalid"):
        module.validate(candidate, ROOT)


def test_authority_requires_approver_expiry_and_external_action_controls():
    module = load_validator()
    candidate = fixture()
    for field in (
        "schema_version", "approved_by", "evidence", "evidence_digest",
        "workspace_roots", "expires_at",
        "denied_paths", "secrets_access", "deployment",
        "irreversible_actions", "network", "budget",
    ):
        broken = copy.deepcopy(candidate)
        del broken["authority"][field]
        with pytest.raises(module.Invalid, match="authority"):
            module.validate(broken, ROOT)


def test_local_artifact_must_be_inside_authorised_artifact_scope():
    module = load_validator()
    candidate = fixture()
    candidate["authority"]["allowed_artifact_paths"] = ["elsewhere"]
    with pytest.raises(module.Invalid, match="outside authority"):
        module.validate(candidate, ROOT)


def test_deterministic_evidence_digest_must_bind_its_declared_artifact():
    module = load_validator()
    candidate = fixture()
    check = next(item for item in candidate["evidence"] if item["kind"] == "deterministic")
    check["result"]["receipt_digest"] = "sha256:" + "c" * 64

    with pytest.raises(module.Invalid, match="receipt digest must bind its declared artifact"):
        module.validate(candidate, ROOT)


def test_deterministic_evidence_artifact_must_be_a_matching_bundle(tmp_path):
    module = load_validator()
    workspace = tmp_path / "arbitrary"
    candidate = fixture("software", workspace)
    artifact = next(item for item in candidate["artifacts"] if item["id"] == "evidence-bundle")
    target = workspace / artifact["path"]
    target.write_text("not a receipt\n")
    digest = "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
    artifact["digest"] = digest
    for item in candidate["evidence"]:
        if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact["id"]:
            item["result"]["receipt_digest"] = digest

    with pytest.raises(module.Invalid, match="valid bundle JSON"):
        module.validate(candidate, ROOT, workspace_root=workspace, verify_hashes=True)

    candidate = fixture("software", workspace)
    artifact = next(item for item in candidate["artifacts"] if item["id"] == "evidence-bundle")
    target = workspace / artifact["path"]
    bundle = json.loads(target.read_text())
    bundle["checks"][0]["gate"] = "wrong-gate"
    target.write_text(json.dumps(bundle) + "\n")
    digest = "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
    artifact["digest"] = digest
    for item in candidate["evidence"]:
        if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact["id"]:
            item["result"]["receipt_digest"] = digest

    with pytest.raises(module.Invalid, match="does not match its evidence row"):
        module.validate(candidate, ROOT, workspace_root=workspace, verify_hashes=True)


def test_risk_tier_cannot_be_downgraded_without_human_evidence():
    module = load_validator()
    candidate = fixture()
    candidate["risk_tier"] = "routine"
    with pytest.raises(module.Invalid, match="risk downgrade"):
        module.validate(candidate, ROOT)
    candidate["risk_override"] = {
        "status": "approved",
        "approved_by": "human",
        "evidence": "risk-override-approval",
        "reason": "bounded reference run",
    }
    module.validate(candidate, ROOT)
    candidate["risk_override"]["evidence"] = "intent-approval"
    with pytest.raises(module.Invalid, match="risk override"):
        module.validate(candidate, ROOT)


def test_validator_rejects_artifact_symlink_that_leaves_declared_scope(tmp_path):
    module = load_validator()
    candidate = fixture()
    candidate["authority"]["allowed_artifact_paths"] = ["allowed"]
    candidate["intent"]["artifact"] = "allowed/intent.md"
    candidate["artifacts"][0]["path"] = "allowed/intent.md"
    candidate["artifacts"][1]["path"] = "allowed/evidence.json"
    (tmp_path / "allowed").mkdir()
    (tmp_path / "outside").mkdir()
    (tmp_path / "outside" / "intent.md").write_text("outside\n")
    (tmp_path / "allowed" / "intent.md").symlink_to("../outside/intent.md")

    with pytest.raises(module.Invalid, match="resolves outside authority.allowed_artifact_paths"):
        module.validate(candidate, ROOT, workspace_root=tmp_path)


def test_validator_rechecks_live_risk_override_artifact_digest(tmp_path):
    module = load_validator()
    candidate = fixture(workspace_root=tmp_path)
    approval = tmp_path / "risk-approval.json"
    approval.write_text('{"approved":true}\n')
    approval_digest = "sha256:" + hashlib.sha256(approval.read_bytes()).hexdigest()
    candidate["artifacts"].append({
        "id": "risk-override-artifact",
        "path": "risk-approval.json",
        "media_type": "application/json",
        "artifact_type": "evidence",
        "digest": approval_digest,
        "class": "evidence",
        "owner": "delivery-chair",
        "retention": "risk-policy",
    })
    next(item for item in candidate["evidence"] if item["id"] == "risk-override-approval")["artifact_id"] = "risk-override-artifact"
    candidate["risk_tier"] = "routine"
    candidate["risk_override"] = {
        "status": "approved",
        "approved_by": "human",
        "evidence": "risk-override-approval",
        "reason": "bounded reference run",
    }
    module.validate(candidate, ROOT, workspace_root=tmp_path, verify_hashes=True)

    approval.write_text('{"approved":false}\n')
    with pytest.raises(module.Invalid, match="risk override artifact digest"):
        module.validate(candidate, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_validator_requires_canonical_method_for_full_tests_gate(tmp_path):
    module = load_validator()
    candidate = fixture(workspace_root=tmp_path)
    harness = tmp_path / "scripts" / "check-harness"
    harness.parent.mkdir()
    harness.write_text("#!/bin/sh\nexit 0\n")
    tests_evidence = next(item for item in candidate["evidence"] if item["gate"] == "tests")
    tests_evidence["method"] = "python -m pytest tests/test_delivery_contract.py -q"

    with pytest.raises(module.Invalid, match="scripts/check-harness"):
        module.validate(candidate, ROOT, workspace_root=tmp_path)


def test_validator_allows_custom_method_for_generic_tests_gate(tmp_path):
    module = load_validator()
    candidate = fixture(workspace_root=tmp_path)
    tests_evidence = next(item for item in candidate["evidence"] if item["gate"] == "tests")
    tests_evidence["method"] = "python -m pytest tests/test_delivery_contract.py -q"

    module.validate(candidate, ROOT, workspace_root=tmp_path)


def test_validator_rejects_source_symlink_that_leaves_declared_scope(tmp_path):
    module = load_validator()
    candidate = fixture(workspace_root=tmp_path)
    candidate["authority"]["allowed_source_paths"].append("allowed")
    outside = tmp_path / "outside.txt"
    outside.write_text("outside\n")
    allowed = tmp_path / "allowed"
    allowed.mkdir()
    (allowed / "source").symlink_to(outside)
    tests_evidence = next(item for item in candidate["evidence"] if item["gate"] == "tests")
    tests_evidence["source_paths"] = ["allowed/source"]

    with pytest.raises(module.Invalid, match="leaves authority.allowed_source_paths"):
        module.validate(candidate, ROOT, workspace_root=tmp_path)


def test_validator_requires_bounded_reviewer_id_for_passing_review():
    module = load_validator()
    candidate = fixture()
    review = next(item for item in candidate["reviews"] if item["status"] == "pass")
    review.pop("reviewer_id")

    with pytest.raises(module.Invalid, match=r"review 0\.reviewer_id"):
        module.validate(candidate, ROOT)


def test_validator_requires_route_receipt_for_passing_review():
    module = load_validator()
    candidate = fixture()
    review = next(item for item in candidate["reviews"] if item["status"] == "pass")
    linked = next(item for item in candidate["evidence"] if item["id"] == review["evidence_id"])
    del linked["route_receipt"]

    with pytest.raises(module.Invalid, match="route_receipt"):
        module.validate(candidate, ROOT)


def test_substantial_run_requires_design_and_targeted_other_primary_review_lanes():
    module = load_validator()
    candidate = fixture("research")
    candidate["design"]["status"] = "not-required"
    with pytest.raises(module.Invalid, match="design"):
        module.validate(candidate, ROOT)
    candidate = fixture("research")
    candidate["reviews"] = [review for review in candidate["reviews"] if review["provider_family"] != "anthropic"]
    with pytest.raises(module.Invalid, match="other-primary"):
        module.validate(candidate, ROOT)


def test_review_ladder_accepts_targeted_lenses_without_whole_change_leg():
    module = load_validator()
    candidate = fixture("research")
    targeted = copy.deepcopy(candidate["reviews"][0])
    targeted["role"] = "targeted"
    targeted["lenses"] = ["correctness", "adversarial"]
    candidate["reviews"] = [targeted, candidate["reviews"][1]]
    module.validate(candidate, ROOT)


def test_crucial_review_ladder_does_not_require_distinct_family():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["risk_tier"] = "crucial"
    candidate["reviews"] = [
        review for review in candidate["reviews"] if review["role"] != "distinct-family"
    ]
    candidate["reviews"].append({
        "role": "distinct-family", "provider_family": "google", "adapter": "gemini",
        "model": "", "independent_of_authorship": True, "lenses": ["blind-spots"],
        "status": "skipped", "evidence_id": "", "reason": "not warranted for this terminal change",
    })
    candidate["reviews"][0]["lenses"] = ["correctness", "tests"]
    module._validate_reviews(
        candidate,
        {item["id"]: item for item in candidate["evidence"]},
        required=True,
    )


def test_terminal_review_ladder_requires_adversarial_pressure_not_fixed_provider_count():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["risk_tier"] = "terminal"
    candidate["reviews"] = [
        review for review in candidate["reviews"] if review["role"] != "distinct-family"
    ]
    candidate["reviews"].append({
        "role": "distinct-family", "provider_family": "google", "adapter": "gemini",
        "model": "", "independent_of_authorship": True, "lenses": ["blind-spots"],
        "status": "skipped", "evidence_id": "", "reason": "not warranted for this terminal change",
    })
    candidate["reviews"][0]["lenses"] = ["correctness", "reliability", "adversarial"]
    module._validate_reviews(
        candidate,
        {item["id"]: item for item in candidate["evidence"]},
        required=True,
    )

    candidate["reviews"][0]["lenses"] = ["correctness", "reliability", "tests"]
    with pytest.raises(module.Invalid, match="adversarial"):
        module._validate_reviews(
            candidate,
            {item["id"]: item for item in candidate["evidence"]},
            required=True,
        )


def test_same_family_other_primary_review_is_blocked():
    module = load_validator()
    candidate = fixture("research")
    candidate["chair_family"] = "openai"
    targeted = candidate["reviews"][0]
    other_primary = next(r for r in candidate["reviews"] if r["role"] == "other-primary")
    other_primary["provider_family"] = targeted["provider_family"]
    other_primary["adapter"] = targeted["adapter"]
    other_primary["model"] = targeted["model"]
    other_primary["evidence_id"] = targeted["evidence_id"]
    with pytest.raises(module.Invalid, match="distinct primary family"):
        module.validate(candidate, ROOT)


def test_missing_chair_family_is_rejected():
    module = load_validator()
    candidate = fixture()
    candidate["chair_family"] = "google"
    with pytest.raises(module.Invalid, match="chair_family"):
        module.validate(candidate, ROOT)


def test_crucial_review_ladder_blocks_with_no_distinct_family_or_recorded_skip():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["risk_tier"] = "crucial"
    candidate["reviews"] = [
        review for review in candidate["reviews"] if review["role"] != "distinct-family"
    ]
    with pytest.raises(module.Invalid, match="distinct-family review or recorded skip"):
        module._validate_reviews(
            candidate,
            {item["id"]: item for item in candidate["evidence"]},
            required=True,
        )


def test_terminal_review_ladder_blocks_with_no_distinct_family_or_recorded_skip():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["risk_tier"] = "terminal"
    candidate["reviews"] = [
        review for review in candidate["reviews"] if review["role"] != "distinct-family"
    ]
    candidate["reviews"][0]["lenses"] = ["correctness", "reliability", "adversarial"]
    with pytest.raises(module.Invalid, match="distinct-family review or recorded skip"):
        module._validate_reviews(
            candidate,
            {item["id"]: item for item in candidate["evidence"]},
            required=True,
        )


def test_crucial_design_requires_alternatives_failure_analysis_and_containment():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["risk_tier"] = "crucial"
    candidate["design"]["alternatives"] = []
    with pytest.raises(module.Invalid, match="alternatives"):
        module.validate(candidate, ROOT)


def test_crucial_design_rejects_unresolved_or_demoted_one_way_door():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["risk_tier"] = "crucial"
    candidate["design"]["one_way_doors"] = [{
        "id": "irreversible-schema-cutover",
        "decision": "replace the old store",
        "classification": "implementation-detail",
        "status": "unresolved",
        "evidence": "",
    }]
    with pytest.raises(module.Invalid, match="implementation detail|unresolved"):
        module.validate(candidate, ROOT)

    candidate["design"]["one_way_doors"][0].update({
        "classification": "design-decision",
        "status": "deferred",
        "evidence": "human:design-deferral",
        "approved_by": "",
        "reason": "",
    })
    with pytest.raises(module.Invalid, match="human evidence|human approval"):
        module.validate(candidate, ROOT)


@pytest.mark.parametrize("profile", ["software", "agent-product"])
def test_crucial_technical_profiles_require_applicable_security_evidence(profile):
    module = load_validator()
    candidate = fixture(profile)
    candidate["risk_tier"] = "crucial"
    candidate["security"]["checks"] = []
    with pytest.raises(module.Invalid, match="security"):
        module.validate(candidate, ROOT)


def test_agent_product_requires_owasp_agentic_risk_disposition():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["security"]["agentic_risks"] = []
    with pytest.raises(module.Invalid, match="agentic risk"):
        module.validate(candidate, ROOT)

    candidate = fixture("agent-product")
    passing_risk = next(item for item in candidate["security"]["agentic_risks"] if item["status"] == "pass")
    passing_risk["evidence_id"] = next(
        item["id"] for item in candidate["evidence"] if item["kind"] == "judgement"
    )
    with pytest.raises(module.Invalid, match="deterministic"):
        module.validate(candidate, ROOT)


def test_security_checks_are_exact_policy_selected_deterministic_evidence():
    module = load_validator()
    candidate = fixture("software")
    candidate["risk_tier"] = "crucial"
    candidate["security"]["checks"] = [{"id": "made-up", "surface": "made-up", "status": "pass", "evidence_id": "tests"}]
    with pytest.raises(module.Invalid, match="security"):
        module.validate(candidate, ROOT)
    candidate = fixture("software")
    candidate["risk_tier"] = "crucial"
    candidate["security"]["checks"][0]["evidence_id"] = next(item["id"] for item in candidate["evidence"] if item["kind"] == "judgement")
    with pytest.raises(module.Invalid, match="deterministic"):
        module.validate(candidate, ROOT)

    candidate = fixture("software")
    candidate["risk_tier"] = "crucial"
    candidate["security"]["changed_surfaces"] = ["generated-artifact"]
    candidate["security"]["artifact_surfaces"] = [{"artifact_id": "intent", "surfaces": ["generated-artifact"]}]
    candidate["security"]["checks"] = [{"id": "provenance", "surface": "generated-artifact", "status": "pass", "evidence_id": "security-provenance"}]
    candidate["evidence"].append({"id": "security-provenance", "kind": "deterministic", "gate": "provenance", "status": "pass", "method": "probe", "artifact_id": "evidence-bundle", "source_paths": ["input"], "result": {"exit_code": 0, "receipt_digest": "sha256:" + "b" * 64}})
    with pytest.raises(module.Invalid, match="derived surfaces"):
        module.validate(candidate, ROOT)

    candidate = fixture("software")
    candidate["security"].update({"status": "not_applicable", "reason": "self-declared", "changed_surfaces": [], "artifact_surfaces": [], "checks": []})
    with pytest.raises(module.Invalid, match=r"substantial\+ technical profile"):
        module.validate(candidate, ROOT)


def test_optional_family_failure_is_recorded_but_non_blocking(tmp_path):
    module = load_validator()
    workspace_root = tmp_path / "agent-product"
    candidate = fixture("agent-product", workspace_root)
    candidate["reviews"].append({
        "role": "distinct-family",
        "provider_family": "google",
        "adapter": "gemini",
        "independent_of_authorship": True,
        "lenses": ["security"],
        "status": "unavailable",
        "evidence_id": "",
        "reason": "quota",
    })
    module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)


def test_primary_reviews_require_distinct_matching_judgement_evidence():
    module = load_validator()
    candidate = fixture()
    candidate["reviews"][1]["evidence_id"] = candidate["reviews"][0]["evidence_id"]
    with pytest.raises(module.Invalid, match="distinct|lineage"):
        module.validate(candidate, ROOT)
    candidate = fixture()
    candidate["reviews"][1]["evidence_id"] = "tests"
    with pytest.raises(module.Invalid, match="judgement"):
        module.validate(candidate, ROOT)


def test_profile_gate_cannot_be_satisfied_by_wrong_evidence_kind():
    module = load_validator()
    candidate = fixture("research")
    next(item for item in candidate["evidence"] if item["gate"] == "source-coverage")["kind"] = "judgement"
    with pytest.raises(module.Invalid, match="profile gate|model_lineage"):
        module.validate(candidate, ROOT)


def test_closed_run_requires_observation_contract_and_accepted_human_gate():
    module = load_validator()
    candidate = fixture()
    candidate["status"] = "closed"
    candidate["checkpoint"].update({"current_slice": "closed", "next_action": "cycle closed", "in_flight": []})
    candidate["state_history"].extend([
        {"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "awaiting_release", "at": "2026-07-10T00:10:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "observing", "at": "2026-07-10T00:11:00Z", "evidence_ids": ["release-approval"]},
        {"state": "closed", "at": "2026-07-11T00:11:00Z", "evidence_ids": []},
    ])
    candidate["human_gates"]["acceptance"] = {"status": "approved", "approver": "human", "evidence": "acceptance-approval"}
    candidate["human_gates"]["release"] = {"status": "approved", "approver": "human", "evidence": "release-approval"}
    candidate["observation"] = None
    with pytest.raises(module.Invalid, match="observation"):
        module.validate(candidate, ROOT)

    candidate = fixture()
    candidate["human_gates"]["acceptance"] = {"status": "approved", "approver": "human", "evidence": "intent-approval"}
    candidate["status"] = "accepted"
    candidate["checkpoint"].update({"current_slice": "accepted", "next_action": "prepare release", "in_flight": []})
    candidate["state_history"].append({"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["intent-approval"]})
    with pytest.raises(module.Invalid, match="human acceptance"):
        module.validate(candidate, ROOT)


def test_observation_not_applicable_requires_profile_justification():
    module = load_validator()
    candidate = fixture()
    candidate["observation"] = {"status": "not_applicable", "reason": ""}
    with pytest.raises(module.Invalid, match="not_applicable"):
        module.validate(candidate, ROOT)


def test_closed_crucial_or_incident_cycle_requires_retrospective_linkage(tmp_path):
    module = load_validator()
    workspace_root = tmp_path / "agent-product"
    candidate = fixture("agent-product", workspace_root)
    candidate["risk_tier"] = "crucial"
    candidate["status"] = "closed"
    candidate["checkpoint"].update({"current_slice": "closed", "next_action": "cycle closed", "in_flight": []})
    candidate["state_history"].extend([
        {"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "awaiting_release", "at": "2026-07-10T00:10:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "observing", "at": "2026-07-10T00:11:00Z", "evidence_ids": ["release-approval"]},
        {"state": "closed", "at": "2026-07-11T00:11:00Z", "evidence_ids": ["observation"]},
    ])
    candidate["human_gates"]["acceptance"] = {"status": "approved", "approver": "human", "evidence": "acceptance-approval"}
    candidate["human_gates"]["release"] = {"status": "approved", "approver": "human", "evidence": "release-approval"}
    candidate["evidence"].append({"id": "observation-result", "kind": "observation", "gate": "task-success", "status": "pass", "method": "reference-observation", "artifact_id": "evidence-bundle", "source_paths": ["input"], "observed_at": "2026-07-10T12:00:00Z", "measured_value": 1})
    candidate["state_history"][-1]["evidence_ids"] = ["observation-result"]
    candidate["observation"]["status"] = "pass"
    candidate["observation"].update({"started_at": "2026-07-10T00:11:00Z", "ended_at": "2026-07-11T00:11:00Z", "observed_events": 1, "evidence_ids": ["observation-result"]})
    candidate["retrospective"] = None
    with pytest.raises(module.Invalid, match="retrospective"):
        module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)


def test_required_retrospective_cannot_borrow_another_delivery_cycle(tmp_path):
    module = load_validator()
    product_root = tmp_path / "separate-product"
    shutil.copytree(ROOT / "config", product_root / "config")
    candidate = fixture("agent-product", tmp_path)
    candidate["risk_tier"] = "crucial"
    candidate["status"] = "closed"
    candidate["checkpoint"].update({"current_slice": "closed", "next_action": "cycle closed", "in_flight": []})
    candidate["state_history"].extend([
        {"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "awaiting_release", "at": "2026-07-10T00:10:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "observing", "at": "2026-07-10T00:11:00Z", "evidence_ids": ["release-approval"]},
        {"state": "closed", "at": "2026-07-11T00:11:00Z", "evidence_ids": ["observation-result"]},
    ])
    candidate["human_gates"]["acceptance"] = {"status": "approved", "approver": "human", "evidence": "acceptance-approval"}
    candidate["human_gates"]["release"] = {"status": "approved", "approver": "human", "evidence": "release-approval"}
    candidate["evidence"].append({
        "id": "observation-result", "kind": "observation", "gate": "task-success",
        "status": "pass", "method": "reference-observation", "artifact_id": "evidence-bundle",
        "source_paths": ["input"], "observed_at": "2026-07-10T12:00:00Z", "measured_value": 1,
    })
    candidate["observation"].update({
        "status": "pass", "started_at": "2026-07-10T00:11:00Z",
        "ended_at": "2026-07-11T00:11:00Z", "observed_events": 1,
        "evidence_ids": ["observation-result"],
    })

    retro_dir = tmp_path / "retro"
    retro_dir.mkdir()
    other = fixture("agent-product")
    other["run_id"] = "OTHER-CYCLE"
    other["fabric_relationships"]["delivery_run_id"] = "OTHER-CYCLE"
    source_path = retro_dir / "OTHER.json"
    source_path.write_text(json.dumps(other))
    retro = json.loads((ROOT / "skills" / "retrospect" / "templates" / "RETROSPECT.template.json").read_text())
    retro.update({"status": "no-change", "proposals": []})
    retro["scope"].update({
        "cycle_ids": ["OTHER-CYCLE"], "profile": "agent-product",
        "baseline": {"cycle_ids": ["OTHER-CYCLE"], "absence_reason": ""},
    })
    retro["sources"] = [{
        "kind": "delivery", "id": "OTHER-CYCLE", "path": "OTHER.json",
        "sha256": hashlib.sha256(source_path.read_bytes()).hexdigest(), "schema_version": 1,
    }]
    retro["metrics"][0]["source_ids"] = ["OTHER-CYCLE"]
    retro["findings"][0]["evidence_ids"] = ["OTHER-CYCLE"]
    retro_path = retro_dir / "RETROSPECT.json"
    retro_path.write_text(json.dumps(retro))
    retro_digest = "sha256:" + hashlib.sha256(retro_path.read_bytes()).hexdigest()
    candidate["artifacts"].append({
        "id": "retrospective", "path": "retro/RETROSPECT.json", "media_type": "application/json",
        "artifact_type": "evidence", "digest": retro_digest, "class": "evidence",
        "owner": "delivery-chair", "retention": "risk-policy",
    })
    candidate["retrospective"] = {"status": "no-change", "artifact_id": "retrospective", "digest": retro_digest}
    with pytest.raises(module.Invalid, match="current delivery cycle"):
        module.validate(
            candidate, product_root, workspace_root=tmp_path, verify_hashes=True,
        )


def test_checkpoint_and_observation_substates_follow_lifecycle_state():
    module = load_validator()
    candidate = fixture()
    candidate["status"] = "observing"
    candidate["state_history"].extend([
        {"state": "accepted", "at": "2026-07-10T00:09:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "awaiting_release", "at": "2026-07-10T00:10:00Z", "evidence_ids": ["acceptance-approval"]},
        {"state": "observing", "at": "2026-07-10T00:11:00Z", "evidence_ids": ["release-approval"]},
    ])
    candidate["human_gates"]["acceptance"] = {"status": "approved", "approver": "human", "evidence": "acceptance-approval"}
    candidate["human_gates"]["release"] = {"status": "approved", "approver": "human", "evidence": "release-approval"}
    candidate["checkpoint"].update({"current_slice": "observing", "next_action": "complete observation", "in_flight": []})
    candidate["observation"]["status"] = "planned"
    with pytest.raises(module.Invalid, match="observing.*active or pass"):
        module.validate(candidate, ROOT)

    candidate["observation"]["status"] = "active"
    module.validate(candidate, ROOT)
    candidate["checkpoint"]["current_slice"] = "awaiting-acceptance"
    with pytest.raises(module.Invalid, match="checkpoint.current_slice"):
        module.validate(candidate, ROOT)


def test_closed_checkpoint_is_terminal_and_only_references_known_artifacts():
    module = load_validator()
    candidate = fixture()
    candidate["checkpoint"]["artifact_paths"] = ["missing-review.md"]
    with pytest.raises(module.Invalid, match="checkpoint artifact.*declared or live"):
        module.validate(candidate, ROOT)


def test_high_stakes_overlay_requires_source_authority_privacy_and_qualified_review():
    module = load_validator()
    candidate = fixture("document")
    candidate["high_stakes"] = True
    with pytest.raises(module.Invalid, match="high[-_]stakes"):
        module.validate(candidate, ROOT)
    candidate["high_stakes_controls"] = {
        "source_authority": {"status": "pass", "evidence_id": "intent-approval", "authority": "human"},
        "privacy": {"status": "pass", "evidence_id": "render", "privacy_boundary": "private"},
        "qualified_domain_review": {"status": "pass", "evidence_id": "intent-approval", "domain": "law", "reviewer": "human", "qualification": "claimed"},
        "explicit_human_action_gate": {"status": "pass", "evidence_id": "intent-approval", "action": "file", "approved_by": "human"},
    }
    with pytest.raises(module.Invalid, match="matching|distinct"):
        module.validate(candidate, ROOT)


def test_awaiting_acceptance_requires_outcome_trajectory_and_bound_stochastic_evidence(tmp_path):
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["measures"]["trajectory"] = []
    with pytest.raises(module.Invalid, match="trajectory"):
        module.validate(candidate, ROOT)

    workspace_root = tmp_path / "agent-product"
    candidate = fixture("agent-product", workspace_root)
    with pytest.raises(module.Invalid, match="requires --verify-hashes"):
        module.validate(candidate, ROOT, workspace_root=workspace_root)
    module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)

    candidate["assurance"]["evaluations"][0]["repetitions"] = 3
    with pytest.raises(module.Invalid, match="canonical receipt binding fields"):
        module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)

    candidate = fixture("software")
    candidate["measures"]["outcome"][0].pop("target")
    with pytest.raises(module.Invalid, match="value, target and aggregation"):
        module.validate(candidate, ROOT)


def test_prompt_agent_product_cannot_self_declare_stochastic_evaluation_unnecessary():
    module = load_validator()
    candidate = fixture("agent-product")
    candidate["assurance"] = {
        "stochastic_required": False,
        "reason": "deterministic policy and permission-boundary change",
        "evaluations": [],
    }
    candidate["artifacts"] = [
        artifact
        for artifact in candidate["artifacts"]
        if artifact["id"] != "evaluation-receipt"
    ]

    with pytest.raises(module.Invalid, match="canonical artifact classification"):
        module.validate(candidate, ROOT)


@pytest.mark.parametrize(
    "classified_types",
    ["prompt", [], ["prompt", 7], ["prompt", "prompt"], ["not-a-profile-artifact"]],
)
def test_profile_registry_rejects_malformed_stochastic_artifact_classification(
    tmp_path, classified_types,
):
    module = load(POLICY_VALIDATOR_PATH, "delivery_policy_validation")
    registry = json.loads((ROOT / "config" / "delivery-profiles.json").read_text())
    registry["profiles"]["agent-product"]["stochastic_policy"][
        "required_for_artifact_types"
    ] = classified_types
    config = tmp_path / "config"
    config.mkdir()
    (config / "delivery-profiles.json").write_text(json.dumps(registry))

    with pytest.raises(ValueError, match="required_for_artifact_types"):
        module.load_profiles(tmp_path, invalid_type=ValueError)


def test_stochastic_binding_anchors_plan_before_execution_then_completes(tmp_path):
    module = load_validator()
    workspace_root = tmp_path / "two-phase"
    complete = fixture("agent-product", workspace_root)
    complete_row = complete["assurance"]["evaluations"][0]
    anchored_values = (complete_row["evaluation_id"], complete_row["plan_digest"])

    planned = copy.deepcopy(complete)
    planned["status"] = "executing"
    planned["state_history"] = planned["state_history"][:4]
    planned["checkpoint"].update({
        "current_slice": "executing", "next_action": "run frozen evaluation",
        "in_flight": ["evaluation"],
    })
    planned["reviews"] = []
    planned["measures"] = {"outcome": [], "trajectory": []}
    planned["evidence"] = [
        item for item in planned["evidence"] if item["kind"] != "judgement"
    ]
    planned["artifacts"] = [
        item for item in planned["artifacts"] if item["id"] != "evaluation-receipt"
    ]
    planned_row = planned["assurance"]["evaluations"][0]
    planned_row.update({
        "status": "planned", "evaluation_artifact_id": "",
        "evaluation_digest": "", "evidence_id": "",
    })
    module.validate(planned, ROOT)
    assert (planned_row["evaluation_id"], planned_row["plan_digest"]) == anchored_values

    transitioned = copy.deepcopy(complete)
    transitioned_row = copy.deepcopy(planned_row)
    transitioned_row.update({
        field: complete_row[field]
        for field in ("status", "evaluation_artifact_id", "evaluation_digest", "evidence_id")
    })
    transitioned["assurance"]["evaluations"][0] = transitioned_row
    assert (transitioned_row["evaluation_id"], transitioned_row["plan_digest"]) == anchored_values
    module.validate(
        transitioned, ROOT, workspace_root=workspace_root, verify_hashes=True,
    )

    awaiting = copy.deepcopy(complete)
    awaiting_row = awaiting["assurance"]["evaluations"][0]
    awaiting_row.update({
        "status": "planned", "evaluation_artifact_id": "",
        "evaluation_digest": "", "evidence_id": "",
    })
    awaiting["artifacts"] = [
        item for item in awaiting["artifacts"] if item["id"] != "evaluation-receipt"
    ]
    with pytest.raises(module.Invalid, match="must be complete before stochastic acceptance"):
        module.validate(awaiting, ROOT)

    assert (complete_row["evaluation_id"], complete_row["plan_digest"]) == anchored_values


def test_stochastic_anchor_must_precede_nested_evaluation_execution(tmp_path):
    module = load_validator()
    workspace_root = tmp_path / "late-anchor"
    candidate = fixture("agent-product", workspace_root)
    candidate["assurance"]["evaluations"][0]["anchored_at"] = "2026-07-10T00:03:00Z"
    with pytest.raises(module.Invalid, match="precede its nested evaluation execution"):
        module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)


@pytest.mark.parametrize("terminal_status", ["failed", "incomplete"])
def test_terminal_evaluation_is_retained_but_cannot_satisfy_acceptance(
    tmp_path, terminal_status,
):
    module = load_validator()
    materialiser = load(
        REFERENCE_EVALUATION_PATH, f"terminal_evaluation_{terminal_status}",
    )
    workspace_root = tmp_path / terminal_status
    candidate = fixture("agent-product")
    terminalise_reference_evaluation(candidate, terminal_status)
    awaiting_transition = copy.deepcopy(candidate["state_history"][-1])
    candidate["status"] = "reviewing"
    candidate["state_history"] = candidate["state_history"][:-1]
    candidate["checkpoint"].update({
        "current_slice": "reviewing", "next_action": "decide whether to retry",
        "in_flight": [],
    })
    materialiser.materialise_reference_run(
        candidate, workspace_root, ROOT,
        evaluation_repetitions=2, evaluation_sample_size=1,
    )
    module.validate(
        candidate, ROOT, workspace_root=workspace_root, verify_hashes=True,
    )

    awaiting = copy.deepcopy(candidate)
    awaiting["status"] = "awaiting_acceptance"
    awaiting["state_history"].append(awaiting_transition)
    awaiting["checkpoint"].update({
        "current_slice": "awaiting-acceptance", "next_action": "human acceptance",
    })
    with pytest.raises(module.Invalid, match="at least one complete passing evaluation"):
        module.validate(
            awaiting, ROOT, workspace_root=workspace_root, verify_hashes=True,
        )


def test_terminal_evaluation_requires_deterministic_not_judgement_evidence(tmp_path):
    module = load_validator()
    materialiser = load(REFERENCE_EVALUATION_PATH, "terminal_evidence_kind")
    workspace_root = tmp_path / "terminal-evidence"
    candidate = fixture("agent-product")
    binding = terminalise_reference_evaluation(candidate)
    materialiser.materialise_reference_run(candidate, workspace_root, ROOT)
    binding["evidence_id"] = next(
        item["id"] for item in candidate["evidence"] if item["kind"] == "judgement"
    )
    with pytest.raises(module.Invalid, match="terminal nonpass.*deterministic evidence"):
        module.validate(
            candidate, ROOT, workspace_root=workspace_root, verify_hashes=True,
        )


def test_fresh_complete_plan_after_failed_evaluation_can_satisfy_acceptance(tmp_path):
    module = load_validator()
    materialiser = load(REFERENCE_EVALUATION_PATH, "fresh_after_failed")
    workspace_root = tmp_path / "fresh-after-failed"
    candidate = fixture("agent-product")
    failed = terminalise_reference_evaluation(candidate)
    failed["evaluation_id"] = "EVAL-FAILED-FIRST"

    candidate["artifacts"].append({
        "id": "evaluation-retry",
        "path": "evaluation-retry/EVALUATION.json",
        "media_type": "application/json",
        "artifact_type": "evidence",
        "digest": "sha256:" + "e" * 64,
        "class": "evidence",
        "owner": "evaluation-chair",
        "retention": "risk-policy",
    })
    retry = {
        "status": "complete",
        "anchored_at": "2026-07-10T00:04:30Z",
        "evidence_id": next(
            item["id"] for item in candidate["evidence"]
            if item["kind"] == "judgement" and item["model_lineage"]["provider_family"] == "openai"
        ),
        "evaluation_artifact_id": "evaluation-retry",
        "evaluation_id": "EVAL-RETRY",
        "evaluation_digest": "sha256:" + "e" * 64,
        "plan_digest": "sha256:" + "e" * 64,
    }
    candidate["assurance"]["evaluations"].append(retry)
    candidate["state_history"][-2]["at"] = "2026-07-10T00:07:30Z"
    candidate["state_history"][-1]["at"] = "2026-07-10T00:08:00Z"

    materialiser.materialise_reference_run(candidate, workspace_root, ROOT)
    materialiser.materialise_evaluation_binding(
        candidate, workspace_root, ROOT, binding_index=0,
        repetitions=2, sample_size=1,
    )
    materialiser.materialise_evaluation_binding(
        candidate, workspace_root, ROOT, binding_index=1,
        repetitions=3, sample_size=10, time_offset_minutes=2,
    )

    first_execution = next(
        item["at"] for item in candidate["state_history"] if item["state"] == "executing"
    )
    assert retry["anchored_at"] > first_execution

    planned_retry = copy.deepcopy(candidate)
    planned_retry["status"] = "executing"
    planned_retry["state_history"] = planned_retry["state_history"][:5] + [{
        "state": "executing", "at": "2026-07-10T00:05:30Z", "evidence_ids": [],
    }]
    planned_retry["checkpoint"].update({
        "current_slice": "executing", "next_action": "run fresh evaluation plan",
        "in_flight": ["EVAL-RETRY"],
    })
    planned_retry_binding = planned_retry["assurance"]["evaluations"][1]
    planned_retry_binding.update({
        "status": "planned", "evaluation_artifact_id": "",
        "evaluation_digest": "", "evidence_id": "",
    })
    planned_retry["artifacts"] = [
        item for item in planned_retry["artifacts"] if item["id"] != "evaluation-retry"
    ]
    module.validate(
        planned_retry, ROOT, workspace_root=workspace_root, verify_hashes=True,
    )

    module.validate(
        candidate, ROOT, workspace_root=workspace_root, verify_hashes=True,
    )


@pytest.mark.parametrize(
    ("anchor", "replacement", "message"),
    [
        ("evaluation_id", "EVAL-FORGED", "evaluation_id does not match"),
        ("plan_digest", "sha256:" + "0" * 64, "plan.digest does not match"),
    ],
)
def test_stochastic_evaluation_rejects_forged_delivery_anchors(
    tmp_path, anchor, replacement, message,
):
    module = load_validator()
    workspace_root = tmp_path / anchor
    candidate = fixture("agent-product", workspace_root)
    candidate["assurance"]["evaluations"][0][anchor] = replacement
    with pytest.raises(module.Invalid, match=message):
        module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)


def test_stochastic_evaluation_is_bound_to_the_enclosing_delivery_run(tmp_path):
    module = load_validator()
    workspace_root = tmp_path / "run-id"
    candidate = fixture("agent-product", workspace_root)
    candidate["run_id"] = "FORGED-DELIVERY"
    candidate["fabric_relationships"]["delivery_run_id"] = "FORGED-DELIVERY"
    with pytest.raises(module.Invalid, match="enclosing_delivery_run_id does not match"):
        module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)


def test_stochastic_evaluation_verifies_nested_artifact_hashes(tmp_path):
    module = load_validator()
    workspace_root = tmp_path / "nested-hash"
    candidate = fixture("agent-product", workspace_root)
    output = workspace_root / "evaluation" / "evidence" / "output.json"
    output.write_text(output.read_text() + "tampered\n")
    with pytest.raises(module.Invalid, match="artifact output digest mismatch"):
        module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)


@pytest.mark.parametrize(
    ("repetitions", "sample_size", "message"),
    [
        (2, 10, "bound plan repetitions are below"),
        (3, 1, "bound plan sample size is below"),
    ],
)
def test_stochastic_evaluation_uses_bound_plan_for_profile_minimums(
    tmp_path, repetitions, sample_size, message,
):
    module = load_validator()
    workspace_root = tmp_path / f"{repetitions}-{sample_size}"
    candidate = fixture(
        "agent-product", workspace_root,
        evaluation_repetitions=repetitions,
        evaluation_sample_size=sample_size,
    )
    with pytest.raises(module.Invalid, match=message):
        module.validate(candidate, ROOT, workspace_root=workspace_root, verify_hashes=True)


def test_explicit_product_policy_root_can_be_separate_from_installed_skills(tmp_path):
    module = load_validator()
    product_root = tmp_path / "product"
    shutil.copytree(ROOT / "config", product_root / "config")

    module.validate(fixture(), product_root)


def test_project_policy_can_only_add_a_digest_bound_profile_or_gate(tmp_path):
    module = load_validator()
    candidate = fixture("research")
    overlay_path = tmp_path / "delivery-policy.json"
    overlay = {"schema_version": 1, "profiles": {"research": {"artifact_types": ["anything"]}}}
    raw = json.dumps(overlay).encode()
    overlay_path.write_bytes(raw)
    candidate["project_policy"] = {"path": "delivery-policy.json", "digest": "sha256:" + hashlib.sha256(raw).hexdigest()}
    with pytest.raises(module.Invalid, match="non-additive"):
        module.validate(candidate, ROOT, workspace_root=tmp_path, project_policy_path=overlay_path)

    registry = json.loads((ROOT / "config" / "delivery-profiles.json").read_text())
    custom = copy.deepcopy(registry["profiles"]["research"])
    overlay = {"schema_version": 1, "profiles": {"evidence-brief": custom}}
    raw = json.dumps(overlay).encode()
    overlay_path.write_bytes(raw)
    candidate = fixture("research")
    candidate["profile"] = "evidence-brief"
    candidate["project_policy"] = {"path": "delivery-policy.json", "digest": "sha256:" + hashlib.sha256(raw).hexdigest()}
    module.validate(candidate, ROOT, workspace_root=tmp_path, project_policy_path=overlay_path)


@pytest.mark.parametrize(
    "classified_types",
    ["prompt", [], ["prompt", 7], ["prompt", "prompt"], ["not-a-profile-artifact"]],
)
def test_project_policy_rejects_malformed_stochastic_artifact_classification(
    tmp_path, classified_types,
):
    module = load_validator()
    registry = json.loads((ROOT / "config" / "delivery-profiles.json").read_text())
    custom = copy.deepcopy(registry["profiles"]["agent-product"])
    custom["stochastic_policy"]["required_for_artifact_types"] = classified_types
    overlay_path = tmp_path / "delivery-policy.json"
    raw = json.dumps({"schema_version": 1, "profiles": {"custom-agent-product": custom}}).encode()
    overlay_path.write_bytes(raw)
    candidate = fixture("agent-product")
    candidate["profile"] = "custom-agent-product"
    candidate["project_policy"] = {
        "path": "delivery-policy.json",
        "digest": "sha256:" + hashlib.sha256(raw).hexdigest(),
    }

    with pytest.raises(module.Invalid, match="required_for_artifact_types"):
        module.validate(
            candidate,
            ROOT,
            workspace_root=tmp_path,
            project_policy_path=overlay_path,
        )


def test_project_defined_technical_profile_cannot_bypass_artifact_security(tmp_path):
    module = load_validator()
    registry = json.loads((ROOT / "config" / "delivery-profiles.json").read_text())
    overlay_path = tmp_path / "delivery-policy.json"
    overlay = {"schema_version": 1, "profiles": {"firmware": copy.deepcopy(registry["profiles"]["software"])}}
    raw = json.dumps(overlay).encode()
    overlay_path.write_bytes(raw)
    candidate = fixture("software")
    candidate["profile"] = "firmware"
    candidate["project_policy"] = {"path": "delivery-policy.json", "digest": "sha256:" + hashlib.sha256(raw).hexdigest()}
    module.validate(candidate, ROOT, workspace_root=tmp_path, project_policy_path=overlay_path)

    candidate["security"].update({"status": "not_applicable", "reason": "custom profile", "changed_surfaces": [], "artifact_surfaces": [], "checks": []})
    with pytest.raises(module.Invalid, match=r"substantial\+ technical profile"):
        module.validate(candidate, ROOT, workspace_root=tmp_path, project_policy_path=overlay_path)

    unknown = copy.deepcopy(registry["profiles"]["software"])
    unknown["artifact_types"] = ["firmware-binary"]
    raw = json.dumps({"schema_version": 1, "profiles": {"firmware": unknown}}).encode()
    overlay_path.write_bytes(raw)
    candidate = fixture("software")
    candidate["profile"] = "firmware"
    candidate["artifacts"][0]["artifact_type"] = "firmware-binary"
    candidate["project_policy"] = {"path": "delivery-policy.json", "digest": "sha256:" + hashlib.sha256(raw).hexdigest()}
    with pytest.raises(module.Invalid, match="unclassified artifact type"):
        module.validate(candidate, ROOT, workspace_root=tmp_path, project_policy_path=overlay_path)


def test_pass_output_binds_resolved_product_root(tmp_path):
    product_copy = tmp_path / "copy" / "product"
    shutil.copytree(ROOT / "config", product_copy / "config")
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    runs = load(REFERENCE_RUNS_PATH, "reference_runs_product_root_cli")
    run = runs.make_reference_run("agent-product", product_copy)
    materialiser = load(REFERENCE_EVALUATION_PATH, "reference_evaluation_product_root_cli")
    materialiser.materialise_reference_run(run, workspace, product_copy)
    receipt = workspace / "receipt.json"
    receipt.write_text(json.dumps(run))
    result = subprocess.run(
        [
            sys.executable,
            str(VALIDATOR_PATH),
            str(receipt),
            "--workspace-root",
            str(workspace),
            "--product-root",
            str(product_copy),
            "--verify-hashes",
        ],
        text=True,
        capture_output=True,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.startswith("PASS: delivery-v1 delivery receipt"), result.stdout
    assert f"(product_root={product_copy.resolve()})" in result.stdout
