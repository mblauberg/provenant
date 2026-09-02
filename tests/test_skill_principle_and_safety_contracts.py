from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def text(path: str) -> str:
    return (ROOT / path).read_text()


def squash(value: str) -> str:
    return " ".join(value.split())


def test_solid_is_a_probe_not_a_standalone_review_finding():
    lenses = squash(text("skills/code-review/references/review-lenses.md"))
    for principle in (
        "Single responsibility",
        "Open/closed",
        "Liskov substitution",
        "Interface segregation",
        "Dependency inversion",
    ):
        assert principle in lenses
    assert "generate hypotheses, never as standalone" in lenses
    assert "evidence ties the principle to a present defect" in lenses
    assert "both the delta and the unchanged enforcement path" in lenses
    assert "delegated permission inheritance" in lenses


def test_refactor_preserves_behaviour_and_rejects_acronym_compliance():
    skill = squash(text("skills/refactor/SKILL.md"))
    assert "behaviour-preserving" in skill
    assert "Use SOLID and related principles as probes, not success metrics" in skill
    assert "acronym compliance" in skill
    assert "Changed behaviour is a separate TDD slice" in skill
    assert "Unknown, user-owned and unrelated files stay untouched" in skill


def test_tdd_and_autopilot_cleanup_never_delete_unknown_or_preexisting_work():
    tdd = text("skills/tdd/SKILL.md")
    skill = text("skills/autopilot/SKILL.md")
    recovery = text("skills/autopilot/references/recovery-and-cadence.md")
    combined = squash("\n".join((tdd, skill, recovery))).lower()
    assert "delete it and reconstruct" not in combined
    assert "delete cruft" not in combined
    assert "never delete unknown files" in combined
    assert "unknown or user-owned material stays untouched" in combined


def test_caveman_preserves_evidence_altitude_and_never_grants_authority():
    skill = text("skills/caveman/SKILL.md")
    assert "presentation overlay, not authority" in skill
    assert "ordinary governing preference for terse output does not trigger" in skill
    assert "Full and ultra remain user-explicit" in skill
    assert "evidence relationships" in skill
    assert "Never imply that an unverified source confirms a claim" in skill
    assert "Suspend compression" in skill


def test_implementation_grounds_version_sensitive_interfaces_without_overriding_local_policy():
    skill = squash(text("skills/implement/SKILL.md"))
    grounding = squash(text("skills/implement/references/source-grounding.md"))
    assert "version-sensitive external interface" in skill
    assert "installed or locked version" in grounding
    assert "primary source" in grounding
    assert "unverified" in grounding
    assert "repository convention" in grounding
    assert "every ordinary code line" in grounding


def test_migrations_preserve_mixed_version_safety_and_expire_compatibility_paths():
    skill = squash(text("skills/implement/SKILL.md"))
    migration = squash(text("skills/implement/references/migration-compatibility.md"))
    for phrase in (
        "mixed-version window",
        "expand, migrate, contract",
        "usage-zero evidence",
        "expiry owner",
        "containment",
    ):
        assert phrase in f"{skill} {migration}"
    assert "Every migration needs a down migration" not in migration
