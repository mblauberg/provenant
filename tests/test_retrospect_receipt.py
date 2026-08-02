import importlib.util
import hashlib
import json
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "skills" / "retrospect" / "templates" / "RETROSPECT.template.json"
MODULE_PATH = ROOT / "skills" / "retrospect" / "scripts" / "validate_retrospect.py"


def load_module():
    spec = importlib.util.spec_from_file_location("validate_retrospect", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def data():
    return json.loads(TEMPLATE.read_text())


def add_source(candidate, source_id):
    candidate["sources"].append({
        "kind": "delivery",
        "id": source_id,
        "path": f"{source_id}.json",
        "sha256": "c" * 64,
        "schema_version": 1,
    })


def add_regression_evidence(candidate, source_id="CHECK-1"):
    candidate["sources"].append({
        "kind": "check",
        "id": source_id,
        "path": f"{source_id}.json",
        "sha256": "d" * 64,
        "schema_version": 1,
    })
    candidate["proposals"][0]["regression_gate"]["evidence_ids"] = [source_id]


def test_template_passes_proposal_gate():
    load_module().validate(data(), "propose")


def test_missing_baseline_and_reason_fails():
    module = load_module()
    candidate = data()
    candidate["scope"]["baseline"] = {"cycle_ids": [], "absence_reason": ""}
    with pytest.raises(module.Invalid, match="baseline needs exactly one"):
        module.validate(candidate, "propose")


def test_findings_must_link_to_evidence():
    module = load_module()
    candidate = data()
    candidate["findings"][0]["evidence_ids"] = ["missing"]
    with pytest.raises(module.Invalid, match="lacks valid evidence"):
        module.validate(candidate, "propose")


@pytest.mark.parametrize("forbidden", ["transcript", "prompt", "messages", "tool_arguments"])
def test_raw_session_payloads_are_rejected(forbidden):
    module = load_module()
    candidate = data()
    candidate[forbidden] = "private"
    with pytest.raises(module.Invalid, match="forbidden raw-content key"):
        module.validate(candidate, "propose")


def test_monitoring_needs_separate_intervention_and_passing_regression():
    module = load_module()
    candidate = data()
    candidate["status"] = "monitoring"
    candidate["proposals"][0]["status"] = "implemented"
    with pytest.raises(module.Invalid, match="approved intervention receipt"):
        module.validate(candidate, "monitor")


def test_underpowered_recurrence_cannot_claim_improvement():
    module = load_module()
    candidate = data()
    candidate["status"] = "closed"
    proposal = candidate["proposals"][0]
    proposal["status"] = "closed"
    proposal["intervention"] = {
        "receipt": "DELIVERY.json",
        "sha256": "b" * 64,
        "version_or_revision": "v2",
    }
    proposal["regression_gate"]["status"] = "pass"
    add_regression_evidence(candidate)
    add_source(candidate, "DEL-2")
    proposal["recurrence"].update({
        "decision": "improved",
        "observed_cycle_ids": ["DEL-2"],
        "observed_denominator": 5,
        "observed_value": 0.05,
        "observed_guard_values": {"false_positive_rate": 0.01},
    })
    with pytest.raises(module.Invalid, match="lacks comparable cycles"):
        module.validate(candidate, "close")


def test_improvement_needs_all_guard_metrics():
    module = load_module()
    candidate = data()
    candidate["status"] = "closed"
    proposal = candidate["proposals"][0]
    proposal["status"] = "closed"
    proposal["intervention"] = {"receipt": "DELIVERY.json", "sha256": "b" * 64, "version_or_revision": "v2"}
    proposal["regression_gate"]["status"] = "pass"
    add_regression_evidence(candidate)
    for cycle_id in ("DEL-2", "DEL-3", "DEL-4"):
        add_source(candidate, cycle_id)
    proposal["recurrence"].update({
        "decision": "improved",
        "observed_cycle_ids": ["DEL-2", "DEL-3", "DEL-4"],
        "observed_denominator": 20,
        "observed_value": 0.05,
    })
    with pytest.raises(module.Invalid, match="lacks guard metrics"):
        module.validate(candidate, "close")


def test_linked_source_hash_is_verified(tmp_path):
    module = load_module()
    source = tmp_path / "RUN.json"
    source.write_text("evidence")
    candidate = data()
    candidate["sources"][0]["sha256"] = "0" * 64
    with pytest.raises(module.Invalid, match="hash mismatch"):
        module.verify_hashes(candidate, tmp_path)


@pytest.mark.parametrize("bad_value", [True, float("nan"), float("inf")])
def test_metric_values_must_be_finite_numbers(bad_value):
    module = load_module()
    candidate = data()
    candidate["metrics"][0]["value"] = bad_value
    with pytest.raises(module.Invalid, match="finite number"):
        module.validate(candidate, "propose")


def test_no_change_is_a_valid_outcome_without_proposals():
    module = load_module()
    candidate = data()
    candidate["status"] = "no-change"
    candidate["proposals"] = []
    module.validate(candidate, "propose")


def test_no_change_still_requires_measured_evidence():
    module = load_module()
    candidate = data()
    candidate.update({"status": "no-change", "metrics": [], "findings": [], "proposals": []})
    with pytest.raises(module.Invalid, match="evidence-backed"):
        module.validate(candidate, "propose")


def test_closed_receipt_cannot_pass_proposal_gate():
    module = load_module()
    candidate = data()
    candidate["status"] = "closed"
    with pytest.raises(module.Invalid, match="invalid for propose gate"):
        module.validate(candidate, "propose")


def test_observed_cycles_must_be_unique_and_evidence_linked():
    module = load_module()
    candidate = data()
    candidate["status"] = "monitoring"
    proposal = candidate["proposals"][0]
    proposal["status"] = "implemented"
    proposal["intervention"] = {"receipt": "DELIVERY.json", "sha256": "b" * 64, "version_or_revision": "v2"}
    proposal["regression_gate"]["status"] = "pass"
    add_regression_evidence(candidate)
    proposal["recurrence"]["observed_cycle_ids"] = ["invented", "invented"]
    with pytest.raises(module.Invalid, match="duplicate observed cycles"):
        module.validate(candidate, "monitor")


def test_terminal_improvement_risk_is_supported():
    module = load_module()
    candidate = data()
    candidate["proposals"][0]["risk_tier"] = "terminal"
    module.validate(candidate, "propose")


def test_passing_regression_requires_verified_result_evidence():
    module = load_module()
    candidate = data()
    candidate["proposals"][0]["regression_gate"]["status"] = "pass"
    with pytest.raises(module.Invalid, match="verified result evidence"):
        module.validate(candidate, "propose")


def test_passing_regression_needs_fixture_or_argv_command():
    module = load_module()
    candidate = data()
    add_regression_evidence(candidate)
    gate = candidate["proposals"][0]["regression_gate"]
    gate.update({"status": "pass", "fixture_ids": [], "commands": []})
    with pytest.raises(module.Invalid, match="fixture or command"):
        module.validate(candidate, "propose")


def test_scope_cycles_must_resolve_to_sources():
    module = load_module()
    candidate = data()
    candidate["scope"]["cycle_ids"] = ["UNVERIFIED"]
    with pytest.raises(module.Invalid, match="unverified cycle"):
        module.validate(candidate, "propose")


def test_proposal_ids_are_unique():
    module = load_module()
    candidate = data()
    candidate["proposals"].append(candidate["proposals"][0].copy())
    with pytest.raises(module.Invalid, match="duplicate id"):
        module.validate(candidate, "propose")


def test_retrospective_cannot_claim_change_authority():
    module = load_module()
    candidate = data()
    candidate["scope"]["authority_mode"] = "approved-change"
    with pytest.raises(module.Invalid, match="must remain read-only"):
        module.validate(candidate, "propose")


def test_dated_diary_is_not_a_canonical_promotion_destination():
    module = load_module()
    candidate = data()
    candidate["proposals"][0]["destination"] = "docs/retrospectives/2026-07-10-diary.md"
    with pytest.raises(module.Invalid, match="not a canonical owner"):
        module.validate(candidate, "propose")


def test_comparable_cycle_ids_cannot_alias_one_receipt():
    module = load_module()
    candidate = data()
    for cycle_id in ("DEL-2", "DEL-3", "DEL-4"):
        add_source(candidate, cycle_id)
    for source in candidate["sources"][-3:]:
        source["path"] = "same-RUN.json"
        source["sha256"] = "e" * 64
    candidate["proposals"][0]["recurrence"]["observed_cycle_ids"] = ["DEL-2", "DEL-3", "DEL-4"]
    with pytest.raises(module.Invalid, match="alias the same delivery receipt"):
        module.validate(candidate, "propose")


def test_delivery_source_embedded_id_must_match(tmp_path):
    module = load_module()
    source = tmp_path / "RUN.json"
    source.write_text(json.dumps({"run_id": "OTHER"}))
    candidate = data()
    candidate["sources"][0]["sha256"] = __import__("hashlib").sha256(source.read_bytes()).hexdigest()
    with pytest.raises(module.Invalid, match="embedded identity mismatch"):
        module.verify_hashes(candidate, tmp_path)


def test_delivery_source_must_be_a_valid_canonical_receipt(tmp_path):
    module = load_module()
    source = tmp_path / "RUN.json"
    source.write_text(json.dumps({"schema_version": 1, "contract": "delivery-run", "run_id": "DEL-example"}))
    candidate = data()
    candidate["sources"][0]["sha256"] = __import__("hashlib").sha256(source.read_bytes()).hexdigest()
    with pytest.raises(module.Invalid, match="canonical delivery receipt"):
        module.verify_hashes(candidate, tmp_path)


def test_valid_canonical_delivery_source_is_accepted(tmp_path):
    module = load_module()
    reference_path = ROOT / "skills" / "deliver" / "scripts" / "reference_runs.py"
    spec = importlib.util.spec_from_file_location("retrospect_delivery_fixture", reference_path)
    reference = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(reference)
    delivery = reference.make_reference_run("software", ROOT)
    delivery["run_id"] = "DEL-example"
    delivery["fabric_relationships"]["delivery_run_id"] = "DEL-example"
    for item in delivery["evidence"]:
        if item.get("kind") == "deterministic":
            item["result"]["run_identity"]["run_id"] = "DEL-example"
    source = tmp_path / "RUN.json"
    source.write_text(json.dumps(delivery))
    candidate = data()
    candidate["sources"][0]["sha256"] = __import__("hashlib").sha256(source.read_bytes()).hexdigest()
    module.verify_hashes(candidate, tmp_path)


def _write_policy_bound_delivery_source(workspace, policy_path="delivery-policy.json"):
    reference_path = ROOT / "skills" / "deliver" / "scripts" / "reference_runs.py"
    spec = importlib.util.spec_from_file_location("retrospect_policy_delivery_fixture", reference_path)
    reference = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(reference)
    delivery = reference.make_reference_run("software", ROOT)
    delivery["run_id"] = "DEL-example"
    delivery["fabric_relationships"]["delivery_run_id"] = "DEL-example"
    for item in delivery["evidence"]:
        if item.get("kind") == "deterministic":
            item["result"]["run_identity"]["run_id"] = "DEL-example"
    policy = {
        "schema_version": 1,
        "profiles": {
            "software": {
                "required_evidence": {"deterministic": ["tests"]},
                "required_measures": {"outcome": ["functional-correctness"]},
            }
        },
    }
    raw_policy = json.dumps(policy, sort_keys=True).encode()
    delivery["project_policy"] = {
        "path": policy_path,
        "digest": "sha256:" + hashlib.sha256(raw_policy).hexdigest(),
    }
    source = workspace / "RUN.json"
    source.write_text(json.dumps(delivery))
    candidate = data()
    candidate["sources"][0]["sha256"] = hashlib.sha256(source.read_bytes()).hexdigest()
    return candidate, raw_policy


def test_valid_policy_bound_delivery_source_is_accepted(tmp_path):
    module = load_module()
    candidate, raw_policy = _write_policy_bound_delivery_source(tmp_path)
    (tmp_path / "delivery-policy.json").write_bytes(raw_policy)

    module.verify_hashes(candidate, tmp_path, workspace_root=tmp_path)


def test_policy_bound_delivery_source_requires_existing_policy(tmp_path):
    module = load_module()
    candidate, _ = _write_policy_bound_delivery_source(tmp_path)

    with pytest.raises(module.Invalid, match="project policy is missing"):
        module.verify_hashes(candidate, tmp_path, workspace_root=tmp_path)


def test_policy_bound_delivery_source_cannot_escape_workspace_via_symlink(tmp_path):
    module = load_module()
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    candidate, raw_policy = _write_policy_bound_delivery_source(workspace)
    outside = tmp_path / "outside-policy.json"
    outside.write_bytes(raw_policy)
    (workspace / "delivery-policy.json").symlink_to(outside)

    with pytest.raises(module.Invalid, match="project policy escapes workspace_root"):
        module.verify_hashes(candidate, workspace, workspace_root=workspace)


def test_enclosing_delivery_cycle_and_profile_must_match():
    module = load_module()
    candidate = data()
    with pytest.raises(module.Invalid, match="current delivery cycle"):
        module.validate(candidate, "propose", expected_cycle_id="OTHER")
    with pytest.raises(module.Invalid, match="delivery profile"):
        module.validate(candidate, "propose", expected_profile="research")


def test_baseline_cycles_cannot_be_reused_as_follow_up_evidence():
    module = load_module()
    candidate = data()
    candidate["proposals"][0]["recurrence"]["observed_cycle_ids"] = ["DEL-example"]
    with pytest.raises(module.Invalid, match="distinct from baseline"):
        module.validate(candidate, "propose")
