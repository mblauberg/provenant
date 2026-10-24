"""Producer tests for the flat RUN.json receipt writer.

The suite is deliberately narrow: one happy path that walks a run from init to
a closed, validator-clean receipt, named regressions for the three failures
that have actually happened, and the in-process cases that exercise the
producer's own code paths. Everything else the producer refuses is covered by
the independent validator suite; see #758.
"""

import hashlib
import json
import os
import signal
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"
VALIDATOR = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"
CHECKPOINT = ROOT / "skills" / "implement" / "scripts" / "checkpoint_run.py"


def load(path: Path, name: str):
    return load_script(path, name)


def load_producer():
    return load(PRODUCER, "delivery_receipt_under_test")


def test_init_accepts_canonical_runs_directory_and_writes_local_exclude(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    (workspace / "intent.md").write_text("# Intent\n")
    args = init_args("DEL-TEST")
    name = "20260923-1000-delivery-del-test-a1b2c3"
    args[2] = f".agent-run/runs/{name}"
    result = run_cli(workspace, *args)
    assert result.returncode == 0, result.stderr
    assert (workspace / ".agent-run" / "runs" / name / "RUN.json").is_file()
    assert "/.agent-run/" in (workspace / ".git" / "info" / "exclude").read_text()
    checkpoint = subprocess.run([
        sys.executable, str(CHECKPOINT), str(workspace / ".agent-run" / "runs" / name / "RUN.json"),
        "--current-slice", "implementation", "--next-action", "verify",
        "--artifact-paths-json", '["intent.md"]',
    ], cwd=workspace, text=True, capture_output=True, check=False)
    assert checkpoint.returncode == 0, checkpoint.stdout + checkpoint.stderr


def test_repair_records_failed_gates_in_cycle_sidecar(tmp_path):
    run_dir = initialise(tmp_path)
    first = run_cli(tmp_path, "repair", "--run-dir", str(run_dir), "--reason", "repair one",
                    "--failing", "a", "--failing", "b")
    second = run_cli(tmp_path, "repair", "--run-dir", str(run_dir), "--reason", "repair two",
                     "--failing", "b")
    assert first.returncode == second.returncode == 0
    rows = [json.loads(line) for line in (run_dir / "repairs.jsonl").read_text().splitlines()]
    assert [row["cycle"] for row in rows] == [1, 2]
    assert [row["failing_before"] for row in rows] == [["a", "b"], ["b"]]


def test_finding_rejects_unknown_disposition(tmp_path):
    run_dir = initialise(tmp_path)
    result = run_cli(tmp_path, "finding", "--run-dir", str(run_dir), "--review", "review-1",
                     "--finding", "finding-1", "--disposition", "unclear")
    assert result.returncode != 0
    assert "invalid choice" in result.stderr


def test_finding_requires_duplicate_target_for_duplicate_disposition(tmp_path):
    run_dir = initialise(tmp_path)
    result = run_cli(tmp_path, "finding", "--run-dir", str(run_dir), "--review", "review-1",
                     "--finding", "finding-1", "--disposition", "duplicate")
    assert result.returncode != 0
    assert "duplicate disposition requires --duplicate-of" in result.stderr
    assert not (run_dir / "findings.jsonl").exists()


def test_finding_rejects_duplicate_target_for_other_disposition(tmp_path):
    run_dir = initialise(tmp_path)
    result = run_cli(tmp_path, "finding", "--run-dir", str(run_dir), "--review", "review-1",
                     "--finding", "finding-1", "--disposition", "confirmed",
                     "--duplicate-of", "finding-0")
    assert result.returncode != 0
    assert "--duplicate-of requires duplicate disposition" in result.stderr
    assert not (run_dir / "findings.jsonl").exists()


@pytest.mark.parametrize(("option", "value"), [
    ("--reviewer-family", "bad family"), ("--model", "bad model"), ("--reviewer-id", "bad reviewer"),
])
def test_finding_validates_optional_reviewer_identifiers(tmp_path, option, value):
    run_dir = initialise(tmp_path)
    result = run_cli(tmp_path, "finding", "--run-dir", str(run_dir), "--review", "review-1",
                     "--finding", "finding-1", "--disposition", "confirmed", option, value)
    assert result.returncode != 0
    assert "bounded stable identifier" in result.stderr


def test_finding_records_dispositions_with_review_lineage(tmp_path):
    run_dir = initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["reviews"].append({
        "id": "review-1", "provider_family": "anthropic", "model": "claude-test",
        "reviewer_id": "reviewer", "status": "pass",
    })
    receipt_path.write_text(json.dumps(receipt))
    for index, disposition in enumerate(("confirmed", "false_positive", "reviewer_bias", "duplicate")):
        args = ["finding", "--run-dir", str(run_dir), "--review", "review-1",
                "--finding", f"finding-{index}", "--disposition", disposition]
        if disposition == "duplicate":
            args += ["--duplicate-of", "finding-0"]
        result = run_cli(tmp_path, *args)
        assert result.returncode == 0, result.stderr
    rows = [json.loads(line) for line in (run_dir / "findings.jsonl").read_text().splitlines()]
    assert [row["disposition"] for row in rows] == ["confirmed", "false_positive", "reviewer_bias", "duplicate"]
    assert rows[-1]["duplicate_of"] == "finding-0"


def test_init_rejects_canonical_run_in_linked_worktree(tmp_path):
    primary = tmp_path / "primary"
    primary.mkdir()
    subprocess.run(["git", "init", "-q", str(primary)], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(primary), "config", "user.name", "Test"], check=True)
    (primary / "intent.md").write_text("# Intent\n")
    subprocess.run(["git", "-C", str(primary), "add", "intent.md"], check=True)
    subprocess.run(["git", "-C", str(primary), "commit", "-qm", "init"], check=True)
    linked = tmp_path / "linked"
    subprocess.run(["git", "-C", str(primary), "worktree", "add", "-qb", "test/linked", str(linked)], check=True)
    args = init_args("DEL-TEST")
    args[2] = ".agent-run/runs/20260923-1000-delivery-del-test-a1b2c3"
    result = run_cli(linked, *args)
    assert result.returncode != 0
    assert "primary checkout" in result.stderr
    assert not (linked / ".agent-run").exists()


def load_validator():
    return load(VALIDATOR, "validate_delivery_for_producer")


def load_checkpoint():
    return load(CHECKPOINT, "checkpoint_run_for_producer")


def run_cli(workspace: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(PRODUCER), *args],
        cwd=workspace,
        text=True,
        capture_output=True,
        check=False,
    )


def digest_of(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


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
        "provider_family": family,
        "route_alias": "principal",
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
        "--run-dir", f".agent-run/{run_id}",
        "--run-id", run_id,
        "--profile", "software",
        "--chair-family", "openai",
        "--risk-assessment", json.dumps(routine_assessment()),
        "--intent", "intent.md",
        "--authority", json.dumps(authority(run_id)),
    ]


def initialise(workspace: Path, run_id: str = "DEL-TEST") -> Path:
    (workspace / "intent.md").write_text("# Intent\n")
    result = run_cli(workspace, *init_args(run_id))
    assert result.returncode == 0, result.stderr
    return workspace / ".agent-run" / run_id


def install_fixture_command(workspace: Path, body: str = "printf 'pass\\n'") -> None:
    script = workspace / "scripts" / "fixture-check"
    script.parent.mkdir(exist_ok=True)
    script.write_text(f"#!/bin/sh\n{body}\n")
    script.chmod(0o755)


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
        "artifact", "add",
        "--run-dir", str(run_dir),
        "--id", artifact_id,
        "--path", path,
        "--class", artifact_class,
        "--media-type", media_type,
        "--artifact-type", artifact_type,
        "--owner", "delivery-chair",
        "--retention", "risk-policy" if artifact_class == "evidence" else "project-policy",
    )


def write_bundle(run_dir: Path) -> Path:
    bundle = run_dir / "evidence.json"
    bundle.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    return bundle


def review_args(
    run_dir: Path, *, role: str = "targeted", family: str = "openai",
) -> list[str]:
    return [
        "review", "add",
        "--run-dir", str(run_dir),
        "--id", "review-1",
        "--role", role,
        "--artifact", "review.md",
        "--route-receipt", "route.json",
        "--reviewer-id", "reviewer-1",
        "--adapter", "native-subagent",
        "--provider-family", family,
        "--model", "runtime-model",
        "--lens", "correctness-spec",
    ]


def validate(run_dir: Path, workspace: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable, str(VALIDATOR), str(run_dir / "RUN.json"),
            "--workspace-root", str(workspace),
            "--product-root", str(ROOT),
            "--verify-hashes",
        ],
        text=True,
        capture_output=True,
        check=False,
    )


def deliver_run(tmp_path: Path) -> Path:
    """Drive a run from init to a reviewed, checkpointed receipt.

    This is the producer happy path, shared by the end-to-end test and by the
    regressions that need a realistic receipt to attack.
    """
    install_fixture_command(tmp_path)
    approval_raw = (
        b'{"approved":true,"approver":"human-owner","gate":"task-success",'
        b'"measured_value":1}\n'
    )
    auth = authority("DEL-E2E")
    auth["evidence_digest"] = digest_of(approval_raw)
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
        approved = run_cli(
            tmp_path, "evidence", "human", "--run-dir", str(run_dir),
            "--id", evidence_id, "--gate", gate, "--artifact-id", "approval",
            "--approver", "human-owner", "--source", "approval.json",
        )
        assert approved.returncode == 0, approved.stderr

    design_path = tmp_path / "design.json"
    design_path.write_text(json.dumps({
        "status": "draft",
        "artifact_id": "intent",
        "digest": digest_of((tmp_path / "intent.md").read_bytes()),
        "approver": "",
        "evidence": "",
        "alternatives": ["retain-current-loop"],
        "failure_analysis": "bounded",
        "containment": "revert",
        "one_way_doors": [],
    }))
    bound = run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir), "--section", "design",
        "--from", str(design_path),
    )
    assert bound.returncode == 0, bound.stderr

    write_bundle(run_dir)
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-E2E/evidence.json"
    ).returncode == 0
    executed = run_cli(
        tmp_path, "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "intent.md", "--", "scripts/fixture-check",
    )
    assert executed.returncode == 0, executed.stderr

    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    (tmp_path / "route.json").write_text(json.dumps(route_receipt()))
    reviewed = run_cli(tmp_path, *review_args(run_dir))
    assert reviewed.returncode == 0, reviewed.stderr

    checkpoint = run_cli(
        tmp_path, "checkpoint", "set", "--run-dir", str(run_dir),
        "--current-slice", "executing", "--next-action", "verify",
        "--artifact", "intent.md",
    )
    assert checkpoint.returncode == 0, checkpoint.stderr
    return run_dir


def close_run(tmp_path: Path, run_dir: Path) -> None:
    """Take a delivered run through acceptance, release and observation close."""
    measures = {
        "outcome": [{
            "id": "functional-correctness", "status": "pass", "evidence_id": "tests",
            "evidence_kind": "deterministic", "value": 1, "target": "pass",
            "aggregation": "single-run",
        }],
        "trajectory": [{
            "id": "verification-completion", "status": "pass", "evidence_id": "tests",
            "evidence_kind": "deterministic", "value": 1, "target": "complete",
            "aggregation": "single-run",
        }],
    }
    (tmp_path / "measures.json").write_text(json.dumps(measures))
    bound = run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir), "--section", "measures",
        "--from", "measures.json",
    )
    assert bound.returncode == 0, bound.stderr

    accepted = run_cli(
        tmp_path, "evidence", "human", "--run-dir", str(run_dir),
        "--id", "acceptance-approval", "--gate", "human-acceptance",
        "--artifact-id", "approval", "--approver", "human-owner",
        "--source", "approval.json",
    )
    assert accepted.returncode == 0, accepted.stderr

    observation = {
        "status": "active",
        "window": {"kind": "event-count", "minimum": 1},
        "signals": ["task-success"],
        "thresholds": {"task-success": {"direction": "gte", "limit": 1}},
        "owner": "human-owner",
        "containment": "revert",
        "privacy": "aggregate",
        "close_condition": "task succeeds",
        "started_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "ended_at": "",
        "observed_events": 0,
        "evidence_ids": [],
    }
    (tmp_path / "observation.json").write_text(json.dumps(observation))
    assert run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir),
        "--section", "observation-plan", "--from", "observation.json",
    ).returncode == 0
    released = run_cli(
        tmp_path, "evidence", "human", "--run-dir", str(run_dir),
        "--id", "release-approval", "--gate", "human-release",
        "--artifact-id", "approval", "--approver", "human-owner",
        "--source", "approval.json",
    )
    assert released.returncode == 0, released.stderr

    measured = run_cli(
        tmp_path, "evidence", "observation", "--run-dir", str(run_dir),
        "--id", "observed-task-success", "--gate", "task-success",
        "--artifact-id", "approval", "--measured-value", "1",
        "--source", "approval.json",
    )
    assert measured.returncode == 0, measured.stderr
    observation.update({
        "status": "pass",
        "ended_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "observed_events": 1,
        "evidence_ids": ["observed-task-success"],
    })
    (tmp_path / "observation.json").write_text(json.dumps(observation))
    assert run_cli(
        tmp_path, "bind", "--run-dir", str(run_dir),
        "--section", "observation-plan", "--from", "observation.json",
    ).returncode == 0


# --------------------------------------------------------------------------
# Happy path
# --------------------------------------------------------------------------


def test_init_derives_tier_and_writes_a_flat_receipt(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")

    result = run_cli(tmp_path, *init_args())

    assert result.returncode == 0, result.stderr
    run_dir = tmp_path / ".agent-run" / "DEL-TEST"
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["risk_tier"] == "routine"
    assert receipt["initial_risk_tier"] == "routine"
    assert receipt["evidence"] == []
    assert "status" not in receipt
    assert "state_history" not in receipt
    assert (run_dir / ".RUN.lock").exists()


def test_init_records_a_known_writer_model(tmp_path):
    (tmp_path / "intent.md").write_text("# Intent\n")

    result = run_cli(tmp_path, *init_args("DEL-WRITER"), "--writer-model", "gpt-6-luna")

    assert result.returncode == 0, result.stderr
    receipt = json.loads((tmp_path / ".agent-run" / "DEL-WRITER" / "RUN.json").read_text())
    assert receipt["writer_model"] == "gpt-6-luna"


def test_producer_run_reaches_a_validator_clean_closed_receipt(tmp_path):
    run_dir = deliver_run(tmp_path)

    delivered = validate(run_dir, tmp_path)
    assert delivered.returncode == 0, delivered.stderr + delivered.stdout
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert "status" not in receipt
    assert "state_history" not in receipt
    artifact = next(
        item for item in receipt["artifacts"] if item["id"] == "evidence-bundle"
    )
    evidence = next(item for item in receipt["evidence"] if item["id"] == "tests")
    assert evidence["status"] == "pass"
    assert evidence["result"]["exit_code"] == 0
    assert evidence["result"]["receipt_digest"] == artifact["digest"]
    assert json.loads((tmp_path / artifact["path"]).read_text())["checks"] == [{
        "id": "tests",
        "gate": "tests",
        "status": "pass",
        "method": "scripts/fixture-check",
        "source_paths": ["intent.md"],
        "exit_code": 0,
    }]
    review = receipt["reviews"][0]
    review_evidence = next(
        item for item in receipt["evidence"] if item["id"] == "review-1"
    )
    assert review["evidence_id"] == "review-1"
    assert review_evidence["model_lineage"] == {
        "adapter": "native-subagent",
        "provider_family": "openai",
        "model": "runtime-model",
    }
    assert review_evidence["route_receipt"]["digest"] == digest_of(
        (tmp_path / "route.json").read_bytes()
    )

    close_run(tmp_path, run_dir)

    assert json.loads((run_dir / "RUN.json").read_text())["delivery_acceptance"] == {
        "basis": "user", "evidence_ids": ["tests", "review-1", "acceptance-approval"],
    }
    closed = validate(run_dir, tmp_path)
    assert closed.returncode == 0, closed.stderr + closed.stdout
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["human_gates"]["release"]["status"] == "approved"
    assert receipt["observation"]["status"] == "pass"


def test_implement_checkpoints_preserve_a_complete_producer_receipt(tmp_path):
    """A real open receipt survives successive implement recovery checkpoints."""
    run_dir = deliver_run(tmp_path)
    checkpoint = load_checkpoint()

    first = checkpoint.update(
        run_dir / "RUN.json", "review", "run verification", ["reviewer-2"],
        ["review.md"],
    )
    second = checkpoint.update(
        run_dir / "RUN.json", "human-gate", "request human acceptance", [], [],
    )

    assert first == {"path": str(run_dir / "RUN.json"), "generation": 2, "verified": True}
    assert second == {"path": str(run_dir / "RUN.json"), "generation": 3, "verified": True}
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["checkpoint"] == {
        "generation": 3,
        "current_slice": "human-gate",
        "next_action": "request human acceptance",
        "in_flight": [],
        "artifact_paths": ["RUN.json", "intent.md", "review.md"],
    }
    assert "updated_at" not in receipt

    validated = validate(run_dir, tmp_path)
    assert validated.returncode == 0, validated.stderr + validated.stdout


# --------------------------------------------------------------------------
# The three failures that have actually happened
# --------------------------------------------------------------------------


def initialise_downgraded_run(tmp_path: Path) -> tuple[Path, Path]:
    tmp_path.mkdir(parents=True, exist_ok=True)
    (tmp_path / "intent.md").write_text("# Intent\n")
    approval = tmp_path / "risk-approval.json"
    approval.write_text('{"approved":true}\n')
    assessment = routine_assessment()
    assessment["critical_surface"] = "build-release-gate"
    args = init_args()
    args[args.index("--risk-assessment") + 1] = json.dumps(assessment)
    args.extend([
        "--risk-tier", "routine",
        "--risk-override", json.dumps({
            "status": "approved",
            "approved_by": "human-owner",
            "evidence": "risk-override-approval",
            "reason": "owner accepted the bounded downgrade",
            "artifact": "risk-approval.json",
            "artifact_id": "risk-override-artifact",
        }),
    ])
    initialised = run_cli(tmp_path, *args)
    assert initialised.returncode == 0, initialised.stderr
    return tmp_path / ".agent-run" / "DEL-TEST", approval


def test_regression_artifact_digest_divergence_is_refused(tmp_path):
    """Receipts once carried digests that did not match the live bytes.

    Incident: issue #546 (lane-authored receipts presented invented and stale
    artifact digests as validating) and its producer-side class M3 in issue
    #584, landed on main as commit 89cf7eda. The producer kept accepting
    mutations on a receipt whose recorded digests had already diverged from
    disk, so a receipt could be carried all the way to closed while attesting
    to bytes that no longer existed.

    Both halves of the guard are asserted: the independent validator rejects a
    digest that no longer matches, and the producer refuses a mutation once the
    live bytes behind a recorded digest have changed underneath it.
    """
    run_dir = deliver_run(tmp_path)
    run_path = run_dir / "RUN.json"
    receipt = json.loads(run_path.read_text())
    receipt["artifacts"][0]["digest"] = "sha256:" + "0" * 64
    run_path.write_text(json.dumps(receipt))

    diverged = validate(run_dir, tmp_path)

    assert diverged.returncode == 1
    assert "digest does not match live bytes" in diverged.stderr + diverged.stdout

    downgraded_dir, approval = initialise_downgraded_run(tmp_path / "second")
    approval.write_text('{"approved":false}\n')

    refused = run_cli(
        tmp_path / "second", "checkpoint", "set", "--run-dir", str(downgraded_dir),
        "--current-slice", "draft", "--next-action", "continue",
    )

    assert refused.returncode == 1
    assert "risk override artifact digest does not match live bytes" in refused.stderr
    assert json.loads(
        (downgraded_dir / "RUN.json").read_text()
    )["checkpoint"]["generation"] == 0


def test_regression_closed_run_refuses_every_mutation(tmp_path):
    """A sealed receipt once still accepted edits after the run had closed.

    Incident: commit d7e1d43a ("harden receipt producer integrity"), salvaged
    onto main as commit 89cf7eda and listed among the #550 integrity defects in
    issue #584. A run already closed accepted artifact adds, checkpoint
    rewrites of current_slice and in_flight, and rebinding of the closed
    observation result, so the attestation could be edited after the run it
    attested to had finished.
    """
    run_dir = deliver_run(tmp_path)
    close_run(tmp_path, run_dir)
    receipt_path = run_dir / "RUN.json"
    sealed = receipt_path.read_bytes()
    sealed_receipt = json.loads(sealed)
    sealed_files = sorted(
        (path.relative_to(run_dir).as_posix(), path.read_bytes())
        for path in run_dir.iterdir()
        if path.is_file() and path.name != ".RUN.lock"
    )
    (tmp_path / "late.json").write_text("{}\n")
    reopened = dict(sealed_receipt["observation"], status="fail")
    (tmp_path / "reopened.json").write_text(json.dumps(reopened))
    marker = tmp_path / "closed-command-ran"

    refusals = {
        "artifact": add_artifact(tmp_path, run_dir, "late-artifact", "late.json"),
        "checkpoint": run_cli(
            tmp_path, "checkpoint", "set", "--run-dir", str(run_dir),
            "--current-slice", "post-close", "--next-action", "should refuse",
            "--in-flight", "late-work",
        ),
        "evidence-run": run_cli(
            tmp_path, "evidence", "run", "--run-dir", str(run_dir),
            "--id", "late-tests", "--gate", "tests",
            "--artifact-id", "evidence-bundle", "--source", "intent.md", "--",
            sys.executable, "-c",
            f"from pathlib import Path; Path({str(marker)!r}).write_text('ran')",
        ),
        "evidence-remove": run_cli(
            tmp_path, "evidence", "remove", "--run-dir", str(run_dir), "--id", "tests",
        ),
        "evidence-rebuild": run_cli(
            tmp_path, "evidence", "rebuild", "--run-dir", str(run_dir),
            "--artifact-id", "evidence-bundle",
        ),
        "observation-bind": run_cli(
            tmp_path, "bind", "--run-dir", str(run_dir),
            "--section", "observation-plan", "--from", "reopened.json",
        ),
    }

    for name, result in refusals.items():
        assert result.returncode == 1, f"{name} was not refused: {result.stdout}"
        assert "closed run is immutable" in result.stderr, name
    assert not marker.exists()
    assert receipt_path.read_bytes() == sealed
    assert sorted(
        (path.relative_to(run_dir).as_posix(), path.read_bytes())
        for path in run_dir.iterdir()
        if path.is_file() and path.name != ".RUN.lock"
    ) == sealed_files




# --------------------------------------------------------------------------
# In-process producer internals
#
# These drive delivery_receipt.py and its process runner directly rather than
# through the CLI, so they are the only measured coverage this file
# contributes: a subprocess run records none. See #758.
# --------------------------------------------------------------------------


def descendant_pair(ready: Path, child_body: str, *, leader_tail: str = "") -> str:
    """Build a leader command that spawns a SIGTERM-immune descendant."""
    child = (
        "import os,pathlib,signal,time; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        + child_body
    )
    return (
        "import pathlib,subprocess,sys,time; "
        f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
        f"ready=pathlib.Path({str(ready)!r}); "
        "deadline=time.monotonic()+0.5; "
        "\nwhile not ready.exists() and time.monotonic()<deadline: time.sleep(0.005)"
        + leader_tail
    )


def wait_for_ready_pid(path: Path, timeout: float = 1) -> int:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            return int(path.read_text())
        except (OSError, ValueError):
            # Absent, or caught between the writer's truncate and write.
            time.sleep(0.005)
    pytest.fail(f"descendant did not write its ready file: {path}")


def wait_for_process_exit(pid: int, timeout: float = 1) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.005)
    return False


def test_evidence_bundle_publication_keeps_old_receipt_valid_on_run_write_failure(
    tmp_path, monkeypatch,
):
    run_dir = initialise(tmp_path)
    install_fixture_command(tmp_path)
    bundle_path = write_bundle(run_dir)
    original_bundle = bundle_path.read_bytes()
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
        "--source", "intent.md", "--", "scripts/fixture-check",
    ])
    with pytest.raises(module.ReceiptError, match="injected RUN write failure"):
        module.command_evidence_run(args)

    assert json.loads((run_dir / "RUN.json").read_text()) == original_receipt
    assert bundle_path.read_bytes() == original_bundle


def test_evidence_run_rechecks_source_scope_after_command_execution(
    tmp_path, monkeypatch,
):
    run_dir = initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["authority"]["allowed_source_paths"] = ["allowed"]
    receipt_path.write_text(json.dumps(receipt))
    (tmp_path / "allowed").mkdir()
    source = tmp_path / "allowed" / "source"
    source.write_text("source\n")
    (tmp_path / "disallowed").mkdir()
    (tmp_path / "disallowed" / "secret").write_text("secret\n")
    write_bundle(run_dir)
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    module = load_producer()

    def swap_source(_command, *, cwd, timeout_seconds=module.EVIDENCE_TIMEOUT_SECONDS):
        del cwd, timeout_seconds
        source.unlink()
        source.symlink_to("../disallowed/secret")
        return 0, "", ""

    monkeypatch.setattr(module, "execute_bounded", swap_source)
    args = module.build_parser().parse_args([
        "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "allowed/source", "--", "scripts/fixture-check",
    ])

    with pytest.raises(module.ReceiptError, match="allowed_source_paths"):
        module.command_evidence_run(args)

    receipt = json.loads(receipt_path.read_text())
    assert not any(item.get("id") == "tests" for item in receipt["evidence"])


def test_review_add_parses_the_exact_route_receipt_bytes_that_were_hashed(
    tmp_path, monkeypatch,
):
    run_dir = initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    route_path = tmp_path / "route.json"
    route_path.write_text(json.dumps(route_receipt(model="different-model")))
    original_read_text = Path.read_text

    def swapped_read_text(path, *args, **kwargs):
        if path.resolve() == route_path.resolve():
            return json.dumps(route_receipt())
        return original_read_text(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", swapped_read_text)
    module = load_producer()
    parsed = module.build_parser().parse_args(review_args(run_dir))

    with pytest.raises(module.ReceiptError, match="route receipt identity"):
        module.command_review_add(parsed)


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
    assert not any(item["id"] == "distinct-family-skip" for item in receipt["evidence"])
    load_validator()._validate_reviews(receipt, {}, required=False)


@pytest.mark.parametrize(("model_family", "provider_family", "passes"), [
    ("openai", "openai", True),
    ("anthropic", "anthropic", False),
    ("google", "openai", True),
    ("openai", "anthropic", False),
    ("openai", None, False),
])
def test_review_add_requires_certification_by_provider_lineage(tmp_path, model_family, provider_family, passes):
    run_dir = initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    route = route_receipt(family=model_family)
    route["certification_eligible"] = False
    if provider_family is None:
        del route["provider_family"]
    else:
        route["provider_family"] = provider_family
    (tmp_path / "route.json").write_text(json.dumps(route))

    result = run_cli(tmp_path, *review_args(run_dir, family=model_family))

    if not passes:
        assert result.returncode != 0
        assert "route receipt identity" in result.stderr
        return
    assert result.returncode == 0, result.stderr
    blocker = run_cli(
        tmp_path, "review", "add", "--run-dir", str(run_dir),
        "--id", "provider-down", "--role", "targeted", "--adapter", "claude",
        "--provider-family", "anthropic", "--lens", "security",
        "--status", "unavailable", "--reason", "provider quota",
    )
    assert blocker.returncode == 0, blocker.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    receipt["risk_tier"] = "substantial"
    validator = load_validator()
    validator._validate_reviews(
        receipt, {item["id"]: item for item in receipt["evidence"]},
        required=True, artifact_root=tmp_path, verify_hashes=True,
    )

    receipt["reviews"] = [item for item in receipt["reviews"] if item["status"] == "pass"]
    with pytest.raises(validator.Invalid, match="recorded blocker"):
        validator._validate_reviews(
            receipt, {item["id"]: item for item in receipt["evidence"]},
            required=True, artifact_root=tmp_path, verify_hashes=True,
        )


@pytest.mark.parametrize("role", ["targeted", "distinct-family"])
@pytest.mark.parametrize(("field", "value", "error"), [
    ("cross_family", False, "cross-family route receipt"),
    ("provider_family", None, "identity does not match"),
    ("provider_family", "", "identity does not match"),
    ("provider_family", "   ", "identity does not match"),
])
def test_validator_rechecks_cross_family_route_lineage_regardless_of_role(tmp_path, role, field, value, error):
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
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    route = route_receipt(family="anthropic")
    route["cross_family"] = True
    (tmp_path / "route.json").write_text(json.dumps(route))
    added = run_cli(
        tmp_path, *review_args(run_dir, role=role, family="anthropic")
    )
    assert added.returncode == 0, added.stderr

    validator = load_validator()
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    validator.validate(receipt, ROOT, workspace_root=tmp_path, verify_hashes=True)

    route[field] = value
    raw = json.dumps(route).encode()
    (tmp_path / "route.json").write_bytes(raw)
    digest = digest_of(raw)
    next(
        item for item in receipt["artifacts"] if item["id"] == "review-1.route"
    )["digest"] = digest
    next(
        item for item in receipt["evidence"] if item["id"] == "review-1"
    )["route_receipt"]["digest"] = digest
    next(
        item for item in receipt["reviews"] if item["id"] == "review-1"
    )["route_receipt_digest"] = digest
    receipt_path.write_text(json.dumps(receipt))

    with pytest.raises(validator.Invalid, match=error):
        validator.validate(receipt, ROOT, workspace_root=tmp_path, verify_hashes=True)


@pytest.mark.parametrize(
    ("role", "provider_family", "cross_family", "error"),
    [
        pytest.param("targeted", "anthropic", False, "cross-family route receipt", id="targeted-needs-cross-family-proof"),
        pytest.param("distinct-family", "anthropic", False, "cross-family route receipt", id="distinct-family-needs-cross-family-proof"),
        pytest.param("targeted", None, True, "identity does not match", id="missing-provider-family"),
        pytest.param("targeted", "", True, "identity does not match", id="empty-provider-family"),
        pytest.param("targeted", "   ", True, "identity does not match", id="blank-provider-family"),
    ],
)
def test_review_add_rejects_invalid_cross_family_route_regardless_of_role(
    tmp_path, role, provider_family, cross_family, error,
):
    run_dir = initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    route = route_receipt(family="anthropic")
    route.update({"provider_family": provider_family, "cross_family": cross_family})
    (tmp_path / "route.json").write_text(json.dumps(route))

    rejected = run_cli(tmp_path, *review_args(run_dir, role=role, family="anthropic"))

    assert rejected.returncode != 0
    assert error in rejected.stderr
    assert json.loads((run_dir / "RUN.json").read_text())["reviews"] == []


def test_review_add_rejects_whitespace_only_lens(tmp_path):
    run_dir = initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    (tmp_path / "route.json").write_text(json.dumps(route_receipt()))
    args = review_args(run_dir)
    args[-1] = "   "

    rejected = run_cli(tmp_path, *args)

    assert rejected.returncode != 0
    assert "non-empty lens" in rejected.stderr


def test_bounded_execution_times_out_and_truncates_output():
    module = load_producer()

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            cwd=ROOT,
            timeout_seconds=0.05,
        )
    noisy = (
        f"import sys; sys.stdout.write('x'*{module.MAX_LOG_BYTES * 2}); "
        f"sys.stderr.write('y'*{module.MAX_LOG_BYTES * 2})"
    )

    exit_code, stdout, stderr = module.execute_bounded(
        [sys.executable, "-c", noisy], cwd=ROOT,
    )

    assert exit_code == 0
    assert len(stdout.encode()) <= module.MAX_LOG_BYTES
    assert len(stderr.encode()) <= module.MAX_LOG_BYTES


def test_bounded_execution_enforces_deadline_after_output_closes():
    module = load_producer()
    started = time.monotonic()

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [
                sys.executable, "-c",
                "import os,time; os.close(1); os.close(2); time.sleep(2)",
            ],
            cwd=ROOT,
            timeout_seconds=0.05,
        )

    assert time.monotonic() - started < 1


def test_bounded_execution_kills_descendants_on_timeout(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    release = tmp_path / "descendant-release"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        f"pathlib.Path({str(ready)!r}).write_text(str(os.getpid())); "
        f"\nwhile not pathlib.Path({str(release)!r}).exists(): time.sleep(0.005)\n"
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
        leader_tail="\ntime.sleep(60)",
    )

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=0.25,
        )
    pid = wait_for_ready_pid(ready)
    release.touch()
    assert wait_for_process_exit(pid)
    assert not marker.exists()


def test_bounded_execution_kills_descendants_after_successful_leader_exit(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    release = tmp_path / "descendant-release"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        "os.close(1); os.close(2); "
        f"pathlib.Path({str(ready)!r}).write_text(str(os.getpid())); "
        f"\nwhile not pathlib.Path({str(release)!r}).exists(): time.sleep(0.005)\n"
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )
    assert exit_code == 0
    pid = wait_for_ready_pid(ready)
    release.touch()
    assert wait_for_process_exit(pid)
    assert not marker.exists()


def test_bounded_execution_preserves_success_while_cleaning_descendants(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    release = tmp_path / "descendant-release"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        f"pathlib.Path({str(ready)!r}).write_text(str(os.getpid())); "
        f"\nwhile not pathlib.Path({str(release)!r}).exists(): time.sleep(0.005)\n"
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )

    assert exit_code == 0
    pid = wait_for_ready_pid(ready)
    release.touch()
    assert wait_for_process_exit(pid)
    assert not marker.exists()


def test_bounded_execution_stops_draining_pipes_from_escaped_descendant(tmp_path):
    module = load_producer()
    child_pid = tmp_path / "escaped-child-pid"
    leader = descendant_pair(
        child_pid,
        f"os.setsid(); pathlib.Path({str(child_pid)!r}).write_text(str(os.getpid())); "
        "\nwhile True: time.sleep(0.005)",
    )
    started = time.monotonic()

    try:
        exit_code, _stdout, _stderr = module.execute_bounded(
            [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
        )
    finally:
        if child_pid.exists():
            try:
                os.kill(int(child_pid.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass

    assert exit_code == 0
    assert time.monotonic() - started < 0.6


def test_post_sigkill_wait_uses_executor_timeout(monkeypatch):
    module = load_producer()
    waits = []

    class StuckProcess:
        pid = 12345

        def wait(self, timeout=None):
            waits.append(timeout)
            raise subprocess.TimeoutExpired(["stuck-process"], timeout)

    monkeypatch.setattr(module.process_runner.os, "killpg", lambda _pid, _signal: None)
    monkeypatch.setattr(module.process_runner.time, "sleep", lambda _seconds: None)

    with pytest.raises(
        module.ReceiptError, match="did not exit after SIGKILL within 0.25 seconds",
    ):
        module.process_runner.terminate_process_group(
            StuckProcess(), timeout_seconds=0.25, error_type=module.ReceiptError,
        )

    assert waits == [0.1, 0.25]


def test_bounded_execution_closes_resources_when_termination_raises(
    tmp_path, monkeypatch,
):
    module = load_producer()
    real_selector = module.process_runner.selectors.DefaultSelector
    real_popen = module.process_runner.subprocess.Popen
    real_terminate = module.process_runner.terminate_process_group
    selectors: list = []
    processes: list = []
    termination_attempts: list = []

    class TrackingSelector:
        def __init__(self):
            self.inner = real_selector()
            self.closed = False
            selectors.append(self)

        def __getattr__(self, name):
            return getattr(self.inner, name)

        def close(self):
            self.closed = True
            self.inner.close()

    def tracking_popen(*args, **kwargs):
        process = real_popen(*args, **kwargs)
        processes.append(process)
        return process

    def failing_terminate(process, *, timeout_seconds, error_type):
        termination_attempts.append(process.pid)
        real_terminate(process, timeout_seconds=timeout_seconds, error_type=error_type)
        raise error_type("forced termination failure")

    monkeypatch.setattr(
        module.process_runner.selectors, "DefaultSelector", TrackingSelector,
    )
    monkeypatch.setattr(module.process_runner.subprocess, "Popen", tracking_popen)
    monkeypatch.setattr(module.process_runner, "terminate_process_group", failing_terminate)

    with pytest.raises(module.ReceiptError, match="forced termination failure"):
        module.execute_bounded(
            [sys.executable, "-c", "pass"], cwd=tmp_path, timeout_seconds=1,
        )

    assert len(termination_attempts) == 1
    assert selectors[0].closed
    assert processes[0].stdout is not None and processes[0].stdout.closed
    assert processes[0].stderr is not None and processes[0].stderr.closed


def test_ordinary_acceptance_is_validated_and_freezes_bound_sections(tmp_path):
    run_dir = deliver_run(tmp_path)
    command = ("accept", "--run-dir", str(run_dir), "--basis", "checks-and-review",
               "--evidence", "tests", "--evidence", "review-1")
    before = (run_dir / "RUN.json").read_bytes()
    refused = run_cli(tmp_path, *command)
    assert refused.returncode != 0
    assert (run_dir / "RUN.json").read_bytes() == before
    reference = load(ROOT / "skills/deliver/scripts/reference_runs.py", "accept_reference")
    measures = reference.make_reference_run("software", ROOT)["measures"]
    (tmp_path / "measures.json").write_text(json.dumps(measures))
    assert run_cli(tmp_path, "bind", "--run-dir", str(run_dir), "--section", "measures",
                   "--from", "measures.json").returncode == 0
    accepted = run_cli(tmp_path, *command)
    assert accepted.returncode == 0, accepted.stderr
    run = json.loads((run_dir / "RUN.json").read_text())
    assert run["delivery_acceptance"]["basis"] == "checks-and-review"
    assert run["human_gates"]["acceptance"]["status"] == "pending"
    assert run["human_gates"]["release"]["status"] == "pending"
    assert validate(run_dir, tmp_path).returncode == 0
    for section in ("measures", "assurance-plan", "security-plan", "software-delivery"):
        (tmp_path / "replacement.json").write_text('{}')
        changed = run_cli(tmp_path, "bind", "--run-dir", str(run_dir), "--section", section,
                          "--from", "replacement.json")
        assert changed.returncode == 1
        assert "gate is already closed" in changed.stderr
    refused = run_cli(tmp_path, *command[:-1], "missing")
    assert refused.returncode == 1
