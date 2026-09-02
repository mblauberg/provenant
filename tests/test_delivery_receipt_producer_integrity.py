import importlib.util
import json
import os
import signal
import subprocess
import sys
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"
HELPERS = ROOT / "tests" / "test_delivery_receipt_producer.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def helpers():
    return load(HELPERS, "delivery_receipt_integrity_helpers")


def producer():
    return load(PRODUCER, "delivery_receipt_integrity_producer")


def run_cli(workspace: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(PRODUCER), *args],
        cwd=workspace,
        text=True,
        capture_output=True,
        check=False,
    )


def timestamp(offset: timedelta) -> str:
    return (datetime.now(UTC) + offset).isoformat().replace("+00:00", "Z")


def test_bind_refuses_to_replace_the_observation_plan_after_release(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["human_gates"]["release"] = {
        "status": "approved", "approver": "human-owner", "evidence": "release-approval",
    }
    receipt["evidence"].append({
        "id": "release-approval", "kind": "human", "gate": "human-release",
        "status": "pass",
    })
    receipt["observation"]["status"] = "active"
    original = json.loads(json.dumps(receipt["observation"]))
    replacement = json.loads(json.dumps(original))
    replacement["thresholds"]["citation-audit"]["limit"] = 999
    receipt_path.write_text(json.dumps(receipt))
    (tmp_path / "replacement.json").write_text(json.dumps(replacement))

    result = run_cli(
        tmp_path,
        "bind",
        "--run-dir",
        str(run_dir),
        "--section",
        "observation-plan",
        "--from",
        "replacement.json",
    )

    assert result.returncode == 1
    assert "observation gate is already closed" in result.stderr
    assert json.loads(receipt_path.read_text())["observation"] == original


def test_rebuild_refuses_to_mutate_a_referenced_sibling_digest(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    (tmp_path / "scripts").mkdir()
    check = tmp_path / "scripts" / "check-harness"
    check.write_text("#!/bin/sh\nexit 0\n")
    check.chmod(0o755)
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    assert support.add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    assert run_cli(
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
        "scripts/check-harness",
    ).returncode == 0
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    row = next(item for item in receipt["evidence"] if item["id"] == "tests")
    original_digest = row["result"]["receipt_digest"]
    row["method"] = "changed after citation"
    receipt["measures"]["outcome"] = [{"id": "task-success", "evidence_id": "tests"}]
    receipt_path.write_text(json.dumps(receipt))

    result = run_cli(
        tmp_path,
        "evidence",
        "rebuild",
        "--run-dir",
        str(run_dir),
        "--artifact-id",
        "evidence-bundle",
    )

    assert result.returncode == 1
    assert "tests is referenced by measures" in result.stderr
    current = json.loads(receipt_path.read_text())
    current_row = next(item for item in current["evidence"] if item["id"] == "tests")
    assert current_row["result"]["receipt_digest"] == original_digest


def test_artifact_scope_rejects_symlink_that_resolves_outside_declared_root(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["authority"]["allowed_artifact_paths"] = [
        "allowed",
        ".agent-run/DEL-TEST",
    ]
    receipt_path.write_text(json.dumps(receipt))
    (tmp_path / "allowed").mkdir()
    (tmp_path / "outside.txt").write_text("outside\n")
    (tmp_path / "allowed" / "escape.txt").symlink_to("../outside.txt")

    result = support.add_artifact(
        tmp_path,
        run_dir,
        "escaped",
        "allowed/escape.txt",
        artifact_class="canonical",
        artifact_type="documentation",
        media_type="text/plain",
    )

    assert result.returncode == 1
    assert "leaves authority.allowed_artifact_paths" in result.stderr


def test_evidence_source_scope_rejects_symlink_outside_declared_root(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["authority"]["allowed_source_paths"] = ["allowed"]
    receipt_path.write_text(json.dumps(receipt))
    (tmp_path / "allowed").mkdir()
    (tmp_path / "disallowed").mkdir()
    (tmp_path / "disallowed" / "secret").write_text("secret\n")
    (tmp_path / "allowed" / "link").symlink_to("../disallowed/secret")
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    assert support.add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    marker = tmp_path / "evidence-command-ran"

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
        "allowed/link",
        "--",
        sys.executable,
        "-c",
        f"from pathlib import Path; Path({str(marker)!r}).write_text('ran')",
    )

    assert result.returncode == 1
    assert "evidence source leaves authority.allowed_source_paths" in result.stderr
    assert not marker.exists()
    receipt = json.loads(receipt_path.read_text())
    assert not any(item.get("id") == "tests" for item in receipt["evidence"])


def test_evidence_run_rechecks_source_scope_after_command_execution(
    tmp_path, monkeypatch,
):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["authority"]["allowed_source_paths"] = ["allowed"]
    receipt_path.write_text(json.dumps(receipt))
    allowed = tmp_path / "allowed"
    allowed.mkdir()
    source = allowed / "source"
    source.write_text("source\n")
    disallowed = tmp_path / "disallowed"
    disallowed.mkdir()
    outside = disallowed / "secret"
    outside.write_text("secret\n")
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}'
    )
    assert support.add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    module = producer()

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


def initialise_downgraded_run(tmp_path: Path) -> tuple[Path, Path]:
    support = helpers()
    (tmp_path / "intent.md").write_text("# Intent\n")
    approval = tmp_path / "risk-approval.json"
    approval.write_text('{"approved":true}\n')
    assessment = support.routine_assessment()
    assessment["critical_surface"] = "build-release-gate"
    args = support.init_args()
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
    initialised = run_cli(tmp_path, *args)
    assert initialised.returncode == 0, initialised.stderr
    return tmp_path / ".agent-run" / "DEL-TEST", approval


def test_mutation_refuses_changed_live_risk_override_artifact(tmp_path):
    run_dir, approval = initialise_downgraded_run(tmp_path)
    approval.write_text('{"approved":false}\n')

    result = run_cli(
        tmp_path,
        "checkpoint",
        "set",
        "--run-dir",
        str(run_dir),
        "--current-slice",
        "draft",
        "--next-action",
        "continue",
    )

    assert result.returncode == 1
    assert "risk override artifact digest does not match live bytes" in result.stderr
    assert json.loads((run_dir / "RUN.json").read_text())["checkpoint"]["generation"] == 0


def test_mutation_rechecks_risk_override_after_evidence_command(tmp_path):
    support = helpers()
    run_dir, approval = initialise_downgraded_run(tmp_path)
    (tmp_path / "scripts").mkdir()
    check = tmp_path / "scripts" / "check-harness"
    check.write_text(
        f"#!/bin/sh\nprintf changed > {str(approval)!r}\n"
    )
    check.chmod(0o755)
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    assert support.add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0
    before = sorted(run_dir.iterdir())

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
        "scripts/check-harness",
    )

    assert result.returncode == 1
    assert "risk override artifact digest does not match live bytes" in result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert not any(item["id"] == "tests" for item in receipt["evidence"])
    assert sorted(run_dir.iterdir()) == before


def test_rebuild_refuses_risk_override_artifact_as_bundle(tmp_path):
    run_dir, _approval = initialise_downgraded_run(tmp_path)

    result = run_cli(
        tmp_path,
        "evidence",
        "rebuild",
        "--run-dir",
        str(run_dir),
        "--artifact-id",
        "risk-override-artifact",
    )

    assert result.returncode == 1
    assert "risk override artifact cannot be a deterministic bundle" in result.stderr


def test_deterministic_bundle_refuses_human_evidence_artifact(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    approval_path = tmp_path / "approval.json"
    approval_path.write_text('{"approved":true}\n')
    assert support.add_artifact(
        tmp_path, run_dir, "approval", "approval.json"
    ).returncode == 0
    accepted = run_cli(
        tmp_path,
        "evidence",
        "human",
        "--run-dir",
        str(run_dir),
        "--id",
        "acceptance",
        "--gate",
        "human-acceptance",
        "--artifact-id",
        "approval",
        "--approver",
        "human-owner",
        "--source",
        "approval.json",
    )
    assert accepted.returncode == 0, accepted.stderr
    before = json.loads((run_dir / "RUN.json").read_text())
    before_artifact = next(
        item for item in before["artifacts"] if item["id"] == "approval"
    )
    before_files = sorted(run_dir.iterdir())
    marker = tmp_path / "deterministic-command-ran"

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
        "approval",
        "--source",
        "approval.json",
        "--",
        sys.executable,
        "-c",
        f"from pathlib import Path; Path({str(marker)!r}).write_text('ran')",
    )

    assert result.returncode == 1
    assert "human evidence artifact cannot be a deterministic bundle" in result.stderr
    assert not marker.exists()
    assert sorted(run_dir.iterdir()) == before_files
    current = json.loads((run_dir / "RUN.json").read_text())
    assert next(
        item for item in current["artifacts"] if item["id"] == "approval"
    ) == before_artifact
    assert not any(item.get("id") == "tests" for item in current["evidence"])


def test_closed_run_refuses_artifact_and_checkpoint_mutations(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["human_gates"]["release"] = {
        "status": "approved", "approver": "human-owner", "evidence": "release-approval",
    }
    receipt["evidence"].append({
        "id": "release-approval", "kind": "human", "gate": "human-release",
        "status": "pass",
    })
    receipt["observation"]["status"] = "pass"
    receipt_path.write_text(json.dumps(receipt))
    (tmp_path / "late.json").write_text("{}\n")

    artifact = support.add_artifact(
        tmp_path, run_dir, "late-artifact", "late.json"
    )
    checkpoint = run_cli(
        tmp_path,
        "checkpoint",
        "set",
        "--run-dir",
        str(run_dir),
        "--current-slice",
        "post-close",
        "--next-action",
        "should refuse",
        "--in-flight",
        "late-work",
    )

    assert artifact.returncode == 1
    assert checkpoint.returncode == 1
    assert "closed run is immutable" in artifact.stderr
    assert "closed run is immutable" in checkpoint.stderr
    current = json.loads(receipt_path.read_text())
    assert not any(
        item.get("id") == "late-artifact" for item in current["artifacts"]
    )
    assert current["checkpoint"] == receipt["checkpoint"]


def test_closed_run_refuses_manual_evidence_mutations(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    (tmp_path / "scripts").mkdir()
    check = tmp_path / "scripts" / "check-harness"
    check.write_text("#!/bin/sh\nexit 0\n")
    check.chmod(0o755)
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    added = support.add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    )
    assert added.returncode == 0, added.stderr
    executed = run_cli(
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
        "scripts/check-harness",
    )
    assert executed.returncode == 0, executed.stderr
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["human_gates"]["release"] = {
        "status": "approved", "approver": "human-owner", "evidence": "release-approval",
    }
    receipt["evidence"].append({
        "id": "release-approval", "kind": "human", "gate": "human-release",
        "status": "pass",
    })
    receipt["observation"]["status"] = "pass"
    receipt_path.write_text(json.dumps(receipt))
    before_receipt = receipt_path.read_bytes()
    before_files = sorted(
        (path.relative_to(run_dir).as_posix(), path.read_bytes())
        for path in run_dir.iterdir()
        if path.is_file() and path.name != ".RUN.lock"
    )
    marker = tmp_path / "closed-command-ran"

    results = [
        run_cli(
            tmp_path,
            "evidence",
            "run",
            "--run-dir",
            str(run_dir),
            "--id",
            "late-tests",
            "--gate",
            "tests",
            "--artifact-id",
            "evidence-bundle",
            "--source",
            "intent.md",
            "--",
            sys.executable,
            "-c",
            f"from pathlib import Path; Path({str(marker)!r}).write_text('ran')",
        ),
        run_cli(
            tmp_path,
            "evidence",
            "remove",
            "--run-dir",
            str(run_dir),
            "--id",
            "tests",
        ),
        run_cli(
            tmp_path,
            "evidence",
            "rebuild",
            "--run-dir",
            str(run_dir),
            "--artifact-id",
            "evidence-bundle",
        ),
    ]

    assert all(result.returncode == 1 for result in results)
    assert all("closed run is immutable" in result.stderr for result in results)
    assert not marker.exists()
    assert receipt_path.read_bytes() == before_receipt
    assert sorted(
        (path.relative_to(run_dir).as_posix(), path.read_bytes())
        for path in run_dir.iterdir()
        if path.is_file() and path.name != ".RUN.lock"
    ) == before_files


def review_args(
    run_dir: Path, *, role: str = "targeted", family: str = "openai",
) -> list[str]:
    return [
        "review",
        "add",
        "--run-dir",
        str(run_dir),
        "--id",
        "review-1",
        "--role",
        role,
        "--artifact",
        "review.md",
        "--route-receipt",
        "route.json",
        "--reviewer-id",
        "reviewer-1",
        "--adapter",
        "native-subagent",
        "--provider-family",
        family,
        "--model",
        "runtime-model",
        "--lens",
        "correctness",
    ]


def test_review_add_parses_the_exact_route_receipt_bytes_that_were_hashed(
    tmp_path, monkeypatch,
):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    mismatched = support.route_receipt(model="different-model")
    matching = support.route_receipt()
    route_path = tmp_path / "route.json"
    route_path.write_text(json.dumps(mismatched))
    original_read_text = Path.read_text

    def swapped_read_text(path, *args, **kwargs):
        if path.resolve() == route_path.resolve():
            return json.dumps(matching)
        return original_read_text(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", swapped_read_text)
    module = producer()
    parsed = module.build_parser().parse_args(review_args(run_dir))

    with pytest.raises(module.ReceiptError, match="route receipt identity"):
        module.command_review_add(parsed)


def test_review_add_refuses_other_primary_without_cross_family_dispatch(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    (tmp_path / "route.json").write_text(json.dumps(
        support.route_receipt(family="anthropic")
    ))

    result = run_cli(
        tmp_path,
        *review_args(run_dir, role="other-primary", family="anthropic"),
    )

    assert result.returncode == 1
    assert "other-primary review requires a cross-family route receipt" in result.stderr


def test_evidence_run_refuses_custom_command_for_full_tests_gate(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    support.install_canonical_check_harness(tmp_path)
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    assert support.add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0

    result = run_cli(
        tmp_path,
        "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "intent.md", "--", sys.executable, "-c", "print('green')",
    )

    assert result.returncode == 1
    assert "scripts/check-harness" in result.stderr


def test_evidence_run_allows_custom_command_for_generic_tests_gate(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    assert support.add_artifact(
        tmp_path, run_dir, "evidence-bundle", ".agent-run/DEL-TEST/evidence.json"
    ).returncode == 0

    result = run_cli(
        tmp_path,
        "evidence", "run", "--run-dir", str(run_dir), "--id", "tests",
        "--gate", "tests", "--artifact-id", "evidence-bundle",
        "--source", "intent.md", "--", sys.executable, "-c", "print('green')",
    )

    assert result.returncode == 0, result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    row = next(item for item in receipt["evidence"] if item["id"] == "tests")
    assert row["method"] == f"{sys.executable} -c 'print('\"'\"'green'\"'\"')'"
    assert row["stdout"] == "green\n"
    assert row["stderr"] == ""
    assert row["result"]["exit_code"] == 0


def test_validator_rechecks_other_primary_route_receipt_cross_family(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    (tmp_path / "approval.json").write_text(
        '{"approved":true,"approver":"human-owner"}\n'
    )
    assert support.add_artifact(tmp_path, run_dir, "approval", "approval.json").returncode == 0
    assert run_cli(
        tmp_path, "evidence", "human", "--run-dir", str(run_dir),
        "--id", "authority-approval", "--gate", "authority-approval",
        "--artifact-id", "approval", "--approver", "human-owner",
    ).returncode == 0
    (tmp_path / "review.md").write_text("# Review\n\nClean.\n")
    route = support.route_receipt(family="anthropic")
    route["cross_family"] = True
    (tmp_path / "route.json").write_text(json.dumps(route))
    added = run_cli(tmp_path, *review_args(run_dir, role="other-primary", family="anthropic"))
    assert added.returncode == 0, added.stderr

    validator = load(helpers().ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py", "route_receipt_validator")
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    validator.validate(receipt, helpers().ROOT, workspace_root=tmp_path, verify_hashes=True)

    route["cross_family"] = False
    raw = json.dumps(route).encode()
    (tmp_path / "route.json").write_bytes(raw)
    digest = "sha256:" + __import__("hashlib").sha256(raw).hexdigest()
    route_artifact = next(item for item in receipt["artifacts"] if item["id"] == "review-1.route")
    route_artifact["digest"] = digest
    evidence = next(item for item in receipt["evidence"] if item["id"] == "review-1")
    evidence["route_receipt"]["digest"] = digest
    next(item for item in receipt["reviews"] if item["id"] == "review-1")["route_receipt_digest"] = digest
    receipt_path.write_text(json.dumps(receipt))

    with pytest.raises(validator.Invalid, match="cross-family route receipt"):
        validator.validate(receipt, helpers().ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_bounded_execution_kills_descendants_after_successful_leader_exit(tmp_path):
    module = producer()
    ready = tmp_path / "descendant-ready"
    marker = tmp_path / "descendant-survived"
    child = (
        "import os,pathlib,signal,time; "
        "os.close(1); os.close(2); signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        f"pathlib.Path({str(ready)!r}).write_text('ready'); time.sleep(0.6); "
        f"pathlib.Path({str(marker)!r}).write_text('alive')"
    )
    leader = (
        "import pathlib,subprocess,sys,time; "
        f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
        f"ready=pathlib.Path({str(ready)!r}); "
        "deadline=time.monotonic()+0.5; "
        "\nwhile not ready.exists() and time.monotonic()<deadline: time.sleep(0.005)"
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )
    time.sleep(0.7)

    assert exit_code == 0
    assert ready.exists()
    assert not marker.exists()


def test_post_sigkill_wait_uses_executor_timeout(monkeypatch):
    module = producer()
    waits = []

    class StuckProcess:
        pid = 12345

        def wait(self, timeout=None):
            waits.append(timeout)
            raise subprocess.TimeoutExpired(["stuck-process"], timeout)

    monkeypatch.setattr(module.process_runner.os, "killpg", lambda _pid, _signal: None)
    monkeypatch.setattr(module.process_runner.time, "sleep", lambda _seconds: None)

    with pytest.raises(
        module.ReceiptError,
        match="did not exit after SIGKILL within 0.25 seconds",
    ):
        module.process_runner.terminate_process_group(
            StuckProcess(), timeout_seconds=0.25, error_type=module.ReceiptError,
        )

    assert waits == [0.1, 0.25]


def test_bounded_execution_closes_resources_when_termination_raises(
    tmp_path, monkeypatch,
):
    module = producer()
    real_selector = module.process_runner.selectors.DefaultSelector
    real_popen = module.process_runner.subprocess.Popen
    real_terminate = module.process_runner.terminate_process_group
    selectors = []
    processes = []
    termination_attempts = []

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
        real_terminate(
            process, timeout_seconds=timeout_seconds, error_type=error_type,
        )
        raise error_type("forced termination failure")

    monkeypatch.setattr(
        module.process_runner.selectors, "DefaultSelector", TrackingSelector,
    )
    monkeypatch.setattr(module.process_runner.subprocess, "Popen", tracking_popen)
    monkeypatch.setattr(
        module.process_runner, "terminate_process_group", failing_terminate,
    )

    with pytest.raises(module.ReceiptError, match="forced termination failure"):
        module.execute_bounded(
            [sys.executable, "-c", "pass"],
            cwd=tmp_path,
            timeout_seconds=1,
        )

    assert len(termination_attempts) == 1
    assert selectors[0].closed
    assert processes[0].stdout is not None and processes[0].stdout.closed
    assert processes[0].stderr is not None and processes[0].stderr.closed


def test_bounded_execution_preserves_success_while_cleaning_descendants(tmp_path):
    module = producer()
    ready = tmp_path / "descendant-ready"
    child = (
        "import pathlib,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        f"pathlib.Path({str(ready)!r}).write_text('ready'); "
        "time.sleep(1)"
    )
    leader = (
        "import pathlib,subprocess,sys,time; "
        f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
        f"ready=pathlib.Path({str(ready)!r}); "
        "deadline=time.monotonic()+0.5; "
        "\nwhile not ready.exists() and time.monotonic()<deadline: time.sleep(0.005)"
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=1,
    )

    assert exit_code == 0
    assert ready.exists()


def test_bounded_execution_stops_draining_pipes_from_escaped_descendant(tmp_path):
    module = producer()
    child_pid = tmp_path / "escaped-child-pid"
    child = (
        "import os,pathlib,signal,time; os.setsid(); "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        f"pathlib.Path({str(child_pid)!r}).write_text(str(os.getpid())); "
        "time.sleep(1)"
    )
    leader = (
        "import pathlib,subprocess,sys,time; "
        f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
        f"ready=pathlib.Path({str(child_pid)!r}); "
        "deadline=time.monotonic()+0.5; "
        "\nwhile not ready.exists() and time.monotonic()<deadline: time.sleep(0.005)"
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


def test_bind_refuses_to_change_closed_observation_results(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    original = json.loads(json.dumps(receipt["observation"]))
    original["status"] = "pass"
    receipt["observation"] = original
    receipt["human_gates"]["release"] = {
        "status": "approved", "approver": "human-owner", "evidence": "release-approval",
    }
    receipt["evidence"].append({
        "id": "release-approval", "kind": "human", "gate": "human-release",
        "status": "pass",
    })
    receipt_path.write_text(json.dumps(receipt))
    replacement = json.loads(json.dumps(original))
    replacement["status"] = "fail"
    (tmp_path / "replacement.json").write_text(json.dumps(replacement))

    result = run_cli(
        tmp_path,
        "bind",
        "--run-dir",
        str(run_dir),
        "--section",
        "observation-plan",
        "--from",
        "replacement.json",
    )

    assert result.returncode == 1
    assert "closed run is immutable" in result.stderr
    assert json.loads(receipt_path.read_text())["observation"] == original


def test_mutation_rejects_future_date_only_human_correction_without_type_error(
    tmp_path,
):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["human_corrections"] = [{
        "at": "2999-01-01Z",
        "summary": "future correction",
        "evidence_id": "correction-1",
    }]
    receipt_path.write_text(json.dumps(receipt))

    result = run_cli(
        tmp_path,
        "checkpoint",
        "set",
        "--run-dir",
        str(run_dir),
        "--current-slice",
        "draft",
        "--next-action",
        "continue",
    )

    assert result.returncode == 1
    assert "human_corrections[0].at exceeds the future timestamp tolerance" in result.stderr
    assert "TypeError" not in result.stderr
