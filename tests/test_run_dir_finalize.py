import importlib.util
import json
from pathlib import Path
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
INIT = ROOT / "skills" / "orchestrate" / "scripts" / "run_dir_init.sh"
SCRIPT = ROOT / "skills" / "orchestrate" / "scripts" / "run_dir_finalize.py"
SPEC = importlib.util.spec_from_file_location("run_dir_finalize", SCRIPT)
run_dir_finalize = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = run_dir_finalize
SPEC.loader.exec_module(run_dir_finalize)

from _shared.review_ladder import (
    REVIEW_PLAN_COMPLETE_STATUS,
    REVIEW_PLAN_STATUSES,
    SKIPPED_STATUSES,
)
from _shared.review_panel import panel_result, resolve_panel


def init_run(tmp_path):
    run = tmp_path / "run"
    result = subprocess.run([str(INIT), str(run)], text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    return run


def add_manifest_row(run, row):
    with (run / "MANIFEST.md").open("a") as handle:
        handle.write(row + "\n")


def review(review_id, scope, lens, family, tier="flagship", status="complete", substitution_for="", wave=1, reason=None):
    evidence_digest = "sha256:" + __import__("hashlib").sha256((review_id + ":evidence").encode()).hexdigest()
    route_digest = "sha256:" + __import__("hashlib").sha256((review_id + ":route").encode()).hexdigest()
    return {
        "id": review_id, "scope": scope, "lens": lens, "family": family,
        "tier": tier, "status": status, "substitution_for": substitution_for,
        "evidence": {"path": f"reviews/{review_id}.md", "digest": evidence_digest},
        "reason": reason if reason is not None else ("provider unavailable" if status != "complete" else ""),
        "verdict": "approve" if status == "complete" else "",
        "terminal_result": {
            "path": f"reviews/{review_id}.result.json",
            "digest": "sha256:" + __import__("hashlib").sha256((review_id + ":terminal").encode()).hexdigest(),
        } if status == "complete" else None,
        "wave": wave,
        "adapter": "claude" if family == "anthropic" else "codex",
        "adapter_gate": "direct-cli",
        "model": "opus" if status == "complete" else "",
        "catalog_model": "" if status == "complete" else "",
        "route_receipt": {"path": f"reviews/{review_id}.route.json", "digest": route_digest},
        "reviewer_id": review_id,
    }


def substantial_plan(risk="substantial"):
    terminal_lens = "adversarial" if risk == "terminal" else "authority"
    plan = {
        "risk_tier": risk,
        "chair_family": "openai",
        "concurrency_ceiling": 4,
        "panels": [],
        "reviews": [
            review("target-memory", "targeted", "memory", "openai", "workhorse"),
            review("target-routing", "targeted", "routing", "openai", "workhorse"),
            review("target-authority", "targeted", terminal_lens, "anthropic", "workhorse"),
            review("opus-primary", "primary", "whole-change", "anthropic"),
        ],
    }
    if risk == "terminal":
        plan["reviews"].append(
            review(
                "distinct-family-skipped", "primary", "distinct-family", "google",
                status="omitted", reason="not warranted for this change",
            )
        )
    return plan


def bind_complete_reviews(run, plan):
    reviews_dir = run / "reviews"
    reviews_dir.mkdir()
    for row in plan["reviews"]:
        if row["status"] != "complete":
            continue
        evidence = run / row["evidence"]["path"]
        evidence.write_text(row["id"] + ":evidence")
        row["evidence"]["digest"] = "sha256:" + __import__("hashlib").sha256(evidence.read_bytes()).hexdigest()
        terminal = run / row["terminal_result"]["path"]
        terminal.write_text(json.dumps({
            "id": row["id"], "attempt_id": "attempt-" + row["id"], "kind": "complete",
            "summary": row["id"] + ":complete", "verdict": row["verdict"],
        }))
        row["terminal_result"]["digest"] = "sha256:" + __import__("hashlib").sha256(terminal.read_bytes()).hexdigest()
        route = run / row["route_receipt"]["path"]
        endpoint = "anthropic" if row["adapter"] == "claude" else "openai"
        route.write_text(json.dumps({
            "status": "ok", "adapter": row["adapter"], "resolved_model": row["model"],
            "adapter_gate": row["adapter_gate"],
            "catalog_model": row["catalog_model"], "model_family": row["family"],
            "provider_family": row["family"], "endpoint_provider": endpoint,
            "orchestrator_family": plan["chair_family"] or row["family"],
            "read_only_guarantee": "enforced", "route_alias": row["tier"],
            "reviewer_id": row["reviewer_id"], "id": row["id"],
            "attempt_id": "attempt-" + row["id"], "terminal_observed": True,
            "exit": 0, "output_path": row["terminal_result"]["path"],
            "output_sha256": row["terminal_result"]["digest"],
            "cross_family": row["scope"] == "primary",
            "certification_eligible": row["scope"] == "primary",
        }))
        row["route_receipt"]["digest"] = "sha256:" + __import__("hashlib").sha256(route.read_bytes()).hexdigest()


def panel(panel_id, preset, membership, reviews, output):
    spec = resolve_panel(preset, membership=membership)
    members = [row for row in reviews if row["id"] in membership]
    return {
        "id": panel_id,
        "spec": spec,
        "result": panel_result(spec, members, output),
    }


def test_real_run_rejects_unavailable_other_primary_with_distinct_family_reviews(tmp_path):
    plan = substantial_plan()
    plan["reviews"][-1] = review(
        "primary-down", "primary", "other-primary", "anthropic", status="unavailable"
    )
    plan["reviews"].extend([
        review("sub-google", "primary", "whole-change-a", "google", substitution_for="other-primary", wave=2),
        review("sub-xai", "primary", "whole-change-b", "xai", substitution_for="other-primary", wave=2),
    ])
    bind_complete_reviews(tmp_path, plan)

    assert any("passing other-primary" in error for error in run_dir_finalize._validate_review_plan(plan, tmp_path))


def test_real_run_allows_recorded_targeted_omission(tmp_path):
    plan = substantial_plan()
    plan["reviews"] = [plan["reviews"][0], plan["reviews"][1], plan["reviews"][3]]
    plan["reviews"].append(
        review(
            "target-omitted", "targeted", "authority", "google",
            status="omitted", substitution_for="targeted-lens", wave=2,
        )
    )
    bind_complete_reviews(tmp_path, plan)

    assert run_dir_finalize._validate_review_plan(plan, tmp_path) == []


def test_real_run_allows_recorded_crucial_second_family_omission(tmp_path):
    plan = substantial_plan("crucial")
    plan["reviews"].append(
        review(
            "second-family-omitted", "primary", "terminal-challenge", "google",
            status="omitted", substitution_for="additional-distinct-family", wave=2,
        )
    )
    bind_complete_reviews(tmp_path, plan)

    assert run_dir_finalize._validate_review_plan(plan, tmp_path) == []


def test_real_run_complete_primary_still_requires_route_receipt(tmp_path):
    plan = substantial_plan()
    bind_complete_reviews(tmp_path, plan)
    (tmp_path / plan["reviews"][-1]["route_receipt"]["path"]).unlink()

    errors = run_dir_finalize._validate_review_plan(plan, tmp_path)

    assert any("route_receipt is missing or does not match" in error for error in errors)


def test_substantial_review_topology_is_machine_checked():
    assert run_dir_finalize._validate_review_plan(substantial_plan()) == []
    plan = substantial_plan()
    other_primary = plan["reviews"][3]
    plan["reviews"] = [plan["reviews"][0], plan["reviews"][1], other_primary]
    assert run_dir_finalize._validate_review_plan(plan) == []
    plan["reviews"] = [plan["reviews"][0], other_primary]
    assert any("targeted lenses" in error for error in run_dir_finalize._validate_review_plan(plan))


def test_other_primary_review_requires_flagship_strength():
    plan = substantial_plan()
    plan["reviews"][3]["tier"] = "workhorse"
    assert any("flagship strength" in error for error in run_dir_finalize._validate_review_plan(plan))


def test_other_primary_unavailability_remains_load_bearing():
    plan = substantial_plan()
    plan["reviews"][-1] = review("primary-down", "primary", "other-primary", "anthropic", status="unavailable")
    plan["reviews"].extend([
        review("sub-google", "primary", "whole-change-a", "google", substitution_for="other-primary", wave=2),
        review("sub-xai", "primary", "whole-change-b", "xai", substitution_for="other-primary", wave=2),
    ])
    assert any("passing other-primary" in error for error in run_dir_finalize._validate_review_plan(plan))


def test_crucial_review_topology_records_unavailable_second_family():
    plan = substantial_plan("crucial")
    assert any(
        "distinct-family" in error for error in run_dir_finalize._validate_review_plan(plan)
    )
    plan["reviews"].append(
        review("second-family-down", "primary", "terminal-challenge", "google", status="unavailable", substitution_for="additional-distinct-family", wave=2)
    )
    assert run_dir_finalize._validate_review_plan(plan) == []


def test_crucial_review_topology_blocks_with_no_distinct_family_or_recorded_skip():
    plan = substantial_plan("crucial")
    assert any(
        "distinct-family review or recorded skip" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_terminal_review_topology_blocks_with_no_distinct_family_or_recorded_skip():
    plan = substantial_plan("terminal")
    plan["reviews"] = plan["reviews"][:-1]
    assert any(
        "distinct-family review or recorded skip" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_terminal_review_topology_uses_adversarial_pressure_without_fixed_provider_count():
    plan = substantial_plan("terminal")
    assert run_dir_finalize._validate_review_plan(plan) == []
    plan["reviews"][2]["lens"] = "authority"
    assert any("adversarial" in error for error in run_dir_finalize._validate_review_plan(plan))


def test_review_topology_rejects_invalid_concurrency_ceiling():
    plan = substantial_plan()
    plan["concurrency_ceiling"] = 0
    assert any("concurrency_ceiling" in error for error in run_dir_finalize._validate_review_plan(plan))
    plan = substantial_plan()
    plan["concurrency_ceiling"] = 3
    assert any("observed wave" in error for error in run_dir_finalize._validate_review_plan(plan))


def test_review_topology_binds_account_default_route_and_review_evidence(tmp_path):
    evidence = tmp_path / "review.md"
    evidence.write_text("review output")
    route = tmp_path / "route.json"
    route.write_text(json.dumps({
        "adapter": "codex", "adapter_gate": "direct-cli",
        "resolved_model": "", "catalog_model": "gpt-5.6-sol",
        "model_family": "openai", "model_selection": "account-default",
        "status": "ok", "route_alias": "flagship", "reviewer_id": "account-default",
    }))
    row = review("account-default", "targeted", "correctness", "openai")
    row.update({
        "model": "", "catalog_model": "gpt-5.6-sol",
        "evidence": {"path": evidence.name, "digest": "sha256:" + __import__("hashlib").sha256(evidence.read_bytes()).hexdigest()},
        "route_receipt": {"path": route.name, "digest": "sha256:" + __import__("hashlib").sha256(route.read_bytes()).hexdigest()},
    })
    terminal = tmp_path / row["terminal_result"]["path"]
    terminal.parent.mkdir(parents=True, exist_ok=True)
    terminal.write_text(json.dumps({
        "id": row["id"], "attempt_id": "attempt-" + row["id"], "kind": "complete",
        "summary": "account default complete", "verdict": row["verdict"],
    }))
    row["terminal_result"]["digest"] = "sha256:" + __import__("hashlib").sha256(terminal.read_bytes()).hexdigest()
    route_value = json.loads(route.read_text())
    route_value.update({
        "id": row["id"], "attempt_id": "attempt-" + row["id"], "terminal_observed": True,
        "exit": 0, "output_path": row["terminal_result"]["path"],
        "output_sha256": row["terminal_result"]["digest"],
    })
    route.write_text(json.dumps(route_value))
    row["route_receipt"]["digest"] = "sha256:" + __import__("hashlib").sha256(route.read_bytes()).hexdigest()
    plan = {
        "risk_tier": "routine", "chair_family": "", "concurrency_ceiling": 1,
        "panels": [], "reviews": [row],
    }
    assert run_dir_finalize._validate_review_plan(plan, tmp_path) == []
    route_value = json.loads(route.read_text())
    route_value["status"] = "error"
    route.write_text(json.dumps(route_value))
    row["route_receipt"]["digest"] = "sha256:" + __import__("hashlib").sha256(route.read_bytes()).hexdigest()
    assert any("identity does not match" in error for error in run_dir_finalize._validate_review_plan(plan, tmp_path))
    route_value["status"] = "ok"
    route_value["reviewer_id"] = "different-reviewer"
    route.write_text(json.dumps(route_value))
    row["route_receipt"]["digest"] = "sha256:" + __import__("hashlib").sha256(route.read_bytes()).hexdigest()
    assert any("identity does not match" in error for error in run_dir_finalize._validate_review_plan(plan, tmp_path))


def test_review_topology_rejects_malformed_fields_and_symlink_escape(tmp_path):
    plan = substantial_plan()
    plan["reviews"][0]["lens"] = []
    assert any("lens is required" in error for error in run_dir_finalize._validate_review_plan(plan))
    run = tmp_path / "run"
    run.mkdir()
    outside = tmp_path / "outside.md"
    outside.write_text("outside")
    (run / "review.md").symlink_to(outside)
    route = run / "route.json"
    route.write_text(json.dumps({
        "status": "ok", "adapter": "codex", "adapter_gate": "direct-cli",
        "resolved_model": "gpt-test",
        "catalog_model": "", "model_family": "openai", "route_alias": "workhorse", "reviewer_id": "escaped",
    }))
    row = review("escaped", "targeted", "correctness", "openai", tier="workhorse")
    row.update({
        "model": "gpt-test", "catalog_model": "",
        "evidence": {"path": "review.md", "digest": "sha256:" + __import__("hashlib").sha256(outside.read_bytes()).hexdigest()},
        "route_receipt": {"path": "route.json", "digest": "sha256:" + __import__("hashlib").sha256(route.read_bytes()).hexdigest()},
    })
    routine = {
        "risk_tier": "routine", "chair_family": "", "concurrency_ceiling": 1,
        "panels": [], "reviews": [row],
    }
    assert any("evidence is missing" in error for error in run_dir_finalize._validate_review_plan(routine, run))


def test_targeted_reviews_require_distinct_reviewer_and_artifact_identity():
    plan = substantial_plan()
    shared_evidence = plan["reviews"][0]["evidence"]
    shared_route = plan["reviews"][0]["route_receipt"]
    for row in plan["reviews"][:3]:
        row["reviewer_id"] = "same-reviewer"
        row["evidence"] = shared_evidence
        row["route_receipt"] = shared_route
    errors = run_dir_finalize._validate_review_plan(plan)
    assert any("distinct reviewer_id" in error for error in errors)
    assert any("distinct evidence" in error for error in errors)
    assert any("distinct route_receipt" in error for error in errors)
    plan = substantial_plan()
    evidence_digest = plan["reviews"][0]["evidence"]["digest"]
    route_digest = plan["reviews"][0]["route_receipt"]["digest"]
    for index, row in enumerate(plan["reviews"][:3]):
        row["evidence"] = {"path": f"reviews/copy-{index}.md", "digest": evidence_digest}
        row["route_receipt"] = {"path": f"reviews/copy-{index}.route.json", "digest": route_digest}
    errors = run_dir_finalize._validate_review_plan(plan)
    assert any("distinct evidence" in error for error in errors)
    assert any("distinct route_receipt" in error for error in errors)


@pytest.mark.parametrize(("family", "tier"), (("openai", "flagship"), ("anthropic", "flagship")))
def test_primary_family_recorded_omission_does_not_satisfy_distinct_family(family, tier):
    plan = substantial_plan("crucial")
    plan["reviews"].append(
        review("invalid-extra", "primary", "terminal-challenge", family, tier=tier, status="omitted", substitution_for="additional-distinct-family", wave=0)
    )
    assert any(
        "distinct-family review or recorded skip" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_recorded_distinct_family_skip_counts_regardless_of_tier():
    plan = substantial_plan("crucial")
    plan["reviews"].append(
        review("invalid-extra", "primary", "terminal-challenge", "google", tier="workhorse", status="omitted", substitution_for="additional-distinct-family", wave=0)
    )
    assert run_dir_finalize._validate_review_plan(plan) == []


def test_review_plan_status_vocabulary_matches_the_shared_ladder():
    shared_statuses = SKIPPED_STATUSES | {REVIEW_PLAN_COMPLETE_STATUS}

    for status in shared_statuses:
        plan = {
            "risk_tier": "routine",
            "chair_family": "",
            "concurrency_ceiling": 1,
            "panels": [],
            "reviews": [
                review(
                    f"shared-{status}",
                    "targeted",
                    "status-vocabulary",
                    "openai",
                    status=status,
                )
            ],
        }

        assert not any(
            ".status is invalid" in error
            for error in run_dir_finalize._validate_review_plan(plan)
        ), status

    assert REVIEW_PLAN_STATUSES == shared_statuses

    excluded_status = "skipped"
    assert excluded_status not in REVIEW_PLAN_STATUSES
    plan["reviews"][0] = review(
        f"excluded-{excluded_status}",
        "targeted",
        "status-vocabulary",
        "openai",
        status=excluded_status,
    )
    assert any(
        ".status is invalid" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_review_plan_and_review_records_remain_closed_after_panel_widening():
    plan = substantial_plan()
    without_panels = dict(plan)
    del without_panels["panels"]
    assert any(
        "closed review topology schema" in error
        for error in run_dir_finalize._validate_review_plan(without_panels)
    )

    del plan["reviews"][0]["adapter_gate"]
    assert any(
        "closed review record schema" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


@pytest.mark.parametrize("adapter_gate", ("", "socket", [], None))
def test_review_adapter_gate_is_explicit_and_closed(adapter_gate):
    plan = substantial_plan()
    plan["reviews"][0]["adapter_gate"] = adapter_gate

    assert any(
        ".adapter_gate is invalid" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_review_route_receipt_binds_adapter_gate(tmp_path):
    plan = substantial_plan()
    bind_complete_reviews(tmp_path, plan)
    route_path = tmp_path / plan["reviews"][0]["route_receipt"]["path"]
    route = json.loads(route_path.read_text())
    route["adapter_gate"] = "fabric"
    route_path.write_text(json.dumps(route))
    plan["reviews"][0]["route_receipt"]["digest"] = (
        "sha256:" + __import__("hashlib").sha256(route_path.read_bytes()).hexdigest()
    )

    assert any(
        "identity does not match" in error
        for error in run_dir_finalize._validate_review_plan(plan, tmp_path)
    )


def test_panel_records_are_closed_and_reference_existing_reviews():
    plan = substantial_plan()
    membership = [row["id"] for row in plan["reviews"]]
    plan["panels"] = [
        panel(
            "architecture-council",
            "council",
            membership,
            plan["reviews"],
            {
                "agreements": ["Keep dispatch chair-owned."],
                "conflicts": ["The naming remains contested."],
                "minority_views": ["Prefer the existing name."],
            },
        )
    ]

    assert run_dir_finalize._validate_review_plan(plan) == []
    plan["panels"][0]["extra"] = True
    assert any(
        "closed panel record schema" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_panel_member_must_reference_a_review_record():
    plan = substantial_plan()
    spec = resolve_panel(
        "breadth",
        membership=["target-memory", "missing-review"],
    )
    plan["panels"] = [{
        "id": "breadth",
        "spec": spec,
        "result": {
            "shortfall": None,
            "output": {"findings": ["A"]},
        },
    }]

    assert any(
        "unknown review id" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_panel_shortfall_is_explicit_but_does_not_block():
    plan = substantial_plan()
    for row in plan["reviews"][2:]:
        row["status"] = "unavailable"
        row["reason"] = "provider unavailable"
        row["model"] = ""
    membership = [row["id"] for row in plan["reviews"]]
    plan["panels"] = [
        panel(
            "degraded-council",
            "council",
            membership,
            plan["reviews"],
            {
                "agreements": [],
                "conflicts": ["Only two members answered."],
                "minority_views": [],
            },
        )
    ]

    errors = run_dir_finalize._validate_review_plan(plan)

    assert not any("panel" in error for error in errors)
    assert plan["panels"][0]["result"]["shortfall"] == {
        "minimum_members": 3,
        "completed_members": 2,
        "missing_members": 1,
    }


def test_panel_result_must_match_declared_reducer_and_membership():
    plan = substantial_plan()
    membership = [row["id"] for row in plan["reviews"]]
    plan["panels"] = [
        panel(
            "architecture-council",
            "council",
            membership,
            plan["reviews"],
            {"agreements": [], "conflicts": [], "minority_views": []},
        )
    ]
    plan["panels"][0]["result"]["output"] = {"findings": ["wrong reducer"]}

    assert any(
        "result.output is invalid" in error
        for error in run_dir_finalize._validate_review_plan(plan)
    )


def test_fresh_run_receipt_declares_empty_panels(tmp_path):
    run = init_run(tmp_path)
    receipt = json.loads((run / "RUN_RECEIPT.json").read_text())

    assert receipt["review_plan"]["panels"] == []


@pytest.mark.parametrize(("field", "value"), (("risk_tier", []), ("chair_family", [])))
def test_review_topology_malformed_top_level_values_fail_closed(field, value):
    plan = substantial_plan()
    plan[field] = value
    assert run_dir_finalize._validate_review_plan(plan)


@pytest.mark.parametrize("field", ("scope", "tier", "status"))
def test_review_topology_malformed_review_enums_fail_closed(field):
    plan = substantial_plan()
    plan["reviews"][0][field] = []
    assert run_dir_finalize._validate_review_plan(plan)


def test_failed_terminalisation_requires_reason(tmp_path):
    run = init_run(tmp_path)
    assert run_dir_finalize.main([str(run), "--status", "failed"]) == 1
    assert run_dir_finalize.main([str(run), "--status", "failed", "--reason", "reviewer timeout"]) == 0


@pytest.mark.parametrize(
    ("status", "reason"),
    (("failed", "worker crashed"), ("failed", "worker returned null"), ("cancelled", "worker result missing")),
)
def test_non_success_terminalisation_preserves_possible_worker_partial(tmp_path, status, reason):
    run = init_run(tmp_path)
    partial = run / "findings" / "partial-scan.md"
    partial.write_text("worker crashed mid-write")
    assert run_dir_finalize.main([str(run), "--status", status, "--reason", reason]) == 0
    assert partial.exists()
    receipt = json.loads((run / "RUN_RECEIPT.json").read_text())
    assert receipt["unclassified_paths"] == ["findings/partial-scan.md"]


def test_success_requires_closed_final_gate(tmp_path):
    run = init_run(tmp_path)
    assert run_dir_finalize.main([str(run), "--status", "succeeded"]) == 1


def test_pruning_is_dry_run_then_removes_only_classified_ephemeral(tmp_path):
    run = init_run(tmp_path)
    raw = run / "findings" / "raw.txt"
    raw.write_text("duplicate payload")
    add_manifest_row(run, "| A1 | findings/raw.txt | retry noise | worker | 2026-07-10 | retired | ephemeral | - |")
    args = [str(run), "--status", "failed", "--reason", "bounded failure", "--prune-ephemeral"]
    assert run_dir_finalize.main(args) == 0
    assert raw.exists()
    assert run_dir_finalize.main(args + ["--apply"]) == 0
    assert not raw.exists()


def test_manifest_rejects_parent_path(tmp_path):
    run = init_run(tmp_path)
    add_manifest_row(run, "| A1 | ../outside.txt | escape | worker | 2026-07-10 | verified | evidence | - |")
    errors, _ = run_dir_finalize.validate(run, "failed", "bad manifest")
    assert any("path escapes run directory" in error for error in errors)


def test_terminalisation_requires_owned_panes_to_be_closed_or_handed_off(tmp_path):
    run = init_run(tmp_path)
    receipt = run / "RUN_RECEIPT.json"
    data = json.loads(receipt.read_text())
    data["owned_panes"] = [{"pane_id": "w1:p2", "role": "review"}]
    receipt.write_text(json.dumps(data))
    errors, _ = run_dir_finalize.validate(run, "failed", "cancelled")
    assert any("run-owned panes/resources" in error for error in errors)


def test_handed_off_panes_require_typed_acknowledged_evidence(tmp_path):
    run = init_run(tmp_path)
    receipt = run / "RUN_RECEIPT.json"
    data = json.loads(receipt.read_text())
    data["handed_off_panes"] = ["not-a-handoff"]
    receipt.write_text(json.dumps(data))
    errors, _ = run_dir_finalize.validate(run, "failed", "handoff")
    assert any("structured handoff record" in error for error in errors)


def test_paired_receipt_must_preserve_compaction_and_lease_state(tmp_path):
    run = init_run(tmp_path)
    receipt = run / "RUN_RECEIPT.json"
    data = json.loads(receipt.read_text())
    assignment = run / "decisions.md"
    assignment_digest = __import__("hashlib").sha256(assignment.read_bytes()).hexdigest()
    def stage_evidence(stage):
        values = {}
        for kind in ("assignment", "acknowledgement", "output"):
            path = run / "findings" / f"{stage}-{kind}.md"
            path.write_text(f"{stage}-{kind}")
            values[f"{kind}_path"] = path.relative_to(run).as_posix()
            values[f"{kind}_sha256"] = __import__("hashlib").sha256(path.read_bytes()).hexdigest()
        return values
    (run / "LEASE.json").write_text(json.dumps({
        "schema_version": 1, "status": "released", "holder": "", "previous_holder": "openai-session-1",
        "generation": 2, "updated_at": "2026-07-10T00:00:00Z", "expires_at": "",
    }))
    data["pair"] = {
        "mode": "paired-primary", "chair_family": "openai", "chair_id": "openai-session-1",
        "peer_family": "anthropic", "peer_id": "anthropic-session-1",
        "status": "complete", "degradation_reason": "", "lease_path": "LEASE.json",
        "lease_generation": 2, "checkpoint_generation": 2,
        "current_stage": "implementation", "in_flight": [],
        "assignment_artifacts": [{"path": "decisions.md", "sha256": assignment_digest}],
        "stage_ledger": [
            {"stage": "scope", "owner_family": "openai", "peer_family": "anthropic", "generation": 1,
             "status": "complete", "acknowledged": True, "base_revision": "a", "result_revision": "b",
             "checks": [{"command": "spec-check", "exit_code": 0}], "human_gates": ["spec"], **stage_evidence("scope")},
            {"stage": "implementation", "owner_family": "anthropic", "peer_family": "openai", "generation": 2,
             "status": "complete", "acknowledged": True, "base_revision": "b", "result_revision": "c",
             "checks": [{"command": "pytest", "exit_code": 0}], "human_gates": [], **stage_evidence("implementation")},
        ],
        "handoff_generation": 0,
    }
    receipt.write_text(json.dumps(data))
    errors, _ = run_dir_finalize.validate(run, "failed", "demo")
    assert errors == []
    data["pair"]["in_flight"] = ["worker"]
    receipt.write_text(json.dumps(data))
    errors, _ = run_dir_finalize.validate(run, "failed", "demo")
    assert any("empty in_flight" in error for error in errors)


def test_successful_terminalisation_closes_a_complete_run(tmp_path):
    run = init_run(tmp_path)
    receipt = run / "RUN_RECEIPT.json"
    data = json.loads(receipt.read_text())
    data["task"] = "context hygiene test"
    receipt.write_text(json.dumps(data))
    (run / "SYNTHESIS.md").write_text("Verified synthesis\n")
    gate = run / "FINAL_GATE.md"
    rewritten = []
    for line in gate.read_text().splitlines():
        if line.startswith("|") and not line.startswith(("| gate", "|---")):
            cells = [cell.strip() for cell in line.strip("|").split("|")]
            cells[1] = "PASS"
            cells[2] = "verified"
            line = "| " + " | ".join(cells) + " |"
        rewritten.append(line)
    gate.write_text("\n".join(rewritten) + "\n")
    assert run_dir_finalize.main([str(run), "--status", "succeeded"]) == 0
    assert json.loads(receipt.read_text())["status"] == "succeeded"


def test_success_rejects_incomplete_gate_schema(tmp_path):
    run = init_run(tmp_path)
    receipt = run / "RUN_RECEIPT.json"
    data = json.loads(receipt.read_text())
    data["task"] = "incomplete gate"
    receipt.write_text(json.dumps(data))
    (run / "SYNTHESIS.md").write_text("done")
    (run / "FINAL_GATE.md").write_text("| gate | status | evidence |\n|---|---|---|\n| arbitrary | PASS | none |\n")
    errors, _ = run_dir_finalize.validate(run, "succeeded", None)
    assert any("missing gates" in error for error in errors)


def test_prune_preserves_ephemeral_file_referenced_by_retained_evidence(tmp_path):
    run = init_run(tmp_path)
    summary = run / "findings" / "summary.md"
    raw = run / "findings" / "raw.txt"
    summary.write_text("Evidence: findings/raw.txt")
    raw.write_text("raw")
    add_manifest_row(run, "| S1 | findings/summary.md | summary | worker | 2026-07-10 | verified | evidence | - |")
    add_manifest_row(run, "| R1 | findings/raw.txt | raw | worker | 2026-07-10 | retired | ephemeral | - |")
    errors, rows = run_dir_finalize.validate(run, "failed", "demo")
    assert errors == []
    assert run_dir_finalize.prune_candidates(run, rows) == []


def test_prune_preserves_relative_markdown_link_from_retained_evidence(tmp_path):
    run = init_run(tmp_path)
    summary = run / "findings" / "summary.md"
    raw = run / "findings" / "raw.txt"
    summary.write_text("[raw evidence](raw.txt)")
    raw.write_text("raw")
    add_manifest_row(run, "| S1 | findings/summary.md | summary | worker | 2026-07-10 | verified | evidence | - |")
    add_manifest_row(run, "| R1 | findings/raw.txt | raw | worker | 2026-07-10 | retired | ephemeral | - |")
    errors, rows = run_dir_finalize.validate(run, "failed", "demo")
    assert errors == []
    assert run_dir_finalize.prune_candidates(run, rows) == []


def test_duplicate_manifest_path_cannot_downgrade_retained_evidence(tmp_path):
    run = init_run(tmp_path)
    report = run / "findings" / "report.md"
    report.write_text("verified evidence")
    add_manifest_row(run, "| A1 | findings/report.md | report | reviewer | 2026-07-10 | verified | capsule | - |")
    add_manifest_row(run, "| A2 | findings/report.md | alias | worker | 2026-07-10 | superseded | ephemeral | - |")
    errors, rows = run_dir_finalize.validate(run, "failed", "demo")
    assert any("duplicate manifest path" in error for error in errors)
    assert run_dir_finalize.prune_candidates(run, rows) == []
    assert report.exists()


def test_receipt_schema_is_validated(tmp_path):
    run = init_run(tmp_path)
    (run / "RUN_RECEIPT.json").write_text("[]")
    errors, _ = run_dir_finalize.validate(run, "failed", "bad")
    assert "root must be an object" in errors[0]


def test_receipt_path_lists_reject_non_strings(tmp_path):
    run = init_run(tmp_path)
    receipt = run / "RUN_RECEIPT.json"
    data = json.loads(receipt.read_text())
    data["pruned_paths"] = [{}]
    receipt.write_text(json.dumps(data))
    errors, _ = run_dir_finalize.validate(run, "failed", "bad")
    assert "receipt pruned_paths entries must be strings" in errors
