"""Focused checks for provider admission and its visible waiting state."""

from skills.orchestrate.scripts import memory_admission
from skills.orchestrate.scripts.fabric_records import render_digest


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
    assert memory_admission.admit(reasons.append, lambda: False, warnings.append,
                                  probe=lambda: next(readings), pause=lambda _: None)
    assert reasons == ["waiting for memory: 812 MB available, floor 1024 MB"]
    assert warnings == []
    assert not memory_admission._waiting


def test_probe_failure_admits_with_warning(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    warnings = []
    def broken_probe():
        raise OSError("unavailable")
    assert memory_admission.admit(lambda _: None, lambda: False, warnings.append,
                                  probe=broken_probe)
    assert warnings == ["memory probe failed: unavailable"]


def test_zero_floor_skips_the_probe(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "0")
    def fail_probe():
        raise AssertionError("probe called")
    assert memory_admission.admit(lambda _: None, lambda: False, lambda _: None,
                                  probe=fail_probe)


def test_cancel_removes_waiting_attempt(monkeypatch):
    monkeypatch.setenv("FABRIC_MEMORY_FLOOR_MB", "1024")
    cancelled = [False]
    def waiting(_):
        cancelled[0] = True
    assert not memory_admission.admit(waiting, lambda: cancelled[0], lambda _: None,
                                      probe=lambda: 812)
    assert not memory_admission._waiting


def test_queued_digest_shows_current_numbers():
    row = {"run_id": "mcp-example", "state": "queued", "attempt": 1,
           "provenance": {"requested": {"adapter": "codex"}, "resolved_model": "gpt-6-sol"},
           "reason": "waiting for memory: 812 MB available, floor 1024 MB"}
    assert render_digest(row) == (
        "queued mcp-example codex/gpt-6-sol · waiting for memory: 812 MB available, floor 1024 MB"
    )
