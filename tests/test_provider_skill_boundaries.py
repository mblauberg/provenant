from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def test_agy_is_a_fabric_adapter_not_a_parallel_provider_skill():
    assert not (ROOT / "skills/agy-headless").exists()
    spec = read("docs/specs/agent-fabric/provider-actions-and-adapters.md")
    orchestrate = read("skills/orchestrate/SKILL.md")
    assert "Agy | Gemini or Antigravity access | Adapter only; no separate provider skill" in spec
    assert "Answer-bearing external work uses Fabric request/reply" in orchestrate


def test_headless_helpers_cannot_bypass_fabric_for_agy():
    dispatcher = read("skills/orchestrate/scripts/cf_dispatch.sh")
    assert not (ROOT / "skills/autopilot/scripts/cross-family.sh").exists()
    for forbidden in (
        "CF_DISPATCH_ENABLE_AGY",
        "agy_cmd=(",
        "run-agy-headless",
    ):
        assert forbidden not in dispatcher


def test_autopilot_routes_bonus_gemini_through_fabric():
    reference = read("skills/autopilot/references/cross-family-review.md")
    assert "All answer-bearing cross-family work goes through Agent Fabric" in reference
    assert "agy-headless" not in reference
    assert "direct `agy`" not in reference


def test_direct_agy_dispatch_keeps_the_compatibility_contract():
    compatibility = read("config/adapter-compatibility.yaml")
    assert "  agy:" in compatibility
