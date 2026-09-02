from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def test_agy_has_no_parallel_provider_skill():
    assert not (ROOT / "skills/agy-headless").exists()


def test_headless_dispatcher_uses_direct_router_without_daemon_gate():
    dispatcher = read("skills/orchestrate/scripts/cf_dispatch.sh")
    assert not (ROOT / "skills/autopilot/scripts/cross-family.sh").exists()
    assert "--adapter-gate" not in dispatcher
    # The point of this guard is that agy is reached the same way every other
    # provider is: one adapter inside the dispatcher, never a parallel
    # provider skill and never an opt-in gate that would make the only route
    # to the Gemini family second-class. Agy has its own seat for stable
    # addressing, while the dispatcher branch is the provider call and Fabric
    # carries coordination.
    for forbidden in (
        "CF_DISPATCH_ENABLE_AGY",
        "run-agy-headless",
    ):
        assert forbidden not in dispatcher
    assert "agy)" in dispatcher, "agy must be dispatchable like every other adapter"


def test_direct_agy_dispatch_keeps_the_compatibility_contract():
    compatibility = read("config/adapter-compatibility.yaml")
    assert "  agy:" in compatibility
