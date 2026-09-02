"""Producer tests for the flat RUN.json receipt writer.

The suite is deliberately narrow: one happy path that walks a run from init to
a closed, validator-clean receipt, named regressions for the three failures
that have actually happened, and the in-process cases that exercise the
producer's own code paths. Everything else the producer refuses is covered by
the independent validator suite; see #758.
"""

import hashlib
import importlib.util
import json
import os
import signal
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"
VALIDATOR = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def load_producer():
    return load(PRODUCER, "delivery_receipt_under_test")


def load_validator():
    return load(VALIDATOR, "validate_delivery_for_producer")


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


def install_canonical_check_harness(workspace: Path, body: str = "printf 'pass\\n'") -> None:
    script = workspace / "scripts" / "check-harness"
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
    install_canonical_check_harness(tmp_path)
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
        "--source", "intent.md", "--", "scripts/check-harness",
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
        "method": "scripts/check-harness",
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

    closed = validate(run_dir, tmp_path)
    assert closed.returncode == 0, closed.stderr + closed.stdout
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert receipt["human_gates"]["release"]["status"] == "approved"
    assert receipt["observation"]["status"] == "pass"


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


def test_regression_tests_gate_records_only_the_canonical_harness(tmp_path):
    """Scoped test runs were once recorded as if they were the full gate.

    Incident: issue #582, fixed by commit a2e7205b and narrowed by commit
    c6be0a93. Two lanes in one run recorded gate evidence as "tests: pass" from
    a narrow selection, one reporting 15 passing tests against a brief
    specifying thousands, because the receipt recorded the gate status without
    recording what was actually executed.

    The narrowing in c6be0a93 matters as much as the refusal: the canonical
    command is only demanded where a canonical harness exists, so a workspace
    without one can still record a generic tests gate.
    """
    run_dir = initialise(tmp_path)
    install_canonical_check_harness(tmp_path)
    write_bundle(run_dir)
    assert add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    evidence_run = [
        "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "intent.md", "--",
    ]

    scoped = run_cli(tmp_path, *evidence_run, sys.executable, "-c", "print('green')")

    assert scoped.returncode == 1
    assert "scripts/check-harness" in scoped.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert not any(item["id"] == "tests" for item in receipt["evidence"])

    (tmp_path / "scripts" / "check-harness").unlink()
    without_harness = run_cli(
        tmp_path, *evidence_run, sys.executable, "-c", "print('green')",
    )

    assert without_harness.returncode == 0, without_harness.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    row = next(item for item in receipt["evidence"] if item["id"] == "tests")
    assert row["method"] == f"{sys.executable} -c 'print('\"'\"'green'\"'\"')'"
    assert row["stdout"] == "green\n"
    assert row["result"]["exit_code"] == 0


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


def test_evidence_bundle_publication_keeps_old_receipt_valid_on_run_write_failure(
    tmp_path, monkeypatch,
):
    run_dir = initialise(tmp_path)
    install_canonical_check_harness(tmp_path)
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
        "--source", "intent.md", "--", "scripts/check-harness",
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
        "--source", "allowed/source", "--", "scripts/check-harness",
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


def test_validator_rechecks_other_primary_route_receipt_cross_family(tmp_path):
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
        tmp_path, *review_args(run_dir, role="other-primary", family="anthropic")
    )
    assert added.returncode == 0, added.stderr

    validator = load_validator()
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    validator.validate(receipt, ROOT, workspace_root=tmp_path, verify_hashes=True)

    route["cross_family"] = False
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

    with pytest.raises(validator.Invalid, match="cross-family route receipt"):
        validator.validate(receipt, ROOT, workspace_root=tmp_path, verify_hashes=True)


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
                "import os,time; os.close(1); os.close(2); time.sleep(0.2)",
            ],
            cwd=ROOT,
            timeout_seconds=0.05,
        )

    assert time.monotonic() - started < 0.25


def test_bounded_execution_kills_descendants_on_timeout(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        f"pathlib.Path({str(ready)!r}).write_text('ready'); time.sleep(0.6); "
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
        leader_tail="\ntime.sleep(60)",
    )

    with pytest.raises(module.ReceiptError, match="timed out"):
        module.execute_bounded(
            [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=0.25,
        )
    time.sleep(0.7)

    assert ready.exists()
    assert not marker.exists()


def test_bounded_execution_kills_descendants_after_successful_leader_exit(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    marker = tmp_path / "descendant-survived"
    leader = descendant_pair(
        ready,
        "os.close(1); os.close(2); "
        f"pathlib.Path({str(ready)!r}).write_text('ready'); time.sleep(0.6); "
        f"pathlib.Path({str(marker)!r}).write_text('alive')",
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )
    time.sleep(0.7)

    assert exit_code == 0
    assert ready.exists()
    assert not marker.exists()


def test_bounded_execution_preserves_success_while_cleaning_descendants(tmp_path):
    module = load_producer()
    ready = tmp_path / "descendant-ready"
    leader = descendant_pair(
        ready, f"pathlib.Path({str(ready)!r}).write_text('ready'); time.sleep(1)",
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )

    assert exit_code == 0
    assert ready.exists()


def test_bounded_execution_stops_draining_pipes_from_escaped_descendant(tmp_path):
    module = load_producer()
    child_pid = tmp_path / "escaped-child-pid"
    leader = descendant_pair(
        child_pid,
        f"os.setsid(); pathlib.Path({str(child_pid)!r}).write_text(str(os.getpid())); "
        "time.sleep(1)",
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
