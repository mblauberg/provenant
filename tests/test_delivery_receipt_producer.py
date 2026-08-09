import hashlib
import importlib.util
import json
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"
VALIDATOR = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"


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


def load_validator():
    spec = importlib.util.spec_from_file_location("validate_delivery_for_producer", VALIDATOR)
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
        "denied_paths": [],
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
            cwd=ROOT,
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
    ], cwd=ROOT)
    assert exit_code == 0
    assert len(stdout.encode()) <= module.MAX_LOG_BYTES
    assert len(stderr.encode()) <= module.MAX_LOG_BYTES


def test_bounded_execution_enforces_deadline_after_output_closes():
    module = load_producer()
    command = [
        sys.executable,
        "-c",
        "import os,time; os.close(1); os.close(2); time.sleep(0.2)",
    ]
    started = time.monotonic()

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(command, cwd=ROOT, timeout_seconds=0.05)

    assert time.monotonic() - started < 0.25


def test_bounded_execution_kills_descendants_on_timeout(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    marker = tmp_path / "descendant-survived"
    child = (
        "import pathlib,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        f"pathlib.Path({str(ready)!r}).write_text('ready'); time.sleep(0.6); "
        f"pathlib.Path({str(marker)!r}).write_text('alive')"
    )
    parent = (
        "import pathlib,subprocess,sys,time; "
        f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
        f"ready=pathlib.Path({str(ready)!r}); "
        "deadline=time.monotonic()+0.5; "
        "\nwhile not ready.exists() and time.monotonic()<deadline: time.sleep(0.005)"
        "\ntime.sleep(60)"
    )

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [sys.executable, "-c", parent], cwd=tmp_path, timeout_seconds=0.25,
        )
    time.sleep(0.7)

    assert ready.exists()
    assert not marker.exists()


def test_evidence_run_executes_from_receipt_workspace(tmp_path):
    run_dir = initialise(tmp_path)
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    outside = tmp_path.parent / f"{tmp_path.name}-outside"
    outside.mkdir()

    result = subprocess.run(
        [
            sys.executable, str(PRODUCER), "evidence", "run",
            "--run-dir", str(run_dir), "--id", "tests", "--gate", "tests",
            "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
            sys.executable, "-c",
            "from pathlib import Path; Path('workspace-marker').write_text('ok')",
        ],
        cwd=outside,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert (tmp_path / "workspace-marker").read_text() == "ok"
    assert not (outside / "workspace-marker").exists()


def test_human_evidence_links_existing_nonempty_artifact(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "approval.json").write_text(
        '{"approved":true,"approver":"human-owner"}\n'
    )
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
    assert evidence["artifact_digest"] == next(
        item["digest"] for item in receipt["artifacts"] if item["id"] == "approval"
    )


def test_authority_approval_refuses_uncorroborated_approver(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "approval.json").write_text(
        '{"approved":false,"approver":"forged-person"}\n'
    )
    added = add_artifact(tmp_path, run_dir, "approval", "approval.json")
    assert added.returncode == 0, added.stderr

    refused = run_cli(
        tmp_path, "evidence", "human", "--run-dir", str(run_dir),
        "--id", "forged-authority-approval", "--gate", "authority-approval",
        "--artifact-id", "approval", "--approver", "forged-person",
        "--source", "approval.json",
    )

    assert refused.returncode == 1
    assert (
        "authority approval artifact does not corroborate approver forged-person"
        in refused.stderr
    )
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["authority"]["approved_by"] == "human-owner"
    assert all(
        item.get("id") != "forged-authority-approval"
        for item in receipt["evidence"]
    )


def test_authority_approval_refuses_blank_approver(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "approval.json").write_text(
        '{"approved":true,"approver":""}\n'
    )
    added = add_artifact(tmp_path, run_dir, "approval", "approval.json")
    assert added.returncode == 0, added.stderr

    refused = run_cli(
        tmp_path, "evidence", "human", "--run-dir", str(run_dir),
        "--id", "blank-authority-approval", "--gate", "authority-approval",
        "--artifact-id", "approval", "--approver", "",
        "--source", "approval.json",
    )

    assert refused.returncode == 1
    assert "authority approval requires a non-empty approver" in refused.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["authority"]["approved_by"] == "human-owner"
    assert all(
        item.get("id") != "blank-authority-approval"
        for item in receipt["evidence"]
    )


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
    (tmp_path / "approval.json").write_text(
        '{"approved":true,"approver":"human-owner"}\n'
    )
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


def test_review_add_records_reasoned_skip_without_judgement_evidence(tmp_path):
    run_dir = initialise(tmp_path)

    result = run_cli(
        tmp_path, "review", "add", "--run-dir", str(run_dir),
        "--id", "distinct-family-skip", "--role", "distinct-family",
        "--adapter", "gemini", "--provider-family", "google",
        "--lens", "blind-spots", "--status", "skipped",
        "--reason", "provider quota unavailable",
    )

    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["reviews"][-1]["status"] == "skipped"
    assert receipt["reviews"][-1]["reason"] == "provider quota unavailable"
    assert not any(
        item["id"] == "distinct-family-skip" for item in receipt["evidence"]
    )
    load_validator()._validate_reviews(receipt, {}, required=False)


@pytest.mark.parametrize(
    ("adapter", "family", "expected"),
    [
        ("", "google", "adapter"),
        ("gemini", "", "provider-family"),
        ("native-subagent", "openai", "non-primary"),
    ],
)
def test_review_add_refuses_invalid_distinct_family_skip_lineage(
    tmp_path, adapter, family, expected,
):
    run_dir = initialise(tmp_path)

    result = run_cli(
        tmp_path, "review", "add", "--run-dir", str(run_dir),
        "--id", "distinct-family-skip", "--role", "distinct-family",
        "--adapter", adapter, "--provider-family", family,
        "--lens", "blind-spots", "--status", "skipped",
        "--reason", "provider quota unavailable",
    )

    assert result.returncode == 1
    assert expected in result.stderr


def test_review_ladder_accepts_reasoned_distinct_family_skip():
    module = load_producer()
    run = {
        "risk_tier": "crucial",
        "chair_family": "openai",
        "reviews": [
            {
                "role": "targeted", "provider_family": "openai",
                "status": "pass", "lenses": ["correctness", "tests"], "reason": "",
            },
            {
                "role": "other-primary", "provider_family": "anthropic",
                "status": "pass", "lenses": ["architecture"], "reason": "",
            },
            {
                "role": "distinct-family", "provider_family": "google",
                "status": "skipped", "lenses": ["blind-spots"],
                "reason": "provider quota unavailable",
            },
        ],
    }

    assert module.lifecycle.review_ladder_error(run) is None


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
    approval_raw = (
        b'{"approved":true,"approver":"human-owner","gate":"task-success",'
        b'"measured_value":1}\n'
    )
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


def test_reviewing_refuses_evidence_predating_repair(tmp_path):
    test_end_to_end_producer_receipt_passes_independent_validator(tmp_path)
    run_dir = tmp_path / ".agent-run" / "DEL-E2E"
    for state in ("repairing", "verifying"):
        transitioned = run_cli(
            tmp_path, "transition", "--run-dir", str(run_dir), "--to", state,
        )
        assert transitioned.returncode == 0, transitioned.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    repair_at = receipt["state_history"][-2]["at"]

    refused = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "reviewing",
        "--evidence", "tests",
    )

    assert refused.returncode == 1
    assert (
        f"deterministic gate tests evidence predates repairing transition at {repair_at}"
        in refused.stderr
    )


def test_reviewing_accepts_evidence_after_repair(tmp_path):
    test_end_to_end_producer_receipt_passes_independent_validator(tmp_path)
    run_dir = tmp_path / ".agent-run" / "DEL-E2E"
    for state in ("repairing", "verifying"):
        transitioned = run_cli(
            tmp_path, "transition", "--run-dir", str(run_dir), "--to", state,
        )
        assert transitioned.returncode == 0, transitioned.stderr
    bundle = run_dir / "evidence-after-repair.json"
    bundle.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    added = add_artifact(
        tmp_path, run_dir, "evidence-after-repair",
        ".agent-run/DEL-E2E/evidence-after-repair.json",
    )
    assert added.returncode == 0, added.stderr
    executed = run_cli(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir),
        "--id", "tests-after-repair", "--gate", "tests",
        "--artifact-id", "evidence-after-repair", "--source", "intent.md",
        "--", sys.executable, "-c", "print('pass')",
    )
    assert executed.returncode == 0, executed.stderr

    transitioned = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "reviewing",
        "--evidence", "tests-after-repair",
    )

    assert transitioned.returncode == 0, transitioned.stderr


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


def test_transition_uses_conditional_evidence_and_risk_review_ladder(tmp_path):
    run_dir = initialise(tmp_path)
    module = load_producer()
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["profile"] = "document"
    receipt["artifacts"].append({
        "id": "page",
        "path": "intent.md",
        "media_type": "text/html",
        "artifact_type": "html",
        "digest": digest_bytes_for_test((tmp_path / "intent.md").read_bytes()),
        "class": "canonical",
        "owner": "delivery-chair",
        "retention": "project-policy",
    })
    receipt["evidence"].append({
        "id": "render",
        "kind": "deterministic",
        "gate": "render",
        "status": "pass",
    })

    error = module.lifecycle.transition_gate_error(
        receipt, "reviewing", {"render"}, vars(module),
    )

    assert error == "reviewing deterministic gate is not satisfied"
    receipt["risk_tier"] = "substantial"
    receipt["reviews"] = [{
        "role": "targeted",
        "provider_family": "openai",
        "status": "pass",
        "lenses": ["correctness"],
        "reason": "",
    }]
    assert "targeted lenses" in module.lifecycle.review_ladder_error(receipt)


def test_transition_refuses_wrong_kind_gate_and_invalid_measure_rows(tmp_path):
    run_dir = initialise(tmp_path)
    module = load_producer()
    receipt = json.loads((run_dir / "RUN.json").read_text())
    receipt["evidence"] = [
        {"id": "tests", "kind": "deterministic", "gate": "tests", "status": "pass"},
        {"id": "wrong-tests", "kind": "human", "gate": "tests", "status": "pass"},
        {
            "id": "review-1",
            "kind": "judgement",
            "gate": "code-review",
            "status": "pass",
        },
    ]
    assert module.lifecycle.transition_gate_error(
        receipt, "reviewing", {"tests", "wrong-tests"}, vars(module),
    ) == "reviewing deterministic gate is not satisfied"
    receipt["reviews"] = [{
        "role": "targeted",
        "provider_family": "openai",
        "status": "pass",
        "evidence_id": "review-1",
        "lenses": ["correctness"],
    }]
    receipt["measures"] = {
        "outcome": [{
            "id": "functional-correctness",
            "status": "pass",
            "evidence_id": "tests",
            "evidence_kind": "deterministic",
            "value": 1,
            "target": "pass",
            "aggregation": "single",
        }, {
            "id": "extra",
            "status": "fail",
            "evidence_id": "tests",
            "evidence_kind": "deterministic",
            "value": 0,
            "target": "pass",
            "aggregation": "single",
        }],
        "trajectory": [{
            "id": "verification-completion",
            "status": "pass",
            "evidence_id": "tests",
            "evidence_kind": "deterministic",
            "value": 1,
            "target": "pass",
            "aggregation": "single",
        }],
    }
    profile = module.lifecycle.profile_requirements(
        receipt, module.PROFILE_PATH, module.ReceiptError,
    )
    assert not module.lifecycle.measures_gate_ready(
        receipt, profile, {"tests", "review-1"},
    )


@pytest.mark.parametrize("value", ["nan", "inf", "-inf"])
def test_observation_evidence_refuses_non_finite_measurement(tmp_path, value):
    run_dir = initialise(tmp_path)

    result = run_cli(
        tmp_path, "evidence", "observation", "--run-dir", str(run_dir),
        "--id", "observation-1", "--gate", "task-success",
        "--artifact-id", "intent", f"--measured-value={value}",
        "--source", "intent.md",
    )

    assert result.returncode == 1
    assert "measured value must be finite" in result.stderr


def test_observation_evidence_refuses_uncorroborated_measurement(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "measurement.json").write_text(
        '{"thresholds":{"task-success":1},"observations":{"task-success":0}}\n'
    )
    added = add_artifact(
        tmp_path, run_dir, "measurement", "measurement.json",
    )
    assert added.returncode == 0, added.stderr

    refused = run_cli(
        tmp_path, "evidence", "observation", "--run-dir", str(run_dir),
        "--id", "observation-1", "--gate", "task-success",
        "--artifact-id", "measurement", "--measured-value", "1",
        "--source", "measurement.json",
    )

    assert refused.returncode == 1
    assert (
        "observation evidence artifact does not corroborate measured value 1"
        in refused.stderr
    )
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert all(item.get("id") != "observation-1" for item in receipt["evidence"])


def test_observation_evidence_refuses_precision_collapsed_measurement(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "measurement.json").write_text(
        '{"gate":"task-success","measured_value":9007199254740993}\n'
    )
    added = add_artifact(
        tmp_path, run_dir, "measurement", "measurement.json",
    )
    assert added.returncode == 0, added.stderr

    refused = run_cli(
        tmp_path, "evidence", "observation", "--run-dir", str(run_dir),
        "--id", "observation-1", "--gate", "task-success",
        "--artifact-id", "measurement",
        "--measured-value", "9007199254740992",
        "--source", "measurement.json",
    )

    assert refused.returncode == 1
    assert "does not corroborate measured value" in refused.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert all(item.get("id") != "observation-1" for item in receipt["evidence"])


def test_init_refuses_risk_override_identifier_collisions(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")
    (tmp_path / "risk-approval.json").write_text('{"approved":true}\n')
    assessment = routine_assessment()
    assessment["critical_surface"] = "build-release-gate"
    base = init_args()
    base[base.index("--risk-assessment") + 1] = json.dumps(assessment)

    def attempt(evidence_id: str, artifact_id: str):
        return run_cli(
            tmp_path, *base, "--risk-tier", "routine", "--risk-override",
            json.dumps({
                "status": "approved",
                "approved_by": "human-owner",
                "evidence": evidence_id,
                "reason": "bounded downgrade",
                "artifact": "risk-approval.json",
                "artifact_id": artifact_id,
            }),
        )

    evidence_collision = attempt("authority-approval", "risk-artifact")
    artifact_collision = attempt("risk-approval", "intent")

    assert evidence_collision.returncode == 1
    assert "reserved approval id" in evidence_collision.stderr
    assert artifact_collision.returncode == 1
    assert "reserved intent" in artifact_collision.stderr


def test_producer_can_reach_validator_compatible_closed_state(tmp_path):
    test_end_to_end_producer_receipt_passes_independent_validator(tmp_path)
    run_dir = tmp_path / ".agent-run" / "DEL-E2E"
    measures = {
        "outcome": [{
            "id": "functional-correctness",
            "status": "pass",
            "evidence_id": "tests",
            "evidence_kind": "deterministic",
            "value": 1,
            "target": "pass",
            "aggregation": "single-run",
        }],
        "trajectory": [{
            "id": "verification-completion",
            "status": "pass",
            "evidence_id": "tests",
            "evidence_kind": "deterministic",
            "value": 1,
            "target": "complete",
            "aggregation": "single-run",
        }],
    }
    (tmp_path / "measures.json").write_text(json.dumps(measures))
    bound = run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir), "--section", "measures",
        "--from", "measures.json",
    )
    assert bound.returncode == 0, bound.stderr
    awaiting = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir),
        "--to", "awaiting_acceptance", "--evidence", "tests",
        "--evidence", "review-1",
    )
    assert awaiting.returncode == 0, awaiting.stderr

    for evidence_id, gate in (
        ("acceptance-approval", "human-acceptance"),
        ("release-approval", "human-release"),
    ):
        added = run_cli(
            tmp_path, "evidence", "human", "--run-dir", str(run_dir),
            "--id", evidence_id, "--gate", gate, "--artifact-id", "approval",
            "--approver", "human-owner", "--source", "approval.json",
        )
        assert added.returncode == 0, added.stderr
    empty_observation = {
        "status": "active",
        "window": {"kind": "event-count", "minimum": 1},
        "signals": ["task-success"],
        "thresholds": {"task-success": {"direction": "gte", "limit": 1}},
        "owner": "human-owner",
        "containment": "revert",
        "privacy": "aggregate",
        "close_condition": "task succeeds",
        "started_at": "",
        "ended_at": "",
        "observed_events": 0,
        "evidence_ids": [],
    }
    (tmp_path / "observation.json").write_text(json.dumps(empty_observation))
    assert run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir),
        "--section", "observation-plan", "--from", "observation.json",
    ).returncode == 0
    for state, evidence_id in (
        ("accepted", "acceptance-approval"),
        ("awaiting_release", None),
        ("observing", "release-approval"),
    ):
        args = ["transition", "--run-dir", str(run_dir), "--to", state]
        if evidence_id:
            args.extend(["--evidence", evidence_id])
        transitioned = run_cli(tmp_path, *args)
        assert transitioned.returncode == 0, transitioned.stderr

    receipt = json.loads((run_dir / "RUN.json").read_text())
    observing_at = receipt["state_history"][-1]["at"]
    empty_refused = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "closed",
    )
    assert empty_refused.returncode == 1
    assert "closed observation gate is not satisfied" in empty_refused.stderr

    measured = run_cli(
        tmp_path, "evidence", "observation", "--run-dir", str(run_dir),
        "--id", "observed-task-success", "--gate", "task-success",
        "--artifact-id", "approval", "--measured-value", "1",
        "--source", "approval.json",
    )
    assert measured.returncode == 0, measured.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    observed_at = next(
        item["observed_at"] for item in receipt["evidence"]
        if item["id"] == "observed-task-success"
    )
    empty_observation.update({
        "status": "pass",
        "started_at": observing_at,
        "ended_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "observed_events": 1,
        "evidence_ids": ["observed-task-success"],
    })
    assert empty_observation["started_at"] <= observed_at <= empty_observation["ended_at"]
    (tmp_path / "observation.json").write_text(json.dumps(empty_observation))
    assert run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir),
        "--section", "observation-plan", "--from", "observation.json",
    ).returncode == 0
    receipt = json.loads((run_dir / "RUN.json").read_text())
    empty_observation["started_at"] = receipt["state_history"][0]["at"]
    (tmp_path / "observation.json").write_text(json.dumps(empty_observation))
    assert run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir),
        "--section", "observation-plan", "--from", "observation.json",
    ).returncode == 0
    early_window = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "closed",
        "--evidence", "observed-task-success",
    )
    assert early_window.returncode == 1
    assert "closed observation gate is not satisfied" in early_window.stderr
    empty_observation["started_at"] = observing_at
    empty_observation["thresholds"]["task-success"]["limit"] = float("-inf")
    (tmp_path / "observation.json").write_text(json.dumps(empty_observation))
    changed_plan = run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir),
        "--section", "observation-plan", "--from", "observation.json",
    )
    assert changed_plan.returncode == 1
    assert "observation lifecycle gate has passed" in changed_plan.stderr
    empty_observation["thresholds"]["task-success"]["limit"] = 1
    (tmp_path / "observation.json").write_text(json.dumps(empty_observation))
    assert run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir),
        "--section", "observation-plan", "--from", "observation.json",
    ).returncode == 0
    closed = run_cli(
        tmp_path, "transition", "--run-dir", str(run_dir), "--to", "closed",
        "--evidence", "observed-task-success",
    )
    assert closed.returncode == 0, closed.stderr

    validated = subprocess.run(
        [
            sys.executable,
            str(ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"),
            str(run_dir / "RUN.json"), "--workspace-root", str(tmp_path),
            "--product-root", str(ROOT), "--verify-hashes",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert validated.returncode == 0, validated.stderr + validated.stdout


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
