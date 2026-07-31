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


def test_transition_to_observing_requires_active_or_passing_observation(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["status"] = "awaiting_release"
    receipt["human_gates"]["release"] = {
        "status": "approved",
        "approver": "human-owner",
        "evidence": "release-approval",
    }
    receipt["evidence"].append({
        "id": "release-approval",
        "kind": "human",
        "gate": "human-release",
        "status": "pass",
    })
    assert receipt["observation"]["status"] == "planned"
    receipt_path.write_text(json.dumps(receipt))

    result = run_cli(
        tmp_path,
        "transition",
        "--run-dir",
        str(run_dir),
        "--to",
        "observing",
        "--evidence",
        "release-approval",
    )

    assert result.returncode == 1
    assert "observation status active or pass" in result.stderr


def test_transition_to_closed_clears_checkpoint_in_flight(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    observing_at = timestamp(timedelta(seconds=-3))
    observed_at = timestamp(timedelta(seconds=-2))
    receipt["status"] = "observing"
    receipt["state_history"].append({
        "state": "observing",
        "at": observing_at,
        "evidence_ids": [],
        "risk_tier": receipt["risk_tier"],
    })
    receipt["observation"].update({
        "status": "pass",
        "started_at": observing_at,
        "ended_at": observed_at,
        "observed_events": 1,
        "evidence_ids": ["observation-1"],
    })
    receipt["evidence"].append({
        "id": "observation-1",
        "kind": "observation",
        "gate": "citation-audit",
        "status": "pass",
        "observed_at": observed_at,
        "measured_value": 1,
    })
    receipt["checkpoint"]["in_flight"] = ["observation-monitor"]
    receipt_path.write_text(json.dumps(receipt))

    result = run_cli(
        tmp_path,
        "transition",
        "--run-dir",
        str(run_dir),
        "--to",
        "closed",
        "--evidence",
        "observation-1",
    )

    assert result.returncode == 0, result.stderr
    closed = json.loads(receipt_path.read_text())
    assert closed["checkpoint"]["in_flight"] == []


def test_bind_refuses_to_replace_observation_after_its_lifecycle_gate(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["status"] = "observing"
    receipt["state_history"].append({
        "state": "observing",
        "at": timestamp(timedelta(seconds=-1)),
        "evidence_ids": [],
        "risk_tier": receipt["risk_tier"],
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
    assert "observation lifecycle gate has passed" in result.stderr
    assert json.loads(receipt_path.read_text())["observation"] == original


def test_rebuild_refuses_to_mutate_a_referenced_sibling_digest(tmp_path):
    support = helpers()
    run_dir = support.initialise(tmp_path)
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
        sys.executable,
        "-c",
        "pass",
    ).returncode == 0
    receipt_path = run_dir / "RUN.json"
    receipt = json.loads(receipt_path.read_text())
    row = next(item for item in receipt["evidence"] if item["id"] == "tests")
    original_digest = row["result"]["receipt_digest"]
    row["method"] = "changed after citation"
    receipt["state_history"][0]["evidence_ids"] = ["tests"]
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
    assert "tests is referenced by state_history[0]" in result.stderr
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
    bundle_path = run_dir / "evidence.json"
    bundle_path.write_text(
        '{"schema_version":1,"contract":"deterministic-evidence-bundle","checks":[]}\n'
    )
    assert support.add_artifact(
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
        f"from pathlib import Path; Path({str(approval)!r}).write_text('changed')",
    )

    assert result.returncode == 1
    assert "risk override artifact digest does not match live bytes" in result.stderr
    receipt = json.loads((run_dir / "RUN.json").read_text())
    assert not any(item["id"] == "tests" for item in receipt["evidence"])


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


def test_bounded_execution_preserves_success_while_cleaning_descendants(tmp_path):
    module = producer()
    child = (
        "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        "time.sleep(1)"
    )
    leader = (
        "import subprocess,sys,time; "
        f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
        "time.sleep(0.02)"
    )

    exit_code, _stdout, _stderr = module.execute_bounded(
        [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=0.08,
    )

    assert exit_code == 0


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
            [sys.executable, "-c", leader], cwd=tmp_path, timeout_seconds=0.08,
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
    receipt["status"] = "closed"
    receipt["state_history"].extend([
        {
            "state": "observing",
            "at": timestamp(timedelta(seconds=-2)),
            "evidence_ids": [],
            "risk_tier": receipt["risk_tier"],
        },
        {
            "state": "closed",
            "at": timestamp(timedelta(seconds=-1)),
            "evidence_ids": [],
            "risk_tier": receipt["risk_tier"],
        },
    ])
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
    assert "observation lifecycle gate has passed" in result.stderr
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
