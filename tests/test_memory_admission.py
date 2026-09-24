"""Focused checks for provider admission and its visible waiting state."""

import threading
import time
import pytest

from skills.orchestrate.scripts import memory_admission
from skills.orchestrate.scripts.fabric_records import render_digest


@pytest.fixture(autouse=True)
def admission_state_home(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))


def test_vm_stat_available_pages_use_reported_page_size():
    sample = """Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free: 1024.
Pages active: 999.
Pages inactive: 32,768.
Pages speculative: 32,768.
"""
    assert memory_admission.parse_vm_stat(sample) == 1040


def test_linux_memavailable_uses_kibibytes():
    assert memory_admission.parse_meminfo("MemTotal: 999 kB\nMemAvailable: 1048576 kB\n") == 1024


def test_low_memory_waits_then_admits(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    monkeypatch.setattr(memory_admission, "POLL_SECONDS", 0)
    readings = iter([812, 1300])
    reasons = []
    warnings = []
    lease = memory_admission.admit(reasons.append, lambda: False, warnings.append,
                                   probe=lambda: next(readings), pause=lambda _: None)
    assert lease is not None
    lease.close()
    assert len(reasons) == 1 and reasons[0].startswith("waiting for memory: 812 MB available, floor 1024 MB;")
    assert "elapsed" in reasons[0] and "remaining" in reasons[0]
    assert warnings == []


def test_every_low_probe_publishes_fresh_reason(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    monkeypatch.setattr(memory_admission, "POLL_SECONDS", 0)
    readings = iter([812, 900, 1300])
    reasons = []
    lease = memory_admission.admit(reasons.append, lambda: False, lambda _: None,
                                   probe=lambda: next(readings), pause=lambda _: None)
    assert lease is not None
    lease.close()
    assert len(reasons) == 2
    assert reasons[0].startswith("waiting for memory: 812 MB available, floor 1024 MB;")
    assert reasons[1].startswith("waiting for memory: 900 MB available, floor 1024 MB;")


def test_separate_attempts_publish_their_first_wait_independently(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    first_wait = threading.Event()
    second_wait = threading.Event()
    cancelled = threading.Event()
    def owner(waited):
        lease = memory_admission.admit(lambda _: waited.set(), cancelled.is_set, lambda _: None,
                                       probe=lambda: 812)
        if lease is not None:
            lease.close()
    first = threading.Thread(target=owner, args=(first_wait,))
    second = threading.Thread(target=owner, args=(second_wait,))
    first.start()
    try:
        assert first_wait.wait(1)
        second.start()
        assert second_wait.wait(0.5)
    finally:
        cancelled.set()
        first.join(1)
        if second.ident is not None:
            second.join(1)
    assert not first.is_alive() and not second.is_alive()


def test_probe_failure_admits_with_warning(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    warnings = []
    def broken_probe():
        raise OSError("unavailable")
    lease = memory_admission.admit(lambda _: None, lambda: False, warnings.append,
                                   probe=broken_probe)
    assert lease is not None
    lease.close()
    assert warnings == ["memory probe failed: unavailable"]


def test_zero_floor_skips_the_probe(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "0")
    def fail_probe():
        raise AssertionError("probe called")
    lease = memory_admission.admit(lambda _: None, lambda: False, lambda _: None,
                                   probe=fail_probe)
    assert lease is not None
    lease.close()


def test_cancel_stops_waiting_attempt(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    cancelled = [False]
    def waiting(_):
        cancelled[0] = True
    assert memory_admission.admit(waiting, lambda: cancelled[0], lambda _: None,
                                  probe=lambda: 812) is None


def test_queued_digest_shows_current_numbers():
    row = {"run_id": "mcp-example", "state": "queued", "attempt": 1,
           "provenance": {"requested": {"adapter": "codex"}, "resolved_model": "gpt-6-sol"},
           "reason": "waiting for memory: 812 MB available, floor 1024 MB"}
    assert render_digest(row) == (
        "queued mcp-example codex/gpt-6-sol · waiting for memory: 812 MB available, floor 1024 MB"
    )


def test_lock_serializes_owners_through_provider_settle(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    monkeypatch.setattr(memory_admission, "SETTLE_SECONDS", 1)
    first_reserved = threading.Event()
    allow_start = threading.Event()
    first_started = threading.Event()
    second_waiting = threading.Event()
    second_low = threading.Event()
    memory_consumed = threading.Event()
    cancelled = threading.Event()
    admitted = []

    def probe():
        return 800 if memory_consumed.is_set() else 1300

    def owner_first():
        lease = memory_admission.admit(lambda _: None, cancelled.is_set, lambda _: None, probe=probe)
        admitted.append("first")
        first_reserved.set()
        allow_start.wait(1)
        lease.started()
        first_started.set()
        time.sleep(1.1)
        lease.close()

    def owner_second():
        def waiting(reason):
            second_waiting.set()
            if "800 MB available" in reason:
                second_low.set()
        lease = memory_admission.admit(waiting, cancelled.is_set,
                                       lambda _: None, probe=probe)
        if lease is not None:
            admitted.append("second")
            lease.close()

    first = threading.Thread(target=owner_first)
    second = threading.Thread(target=owner_second)
    first.start()
    assert first_reserved.wait(1)
    second.start()
    try:
        assert second_waiting.wait(1)
        assert admitted == ["first"]
        allow_start.set()
        assert first_started.wait(1)
        time.sleep(0.2)
        assert admitted == ["first"]
        memory_consumed.set()
        assert second_low.wait(2)
        assert admitted == ["first"]
    finally:
        allow_start.set()
        cancelled.set()
        first.join(2)
        second.join(1)
    assert not first.is_alive() and not second.is_alive()


def test_lock_creation_failure_admits_with_warning(tmp_path, monkeypatch):
    blocked = tmp_path / "not-a-directory"
    blocked.write_text("x")
    monkeypatch.setenv("XDG_STATE_HOME", str(blocked))
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    warnings = []
    lease = memory_admission.admit(lambda _: None, lambda: False, warnings.append,
                                   probe=lambda: 1300)
    assert lease is not None
    lease.close()
    assert len(warnings) == 1 and warnings[0].startswith("memory admission lock unavailable:")


def test_wait_expiry_and_visible_budget(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    monkeypatch.setenv("FABRIC_MEMORY_WAIT_SECONDS", "0.02")
    monkeypatch.setattr(memory_admission, "POLL_SECONDS", 0.01)
    reasons = []
    try:
        memory_admission.admit(reasons.append, lambda: False, lambda _: None,
                               probe=lambda: 812)
    except memory_admission.MemoryUnavailableError as exc:
        assert exc.code == "memory_unavailable"
    else:
        assert False, "wait should expire"
    assert reasons
    assert all("elapsed" in reason and "remaining" in reason for reason in reasons)


def test_recovered_memory_after_wait_deadline_does_not_admit(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    monkeypatch.setenv("FABRIC_MEMORY_WAIT_SECONDS", "0.01")
    monkeypatch.setattr(memory_admission, "POLL_SECONDS", 0.01)
    readings = iter([812, 1300])
    with pytest.raises(memory_admission.MemoryUnavailableError):
        memory_admission.admit(lambda _: None, lambda: False, lambda _: None,
                               probe=lambda: next(readings), pause=lambda _: time.sleep(0.02))


def test_prior_wait_consumes_the_same_attempt_budget(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    monkeypatch.setenv("FABRIC_MEMORY_WAIT_SECONDS", "0.01")
    with pytest.raises(memory_admission.MemoryUnavailableError):
        memory_admission.admit(lambda _: None, lambda: False, lambda _: None,
                               probe=lambda: 812, waited_seconds=0.01)


def test_slow_recovery_probe_cannot_overrun_remaining_budget(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    monkeypatch.setenv("FABRIC_MEMORY_WAIT_SECONDS", "0.01")
    def slow_probe():
        time.sleep(0.02)
        return 1300
    with pytest.raises(memory_admission.MemoryUnavailableError):
        memory_admission.admit(lambda _: None, lambda: False, lambda _: None,
                               probe=slow_probe, waited_seconds=0.005)
