from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"


def _text(name: str) -> str:
    return (SKILL / "references" / name).read_text().lower()


def _has_all(text: str, *concepts: str) -> None:
    missing = [concept for concept in concepts if concept.lower() not in text]
    assert not missing, missing


def test_review_contract_keeps_evidence_coverage_and_certification_boundaries():
    review = _text("review.md")
    qa = _text("visual-qa.md")

    _has_all(
        review,
        "verified",
        "judgement",
        "tested",
        "failed",
        "not tested",
        "not applicable",
        "not reviewed",
        "not verified",
        "wcag certification",
    )
    _has_all(review, "observation", "divergence", "remedy", "pass", "minor", "major")
    _has_all(review, "navigation", "get", "screenshot", "submission", "outside the protected")
    _has_all(qa, "field performance", "screenshot", "does not prove")
    _has_all(
        review,
        "node <skill-root>/scripts/detect.mjs",
        "file",
        "directory",
        "url",
        "incomplete",
        "supporting evidence",
    )


def test_design_and_system_contracts_are_actionable_without_questionnaire_ceremony():
    design = _text("design.md")
    systems = _text("design-systems.md")

    _has_all(design, "optional brief", "batch", "assumption", "goal", "states", "interaction", "content")
    assert "four approval" not in design
    _has_all(systems, "primitive", "semantic", "component")
    _has_all(systems, "document", "extract", "frontmatter", "tokens", "components", "do not overwrite")
    _has_all(systems, "sidecar", "scan", "seed", "incremental")
    _has_all(
        systems,
        "overview",
        "colors",
        "typography",
        "elevation",
        "do's and don'ts",
        "token references",
        "{path.to.token}",
        "schemaVersion",
        "extensions",
        "narrative",
        "round-trip",
        "no-overwrite",
    )


def test_interaction_and_motion_contracts_cover_recovery_and_high_frequency_use():
    states = _text("interaction-states.md")
    motion = _text("motion.md")

    _has_all(
        states,
        "event",
        "guard",
        "feedback",
        "recovery",
        "rollback",
        "cancellation",
        "aria-invalid",
        "aria-describedby",
        "layout shift",
    )
    _has_all(states, "retention", "expiry", "restoration", "concurrent", "failure")
    _has_all(
        states,
        "built-in invoker",
        "focus",
        "dismissal",
        "accessible name",
        "chosen type",
        "background",
    )
    assert "not accessible by default" not in states
    _has_all(motion, "high-frequency", "keyboard", "retarget", "tooltip", "stagger", "reduced-motion")
    assert "no universal duration" in motion


def test_content_responsive_data_and_visual_qa_contracts_keep_load_bearing_depth():
    content = _text("content-conversion.md")
    responsive = _text("responsive-accessibility.md")
    surfaces = _text("surfaces.md")
    qa = _text("visual-qa.md")

    _has_all(
        content,
        "traffic",
        "headline",
        "primary cta",
        "proof inventory",
        "objection",
        "faq",
        "risk reversal",
        "seo",
        "noindex",
    )
    _has_all(
        content,
        "pre-code handoff",
        "section order",
        "hero copy",
        "benefit",
        "how-it-works",
        "layout rationale",
        "final cta",
    )
    _has_all(
        responsive,
        "srcset",
        "picture",
        "localisation",
        "logical properties",
        "plural",
        "print",
        "email",
    )
    _has_all(responsive, "input font", "16px", "touch target")
    _has_all(surfaces, "question", "encoding", "direct label", "keyboard", "table")
    _has_all(qa, "unbroken", "url", "uuid", "min-width", "line clamp")


def test_conditional_doctrine_replaces_conflicting_absolutes_and_aliases_jargon():
    visual = _text("visual-system.md")
    live = _text("live.md")

    _has_all(visual, "pure black", "neither required nor forbidden", "actual face", "dark mode")
    _has_all(live, "variant promotion", "carbonisation")


def test_optional_image_dependent_flow_is_low_ceremony_and_fidelity_preserving():
    grounding = _text("reference-grounding.md")
    _has_all(
        grounding,
        "authorises image generation",
        "material visual unknowns",
        "palette",
        "structural mock",
        "cheap",
        "expensive raster",
        "image-native fidelity",
        "optional",
    )
    _has_all(grounding, "no mandatory approval", "questionnaire")


def test_live_reference_has_first_run_schema_fallback_and_real_command_contracts():
    live = _text("live.md")
    _has_all(
        live,
        "first run",
        '"files"',
        '"insertbefore"',
        '"commentsyntax"',
        "config_missing",
        '"path"',
        "handle fallback",
        "element_ambiguous",
        "agent-driven",
        "live-complete.mjs",
    )


def test_live_source_write_contract_does_not_claim_pathname_or_crash_atomicity():
    live = _text("live.md")
    browser = (SKILL / "scripts" / "live-browser.js").read_text().lower()
    _has_all(live, "descriptor-bound", "in-place", "not crash-atomic", "briefly visible")
    assert "arrive atomically" not in browser
    assert "completed source edit" in browser


def test_runtime_help_does_not_advertise_uninstalled_impeccable_commands():
    scripts = "\n".join(
        path.read_text()
        for path in (SKILL / "scripts").rglob("*")
        if path.is_file() and path.suffix in {".js", ".mjs"}
        and path.name != "modern-screenshot.umd.js"
    ).lower()
    assert "npx impeccable" not in scripts
    assert "usage: impeccable " not in scripts
