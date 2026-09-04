"""Characterisation of the run-state invariants shared by `deliver` and `implement`.

`ensure_immutable_risk` and `ensure_run_open` are the risk-policy and
closed-run invariants that both the delivery receipt producer and the
`implement` checkpoint writer must enforce identically. This file pins their
observable behaviour -- which input raises, with which type and which message --
through *both* call paths, so a relocation of the definitions can be shown to
preserve behaviour rather than asserted to (#755).

Nothing here reaches for the module that happens to define the invariants. The
producer is exercised through its own public names and the checkpoint writer
only through `update()`, so the tests survive a move of the definitions.
"""

from __future__ import annotations

import copy
import importlib.util
import json
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PRODUCER = ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt.py"
CHECKPOINT = ROOT / "skills" / "implement" / "scripts" / "checkpoint_run.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


@pytest.fixture(scope="module")
def producer():
    """The producer under the name a real process imports it by.

    Registering it in `sys.modules` under its canonical name is what makes the
    identity assertion below a statement about the product rather than about
    this file: loading the same source twice under two private names would
    manufacture two `ReceiptError` classes no matter how the product is
    arranged.
    """
    module = load(PRODUCER, "delivery_receipt")
    previous = sys.modules.get("delivery_receipt")
    sys.modules["delivery_receipt"] = module
    yield module
    if previous is None:
        sys.modules.pop("delivery_receipt", None)
    else:
        sys.modules["delivery_receipt"] = previous


@pytest.fixture(scope="module")
def checkpoint(producer):
    return load(CHECKPOINT, "checkpoint_run_for_invariants")


ROUTINE_ASSESSMENT = {
    "blast_radius": "local",
    "reversibility": "easy",
    "data_sensitivity": "public",
    "migration": "none",
    "oracle_quality": "strong",
    "external_effects": "none",
    "critical_surface": "none",
}


def base_run() -> dict:
    return {
        "schema_version": 1,
        "contract": "delivery-run",
        "run_id": "run-characterisation",
        "risk_tier": "routine",
        "initial_risk_tier": "routine",
        "risk_assessment": copy.deepcopy(ROUTINE_ASSESSMENT),
        "human_corrections": [],
        "authority": {"allowed_artifact_paths": ["."], "allowed_source_paths": ["."]},
        "artifacts": [],
        "evidence": [],
        "human_gates": {},
        "observation": {},
        "checkpoint": {
            "generation": 0,
            "current_slice": "",
            "next_action": "",
            "in_flight": [],
            "artifact_paths": [],
        },
    }


def crucial_assessment() -> dict:
    assessment = copy.deepcopy(ROUTINE_ASSESSMENT)
    assessment["critical_surface"] = "auth-security"
    return assessment


# Each case mutates the base run in place and names the message the invariant
# must refuse it with. `None` means the invariant must accept the run.
def _tier_not_a_tier(run, _workspace):
    run["initial_risk_tier"] = "spicy"


def _tier_moved_after_init(run, _workspace):
    run["risk_tier"] = "substantial"


def _downgrade_without_override(run, _workspace):
    run["risk_assessment"] = crucial_assessment()


def _downgrade_with_incomplete_override(run, _workspace):
    run["risk_assessment"] = crucial_assessment()
    run["risk_override"] = {"status": "approved", "approved_by": "user"}


def _approved_override_without_evidence(run, _workspace):
    run["risk_override"] = {
        "status": "approved", "approved_by": "user",
        "evidence": "ev-1", "reason": "adjudicated",
    }


def _approved_override_without_artifact(run, _workspace):
    _approved_override_without_evidence(run, _workspace)
    run["evidence"] = [{
        "id": "ev-1", "kind": "human", "status": "pass",
        "gate": "risk-override", "artifact_id": "art-1",
    }]


def _approved_override_with_stale_digest(run, workspace):
    _approved_override_without_artifact(run, workspace)
    target = workspace / "override.md"
    target.write_text("live bytes\n")
    run["artifacts"] = [{
        "id": "art-1", "path": "override.md", "class": "canonical",
        "digest": "sha256:" + "0" * 64,
    }]


def _approved_override_artifact_missing_from_disk(run, workspace):
    _approved_override_without_artifact(run, workspace)
    run["artifacts"] = [{
        "id": "art-1", "path": "override.md", "class": "canonical",
        "digest": "sha256:" + "0" * 64,
    }]


def _approved_override_artifact_escapes_workspace(run, workspace):
    _approved_override_without_artifact(run, workspace)
    run["artifacts"] = [{
        "id": "art-1", "path": "../escape.md", "class": "canonical",
        "digest": "sha256:" + "0" * 64,
    }]


def _approved_override_artifact_outside_authority(run, workspace):
    _approved_override_with_stale_digest(run, workspace)
    run["authority"]["allowed_artifact_paths"] = ["docs"]


def _corrections_not_a_list(run, _workspace):
    run["human_corrections"] = {}


def _correction_not_an_object(run, _workspace):
    run["human_corrections"] = ["yesterday"]


def _correction_in_the_future(run, _workspace):
    ahead = datetime.now(UTC) + timedelta(hours=2)
    run["human_corrections"] = [{"at": ahead.strftime("%Y-%m-%dT%H:%M:%SZ")}]


def _assessment_missing_a_factor(run, _workspace):
    run["risk_assessment"].pop("migration")


def _assessment_factor_invalid(run, _workspace):
    run["risk_assessment"]["blast_radius"] = "galactic"


def _accepted(run, _workspace):
    return None


def _closed_run(run, _workspace):
    run["human_gates"] = {"release": {"status": "approved"}}
    run["observation"] = {"status": "pass"}


IMMUTABLE_RISK_CASES = [
    (_tier_not_a_tier, "risk tier is immutable after init"),
    (_tier_moved_after_init, "risk tier is immutable after init"),
    (_downgrade_without_override, "approved human risk override is missing"),
    (
        _downgrade_with_incomplete_override,
        "risk tier below derived crucial requires an approved human override",
    ),
    (_approved_override_without_evidence, "approved human risk override evidence is missing"),
    (_approved_override_without_artifact, "approved human risk override artifact is missing"),
    (
        _approved_override_with_stale_digest,
        "risk override artifact digest does not match live bytes",
    ),
    (
        _approved_override_artifact_missing_from_disk,
        "risk override artifact digest does not match live bytes",
    ),
    (
        _approved_override_artifact_escapes_workspace,
        "risk override artifact must be safe and workspace-relative",
    ),
    (
        _approved_override_artifact_outside_authority,
        "artifact path leaves authority.allowed_artifact_paths",
    ),
    (_corrections_not_a_list, "human_corrections must be a list"),
    (_correction_not_an_object, "human_corrections[0] must be an object"),
    (
        _correction_in_the_future,
        "human_corrections[0].at exceeds the future timestamp tolerance",
    ),
    (_assessment_missing_a_factor, "risk-assessment must cover every policy factor"),
    (_assessment_factor_invalid, "risk-assessment.blast_radius is invalid"),
    (_accepted, None),
]

CASE_IDS = [case.__name__.lstrip("_") for case, _ in IMMUTABLE_RISK_CASES]


@pytest.mark.parametrize(("mutate", "message"), IMMUTABLE_RISK_CASES, ids=CASE_IDS)
def test_ensure_immutable_risk_through_the_producer(producer, tmp_path, mutate, message):
    run = base_run()
    mutate(run, tmp_path)
    if message is None:
        assert producer.ensure_immutable_risk(run, tmp_path) is None
        return
    with pytest.raises(producer.ReceiptError) as raised:
        producer.ensure_immutable_risk(run, tmp_path)
    assert str(raised.value) == message
    # The refusal stays a ValueError so existing `except ValueError` callers
    # keep catching it.
    assert isinstance(raised.value, ValueError)


def test_ensure_run_open_accepts_an_open_run(producer):
    assert producer.ensure_run_open(base_run()) is None


def test_ensure_run_open_refuses_a_closed_run(producer):
    run = base_run()
    _closed_run(run, None)
    with pytest.raises(producer.ReceiptError) as raised:
        producer.ensure_run_open(run)
    assert str(raised.value) == "closed run is immutable"


def write_run(workspace: Path, run: dict) -> Path:
    run_dir = workspace / ".agent-run" / "run-characterisation"
    run_dir.mkdir(parents=True, exist_ok=True)
    path = run_dir / "RUN.json"
    path.write_text(json.dumps(run, indent=2) + "\n")
    return path


def checkpoint_update(checkpoint, path: Path):
    return checkpoint.update(path, "slice", "next", [], [])


# The same invariants must refuse the same runs when the `implement` checkpoint
# writer is the caller. Only `update()` is used, so this pins the behaviour of
# the call path and not of any particular import arrangement behind it.
@pytest.mark.parametrize(("mutate", "message"), IMMUTABLE_RISK_CASES, ids=CASE_IDS)
def test_ensure_immutable_risk_through_the_checkpoint_writer(
    checkpoint, tmp_path, mutate, message,
):
    run = base_run()
    mutate(run, tmp_path)
    path = write_run(tmp_path, run)
    if message is None:
        assert checkpoint_update(checkpoint, path)["verified"] is True
        return
    with pytest.raises(ValueError) as raised:
        checkpoint_update(checkpoint, path)
    assert str(raised.value) == message


def test_the_checkpoint_writer_refuses_a_closed_run(checkpoint, tmp_path):
    run = base_run()
    _closed_run(run, None)
    path = write_run(tmp_path, run)
    with pytest.raises(ValueError) as raised:
        checkpoint_update(checkpoint, path)
    assert str(raised.value) == "closed run is immutable"


def test_both_call_paths_raise_the_same_refusal_class(producer, checkpoint, tmp_path):
    """One `ReceiptError` class object, not two that merely share a name.

    A second class would let `except ReceiptError` in one caller silently stop
    catching the other caller's refusal, so this asserts identity rather than
    name equality.
    """
    run = base_run()
    _tier_moved_after_init(run, tmp_path)
    path = write_run(tmp_path, run)

    with pytest.raises(ValueError) as from_checkpoint:
        checkpoint_update(checkpoint, path)
    with pytest.raises(ValueError) as from_producer:
        producer.ensure_immutable_risk(run, tmp_path)

    assert type(from_checkpoint.value) is type(from_producer.value)
    assert type(from_checkpoint.value) is producer.ReceiptError
    assert isinstance(from_checkpoint.value, producer.ReceiptError)


# Regression guard, added after the fact rather than characterised up front.
# The first relocation computed the risk policy path once at module load. That
# is safe while the invariants live inside a script the caller reloads, and
# unsafe once they live in a shared module that stays cached in `sys.modules`
# for the life of the process: the first importer's product root would be
# frozen in for every later caller. `test_product_root_single_resolver` caught
# it. This pins the shared module directly, which that test does not reach.
def test_the_risk_policy_is_resolved_against_the_current_product_root(
    monkeypatch, tmp_path,
):
    from _shared import delivery_run_invariants as invariants

    policy = json.loads((ROOT / "config" / "risk-policy.json").read_text())
    configured = tmp_path / "config"
    configured.mkdir()
    (configured / "risk-policy.json").write_text(json.dumps(policy))

    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(tmp_path))

    assert invariants.risk_policy_path() == configured / "risk-policy.json"
    assert invariants.load_risk_policy() == policy

    # And a configured root with no policy must refuse, not silently fall back
    # to the one the module was first imported under.
    empty = tmp_path / "empty"
    empty.mkdir()
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(empty))
    with pytest.raises(invariants.ReceiptError) as raised:
        invariants.load_risk_policy()
    assert str(raised.value).startswith("risk policy is unreadable")
