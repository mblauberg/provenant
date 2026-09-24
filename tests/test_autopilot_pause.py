import importlib.util
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "autopilot" / "scripts" / "validate_idle_pause.py"

CLEAN_QUEUE = """# QUEUE

## Tier 0 — foundational one-way-doors

| id | status | depends-on | lease-owner | lease-expiry | notes |
|----|--------|------------|-------------|---------------|-------|
"""


def load_module():
    spec = importlib.util.spec_from_file_location("validate_idle_pause", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_clean_queue(tmp_path, text=CLEAN_QUEUE):
    queue = tmp_path / "QUEUE.md"
    queue.write_text(text)
    return queue


def base_state(
    resume="restart-on: human-directive",
    lease="release-on-driver-exit",
    in_flight="(none)",
    next_up="(none — dry after bounded re-enumeration)",
    status="PAUSED — reason: idle-frontier",
):
    return f"""# STATE
- **Run status:** {status}
- **Conductor lease:** {lease}
- **In flight:** {in_flight}
- **Next up:** {next_up}
- **Resume protocol:** {resume}
"""


def test_template_pause_requires_dry_frontier_resume_trigger_and_lease_release(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state(resume="restart-on: human-directive, external-completion"))
    write_clean_queue(tmp_path)

    assert load_module().validate(state) == []


def test_pause_with_in_flight_work_cannot_stop_driver(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state(in_flight="run-17 still executing"))
    write_clean_queue(tmp_path)

    assert any("in-flight" in error for error in load_module().validate(state))


def test_fallback_section_format_uses_the_same_pause_contract(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text("""# STATE
## Heartbeat

- **Run status:** PAUSED — reason: idle-frontier
- **In flight:** (none)
- **Next up:** (none — dry after bounded re-enumeration)
CONDUCTOR LEASE: release-on-driver-exit
RESUME PROTOCOL: restart-on: gate-answer, external-completion
""")
    write_clean_queue(tmp_path)

    assert load_module().validate(state) == []


def test_pause_without_external_resume_trigger_is_rejected(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state(resume="wait and see", lease="released"))
    write_clean_queue(tmp_path)

    assert any("external resume" in error for error in load_module().validate(state))


def test_pause_rejects_negated_resume_trigger(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(
        base_state(resume="do not restart after any human directive.", lease="released")
    )
    write_clean_queue(tmp_path)

    assert any("external resume" in error for error in load_module().validate(state))


def test_pause_rejects_negative_synonym_in_resume_prose(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(
        base_state(resume="restart after refusing every human directive.", lease="released")
    )
    write_clean_queue(tmp_path)

    assert any("external resume" in error for error in load_module().validate(state))


@pytest.mark.parametrize(
    "resume",
    (
        "restart-on: human-directive, human-directive",
        "restart-on: human-directive, timer-expiry",
        "restart-on: human-directive after checking state",
        "restart-on: human-directive.",
    ),
)
def test_pause_rejects_noncanonical_structured_resume_values(tmp_path, resume):
    state = tmp_path / "STATE.md"
    state.write_text(base_state(resume=resume, lease="released"))
    write_clean_queue(tmp_path)

    assert any("external resume" in error for error in load_module().validate(state))


def test_pause_rejects_negated_release_and_dry_sentinels(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(
        base_state(lease="not released", next_up="not dry: task-17 remains")
    )
    write_clean_queue(tmp_path)

    errors = load_module().validate(state)
    assert any("release-on-driver-exit" in error for error in errors)
    assert any("frontier" in error for error in errors)


def test_pause_rejects_negated_status(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state(status="NOT PAUSED — reason: idle-frontier"))
    write_clean_queue(tmp_path)

    assert any("PAUSED" in error for error in load_module().validate(state))


def test_pause_rejects_pending_queue_rows(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state())
    write_clean_queue(
        tmp_path,
        CLEAN_QUEUE
        + "| W001 | PENDING | none | - | - | Select the next implementation slice. |\n",
    )

    assert any("PENDING" in error or "LEASED" in error for error in load_module().validate(state))


def test_pause_rejects_leased_queue_rows(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state())
    write_clean_queue(
        tmp_path,
        CLEAN_QUEUE
        + "| W001 | LEASED | none | agent-a | 2026-07-15T00:00:00Z | in flight |\n",
    )

    assert any("PENDING" in error or "LEASED" in error for error in load_module().validate(state))


def test_pause_fails_closed_on_unrecognized_queue_status(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state())
    write_clean_queue(
        tmp_path,
        CLEAN_QUEUE
        + "| W001 | ACTIVE | none | - | - | typo or ad-hoc status must not read as idle. |\n",
    )

    assert any("status" in error.lower() for error in load_module().validate(state))


def test_pause_fails_closed_on_malformed_queue_row(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state())
    write_clean_queue(
        tmp_path,
        CLEAN_QUEUE + "| W001 | DONE | none | missing-cells |\n",
    )

    assert any("queue" in error.lower() for error in load_module().validate(state))


def test_pause_accepts_closed_queue_rows_with_escaped_pipes(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state(resume="restart-on: material-change, explicit-restart"))
    write_clean_queue(
        tmp_path,
        CLEAN_QUEUE + r"| W001 | DEFERRED | none | - | - | Retained A \| B evidence. |" + "\n",
    )

    assert load_module().validate(state) == []


def test_pause_requires_the_canonical_queue_tier_section(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state())
    write_clean_queue(tmp_path, "# QUEUE\n\nno tier heading here\n")

    assert any("queue" in error.lower() for error in load_module().validate(state))


def test_pause_requires_the_canonical_queue_file(tmp_path):
    state = tmp_path / "STATE.md"
    state.write_text(base_state())
    # No QUEUE.md written at all.

    assert any("queue" in error.lower() for error in load_module().validate(state))
