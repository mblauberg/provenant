import hashlib
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"


def run_cli(workspace: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(PRODUCER), *args],
        cwd=workspace,
        text=True,
        capture_output=True,
        check=False,
    )


def load_producer():
    spec = importlib.util.spec_from_file_location("delivery_receipt_under_test", PRODUCER)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def route_receipt(
    *,
    adapter: str = "native-subagent",
    family: str = "openai",
    model: str = "runtime-model",
    reviewer_id: str = "reviewer-1",
) -> dict[str, object]:
    return {
        "status": "ok",
        "adapter": adapter,
        "adapter_gate": "fabric",
        "reviewer_id": reviewer_id,
        "resolved_model": model,
        "catalog_model": "",
        "model_family": family,
        "route_alias": "flagship",
        "cross_family": False,
        "certification_eligible": True,
    }


def routine_assessment() -> dict[str, str]:
    return {
        "blast_radius": "local",
        "reversibility": "easy",
        "data_sensitivity": "public",
        "migration": "none",
        "oracle_quality": "strong",
        "external_effects": "none",
        "critical_surface": "none",
    }


def authority(run_id: str) -> dict[str, object]:
    return {
        "schema_version": 2,
        "approved_by": "human-owner",
        "evidence": "authority-approval",
        "evidence_digest": "sha256:" + "a" * 64,
        "workspace_roots": ["."],
        "expires_at": "2099-01-01T00:00:00Z",
        "allowed_source_paths": ["."],
        "allowed_artifact_paths": [".", f".agent-run/{run_id}"],
        "allowed_fabric_operations": [],
        "denied_paths": [],
        "denied_fabric_operations": [],
        "prohibited_actions": ["external-publish", "deployment", "irreversible-action"],
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
    }


def init_args(run_id: str = "DEL-TEST") -> list[str]:
    return [
        "init",
        "--run-dir",
        f".agent-run/{run_id}",
        "--run-id",
        run_id,
        "--profile",
        "software",
        "--chair-family",
        "openai",
        "--risk-assessment",
        json.dumps(routine_assessment()),
        "--intent",
        "intent.md",
        "--authority",
        json.dumps(authority(run_id)),
    ]


def initialise(workspace: Path, run_id: str = "DEL-TEST") -> Path:
    (workspace / "intent.md").write_text("# Intent\n")
    result = run_cli(workspace, *init_args(run_id))
    assert result.returncode == 0, result.stderr
    return workspace / ".agent-run" / run_id


def add_artifact(
    workspace: Path,
    run_dir: Path,
    artifact_id: str,
    path: str,
    *,
    artifact_class: str = "evidence",
    artifact_type: str = "evidence",
    media_type: str = "application/json",
) -> subprocess.CompletedProcess[str]:
    return run_cli(
        workspace,
        "artifact",
        "add",
        "--run-dir",
        str(run_dir),
        "--id",
        artifact_id,
        "--path",
        path,
        "--class",
        artifact_class,
        "--media-type",
        media_type,
        "--artifact-type",
        artifact_type,
        "--owner",
        "delivery-chair",
        "--retention",
        "risk-policy" if artifact_class == "evidence" else "project-policy",
    )


def test_init_derives_tier_and_writes_timestamped_draft_history(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")

    result = run_cli(tmp_path, *init_args())

    assert result.returncode == 0, result.stderr
    receipt = json.loads((tmp_path / ".agent-run" / "DEL-TEST" / "RUN.json").read_text())
    assert receipt["risk_tier"] == "routine"
    assert receipt["status"] == "draft"
    assert receipt["state_history"][0]["state"] == "draft"
    assert receipt["state_history"][0]["risk_tier"] == "routine"
    assert receipt["state_history"][0]["at"].endswith("Z")
    assert receipt["evidence"] == []
    assert (tmp_path / ".agent-run" / "DEL-TEST" / ".RUN.lock").exists()


def test_init_refuses_invalid_run_identifier(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")
    args = init_args("invalid id")

    result = run_cli(tmp_path, *args)

    assert result.returncode == 1
    assert "run-id must be a bounded stable identifier" in result.stderr
    assert not (tmp_path / ".agent-run" / "invalid id" / "RUN.json").exists()


def test_bind_and_artifact_add_mutate_only_through_atomic_receipt_write(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "outcome.py").write_text("print('ok')\n")
    design_path = tmp_path / "design.json"
    design = {
        "status": "draft",
        "artifact_id": "outcome",
        "digest": "sha256:" + "0" * 64,
        "approver": "",
        "evidence": "",
        "alternatives": ["retain-current-loop"],
        "failure_analysis": "bounded",
        "containment": "revert",
        "one_way_doors": [],
    }
    design_path.write_text(json.dumps(design))

    added = run_cli(
        tmp_path,
        "artifact",
        "add",
        "--run-dir",
        str(run_dir),
        "--id",
        "outcome",
        "--path",
        "outcome.py",
        "--class",
        "canonical",
        "--media-type",
        "text/x-python",
        "--artifact-type",
        "source",
        "--owner",
        "delivery-chair",
        "--retention",
        "project-policy",
    )
    bound = run_cli(
        tmp_path,
        "bind",
        "--run-dir",
        str(run_dir),
        "--section",
        "design",
        "--from",
        str(design_path),
    )

    assert added.returncode == 0, added.stderr
    assert bound.returncode == 0, bound.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    outcome = next(item for item in receipt["artifacts"] if item["id"] == "outcome")
    assert outcome["digest"] == "sha256:" + hashlib.sha256(
        (tmp_path / "outcome.py").read_bytes()
    ).hexdigest()
    assert receipt["design"] == design


def test_artifact_add_refuses_invalid_id_and_escaping_path(tmp_path):
    run_dir = initialise(tmp_path)
    invalid_id = run_cli(
        tmp_path,
        "artifact",
        "add",
        "--run-dir",
        str(run_dir),
        "--id",
        "free text",
        "--path",
        "intent.md",
        "--class",
        "canonical",
        "--media-type",
        "text/markdown",
        "--artifact-type",
        "documentation",
        "--owner",
        "chair",
        "--retention",
        "project-policy",
    )
    escaping = run_cli(
        tmp_path,
        "artifact",
        "add",
        "--run-dir",
        str(run_dir),
        "--id",
        "outside",
        "--path",
        "../outside",
        "--class",
        "canonical",
        "--media-type",
        "text/plain",
        "--artifact-type",
        "documentation",
        "--owner",
        "chair",
        "--retention",
        "project-policy",
    )

    assert invalid_id.returncode == 1
    assert "artifact id must be a bounded stable identifier" in invalid_id.stderr
    assert escaping.returncode == 1
    assert "must be safe and workspace-relative" in escaping.stderr


def test_evidence_run_executes_command_and_rebuilds_deterministic_bundle(tmp_path):
    run_dir = initialise(tmp_path)
    bundle_path = tmp_path / ".agent-run" / "DEL-TEST" / "evidence.json"
    bundle_path.write_text(json.dumps({
        "schema_version": 1,
        "contract": "deterministic-evidence-bundle",
        "checks": [],
    }))
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0

    result = run_cli(
        tmp_path,
        "evidence",
        "run",
        "--run-dir",
        str(run_dir),
        "--id",
        "tests",
        "--gate",
        "tests",
        "--artifact-id",
        "evidence-bundle",
        "--source",
        "intent.md",
        "--",
        sys.executable,
        "-c",
        "print('green')",
    )

    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    evidence = next(item for item in receipt["evidence"] if item["id"] == "tests")
    artifact = next(item for item in receipt["artifacts"] if item["id"] == "evidence-bundle")
    bundle = json.loads((tmp_path / artifact["path"]).read_text())
    assert evidence["status"] == "pass"
    assert evidence["result"]["exit_code"] == 0
    assert evidence["stdout"] == "green\n"
    assert evidence["result"]["receipt_digest"] == artifact["digest"]
    assert bundle["checks"] == [{
        "id": "tests",
        "gate": "tests",
        "status": "pass",
        "method": f"{sys.executable} -c 'print('\"'\"'green'\"'\"')'",
        "source_paths": ["intent.md"],
        "exit_code": 0,
    }]


def test_evidence_run_refuses_free_text_id_and_supplied_exit_code(tmp_path):
    run_dir = initialise(tmp_path)
    bundle_path = tmp_path / ".agent-run" / "DEL-TEST" / "evidence.json"
    bundle_path.write_text('{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n')
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    common = [
        "evidence", "run", "--run-dir", str(run_dir), "--gate", "tests",
        "--artifact-id", "evidence-bundle", "--source", "intent.md",
    ]

    invalid_id = run_cli(
        tmp_path, *common, "--id", "free text", "--", sys.executable, "-c", "pass",
    )
    supplied = run_cli(
        tmp_path, *common, "--id", "tests", "--exit-code", "0",
        "--", sys.executable, "-c", "pass",
    )

    assert invalid_id.returncode == 1
    assert "evidence id must be a bounded stable identifier" in invalid_id.stderr
    assert supplied.returncode == 2
    assert "unrecognized arguments: --exit-code" in supplied.stderr


def test_evidence_run_preflights_bundle_before_executing_command(tmp_path):
    run_dir = initialise(tmp_path)
    marker = tmp_path / "should-not-exist"

    result = run_cli(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "intent", "--source", "intent.md",
        "--", sys.executable, "-c", f"open({str(marker)!r}, 'w').close()",
    )

    assert result.returncode == 1
    assert "deterministic bundle artifact must be local JSON evidence" in result.stderr
    assert not marker.exists()


def test_evidence_bundle_publication_keeps_old_receipt_valid_on_run_write_failure(
    tmp_path, monkeypatch,
):
    run_dir = initialise(tmp_path)
    original_bundle = b'{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_bytes(original_bundle)
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    module = load_producer()
    original_receipt = json.loads((run_dir / "RUN.json").read_text())

    def fail_run_write(_path, _value):
        raise module.ReceiptError("injected RUN write failure")

    monkeypatch.setattr(module, "write_json_atomic", fail_run_write)
    args = module.build_parser().parse_args([
        "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "intent.md", "--", sys.executable, "-c", "pass",
    ])
    with pytest.raises(module.ReceiptError, match="injected RUN write failure"):
        module.command_evidence_run(args)

    assert json.loads((run_dir / "RUN.json").read_text()) == original_receipt
    assert bundle_path.read_bytes() == original_bundle


def test_bounded_execution_times_out_and_truncates_output():
    module = load_producer()

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            timeout_seconds=0.05,
        )
    noisy_command = (
        f"import sys; sys.stdout.write('x'*{module.MAX_LOG_BYTES * 2}); "
        f"sys.stderr.write('y'*{module.MAX_LOG_BYTES * 2})"
    )
    exit_code, stdout, stderr = module.execute_bounded([
        sys.executable,
        "-c",
        noisy_command,
    ])
    assert exit_code == 0
    assert len(stdout.encode()) <= module.MAX_LOG_BYTES
    assert len(stderr.encode()) <= module.MAX_LOG_BYTES


def test_human_evidence_links_existing_nonempty_artifact(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "approval.json").write_text('{"approved":true}\n')
    assert add_artifact(
        tmp_path, run_dir, "approval", "approval.json"
    ).returncode == 0

    result = run_cli(
        tmp_path,
        "evidence",
        "human",
        "--run-dir",
        str(run_dir),
        "--id",
        "authority-approval",
        "--gate",
        "authority-approval",
        "--artifact-id",
        "approval",
        "--approver",
        "human-owner",
        "--source",
        "approval.json",
    )

    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    evidence = next(item for item in receipt["evidence"] if item["id"] == "authority-approval")
    assert evidence["kind"] == "human"
    assert evidence["approver"] == "human-owner"
    assert evidence["artifact_id"] == "approval"


def test_evidence_remove_refuses_referenced_id_and_rebuild_updates_digest(tmp_path):
    run_dir = initialise(tmp_path)
    bundle_path = tmp_path / ".agent-run" / "DEL-TEST" / "evidence.json"
    bundle_path.write_text('{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n')
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    assert run_cli(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "intent.md", "--", sys.executable, "-c", "pass",
    ).returncode == 0
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["state_history"][0]["evidence_ids"] = ["tests"]
    receipt_path.write_text(json.dumps(receipt))

    refused = run_cli(
        tmp_path, "evidence", "remove", "--run-dir", str(run_dir), "--id", "tests",
    )
    receipt["state_history"][0]["evidence_ids"] = []
    receipt_path.write_text(json.dumps(receipt))
    removed = run_cli(
        tmp_path, "evidence", "remove", "--run-dir", str(run_dir), "--id", "tests",
    )
    rebuilt = run_cli(
        tmp_path, "evidence", "rebuild", "--run-dir", str(run_dir),
        "--artifact-id", "evidence-bundle",
    )

    assert refused.returncode == 1
    assert "referenced by state_history" in refused.stderr
    assert removed.returncode == 0, removed.stderr
    assert rebuilt.returncode == 0, rebuilt.stderr
    receipt = json.loads(receipt_path.read_text())
    assert receipt["evidence"] == []
    artifact = next(item for item in receipt["artifacts"] if item["id"] == "evidence-bundle")
    assert artifact["digest"] == digest_bytes_for_test(
        (tmp_path / artifact["path"]).read_bytes()
    )


def test_evidence_remove_refuses_authority_and_review_references(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "approval.json").write_text('{"approved":true}\n')
    assert add_artifact(tmp_path, run_dir, "approval", "approval.json").returncode == 0
    assert run_cli(
        tmp_path, "evidence", "human", "--run-dir", str(run_dir),
        "--id", "authority-approval", "--gate", "authority-approval",
        "--artifact-id", "approval", "--approver", "human-owner",
    ).returncode == 0
    authority_refused = run_cli(
        tmp_path, "evidence", "remove", "--run-dir", str(run_dir),
        "--id", "authority-approval",
    )
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    (tmp_path / "route.json").write_text(json.dumps(route_receipt()))
    assert run_cli(
        tmp_path, "review", "add", "--run-dir", str(run_dir), "--id", "review-1",
        "--role", "targeted", "--artifact", "review.md",
        "--route-receipt", "route.json", "--reviewer-id", "reviewer-1",
        "--adapter", "native-subagent", "--provider-family", "openai",
        "--model", "runtime-model", "--lens", "correctness-spec",
    ).returncode == 0
    review_refused = run_cli(
        tmp_path, "evidence", "remove", "--run-dir", str(run_dir), "--id", "review-1",
    )

    assert authority_refused.returncode == 1
    assert "referenced by authority" in authority_refused.stderr
    assert review_refused.returncode == 1
    assert "referenced by reviews[0]" in review_refused.stderr


def digest_bytes_for_test(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def test_review_add_requires_live_artifacts_and_records_matching_lineage(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    (tmp_path / "route.json").write_text(json.dumps(route_receipt()))

    result = run_cli(
        tmp_path,
        "review",
        "add",
        "--run-dir",
        str(run_dir),
        "--id",
        "review-1",
        "--role",
        "targeted",
        "--artifact",
        "review.md",
        "--route-receipt",
        "route.json",
        "--reviewer-id",
        "reviewer-1",
        "--adapter",
        "native-subagent",
        "--provider-family",
        "openai",
        "--model",
        "runtime-model",
        "--lens",
        "correctness-spec",
        "--lens",
        "tests",
    )

    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    review = receipt["reviews"][0]
    evidence = next(item for item in receipt["evidence"] if item["id"] == "review-1")
    assert review["evidence_id"] == "review-1"
    assert review["lenses"] == ["correctness-spec", "tests"]
    assert evidence["model_lineage"] == {
        "adapter": "native-subagent",
        "provider_family": "openai",
        "model": "runtime-model",
    }
    assert evidence["route_receipt"]["digest"] == digest_bytes_for_test(
        (tmp_path / "route.json").read_bytes()
    )


def test_review_add_refuses_route_receipt_lineage_mismatch(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    (tmp_path / "route.json").write_text(json.dumps(route_receipt(model="different-model")))

    result = run_cli(
        tmp_path, "review", "add", "--run-dir", str(run_dir), "--id", "review-1",
        "--role", "targeted", "--artifact", "review.md",
        "--route-receipt", "route.json", "--reviewer-id", "reviewer-1",
        "--adapter", "native-subagent", "--provider-family", "openai",
        "--model", "runtime-model", "--lens", "correctness-spec",
    )

    assert result.returncode == 1
    assert "route receipt identity does not match review lineage" in result.stderr


def test_checkpoint_transition_show_and_refusals(tmp_path):
    run_dir = initialise(tmp_path)
    checkpoint = run_cli(
        tmp_path,
        "checkpoint",
        "set",
        "--run-dir",
        str(run_dir),
        "--current-slice",
        "scope",
        "--next-action",
        "approve",
        "--in-flight",
        "scope-1",
        "--artifact",
        "intent.md",
    )
    scoped = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "scoped",
    )
    jump = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "executing",
    )
    timestamp = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "approved",
        "--timestamp", "2099-01-01T00:00:00Z",
    )
    shown = run_cli(tmp_path, "show", "--run-dir", str(run_dir))

    assert checkpoint.returncode == 0, checkpoint.stderr
    assert scoped.returncode == 0, scoped.stderr
    assert jump.returncode == 1
    assert "invalid lifecycle transition scoped -> executing" in jump.stderr
    assert timestamp.returncode == 2
    assert "unrecognized arguments: --timestamp" in timestamp.stderr
    assert shown.returncode == 0, shown.stderr
    receipt = json.loads(shown.stdout)
    assert receipt["status"] == "scoped"
    assert receipt["checkpoint"]["generation"] == 2
    assert receipt["checkpoint"]["artifact_paths"] == ["RUN.json", "intent.md"]


def test_transition_refuses_clock_regression_and_mutations_refuse_tier_change(tmp_path):
    run_dir = initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["state_history"][-1]["at"] = "2999-01-01T00:00:00Z"
    receipt_path.write_text(json.dumps(receipt))

    regression = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "scoped",
    )
    receipt["state_history"][-1]["at"] = "2026-01-01T00:00:00Z"
    receipt["risk_tier"] = "substantial"
    receipt_path.write_text(json.dumps(receipt))
    changed_tier = run_cli(
        tmp_path,
        "checkpoint",
        "set",
        "--run-dir",
        str(run_dir),
        "--current-slice",
        "draft",
        "--next-action",
        "stop",
    )

    assert regression.returncode == 1
    assert "transition timestamp must strictly increase" in regression.stderr
    assert changed_tier.returncode == 1
    assert "risk tier is immutable after init" in changed_tier.stderr


def test_transition_refuses_equal_process_timestamp(tmp_path, monkeypatch):
    run_dir = initialise(tmp_path)
    receipt = json.loads((run_dir / "RUN.json").read_text())
    module = load_producer()
    monkeypatch.setattr(module, "utc_now", lambda: receipt["state_history"][-1]["at"])
    args = module.build_parser().parse_args([
        "transition", "--run-dir", str(run_dir), "--to", "scoped",
    ])

    with pytest.raises(module.ReceiptError, match="must strictly increase"):
        module.command_transition(args)


def test_init_risk_tier_below_derived_requires_approved_override(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")
    assessment = routine_assessment()
    assessment["critical_surface"] = "build-release-gate"
    args = init_args()
    args[args.index("--risk-assessment") + 1] = json.dumps(assessment)
    args.extend(["--risk-tier", "routine"])

    refused = run_cli(tmp_path, *args)

    assert refused.returncode == 1
    assert "below derived crucial requires an approved human override" in refused.stderr


def test_init_accepts_structurally_approved_human_risk_override(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")
    (tmp_path / "risk-approval.json").write_text('{"approved":true}\n')
    assessment = routine_assessment()
    assessment["critical_surface"] = "build-release-gate"
    args = init_args()
    args[args.index("--risk-assessment") + 1] = json.dumps(assessment)
    args.extend([
        "--risk-tier",
        "routine",
        "--risk-override",
        json.dumps({
            "status": "approved",
            "approved_by": "human-owner",
            "evidence": "risk-override-approval",
            "reason": "owner accepted the bounded downgrade",
            "artifact": "risk-approval.json",
            "artifact_id": "risk-override-artifact",
        }),
    ])

    accepted = run_cli(tmp_path, *args)

    assert accepted.returncode == 0, accepted.stderr
    receipt = json.loads((tmp_path / ".agent-run" / "DEL-TEST" / "RUN.json").read_text())
    assert receipt["risk_tier"] == "routine"
    assert receipt["risk_override"]["status"] == "approved"
    evidence = next(
        item for item in receipt["evidence"]
        if item["id"] == "risk-override-approval"
    )
    assert evidence["gate"] == "risk-override"


def test_end_to_end_producer_receipt_passes_independent_validator(tmp_path):
    approval_raw = b'{"approved":true}\n'
    auth = authority("DEL-E2E")
    auth["evidence_digest"] = digest_bytes_for_test(approval_raw)
    args = init_args("DEL-E2E")
    args[args.index("--authority") + 1] = json.dumps(auth)
    (tmp_path / "intent.md").write_text("# Intent\n")
    init = run_cli(tmp_path, *args)
    assert init.returncode == 0, init.stderr
    run_dir = tmp_path / ".agent-run" / "DEL-E2E"
    (tmp_path / "approval.json").write_bytes(approval_raw)
    assert add_artifact(tmp_path, run_dir, "approval", "approval.json").returncode == 0
    for evidence_id, gate in (
        ("authority-approval", "authority-approval"),
        ("intent-approval", "intent-approval"),
    ):
        result = run_cli(
            tmp_path, "evidence", "human", "--run-dir", str(run_dir),
            "--id", evidence_id, "--gate", gate, "--artifact-id", "approval",
            "--approver", "human-owner", "--source", "approval.json",
        )
        assert result.returncode == 0, result.stderr
    design_path = tmp_path / "design.json"
    design_path.write_text(json.dumps({
        "status": "draft",
        "artifact_id": "intent",
        "digest": digest_bytes_for_test((tmp_path / "intent.md").read_bytes()),
        "approver": "",
        "evidence": "",
        "alternatives": ["retain-current-loop"],
        "failure_analysis": "bounded",
        "containment": "revert",
        "one_way_doors": [],
    }))
    assert run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir), "--section", "design",
        "--from", str(design_path),
    ).returncode == 0
    bundle = run_dir / "evidence.json"
    bundle.write_text('{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n')
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-E2E/evidence.json"
    ).returncode == 0
    executed = run_cli(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "intent.md", "--", sys.executable, "-c", "print('pass')",
    )
    assert executed.returncode == 0, executed.stderr
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    (tmp_path / "route.json").write_text(json.dumps(route_receipt()))
    reviewed = run_cli(
        tmp_path, "review", "add", "--run-dir", str(run_dir), "--id", "review-1",
        "--role", "targeted", "--artifact", "review.md",
        "--route-receipt", "route.json", "--reviewer-id", "reviewer-1",
        "--adapter", "native-subagent", "--provider-family", "openai",
        "--model", "runtime-model", "--lens", "correctness-spec",
    )
    assert reviewed.returncode == 0, reviewed.stderr
    checkpoint = run_cli(
        tmp_path, "checkpoint", "set", "--run-dir", str(run_dir),
        "--current-slice", "executing", "--next-action", "verify",
        "--artifact", "intent.md",
    )
    assert checkpoint.returncode == 0, checkpoint.stderr
    for state in ("scoped", "approved", "executing", "verifying"):
        transitioned = run_cli(
            tmp_path, "transition", "--run-dir", str(run_dir), "--to", state,
        )
        assert transitioned.returncode == 0, transitioned.stderr
    transitioned = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "reviewing",
        "--evidence", "tests",
    )
    assert transitioned.returncode == 0, transitioned.stderr
    shown = run_cli(tmp_path, "show", "--run-dir", str(run_dir))
    assert shown.returncode == 0
    assert json.loads(shown.stdout)["status"] == "reviewing"

    validated = subprocess.run(
        [
            sys.executable,
            str(ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"),
            str(run_dir / "RUN.json"),
            "--workspace-root",
            str(tmp_path),
            "--product-root",
            str(ROOT),
            "--verify-hashes",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert validated.returncode == 0, validated.stderr + validated.stdout


def test_transition_refuses_adjacent_acceptance_gate_without_required_evidence(tmp_path):
    test_end_to_end_producer_receipt_passes_independent_validator(tmp_path)
    run_dir = tmp_path / ".agent-run" / "DEL-E2E"

    refused = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir),
        "--to", "awaiting_acceptance", "--evidence", "tests",
        "--evidence", "review-1",
    )

    assert refused.returncode == 1
    assert "awaiting_acceptance gate is not satisfied" in refused.stderr


@pytest.mark.parametrize(
    ("mutation", "expected_error"),
    [
        ("timestamp-order", "state history timestamps must increase"),
        ("unknown-evidence", "references unknown evidence ids"),
        ("state-jump", "invalid lifecycle transition"),
        ("artifact-digest", "digest does not match live bytes"),
    ],
)
def test_hand_mutated_producer_output_fails_independent_validator(
    tmp_path, mutation, expected_error,
):
    test_end_to_end_producer_receipt_passes_independent_validator(tmp_path)
    run_path = tmp_path / ".agent-run" / "DEL-E2E" / "RUN.json"
    receipt = json.loads(run_path.read_text())
    if mutation == "timestamp-order":
        receipt["state_history"][1]["at"] = receipt["state_history"][0]["at"]
    elif mutation == "unknown-evidence":
        receipt["state_history"][-1]["evidence_ids"].append("missing-evidence")
    elif mutation == "state-jump":
        receipt["state_history"][1]["state"] = "executing"
    else:
        receipt["artifacts"][0]["digest"] = "sha256:" + "0" * 64
    run_path.write_text(json.dumps(receipt))

    validated = subprocess.run(
        [
            sys.executable,
            str(ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"),
            str(run_path),
            "--workspace-root",
            str(tmp_path),
            "--product-root",
            str(ROOT),
            "--verify-hashes",
        ],
        text=True,
        capture_output=True,
        check=False,
    )

    assert validated.returncode == 1
    assert expected_error in validated.stderr + validated.stdout
